//! The live-state stream: wire layout, token guard, rate, reconnects, hardening and shutdown.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use grooviq_engine::{
    LIVE_LEN, LiveShared, LiveSnapshot, StreamServer, decode_live, encode_live, start_stream,
};
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

#[test]
fn a_reconnecting_client_is_served() {
    let server = start_stream(Arc::new(LiveShared::default()), 60).unwrap();
    let url = url(&server);
    assert_eq!(read_frames(&url, 5).len(), 5);
    // The first client is dropped at the end of `read_frames`.
    let (mut ws, _) = tungstenite::connect(url.as_str()).unwrap();
    for _ in 0..5 {
        ws.read().unwrap();
    }
    wait_for("one client", || server.clients() == 1);
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
    wait_for("both clients", || server.clients() == 2);
    let port = server.port;
    let start = Instant::now();
    drop(server);
    assert!(
        start.elapsed() < Duration::from_millis(300),
        "drop took {:?}",
        start.elapsed()
    );
    // The stream is over and the listener is gone.
    while ws.read().is_ok() {}
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}
