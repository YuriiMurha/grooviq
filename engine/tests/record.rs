//! The recording tap (live notes with latency-corrected ticks), the metronome and the fade of a
//! removed channel.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    AudioProcessor, ChannelId, ChannelMix, ChannelSetup, Command, ControlError, Controller,
    EngineConfig, EngineState, ExportOptions, NoteId, PatternData, RECORD_CAPACITY, RecordEvent,
    SampleBuf, engine_with, export_wav, render_offline_with,
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
        id: NoteId(60),
    }
}

/// Ticks in the default 16-step pattern.
const LOOP: u64 = 3_840;

/// A note recorded at `tick`, in the default pattern (no resize: its position is the tick in
/// the loop), in take 1 (the first Record-on starts it).
fn recorded(key_on: bool, tick: u64) -> RecordEvent {
    RecordEvent::Note {
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        on: key_on,
        tick,
        position: (tick % LOOP) as u32,
        take: 1,
    }
}

fn end(tick: u64) -> RecordEvent {
    RecordEvent::End {
        tick,
        position: (tick % LOOP) as u32,
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
            end(1_600),
            // The End and the next Record-on each move the take number on.
            in_take(recorded(false, 2_000), 3),
            end(2_560),
        ]
    );
}

#[test]
fn the_take_number_moves_on_even_when_the_end_is_dropped() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    // 100 notes a block, never drained, until the queue is full.
    let mut sent = 0;
    while sent < RECORD_CAPACITY + 10 {
        for _ in 0..100 {
            r.c.send(note(sent % 2 == 0)).unwrap();
            sent += 1;
        }
        let to = r.at + 64;
        r.run_to(to);
    }
    assert!(r.c.record_dropped() > 0);
    // The take ends while the queue is full: its End is dropped.
    r.c.send(Command::SetRecording { on: false }).unwrap();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    let first = r.take();
    assert!(!first.iter().any(|e| matches!(e, RecordEvent::End { .. })));
    assert!(
        first
            .iter()
            .all(|e| matches!(e, RecordEvent::Note { take: 1, .. }))
    );
    r.c.send(note(true)).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    let next = r.take();
    assert!(
        // The dropped End moved the number on, and so did the Record-on.
        matches!(next[..], [RecordEvent::Note { take: 3, .. }]),
        "{next:?}"
    );
}

/// `event` (a note) as recorded in take `take`.
fn in_take(event: RecordEvent, take: u32) -> RecordEvent {
    match event {
        RecordEvent::Note {
            channel,
            key,
            velocity,
            on,
            tick,
            position,
            ..
        } => RecordEvent::Note {
            channel,
            key,
            velocity,
            on,
            tick,
            position,
            take,
        },
        other => other,
    }
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
fn the_metronome_follows_a_tempo_change_in_the_middle_of_a_beat() {
    // Play at frame 6 400 at 120 BPM (a beat is 24 000 frames, 25 frames a tick): beat 0 at
    // 6 400. At 18 432, 481.28 ticks into beat 0, the tempo halves, so the 478.72 ticks left
    // take 50 frames each: beat 1 lands at 42 368 and beat 2 at 90 368 (not at 30 400, 54 400
    // and 78 400 as at 120).
    let events = [
        (0, Command::SetMetronome { on: true }),
        (6_400, Command::Play),
        (18_432, Command::SetTempo { bpm: 60.0 }),
    ];
    let l = render_clicks(&events, 92_000, 64);
    let peak = |a: usize, b: usize| l[a..b].iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(l[..6_400].iter().all(|x| *x == 0.0), "silent before play");
    for (beat, onset) in [6_400, 42_368, 90_368].into_iter().enumerate() {
        // Each burst has ended long before the next starts.
        assert!(
            l[onset - 1_440..onset].iter().all(|x| *x == 0.0),
            "silent before beat {beat}"
        );
        let first = l[onset..].iter().position(|x| *x != 0.0).unwrap();
        assert!(first <= 1, "beat {beat} starts at {onset} (+{first})");
        assert!(peak(onset, onset + 1_440) > 0.2, "beat {beat} at {onset}");
    }
    // Where the old tempo would have put the beats: silence or a fading tail.
    for stale in [30_400, 54_400, 78_400] {
        assert!(peak(stale, stale + 1_440) < 0.01, "a click at {stale}");
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
    let mark = c.record_mark();
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
    assert_eq!(
        taken,
        [RecordEvent::Mark { id: mark }, end(6_722)],
        "recording too"
    );
}

/// The tick each note-on at the given frames was recorded at.
fn ticks(events: &[RecordEvent]) -> Vec<u64> {
    events
        .iter()
        .map(|e| match e {
            RecordEvent::Note { tick, .. } | RecordEvent::End { tick, .. } => *tick,
            RecordEvent::Mark { .. } => unreachable!("no new state in these tests"),
        })
        .collect()
}

/// Where in the loop each event was heard.
fn positions(events: &[RecordEvent]) -> Vec<u32> {
    events
        .iter()
        .map(|e| match e {
            RecordEvent::Note { position, .. } | RecordEvent::End { position, .. } => *position,
            RecordEvent::Mark { .. } => unreachable!("no new state in these tests"),
        })
        .collect()
}

fn resize(length: u32) -> PatternData {
    PatternData {
        length,
        notes: Vec::new(),
    }
}

#[test]
fn recorded_ticks_follow_the_tempo_at_the_heard_frame() {
    // 120 BPM (25 frames a tick) from 0, 60 BPM (50) from 3 200, 240 BPM (12.5) from 4 800:
    // ticks 128 and 160 at the changes. Each note is heard 2 400 frames before it lands.
    let mut r = Rig::new();
    r.c.live_shared().set_output_latency_frames(2_400);
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(2_500, note(true));
    r.at(3_200, Command::SetTempo { bpm: 60.0 });
    r.at(4_800, Command::SetTempo { bpm: 240.0 });
    for frame in [5_600, 5_601, 7_199, 7_200, 8_000] {
        r.at(frame, note(true));
    }
    r.run_to(9_000);
    // Heard at 100, 3 200, 3 201, 4 799, 4 800 and 5 600.
    assert_eq!(ticks(&r.take()), [4, 128, 128, 159, 160, 224]);

    // The reviewer's case: 60 BPM from 6 400, a note at 6 500 was heard at 4 100 (tick 164).
    let mut r = Rig::new();
    r.c.live_shared().set_output_latency_frames(2_400);
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(6_400, Command::SetTempo { bpm: 60.0 });
    r.at(6_500, note(true));
    r.run_to(7_000);
    assert_eq!(ticks(&r.take()), [164]);
}

#[test]
fn recorded_ticks_follow_a_sample_rate_change() {
    // 48 kHz (25 frames a tick) until 6 400 (tick 256), then 96 kHz (50 frames a tick).
    let mut r = Rig::new();
    r.c.live_shared().set_output_latency_frames(2_400);
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.run_to(6_400);
    r.c.reconfigure(
        &mut r.p,
        EngineConfig {
            sample_rate: 96_000.0,
        },
    );
    r.at(6_500, note(true));
    r.at(11_400, note(true));
    r.run_to(12_000);
    // Heard at 4 100 (48 kHz: 164) and 9 000 (96 kHz: 256 + 2 600 / 50).
    assert_eq!(ticks(&r.take()), [164, 308]);
}

#[test]
fn a_note_heard_before_play_started_records_at_tick_0() {
    // Play at 6 400 at 120 BPM, 60 BPM from 6 464 (tick 2.56 there).
    let mut r = Rig::new();
    r.c.live_shared().set_output_latency_frames(2_400);
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.run_to(6_400);
    r.c.send(Command::Play).unwrap();
    r.at(6_464, Command::SetTempo { bpm: 60.0 });
    // Heard at 4 100, 6 400 (play start), 6 464 and 6 514.
    for frame in [6_500, 8_800, 8_864, 8_914] {
        r.at(frame, note(true));
    }
    r.run_to(9_000);
    assert_eq!(ticks(&r.take()), [0, 0, 2, 3]);
}

#[test]
fn held_notes_keep_their_length_across_pattern_resizes() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    // Across the loop end of the default 3 840-tick pattern (frame 96 000).
    r.at(95_000, note(true));
    r.at(97_000, note(false));
    // Held while the pattern grows to 7 680 ticks at 120 000.
    r.at(119_000, note(true));
    r.run_to(120_000);
    r.c.set_pattern(resize(7_680)).unwrap();
    r.at(120_000, note(false));
    // Held while it shrinks to 1 920 ticks at 140 032.
    r.at(139_000, note(true));
    r.run_to(140_032);
    r.c.set_pattern(resize(1_920)).unwrap();
    r.at(141_000, note(false));
    r.run_to(142_000);
    let got = r.take();
    assert_eq!(ticks(&got), [3_800, 3_880, 4_760, 4_800, 5_560, 5_640]);
    // Where they were heard: the growth kept the place in the loop (shift 3 840), the shrink
    // wrapped it into 1 920 ticks.
    assert_eq!(positions(&got), [3_800, 40, 920, 960, 1_720, 1_800]);
}

#[test]
fn positions_wrap_at_the_loop_end_and_follow_a_resize() {
    // The #142 case: 16 -> 32 steps at played tick 4 800 (frame 120 000); playback keeps its
    // place (960 into the loop), so a note 240 ticks later was heard at 1 200.
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(96_250, note(true));
    r.at(119_000, note(true));
    r.run_to(120_000);
    r.c.set_pattern(resize(7_680)).unwrap();
    r.at(126_000, note(true));
    r.run_to(127_000);
    let got = r.take();
    assert_eq!(ticks(&got), [3_850, 4_760, 5_040]);
    assert_eq!(positions(&got), [10, 920, 1_200]);

    // 16 -> 24 steps at the same place: the shift is 1 920. A note at played tick 10 000 is
    // past the new loop's end: (10 000 + 1 920) % 5 760.
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.run_to(120_000);
    r.c.set_pattern(resize(5_760)).unwrap();
    r.at(126_000, note(true));
    r.at(250_000, note(true));
    r.run_to(251_000);
    let got = r.take();
    assert_eq!(ticks(&got), [5_040, 10_000]);
    assert_eq!(positions(&got), [1_200, 400]);
}

#[test]
fn shrinking_while_playing_moves_later_positions_into_the_shorter_loop() {
    // 3 840 -> 1 920 ticks at frame 75 008 (played tick 3 000.32, past the new end).
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(74_000, note(true));
    r.run_to(75_008);
    r.c.set_pattern(resize(1_920)).unwrap();
    r.at(76_000, note(true));
    r.run_to(77_000);
    let got = r.take();
    assert_eq!(ticks(&got), [2_960, 3_040]);
    assert_eq!(positions(&got), [2_960, 1_120]);
}

#[test]
fn a_resize_inside_the_latency_window_keeps_the_old_loop_for_notes_heard_before_it() {
    // A resize at frame 96 256, 11 ticks into the second loop. With 2 400 frames of latency, a
    // note landing at 96 300 was heard at 93 900 (tick 3 756), still in the first loop of the
    // old pattern; one landing at 99 000 was heard at 96 600 (tick 3 864), after the resize.
    for (length, after) in [(1_920, 24), (7_680, 24)] {
        let mut r = Rig::new();
        r.c.live_shared().set_output_latency_frames(2_400);
        r.c.send(Command::SetRecording { on: true }).unwrap();
        r.c.send(Command::Play).unwrap();
        r.run_to(96_256);
        r.c.set_pattern(resize(length)).unwrap();
        r.at(96_300, note(true));
        r.at(99_000, note(true));
        r.run_to(100_000);
        let got = r.take();
        assert_eq!(ticks(&got), [3_756, 3_864], "{length}");
        assert_eq!(positions(&got), [3_756, after], "{length}");
    }
}

#[test]
fn stop_and_all_sound_off_end_a_click_within_1_ms() {
    // A click at frame 0 is in its decay at 128; in 4/16 at 120 BPM the second click starts
    // at 6 000 and is in its attack at block 6 016.
    for (stop, signature) in [(128, (4, 4)), (6_016, (4, 16))] {
        for cut in [
            Command::Stop,
            Command::AllSoundOff,
            Command::SetMetronome { on: false },
        ] {
            let (beats, unit) = signature;
            let events = [
                (0, Command::SetTimeSignature { beats, unit }),
                (0, Command::SetMetronome { on: true }),
                (0, Command::Play),
                (stop, cut),
            ];
            let l = render_clicks(&events, 12_000, 64);
            let stop = stop as usize;
            let peak = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
            // A fade, not a cut: it still sounds after the stop, falling off over 1 ms.
            let (early, late) = (peak(&l[stop..stop + 16]), peak(&l[stop + 32..stop + 48]));
            assert!(
                early > 0.0 && late < 0.5 * early,
                "{cut:?} at {stop}: {early} {late}"
            );
            assert!(
                l[stop + 48..].iter().all(|x| *x == 0.0),
                "{cut:?} at {stop}: silent 1 ms later"
            );
        }
    }
}

/// Every new state is a recording barrier: what was recorded before it comes out first, then a
/// numbered `Mark`, whether a take was running or not.
#[test]
fn a_new_state_queues_a_mark_behind_what_was_recorded_before_it() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(30_000, note(true));
    r.run_to(32_000);
    r.c.load_state(state(vec![channel(1, &dc(0.5))])).unwrap();
    let first = r.c.record_mark();
    r.run_to(34_000);
    let got = r.take();
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0], recorded(true, 1_200));
    assert!(matches!(got[1], RecordEvent::End { .. }), "{got:?}");
    assert_eq!(got[2], RecordEvent::Mark { id: first });

    // Stopped and not recording: still a mark, with the next number.
    r.c.send(Command::SetRecording { on: false }).unwrap();
    r.c.load_state(state(vec![channel(1, &dc(0.5))])).unwrap();
    let second = r.c.record_mark();
    assert_ne!(second, first);
    r.run_to(36_000);
    assert_eq!(r.take(), [RecordEvent::Mark { id: second }]);
}

/// A mark that finds the queue full waits and is retried every block, and nothing recorded
/// after it can overtake it: the order of the barrier holds.
#[test]
fn a_mark_that_finds_the_queue_full_goes_out_first_later() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    let mut sent = 0;
    while sent < RECORD_CAPACITY {
        for _ in 0..100.min(RECORD_CAPACITY - sent) {
            r.c.send(note(sent % 2 == 0)).unwrap();
            sent += 1;
        }
        let to = r.at + 64;
        r.run_to(to);
    }
    // Full: the stop's End is dropped (counted), the mark waits; a note in the same block
    // after it is dropped rather than going ahead of it.
    r.c.load_state(state(vec![channel(1, &dc(0.5))])).unwrap();
    let mark = r.c.record_mark();
    r.c.send(Command::Play).unwrap();
    r.c.send(note(true)).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    let dropped = r.c.record_dropped();
    let got = r.take();
    assert_eq!(got.len(), RECORD_CAPACITY);
    assert!(!got.contains(&RecordEvent::Mark { id: mark }));
    // Room again: the mark goes out at the next block, before the next note.
    r.c.send(note(false)).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    let got = r.take();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0], RecordEvent::Mark { id: mark });
    assert!(
        matches!(got[1], RecordEvent::Note { on: false, .. }),
        "{got:?}"
    );
    assert_eq!(
        r.c.record_dropped(),
        dropped,
        "the mark itself is never counted lost"
    );
}

/// A command sent with a barrier: what its take recorded, its End included, comes before the
/// mark. Both or neither are queued.
#[test]
fn a_marked_record_off_ends_the_take_before_its_mark() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.at(30_000, note(true));
    r.run_to(32_000);
    let id =
        r.c.send_marked(Command::SetRecording { on: false })
            .unwrap();
    assert_eq!(id, r.c.record_mark());
    r.run_to(34_000);
    let got = r.take();
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0], recorded(true, 1_200));
    assert!(matches!(got[1], RecordEvent::End { .. }), "{got:?}");
    assert_eq!(got[2], RecordEvent::Mark { id });
    // A full command queue takes neither.
    while r.c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    assert_eq!(
        r.c.send_marked(Command::SetRecording { on: true }),
        Err(ControlError::Full)
    );
    assert_eq!(r.c.record_mark(), id, "no mark was used up");
}

/// With exactly one free slot, a command and its barrier do not both fit: neither is queued,
/// and no mark id is used up. (The re-review's probe.)
#[test]
fn a_marked_command_with_exactly_one_free_slot_is_refused_whole() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.run_to(1_000);
    // The queue's capacity, found through the public API; then start over empty.
    let mut capacity = 0;
    while r.c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {
        capacity += 1;
    }
    r.run_to(2_000);
    for _ in 0..capacity - 1 {
        r.c.send(Command::SetTempo { bpm: 120.0 }).unwrap();
    }
    let mark = r.c.record_mark();
    assert_eq!(
        r.c.send_marked(Command::SetRecording { on: false }),
        Err(ControlError::Full)
    );
    assert_eq!(r.c.record_mark(), mark);
    // The free slot is still free, and the record-off did not enter the queue.
    r.c.send(note(true)).unwrap();
    assert!(r.c.send(note(false)).is_err());
    r.run_to(3_000);
    assert!(
        r.take()
            .iter()
            .any(|e| matches!(e, RecordEvent::Note { on: true, .. }))
    );
}

#[test]
fn every_record_on_starts_a_new_take() {
    let mut r = Rig::new();
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(Command::Play).unwrap();
    r.c.send(note(true)).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    // Record-on again while it is on (a switch-off that never reached the engine was
    // cancelled): a new take all the same.
    r.c.send(Command::SetRecording { on: true }).unwrap();
    r.c.send(note(false)).unwrap();
    let to = r.at + 64;
    r.run_to(to);
    let got = r.take();
    assert!(
        matches!(
            got[..],
            [
                RecordEvent::Note {
                    take: 1,
                    on: true,
                    ..
                },
                RecordEvent::Note {
                    take: 2,
                    on: false,
                    ..
                }
            ]
        ),
        "{got:?}"
    );
}
