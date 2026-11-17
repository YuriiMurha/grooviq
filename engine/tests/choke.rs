//! Choke groups: a note-on in a channel of group g cuts every other channel of g at its exact
//! frame (a 3 ms fade), and nothing else changes.
//!
//! The channel under test is panned hard left and the chokers hard right, so the left output is
//! the choked channel alone.

use grooviq_dsp::sampler;
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig, EngineState, MAX_CHOKE_GROUP,
    PatternData, SampleBuf, SeqNote, engine_with, render_offline_with,
};
use std::f32::consts::SQRT_2;
use std::sync::Arc;

const SR: f32 = 48_000.0;
/// The 3 ms choke fade at 48 kHz.
const FADE: usize = 144;
const DECLICK: usize = 14;
/// A constant 0.5 through a hard-left pan.
const OPEN: f32 = 0.5 * SQRT_2;

fn cfg() -> EngineConfig {
    EngineConfig { sample_rate: SR }
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

/// Sampler parameters at their defaults with declick off, so the output is the sample itself.
fn exact() -> Vec<f32> {
    let mut p: Vec<f32> = sampler::SPEC.iter().map(|&(_, _, d)| d).collect();
    p[DECLICK] = 0.0;
    p
}

/// A DC channel in `group`, panned hard left (`pan` -1) or hard right (1).
fn hat(id: u32, group: u8, pan: f32) -> ChannelSetup {
    ChannelSetup {
        choke: group,
        ..ChannelSetup::sampler(
            ChannelId(id),
            Some(dc()),
            exact(),
            ChannelMix {
                pan,
                ..ChannelMix::default()
            },
        )
    }
}

fn state(channels: Vec<ChannelSetup>) -> EngineState {
    EngineState {
        channels,
        ..EngineState::default()
    }
}

fn hit(frame: u64, channel: u32) -> (u64, Command) {
    (
        frame,
        Command::ChannelNote {
            channel: ChannelId(channel),
            key: 60,
            velocity: 1.0,
            on: true,
        },
    )
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

fn right(inter: &[f32]) -> Vec<f32> {
    inter.iter().skip(1).step_by(2).copied().collect()
}

fn render(channels: Vec<ChannelSetup>, events: &[(u64, Command)], frames: usize) -> Vec<f32> {
    render_offline_with(cfg(), state(channels), events, frames, 64)
}

#[test]
fn a_closed_hat_chokes_the_open_hat_at_its_frame() {
    // Frame 1 000 is 40 frames into a block.
    let out = left(&render(
        vec![hat(1, 1, -1.0), hat(2, 1, 1.0)],
        &[hit(0, 1), hit(1_000, 2)],
        4_000,
    ));
    assert!(out[..1_000].iter().all(|&x| x == OPEN), "unchanged before");
    // A 3 ms linear fade from the frame of the note-on.
    assert!(
        out[1_000] < OPEN && out[1_000] > 0.9 * OPEN,
        "{}",
        out[1_000]
    );
    assert!(out[1_000..1_000 + FADE].windows(2).all(|w| w[1] <= w[0]));
    assert!(out[1_000 + FADE / 2] > 0.3 * OPEN && out[1_000 + FADE / 2] < 0.7 * OPEN);
    assert!(out[1_000 + FADE..].iter().all(|x| x.abs() < 1e-6), "cut");
}

#[test]
fn the_choke_is_sequenced_on_the_closed_hat_s_frame() {
    // At 120 BPM a tick is 25 frames: the closed hat at tick 41 lands on frame 1 025.
    let note = |tick, channel| SeqNote {
        tick,
        channel: ChannelId(channel),
        key: 60,
        velocity: 1.0,
        length: 240,
        slide: false,
    };
    let st = EngineState {
        bpm: 120.0,
        channels: vec![hat(1, 4, -1.0), hat(2, 4, 1.0)],
        pattern: PatternData {
            length: 3_840,
            notes: vec![note(0, 1), note(41, 2)],
        },
        ..EngineState::default()
    };
    let out = left(&render_offline_with(
        cfg(),
        st,
        &[(0, Command::Play)],
        4_000,
        64,
    ));
    assert!(out[..1_025].iter().all(|&x| x == OPEN));
    assert!(out[1_025] < OPEN);
    assert!(out[1_025 + FADE..].iter().all(|x| x.abs() < 1e-6));
}

#[test]
fn a_channel_never_chokes_itself() {
    // Channel 1 retriggers itself (poly: two voices) while channel 2 of its group is silent.
    let events = [hit(0, 1), hit(1_000, 1)];
    let grouped = render(vec![hat(1, 1, -1.0), hat(2, 1, 1.0)], &events, 3_000);
    let alone = render(vec![hat(1, 0, -1.0)], &events, 3_000);
    assert!(grouped == alone);
    assert_eq!(left(&grouped)[2_000], 2.0 * OPEN, "both voices sound");
}

#[test]
fn group_zero_never_chokes() {
    let events = [hit(0, 1), hit(1_000, 2), hit(1_500, 3)];
    let alone = left(&render(vec![hat(1, 0, -1.0)], &events[..1], 3_000));
    assert!(alone.iter().all(|&x| x == OPEN));
    // Group 0 everywhere, and the choker in another group.
    for (a, b) in [(0, 0), (1, 2), (0, 1)] {
        let out = left(&render(
            vec![hat(1, a, -1.0), hat(2, b, 1.0), hat(3, b, 1.0)],
            &events,
            3_000,
        ));
        assert!(out == alone, "groups {a} and {b}");
    }
}

#[test]
fn two_note_ons_in_one_block_split_twice() {
    // In the block from frame 960: channel 2 chokes channel 1 at 970, channel 1 plays again at
    // 990, and channel 3 chokes it again at 1 010.
    let out = left(&render(
        vec![hat(1, 1, -1.0), hat(2, 1, 1.0), hat(3, 1, 1.0)],
        &[hit(0, 1), hit(970, 2), hit(990, 1), hit(1_010, 3)],
        3_000,
    ));
    assert!(out[..970].iter().all(|&x| x == OPEN));
    assert!(out[970] < OPEN, "first cut at 970");
    assert!(out[989] < OPEN && out[989] > 0.0, "fading");
    assert!(out[990] > OPEN, "its own note at 990 is not choked");
    assert!(out[1_009] > OPEN);
    assert!(out[1_010] < out[1_009], "second cut at 1 010");
    assert!(out[1_010 + FADE..].iter().all(|x| x.abs() < 1e-6));
}

#[test]
fn no_choke_leaves_the_output_unchanged() {
    // A group with one channel, and a group whose other members never play in the same block:
    // both render exactly as group 0.
    let events = [hit(0, 1), hit(500, 1), hit(5_000, 1)];
    let reference = render(vec![hat(1, 0, -0.3)], &events, 8_000);
    for group in [1, MAX_CHOKE_GROUP] {
        let alone = render(vec![hat(1, group, -0.3)], &events, 8_000);
        assert!(alone == reference, "group {group} alone");
    }
    let quiet = render(vec![hat(1, 5, -0.3), hat(2, 5, 1.0)], &events, 8_000);
    assert!(left(&quiet) == left(&reference));
}

#[test]
fn set_choke_moves_a_channel_between_groups() {
    let (mut c, mut p) = engine_with(cfg(), state(vec![hat(1, 0, -1.0), hat(2, 2, 1.0)]));
    let set = |group| Command::SetChoke {
        channel: ChannelId(1),
        group,
    };
    assert!(c.send(set(MAX_CHOKE_GROUP + 1)).is_err(), "17 is refused");
    assert_eq!(c.state().channels[0].choke, 0);
    c.send(set(2)).unwrap();
    assert_eq!(c.state().channels[0].choke, 2);
    c.send(hit(0, 1).1).unwrap();
    let mut out = vec![0f32; 640 * 2];
    p.process(&mut out, 2);
    c.send(hit(0, 2).1).unwrap();
    p.process(&mut out, 2);
    assert!(left(&out)[FADE..].iter().all(|x| x.abs() < 1e-6), "choked");
}

#[test]
fn a_choke_group_out_of_range_is_refused() {
    let st = state(vec![hat(1, MAX_CHOKE_GROUP + 1, 0.0)]);
    let (mut c, _p) = engine_with(cfg(), EngineState::default());
    assert!(c.load_state(st.clone()).is_err());
    assert!(c.add_channel(st.channels[0].clone()).is_err());
}

#[test]
fn more_than_256_grouped_note_ons_in_a_block_never_panic() {
    // In the block from frame 64, grouped note-ons count in channel order, then offset:
    // channel 1 gives entries 1-128 (offset 0), channel 2 entries 129-256 (offset 0, the last
    // at 20) and channel 3 entry 257 (offset 40). Channel 4, heard alone on the left, is held
    // from frame 0 and struck again at offsets 10 and 30: entry 256 must cut the note at 10,
    // and entry 257 must play without cutting the note at 30.
    let mut events = Vec::new();
    events.push(hit(0, 4));
    events.extend((0..128).map(|_| hit(64, 1)));
    events.extend((0..127).map(|_| hit(64, 2)));
    events.push(hit(64 + 20, 2));
    events.push(hit(64 + 40, 3));
    events.push(hit(64 + 10, 4));
    events.push(hit(64 + 30, 4));
    let channels = || {
        vec![
            hat(1, 1, 1.0),
            hat(2, 1, 1.0),
            hat(3, 1, 1.0),
            hat(4, 1, -1.0),
        ]
    };
    let out = render(channels(), &events, 1_024);
    assert!(out.iter().all(|x| x.is_finite()));
    let l = left(&out);
    assert!(l[..64].iter().all(|&x| x == OPEN));
    // Every cut note has faded by 64 + 20 + 144; only the note struck at offset 30 is left.
    assert!(l[64 + 20 + FADE..].iter().all(|&x| x == OPEN), "{}", l[300]);

    // Entry 257 plays: on the right, the render without it differs by exactly its note (0.5
    // through a hard-right pan) from its frame on, and by nothing before.
    let without: Vec<(u64, Command)> = events
        .iter()
        .copied()
        .filter(|e| *e != hit(104, 3))
        .collect();
    assert_eq!(without.len(), events.len() - 1);
    let reference = render(channels(), &without, 1_024);
    let (r, r_ref) = (right(&out), right(&reference));
    assert!(r[..104] == r_ref[..104]);
    assert!(
        (104..1_024).all(|i| r[i] == r_ref[i] + OPEN),
        "entry 257 sounds"
    );
    assert!(left(&reference) == l, "and cuts nothing");
}

#[test]
fn notes_starting_together_in_a_group_both_survive() {
    // Channels 1 and 2 of one group, each holding a note, strike again on the same frame: each
    // cuts the other's old note, and both new notes play on. At an offset inside a block, and
    // at a block start.
    for frame in [1_000, 1_024] {
        let out = render(
            vec![hat(1, 1, -1.0), hat(2, 1, 1.0)],
            &[hit(0, 1), hit(0, 2), hit(frame, 1), hit(frame, 2)],
            3_000,
        );
        let f = frame as usize;
        let (l, r) = (left(&out), right(&out));
        assert!(
            l[f] > OPEN,
            "frame {frame}: the new note sounds from its frame"
        );
        assert!(
            l[f + FADE..].iter().all(|&x| x == OPEN),
            "frame {frame}: left"
        );
        assert!(
            r[f + FADE..].iter().all(|&x| x == OPEN),
            "frame {frame}: right"
        );
    }
}

#[test]
fn only_a_sounding_note_on_chokes() {
    // A held note on channel 1, then an event on channel 2 of its group at frame 1 000.
    let at_1000 = |velocity, on| {
        let event = (
            1_000,
            Command::ChannelNote {
                channel: ChannelId(2),
                key: 60,
                velocity,
                on,
            },
        );
        left(&render(
            vec![hat(1, 1, -1.0), hat(2, 1, 1.0)],
            &[hit(0, 1), event],
            2_000,
        ))
    };
    // A note-on with velocity 0 and a note-off (with a velocity) leave it untouched.
    assert!(at_1000(0.0, true).iter().all(|&x| x == OPEN), "velocity 0");
    assert!(at_1000(1.0, false).iter().all(|&x| x == OPEN), "note-off");
    // A note-on that sounds cuts it.
    let cut = at_1000(0.5, true);
    assert!(
        cut[1_000 + FADE..].iter().all(|x| x.abs() < 1e-6),
        "note-on"
    );
}
