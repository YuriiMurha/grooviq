//! Nested same-key notes: on one key, a short note inside a long one (the inner ends first) and
//! a note struck inside another that outlives it (the outer ends first), on a looping sampler
//! and on the synth in poly mode with a fast attack (a new voice per strike) and a slow one (the
//! voice retriggers in place). Each note-off must end its own note and the key must sound until
//! its last held note ends, so the export is pinned byte for byte in `refs/nested_notes.wav`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use grooviq_dsp::synth;
use grooviq_engine::{InstrumentKind, STEP_TICKS};
use grooviq_project::{
    Channel, Note, Pattern, Project, SampleFiles, SampleRef, TimeSignature, read_sample_file,
};

/// Loops the export plays.
pub const LOOPS: u32 = 1;
/// Steps in the pattern: one bar at 120 BPM, two seconds.
pub const STEPS: u32 = 16;
/// Channel ids.
pub const SUB: u32 = 1;
pub const FAST: u32 = 2;
pub const SLOW: u32 = 3;

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
        volume: 0.4,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
    }
}

/// A poly synth channel from `preset`, its amp release shortened to `release` ms.
fn synth_channel(id: u32, name: &str, preset: &str, release: f32) -> Channel {
    let values = synth::preset(preset).unwrap_or_else(|| panic!("no synth preset {preset:?}"));
    let mut params: BTreeMap<String, f32> = values
        .into_iter()
        .map(|(index, value)| (synth::PARAM_IDS[index as usize].to_string(), value))
        .collect();
    params.insert("amp_release".into(), release);
    params.insert("voice_mode".into(), 0.0);
    Channel {
        instrument: InstrumentKind::Synth,
        synth: params,
        preset: Some(preset.into()),
        ..base(id, name)
    }
}

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

/// The two nested pairs on `key` of `channel`: steps 0-6 holding 2-4 (inner ends first), then
/// steps 8-11 holding the start of 10-15 (outer ends first).
fn pairs(channel: u32, key: u8) -> [Note; 4] {
    [
        note(channel, 0, 6, key, 1.0),
        note(channel, 2, 2, key, 0.4),
        note(channel, 8, 3, key, 1.0),
        note(channel, 10, 5, key, 0.4),
    ]
}

/// The nested-notes project and its sample file (`sub.wav`, read from `samples_dir`).
///
/// # Panics
///
/// If the kit file cannot be read.
pub fn nested_notes_project(samples_dir: &Path) -> (Project, SampleFiles) {
    let sub = Channel {
        // Only the file name: the project's JSON is the same on every machine.
        sample: Some(SampleRef {
            path: PathBuf::from("sub.wav"),
            file: None,
        }),
        // Gate mode and the loop on: each note sounds until its own release.
        sampler: [
            ("root", 33.0),
            ("one_shot", 0.0),
            ("loop", 1.0),
            ("release", 30.0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        ..base(SUB, "Sub")
    };
    let mut notes: Vec<Note> = [pairs(SUB, 33), pairs(FAST, 57), pairs(SLOW, 64)]
        .into_iter()
        .flatten()
        .collect();
    for (n, id) in notes.iter_mut().zip(1..) {
        n.id = id;
    }
    let next_note_id = notes.len() as u32 + 1;
    let project = Project {
        name: "Nested notes".into(),
        bpm: 120.0,
        time_signature: TimeSignature { beats: 4, unit: 4 },
        channels: vec![
            sub,
            synth_channel(FAST, "Init", "Init", 120.0),
            synth_channel(SLOW, "Dark Pad", "Dark Pad", 200.0),
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
        next_id: 4,
        key: None,
    };
    let path = samples_dir.join("sub.wav");
    let bytes =
        read_sample_file(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    (project, [(SUB, bytes)].into_iter().collect())
}
