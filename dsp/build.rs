//! Generate the sampler's polyphase FIR coefficients once, at build time.
use std::env;
use std::f64::consts::TAU;
use std::fs;
use std::path::PathBuf;

const TAPS: usize = 48;
const WIDE_TAPS: usize = 80;
const EXTREME_TAPS: usize = 256;
const PHASES: usize = 32;
const BANDS: usize = 64;
// At band 20 (step ≈2.76), both banks contain the original kernel.
const JOIN_BAND: usize = 20;
const FULL_WIDTH_BAND: usize = 14;
// The nominal cutoff at step 8 lands on wide band 6. Subdivide the narrow bands so
// interpolation still tracks the cutoff when it approaches zero at extreme steps.
const EXTREME_SUBDIVISIONS: usize = 4;
const EXTREME_JOIN_BAND: usize = 6 * EXTREME_SUBDIVISIONS;
const EXTREME_FULL_WIDTH_BAND: usize = 3 * EXTREME_SUBDIVISIONS;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build directory"));
    let mut bytes = Vec::new();
    for band in JOIN_BAND..=BANDS {
        write_band::<TAPS>(&mut bytes, band);
    }
    for band in 0..=JOIN_BAND {
        write_band::<WIDE_TAPS>(&mut bytes, band);
    }
    for band in 0..=EXTREME_JOIN_BAND {
        let expansion = (EXTREME_JOIN_BAND - band).min(EXTREME_JOIN_BAND - EXTREME_FULL_WIDTH_BAND)
            as f64
            / (EXTREME_JOIN_BAND - EXTREME_FULL_WIDTH_BAND) as f64;
        let width = WIDE_TAPS as f64 + (EXTREME_TAPS - WIDE_TAPS) as f64 * expansion;
        let cutoff =
            (0.5 * band as f64 / (BANDS * EXTREME_SUBDIVISIONS) as f64) * (1.09 - 0.02 * expansion);
        let support = if band == EXTREME_JOIN_BAND {
            WIDE_TAPS
        } else {
            EXTREME_TAPS
        };
        write_kernel::<EXTREME_TAPS>(&mut bytes, cutoff, width, support);
    }
    fs::write(out.join("sampler-fir.bin"), bytes).expect("write sampler coefficients");
    fs::write(
        out.join("sampler-fir.rs"),
        format!(
            "const TAPS: usize = {TAPS};\nconst WIDE_TAPS: usize = {WIDE_TAPS};\n\
             const PHASES: usize = {PHASES};\nconst BANDS: usize = {BANDS};\n\
             const JOIN_BAND: usize = {JOIN_BAND};\n\
             const EXTREME_TAPS: usize = {EXTREME_TAPS};\n\
             const EXTREME_SUBDIVISIONS: usize = {EXTREME_SUBDIVISIONS};\n\
             const EXTREME_JOIN_BAND: usize = {EXTREME_JOIN_BAND};\n"
        ),
    )
    .expect("write sampler coefficient dimensions");
}

fn write_band<const N: usize>(bytes: &mut Vec<u8>, band: usize) {
    // Gradually widen the window and move the transition toward Nyquist as the
    // destination bandwidth shrinks. Interpolating the rows also interpolates this change.
    let expansion = JOIN_BAND
        .saturating_sub(band)
        .min(JOIN_BAND - FULL_WIDTH_BAND) as f64
        / (JOIN_BAND - FULL_WIDTH_BAND) as f64;
    let width = TAPS as f64 + (WIDE_TAPS - TAPS) as f64 * expansion;
    let cutoff = (0.5 * band as f64 / BANDS as f64) * (1.0 + 0.09 * expansion);
    let support = if expansion == 0.0 { TAPS } else { N };
    write_kernel::<N>(bytes, cutoff, width, support);
}

fn write_kernel<const N: usize>(bytes: &mut Vec<u8>, cutoff: f64, width: f64, support: usize) {
    // Pad shared rows exactly, including their phase endpoints, without moving lanes.
    let padding = (N - support) / 2;
    for phase in 0..=PHASES {
        let phase = phase as f64 / PHASES as f64;
        let row: [f64; N] = std::array::from_fn(|i| {
            let distance = i as f64 - (N / 2 - 1) as f64 - phase;
            if !(padding..N - padding).contains(&i) || distance.abs() > width / 2.0 {
                return 0.0;
            }
            let angle = TAU * distance / width;
            let window = 0.42 + 0.5 * libm::cos(angle) + 0.08 * libm::cos(2.0 * angle);
            let x = TAU * cutoff * distance;
            // The scale factor 2*cutoff cancels in the DC normalization.
            window * if x == 0.0 { 1.0 } else { libm::sin(x) / x }
        });
        let gain = row.iter().sum::<f64>();
        for weight in row {
            bytes.extend_from_slice(&((weight / gain) as f32).to_le_bytes());
        }
    }
}
