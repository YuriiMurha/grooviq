//! The M1 acceptance scene: a 4-channel drum pattern built as a real [`Project`] from the kit in
//! `samples/` (made by `cargo run -p grooviq-golden --example make_kit`), saved and loaded through
//! the `.grooviq` file and exported to WAV. The export must be byte-identical to
//! `refs/drums_4ch.wav` on every platform.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grooviq_engine::{MAX_SAMPLE_SECONDS, SampleBuf, decode_sample};
use grooviq_project::{Channel, Note, Pattern, Project, SampleFiles, SampleRef, read_sample_file};

/// The four kit files, in channel order.
pub const KIT: [&str; 4] = ["kick.wav", "snare.wav", "hat.wav", "clap.wav"];
/// Loops the export plays.
pub const LOOPS: u32 = 2;
/// The hat step whose note lasts half a step (see [`drums_4ch_project`]).
pub const HALF_HAT_STEP: u32 = 6;

/// `tests/golden/samples`.
pub fn kit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples")
}

/// A hit of one step (the default length and key) at `step`.
fn hit(channel: u32, step: u32, velocity: f32) -> Note {
    Note {
        channel,
        tick: step * grooviq_engine::STEP_TICKS,
        length: grooviq_engine::STEP_TICKS,
        key: grooviq_project::DEFAULT_KEY,
        velocity,
    }
}

/// A channel playing `file` from `samples_dir`, with `sampler` settings by parameter name.
fn channel(id: u32, name: &str, file: &str, sampler: &[(&str, f32)]) -> Channel {
    Channel {
        id,
        name: name.into(),
        color: id as u8,
        sample: Some(SampleRef {
            // Only the file name: the project's JSON is the same on every machine.
            path: PathBuf::from(file),
            file: None,
        }),
        sampler: sampler
            .iter()
            .map(|&(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>(),
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
    }
}

/// The drum pattern and its sample files (read from `samples_dir` with [`read_sample_file`]):
/// 140 BPM, 16 steps, swing 0.5.
///
/// * Kick (`kick.wav`): steps 0, 3, 8, 10 at velocity 1.
/// * Snare (`snare.wav`, tune -1, pan -0.2): steps 4, 12 at 0.9.
/// * Hat (`hat.wav`, one-shot off, release 30 ms, volume 0.7, pan 0.3): every step, 0.9 on even
///   steps and 0.5 on odd ones. The step-6 hat is half a step long, so its note-off arrives
///   while it still sounds; it is the only note that reaches the sampler's release stage (the
///   other hats end before their note-offs), which keeps that maths in the reference.
/// * Clap (`clap.wav`, reverse on, pan -0.25): step 12 at 0.8.
///
/// # Panics
///
/// If a kit file cannot be read.
pub fn drums_4ch_project(samples_dir: &Path) -> (Project, SampleFiles) {
    let (kick, snare, hat, clap) = (1, 2, 3, 4);
    let mut snare_ch = channel(snare, "Snare", KIT[1], &[("tune", -1.0)]);
    snare_ch.pan = -0.2;
    let mut hat_ch = channel(hat, "Hat", KIT[2], &[("one_shot", 0.0), ("release", 30.0)]);
    hat_ch.volume = 0.7;
    hat_ch.pan = 0.3;
    let mut clap_ch = channel(clap, "Clap", KIT[3], &[("reverse", 1.0)]);
    clap_ch.pan = -0.25;

    let mut notes: Vec<Note> = [0, 3, 8, 10].iter().map(|&s| hit(kick, s, 1.0)).collect();
    notes.extend([4, 12].iter().map(|&s| hit(snare, s, 0.9)));
    notes.extend((0..16).map(|s| {
        let mut note = hit(hat, s, if s % 2 == 0 { 0.9 } else { 0.5 });
        if s == HALF_HAT_STEP {
            note.length = grooviq_engine::STEP_TICKS / 2;
        }
        note
    }));
    notes.push(hit(clap, 12, 0.8));

    let project = Project {
        name: "Drums 4ch".into(),
        bpm: 140.0,
        channels: vec![
            channel(kick, "Kick", KIT[0], &[]),
            snare_ch,
            hat_ch,
            clap_ch,
        ],
        patterns: vec![Pattern {
            id: 0,
            name: "Pattern 1".into(),
            steps: 16,
            swing: 0.5,
            notes,
        }],
        current_pattern: 0,
        next_id: 5,
        ..Project::new()
    };

    let samples = (1..=4u32)
        .zip(KIT)
        .map(|(id, file)| {
            let path = samples_dir.join(file);
            let bytes = read_sample_file(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            (id, bytes)
        })
        .collect();
    (project, samples)
}

/// Each sample file decoded for the engine, by channel id.
///
/// # Panics
///
/// If a file does not decode.
pub fn decode_samples(files: &SampleFiles) -> HashMap<u32, Arc<SampleBuf>> {
    files
        .iter()
        .map(|(&id, file)| {
            let ext = Path::new(&file.name).extension().and_then(|e| e.to_str());
            let buf = decode_sample(&file.bytes, ext, MAX_SAMPLE_SECONDS)
                .unwrap_or_else(|e| panic!("{} does not decode: {e}", file.name));
            (id, Arc::new(buf))
        })
        .collect()
}
