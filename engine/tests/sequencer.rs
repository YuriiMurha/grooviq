//! The transport and the step sequencer: notes land on their exact frame at any rate and tempo,
//! the pattern loops without drift, and tempo or pattern changes mid-play keep the grid.
//!
//! Every test plays a click (one 1.0, then zeros; declick off, one-shot) so each note is one
//! frame above the onset threshold, and a velocity of 1 makes that frame exactly 1.0.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    BLOCK, ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig, EngineState, PPQ,
    PatternData, STEP_TICKS, SampleBuf, SeqNote, engine_with, render_offline_with, ticks_to_frames,
};
use std::sync::Arc;

const SR: f32 = 48_000.0;
/// Sampler parameter indices: one-shot switch, release time (ms), declick switch.
const ONE_SHOT: usize = 7;
const RELEASE: usize = 11;
const DECLICK: usize = 14;
/// A frame louder than this is an onset.
const THRESHOLD: f32 = 0.05;

fn cfg(sample_rate: f32) -> EngineConfig {
    EngineConfig { sample_rate }
}

fn click() -> Arc<SampleBuf> {
    let mut l = vec![0.0; 16];
    l[0] = 1.0;
    Arc::new(SampleBuf {
        rate: SR,
        r: l.clone(),
        l,
    })
}

/// Two seconds of 0.5.
fn dc() -> Arc<SampleBuf> {
    let n = 2 * SR as usize;
    Arc::new(SampleBuf {
        rate: SR,
        l: vec![0.5; n],
        r: vec![0.5; n],
    })
}

/// Sampler parameters at their defaults with declick off (one-shot by default).
fn exact() -> Vec<f32> {
    let mut p: Vec<f32> = SPEC.iter().map(|&(_, _, d)| d).collect();
    p[DECLICK] = 0.0;
    p
}

fn channel(id: u32, sample: &Arc<SampleBuf>, sampler: Vec<f32>) -> ChannelSetup {
    ChannelSetup {
        id: ChannelId(id),
        sample: Some(Arc::clone(sample)),
        sampler,
        mix: ChannelMix::default(),
    }
}

fn note(tick: u32, channel: u32, length: u32) -> SeqNote {
    SeqNote {
        tick,
        channel: ChannelId(channel),
        key: 60,
        velocity: 1.0,
        length,
    }
}

/// A pattern of `steps` steps with a one-step note on channel 1 at each tick in `ticks`.
fn pattern(steps: u32, ticks: &[u32]) -> PatternData {
    PatternData {
        length: steps * STEP_TICKS,
        notes: ticks.iter().map(|&t| note(t, 1, STEP_TICKS)).collect(),
    }
}

fn steps(steps: &[u32]) -> Vec<u32> {
    steps.iter().map(|s| s * STEP_TICKS).collect()
}

/// One click channel playing `pattern` at `bpm`.
fn clicks(bpm: f32, pattern: PatternData) -> EngineState {
    EngineState {
        bpm,
        channels: vec![channel(1, &click(), exact())],
        pattern,
    }
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

/// Frames louder than the threshold.
fn onsets(inter: &[f32]) -> Vec<u64> {
    left(inter)
        .iter()
        .enumerate()
        .filter(|(_, x)| x.abs() > THRESHOLD)
        .map(|(i, _)| i as u64)
        .collect()
}

fn play(
    state: EngineState,
    sample_rate: f32,
    events: &[(u64, Command)],
    frames: usize,
) -> Vec<f32> {
    let mut ev = vec![(0, Command::Play)];
    ev.extend_from_slice(events);
    render_offline_with(cfg(sample_rate), state, &ev, frames, 64)
}

fn block_start(frame: u64) -> u64 {
    frame / BLOCK as u64 * BLOCK as u64
}

/// What happens at the block containing a frame.
#[derive(Clone)]
enum Action {
    Send(Command),
    Pattern(PatternData),
}

/// Drives the engine by hand with `callback` frames per call: every action goes in just
/// before the block that contains its frame (commands then apply at its start, as do pattern
/// swaps), so the output does not depend on `callback`.
fn drive(state: EngineState, script: &[(u64, Action)], frames: usize, callback: usize) -> Vec<f32> {
    let (mut c, mut p) = engine_with(cfg(SR), state);
    let mut out = vec![0.0; frames * 2];
    let mut pos = 0usize;
    let mut script = script.iter().peekable();
    while pos < frames {
        while let Some((frame, action)) = script.peek() {
            if block_start(*frame) as usize > pos {
                break;
            }
            match action {
                Action::Send(cmd) => c.send(*cmd).unwrap(),
                Action::Pattern(pat) => c.set_pattern(pat.clone()).unwrap(),
            }
            script.next();
        }
        let next_action = script
            .peek()
            .map_or(frames, |(f, _)| block_start(*f) as usize);
        let end = (pos + callback).min(frames).min(next_action.max(pos + 1));
        p.process(&mut out[pos * 2..end * 2], 2);
        pos = end;
        c.check_health();
    }
    out
}

#[test]
fn steps_land_on_the_exact_frame_at_44k1_and_48k() {
    let pat = || pattern(16, &steps(&[0, 3, 9]));
    let at48 = onsets(&play(clicks(120.0, pat()), 48_000.0, &[], 60_000));
    assert_eq!(at48, [0, 18_000, 54_000]);
    let at44 = onsets(&play(clicks(120.0, pat()), 44_100.0, &[], 55_000));
    assert_eq!(at44, [0, 16_538, 49_613]);
    for (step, frame) in [(3u32, 16_538u64), (9, 49_613)] {
        let ticks = f64::from(step * STEP_TICKS);
        assert_eq!(ticks_to_frames(ticks, 120.0, 44_100.0), frame);
    }
    // And each onset is one voice at full level.
    let l = left(&play(clicks(120.0, pat()), 48_000.0, &[], 60_000));
    assert_eq!(l[18_000], 1.0);
}

#[test]
fn fractional_frames_round_up() {
    let out = play(clicks(140.0, pattern(16, &steps(&[1]))), SR, &[], 10_000);
    assert_eq!(onsets(&out), [5_143]);
    assert_eq!(ticks_to_frames(f64::from(STEP_TICKS), 140.0, SR), 5_143);
}

#[test]
fn rolls_on_fractional_steps() {
    let ticks = [960, 1_020, 1_080, 1_140];
    let out = play(clicks(120.0, pattern(16, &ticks)), SR, &[], 40_000);
    assert_eq!(onsets(&out), [24_000, 25_500, 27_000, 28_500]);
}

#[test]
fn the_pattern_loops_without_drift() {
    let length = 16 * STEP_TICKS;
    let end = ticks_to_frames(f64::from(50 * length), 140.0, SR);
    let out = play(clicks(140.0, pattern(16, &[0])), SR, &[], end as usize);
    let expected: Vec<u64> = (0..50)
        .map(|k| ticks_to_frames(f64::from(k * length), 140.0, SR))
        .collect();
    assert_eq!(onsets(&out), expected);
}

#[test]
fn tempo_change_mid_play_moves_the_following_steps_only() {
    let every: Vec<u32> = (0..16).collect();
    let state = || clicks(120.0, pattern(16, &steps(&every)));
    let change = 30_000;
    let slow = Command::SetTempo { bpm: 60.0 };
    let out = play(state(), SR, &[(change, slow)], 200_000);
    let steady = onsets(&play(state(), SR, &[], 200_000));
    // The command applies at the start of the block that holds its frame.
    let fc = block_start(change);
    let before: Vec<u64> = steady.iter().copied().filter(|&f| f < fc).collect();
    let got = onsets(&out);
    assert_eq!(got[..before.len()], before[..], "earlier steps unchanged");
    // From the tick reached at that block start, the steps follow 60 BPM.
    let frames_per_tick = f64::from(SR) * 60.0 / (120.0 * f64::from(PPQ));
    let reached = fc as f64 / frames_per_tick;
    let later: Vec<u64> = (0u32..)
        .map(|k| k * STEP_TICKS)
        .filter(|&t| f64::from(t) > reached)
        .map(|t| fc + ticks_to_frames(f64::from(t) - reached, 60.0, SR))
        .take_while(|&f| f < 200_000)
        .collect();
    assert_eq!(got[before.len()..], later[..], "later steps at 60 BPM");
    assert!(later.len() >= 10);
    // 12 000 frames apart at 60 BPM (give or take the rounding of each).
    assert!(
        later
            .windows(2)
            .all(|w| w[1] - w[0] >= 11_999 && w[1] - w[0] <= 12_001)
    );
}

#[test]
fn editing_the_pattern_mid_play_keeps_the_grid() {
    // Both patterns hold tick 1 600, which lands on frame 40 000: the swap block's start.
    let a = pattern(16, &[0, 960, 1_600, 1_920, 2_880]);
    let mut b_ticks = steps(&(0..16).collect::<Vec<_>>());
    b_ticks.push(1_600);
    b_ticks.sort_unstable();
    let b = pattern(16, &b_ticks);
    let swap = 40_000;
    assert_eq!(block_start(swap), swap);
    assert_eq!(ticks_to_frames(1_600.0, 120.0, SR), swap);
    let frames = 200_000;
    let script = |pat: Option<&PatternData>| {
        let mut s = vec![(0, Action::Send(Command::Play))];
        if let Some(pat) = pat {
            s.push((swap, Action::Pattern(pat.clone())));
        }
        s
    };
    let edited = drive(clicks(120.0, a.clone()), &script(Some(&b)), frames, 64);
    let only_a = onsets(&drive(clicks(120.0, a), &script(None), frames, 64));
    let only_b = onsets(&drive(clicks(120.0, b), &script(None), frames, 64));
    let got = onsets(&edited);
    let mut expected: Vec<u64> = only_a.iter().copied().filter(|&f| f < swap).collect();
    expected.extend(only_b.iter().filter(|&&f| f >= swap));
    assert_eq!(got, expected);
    // Every onset is one voice: nothing fired twice on one frame.
    let l = left(&edited);
    assert!(got.iter().all(|&f| l[f as usize] == 1.0));
    assert!(
        got.contains(&swap),
        "the note on the swap block's start fired"
    );
}

#[test]
fn shortening_the_pattern_mid_play_wraps_cleanly() {
    let every: Vec<u32> = (0..32).collect();
    let long = pattern(32, &steps(&every));
    let short = pattern(16, &steps(&[0, 4, 8, 12]));
    // Past step 21 (6 000 frames a step at 120 BPM).
    let swap = block_start(21 * 6_000 + 3_000);
    let frames = 400_000;
    let script = vec![
        (0, Action::Send(Command::Play)),
        (swap, Action::Pattern(short.clone())),
    ];
    let got = onsets(&drive(clicks(120.0, long), &script, frames, 64));
    let short_only = onsets(&drive(
        clicks(120.0, short),
        &[(0, Action::Send(Command::Play))],
        frames,
        64,
    ));
    let before: Vec<u64> = (0..=21).map(|s| s * 6_000).collect();
    let after: Vec<u64> = short_only.iter().copied().filter(|&f| f >= swap).collect();
    // Tick 5 158.4 is tick 1 318.4 of the short pattern: its next note is step 8 (frame
    // 144 000), with nothing in between.
    assert_eq!(after[0], 144_000);
    let mut expected = before;
    expected.extend(after);
    assert_eq!(got, expected);
}

/// A pattern of `steps` steps with a one-step note on every step, whose velocity names it:
/// step `s` plays at `0.25 + s / 100`, so an onset's level says which step it was.
fn numbered(steps: u32) -> PatternData {
    PatternData {
        length: steps * STEP_TICKS,
        notes: (0..steps)
            .map(|s| SeqNote {
                velocity: step_level(s),
                ..note(s * STEP_TICKS, 1, STEP_TICKS)
            })
            .collect(),
    }
}

fn step_level(step: u32) -> f32 {
    0.25 + step as f32 / 100.0
}

/// Every onset as (frame, step), the step read back from its level.
fn numbered_onsets(inter: &[f32]) -> Vec<(u64, u32)> {
    let l = left(inter);
    onsets(inter)
        .into_iter()
        .map(|f| {
            let step = ((l[f as usize] - 0.25) * 100.0).round() as u32;
            // One voice at its own level: never two notes on one frame.
            assert!((l[f as usize] - step_level(step)).abs() < 1e-6, "frame {f}");
            (f, step)
        })
        .collect()
}

#[test]
fn changing_the_pattern_length_mid_play_keeps_the_place_in_the_loop() {
    // 16 steps, then a new length during step 4 of the second or third loop. The place in
    // the loop (step 4) carries on into the new pattern: step 5 next, on the frame a run
    // without the swap gives it, every step once, then the new pattern's own wrap.
    for new_steps in [32u32, 17, 15] {
        for loop_index in [1u64, 2] {
            let at_step = 16 * loop_index + 4;
            // 6 000 frames a step at 120 BPM: 100 frames into step 4, rounded to its block.
            let swap = block_start(at_step * 6_000 + 100);
            assert!(swap > at_step * 6_000);
            let total_steps = at_step + 1 + 2 * u64::from(new_steps);
            let frames = (total_steps * 6_000) as usize;
            let script = vec![
                (0, Action::Send(Command::Play)),
                (swap, Action::Pattern(numbered(new_steps))),
            ];
            let got = numbered_onsets(&drive(clicks(120.0, numbered(16)), &script, frames, 64));
            let steady = numbered_onsets(&drive(
                clicks(120.0, numbered(16)),
                &[(0, Action::Send(Command::Play))],
                frames,
                64,
            ));
            let expected: Vec<(u64, u32)> = (0..total_steps)
                .map(|n| {
                    let step = if n <= at_step {
                        n % 16
                    } else {
                        (4 + n - at_step) % u64::from(new_steps)
                    };
                    (n * 6_000, step as u32)
                })
                .collect();
            let case = format!("16 -> {new_steps} steps in loop {}", loop_index + 1);
            assert_eq!(got, expected, "{case}");
            // The first onset after the swap is on the frame the unswapped run gives step 5.
            let next = got.iter().find(|(f, _)| *f >= swap).unwrap();
            assert_eq!(*next, steady[at_step as usize + 1], "{case}");
            assert_eq!(next.1, 5, "{case}");
        }
    }
}

#[test]
fn a_note_off_booked_before_a_tempo_jump_does_not_cut_a_newer_note() {
    // Gate mode, one key: note A (step 0, one step) books its note-off at frame 6 000 at
    // 120 BPM. At 400 BPM from frame 3 008, note B (step 1, eight steps) starts at frame
    // 3 906, before A's note-off arrives. That note-off releases A only: B holds until its
    // own end (frame 18 306).
    let mut gate = exact();
    gate[ONE_SHOT] = 0.0;
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &dc(), gate)],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(0, 1, STEP_TICKS), note(STEP_TICKS, 1, 8 * STEP_TICKS)],
        },
    };
    let jump = 3_008;
    assert_eq!(block_start(jump), jump);
    // The next loop starts at frame 30 906.
    let out = play(
        state,
        SR,
        &[(jump, Command::SetTempo { bpm: 400.0 })],
        30_000,
    );
    let l = left(&out);
    // A and B together, then B alone (A's 40 ms release is far below -60 dB by 15 000).
    assert!((l[5_000] - 1.0).abs() < 1e-3, "{}", l[5_000]);
    assert!(
        (l[15_000] - 0.5).abs() < 1e-3,
        "B still sounds: {}",
        l[15_000]
    );
    assert!(
        l[29_999].abs() < 1e-3,
        "B released at its end: {}",
        l[29_999]
    );
}

#[test]
fn a_note_off_owed_across_an_idle_block_end_does_not_cut_the_next_hit() {
    // Gate mode, one key, a sample of 6 390 frames of 0.5 at 120 BPM (25 frames per tick).
    // Note A (tick 0, two steps) ends by itself after frame 6 389, so the block ending at
    // 6 400 finishes with nothing sounding; note B (tick 257, frame 6 425) starts in the next
    // block. A's note-off arrives at frame 12 000, while B sounds: it belongs to A, so B
    // plays on to its own sample end (frame 12 815).
    let len = 6_390;
    let short = Arc::new(SampleBuf {
        rate: SR,
        l: vec![0.5; len],
        r: vec![0.5; len],
    });
    let mut gate = exact();
    gate[ONE_SHOT] = 0.0;
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &short, gate)],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(0, 1, 2 * STEP_TICKS), note(257, 1, 4 * STEP_TICKS)],
        },
    };
    let (a_end, b_on, a_off) = (len - 1, 257 * 25, 2 * STEP_TICKS as usize * 25);
    assert!(
        block_start(a_end as u64) < block_start(b_on as u64),
        "a block ends between"
    );
    let l = left(&play(state, SR, &[], 13_000));
    assert_eq!(l[a_end], 0.5, "A sounds to its end");
    assert!(
        l[a_end + 1..b_on].iter().all(|&x| x == 0.0),
        "the gap is silent"
    );
    assert_eq!(l[b_on], 0.5, "B starts");
    assert!(a_off < 12_500 && 12_500 < b_on + len);
    assert_eq!(l[12_500], 0.5, "A's note-off did not release B");
}

#[test]
fn play_loops_stops_emitting_at_the_end() {
    let pat = pattern(16, &steps(&[0, 4, 8, 12]));
    let loop_frames = 96_000; // 16 steps at 120 BPM
    let ev = [(0, Command::PlayLoops { loops: 2 })];
    let out = render_offline_with(cfg(SR), clicks(120.0, pat), &ev, 3 * loop_frames, 64);
    let got = onsets(&out);
    let one: Vec<u64> = [0, 24_000, 48_000, 72_000].into();
    let two: Vec<u64> = one
        .iter()
        .copied()
        .chain(one.iter().map(|f| f + loop_frames as u64))
        .collect();
    assert_eq!(got, two);
    assert!(!got.contains(&(2 * loop_frames as u64)));
}

#[test]
fn stop_releases_held_notes_and_rewinds() {
    let mut gate = exact();
    gate[ONE_SHOT] = 0.0;
    let release_ms = gate[RELEASE];
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &dc(), gate)],
        // One note held for the whole pattern.
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(0, 1, 16 * STEP_TICKS)],
        },
    };
    let (stop, again) = (24_000u64, 48_000u64);
    assert_eq!(block_start(stop), stop);
    assert_eq!(block_start(again), again);
    let ev = [(stop, Command::Stop), (again, Command::Play)];
    let l = left(&play(state, SR, &ev, 60_000));
    assert_eq!(l[stop as usize - 1], 0.5, "held until the stop");
    // The release falls by e every quarter of its time: -60 dB takes ln(1000) / 4 of it.
    let release = (f64::from(release_ms) * 0.001 * f64::from(SR)) as usize;
    let to_60db = (release as f64 * 1_000f64.ln() / 4.0).ceil() as usize;
    let quiet = stop as usize + to_60db + BLOCK;
    assert!(
        l[quiet..again as usize].iter().all(|x| x.abs() < 0.5e-3),
        "below -60 dB by frame {quiet}"
    );
    assert!(
        l[stop as usize] < 0.5,
        "the release started at the stop's block"
    );
    // Play again: the first step is at the play block's start.
    let first = l[again as usize - 100..]
        .iter()
        .position(|x| x.abs() > THRESHOLD)
        .unwrap();
    assert_eq!(first, 100);
}

#[test]
fn playback_is_identical_for_any_callback_size() {
    let state = || EngineState {
        bpm: 137.0,
        channels: vec![
            channel(1, &click(), exact()),
            channel(2, &dc(), {
                let mut p = exact();
                p[ONE_SHOT] = 0.0;
                p
            }),
        ],
        pattern: PatternData {
            length: 12 * STEP_TICKS,
            notes: vec![
                note(0, 1, 120),
                note(0, 2, 300),
                note(360, 1, 60),
                note(700, 2, 1_000),
                note(1_440, 1, 240),
                note(2_000, 2, 10),
            ],
        },
    };
    let swapped = PatternData {
        length: 7 * STEP_TICKS,
        notes: vec![note(30, 1, 100), note(500, 2, 400), note(1_000, 1, 1)],
    };
    let script = vec![
        (0, Action::Send(Command::Play)),
        (20_011, Action::Send(Command::SetTempo { bpm: 91.5 })),
        (50_003, Action::Pattern(swapped)),
        (70_000, Action::Send(Command::Stop)),
        (75_555, Action::Send(Command::PlayLoops { loops: 3 })),
        (90_001, Action::Send(Command::SetTempo { bpm: 333.0 })),
    ];
    let frames = 160_000;
    let reference = drive(state(), &script, frames, 64);
    assert!(onsets(&reference).len() > 20, "it played");
    for callback in [1, 37, 441, 4_096] {
        let out = drive(state(), &script, frames, callback);
        assert!(out == reference, "callback {callback}");
    }
    // The offline renderer agrees on the commands alone.
    let commands: Vec<(u64, Command)> = script
        .iter()
        .filter_map(|(f, a)| match a {
            Action::Send(c) => Some((*f, *c)),
            Action::Pattern(_) => None,
        })
        .collect();
    let base = render_offline_with(cfg(SR), state(), &commands, frames, 64);
    for callback in [1, 37, 441, 4_096] {
        let out = render_offline_with(cfg(SR), state(), &commands, frames, callback);
        assert!(out == base, "offline, callback {callback}");
    }
}

#[test]
fn live_position_counts_ticks_while_playing() {
    let (mut c, mut p) = engine_with(cfg(SR), clicks(120.0, pattern(16, &[0])));
    assert_eq!(c.live().position_ticks, 0);
    c.send(Command::Play).unwrap();
    let mut buf = vec![0f32; 48_000 * 2];
    p.process(&mut buf, 2);
    // 25 frames a tick: the last block started 64 frames (2.56 ticks) before frame 48 000.
    // Published for the block start: frame 47 936 is tick 1 917.44.
    assert_eq!(c.live().position_ticks, 1_917);
    c.send(Command::Stop).unwrap();
    p.process(&mut buf[..BLOCK * 2], 2);
    assert_eq!(c.live().position_ticks, 0);
    assert!(!c.live().playing);
}

#[test]
fn a_key_retriggered_inside_one_block_keeps_sounding() {
    // Gate mode: tick 0 for one tick, then the same key from tick 1 (frame 25, same block).
    // The first note-off must reach the channel before the second note-on.
    let mut gate = exact();
    gate[ONE_SHOT] = 0.0;
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &dc(), gate)],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(0, 1, 1), note(1, 1, STEP_TICKS)],
        },
    };
    let l = left(&play(state, SR, &[], 12_000));
    assert_eq!(l[5_000], 0.5, "the second note is held");
    assert!(l[6_025 + 3_000].abs() < 0.05, "and released at its end");
}

#[test]
fn load_state_sets_the_tempo() {
    let pat = pattern(16, &steps(&[1]));
    let (mut c, mut p) = engine_with(cfg(SR), clicks(140.0, pat.clone()));
    c.load_state(clicks(120.0, pat)).unwrap();
    c.send(Command::Play).unwrap();
    let mut buf = vec![0f32; 10_000 * 2];
    p.process(&mut buf, 2);
    // Step 1 at 120 BPM is frame 6 000, counted from the play block's start (frame 0).
    assert_eq!(onsets(&buf), [6_000]);
}

#[test]
fn play_while_playing_changes_nothing() {
    let pat = || pattern(16, &steps(&[0, 3, 5, 9]));
    let once = play(clicks(120.0, pat()), SR, &[], 120_000);
    let twice = play(
        clicks(120.0, pat()),
        SR,
        &[(30_000, Command::Play)],
        120_000,
    );
    assert!(once == twice);
}

#[test]
fn play_loops_while_playing_releases_and_restarts_from_tick_0() {
    let mut gate = exact();
    gate[ONE_SHOT] = 0.0;
    let release_ms = gate[RELEASE];
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &dc(), gate)],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(0, 1, 16 * STEP_TICKS)],
        },
    };
    let restart = 24_000u64;
    let ev = [(restart, Command::PlayLoops { loops: 1 })];
    let l = left(&play(state, SR, &ev, 60_000));
    // The held note is released and the pattern's first note starts again at once: one voice
    // once the old one has faded, not two.
    let release = (f64::from(release_ms) * 0.001 * f64::from(SR)) as usize;
    let settled = restart as usize + (release as f64 * 1_000f64.ln() / 4.0).ceil() as usize;
    assert!(l[restart as usize - 1] == 0.5);
    assert!(
        l[settled + BLOCK..].iter().all(|x| (x - 0.5).abs() < 1e-3),
        "{}",
        l[settled + BLOCK]
    );
}

#[test]
fn a_new_sample_rate_mid_play_keeps_the_position() {
    // A click recorded at 96 kHz reads as one frame at both rates.
    let mut l = vec![0.0; 16];
    l[0] = 1.0;
    let click96 = Arc::new(SampleBuf {
        rate: 96_000.0,
        r: l.clone(),
        l,
    });
    let every: Vec<u32> = (0..16).collect();
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &click96, exact())],
        pattern: pattern(16, &steps(&every)),
    };
    let (mut c, mut p) = engine_with(cfg(48_000.0), state);
    c.send(Command::Play).unwrap();
    let switch = 30_016usize;
    assert_eq!(block_start(switch as u64), switch as u64);
    let mut out = vec![0f32; 80_000 * 2];
    p.process(&mut out[..switch * 2], 2);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96_000.0,
        },
    );
    p.process(&mut out[switch * 2..], 2);
    // 25 frames a tick before the switch, 50 after, from the tick reached at it.
    let reached = switch as f64 / (48_000.0 * 60.0 / (120.0 * f64::from(PPQ)));
    let expected: Vec<u64> = (0..=5u64)
        .map(|s| s * 6_000)
        .chain(
            (6u32..)
                .map(|s| {
                    switch as u64
                        + ticks_to_frames(f64::from(s * STEP_TICKS) - reached, 120.0, 96_000.0)
                })
                .take_while(|&f| f < 80_000),
        )
        .collect();
    assert_eq!(expected[6], 41_984);
    assert_eq!(onsets(&out), expected);
}

#[test]
fn a_tempo_change_and_a_pattern_swap_in_one_block_neither_replay_nor_skip() {
    // A click every 5 ticks, including tick 23 (frame 575 at 120 BPM). Tempo changes both
    // ways; with the same pattern set again in each change's block, the output is the same.
    let ticks: Vec<u32> = (0..768).map(|i| i * 5).chain([23]).collect();
    let mut pat = pattern(16, &ticks);
    pat.notes.sort_by_key(|n| n.tick);
    let changes = [
        (576, 140.0),
        (20_000, 60.0),
        (40_000, 333.0),
        (60_011, 20.0),
        (61_000, 400.0),
    ];
    let script = |swap: bool| {
        let mut s = vec![(0, Action::Send(Command::Play))];
        for (frame, bpm) in changes {
            s.push((frame, Action::Send(Command::SetTempo { bpm })));
            if swap {
                s.push((frame, Action::Pattern(pat.clone())));
            }
        }
        s
    };
    let frames = 120_000;
    let plain = drive(clicks(120.0, pat.clone()), &script(false), frames, 64);
    let swapped = drive(clicks(120.0, pat.clone()), &script(true), frames, 64);
    assert!(onsets(&plain).len() > 500);
    assert!(onsets(&plain).contains(&575));
    assert!(swapped == plain);
}

#[test]
fn a_new_rate_mid_play_neither_replays_nor_skips_a_note() {
    // A click on every tick at 44.1 kHz (22.97 frames a tick), then 22.05 kHz from a block
    // that starts one frame after a tick's frame: that tick has played and must not again.
    let mut l = vec![0.0; 16];
    l[0] = 1.0;
    let click44 = Arc::new(SampleBuf {
        rate: 44_100.0,
        r: l.clone(),
        l,
    });
    let every: Vec<u32> = (0..3_840).collect();
    let state = EngineState {
        bpm: 120.0,
        channels: vec![channel(1, &click44, exact())],
        pattern: PatternData {
            length: 3_840,
            notes: every.iter().map(|&t| note(t, 1, 1)).collect(),
        },
    };
    let at44 = |t: u32| ticks_to_frames(f64::from(t), 120.0, 44_100.0);
    let switch = (1..3_000)
        .map(at44)
        .find(|f| f % BLOCK as u64 == BLOCK as u64 - 1)
        .expect("a tick one frame before a block start")
        + 1;
    let (mut c, mut p) = engine_with(cfg(44_100.0), state);
    c.send(Command::Play).unwrap();
    let total = switch as usize + 20_000;
    let mut out = vec![0f32; total * 2];
    p.process(&mut out[..switch as usize * 2], 2);
    c.reconfigure(&mut p, cfg(22_050.0));
    p.process(&mut out[switch as usize * 2..], 2);
    let reached = switch as f64 / (44_100.0 * 60.0 / (120.0 * f64::from(PPQ)));
    let expected: Vec<u64> = every
        .iter()
        .map(|&t| {
            let old = at44(t);
            if old < switch {
                old
            } else {
                switch + ticks_to_frames(f64::from(t) - reached, 120.0, 22_050.0)
            }
        })
        .take_while(|&f| f < total as u64)
        .collect();
    assert!(expected.contains(&(switch - 1)));
    let got = onsets(&out);
    assert_eq!(got, expected);
    let l = left(&out);
    assert!(
        got.iter().all(|&f| l[f as usize] == 1.0),
        "one voice per onset"
    );
}
