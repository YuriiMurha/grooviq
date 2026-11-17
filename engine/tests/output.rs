//! Output devices: the monitor's decisions, and `AudioOutput` driving a fake backend. No test
//! here opens, lists or probes a real audio device (the one exception is `#[ignore]`d).

use grooviq_engine::{
    AudioOutput, AudioProcessor, AudioSlot, Command, Controller, EngineConfig, NoteId,
    OutputBackend, OutputChoice, OutputEvent, OutputInfo, OutputKind, OutputMonitor, Transport,
    classify, cpal_backend, engine,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock, PoisonError, Weak, mpsc};
use std::thread;
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

fn sony(default: bool) -> OutputInfo {
    dev("sony", "WH-1000XM5", OutputKind::Headphones, default)
}

fn usb(default: bool) -> OutputInfo {
    dev("usb-1", "Scarlett 2i2 USB", OutputKind::Unknown, default)
}

/// A Bluetooth speaker, classified as the backend would.
fn jbl(default: bool) -> OutputInfo {
    let kind = classify("JBL Flip 6", Some(Transport::Bluetooth));
    dev("jbl", "JBL Flip 6", kind, default)
}

fn tone(on: bool) -> Command {
    Command::TestTone {
        on,
        hz: 440.0,
        gain_db: -12.0,
    }
}

fn note_on(key: u8) -> Command {
    Command::Note {
        key,
        velocity: 1.0,
        on: true,
        id: NoteId(u32::from(key)),
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

fn lost(events: &[OutputEvent]) -> bool {
    events.iter().any(|e| matches!(e, OutputEvent::Lost { .. }))
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
fn classify_table() {
    use OutputKind::{Headphones, Speakers, Unknown};
    // Every headphone word, in any case, wins over any transport.
    let headphones = [
        "AirPods Pro",
        "Headphones",
        "USB Headset",
        "Earbuds",
        "Earphones",
        "Galaxy Buds2",
        "Beats Studio Pro",
        "Bose QC45",
        "Jabra Evolve2",
        "WH-1000XM5",
        "WF-1000XM5",
    ];
    for name in headphones {
        for transport in [
            None,
            Some(Transport::BuiltIn),
            Some(Transport::Bluetooth),
            Some(Transport::Hdmi),
        ] {
            for name in [name.to_owned(), name.to_uppercase(), name.to_lowercase()] {
                assert_eq!(
                    classify(&name, transport),
                    Headphones,
                    "{name} {transport:?}"
                );
            }
        }
    }
    // Bluetooth alone does not tell: Bluetooth speakers exist.
    assert_eq!(classify("JBL Flip 6", Some(Transport::Bluetooth)), Unknown);
    assert_eq!(
        classify("Bluetooth Speaker", Some(Transport::Bluetooth)),
        Speakers
    );
    // Built-in, HDMI and DisplayPort outputs with neutral names are speakers.
    assert_eq!(classify("Output", Some(Transport::BuiltIn)), Speakers);
    assert_eq!(classify("LG", Some(Transport::Hdmi)), Speakers);
    assert_eq!(
        classify("Dell U2723QE", Some(Transport::DisplayPort)),
        Speakers
    );
    // Speaker words, with no transport, in any case.
    for name in [
        "Living Room TV",
        "Studio Monitor",
        "USB Speaker",
        "Studio Display",
        "HDMI Out",
    ] {
        assert_eq!(classify(name, None), Speakers, "{name}");
        assert_eq!(classify(&name.to_uppercase(), None), Speakers, "{name}");
        assert_eq!(classify(&name.to_lowercase(), None), Speakers, "{name}");
    }
    // A neutral name says nothing, whatever the other transports.
    for transport in [
        None,
        Some(Transport::Usb),
        Some(Transport::Other),
        Some(Transport::Bluetooth),
    ] {
        assert_eq!(
            classify("Scarlett 2i2", transport),
            Unknown,
            "{transport:?}"
        );
    }
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
    // The fallback to the speakers is the system's move, not the user's: the guard stops it.
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
            OutputEvent::GuardStopped { to: speakers(true) },
            OutputEvent::Switched {
                from: usb(false),
                to: speakers(true)
            }
        ]
    );
}

#[test]
fn chosen_device_disappears_with_the_guard_off_falls_back_quietly() {
    let mut m = OutputMonitor::new(OutputChoice::Device("usb-1".into()), false);
    m.tick(&[speakers(true), usb(false)]);
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
fn any_move_by_itself_to_anything_but_headphones_stops() {
    let moved = |before: &[OutputInfo], after: &[OutputInfo]| {
        let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
        m.tick(before);
        m.tick(after)
    };
    // Headphones to an audio interface, which may drive loud monitors.
    assert_eq!(
        moved(&[airpods(true), usb(false)], &[usb(true)]),
        [
            OutputEvent::GuardStopped { to: usb(true) },
            OutputEvent::Switched {
                from: airpods(true),
                to: usb(true)
            }
        ]
    );
    // The AirPods hand over to a Bluetooth speaker.
    assert_eq!(
        moved(&[airpods(true), jbl(false)], &[jbl(true)]),
        [
            OutputEvent::GuardStopped { to: jbl(true) },
            OutputEvent::Switched {
                from: airpods(true),
                to: jbl(true)
            }
        ]
    );
    // Even from the speakers: a new default is not the user's choice.
    assert_eq!(
        moved(&[speakers(true), usb(false)], &[speakers(false), usb(true)]),
        [
            OutputEvent::GuardStopped { to: usb(true) },
            OutputEvent::Switched {
                from: speakers(true),
                to: usb(true)
            }
        ]
    );
    // A move to headphones never stops.
    assert_eq!(
        moved(
            &[speakers(true), sony(false)],
            &[speakers(false), sony(true)]
        ),
        [OutputEvent::Switched {
            from: speakers(true),
            to: sony(true)
        }]
    );
}

#[test]
fn a_move_the_user_picks_plays_on() {
    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    m.tick(&[speakers(false), airpods(true)]);
    m.set_choice(OutputChoice::Device("speakers".into()));
    assert_eq!(
        m.tick(&[speakers(false), airpods(true)]),
        [OutputEvent::Switched {
            from: airpods(true),
            to: speakers(false)
        }]
    );
    // A chosen device that is missing leaves a fallback, which is the system's move.
    let mut m = OutputMonitor::new(OutputChoice::Device("airpods".into()), true);
    m.tick(&[speakers(true), airpods(false)]);
    m.set_choice(OutputChoice::Device("usb-1".into()));
    assert_eq!(
        m.tick(&[speakers(true), airpods(false)]),
        [
            OutputEvent::Lost {
                fallback: Some(speakers(true))
            },
            OutputEvent::GuardStopped { to: speakers(true) },
            OutputEvent::Switched {
                from: airpods(false),
                to: speakers(true)
            }
        ]
    );
}

#[test]
fn a_lost_choice_is_reported_once_and_followed_when_it_returns() {
    let mut m = OutputMonitor::new(OutputChoice::Device("usb-1".into()), true);
    m.tick(&[speakers(true), usb(false)]);
    assert!(lost(&m.tick(&[speakers(true)])));
    assert!(m.tick(&[speakers(true)]).is_empty());
    // Moving back to it is a move by itself to an unknown output.
    assert_eq!(
        m.tick(&[speakers(true), usb(false)]),
        [
            OutputEvent::GuardStopped { to: usb(false) },
            OutputEvent::Switched {
                from: speakers(true),
                to: usb(false)
            }
        ]
    );
}

#[test]
fn lost_and_no_device_are_reported_again_after_a_return() {
    let mut m = OutputMonitor::new(OutputChoice::Device("usb-1".into()), true);
    m.tick(&[speakers(true), usb(false)]);
    assert!(lost(&m.tick(&[speakers(true)])));
    m.tick(&[speakers(true), usb(false)]);
    assert!(lost(&m.tick(&[speakers(true)])), "lost again");
    // Another device that is not there either.
    m.set_choice(OutputChoice::Device("hdmi-1".into()));
    assert!(
        lost(&m.tick(&[speakers(true)])),
        "the new choice is lost too"
    );
    assert!(!lost(&m.tick(&[speakers(true)])), "once");

    let mut m = OutputMonitor::new(OutputChoice::SystemDefault, true);
    assert_eq!(m.tick(&[]), [OutputEvent::NoDevice]);
    m.tick(&[speakers(true)]);
    assert_eq!(m.tick(&[]), [OutputEvent::NoDevice], "no device again");
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
    /// Calls to `open`, failed ones included.
    attempts: usize,
    /// Calls to `outputs`: the looks.
    polls: usize,
    fail_open: bool,
    /// The rate a device really opens at, when it will not switch to the one asked for.
    open_rate: Option<u32>,
    /// How long an open takes.
    open_delay: Duration,
    /// An open is under way.
    opening: bool,
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

    /// Waits for the poll thread's next look, and a moment more for it to settle into its
    /// 500 ms wait: the next regular look is then most of that away.
    fn after_a_poll(&self) {
        let polls = self.state().polls;
        assert!(wait_for(Duration::from_secs(2), || self.state().polls > polls));
        thread::sleep(Duration::from_millis(20));
    }
}

impl OutputBackend for Fake {
    fn outputs(&self) -> Vec<OutputInfo> {
        let (devices, panic) = {
            let mut state = self.state();
            state.polls += 1;
            let panic = std::mem::take(&mut state.panic_next_poll);
            state.panicked |= panic;
            (state.devices.clone(), panic)
        };
        if panic {
            panic!("{INJECTED_PANIC}");
        }
        devices
    }

    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<u32, String> {
        let delay = {
            let mut state = self.state();
            state.attempts += 1;
            state.opening = true;
            state.open_delay
        };
        // A device that takes its time (outside the fake's lock, so the test can look in).
        thread::sleep(delay);
        let mut state = self.state();
        state.opening = false;
        if state.fail_open {
            return Err("the fake device refuses to open".into());
        }
        state.opened.push(info.id.clone());
        // As a real stream reports it from its callbacks.
        slot.set_output_latency_frames(FAKE_LATENCY);
        state.open = Some((info.id.clone(), slot));
        Ok(state.open_rate.unwrap_or(info.sample_rate))
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
        thread::sleep(Duration::from_millis(2));
    }
    true
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
        c.send(note_on(48)).unwrap();
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
fn a_guard_stop_behind_a_burst_of_notes_stays_silent() {
    // At 16 kHz (the AirPods' headset mode) 10 ms is under three blocks, and a burst of notes
    // still queued holds AllSoundOff back for four (a block takes 256 notes): the silence must
    // count from when AllSoundOff applies, not from when it was queued.
    let at_16k = |info: OutputInfo| OutputInfo {
        sample_rate: 16_000,
        ..info
    };
    let fake = Fake::new(vec![at_16k(speakers(false)), at_16k(airpods(true))]);
    let (c, p) = engine_48k();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    fake.pull(1_600);
    {
        // 1023 notes leave the 1024-slot queue room for AllSoundOff, right behind them.
        let mut c = c.lock().unwrap();
        for i in 0..1_023u32 {
            c.send(note_on(36 + (i % 48) as u8)).unwrap();
        }
    }
    fake.set(vec![at_16k(speakers(true))]);
    out.poll_now();
    assert_eq!(fake.open_id().as_deref(), Some("speakers"));
    assert!(fake.pull(1_600).iter().all(|x| *x == 0.0));
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
fn a_guard_stop_across_a_moment_with_no_device_is_silent() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    {
        let mut c = c.lock().unwrap();
        c.send(tone(true)).unwrap();
        c.send(note_on(48)).unwrap();
    }
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(peak(&fake.pull(4_800)[8_000..]) > 0.2, "the AirPods play");
    fake.set(Vec::new());
    out.poll_now();
    fake.set(vec![speakers(true)]);
    out.poll_now();
    assert_eq!(
        events.lock().unwrap()[1..],
        [
            OutputEvent::NoDevice,
            OutputEvent::GuardStopped { to: speakers(true) },
            OutputEvent::Opened(speakers(true))
        ]
    );
    assert!(fake.pull(4_800).iter().all(|x| *x == 0.0));
}

#[test]
fn a_guard_stop_to_a_device_at_another_rate_stays_silent() {
    let speakers_44k = OutputInfo {
        sample_rate: 44_100,
        ..speakers(true)
    };
    let fake = Fake::new(vec![
        OutputInfo {
            is_default: false,
            ..speakers_44k.clone()
        },
        airpods(true),
    ]);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(peak(&fake.pull(4_800)) > 0.2, "the AirPods play");
    // A key pressed just as the AirPods go: queued, not rendered yet.
    c.lock().unwrap().send(note_on(48)).unwrap();
    fake.set(vec![speakers_44k.clone()]);
    out.poll_now();
    assert_eq!(out.current(), Some(speakers_44k));
    assert!(fake.pull_in(4_410, 441).iter().all(|x| *x == 0.0));
}

#[test]
fn a_device_the_user_picks_plays_on_without_a_guard_stop() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(peak(&fake.pull(4_800)) > 0.2, "the AirPods play");
    out.select(OutputChoice::Device("speakers".into()));
    // The look that applies it, unless the woken poll thread got there first.
    out.poll_now();
    assert_eq!(
        *events.lock().unwrap(),
        [
            OutputEvent::Opened(airpods(true)),
            OutputEvent::Switched {
                from: airpods(true),
                to: speakers(false)
            }
        ]
    );
    assert!(peak(&fake.pull(4_800)) > 0.2, "the tone plays on, as asked");
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
    fake.after_a_poll();
    out.select(OutputChoice::Device("usb-1".into()));
    // Well before the next regular look.
    assert!(wait_for(Duration::from_millis(350), || {
        events.lock().unwrap().len() == 2
    }));
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
            OutputEvent::Opened(headset.clone())
        ]
    );
    assert_eq!(fake.state().opened, ["airpods", "airpods"]);
    assert_eq!(out.current(), Some(headset));
    // The engine now runs at 24 kHz: one second is 24 000 frames, and the tone is still 440 Hz.
    let l = left(&fake.pull_in(24_000, 240));
    let crossings = upward_crossings(&l[12_000..]);
    assert!((219..=221).contains(&crossings), "crossings {crossings}");
}

#[test]
fn a_stream_that_keeps_failing_is_retried_later_and_quietly() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    // A first failure: reopened at once, and reported.
    fake.slot().stream_failed();
    out.poll_now();
    assert_eq!(fake.state().opened.len(), 2);
    // It fails again straight away: the next try waits half a second.
    fake.slot().stream_failed();
    out.poll_now();
    assert_eq!(fake.state().opened.len(), 2);
    assert_eq!(out.current(), None);
    assert!(wait_for(Duration::from_secs(2), || fake
        .state()
        .opened
        .len()
        == 3));
    // Reported once, not at every reopening.
    assert_eq!(
        *events.lock().unwrap(),
        [
            OutputEvent::Opened(speakers(true)),
            OutputEvent::Opened(speakers(true))
        ]
    );
    assert!(wait_for(Duration::from_secs(1), || out.current()
        == Some(speakers(true))));
}

#[test]
fn an_open_failure_is_reported_once_and_retried_with_back_off() {
    let fake = Fake::new(vec![speakers(true), airpods(false)]);
    fake.state().fail_open = true;
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    let failed = OutputEvent::OpenFailed {
        output: speakers(true),
        error: "the fake device refuses to open".into(),
    };
    assert_eq!(*events.lock().unwrap(), std::slice::from_ref(&failed));
    assert_eq!(out.current(), None);
    // Tried again after half a second, then not for another second.
    assert!(wait_for(Duration::from_secs(2), || fake.state().attempts == 2));
    thread::sleep(Duration::from_millis(700));
    assert_eq!(fake.state().attempts, 2);
    assert_eq!(events.lock().unwrap().len(), 1, "reported once");
    // Never another device instead.
    assert!(fake.state().opened.is_empty());
    fake.state().fail_open = false;
    assert!(wait_for(Duration::from_secs(3), || {
        events.lock().unwrap().len() == 2
    }));
    assert_eq!(
        *events.lock().unwrap(),
        [failed, OutputEvent::Opened(speakers(true))]
    );
    assert_eq!(out.current(), Some(speakers(true)));
}

#[test]
fn a_new_choice_tries_a_failing_device_again_and_reports_it_again() {
    let fake = Fake::new(vec![speakers(true), airpods(false)]);
    fake.state().fail_open = true;
    let (c, p) = engine_48k();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    out.select(OutputChoice::Device("speakers".into()));
    out.poll_now();
    let failed = OutputEvent::OpenFailed {
        output: speakers(true),
        error: "the fake device refuses to open".into(),
    };
    assert_eq!(*events.lock().unwrap(), [failed.clone(), failed]);
    // Tried at start and for the new choice (a regular retry may slip in on a slow machine).
    assert!(fake.state().attempts >= 2);
    assert!(fake.state().opened.is_empty(), "never the AirPods instead");
}

#[test]
fn a_device_that_will_not_switch_rate_plays_at_its_own() {
    // It lists 48 kHz, but an external clock holds it at 44.1 kHz.
    let fake = Fake::new(vec![usb(true)]);
    fake.state().open_rate = Some(44_100);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    let at_44k = OutputInfo {
        sample_rate: 44_100,
        ..usb(true)
    };
    assert_eq!(
        *events.lock().unwrap(),
        [OutputEvent::Opened(at_44k.clone())]
    );
    assert_eq!(out.current(), Some(at_44k));
    // The engine runs at 44.1 kHz: one second is 44 100 frames, and the tone is still 440 Hz.
    let l = left(&fake.pull_in(44_100, 441));
    let crossings = upward_crossings(&l[22_050..]);
    assert!((219..=221).contains(&crossings), "crossings {crossings}");
}

#[test]
fn a_slow_open_does_not_hold_up_outputs_current_or_select() {
    let fake = Fake::new(vec![speakers(true), airpods(false)]);
    let (c, p) = engine_48k();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    // The AirPods become the default, and opening them takes a second.
    fake.state().open_delay = Duration::from_secs(1);
    fake.set(vec![speakers(false), airpods(true)]);
    assert!(wait_for(Duration::from_secs(2), || fake.state().opening));
    let asked = Instant::now();
    assert_eq!(out.outputs().len(), 2);
    let _ = out.current();
    out.set_guard(true);
    out.select(OutputChoice::SystemDefault);
    assert!(
        asked.elapsed() < Duration::from_millis(200),
        "they waited {:?}",
        asked.elapsed()
    );
    assert!(wait_for(Duration::from_secs(3), || {
        fake.open_id().as_deref() == Some("airpods")
    }));
}

#[test]
fn a_stream_error_prompts_a_check_before_the_next_poll() {
    let fake = Fake::new(vec![speakers(false), airpods(true)]);
    let (c, p) = engine_48k();
    let (_out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    fake.after_a_poll();
    // The AirPods go and their stream reports it: the switch comes long before the next poll.
    fake.set(vec![speakers(true)]);
    fake.slot().stream_failed();
    assert!(wait_for(Duration::from_millis(350), || {
        events.lock().unwrap().len() == 3
    }));
    assert_eq!(fake.open_id().as_deref(), Some("speakers"));
}

#[test]
fn the_poll_follows_a_change_on_its_own() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (_out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    fake.set(vec![speakers(false), airpods(true)]);
    assert!(wait_for(Duration::from_millis(1_500), || {
        events.lock().unwrap().len() == 2
    }));
    assert_eq!(fake.open_id().as_deref(), Some("airpods"));
}

#[test]
fn a_switch_at_the_same_rate_keeps_the_sound_going() {
    // From one pair of headphones to another: no guard stop, and no rebuilt graph.
    let fake = Fake::new(vec![airpods(true), sony(false)]);
    let (c, p) = engine_48k();
    c.lock().unwrap().send(tone(true)).unwrap();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(peak(&fake.pull(4_800)) > 0.2, "the AirPods play");
    fake.set(vec![airpods(false), sony(true)]);
    out.poll_now();
    assert_eq!(fake.open_id().as_deref(), Some("sony"));
    // A rebuilt graph would fade the tone back in over 5 ms; this one carries on at full level.
    assert!(peak(&fake.pull(48)) > 0.2);
}

#[test]
fn on_event_may_call_back_into_the_output() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let output: Arc<OnceLock<Weak<AudioOutput>>> = Arc::default();
    type Seen = Vec<(OutputEvent, Option<OutputInfo>, usize)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let on_event = {
        let output = Arc::clone(&output);
        let seen = Arc::clone(&seen);
        move |event: OutputEvent| {
            let Some(out) = output.get().and_then(Weak::upgrade) else {
                return;
            };
            let current = out.current();
            let listed = out.outputs().len();
            // A look from inside the callback.
            out.poll_now();
            seen.lock().unwrap().push((event, current, listed));
        }
    };
    let out = Arc::new(AudioOutput::start(
        Box::new(fake.clone()),
        c,
        p,
        OutputChoice::SystemDefault,
        true,
        Box::new(on_event),
    ));
    output.set(Arc::downgrade(&out)).unwrap();
    fake.set(vec![speakers(false), airpods(true)]);
    let (done, finished) = mpsc::channel();
    let looking = Arc::clone(&out);
    thread::spawn(move || {
        looking.poll_now();
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(5)).is_ok(),
        "a callback that calls back deadlocked"
    );
    // It saw the change already carried out.
    let switched = OutputEvent::Switched {
        from: speakers(true),
        to: airpods(true),
    };
    assert_eq!(
        seen.lock().unwrap().first(),
        Some(&(switched, Some(airpods(true)), 2))
    );
}

#[test]
fn a_panicking_backend_never_reaches_the_caller() {
    silence_injected_panics();
    let fake = Fake::new(vec![speakers(true)]);
    fake.state().panic_next_poll = true;
    let (c, p) = engine_48k();
    // Its first look panics inside the backend.
    let (out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(fake.state().panicked);
    fake.state().panic_next_poll = true;
    out.poll_now();
    out.poll_now();
    assert_eq!(
        *events.lock().unwrap(),
        [OutputEvent::Opened(speakers(true))]
    );
}

#[test]
fn the_poll_thread_survives_a_panicking_backend() {
    silence_injected_panics();
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (_out, events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    fake.state().panic_next_poll = true;
    fake.set(vec![speakers(false), airpods(true)]);
    // Wakes the poll thread: its first look panics, a later one still follows the AirPods.
    fake.slot().stream_failed();
    assert!(wait_for(Duration::from_secs(2), || {
        events.lock().unwrap().len() == 2
    }));
    assert!(fake.state().panicked);
    assert_eq!(fake.open_id().as_deref(), Some("airpods"));
}

#[test]
fn a_panicking_on_event_does_not_end_the_polling() {
    silence_injected_panics();
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let events = Events::default();
    let panicked = Arc::new(AtomicBool::new(false));
    let on_event = {
        let events = Arc::clone(&events);
        let panicked = Arc::clone(&panicked);
        move |event: OutputEvent| {
            let switched = matches!(event, OutputEvent::Switched { .. });
            events.lock().unwrap().push(event);
            if switched && !panicked.swap(true, Ordering::SeqCst) {
                panic!("{INJECTED_PANIC}");
            }
        }
    };
    let _out = AudioOutput::start(
        Box::new(fake.clone()),
        c,
        p,
        OutputChoice::SystemDefault,
        true,
        Box::new(on_event),
    );
    // The poll thread's own looks: the first switch panics in the callback, the next is
    // still followed.
    fake.set(vec![speakers(false), airpods(true)]);
    assert!(wait_for(Duration::from_secs(2), || panicked.load(Ordering::SeqCst)));
    fake.set(vec![speakers(false), airpods(false), sony(true)]);
    assert!(wait_for(Duration::from_secs(2), || {
        fake.open_id().as_deref() == Some("sony")
    }));
}

#[test]
fn a_stale_failure_with_no_device_does_not_spin_the_poll_thread() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    let slot = fake.slot();
    fake.set(Vec::new());
    out.poll_now();
    // A late report from the stream that is closed now.
    let polls = fake.state().polls;
    slot.stream_failed();
    thread::sleep(Duration::from_millis(400));
    let looks = fake.state().polls - polls;
    assert!(looks <= 2, "{looks} looks in 400 ms");
}

#[test]
fn dropping_the_output_closes_the_stream_and_ends_its_thread() {
    let fake = Fake::new(vec![speakers(true)]);
    let (c, p) = engine_48k();
    let (out, _events) = start(&fake, &c, p, OutputChoice::SystemDefault, true);
    assert!(fake.open_id().is_some());
    assert_eq!(c.lock().unwrap().live().output_latency_frames, FAKE_LATENCY);
    // Let the poll thread settle into its 500 ms wait: dropping must not sit it out.
    thread::sleep(Duration::from_millis(100));
    let dropping = Instant::now();
    drop(out);
    assert!(
        dropping.elapsed() < Duration::from_millis(250),
        "dropping took {:?}",
        dropping.elapsed()
    );
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
