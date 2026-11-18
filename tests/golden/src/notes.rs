//! The M2 acceptance scene: three synth channels (an 808 with slides, chords, a lead), a sampler
//! using the loop, filter, legato, glide and velocity features, and two hats in a choke group,
//! built as a real [`Project`], saved and loaded through the v2 `.grooviq` file and exported to
//! WAV. The export must be byte-identical to `refs/notes_m2.wav` on every platform.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use grooviq_dsp::synth;
use grooviq_engine::{InstrumentKind, STEP_TICKS};
use grooviq_project::{
    Channel, DEFAULT_KEY, Note, Pattern, Project, ProjectKey, SampleFiles, SampleRef,
    TimeSignature, read_sample_file,
};

/// Loops the export plays.
pub const LOOPS: u32 = 1;
/// Steps in the pattern.
pub const STEPS: u32 = 32;
/// Channel ids, in mixing order.
pub const SYNTH_808: u32 = 1;
pub const KEYS: u32 = 2;
pub const LEAD: u32 = 3;
pub const SUB: u32 = 4;
pub const CLOSED_HAT: u32 = 5;
pub const OPEN_HAT: u32 = 6;

/// A channel with the defaults the golden scenes share.
fn base(id: u32, name: &str) -> Channel {
    Channel {
        id,
        name: name.into(),
        color: id as u8,
        sample: None,
        instrument: InstrumentKind::Sampler,
        sampler: BTreeMap::new(),
        synth: BTreeMap::new(),
        preset: None,
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
    }
}

/// A synth channel started from the factory preset `preset`.
fn synth_channel(id: u32, name: &str, preset: &str) -> Channel {
    let values = synth::preset(preset).unwrap_or_else(|| panic!("no synth preset {preset:?}"));
    Channel {
        instrument: InstrumentKind::Synth,
        synth: values
            .into_iter()
            .map(|(index, value)| (synth::PARAM_IDS[index as usize].to_string(), value))
            .collect(),
        preset: Some(preset.into()),
        ..base(id, name)
    }
}

/// A sampler channel playing `file` with `params` by parameter name.
fn sampler_channel(id: u32, name: &str, file: &str, params: &[(&str, f32)]) -> Channel {
    Channel {
        // Only the file name: the project's JSON is the same on every machine.
        sample: Some(SampleRef {
            path: PathBuf::from(file),
            file: None,
        }),
        sampler: params.iter().map(|&(k, v)| (k.to_string(), v)).collect(),
        ..base(id, name)
    }
}

/// A note on `channel` from `step` (a whole step) for `steps` steps.
fn note(channel: u32, step: u32, steps: u32, key: u8, velocity: f32) -> Note {
    Note {
        id: 0,
        channel,
        tick: step * STEP_TICKS,
        length: steps * STEP_TICKS,
        key,
        velocity,
        slide: false,
    }
}

/// The same, as a slide: the channel's held note glides to `key`.
fn slide(channel: u32, step: u32, steps: u32, key: u8, velocity: f32) -> Note {
    Note {
        slide: true,
        ..note(channel, step, steps, key, velocity)
    }
}

/// The M2 reference project and its sample files (read from `samples_dir`): 140 BPM, 4/4, key A
/// minor, 32 steps (two bars), exported as one loop plus the tail.
///
/// Channel volumes keep the sum below full scale: 808 0.5, Lead 0.8, Sub 0.45, the rest 1.
///
/// * "808" (synth, preset "808 Glide"): A1 (33) at step 0 for 6 steps with a slide to C2 (36) at
///   step 4, E1 (28) at step 16, and a slide to E2 (40) at step 24, after E1 has been released.
/// * "Keys" (synth, "Bell Keys", volume 0.8, pan -0.3): the chord A3-C4-E4 at step 0 and F3-A3-C4
///   at step 16, 8 steps each.
/// * "Lead" (synth, "Trap Lead" with its glide set to 0): an 8-note line from step 8 with one
///   slide (step 13), the only note that bends.
/// * "Sub" (`sub.wav`, a 55 Hz sine): root 33, loop on from the middle, low-pass 800 Hz with a
///   filter envelope, legato with an 80 ms glide, velocity on the level, a 2 ms attack, 300 ms
///   decay to 0.6, +12 cents, start 0.01; overlapping notes, so each glides to the next.
/// * "Closed hat" (`hat.wav`) on every odd step and "Open hat" (`open_hat.wav`, gain -3 dB) on
///   steps 2, 6, 10, ... (even steps), both in choke group 1: each closed hat cuts the open
///   hat before it.
///
/// # Panics
///
/// If a kit file cannot be read.
pub fn notes_m2_project(samples_dir: &Path) -> (Project, SampleFiles) {
    let mut keys = synth_channel(KEYS, "Keys", "Bell Keys");
    keys.volume = 0.8;
    keys.pan = -0.3;
    let sub = sampler_channel(
        SUB,
        "Sub",
        "sub.wav",
        &[
            ("root", 33.0),
            ("loop", 1.0),
            ("loop_start", 0.5),
            ("filter_type", 1.0),
            ("cutoff", 800.0),
            ("filter_env", 0.4),
            ("voice_mode", 2.0),
            ("glide", 80.0),
            ("vel_amp", 0.5),
            ("attack", 2.0),
            ("decay", 300.0),
            ("sustain", 0.6),
            ("fine", 12.0),
            ("start", 0.01),
        ],
    );
    // The preset's glide is zeroed, so only the slide note bends (over the default slide time)
    // and its flag is audible: without it the legato note would jump to its key.
    let mut lead = synth_channel(LEAD, "Lead", "Trap Lead");
    lead.synth.insert("glide".into(), 0.0);
    lead.volume = 0.8;
    let sub = Channel {
        volume: 0.45,
        ..sub
    };
    let mut closed = sampler_channel(CLOSED_HAT, "Closed hat", "hat.wav", &[]);
    closed.choke = 1;
    let mut open = sampler_channel(OPEN_HAT, "Open hat", "open_hat.wav", &[("gain", -3.0)]);
    open.choke = 1;

    let mut notes = vec![
        // 808: a held A1 that slides up to C2 (overlapping: a legato synth glides there with or
        // without the flag), then E1.
        note(SYNTH_808, 0, 6, 33, 1.0),
        slide(SYNTH_808, 4, 2, 36, 1.0),
        note(SYNTH_808, 16, 6, 28, 0.9),
        // After that note's release a legato synth glides only on a slide note: this one bends up
        // from E1 to E2, a plain note there would start at E2.
        slide(SYNTH_808, 24, 4, 40, 0.9),
    ];
    // Keys: two chords.
    notes.extend([57, 60, 64].map(|key| note(KEYS, 0, 8, key, 0.7)));
    notes.extend([53, 57, 60].map(|key| note(KEYS, 16, 8, key, 0.7)));
    // Lead: A minor, one slide (step 13 bends the note held from step 12).
    notes.extend([
        note(LEAD, 8, 2, 69, 0.8),
        note(LEAD, 10, 2, 72, 0.7),
        note(LEAD, 12, 2, 76, 0.9),
        slide(LEAD, 13, 1, 74, 0.9),
        note(LEAD, 14, 1, 72, 0.6),
        note(LEAD, 24, 2, 71, 0.8),
        note(LEAD, 26, 2, 72, 0.7),
        note(LEAD, 28, 3, 76, 1.0),
    ]);
    // Sub: each note starts while the one before still sounds (legato).
    notes.extend([
        note(SUB, 0, 6, 45, 1.0),
        note(SUB, 4, 6, 48, 0.6),
        note(SUB, 12, 5, 43, 0.9),
        note(SUB, 16, 6, 45, 1.0),
        note(SUB, 20, 8, 52, 0.5),
        note(SUB, 26, 5, 40, 0.8),
    ]);
    // Hats: closed on the odd steps, open on every other even step.
    for step in (1..STEPS).step_by(2) {
        notes.push(note(
            CLOSED_HAT,
            step,
            1,
            DEFAULT_KEY,
            if step % 4 == 1 { 0.9 } else { 0.6 },
        ));
    }
    for step in (2..STEPS).step_by(4) {
        notes.push(note(OPEN_HAT, step, 1, DEFAULT_KEY, 0.8));
    }
    for (n, id) in notes.iter_mut().zip(1..) {
        n.id = id;
    }
    let next_note_id = notes.len() as u32 + 1;

    let project = Project {
        play_mode: Default::default(),
        arrangement: Default::default(),
        name: "Notes M2".into(),
        bpm: 140.0,
        time_signature: TimeSignature { beats: 4, unit: 4 },
        channels: vec![
            Channel {
                volume: 0.5,
                ..synth_channel(SYNTH_808, "808", "808 Glide")
            },
            keys,
            lead,
            sub,
            closed,
            open,
        ],
        patterns: vec![Pattern {
            id: 0,
            name: "Pattern 1".into(),
            steps: STEPS,
            swing: 0.0,
            notes,
            next_note_id,
            revision: 1,
        }],
        current_pattern: 0,
        next_id: 7,
        key: Some(ProjectKey {
            root: 9,
            scale: "minor".into(),
        }),
    };

    let samples = [
        (SUB, "sub.wav"),
        (CLOSED_HAT, "hat.wav"),
        (OPEN_HAT, "open_hat.wav"),
    ]
    .into_iter()
    .map(|(id, file)| {
        let path = samples_dir.join(file);
        let bytes = read_sample_file(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        (id, bytes)
    })
    .collect();
    (project, samples)
}
