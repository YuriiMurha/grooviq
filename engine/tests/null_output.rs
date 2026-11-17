//! The silent output (`null_backend`, the app's `GROOVIQ_AUDIO=null`): it opens no audio
//! device, yet runs the processor in real time so the transport, meters and playhead behave as
//! they do on a device. A test binary of its own, so nothing else in the process creates a
//! cpal backend.

use grooviq_engine::{
    AudioOutput, Command, Controller, EngineConfig, OutputChoice, OutputEvent, OutputInfo,
    OutputKind, cpal_backends_created, engine, null_backend,
};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn silent_device() -> OutputInfo {
    OutputInfo {
        id: "null".into(),
        name: "Silent (QA mode)".into(),
        kind: OutputKind::Headphones,
        is_default: true,
        sample_rate: 48_000,
    }
}

type Events = Arc<Mutex<Vec<OutputEvent>>>;

fn start_silent() -> (AudioOutput, Arc<Mutex<Controller>>, Events) {
    let (c, p) = engine(EngineConfig {
        sample_rate: 44_100.0,
    });
    let c = Arc::new(Mutex::new(c));
    let events = Events::default();
    let sink = Arc::clone(&events);
    let out = AudioOutput::start(
        null_backend(),
        Arc::clone(&c),
        p,
        OutputChoice::SystemDefault,
        true,
        Box::new(move |e| sink.lock().unwrap().push(e)),
    );
    (out, c, events)
}

fn frames(c: &Mutex<Controller>) -> u64 {
    c.lock().unwrap().live().frames
}

/// Polls `done` every 5 ms until it holds (true) or `limit` passes (false).
fn wait_for(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > limit {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
    true
}

#[test]
fn the_silent_output_lists_and_opens_only_its_own_device() {
    let (out, c, events) = start_silent();
    assert_eq!(out.outputs(), [silent_device()]);
    assert_eq!(out.current(), Some(silent_device()));
    assert_eq!(
        *events.lock().unwrap(),
        [OutputEvent::Opened(silent_device())]
    );
    // Nothing in the whole run (a tone, the transport, a look, a re-selection, the drop)
    // reaches cpal.
    c.lock()
        .unwrap()
        .send(Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        })
        .unwrap();
    c.lock().unwrap().send(Command::Play).unwrap();
    out.poll_now();
    out.select(OutputChoice::Device("null".into()));
    out.poll_now();
    thread::sleep(Duration::from_millis(50));
    drop(out);
    assert_eq!(cpal_backends_created(), 0);
}

/// The frame counter, read between two clock reads: it was sampled somewhere inside
/// `(before, after)`, which is all a separate `Instant::now()` can say about when.
fn frames_between_clock_reads(c: &Mutex<Controller>) -> (Instant, u64, Instant) {
    let before = Instant::now();
    let frames = frames(c);
    (before, frames, Instant::now())
}

/// A smoke check on the real thread and clock: the silent output renders, and never faster than
/// the clock. The cadence itself (no drift, one block's wait after a stall, no catching up) is
/// proved on a synthetic clock in `null_backend.rs`; a starved CI runner may lose any share
/// of a wall-clock second, so no minimum speed is asserted here.
#[test]
fn the_silent_output_renders_and_never_runs_ahead_of_the_clock() {
    let (out, c, _events) = start_silent();
    // Past the first callbacks, then one measured second.
    thread::sleep(Duration::from_millis(50));
    let (t0, f0, _) = frames_between_clock_reads(&c);
    thread::sleep(Duration::from_millis(1_000));
    let (_, f1, t1) = frames_between_clock_reads(&c);
    drop(out);
    let rendered = (f1 - f0) as f64;
    // The counters were sampled inside their brackets, so the widest elapsed time is from the
    // start of the first bracket to the end of the last: a descheduled test thread can only
    // widen it. The backend renders at most the clock's frames plus two 256-frame callbacks
    // (the one in flight and the one a late wake-up may owe) over any stretch.
    let at_most = t1.duration_since(t0).as_secs_f64() * 48_000.0;
    assert!(
        rendered <= at_most + 512.0,
        "{rendered} frames in at most {at_most} frames' time: too fast"
    );
    assert!(rendered > 0.0, "nothing rendered in the measured second");
}

#[test]
fn the_silent_output_moves_the_transport_and_the_meters() {
    let (out, c, _events) = start_silent();
    {
        let mut c = c.lock().unwrap();
        c.send(Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        })
        .unwrap();
        c.send(Command::Play).unwrap();
    }
    assert!(
        wait_for(Duration::from_secs(2), || {
            let live = c.lock().unwrap().live();
            live.playing && live.position_ticks > 0 && live.peak[0] > 0.1
        }),
        "{:?}",
        c.lock().unwrap().live()
    );
    // The playhead keeps moving, at the transport's pace.
    let before = c.lock().unwrap().live().position_ticks;
    thread::sleep(Duration::from_millis(100));
    assert!(c.lock().unwrap().live().position_ticks > before);
    drop(out);
}

#[test]
fn dropping_the_silent_output_stops_rendering_at_once() {
    let (out, c, _events) = start_silent();
    assert!(wait_for(Duration::from_secs(2), || frames(&c) > 0));
    let dropping = Instant::now();
    drop(out);
    assert!(
        dropping.elapsed() < Duration::from_millis(250),
        "dropping took {:?}",
        dropping.elapsed()
    );
    let stopped = frames(&c);
    thread::sleep(Duration::from_millis(50));
    assert_eq!(frames(&c), stopped, "nothing renders once it is closed");
}

#[test]
fn the_silent_backend_source_names_no_audio_system() {
    let source = include_str!("../src/output/null_backend.rs");
    for word in ["cpal", "macos", "objc2", "CoreAudio", "coreaudio"] {
        assert!(
            !source.contains(word),
            "null_backend.rs mentions {word:?}: the silent backend must not reach an audio \
             system"
        );
    }
}
