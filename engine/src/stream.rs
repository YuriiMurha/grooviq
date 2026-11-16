//! The live-state stream: a localhost WebSocket that pushes [`LiveShared`] to the UI at a fixed
//! rate in the 32-byte [`encode_live`] format, guarded by a random per-launch token.
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
use std::io;
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
/// A client that cannot take a 32-byte frame within this long is dropped.
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
    let period = Duration::from_secs_f64(1.0 / f64::from(hz.clamp(HZ_RANGE.0, HZ_RANGE.1)));

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
    if stream.set_nonblocking(true).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
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
fn drain_incoming(ws: &mut WebSocket<TcpStream>) -> bool {
    for _ in 0..MAX_DRAIN {
        match ws.read() {
            Ok(Message::Close(_)) => {
                // tungstenite queued the Close reply; push it out, then end the connection.
                let _ = ws.get_ref().set_nonblocking(false);
                let _ = ws.flush();
                return false;
            }
            Ok(_) => {}
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    true
}

fn stream_frames(
    ws: &mut WebSocket<TcpStream>,
    live: &LiveShared,
    period: Duration,
    stop: &AtomicBool,
) {
    let mut schedule = Schedule::new(Instant::now(), period);
    loop {
        loop {
            if stop.load(SeqCst) || !drain_incoming(ws) {
                return;
            }
            let now = Instant::now();
            let due = schedule.due();
            if now >= due {
                break;
            }
            thread::sleep((due - now).min(POLL));
        }
        let frame = encode_live(&live.snapshot());
        // Writes block (bounded by WRITE_TIMEOUT) so a frame is whole or the client is dropped.
        let sent = ws.get_ref().set_nonblocking(false).is_ok()
            && ws.send(Message::Binary(frame.to_vec().into())).is_ok()
            && ws.get_ref().set_nonblocking(true).is_ok();
        if !sent {
            return;
        }
        schedule.sent(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    const PERIOD: Duration = Duration::from_micros(16_667);

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
