//! The recording tap (live notes with latency-corrected ticks), the metronome and the fade of a
//! removed channel.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    AudioProcessor, ChannelId, ChannelMix, ChannelSetup, Command, ControlError, Controller,
    EngineConfig, EngineState, ExportOptions, PatternData, RECORD_CAPACITY, RecordEvent, SampleBuf,
    engine_with, export_wav, render_offline_with,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const SR: f32 = 48_000.0;
/// Index of the sampler's declick switch.
const DECLICK: usize = 14;
/// Frames in the 5 ms removal fade.
const RAMP: usize = 240;

fn cfg() -> EngineConfig {
    EngineConfig { sample_rate: SR }
}

/// Two seconds of a constant `level`.
fn dc(level: f32) -> Arc<SampleBuf> {
    let n = 2 * SR as usize;
    Arc::new(SampleBuf {
        rate: SR,
        l: vec![level; n],
        r: vec![level; n],
    })
}

/// A sampler channel that plays its sample exactly (declick off).
fn channel(id: u32, sample: &Arc<SampleBuf>) -> ChannelSetup {
    let mut params: Vec<f32> = SPEC.iter().map(|&(_, _, d)| d).collect();
    params[DECLICK] = 0.0;
    ChannelSetup::sampler(
        ChannelId(id),
        Some(Arc::clone(sample)),
        params,
        ChannelMix::default(),
    )
}

/// 120 BPM at 48 kHz: 25 frames a tick.
fn state(channels: Vec<ChannelSetup>) -> EngineState {
    EngineState {
        bpm: 120.0,
        channels,
        ..EngineState::default()
    }
}

fn note(on: bool) -> Command {
    Command::ChannelNote {
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        on,
    }
}

fn recorded(key_on: bool, tick: u64) -> RecordEvent {
    RecordEvent::Note {
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        on: key_on,
        tick,
    }
}

/// A hand-driven engine with one channel, and how many frames it has rendered.
struct Rig {
    c: Controller,
    p: AudioProcessor,
    at: u64,
    out: Vec<f32>,
}

impl Rig {
    fn new() -> Self {
        let (c, p) = engine_with(cfg(), state(vec![channel(1, &dc(0.5))]));
        Self {
            c,
            p,
            at: 0,
            out: Vec::new(),
        }
    }

    /// Renders up to frame `to`, in callbacks of 441 frames, keeping the left channel.
    fn run_to(&mut self, to: u64) {
        let mut buf = [0f32; 441 * 2];
        while self.at < to {
            let n = (to - self.at).min(441) as usize;
            self.p.process(&mut buf[..n * 2], 2);
            self.out.extend(buf[..n * 2].iter().step_by(2));
            self.at += n as u64;
        }
    }

    fn at(&mut self, frame: u64, cmd: Command) {
        self.c.send_at(frame, cmd).unwrap();
    }

    fn take(&mut self) -> Vec<RecordEvent> {
        let mut got = Vec::new();
        let n = self.c.take_recorded(&mut got);
        assert_eq!(n, got.len());
        got
    }
}

#[test]
fn recorded_ticks_match_the_frame_the_note_landed() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(30_000, note(true));
    r.at(31_010, note(false));
    r.run_to(40_000);
    // 30 000 / 25 = 1 200; 31 010 / 25 = 1 240.4, floored.
    assert_eq!(r.take(), [recorded(true, 1_200), recorded(false, 1_240)]);
    assert!(r.take().is_empty(), "drained");
    assert_eq!(r.c.record_dropped(), 0);
}

#[test]
fn recorded_ticks_subtract_the_output_latency() {
    let mut r = Rig::new();
    r.c.live_shared().set_output_latency_frames(2_400);
    r.c.send(Command::SetRecording { on: true }).unwrap();
    // Play starts at frame 6 400 (a block start).
    r.run_to(6_400);
    r.c.send(Command::Play).unwrap();
    // Heard 100 frames after play started: 2 400 frames earlier is before the start.
    r.at(6_500, note(true));
    r.at(6_400 + 30_000, note(true));
    r.run_to(40_000);
    // (30 000 - 2 400) / 25 = 1 104.
    assert_eq!(r.take(), [recorded(true, 0), recorded(true, 1_104)]);
}

#[test]
fn nothing_is_recorded_while_stopped_or_not_recording() {
    let mut r = Rig::new();
    // Recording, but stopped.
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.at(1_000, note(true));
    r.at(2_000, note(false));
    r.run_to(4_000);
    assert!(r.take().is_empty(), "stopped");
    // Playing, but not recording.
    r.c.send(Command::SetRecording { on: false }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(8_000, note(true));
    r.at(9_000, note(false));
    r.run_to(12_000);
    assert!(r.take().is_empty(), "not recording");
    assert!(r.out.iter().any(|x| *x != 0.0), "the notes still played");
}

#[test]
fn stop_and_record_off_push_an_end_marker() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(30_000, note(true));
    // Commands other than notes apply at their block's start: 40 000 is one.
    r.at(40_000, Command::SetRecording { on: false });
    r.at(44_000, note(false));
    r.at(48_000, Command::SetRecording { on: true });
    r.at(50_000, note(false));
    r.at(64_000, Command::Stop);
    // Stopped: neither the record switch nor a note adds anything.
    r.at(70_000, Command::SetRecording { on: false });
    r.at(71_000, note(true));
    r.run_to(80_000);
    assert_eq!(
        r.take(),
        [
            recorded(true, 1_200),
            RecordEvent::End { tick: 1_600 },
            recorded(false, 2_000),
            RecordEvent::End { tick: 2_560 },
        ]
    );
}

#[test]
fn a_full_queue_counts_drops_instead_of_blocking() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    // 100 notes a block (a channel takes 128), never drained.
    let extra = 10;
    let total = RECORD_CAPACITY + extra;
    let mut sent = 0;
    while sent < total {
        for _ in 0..100.min(total - sent) {
            r.c.send(note(sent % 2 == 0)).unwrap();
            sent += 1;
        }
        let to = r.at + 64;
        r.run_to(to);
    }
    assert_eq!(r.c.record_dropped(), extra as u32);
    let got = r.take();
    assert_eq!(got.len(), RECORD_CAPACITY);
    assert!(matches!(got[0], RecordEvent::Note { on: true, .. }));
    // Room again: the next note is recorded, and the count stays.
    r.c.send(note(true)).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    assert_eq!(r.take().len(), 1);
    assert_eq!(r.c.record_dropped(), extra as u32);
}

/// Left channel of an offline render of `events` with no channels.
fn render_clicks(events: &[(u64, Command)], frames: usize, callback: usize) -> Vec<f32> {
    let out = render_offline_with(cfg(), state(Vec::new()), events, frames, callback);
    out.iter().step_by(2).copied().collect()
}

/// Magnitude of `x`'s component at `hz` (one DFT bin).
fn bin(x: &[f32], hz: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / f64::from(SR);
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &s) in x.iter().enumerate() {
        re += f64::from(s) * libm::cos(w * i as f64);
        im -= f64::from(s) * libm::sin(w * i as f64);
    }
    libm::sqrt(re * re + im * im)
}

#[test]
fn the_metronome_clicks_on_every_beat_with_an_accent_on_the_bar() {
    // 3/4 at 120 BPM: a beat is 960 ticks, 24 000 frames.
    let events = [
        (0, Command::SetTimeSignature { beats: 3, unit: 4 }),
        (0, Command::SetMetronome { on: true }),
        (0, Command::Play),
    ];
    let beat = 24_000;
    let l = render_clicks(&events, 7 * beat + 1_000, 64);
    for k in 0..7 {
        let onset = k * beat;
        if k > 0 {
            assert!(
                l[onset - 5_000..onset].iter().all(|x| *x == 0.0),
                "silent before beat {k}"
            );
        }
        let first = l[onset..].iter().position(|x| *x != 0.0).unwrap();
        assert!(first <= 1, "beat {k} starts at its frame (+{first})");
        let burst = &l[onset..onset + 1_440];
        let peak = burst.iter().fold(0f32, |m, x| m.max(x.abs()));
        // -12 dBFS.
        assert!((0.24..=0.2512).contains(&peak), "beat {k} peak {peak}");
        let (low, high) = (bin(burst, 1_000.0), bin(burst, 1_500.0));
        if k % 3 == 0 {
            assert!(high > 4.0 * low, "bar start {k}: {high} vs {low}");
        } else {
            assert!(low > 4.0 * high, "beat {k}: {low} vs {high}");
        }
    }
    // The same output for any callback size.
    for callback in [1, 441, 4_096] {
        assert!(
            render_clicks(&events, 3 * beat, callback) == l[..3 * beat],
            "callback {callback}"
        );
    }
}

#[test]
fn the_metronome_is_silent_when_stopped_and_off_by_default() {
    let frames = 2 * SR as usize;
    let off = render_clicks(&[(0, Command::Play)], frames, 64);
    assert!(off.iter().all(|x| *x == 0.0), "off by default");
    let stopped = render_clicks(&[(0, Command::SetMetronome { on: true })], frames, 64);
    assert!(stopped.iter().all(|x| *x == 0.0), "stopped");
    // On, then stopped after the first click has rung out.
    let events = [
        (0, Command::SetMetronome { on: true }),
        (0, Command::Play),
        (20_000, Command::Stop),
    ];
    let l = render_clicks(&events, frames, 64);
    assert!(l[..20_000].iter().any(|x| *x != 0.0), "it clicked");
    assert!(l[20_000..].iter().all(|x| *x == 0.0), "and stopped");
}

#[test]
fn exports_never_contain_the_metronome() {
    // No notes: anything in the file would be a click.
    let st = EngineState {
        time_signature: (3, 4),
        pattern: PatternData {
            length: 4 * 3_840,
            notes: Vec::new(),
        },
        ..state(Vec::new())
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("silence.wav");
    let report = export_wav(
        st,
        &ExportOptions::default(),
        &path,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(report.frames >= 4 * 2 * SR as u64, "four bars written");
    assert_eq!(report.peak, 0.0);
}

#[test]
fn removing_a_channel_fades_it_and_drops_its_live_notes() {
    let held = dc(1.0);
    let (mut c, mut p) = engine_with(cfg(), state(vec![channel(1, &held)]));
    let mut buf = vec![0f32; 4_800 * 2];
    c.send(note(true)).unwrap();
    p.process(&mut buf, 2);
    // The test, the controller's state and the graph's node.
    assert_eq!(Arc::strong_count(&held), 3);
    c.remove_channel(ChannelId(1)).unwrap();
    assert_eq!(
        c.send(note(true)),
        Err(note(true)),
        "a note for the removed id is refused"
    );
    assert_eq!(
        c.remove_channel(ChannelId(1)),
        Err(ControlError::UnknownChannel)
    );
    // A new channel may take the id at once, and a note for the id reaches it, not the one
    // fading out.
    let other = dc(0.25);
    c.add_channel(channel(1, &other)).unwrap();
    c.send(note(true)).unwrap();
    p.process(&mut buf, 2);
    let l: Vec<f32> = buf.iter().step_by(2).copied().collect();
    let max_step = l.windows(2).fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(max_step <= 1.0 / RAMP as f32 + 1e-6, "step {max_step}");
    assert!(l[0] > 1.24, "fades from where it was, over the new note");
    assert!(
        l[RAMP..].iter().all(|x| *x == 0.25),
        "only the new channel after 5 ms"
    );
    c.check_health();
    assert_eq!(Arc::strong_count(&held), 1, "freed off the audio thread");
    assert_eq!(Arc::strong_count(&other), 3);
}

#[test]
fn the_time_signature_is_mirrored_and_outlives_a_new_state_and_a_rate_change() {
    let (mut c, mut p) = engine_with(cfg(), state(Vec::new()));
    assert_eq!(c.state().time_signature, (4, 4), "4/4 by default");
    c.send(Command::SetTimeSignature { beats: 2, unit: 4 })
        .unwrap();
    assert_eq!(c.state().time_signature, (2, 4));
    let bad = Command::SetTimeSignature { beats: 4, unit: 5 };
    assert_eq!(c.send(bad), Err(bad));
    let bad_state = EngineState {
        time_signature: (0, 4),
        ..state(Vec::new())
    };
    assert_eq!(c.load_state(bad_state), Err(ControlError::Rejected));
    // A new state brings its own; the switches queued before a rate change still apply.
    let waltz = EngineState {
        time_signature: (3, 4),
        ..state(Vec::new())
    };
    c.load_state(waltz).unwrap();
    assert_eq!(c.state().time_signature, (3, 4));
    c.send(Command::SetMetronome { on: true }).unwrap();
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 24_000.0,
        },
    );
    // 120 BPM at 24 kHz: a beat is 12 000 frames.
    let beat = 12_000;
    let mut out = vec![0f32; 7 * beat * 2];
    p.process(&mut out, 2);
    let l: Vec<f32> = out.iter().step_by(2).copied().collect();
    let bin24 = |x: &[f32], hz: f64| bin(x, hz * 2.0);
    let got: Vec<bool> = (0..7)
        .map(|k| {
            let burst = &l[k * beat..k * beat + 720];
            bin24(burst, 1_500.0) > bin24(burst, 1_000.0)
        })
        .collect();
    assert_eq!(got, [true, false, false, true, false, false, true]);
    // 84 000 frames rendered (1 312.5 blocks): Stop applies at the next block, frame 84 032,
    // 12.5 frames a tick.
    c.send(Command::Stop).unwrap();
    p.process(&mut out[..128], 2);
    let mut taken = Vec::new();
    c.take_recorded(&mut taken);
    assert_eq!(taken, [RecordEvent::End { tick: 6_722 }], "recording too");
}
