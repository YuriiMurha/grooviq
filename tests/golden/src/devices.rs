//! Golden renders of single devices: a deterministic test input (generated in code) is run
//! through the device with each factory preset and compared with `refs/ott_<preset>.wav`.
//! Refresh with `GOLDEN_UPDATE=1 cargo test -p grooviq-golden --test devices` and say why in the
//! commit message. The devices use std maths, so only [`crate::TOLERANCE`] is guaranteed across
//! platforms, not bit-exactness.

use grooviq_dsp::ott::{PRESETS, preset};
use grooviq_dsp::{Device, create};

use crate::SAMPLE_RATE;

/// Length of every device render, in seconds.
pub const SECONDS: f32 = 1.0;
/// Block size the device is driven with (the Web Audio render quantum).
pub const BLOCK: usize = 128;

/// xorshift64 (integer maths, so identical everywhere).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// The input: a seeded noise burst (150 ms, fast fade-out, about -9 dBFS) followed after 100 ms
/// by a decaying sine that falls from -8 dBFS to -68 dBFS, so loud, quiet and in-between levels
/// all occur. The right channel differs (other seed, other pitch). Returns `(left, right)`.
pub fn test_input() -> (Vec<f32>, Vec<f32>) {
    let n = (SECONDS * SAMPLE_RATE as f32).round() as usize;
    let sr = SAMPLE_RATE as f64;
    let burst = (0.15 * sr) as usize;
    let tail_start = (0.25 * sr) as usize;
    let channel = |seed: u64, hz: f64| -> Vec<f32> {
        let mut rng = Rng(seed);
        (0..n)
            .map(|i| {
                let mut v = 0.0f64;
                if i < burst {
                    let fade = 1.0 - i as f64 / burst as f64;
                    v += 0.35 * fade * fade * rng.next();
                }
                if i >= tail_start {
                    let t = (i - tail_start) as f64 / sr;
                    let amp = 0.4 * 10f64.powf(-60.0 * t / 0.75 / 20.0);
                    v += amp * (2.0 * std::f64::consts::PI * hz * t).sin();
                }
                v as f32
            })
            .collect()
    };
    (
        channel(0x9E37_79B9_7F4A_7C15, 220.0),
        channel(0xD1B5_4A32_D192_ED03, 330.0),
    )
}

/// Runs the test input through the multiband compressor with a factory preset (block size
/// `block`) and returns interleaved stereo.
pub fn render_ott(preset_name: &str, block: usize) -> Vec<f32> {
    let (mut l, mut r) = test_input();
    let mut dev: Box<dyn Device> =
        create("ott", SAMPLE_RATE as f32).expect("the ott device exists");
    for (index, value) in preset(preset_name).expect("the preset exists") {
        dev.set_param(index, value);
    }
    let n = l.len();
    let mut i = 0;
    while i < n {
        let e = (i + block).min(n);
        dev.process(&mut l[i..e], &mut r[i..e], None, &[]);
        i = e;
    }
    l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect()
}

/// `(preset name, reference file stem)` of every golden device render.
pub fn ott_cases() -> Vec<(&'static str, String)> {
    PRESETS
        .iter()
        .map(|&name| {
            (
                name,
                format!("ott_{}", name.to_lowercase().replace(' ', "_")),
            )
        })
        .collect()
}
