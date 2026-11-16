//! The live-state stream: wire layout, token guard, rate, reconnects, hardening and shutdown.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use grooviq_engine::{
    LIVE_LEN, LiveShared, LiveSnapshot, StreamServer, decode_live, encode_live, start_stream,
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
fn live_v1_layout_matches_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/live-v1.bin");
    let encoded = encode_live(&sample());
    if std::env::var_os("FIXTURE_UPDATE").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, encoded).unwrap();
    }
    let fixture = std::fs::read(path).expect("fixture (create it with FIXTURE_UPDATE=1)");
    assert_eq!(fixture.len(), LIVE_LEN);
    assert_eq!(encoded.as_slice(), fixture.as_slice());
    assert_eq!(decode_live(&fixture), Some(sample()));
    assert_eq!(decode_live(&[2; 32]), None);
    assert_eq!(decode_live(&fixture[..31]), None);
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
fn delivers_60_updates_per_second_while_the_main_thread_is_blocked() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let url = url(&server);
    let client = std::thread::spawn(move || {
        let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
        let start = Instant::now();
        let mut count = 0;
        while start.elapsed() < Duration::from_secs(1) {
            if matches!(ws.read().unwrap(), Message::Binary(_)) {
                count += 1;
            }
        }
        count
    });
    std::thread::sleep(Duration::from_millis(1100));
    let count = client.join().unwrap();
    println!("measured rate: {count} updates in 1 s");
    assert!(count >= 55, "only {count} updates in 1 s");
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
    // Every client thread is gone by the time drop returns, so every socket is already closed:
    // a non-blocking read may find buffered frames, then EOF or a reset, never "no data yet".
    for stream in [&mut stalled, raw(&mut ws), raw(&mut silent)] {
        stream.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 4096];
        let ended = loop {
            match stream.read(&mut buf) {
                Ok(0) => break true,
                Ok(_) => {}
                Err(e) => break e.kind() != ErrorKind::WouldBlock,
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
fn frames_arrive_on_the_schedule() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let (mut ws, _) = tungstenite::connect(url(&server).as_str()).unwrap();
    let times: Vec<Instant> = (0..120)
        .map(|_| {
            ws.read().unwrap();
            Instant::now()
        })
        .collect();
    let span = times[119] - times[0];
    let ideal = Duration::from_secs_f64(119.0 / 60.0);
    let error = span.as_secs_f64() / ideal.as_secs_f64() - 1.0;
    assert!(
        error.abs() < 0.03,
        "119 intervals took {span:?}, ideal {ideal:?}"
    );
}

#[test]
fn the_rate_is_clamped() {
    // A huge rate must not busy-loop: it is held to 240 frames per second.
    let server = start_stream(Arc::new(LiveShared::default()), u32::MAX).unwrap();
    let (mut ws, _) = tungstenite::connect(url(&server).as_str()).unwrap();
    let start = Instant::now();
    for _ in 0..241 {
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
}
