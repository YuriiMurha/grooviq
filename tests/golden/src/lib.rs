//! Golden renders: reference scenes rendered offline by the real engine and compared with WAV
//! files stored in `refs/`. Any unintended change to the engine or the synth's sound fails the
//! comparison. Refresh the refs on purpose with `GOLDEN_UPDATE=1 cargo test -p grooviq-golden`
//! and say why in the commit message.

use std::path::Path;

use grooviq_dsp::synth::preset;
use grooviq_engine::{Command, EngineConfig, render_offline};

pub mod devices;

/// Sample rate of every scene and reference file.
pub const SAMPLE_RATE: u32 = 48_000;
/// Largest allowed absolute difference between a render and its reference.
pub const TOLERANCE: f32 = 1e-4;

/// A reference scene: what to play and for how long.
pub struct Scene {
    pub name: &'static str,
    pub seconds: f32,
    pub events: Vec<(u64, Command)>,
}

/// Every reference scene.
pub fn scenes() -> Vec<Scene> {
    let tone = Scene {
        name: "tone_440",
        seconds: 1.0,
        events: vec![(
            0,
            Command::TestTone {
                on: true,
                hz: 440.0,
                gain_db: -12.0,
            },
        )],
    };

    let mut events: Vec<(u64, Command)> = preset("808 Glide")
        .expect("the 808 Glide preset exists")
        .into_iter()
        .map(|(index, value)| (0, Command::SetSynthParam { index, value }))
        .collect();
    let note = |key, on| Command::Note {
        key,
        velocity: 1.0,
        on,
    };
    // Key 39 starts while 36 is held: a legato slide. Then both release.
    events.extend([
        (0, note(36, true)),
        (24_000, note(39, true)),
        (30_000, note(36, false)),
        (72_000, note(39, false)),
    ]);
    let synth = Scene {
        name: "synth_808_glide",
        seconds: 2.0,
        events,
    };
    vec![tone, synth]
}

/// Renders a scene as interleaved stereo, calling the engine `callback` frames at a time.
pub fn render(scene: &Scene, callback: usize) -> Vec<f32> {
    let config = EngineConfig {
        sample_rate: SAMPLE_RATE as f32,
    };
    let frames = (scene.seconds * SAMPLE_RATE as f32).round() as usize;
    render_offline(config, &scene.events, frames, callback)
}

/// Same length and every sample within [`TOLERANCE`] (a NaN never matches).
pub fn compare(actual: &[f32], reference: &[f32]) -> Result<(), String> {
    if actual.len() != reference.len() {
        return Err(format!(
            "length differs: {} samples rendered, {} in the reference",
            actual.len(),
            reference.len()
        ));
    }
    let mut worst: Option<(usize, f32)> = None;
    for (i, (a, r)) in actual.iter().zip(reference).enumerate() {
        let diff = (a - r).abs();
        let bad = diff.is_nan() || diff > TOLERANCE;
        if bad && worst.is_none_or(|(_, w)| diff.is_nan() || diff > w) {
            worst = Some((i, diff));
        }
    }
    match worst {
        None => Ok(()),
        Some((i, diff)) => Err(format!(
            "max difference {diff} at sample {i} exceeds {TOLERANCE}"
        )),
    }
}

/// Writes interleaved stereo as a 32-bit float WAV at [`SAMPLE_RATE`].
pub fn write_wav(path: impl AsRef<Path>, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create the reference file");
    for &s in samples {
        writer.write_sample(s).expect("write a sample");
    }
    writer.finalize().expect("finish the reference file");
}

/// Reads a reference WAV; panics unless it is 32-bit float stereo at [`SAMPLE_RATE`].
pub fn read_wav(path: impl AsRef<Path>) -> Vec<f32> {
    let path = path.as_ref();
    let mut reader = hound::WavReader::open(path).unwrap_or_else(|e| {
        panic!(
            "cannot open {} ({e}); create it with GOLDEN_UPDATE=1",
            path.display()
        )
    });
    let spec = reader.spec();
    assert!(
        spec.channels == 2
            && spec.sample_rate == SAMPLE_RATE
            && spec.bits_per_sample == 32
            && spec.sample_format == hound::SampleFormat::Float,
        "{}: expected 32-bit float stereo at {SAMPLE_RATE} Hz, got {spec:?}",
        path.display()
    );
    reader
        .samples::<f32>()
        .map(|s| s.expect("read a sample"))
        .collect()
}
