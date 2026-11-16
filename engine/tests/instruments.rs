//! Instrument channels: a channel holds a sampler or the synth, takes its own instrument's
//! parameters, and plays sequenced slide notes as slides.

use grooviq_dsp::{sampler, synth};
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, Command, ControlError, EngineConfig, EngineState,
    Instrument, InstrumentKind, PatternData, SampleBuf, SeqNote, engine_with, render_offline_with,
};
use std::sync::Arc;

const SR: f32 = 48_000.0;
/// Sampler parameter indices: output gain (dB), declick switch, one-shot switch.
const GAIN: u32 = 12;
const DECLICK: usize = 14;
const ONE_SHOT: usize = 7;

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

/// Sampler parameters at their defaults with declick off, so the output is the sample itself.
fn exact() -> Vec<f32> {
    let mut p: Vec<f32> = sampler::SPEC.iter().map(|&(_, _, d)| d).collect();
    p[DECLICK] = 0.0;
    p
}

fn sampler_channel(id: u32, sample: &Arc<SampleBuf>, params: Vec<f32>) -> ChannelSetup {
    ChannelSetup::sampler(
        ChannelId(id),
        Some(Arc::clone(sample)),
        params,
        ChannelMix::default(),
    )
}

/// The synth's parameters for a factory preset, in `SPEC` order.
fn preset(name: &str) -> Vec<f32> {
    let mut p: Vec<f32> = synth::SPEC.iter().map(|&(_, _, d)| d).collect();
    for (index, value) in synth::preset(name).expect("a factory preset") {
        p[index as usize] = value;
    }
    p
}

fn synth_channel(id: u32, params: Vec<f32>) -> ChannelSetup {
    ChannelSetup::synth(ChannelId(id), params, ChannelMix::default())
}

fn state(channels: Vec<ChannelSetup>) -> EngineState {
    EngineState {
        channels,
        ..EngineState::default()
    }
}

fn cnote(channel: u32, key: u8, on: bool) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.8,
        on,
    }
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, x| m.max(x.abs()))
}

/// Notes at (frame, key, on) on the synth: the global one's commands, and a channel's.
const PHRASE: [(u64, u8, bool); 8] = [
    (100, 36, true),
    (2_000, 43, true),
    (7_001, 36, false),
    (9_000, 48, true),
    (9_000, 43, false),
    (15_500, 48, false),
    (16_000, 31, true),
    (30_000, 31, false),
];

#[test]
fn a_synth_channel_plays_notes_like_the_global_synth() {
    for name in ["808 Glide", "Reese", "Pluck"] {
        let params = preset(name);
        let mut global: Vec<(u64, Command)> = (0u32..)
            .zip(&params)
            .map(|(index, &value)| (0, Command::SetSynthParam { index, value }))
            .collect();
        global.extend(PHRASE.iter().map(|&(frame, key, on)| {
            (
                frame,
                Command::Note {
                    key,
                    velocity: 0.8,
                    on,
                },
            )
        }));
        let channel: Vec<(u64, Command)> = PHRASE
            .iter()
            .map(|&(frame, key, on)| (frame, cnote(7, key, on)))
            .collect();
        let frames = 40_000;
        let expected = render_offline_with(cfg(), EngineState::default(), &global, frames, 64);
        assert!(peak(&expected) > 0.05, "{name} is audible");
        let st = state(vec![synth_channel(7, params)]);
        let actual = render_offline_with(cfg(), st, &channel, frames, 64);
        assert!(actual == expected, "{name}: bit for bit");
    }
}

#[test]
fn sampler_and_synth_channels_mix_in_insertion_order() {
    let (a, c) = (dc(0.1), dc(0.3));
    let chans = [
        sampler_channel(1, &a, exact()),
        synth_channel(2, preset("Reese")),
        sampler_channel(3, &c, exact()),
    ];
    let events = |ids: &[u32]| -> Vec<(u64, Command)> {
        ids.iter()
            .flat_map(|&id| [(64 * id as u64, cnote(id, 45, true))])
            .collect()
    };
    let frames = 6_000;
    let alone: Vec<Vec<f32>> = chans
        .iter()
        .map(|ch| {
            render_offline_with(
                cfg(),
                state(vec![ch.clone()]),
                &events(&[ch.id.0]),
                frames,
                64,
            )
        })
        .collect();
    let all = render_offline_with(
        cfg(),
        state(chans.to_vec()),
        &events(&[1, 2, 3]),
        frames,
        64,
    );
    let in_order: Vec<f32> = (0..frames * 2)
        .map(|i| (alone[0][i] + alone[1][i]) + alone[2][i])
        .collect();
    assert!(all == in_order, "mixed as ((1 + 2) + 3)");
    // The order can be told apart: another one rounds differently somewhere.
    let other: Vec<f32> = (0..frames * 2)
        .map(|i| (alone[0][i] + alone[2][i]) + alone[1][i])
        .collect();
    assert!(all != other, "the test sees the order");
}

#[test]
fn set_channel_param_is_clamped_by_the_channel_s_instrument() {
    let s = dc(0.5);
    let st = state(vec![
        sampler_channel(1, &s, exact()),
        synth_channel(2, Vec::new()),
    ]);
    let (mut c, _p) = engine_with(cfg(), st);
    let param = |channel, index, value| Command::SetChannelParam {
        channel: ChannelId(channel),
        index,
        value,
    };
    let master = synth::SPEC.len() as u32 - 1;
    // The synth's last parameter: fine on the synth, past the sampler's table.
    assert!(c.send(param(2, master, 1e9)).is_ok());
    assert!(c.send(param(1, master, 0.5)).is_err());
    assert!(c.send(param(2, master + 1, 0.5)).is_err());
    assert!(c.send(param(2, 0, f32::NAN)).is_err());
    // Clamped by the sampler's table on the sampler.
    assert!(c.send(param(1, GAIN, 99.0)).is_ok());
    let last = sampler::SPEC.len() as u32 - 1;
    assert!(c.send(param(1, last, -5.0)).is_ok());
    assert!(c.send(param(1, last + 1, 0.0)).is_err());
    let st = c.state().clone();
    assert_eq!(st.channels[0].instrument.kind(), InstrumentKind::Sampler);
    assert_eq!(st.channels[1].instrument.kind(), InstrumentKind::Synth);
    let (sp, yp) = (
        st.channels[0].instrument.params(),
        st.channels[1].instrument.params(),
    );
    assert_eq!(sp[GAIN as usize], sampler::SPEC[GAIN as usize].1);
    assert_eq!(sp[last as usize], sampler::SPEC[last as usize].0);
    assert_eq!(yp[master as usize], synth::SPEC[master as usize].1);
    // The sampler's own command is refused on a synth channel.
    let sampler_cmd = Command::SetSamplerParam {
        channel: ChannelId(2),
        index: GAIN,
        value: 0.0,
    };
    assert!(c.send(sampler_cmd).is_err());
    // Missing trailing parameters took their defaults.
    assert_eq!(yp.len(), synth::SPEC.len());
}

#[test]
fn a_channel_param_reaches_the_channel_s_device() {
    let s = dc(0.5);
    let st = state(vec![sampler_channel(1, &s, exact())]);
    let gain = Command::SetChannelParam {
        channel: ChannelId(1),
        index: GAIN,
        value: -6.0,
    };
    let play = [(0, cnote(1, 60, true))];
    let plain = left(&render_offline_with(cfg(), st.clone(), &play, 1_024, 64));
    let quiet = left(&render_offline_with(
        cfg(),
        st,
        &[(0, gain), play[0]],
        1_024,
        64,
    ));
    let ratio = quiet[600] / plain[600];
    assert!(
        (ratio - grooviq_dsp::db_to_gain(-6.0)).abs() < 1e-6,
        "{ratio}"
    );
}

#[test]
fn set_sample_on_a_synth_channel_is_refused() {
    let s = dc(0.5);
    let (mut c, _p) = engine_with(cfg(), state(vec![synth_channel(2, Vec::new())]));
    let before = c.state().clone();
    assert_eq!(
        c.set_channel_sample(ChannelId(2), Some(Arc::clone(&s))),
        Err(ControlError::WrongInstrument)
    );
    assert_eq!(
        c.set_channel_sample(ChannelId(2), None),
        Err(ControlError::WrongInstrument)
    );
    assert_eq!(*c.state(), before);
    assert_eq!(Arc::strong_count(&s), 1, "nothing queued holds it");
    assert!(matches!(
        c.state().channels[0].instrument,
        Instrument::Synth { .. }
    ));
}

fn seq(tick: u32, key: u8, length: u32, slide: bool) -> SeqNote {
    SeqNote {
        tick,
        channel: ChannelId(1),
        key,
        velocity: 1.0,
        length,
        slide,
    }
}

/// Plays `notes` on channel 1 from frame 0 at 120 BPM (a tick is 25 frames).
fn play(channel: ChannelSetup, notes: Vec<SeqNote>, frames: usize) -> Vec<f32> {
    let st = EngineState {
        bpm: 120.0,
        channels: vec![channel],
        pattern: PatternData {
            length: 16 * 240,
            notes,
        },
    };
    left(&render_offline_with(
        cfg(),
        st,
        &[(0, Command::Play)],
        frames,
        64,
    ))
}

#[test]
fn slide_notes_reach_the_device() {
    // A held 808 on a constant sample: one voice plays 0.5 at any pitch, two play 1.0.
    let s = dc(0.5);
    let mut gate = exact();
    gate[ONE_SHOT] = 0.0;
    let held = || sampler_channel(1, &s, gate.clone());
    // Tick 480 is frame 12 000.
    let slid = play(
        held(),
        vec![seq(0, 36, 960, false), seq(480, 43, 480, true)],
        20_000,
    );
    // (Below its root, a note's first frames interpolate across the sample's start.)
    assert!(
        slid[16..20_000].iter().all(|&x| x == 0.5),
        "one voice bends"
    );
    let struck = play(
        held(),
        vec![seq(0, 36, 960, false), seq(480, 43, 480, false)],
        20_000,
    );
    assert_eq!(struck[11_999], 0.5);
    assert_eq!(
        struck[12_000], 1.0,
        "without the slide flag: a second voice"
    );

    // At one tick, the slide follows the note it bends, whatever order the pattern lists.
    let same_tick = play(
        held(),
        vec![seq(0, 43, 480, true), seq(0, 36, 480, false)],
        8_000,
    );
    assert!(same_tick[..8_000].iter().all(|&x| x < 0.6), "one voice");
    assert!(same_tick[16..8_000].iter().all(|&x| x == 0.5));

    // The synth: the slide bends the sounding note instead of striking a new one.
    let synth = |slide| {
        play(
            synth_channel(1, Vec::new()),
            vec![seq(0, 36, 960, false), seq(480, 43, 480, slide)],
            20_000,
        )
    };
    let (bent, struck) = (synth(true), synth(false));
    assert!(peak(&bent) > 0.05);
    assert!(bent[..12_000] == struck[..12_000]);
    assert!(bent[12_000..] != struck[12_000..]);
}

#[test]
fn synth_channels_are_identical_for_any_callback_size() {
    let s = dc(0.2);
    let st = EngineState {
        bpm: 133.0,
        channels: vec![
            synth_channel(1, preset("808 Glide")),
            sampler_channel(2, &s, exact()),
            synth_channel(3, preset("Bell Keys")),
        ],
        pattern: PatternData {
            length: 8 * 240,
            notes: vec![
                SeqNote {
                    channel: ChannelId(3),
                    ..seq(0, 72, 300, false)
                },
                seq(0, 36, 700, false),
                seq(480, 41, 400, true),
                SeqNote {
                    channel: ChannelId(2),
                    ..seq(960, 60, 100, false)
                },
                SeqNote {
                    channel: ChannelId(3),
                    ..seq(1_000, 76, 500, false)
                },
            ],
        },
    };
    let mut events = vec![(0, Command::Play), (3_001, cnote(3, 64, true))];
    events.push((9_999, cnote(3, 64, false)));
    events.push((
        20_017,
        Command::SetChannelParam {
            channel: ChannelId(1),
            index: 20,
            value: 900.0,
        },
    ));
    let frames = 48_000;
    let reference = render_offline_with(cfg(), st.clone(), &events, frames, 1);
    assert!(peak(&reference) > 0.05);
    for callback in [37, 64, 441, 4_096] {
        let out = render_offline_with(cfg(), st.clone(), &events, frames, callback);
        assert!(out == reference, "callback {callback}");
    }
}

#[test]
fn channel_edits_queued_before_a_new_project_and_a_rate_change_do_not_reach_it() {
    // A gain and a choke group for the old project's channel 1, still queued when a New and a
    // rate change rebuild the graph: the new channel 1 keeps its own (group 0, 0 dB).
    let s = dc(0.5);
    let hat = |id, choke| ChannelSetup {
        choke,
        mix: ChannelMix {
            pan: if id == 1 { -1.0 } else { 1.0 },
            ..ChannelMix::default()
        },
        ..sampler_channel(id, &s, exact())
    };
    let (mut c, mut p) = engine_with(cfg(), state(vec![hat(1, 0)]));
    c.send(Command::SetChannelParam {
        channel: ChannelId(1),
        index: GAIN,
        value: -40.0,
    })
    .unwrap();
    c.send(Command::SetChoke {
        channel: ChannelId(1),
        group: 3,
    })
    .unwrap();
    let fresh = || state(vec![hat(1, 0), hat(2, 3)]);
    c.load_state(fresh()).unwrap();
    c.reconfigure(&mut p, cfg());
    let (mut rc, mut rp) = engine_with(cfg(), fresh());
    let run = |c: &mut grooviq_engine::Controller, p: &mut grooviq_engine::AudioProcessor| {
        c.send(cnote(1, 60, true)).unwrap();
        let mut first = vec![0f32; 128 * 2];
        p.process(&mut first, 2);
        c.send(cnote(2, 60, true)).unwrap();
        let mut out = vec![0f32; 1_024 * 2];
        p.process(&mut out, 2);
        // Hard left: channel 1 alone.
        left(&out)
    };
    let expected = run(&mut rc, &mut rp);
    assert!(
        expected.iter().all(|&x| x == expected[0] && x > 0.5),
        "full level, not choked"
    );
    assert!(run(&mut c, &mut p) == expected);
}
