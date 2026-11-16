//! Output devices: the monitor's decisions, and `AudioOutput` driving a fake backend. No test
//! here opens, lists or probes a real audio device (the one exception is `#[ignore]`d).

use grooviq_engine::{
    AudioOutput, AudioProcessor, AudioSlot, Command, Controller, EngineConfig, OutputBackend,
    OutputChoice, OutputEvent, OutputInfo, OutputKind, OutputMonitor, Transport, classify,
    cpal_backend, engine,
};
use std::sync::{Arc, Mutex, MutexGuard, Once, PoisonError};
use std::time::{Duration, Instant};

const SR: f32 = 48_000.0;

fn dev(id: &str, name: &str, kind: OutputKind, default: bool) -> OutputInfo {
    OutputInfo {
        id: id.into(),
        name: name.into(),
        kind,
        is_default: default,
        sample_rate: 48_000,
    }
}

fn speakers(default: bool) -> OutputInfo {
    dev(
        "speakers",
        "MacBook Pro Speakers",
        OutputKind::Speakers,
        default,
    )
}

fn airpods(default: bool) -> OutputInfo {
    dev("airpods", "AirPods Pro", OutputKind::Headphones, default)
}

fn usb(default: bool) -> OutputInfo {
    dev("usb-1", "Scarlett 2i2 USB", OutputKind::Unknown, default)
}

fn tone(on: bool) -> Command {
    Command::TestTone {
        on,
        hz: 440.0,
        gain_db: -12.0,
    }
}

/// Counts `l[i-1] < 0 && l[i] >= 0`.
fn upward_crossings(left: &[f32]) -> usize {
    left.windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count()
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, x| m.max(x.abs()))
}

// The monitor: which device to use, decided from the device list alone.

#[test]
fn classify_by_transport_and_name() {
    assert_eq!(
        classify("AirPods Pro", Some(Transport::Bluetooth)),
        OutputKind::Headphones
    );
    assert_eq!(
        classify("MacBook Pro Speakers", Some(Transport::BuiltIn)),
        OutputKind::Speakers
    );
    assert_eq!(
        classify("External Headphones", Some(Transport::BuiltIn)),
        OutputKind::Headphones
    );
    assert_eq!(
        classify("LG HDR 4K", Some(Transport::DisplayPort)),
        OutputKind::Speakers
    );
    assert_eq!(classify("Background Music", None), OutputKind::Unknown);
}

#[test]
fn follows_the_system_default() {
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    assert_eq!(
        m.tick(&[speakers(true)]),
        [OutputEvent::Opened(speakers(true))]
    );
    // The AirPods connect and become the default. Moving to headphones needs no guard.
    assert_eq!(
        m.tick(&[speakers(false), airpods(true)]),
        [OutputEvent::Switched {
            from: speakers(true),
            to: airpods(true)
        }]
    );
    assert_eq!(m.current(), Some(&airpods(true)));
}

#[test]
fn guard_stops_when_headphones_drop_to_speakers() {
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    m.tick(&[speakers(false), airpods(true)]);
    assert_eq!(
        m.tick(&[speakers(true)]),
        [
            OutputEvent::GuardStopped { to: speakers(true) },
            OutputEvent::Switched {
                from: airpods(true),
                to: speakers(true)
            }
        ]
    );
}

#[test]
fn guard_off_switches_silently() {
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, false);
    m.tick(&[speakers(false), airpods(true)]);
    assert_eq!(
        m.tick(&[speakers(true)]),
        [OutputEvent::Switched {
            from: airpods(true),
            to: speakers(true)
        }]
    );
}

#[test]
fn chosen_device_disappears_falls_back_to_default() {
    let mut m = OutputMonitor::new(OutputChoice::Device("usb-1".into()), true);
    assert_eq!(
        m.tick(&[speakers(true), usb(false)]),
        [OutputEvent::Opened(usb(false))]
    );
    assert_eq!(
        m.tick(&[speakers(true)]),
        [
            OutputEvent::Lost {
                fallback: Some(speakers(true))
            },
            OutputEvent::Switched {
                from: usb(false),
                to: speakers(true)
            }
        ]
    );
}

#[test]
fn no_devices_is_reported_once() {
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    assert_eq!(m.tick(&[]), [OutputEvent::NoDevice]);
    assert!(m.tick(&[]).is_empty());
    assert_eq!(m.current(), None);
}

#[test]
fn the_guard_also_covers_a_moment_with_no_device() {
    // The AirPods go, and for one poll there is no device at all before the speakers take over.
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    m.tick(&[speakers(false), airpods(true)]);
    assert_eq!(m.tick(&[]), [OutputEvent::NoDevice]);
    assert_eq!(
        m.tick(&[speakers(true)]),
        [
            OutputEvent::GuardStopped { to: speakers(true) },
            OutputEvent::Opened(speakers(true))
        ]
    );
}

#[test]
fn a_lost_choice_is_reported_once_and_followed_when_it_returns() {
    let mut m = OutputMonitor::new(OutputChoice::Device("usb-1".into()), true);
    m.tick(&[speakers(true), usb(false)]);
    assert_eq!(m.tick(&[speakers(true)]).len(), 2); // Lost, Switched
    assert!(m.tick(&[speakers(true)]).is_empty());
    assert_eq!(
        m.tick(&[speakers(true), usb(false)]),
        [OutputEvent::Switched {
            from: speakers(true),
            to: usb(false)
        }]
    );
}

#[test]
fn without_a_default_the_current_device_stays() {
    // While the system moves its default, a poll may find no device flagged.
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    m.tick(&[speakers(true)]);
    assert!(m.tick(&[speakers(false), airpods(false)]).is_empty());
    assert_eq!(m.current(), Some(&speakers(false)));
}

// AudioOutput on a fake backend.

/// A backend whose device list the test edits. An open "device" keeps its slot, so the test
/// can pull audio from it the way the device's callback would.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

#[derive(Default)]
struct FakeState {
    devices: Vec<OutputInfo>,
    open: Option<(String, Arc<AudioSlot>)>,
    /// Every device opened so far, by id.
    opened: Vec<String>,
    fail_open: bool,
    panic_next_poll: bool,
    panicked: bool,
}

const INJECTED_PANIC: &str = "fake backend: injected panic";
/// The output latency every fake device reports: 10 ms at 48 kHz.
const FAKE_LATENCY: u32 = 480;

impl Fake {
    fn new(devices: Vec<OutputInfo>) -> Self {
        let fake = Self::default();
        fake.set(devices);
        fake
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set(&self, devices: Vec<OutputInfo>) {
        self.state().devices = devices;
    }

    fn open_id(&self) -> Option<String> {
        self.state().open.as_ref().map(|(id, _)| id.clone())
    }

    fn slot(&self) -> Arc<AudioSlot> {
        let state = self.state();
        Arc::clone(&state.open.as_ref().expect("a device is open").1)
    }

    /// Pulls `frames` stereo frames from the open device in callbacks of `callback` frames.
    fn pull_in(&self, frames: usize, callback: usize) -> Vec<f32> {
        let slot = self.slot();
        let mut out = vec![0f32; frames * 2];
        for chunk in out.chunks_mut(callback * 2) {
            slot.process(chunk, 2);
        }
        out
    }

    fn pull(&self, frames: usize) -> Vec<f32> {
        self.pull_in(frames, 256)
    }
}

impl OutputBackend for Fake {
    fn outputs(&self) -> Vec<OutputInfo> {
        let (devices, panic) = {
            let mut state = self.state();
            let panic = std::mem::take(&mut state.panic_next_poll);
            state.panicked |= panic;
            (state.devices.clone(), panic)
        };
        if panic {
            panic!("{INJECTED_PANIC}");
        }
        devices
    }

    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<(), String> {
        let mut state = self.state();
        if state.fail_open {
            return Err("the fake device refuses to open".into());
        }
        state.opened.push(info.id.clone());
        // As a real stream reports it from its callbacks.
        slot.set_output_latency_frames(FAKE_LATENCY);
        state.open = Some((info.id.clone(), slot));
        Ok(())
    }

    fn close(&mut self) {
        self.state().open = None;
    }
}

type Events = Arc<Mutex<Vec<OutputEvent>>>;

fn engine_48k() -> (Arc<Mutex<Controller>>, AudioProcessor) {
    let (c, p) = engine(EngineConfig { sample_rate: SR });
    (Arc::new(Mutex::new(c)), p)
}

fn start(
    fake: &Fake,
    controller: &Arc<Mutex<Controller>>,
    processor: AudioProcessor,
    choice: OutputChoice,
    guard: bool,
) -> (AudioOutput, Events) {
    let events = Events::default();
    let sink = Arc::clone(&events);
    let output = AudioOutput::start(
        Box::new(fake.clone()),
        Arc::clone(controller),
        processor,
        choice,
        guard,
        Box::new(move |event| sink.lock().unwrap().push(event)),
    );
    (output, events)
}

/// Polls `done` every 2 ms until it holds (true) or `limit` passes (false).
fn wait_for(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    true
}

#[test]
fn survives_no_devices_and_device_churn() {
    let fake = Fake::new(Vec::new());
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    out.poll_now();
    fake.set(vec![speakers(true)]);
    out.poll_now();
    fake.set(vec![speakers(false), airpods(true)]);
    out.poll_now();
    // Playing on the AirPods.
    {
        let mut c = c.lock().unwrap();
        c.send(Command::Play).unwrap();
        c.send(tone(true)).unwrap();
    }
    fake.pull(1_024);
    let live = c.lock().unwrap().live();
    assert!(live.playing && live.tone);
    // The AirPods go: one list change, so no poll can see a list without a default.
    fake.set(vec![speakers(true)]);
    out.poll_now();
    assert_eq!(
        *events.lock().unwrap(),
        [
            OutputEvent::NoDevice,
            OutputEvent::Opened(speakers(true)),
            OutputEvent::Switched {
                from: speakers(true),
                to: airpods(true)
            },
            OutputEvent::GuardStopped { to: speakers(true) },
            OutputEvent::Switched {
                from: airpods(true),
                to: speakers(true)
            },
        ]
    );
    // The controller sent AllSoundOff, and the processor has already applied it...
    let live = c.lock().unwrap().live();
    assert!(!live.playing && !live.tone);
    // ...and the speakers hold the processor: pulling from them runs the engine.
    assert_eq!(fake.open_id().as_deref(), Some("speakers"));
    let frames = live.frames;
    fake.pull(256);
    assert!(c.lock().unwrap().live().frames > frames);
}

#[test]
fn sample_rate_change_keeps_pitch() {
    let (mut c, mut p) = engine(EngineConfig { sample_rate: SR });
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44_100.0,
        },
    );
    c.send(tone(true)).unwrap();
    let mut buf = vec![0f32; 441 * 2];
    let mut out = Vec::new();
    for _ in 0..100 {
        p.process(&mut buf, 2); // 1 s at 44.1 kHz
        out.extend_from_slice(&buf);
    }
    let crossings = upward_crossings(&left(&out)[22_050..]);
    assert!((219..=221).contains(&crossings), "crossings {crossings}");
}

#[test]
fn a_guard_stop_reaches_the_speakers_as_silence() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    {
        let mut c = c.lock().unwrap();
        c.send(tone(true)).unwrap();
        let note = Command::Note {
            key: 48,
            velocity: 1.0,
            on: true,
        };
        c.send(note).unwrap();
    }
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    let on_airpods = fake.pull(4_800);
    assert!(peak(&on_airpods[8_000..]) > 0.2, "the AirPods play");
    fake.set(vec![speakers(true)]);
    out.poll_now();
    assert_eq!(fake.open_id().as_deref(), Some("speakers"));
    // Not even the 5 ms fade of AllSoundOff reaches the speakers.
    let on_speakers = fake.pull(4_800);
    assert!(on_speakers.iter().all(|x| *x == 0.0));
}

#[test]
fn a_guard_stop_gets_through_a_full_command_queue() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(peak(&fake.pull(4_800)) > 0.2, "the AirPods play");
    // Commands pile up faster than the device takes them, until the queue is full.
    {
        let mut c = c.lock().unwrap();
        while c.send(Command::Play).is_ok() {}
    }
    fake.set(vec![speakers(true)]);
    out.poll_now();
    assert!(fake.pull(4_800).iter().all(|x| *x == 0.0));
}

#[test]
fn losing_every_device_closes_the_stream() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    fake.set(Vec::new());
    out.poll_now();
    assert_eq!(fake.open_id(), None);
    assert_eq!(c.lock().unwrap().live().output_latency_frames, 0);
    assert_eq!(
        *events.lock().unwrap(),
        [OutputEvent::Opened(speakers(true)), OutputEvent::NoDevice]
    );
}

#[test]
fn with_the_guard_off_playback_follows_to_the_speakers() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    out.set_guard(false);
    fake.set(vec![speakers(true)]);
    out.poll_now();
    assert_eq!(
        events.lock().unwrap().last(),
        Some(&OutputEvent::Switched {
            from: airpods(true),
            to: speakers(true)
        })
    );
    assert_eq!(events.lock().unwrap().len(), 2, "no GuardStopped");
    assert!(peak(&fake.pull(4_800)) > 0.2, "the tone plays on");
}

#[test]
fn selecting_a_device_switches_at_once() {
    let fake = Fake::new(vec![speakers(true), usb(false)]);
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert_eq!(out.outputs(), [speakers(true), usb(false)]);
    out.select(OutputChoice::Device("usb-1".into()));
    assert_eq!(fake.open_id().as_deref(), Some("usb-1"));
    assert_eq!(
        *events.lock().unwrap(),
        [
            OutputEvent::Opened(speakers(true)),
            OutputEvent::Switched {
                from: speakers(true),
                to: usb(false)
            }
        ]
    );
}

#[test]
fn a_failed_stream_is_reopened_at_the_rate_the_device_now_runs() {
    // Another app opens the AirPods' microphone: they drop to the 24 kHz headset mode, and the
    // stream reports an error.
    let fake = Fake::new(vec![airpods(true)]);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    let headset = OutputInfo {
        sample_rate: 24_000,
        ..airpods(true)
    };
    fake.set(vec![headset.clone()]);
    fake.slot().stream_failed();
    out.poll_now();
    assert_eq!(
        *events.lock().unwrap(),
        [
            OutputEvent::Opened(airpods(true)),
            OutputEvent::Opened(headset)
        ]
    );
    assert_eq!(fake.state().opened, ["airpods", "airpods"]);
    // The engine now runs at 24 kHz: one second is 24 000 frames, and the tone is still 440 Hz.
    let l = left(&fake.pull_in(24_000, 240));
    let crossings = upward_crossings(&l[12_000..]);
    assert!((219..=221).contains(&crossings), "crossings {crossings}");
}

#[test]
fn a_device_that_fails_to_open_is_retried_and_announced_once_open() {
    let fake = Fake::new(vec![speakers(true)]);
    fake.state().fail_open = true;
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert_eq!(fake.open_id(), None);
    assert!(
        events.lock().unwrap().is_empty(),
        "nothing claims it opened"
    );
    fake.state().fail_open = false;
    out.poll_now();
    assert_eq!(fake.open_id().as_deref(), Some("speakers"));
    assert_eq!(
        *events.lock().unwrap(),
        [OutputEvent::Opened(speakers(true))]
    );
}

#[test]
fn a_stream_error_prompts_a_check_before_the_next_poll() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    let (_out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    // Let the poll thread settle into its 500 ms wait, which then ends about 400 ms from now.
    std::thread::sleep(Duration::from_millis(100));
    // The AirPods go and their stream reports it: the switch must come well before that poll.
    fake.set(vec![speakers(true)]);
    fake.slot().stream_failed();
    assert!(wait_for(Duration::from_millis(250), || {
        events.lock().unwrap().len() == 3
    }));
    assert_eq!(fake.open_id().as_deref(), Some("speakers"));
}

/// Silences the injected panic only; any other panic still prints.
fn silence_injected_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload_as_str() != Some(INJECTED_PANIC) {
                default_hook(info);
            }
        }));
    });
}

#[test]
fn the_poll_thread_survives_a_panicking_backend() {
    silence_injected_panics();
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (_out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    fake.state().panic_next_poll = true;
    fake.set(vec![speakers(false), airpods(true)]);
    // Wakes the poll thread: its first look panics, the next one still follows the AirPods.
    fake.slot().stream_failed();
    assert!(wait_for(Duration::from_secs(2), || {
        events.lock().unwrap().len() == 2
    }));
    assert!(fake.state().panicked);
    assert_eq!(fake.open_id().as_deref(), Some("airpods"));
}

#[test]
fn dropping_the_output_closes_the_stream_and_ends_its_thread() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(fake.open_id().is_some());
    assert_eq!(c.lock().unwrap().live().output_latency_frames, FAKE_LATENCY);
    drop(out);
    assert_eq!(fake.open_id(), None);
    // No stream, no latency.
    assert_eq!(c.lock().unwrap().live().output_latency_frames, 0);
    // The poll thread shared the backend (this fake): once it is joined, the test's handle is
    // the only one left.
    assert_eq!(Arc::strong_count(&fake.0), 1);
}

/// For the owner, on a real machine: lists the outputs and how each is classified. It never
/// opens a device, so it plays nothing. Run it with
/// `cargo test -p grooviq-engine --test output -- --ignored --nocapture`.
#[test]
#[ignore = "lists the real audio devices; run by hand"]
fn lists_the_real_outputs() {
    for output in cpal_backend().outputs() {
        println!("{output:?}");
    }
}
