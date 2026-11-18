//! The live-state stream: wire layout, token guard, rate, reconnects, hardening and shutdown.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use grooviq_engine::{
    LIVE_LEN, LIVE_VERSION, LiveShared, LiveSnapshot, StreamServer, decode_live, encode_live,
    start_stream,
};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message};

fn sample() -> LiveSnapshot {
    LiveSnapshot {
        peak: [0.5, 0.25],
        playing: true,
        tone: false,
        frames: 123_456_789,
        faults: 2,
        guard_hits: 3,
        output_latency_frames: 7_680,
        position_ticks: 0x0102_0304_0506_0708,
    }
}

fn url(server: &StreamServer) -> String {
    format!("ws://127.0.0.1:{}/live?token={}", server.port, server.token)
}

/// The HTTP status a handshake to `url` fails with.
fn handshake_status(url: &str) -> u16 {
    match tungstenite::connect(url) {
        Err(Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("expected an HTTP rejection, got {other:?}"),
        Ok(_) => panic!("handshake unexpectedly succeeded"),
    }
}

fn read_frames(url: &str, n: usize) -> Vec<Vec<u8>> {
    let (mut ws, _) = tungstenite::connect(url).expect("handshake");
    (0..n)
        .map(|_| match ws.read().expect("frame") {
            Message::Binary(b) => b.to_vec(),
            other => panic!("expected binary, got {other:?}"),
        })
        .collect()
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn live_v2_layout_matches_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/live-v2.bin");
    let encoded = encode_live(&sample());
    if std::env::var_os("FIXTURE_UPDATE").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, encoded).unwrap();
    }
    let fixture = std::fs::read(path).expect("fixture (create it with FIXTURE_UPDATE=1)");
    assert_eq!(LIVE_LEN, 40);
    assert_eq!(LIVE_VERSION, 2);
    assert_eq!(fixture.len(), LIVE_LEN);
    assert_eq!(encoded.as_slice(), fixture.as_slice());
    assert_eq!(&fixture[32..40], 0x0102_0304_0506_0708u64.to_le_bytes());
    assert_eq!(decode_live(&fixture), Some(sample()));
    // An unknown version, and a v1 frame, are refused.
    assert_eq!(decode_live(&[3; 40]), None);
    let mut v1 = fixture.clone();
    v1[0] = 1;
    assert_eq!(decode_live(&v1), None);
    assert_eq!(decode_live(&v1[..32]), None);
    // Valid v2 bytes at a wrong length are refused.
    assert_eq!(decode_live(&fixture[..39]), None);
    let mut long = fixture.clone();
    long.push(0);
    assert_eq!(decode_live(&long), None);
}

#[test]
fn rejects_a_missing_or_wrong_token() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let base = format!("ws://127.0.0.1:{}/live", server.port);
    assert_eq!(handshake_status(&base), 401);
    assert_eq!(handshake_status(&format!("{base}?token=")), 401);
    assert_eq!(
        handshake_status(&format!("{base}?token={}", "0".repeat(32))),
        401
    );
    assert_eq!(handshake_status(&format!("{base}?token=short")), 401);
    assert_eq!(server.token.len(), 32);
    assert!(
        server
            .token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

#[test]
fn answers_404_for_another_path() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let wrong = format!(
        "ws://127.0.0.1:{}/other?token={}",
        server.port, server.token
    );
    assert_eq!(handshake_status(&wrong), 404);
}

#[test]
fn produces_fresh_frames_while_the_calling_thread_is_blocked() {
    let live = Arc::new(LiveShared::default());
    let server = start_stream(Arc::clone(&live), 10).unwrap();
    let address = server.local_addr();
    let url = url(&server);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (mut ws, _) = tungstenite::client(url.as_str(), socket).unwrap();
        ready_tx.send(()).unwrap();
        go_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut fresh = 0;
        while fresh < 12 && Instant::now() < deadline {
            if let Message::Binary(bytes) = ws.read().unwrap()
                && decode_live(&bytes).unwrap().output_latency_frames == 7_680
            {
                fresh += 1;
            }
        }
        assert_eq!(fresh, 12, "stream stopped progressing");
        done_tx.send(Instant::now()).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    // A buffered handshake frame cannot satisfy this: publish only after client readiness.
    let published = Instant::now();
    live.set_output_latency_frames(7_680);
    go_tx.send(()).unwrap();
    // No caller-side pumping or polling: production must proceed independently.
    let result = done_rx.recv_timeout(Duration::from_secs(5));
    drop(server); // bounded cancellation also releases the client on failure
    client.join().unwrap();
    let finished = result.expect("no independent multi-frame progress while caller blocked");
    // Twelve fresh frames at 10 Hz span eleven periods. Allow the scheduler's 250 ms
    // catch-up window; measure from publication, never a delayed client's first read.
    // Load can only lengthen this interval, so it cannot cause a false "too fast" failure.
    let elapsed = finished.duration_since(published);
    assert!(
        elapsed >= Duration::from_millis(850),
        "12 fresh frames arrived too fast at 10 Hz: {elapsed:?}"
    );
}

#[test]
fn frames_carry_the_live_state() {
    let live = Arc::new(LiveShared::default());
    live.set_output_latency_frames(7_680);
    let server = start_stream(Arc::clone(&live), 60).unwrap();
    let frames = read_frames(&url(&server), 2);
    for f in frames {
        let snap = decode_live(&f).unwrap();
        assert_eq!(snap.output_latency_frames, 7_680);
    }
}

/// The raw socket under a plain client, to set timeouts and watch for EOF.
fn raw(ws: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>) -> &mut TcpStream {
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s,
        _ => unreachable!("plain TCP client"),
    }
}

#[test]
fn a_reconnecting_client_is_served() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let url = url(&server);
    let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
    for _ in 0..5 {
        ws.read().unwrap();
    }
    // A real Close: the server must answer it (a browser waits for that) and free the slot.
    ws.close(None).unwrap();
    raw(&mut ws)
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(1), "no Close reply");
        match ws.read() {
            Ok(Message::Close(_)) | Err(Error::ConnectionClosed) => break,
            Ok(_) => {}
            Err(e) => panic!("no Close reply: {e:?}"),
        }
    }
    assert!(
        start.elapsed() < Duration::from_millis(150),
        "Close reply took {:?}",
        start.elapsed()
    );
    wait_for("the slot to free", || server.clients() == 0);
    let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
    for _ in 0..5 {
        ws.read().unwrap();
    }
    wait_for("one client", || server.clients() == 1);
}

#[test]
fn a_ping_is_answered() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let (mut ws, _) = tungstenite::connect(url(&server).as_str()).unwrap();
    ws.send(Message::Ping(vec![1, 2, 3].into())).unwrap();
    for _ in 0..30 {
        if let Message::Pong(p) = ws.read().unwrap() {
            assert_eq!(p.as_ref(), [1, 2, 3]);
            return;
        }
    }
    panic!("no Pong");
}

#[test]
fn a_client_that_sends_a_large_message_is_dropped() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let (mut ws, _) = tungstenite::connect(url(&server).as_str()).unwrap();
    let _ = ws.send(Message::Binary(vec![0u8; 4096].into()));
    wait_for("the oversize client to be dropped", || {
        server.clients() == 0
    });
}

#[test]
fn answers_503_beyond_eight_clients() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let url = url(&server);
    let mut held = Vec::new();
    for _ in 0..8 {
        let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
        ws.read().unwrap();
        held.push(ws);
    }
    assert_eq!(handshake_status(&url), 503);
    held.pop();
    wait_for("a free slot", || server.clients() == 7);
    let (mut ws, _) = tungstenite::connect(url.as_str()).expect("a slot is free again");
    ws.read().unwrap();
}

#[test]
fn a_stalled_handshake_is_cut_off() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let mut stalled = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stalled.write_all(b"GET /live HTTP/1.1\r\n").unwrap(); // never finishes the request
    stalled
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let start = Instant::now();
    let mut buf = [0u8; 16];
    // The server closes the connection (EOF or reset) instead of waiting forever.
    let closed = !matches!(stalled.read(&mut buf), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut);
    assert!(closed, "server held the stalled connection");
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(1500) && waited < Duration::from_millis(3500),
        "{waited:?}"
    );
    wait_for("the slot to free", || server.clients() == 0);
}

#[test]
fn dropping_the_server_ends_every_client_quickly() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let url = url(&server);
    let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
    ws.read().unwrap();
    let mut stalled = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stalled.write_all(b"GET /live").unwrap();
    // A client that connects and never reads a byte.
    let (mut silent, _) = tungstenite::connect(url.as_str()).unwrap();
    wait_for("three clients", || server.clients() == 3);
    let port = server.port;
    let start = Instant::now();
    drop(server);
    assert!(
        start.elapsed() < Duration::from_millis(150),
        "drop took {:?}",
        start.elapsed()
    );
    // Every client thread is gone by the time drop returns, so each socket reaches EOF or a
    // reset; allow 200 ms for the FIN to land (slow runners). A surviving thread keeps the
    // socket open: the read then times out, or frames keep arriving past the window.
    for stream in [&mut stalled, raw(&mut ws), raw(&mut silent)] {
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let window = Instant::now() + Duration::from_millis(200);
        let mut buf = [0u8; 4096];
        let ended = loop {
            if Instant::now() > window {
                break false;
            }
            match stream.read(&mut buf) {
                Ok(0) => break true,
                Ok(_) => {}
                Err(e) => break !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            }
        };
        assert!(ended, "a client thread outlived the server");
    }
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

/// Writes `GET /live` padded with 128 bytes every 15 ms (never 20 ms of silence) until the
/// server hangs up; returns how long it lasted (None if it was still connected after `max`).
fn trickle(port: u16, max: Duration) -> Option<Duration> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut first = b"GET /live HTTP/1.1\r\nX-Pad: ".to_vec();
    first.resize(256, b'a');
    s.write_all(&first).unwrap();
    let start = Instant::now();
    while start.elapsed() < max {
        if s.write_all(&[b'a'; 128]).is_err() {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    None
}

#[test]
fn a_trickling_handshake_is_cut_off_at_the_deadline() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let port = server.port;
    let lasted = trickle(port, Duration::from_secs(5)).expect("server hung up");
    assert!(
        lasted < Duration::from_millis(2500),
        "trickler lived {lasted:?}"
    );
    wait_for("the slot to free", || server.clients() == 0);
}

#[test]
fn dropping_the_server_while_a_handshake_trickles_is_quick() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let port = server.port;
    let trickler = std::thread::spawn(move || trickle(port, Duration::from_secs(5)));
    wait_for("the trickler", || server.clients() == 1);
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    drop(server);
    assert!(
        start.elapsed() < Duration::from_millis(300),
        "drop took {:?}",
        start.elapsed()
    );
    assert!(
        trickler.join().unwrap().is_some(),
        "trickler was never hung up on"
    );
}

#[test]
fn the_server_binds_loopback_only() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    assert_eq!(server.local_addr().ip(), std::net::Ipv4Addr::LOCALHOST);
    assert_eq!(server.local_addr().port(), server.port);
}

#[test]
fn every_launch_has_its_own_token() {
    let a = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let b = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    assert_ne!(a.token, b.token);
}

#[test]
fn only_the_whole_token_is_accepted() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let t = &server.token;
    let at = |token: &str| format!("ws://127.0.0.1:{}/live?token={token}", server.port);
    assert_eq!(handshake_status(&at(&t[..1])), 401, "first character");
    assert_eq!(
        handshake_status(&at(&t[..t.len() - 1])),
        401,
        "token minus its last character"
    );
    assert_eq!(
        handshake_status(&at(&format!("{t}0"))),
        401,
        "token plus a character"
    );
    assert_eq!(handshake_status(&at(&t.to_uppercase())), 401, "wrong case");
    // The token may follow other parameters.
    let with_others = format!("ws://127.0.0.1:{}/live?a=b&token={t}&x=1", server.port);
    let (mut ws, _) = tungstenite::connect(with_others.as_str()).unwrap();
    ws.read().unwrap();
}

#[test]
fn real_socket_makes_bounded_progress_at_the_requested_rate() {
    // Exact cadence is checked through the production loop with counted time. Here retain
    // real handshake, socket delivery, and rate wiring without a shared-host speed budget.
    for hz in [30, 60] {
        let server = start_stream(Arc::new(LiveShared::default()), hz).unwrap();
        let buffered_connect = || {
            let (ws, _) = tungstenite::connect(url(&server).as_str()).unwrap();
            // Deliberately let frames buffer before returning control to the client.
            std::thread::sleep(Duration::from_millis(200));
            ws
        };
        // Include the handshake and a delayed client's setup in the measurement.
        let start = Instant::now();
        let mut ws = buffered_connect();
        raw(&mut ws)
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        for _ in 0..120 {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "stream stopped progressing"
            );
            let Message::Binary(frame) = ws.read().expect("bounded frame delivery") else {
                panic!("expected live frame");
            };
            assert!(decode_live(&frame).is_some());
        }
        // Lower bound catches a bypassed/faster rate; contention can only delay delivery.
        assert!(start.elapsed() >= Duration::from_secs_f64(118.0 / f64::from(hz)));
    }
}

#[test]
fn the_rate_is_clamped() {
    // A huge rate must not busy-loop: it is held to 240 frames per second.
    let server = start_stream(Arc::new(LiveShared::default()), u32::MAX).unwrap();
    let buffered_connect = || {
        let (ws, _) = tungstenite::connect(url(&server).as_str()).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        ws
    };
    let start = Instant::now();
    let mut ws = buffered_connect();
    raw(&mut ws)
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    for _ in 0..241 {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "clamped stream stopped progressing"
        );
        ws.read().unwrap();
    }
    assert!(
        start.elapsed() >= Duration::from_millis(900),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn debug_output_hides_the_token() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    assert!(!format!("{server:?}").contains(&server.token));
}

#[test]
fn flags_and_wide_counters_round_trip() {
    let tone = LiveSnapshot {
        tone: true,
        ..LiveSnapshot::default()
    };
    let b = encode_live(&tone);
    assert_eq!(b[1], 0b10);
    assert_eq!(&b[2..4], [0, 0]);
    assert_eq!(decode_live(&b), Some(tone));
    let both = LiveSnapshot {
        tone: true,
        playing: true,
        ..LiveSnapshot::default()
    };
    assert_eq!(encode_live(&both)[1], 0b11);
    assert_eq!(decode_live(&encode_live(&both)), Some(both));
    let wide = LiveSnapshot {
        frames: 0x0123_4567_89AB_CDEF,
        ..sample()
    };
    let b = encode_live(&wide);
    assert_eq!(&b[12..20], 0x0123_4567_89AB_CDEFu64.to_le_bytes());
    assert_eq!(decode_live(&b), Some(wide));
    let far = LiveSnapshot {
        position_ticks: u64::MAX - 1,
        ..sample()
    };
    let b = encode_live(&far);
    assert_eq!(&b[32..40], (u64::MAX - 1).to_le_bytes());
    assert_eq!(decode_live(&b), Some(far));
}
