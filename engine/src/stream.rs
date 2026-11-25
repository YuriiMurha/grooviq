//! The live-state stream: a localhost WebSocket that pushes [`LiveShared`] to the UI at a fixed
//! rate in the 40-byte [`encode_live`] format, guarded by a random per-launch token.
//!
//! Connect to `ws://127.0.0.1:{port}/live?token={token}`. The server binds loopback only,
//! answers 404 for another path, 401 for a missing or wrong token (compared in constant time)
//! and 503 beyond [`MAX_CLIENTS`]; a handshake that takes longer than [`HANDSHAKE_TIMEOUT`] is
//! cut off. Connections that have not yet authenticated count against the same cap of
//! [`MAX_CLIENTS`] as streaming ones, so eight stalled handshakes lock real clients out for up
//! to [`HANDSHAKE_TIMEOUT`] (a deliberate trade for a bounded thread count). Every client has its own thread that reads only atomics, so neither the audio thread
//! nor the caller of [`start_stream`] ever waits on a client. Dropping the [`StreamServer`] stops
//! all threads within about [`POLL`].

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tungstenite::handshake::HandshakeError;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error, Message, WebSocket};

use crate::live::{LiveShared, encode_live};

/// Most concurrent connections (handshaking or streaming); more get a 503.
const MAX_CLIENTS: usize = 8;
/// Most threads answering a 503 at once; beyond this a connection is just dropped.
const MAX_REJECTS: usize = 8;
/// A handshake that has not finished by then is dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often blocked loops look at the stop flag and drain what the client sent.
const POLL: Duration = Duration::from_millis(10);
/// Pause between handshake rounds on the non-blocking socket.
const HANDSHAKE_POLL: Duration = Duration::from_millis(5);
/// Clients never need to send data (Close and Ping only), so frames and messages stay tiny.
const MAX_INCOMING: usize = 256;
/// Most incoming frames handled per wake-up, so a flooding client cannot starve its own stream.
const MAX_DRAIN: usize = 16;
/// Frame rates outside this range are clamped.
const HZ_RANGE: (u32, u32) = (1, 240);
/// The budget for blocked frame output or processing fragmented control input.
const WRITE_TIMEOUT: Duration = Duration::from_millis(50);
/// After a stall longer than this the schedule restarts instead of bursting to catch up.
const MAX_LAG: Duration = Duration::from_millis(250);

/// A running stream. Dropping it stops the accept thread and every client thread.
pub struct StreamServer {
    /// The loopback port the server listens on.
    pub port: u16,
    /// 32 lowercase hex characters, new for every launch; clients pass it as `?token=`.
    pub token: String,
    addr: SocketAddr,
    clients: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

// Hand-written so the token never reaches a log through `{:?}`.
impl fmt::Debug for StreamServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamServer")
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("clients", &self.clients())
            .finish()
    }
}

impl StreamServer {
    /// The address the listener is bound to (always loopback).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Connections currently handshaking or streaming.
    pub fn clients(&self) -> usize {
        self.clients.load(SeqCst)
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// Starts the stream on a free loopback port, sending `hz` frames per second (clamped to
/// 1..=240) to each client.
pub fn start_stream(live: Arc<LiveShared>, hz: u32) -> io::Result<StreamServer> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| io::Error::other(e.to_string()))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    let period = frame_period(hz);

    let clients = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let accept = {
        let (token, clients, stop) = (token.clone(), Arc::clone(&clients), Arc::clone(&stop));
        thread::Builder::new()
            .name("grooviq-stream-accept".into())
            .spawn(move || accept_loop(listener, live, token, period, clients, stop))?
    };
    Ok(StreamServer {
        port,
        token,
        addr,
        clients,
        stop,
        accept: Some(accept),
    })
}

/// Frees a client slot when its thread ends, however it ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

fn accept_loop(
    listener: TcpListener,
    live: Arc<LiveShared>,
    token: String,
    period: Duration,
    clients: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) {
    let rejects = Arc::new(AtomicUsize::new(0));
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(SeqCst) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => {
                // WouldBlock, or a connection that died before we got it.
                thread::sleep(POLL);
                continue;
            }
        };
        threads.retain(|t| !t.is_finished());
        let (slot, reject) = if clients.load(SeqCst) < MAX_CLIENTS {
            clients.fetch_add(1, SeqCst);
            (Slot(Arc::clone(&clients)), false)
        } else if rejects.load(SeqCst) < MAX_REJECTS {
            rejects.fetch_add(1, SeqCst);
            (Slot(Arc::clone(&rejects)), true)
        } else {
            continue; // Over capacity and over the reject budget: drop the connection.
        };
        let (live, token, stop) = (Arc::clone(&live), token.clone(), Arc::clone(&stop));
        let spawned = thread::Builder::new()
            .name("grooviq-stream-client".into())
            .spawn(move || serve(stream, slot, reject, &live, &token, period, &stop));
        if let Ok(handle) = spawned {
            threads.push(handle);
        }
    }
    for t in threads {
        let _ = t.join();
    }
}

/// Why a handshake did not produce a stream.
fn error_response(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(status.to_string()));
    *response.status_mut() = status;
    response
}

/// Equal bytes, in time that depends only on the length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// `ErrorResponse` is the type tungstenite's handshake callback dictates.
#[allow(clippy::result_large_err)]
fn authorise(request: &Request, token: &str) -> Result<(), ErrorResponse> {
    let uri = request.uri();
    if uri.path() != "/live" {
        return Err(error_response(StatusCode::NOT_FOUND));
    }
    // Exact serialized origins from tauri.conf.json / Vite and Tauri's default
    // custom-protocol origins (macOS and Windows respectively).
    let mut origins = request.headers().get_all("origin").iter();
    if let Some(origin) = origins.next()
        && (origins.next().is_some()
            || !matches!(
                origin.to_str(),
                Ok("tauri://localhost" | "http://tauri.localhost" | "http://127.0.0.1:1420")
            ))
    {
        return Err(error_response(StatusCode::FORBIDDEN));
    }
    let given = uri
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
        .unwrap_or("");
    if constant_time_eq(given.as_bytes(), token.as_bytes()) {
        Ok(())
    } else {
        Err(error_response(StatusCode::UNAUTHORIZED))
    }
}

#[allow(clippy::result_large_err)] // the handshake callback's `ErrorResponse`
fn serve(
    stream: TcpStream,
    _slot: Slot,
    reject: bool,
    live: &LiveShared,
    token: &str,
    period: Duration,
    stop: &AtomicBool,
) {
    let _ = stream.set_nodelay(true);
    // Non-blocking on every OS during the handshake: every platform then reports "no data yet"
    // as `WouldBlock` (a read timeout is `TimedOut` on Windows, which tungstenite treats as a
    // failure), and the deadline and the stop flag are checked every round however slowly
    // bytes trickle in.
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let callback = |request: &Request, response: Response| {
        if reject {
            return Err(error_response(StatusCode::SERVICE_UNAVAILABLE));
        }
        authorise(request, token).map(|()| response)
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INCOMING))
        .max_frame_size(Some(MAX_INCOMING));
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let stream = DeadlineIo {
        inner: stream,
        clock: WallClock,
        deadline,
        read_budget: ReadBudget::Handshake,
        stop,
    };
    let mut attempt = tungstenite::accept_hdr_with_config(stream, callback, Some(config));
    let mut ws = loop {
        match attempt {
            Ok(ws) => break ws,
            Err(HandshakeError::Interrupted(mid)) => {
                if stop.load(SeqCst) || Instant::now() >= deadline {
                    return;
                }
                thread::sleep(HANDSHAKE_POLL);
                attempt = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => return,
        }
    };
    if !reject {
        stream_frames(&mut ws, live, period, stop);
    }
}

// All underlying sockets remain nonblocking. Check shutdown on each syscall
// so tungstenite's partial-frame loops cannot hide a stop request.
trait WriteClock {
    fn now(&self) -> Instant;
    fn pause(&mut self, duration: Duration);
}

struct WallClock;
impl WriteClock for WallClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

struct DeadlineIo<'a, S, C = WallClock> {
    inner: S,
    clock: C,
    deadline: Instant,
    read_budget: ReadBudget,
    stop: &'a AtomicBool,
}

enum ReadBudget {
    Handshake,
    Streaming {
        deadline: Option<Instant>,
        partial: bool,
    },
}

impl<S, C: WriteClock> DeadlineIo<'_, S, C> {
    fn begin_streaming_io(&mut self) {
        self.deadline = self.clock.now() + WRITE_TIMEOUT;
        self.read_budget = ReadBudget::Streaming {
            deadline: None,
            partial: false,
        };
    }

    fn check_stop(&self) -> io::Result<()> {
        if self.stop.load(SeqCst) {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(())
        }
    }

    fn check_deadline(&self, deadline: Instant) -> io::Result<()> {
        if self.clock.now() >= deadline {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(())
        }
    }

    fn before_write(&self) -> io::Result<()> {
        self.check_stop()?;
        if matches!(self.read_budget, ReadBudget::Handshake) {
            self.check_deadline(self.deadline)?;
        }
        Ok(())
    }

    fn after_write<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if matches!(&result, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            self.check_deadline(self.deadline)?;
        }
        result
    }
}

impl<S: Read, C: WriteClock> Read for DeadlineIo<'_, S, C> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check_stop()?;
        match self.read_budget {
            ReadBudget::Handshake => self.check_deadline(self.deadline)?,
            ReadBudget::Streaming {
                deadline: Some(deadline),
                partial: true,
            } => self.check_deadline(deadline)?,
            ReadBudget::Streaming { .. } => {}
        }
        let result = self.inner.read(bytes);
        // An idle poll can always return WouldBlock. After receiving bytes, bound
        // the time tungstenite can spend reading more fragments in this poll.
        if let ReadBudget::Streaming { deadline, partial } = &mut self.read_budget
            && !*partial
            && matches!(result, Ok(n) if n > 0)
        {
            deadline.get_or_insert_with(|| self.clock.now() + WRITE_TIMEOUT);
            *partial = true;
        }
        result
    }
}
impl<S: Write, C: WriteClock> Write for DeadlineIo<'_, S, C> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.before_write()?;
        let result = self.inner.write(bytes);
        self.after_write(result)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.before_write()?;
        let result = self.inner.flush();
        self.after_write(result)
    }
}

fn flush_until<S: Read + Write, C: WriteClock>(ws: &mut WebSocket<DeadlineIo<'_, S, C>>) -> bool {
    loop {
        match ws.flush() {
            // Output is complete. A late wake after successful I/O cannot make
            // the peer a stalled writer; shutdown still ends the connection.
            Ok(()) => return !ws.get_ref().stop.load(SeqCst),
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                let remaining = ws
                    .get_ref()
                    .deadline
                    .saturating_duration_since(ws.get_ref().clock.now());
                ws.get_mut()
                    .clock
                    .pause(remaining.min(Duration::from_millis(1)))
            }
            Err(_) => return false,
        }
    }
}

fn send_frame<S: Read + Write, C: WriteClock>(
    ws: &mut WebSocket<DeadlineIo<'_, S, C>>,
    frame: Vec<u8>,
) -> bool {
    ws.get_mut().begin_streaming_io();
    match ws.write(Message::Binary(frame.into())) {
        Ok(()) => {}
        Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
        Err(_) => return false,
    }
    flush_until(ws)
}

fn frame_period(hz: u32) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(hz.clamp(HZ_RANGE.0, HZ_RANGE.1)))
}

/// When frame `n` is due: absolute deadlines (`start + n * period`), so a late wake-up shortens
/// the next wait instead of shifting every later frame. After a stall longer than [`MAX_LAG`]
/// the schedule restarts from the moment of the late frame instead of bursting to catch up.
struct Schedule {
    start: Instant,
    period: Duration,
    n: u32,
}

impl Schedule {
    fn new(start: Instant, period: Duration) -> Self {
        Self {
            start,
            period,
            n: 0,
        }
    }

    fn due(&self) -> Instant {
        self.start + self.period * self.n
    }

    /// Records that the frame due at [`Self::due`] went out at `now`.
    fn sent(&mut self, now: Instant) {
        if now.saturating_duration_since(self.due()) > MAX_LAG {
            self.start = now;
            self.n = 1;
        } else if self.n == u32::MAX {
            // After about two years at 240 Hz: rebase without moving the next deadline.
            self.start = self.due() + self.period;
            self.n = 0;
        } else {
            self.n += 1;
        }
    }
}

/// Reads what the client sent so tungstenite answers Ping and Close. `false` when the
/// connection is over: the client sent Close (the reply is flushed first), the stream failed or
/// closed, or a frame or message exceeded [`MAX_INCOMING`] (or was otherwise a protocol
/// error). Text, Binary and Pong messages within that limit are read and ignored.
fn drain_incoming<S: Read + Write, C: WriteClock>(
    ws: &mut WebSocket<DeadlineIo<'_, S, C>>,
) -> bool {
    ws.get_mut().begin_streaming_io();
    for _ in 0..MAX_DRAIN {
        match ws.read() {
            Ok(Message::Close(_)) => {
                // tungstenite queued the Close reply; push it out, then end the connection.
                let _ = flush_until(ws);
                return false;
            }
            Ok(_) => {
                // A complete message can use up the poll budget. Leave remaining
                // messages for the next poll instead of starting another read.
                let io = ws.get_mut();
                if let ReadBudget::Streaming { deadline, partial } = &mut io.read_budget {
                    *partial = false;
                    if deadline.is_some_and(|deadline| io.clock.now() >= deadline) {
                        return !io.stop.load(SeqCst);
                    }
                }
            }
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    true
}

fn stream_frames(
    ws: &mut WebSocket<DeadlineIo<'_, TcpStream>>,
    live: &LiveShared,
    period: Duration,
    stop: &AtomicBool,
) {
    run_frames(&mut SocketFrames { ws, live, stop }, period);
}

// Keep waiting and schedule advancement in one path for sockets and counted-clock tests.
trait FrameRuntime {
    fn now(&self) -> Instant;
    fn sleep(&mut self, duration: Duration);
    fn poll(&mut self) -> bool;
    fn send(&mut self) -> bool;
}

struct SocketFrames<'a, 'b> {
    ws: &'a mut WebSocket<DeadlineIo<'b, TcpStream>>,
    live: &'a LiveShared,
    stop: &'a AtomicBool,
}

impl FrameRuntime for SocketFrames<'_, '_> {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }

    fn poll(&mut self) -> bool {
        !self.stop.load(SeqCst) && drain_incoming(self.ws)
    }

    fn send(&mut self) -> bool {
        let frame = encode_live(&self.live.snapshot());
        send_frame(self.ws, frame.to_vec())
    }
}

fn run_frames(runtime: &mut impl FrameRuntime, period: Duration) {
    let mut schedule = Schedule::new(runtime.now(), period);
    loop {
        loop {
            if !runtime.poll() {
                return;
            }
            let now = runtime.now();
            let due = schedule.due();
            if now >= due {
                break;
            }
            runtime.sleep((due - now).min(POLL));
        }
        if !runtime.send() {
            return;
        }
        schedule.sent(runtime.now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_exact_single_and_optional_but_never_replaces_token() {
        for origin in [
            None,
            Some("tauri://localhost"),
            Some("http://tauri.localhost"),
            Some("http://127.0.0.1:1420"),
        ] {
            let mut request = Request::builder().uri("/live?token=secret");
            if let Some(value) = origin {
                request = request.header("origin", value);
            }
            assert!(authorise(&request.body(()).unwrap(), "secret").is_ok());
        }
        for origin in [
            "null",
            "http://127.0.0.1:1421",
            "http://localhost:1420",
            "tauri://localhost.evil",
            "http://tauri.localhost/",
            "http://tauri.localhost http://evil",
            "https://evil",
        ] {
            let request = Request::builder()
                .uri("/live?token=secret")
                .header("origin", origin)
                .body(())
                .unwrap();
            assert_eq!(
                authorise(&request, "secret").unwrap_err().status(),
                StatusCode::FORBIDDEN
            );
        }
        let request = Request::builder()
            .uri("/live?token=secret")
            .header("origin", "tauri://localhost")
            .header("origin", "http://evil")
            .body(())
            .unwrap();
        assert!(authorise(&request, "secret").is_err());
        let mut malformed = Request::builder()
            .uri("/live?token=secret")
            .body(())
            .unwrap();
        malformed.headers_mut().insert(
            "origin",
            tungstenite::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert!(authorise(&malformed, "secret").is_err());
        let request = Request::builder()
            .uri("/live?token=wrong")
            .header("origin", "tauri://localhost")
            .body(())
            .unwrap();
        assert_eq!(
            authorise(&request, "secret").unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    #[allow(clippy::result_large_err)] // tungstenite handshake callback type
    fn authenticated_never_reader_hits_write_deadline_and_releases_slot() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let clients = Arc::new(AtomicUsize::new(1));
        let count = clients.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let _slot = Slot(count);
            let (socket, _) = listener.accept().unwrap();
            socket.set_nonblocking(true).unwrap();
            let stop = AtomicBool::new(false);
            let io = DeadlineIo {
                inner: socket,
                clock: WallClock,
                deadline: Instant::now() + HANDSHAKE_TIMEOUT,
                read_budget: ReadBudget::Handshake,
                stop: &stop,
            };
            let callback = |request: &Request, response: Response| {
                authorise(request, "secret").map(|()| response)
            };
            let mut attempt = tungstenite::accept_hdr(io, callback);
            let mut ws = loop {
                match attempt {
                    Ok(ws) => break ws,
                    Err(HandshakeError::Interrupted(mid)) => {
                        thread::sleep(HANDSHAKE_POLL);
                        attempt = mid.handshake();
                    }
                    Err(_) => panic!("loopback handshake failed"),
                }
            };
            // Exercise the production frame write path without waiting minutes for
            // 40-byte frames at 60 Hz to fill the OS buffers. The peer stays open.
            let start = Instant::now();
            let mut sent = 0;
            while start.elapsed() < Duration::from_secs(4) && sent < 1_000_000 {
                if !send_frame(&mut ws, vec![0; 40]) {
                    tx.send((sent, ws.get_ref().clock.now() >= ws.get_ref().deadline))
                        .unwrap();
                    return;
                }
                sent += 1;
            }
            tx.send((sent, false)).unwrap();
        });
        let (_silent, _) =
            tungstenite::connect(format!("ws://{address}/live?token=secret")).unwrap();
        let (sent, expired) = rx.recv_timeout(Duration::from_secs(6)).unwrap();
        worker.join().unwrap();
        assert!(sent > 0, "must authenticate and send actual frames");
        assert!(
            expired,
            "must expire in the write path with the peer still open"
        );
        assert_eq!(
            clients.load(SeqCst),
            0,
            "deadline must return the connection permit"
        );
        let healthy = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
        let mut client = connect(&healthy);
        assert!(client.read().unwrap().is_binary());
    }

    #[test]
    fn websocket_handshake_enforces_origin_with_a_valid_token() {
        use tungstenite::client::IntoClientRequest;
        let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
        let url = format!("ws://127.0.0.1:{}/live?token={}", server.port, server.token);
        for origin in [
            "tauri://localhost",
            "http://tauri.localhost",
            "http://127.0.0.1:1420",
        ] {
            let mut request = url.as_str().into_client_request().unwrap();
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
            let (mut client, _) = tungstenite::connect(request).unwrap();
            assert_eq!(client.read().unwrap().into_data().len(), 40);
            drop(client);
        }
        for origin in ["null", "http://127.0.0.1:1421", "tauri://localhost.evil"] {
            let mut request = url.as_str().into_client_request().unwrap();
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
            assert!(matches!(tungstenite::connect(request),
                Err(Error::Http(response)) if response.status() == StatusCode::FORBIDDEN));
        }
    }

    struct TestClock(std::rc::Rc<std::cell::Cell<Instant>>);
    impl WriteClock for TestClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
        fn pause(&mut self, duration: Duration) {
            assert!(!duration.is_zero(), "blocked I/O retried at its deadline");
            self.0.set(self.0.get() + duration);
        }
    }

    struct LateStartClock {
        started_at: std::cell::Cell<Option<Instant>>,
        resumed_at: Instant,
    }

    impl WriteClock for LateStartClock {
        fn now(&self) -> Instant {
            self.started_at.take().unwrap_or(self.resumed_at)
        }

        fn pause(&mut self, _: Duration) {
            panic!("ready I/O must not wait");
        }
    }

    struct ReadySocket {
        incoming: &'static [u8],
        outgoing: Vec<u8>,
    }

    impl Read for ReadySocket {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.incoming.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.incoming.read(bytes)
        }
    }

    impl Write for ReadySocket {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.outgoing.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn late_start_socket<'a>(
        stop: &'a AtomicBool,
        incoming: &'static [u8],
    ) -> WebSocket<DeadlineIo<'a, ReadySocket, LateStartClock>> {
        let now = Instant::now();
        let io = DeadlineIo {
            inner: ReadySocket {
                incoming,
                outgoing: Vec::new(),
            },
            clock: LateStartClock {
                started_at: std::cell::Cell::new(Some(now)),
                resumed_at: now + Duration::from_millis(51),
            },
            deadline: now,
            read_budget: ReadBudget::Handshake,
            stop,
        };
        WebSocket::from_raw_socket(io, tungstenite::protocol::Role::Server, None)
    }

    #[test]
    fn late_first_write_sends_the_frame() {
        let stop = AtomicBool::new(false);
        let mut ws = late_start_socket(&stop, &[]);
        assert!(send_frame(&mut ws, vec![0x5a; 40]));
        let wire = &ws.get_ref().inner.outgoing;
        assert_eq!(&wire[..2], &[0x82, 40]);
        assert_eq!(&wire[2..], &[0x5a; 40]);
    }

    #[test]
    fn late_control_poll_keeps_idle_and_ready_clients_connected() {
        let stop = AtomicBool::new(false);
        // A masked Ping with one byte of payload must get an unmasked Pong.
        let ping = &[0x89, 0x81, 1, 2, 3, 4, 0x5b];
        for (incoming, outgoing) in [(&[][..], &[][..]), (&ping[..], &[0x8a, 1, 0x5a][..])] {
            let mut ws = late_start_socket(&stop, incoming);
            assert!(drain_incoming(&mut ws));
            assert_eq!(ws.get_ref().inner.outgoing, outgoing);
        }
    }

    fn fragmented_message_obeys_budget(stop_after: Option<usize>, expected_reads: usize) {
        struct Fragments<'a> {
            clock: std::rc::Rc<std::cell::Cell<Instant>>,
            stop: &'a AtomicBool,
            stop_after: Option<usize>,
            reads: usize,
        }
        impl Read for Fragments<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.reads == 100 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                // Valid masked fragments with no final frame or payload: message-size
                // and complete-message limits cannot end this protocol work.
                let frame = [if self.reads == 0 { 0x02 } else { 0x00 }, 0x80, 0, 0, 0, 0];
                bytes[..frame.len()].copy_from_slice(&frame);
                if self.reads != 0 {
                    self.clock.set(self.clock.get() + Duration::from_millis(10));
                }
                self.reads += 1;
                if self.stop_after == Some(self.reads) {
                    self.stop.store(true, SeqCst);
                }
                Ok(frame.len())
            }
        }
        impl Write for Fragments<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let now = Instant::now();
        let clock = std::rc::Rc::new(std::cell::Cell::new(now));
        let stop = AtomicBool::new(false);
        let io = DeadlineIo {
            inner: Fragments {
                clock: clock.clone(),
                stop: &stop,
                stop_after,
                reads: 0,
            },
            clock: TestClock(clock.clone()),
            deadline: now,
            read_budget: ReadBudget::Handshake,
            stop: &stop,
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_INCOMING))
            .max_frame_size(Some(MAX_INCOMING));
        let mut ws =
            WebSocket::from_raw_socket(io, tungstenite::protocol::Role::Server, Some(config));
        assert!(!drain_incoming(&mut ws));
        assert_eq!(ws.get_ref().inner.reads, expected_reads);
        assert_eq!(
            clock.get() - now,
            Duration::from_millis(10 * (expected_reads as u64 - 1))
        );
    }

    #[test]
    fn fragmented_message_cannot_outlive_its_control_deadline() {
        fragmented_message_obeys_budget(None, 6);
    }

    #[test]
    fn fragmented_message_cannot_hide_a_shutdown_request() {
        fragmented_message_obeys_budget(Some(1), 1);
    }

    struct SlowWriter {
        clock: std::rc::Rc<std::cell::Cell<Instant>>,
        bytes: usize,
        partial_until: Instant,
    }
    impl Read for SlowWriter {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.clock.get() < self.partial_until {
                self.clock.set(self.clock.get() + Duration::from_millis(10));
                self.bytes += 1;
                Ok(bytes.len().min(1))
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn successful_ready_flush_is_not_reported_as_failed_output() {
        struct ReadyWriter<'a> {
            clock: std::rc::Rc<std::cell::Cell<Instant>>,
            after_flush: Duration,
            wire: Vec<u8>,
            flushed: bool,
            stop: &'a AtomicBool,
            request_stop: bool,
        }
        impl Read for ReadyWriter<'_> {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        impl Write for ReadyWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.wire.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                // Model descheduling after the socket completed its flush, before
                // the caller next observes time. No blocked or partial write.
                self.clock.set(self.clock.get() + self.after_flush);
                if self.request_stop {
                    self.stop.store(true, SeqCst);
                }
                Ok(())
            }
        }
        for (after_flush, request_stop) in [
            (Duration::ZERO, false),
            (Duration::from_millis(50), false),
            (Duration::from_millis(50), true),
        ] {
            let now = Instant::now();
            let clock = std::rc::Rc::new(std::cell::Cell::new(now));
            let stop = AtomicBool::new(false);
            let io = DeadlineIo {
                inner: ReadyWriter {
                    clock: clock.clone(),
                    after_flush,
                    wire: Vec::new(),
                    flushed: false,
                    stop: &stop,
                    request_stop,
                },
                clock: TestClock(clock.clone()),
                deadline: now,
                read_budget: ReadBudget::Handshake,
                stop: &stop,
            };
            let mut ws = WebSocket::from_raw_socket(io, tungstenite::protocol::Role::Server, None);
            let accepted = send_frame(&mut ws, vec![0x5a; 40]);
            let writer = &ws.get_ref().inner;
            // Independent wire oracle: final unmasked binary frame, short length,
            // and every payload byte. Check completion before the behavior claim.
            assert_eq!(writer.wire.len(), 42);
            assert_eq!(&writer.wire[..2], &[0x82, 40]);
            assert_eq!(&writer.wire[2..], &[0x5a; 40]);
            assert!(writer.flushed);
            assert_eq!(clock.get() - now, after_flush);
            if request_stop {
                assert!(!accepted, "completed flush must not hide shutdown");
            } else {
                assert!(
                    accepted,
                    "complete ready output must not be treated as a blocked client: {after_flush:?}"
                );
            }
        }
    }

    #[test]
    fn persistent_backpressure_expires_with_or_without_partial_output() {
        for dribble in [false, true] {
            let now = Instant::now();
            let clock = std::rc::Rc::new(std::cell::Cell::new(now));
            let stop = AtomicBool::new(false);
            let io = DeadlineIo {
                inner: SlowWriter {
                    clock: clock.clone(),
                    bytes: 0,
                    partial_until: if dribble {
                        now + Duration::from_millis(20)
                    } else {
                        now
                    },
                },
                clock: TestClock(clock.clone()),
                deadline: now,
                read_budget: ReadBudget::Handshake,
                stop: &stop,
            };
            let mut ws = WebSocket::from_raw_socket(io, tungstenite::protocol::Role::Server, None);
            assert!(!send_frame(&mut ws, vec![0x5a; 40]));
            assert_eq!(clock.get() - now, Duration::from_millis(50));
            assert_eq!(ws.get_ref().inner.bytes, if dribble { 2 } else { 0 });
        }
    }

    #[test]
    fn stop_interrupts_pending_frame_flush() {
        struct StopClock<'a> {
            now: std::rc::Rc<std::cell::Cell<Instant>>,
            stop: &'a AtomicBool,
        }
        impl WriteClock for StopClock<'_> {
            fn now(&self) -> Instant {
                self.now.get()
            }
            fn pause(&mut self, duration: Duration) {
                self.now.set(self.now.get() + duration);
                self.stop.store(true, SeqCst);
            }
        }
        let now = Instant::now();
        let clock = std::rc::Rc::new(std::cell::Cell::new(now));
        let stop = AtomicBool::new(false);
        let io = DeadlineIo {
            inner: SlowWriter {
                clock: clock.clone(),
                bytes: 0,
                partial_until: now,
            },
            clock: StopClock {
                now: clock.clone(),
                stop: &stop,
            },
            deadline: now,
            read_budget: ReadBudget::Handshake,
            stop: &stop,
        };
        let mut ws = WebSocket::from_raw_socket(io, tungstenite::protocol::Role::Server, None);
        assert!(!send_frame(&mut ws, vec![0; 40]));
        assert!(
            stop.load(SeqCst),
            "must reach pending flush before requesting shutdown"
        );
        assert_eq!(clock.get() - now, Duration::from_millis(1));
        assert_eq!(ws.get_ref().inner.bytes, 0);
    }

    fn connect(server: &StreamServer) -> WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>> {
        let url = format!("ws://127.0.0.1:{}/live?token={}", server.port, server.token);
        let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
        ws.read().unwrap(); // the handshake is done and the client is streaming
        ws
    }

    #[test]
    fn dropping_the_server_joins_every_client_thread() {
        // Threads exit within one poll of the stop flag, so a drop that did not wait for them
        // would only sometimes be caught: repeat to make a missing join near-certain to show.
        for _ in 0..12 {
            let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
            let _clients: Vec<_> = (0..4).map(|_| connect(&server)).collect();
            let count = Arc::clone(&server.clients);
            assert_eq!(count.load(SeqCst), 4);
            drop(server);
            assert_eq!(count.load(SeqCst), 0, "a client thread outlived the server");
        }
    }

    struct CountedFrames {
        now: Instant,
        sent: Vec<Instant>,
        operations: usize,
    }

    impl FrameRuntime for CountedFrames {
        fn now(&self) -> Instant {
            self.now
        }

        fn sleep(&mut self, duration: Duration) {
            assert!(duration <= POLL, "control messages must still be polled");
            // A consistently late wake plus nonzero send work exposes per-frame resets.
            self.now += duration + Duration::from_millis(1);
        }

        fn poll(&mut self) -> bool {
            self.operations += 1;
            assert!(
                self.operations < 100_000,
                "frame loop stopped making progress"
            );
            self.sent.len() < 120
        }

        fn send(&mut self) -> bool {
            self.sent.push(self.now);
            self.now += Duration::from_micros(250);
            true
        }
    }

    #[test]
    fn production_frame_loop_keeps_cadence_with_late_wakes_and_send_work() {
        for (requested, hz) in [(60, 60), (30, 30), (0, 1), (240, 240), (u32::MAX, 240)] {
            let start = Instant::now();
            let mut runtime = CountedFrames {
                now: start,
                sent: Vec::new(),
                operations: 0,
            };
            run_frames(&mut runtime, frame_period(requested));
            assert_eq!(runtime.sent.len(), 120);
            for (n, sent) in runtime.sent.iter().enumerate() {
                // Independent rate oracle, not Schedule::due or frame_period.
                let ideal = start + Duration::from_secs_f64(n as f64 / f64::from(hz));
                assert!(*sent >= ideal - Duration::from_micros(1), "early frame {n}");
                assert!(
                    sent.saturating_duration_since(ideal) < Duration::from_micros(1001),
                    "requested {requested} Hz, frame {n} drifted by {:?}",
                    sent.saturating_duration_since(ideal)
                );
            }
        }
    }

    const PERIOD: Duration = Duration::from_micros(16_667);

    #[test]
    fn requested_rates_produce_the_exact_deadline_count() {
        for (requested, expected) in [
            (0, 1),
            (1, 1),
            (30, 30),
            (60, 60),
            (240, 240),
            (u32::MAX, 240),
        ] {
            let start = Instant::now();
            let mut schedule = Schedule::new(start, frame_period(requested));
            // Count one full second after the immediate first frame. Exclude that first
            // frame and include the last with nanosecond rounding of the period.
            schedule.sent(start);
            let end = start + Duration::from_secs(1) + Duration::from_nanos(240);
            let mut count = 0;
            while schedule.due() <= end && count <= 240 {
                let due = schedule.due();
                schedule.sent(due);
                count += 1;
            }
            assert_eq!(count, expected, "requested {requested} Hz");
        }
    }

    #[test]
    fn late_wake_ups_do_not_shift_later_frames() {
        let start = Instant::now();
        let mut s = Schedule::new(start, PERIOD);
        for _ in 0..120 {
            let late = s.due() + Duration::from_millis(3); // every sleep overshoots
            s.sent(late);
        }
        assert_eq!(s.due(), start + PERIOD * 120);
    }

    #[test]
    fn a_long_stall_restarts_the_schedule_instead_of_bursting() {
        let start = Instant::now();
        let mut s = Schedule::new(start, PERIOD);
        let late = s.due() + Duration::from_millis(400);
        s.sent(late);
        assert_eq!(
            s.due(),
            late + PERIOD,
            "no frames owed after a stall beyond MAX_LAG"
        );
    }

    #[test]
    fn a_short_stall_is_caught_up() {
        let start = Instant::now();
        let mut s = Schedule::new(start, PERIOD);
        let late = s.due() + MAX_LAG / 2;
        s.sent(late);
        assert_eq!(
            s.due(),
            start + PERIOD,
            "frames owed within MAX_LAG are still sent"
        );
    }
}
