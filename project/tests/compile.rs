//! The compile step: project -> the engine's state.

use grooviq_dsp::sampler::{PARAM_IDS, SPEC, SampleBuf};
use grooviq_dsp::synth;
use grooviq_engine::{ChannelId, ChannelSetup, Instrument, InstrumentKind, STEP_TICKS};
use grooviq_project::{
    Channel, Note, Pattern, Project, compile_pattern, engine_state, is_plain_step, roll_channels,
    sampler_values, synth_values,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

fn channel(id: u32) -> Channel {
    Channel {
        id,
        name: format!("Channel {id}"),
        color: 0,
        sample: None,
        instrument: InstrumentKind::Sampler,
        sampler: Default::default(),
        synth: Default::default(),
        preset: None,
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
    }
}

fn note(channel: u32, tick: u32) -> Note {
    Note {
        id: 0,
        channel,
        tick,
        length: STEP_TICKS,
        key: 60,
        velocity: 0.8,
        slide: false,
    }
}

fn project_with(channels: &[u32], pattern: Pattern) -> Project {
    let mut p = Project::new();
    p.channels = channels.iter().map(|&id| channel(id)).collect();
    p.current_pattern = pattern.id;
    p.patterns = vec![pattern];
    p
}

fn pattern(steps: u32, swing: f32, notes: Vec<Note>) -> Pattern {
    Pattern {
        id: 0,
        name: "P".into(),
        steps,
        swing,
        notes,
        next_note_id: 1,
        revision: 1,
    }
}

#[test]
fn swing_delays_odd_steps_only() {
    let p = project_with(
        &[1],
        pattern(
            16,
            0.5,
            vec![note(1, 240), note(1, 480), note(1, 250), note(1, 720)],
        ),
    );
    let ticks: Vec<u32> = compile_pattern(&p, &p.patterns[0])
        .notes
        .iter()
        .map(|n| n.tick)
        .collect();
    // 240 -> 300 and 720 -> 780 (odd steps), 480 and 250 stay.
    assert_eq!(ticks, vec![250, 300, 480, 780]);

    // Full swing moves 120 ticks, none leaves the notes alone.
    let full = project_with(&[1], pattern(16, 1.0, vec![note(1, 240)]));
    assert_eq!(compile_pattern(&full, &full.patterns[0]).notes[0].tick, 360);
    let none = project_with(&[1], pattern(16, 0.0, vec![note(1, 240)]));
    assert_eq!(compile_pattern(&none, &none.patterns[0]).notes[0].tick, 240);
}

#[test]
fn swing_rounds_to_the_nearest_tick() {
    // 0.105 * 120 = 12.6 ticks: rounded to 13, not cut to 12.
    let p = project_with(&[1], pattern(16, 0.105, vec![note(1, 240)]));
    assert_eq!(compile_pattern(&p, &p.patterns[0]).notes[0].tick, 253);
    // 0.104 * 120 = 12.48 rounds down.
    let p = project_with(&[1], pattern(16, 0.104, vec![note(1, 240)]));
    assert_eq!(compile_pattern(&p, &p.patterns[0]).notes[0].tick, 252);
}

#[test]
fn a_non_finite_velocity_compiles_to_the_default() {
    let mut n = note(1, 0);
    n.velocity = f32::NAN;
    let mut m = note(1, 240);
    m.velocity = f32::INFINITY;
    let p = project_with(&[1], pattern(16, 0.0, vec![n, m]));
    let got: Vec<f32> = compile_pattern(&p, &p.patterns[0])
        .notes
        .iter()
        .map(|n| n.velocity)
        .collect();
    assert_eq!(got, vec![0.8, 0.8]);
}

#[test]
fn compile_drops_notes_of_missing_channels_and_past_the_end_and_sorts() {
    let p = project_with(
        &[1, 2],
        pattern(
            4,
            0.0,
            vec![
                note(2, 480),
                note(9, 0),   // channel 9 does not exist
                note(1, 960), // exactly past the end (4 steps = 960 ticks)
                note(1, 959), // last tick: stays
                note(2, 0),
                note(1, 480),
                note(1, 0),
            ],
        ),
    );
    let compiled = compile_pattern(&p, &p.patterns[0]);
    assert_eq!(compiled.length, 4 * STEP_TICKS);
    let got: Vec<(u32, u32)> = compiled
        .notes
        .iter()
        .map(|n| (n.tick, n.channel.0))
        .collect();
    assert_eq!(got, vec![(0, 1), (0, 2), (480, 1), (480, 2), (959, 1)]);
    let first = compiled.notes[0];
    assert_eq!(first.key, 60);
    assert_eq!(first.velocity, 0.8);
    assert_eq!(first.length, STEP_TICKS);
}

#[test]
fn sampler_values_use_defaults_and_ignore_unknown_names() {
    let mut c = channel(1);
    assert_eq!(
        sampler_values(&c),
        SPEC.iter().map(|s| s.2).collect::<Vec<_>>()
    );

    c.sampler.insert("tune".into(), 7.0);
    c.sampler.insert("gain".into(), 99.0); // clamped to the table's max
    c.sampler.insert("nonsense".into(), 3.0); // ignored
    let v = sampler_values(&c);
    assert_eq!(v.len(), SPEC.len());
    let at = |name: &str| PARAM_IDS.iter().position(|&id| id == name).unwrap();
    assert_eq!(v[at("tune")], 7.0);
    assert_eq!(v[at("gain")], SPEC[at("gain")].1);
    assert_eq!(v[at("root")], SPEC[at("root")].2);
}

#[test]
fn engine_state_carries_bpm_mix_samples_and_the_current_pattern() {
    let mut p = Project::new();
    p.bpm = 128.0;
    let mut a = channel(5);
    a.volume = 0.5;
    a.pan = -0.25;
    a.mute = true;
    let mut b = channel(7);
    b.solo = true;
    p.channels = vec![a, b];
    p.patterns = vec![
        Pattern {
            steps: 8,
            notes: vec![note(5, 0)],
            ..Pattern::new(3, "first")
        },
        Pattern {
            steps: 32,
            notes: vec![note(7, 240), note(5, 0)],
            ..Pattern::new(4, "second")
        },
    ];
    p.current_pattern = 4;

    let buf = Arc::new(SampleBuf {
        rate: 44_100.0,
        l: vec![0.1; 10],
        r: vec![0.1; 10],
    });
    let mut samples = HashMap::new();
    samples.insert(5, buf.clone());

    let state = engine_state(&p, &samples);
    assert_eq!(state.bpm, 128.0);
    assert_eq!(state.pattern.length, 32 * STEP_TICKS);
    assert_eq!(state.pattern.notes.len(), 2);
    assert_eq!(state.channels.len(), 2);

    let (c0, c1) = (&state.channels[0], &state.channels[1]);
    let sample = |c: &ChannelSetup| {
        assert_eq!(c.instrument.kind(), InstrumentKind::Sampler);
        c.instrument.sample().cloned()
    };
    assert_eq!(c0.id, ChannelId(5));
    assert_eq!(c0.choke, 0);
    assert!(Arc::ptr_eq(sample(c0).as_ref().unwrap(), &buf));
    assert_eq!(c0.mix.volume, 0.5);
    assert_eq!(c0.mix.pan, -0.25);
    assert!(c0.mix.mute && !c0.mix.solo);
    assert_eq!(c0.instrument.params(), sampler_values(&p.channels[0]));
    assert_eq!(c1.id, ChannelId(7));
    assert!(sample(c1).is_none(), "no loaded sample, no buffer");
    assert!(c1.mix.solo && !c1.mix.mute);
}

#[test]
fn engine_state_survives_a_project_that_was_never_validated() {
    // Out-of-range values are clamped on the way, so the engine never refuses the state.
    let mut p = Project::new();
    p.bpm = f32::NAN;
    let mut c = channel(1);
    c.volume = 9.0;
    c.pan = f32::NAN;
    p.channels = vec![c.clone(), c]; // a duplicate id
    p.patterns.clear();
    let state = engine_state(&p, &HashMap::new());
    assert_eq!(state.bpm, 140.0);
    assert_eq!(state.channels.len(), 1);
    assert_eq!(state.channels[0].mix.volume, 2.0);
    assert_eq!(state.channels[0].mix.pan, 0.0);
    assert_eq!(state.pattern.length, 16 * STEP_TICKS);
}

#[test]
fn synth_channels_compile_to_synth_instruments() {
    let mut bass = channel(2);
    bass.instrument = InstrumentKind::Synth;
    bass.preset = Some("808 Glide".into());
    bass.synth.insert("filter_cutoff".into(), 650.0);
    bass.synth.insert("glide".into(), 5000.0); // clamped to the table's max
    bass.synth.insert("master".into(), f32::NAN); // the default
    bass.synth.insert("nonsense".into(), 1.0); // ignored
    // A sampler value on a synth channel is kept in the file but never reaches the synth.
    bass.sampler.insert("tune".into(), 5.0);
    let mut p = project_with(&[1], pattern(16, 0.0, vec![note(1, 0), note(2, 240)]));
    p.channels.push(bass);

    let at = |name: &str| synth::PARAM_IDS.iter().position(|&id| id == name).unwrap();
    let mut expected: Vec<f32> = synth::SPEC.iter().map(|s| s.2).collect();
    expected[at("filter_cutoff")] = 650.0;
    expected[at("glide")] = synth::SPEC[at("glide")].1;
    assert_eq!(synth_values(&p.channels[1]), expected);
    assert_eq!(
        synth_values(&channel(9)),
        synth::SPEC.iter().map(|s| s.2).collect::<Vec<_>>(),
        "a channel with no synth values has the defaults"
    );

    // A sample decoded under the synth channel's id (left over from a sampler) is not used.
    let buf = Arc::new(SampleBuf {
        rate: 44_100.0,
        l: vec![0.1; 10],
        r: vec![0.1; 10],
    });
    let samples = HashMap::from([(1, buf.clone()), (2, buf.clone())]);
    let state = engine_state(&p, &samples);
    assert_eq!(state.channels.len(), 2);
    assert_eq!(state.channels[0].instrument.kind(), InstrumentKind::Sampler);
    assert_eq!(state.channels[1].id, ChannelId(2));
    assert_eq!(
        state.channels[1].instrument,
        Instrument::Synth { params: expected }
    );
    assert_eq!(
        state.pattern.notes.len(),
        2,
        "synth notes play like any other"
    );
}

#[test]
fn slide_and_choke_reach_the_engine_state() {
    let mut open_hat = channel(1);
    open_hat.choke = 3;
    let mut closed_hat = channel(2);
    closed_hat.choke = 99; // never validated: clamped on the way
    let mut slide = note(2, 480);
    slide.slide = true;
    slide.key = 67;
    let mut p = project_with(&[], pattern(16, 0.0, vec![slide, note(1, 0), note(2, 480)]));
    p.channels = vec![open_hat, closed_hat, channel(3)];

    let state = engine_state(&p, &HashMap::new());
    let chokes: Vec<u8> = state.channels.iter().map(|c| c.choke).collect();
    assert_eq!(chokes, vec![3, 16, 0]);
    let notes: Vec<(u32, u32, u8, bool)> = state
        .pattern
        .notes
        .iter()
        .map(|n| (n.tick, n.channel.0, n.key, n.slide))
        .collect();
    // At one tick, the slide follows the plain note it bends.
    assert_eq!(
        notes,
        vec![(0, 1, 60, false), (480, 2, 60, false), (480, 2, 67, true)]
    );
}

#[test]
fn plain_steps_and_roll_channels_follow_the_rule() {
    let plain = note(1, 3 * STEP_TICKS);
    assert!(is_plain_step(&plain));
    let broken = [
        Note {
            tick: 3 * STEP_TICKS + 10,
            ..plain
        },
        Note {
            length: STEP_TICKS / 2,
            ..plain
        },
        Note { key: 61, ..plain },
        Note {
            slide: true,
            ..plain
        },
    ];
    for n in &broken {
        assert!(!is_plain_step(n), "{n:?}");
    }

    // Channel 1 holds only plain steps; channels 2..=5 each hold one note that breaks one rule.
    let mut notes = vec![note(1, 0), note(1, STEP_TICKS), plain];
    for (i, n) in broken.iter().enumerate() {
        let channel = i as u32 + 2;
        notes.push(note(channel, 0));
        notes.push(Note { channel, ..*n });
    }
    let p = pattern(16, 0.0, notes);
    assert_eq!(roll_channels(&p), BTreeSet::from([2, 3, 4, 5]));
    assert!(roll_channels(&pattern(16, 0.0, vec![])).is_empty());
}
