//! The live-state stream: a localhost WebSocket that pushes [`LiveShared`] to the UI at a fixed
//! rate in the 32-byte [`encode_live`] format, guarded by a random per-launch token.
//!
//! Connect to `ws://127.0.0.1:{port}/live?token={token}`. The server binds loopback only,
//! answers 404 for another path, 401 for a missing or wrong token (compared in constant time)
//! and 503 beyond [`MAX_CLIENTS`]; a handshake that takes longer than [`HANDSHAKE_TIMEOUT`] is
//! cut off. Every client has its own thread that reads only atomics, so neither the audio thread
//! nor the caller of [`start_stream`] ever waits on a client. Dropping the [`StreamServer`] stops
//! all threads within about [`POLL`].

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tungstenite::Message;
use tungstenite::handshake::HandshakeError;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;

use crate::live::{LiveShared, encode_live};

/// Most concurrent connections (handshaking or streaming); more get a 503.
const MAX_CLIENTS: usize = 8;
/// Most threads answering a 503 at once; beyond this a connection is just dropped.
const MAX_REJECTS: usize = 8;
/// A handshake that has not finished by then is dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often blocked loops look at the stop flag.
const POLL: Duration = Duration::from_millis(20);
/// A client that cannot take a 32-byte frame within this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_millis(50);
/// After a stall longer than this the schedule restarts instead of bursting to catch up.
const MAX_LAG: Duration = Duration::from_millis(250);

/// A running stream. Dropping it stops the accept thread and every client thread.
#[derive(Debug)]
pub struct StreamServer {
    /// The loopback port the server listens on.
    pub port: u16,
    /// 32 lowercase hex characters, new for every launch; clients pass it as `?token=`.
    pub token: String,
    clients: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl StreamServer {
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

/// Starts the stream on a free loopback port, sending `hz` frames per second (at least 1) to
/// each client.
pub fn start_stream(live: Arc<LiveShared>, hz: u32) -> io::Result<StreamServer> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| io::Error::other(e.to_string()))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let period = Duration::from_secs_f64(1.0 / f64::from(hz.max(1)));

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
    // The accepted socket must block (it may inherit the listener's non-blocking mode); reads
    // time out every POLL so the handshake can watch the deadline and the stop flag.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(POLL)).is_err()
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
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut attempt = tungstenite::accept_hdr(stream, callback);
    let mut ws = loop {
        match attempt {
            Ok(ws) => break ws,
            Err(HandshakeError::Interrupted(mid)) => {
                if stop.load(SeqCst) || Instant::now() >= deadline {
                    return;
                }
                attempt = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => return,
        }
    };
    if reject {
        return;
    }
    stream_frames(&mut ws, live, period, stop);
}

fn stream_frames(
    ws: &mut tungstenite::WebSocket<TcpStream>,
    live: &LiveShared,
    period: Duration,
    stop: &AtomicBool,
) {
    let mut start = Instant::now();
    let mut n: u32 = 0;
    loop {
        // Absolute deadlines: a late sleep shortens the next one instead of shifting the rest.
        let due = start + period * n;
        loop {
            if stop.load(SeqCst) {
                return;
            }
            let now = Instant::now();
            if now >= due {
                if now - due > MAX_LAG {
                    start = now;
                    n = 0;
                }
                break;
            }
            thread::sleep((due - now).min(POLL));
        }
        let frame = encode_live(&live.snapshot());
        if ws.send(Message::Binary(frame.to_vec().into())).is_err() {
            return;
        }
        n = n.wrapping_add(1);
        if n == 0 {
            start = Instant::now(); // after about 2 years at 60 Hz
        }
    }
}
