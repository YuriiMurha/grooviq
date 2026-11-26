//! Generate the sampler's polyphase FIR coefficients once, at build time.
use std::env;
use std::f64::consts::TAU;
use std::fs;
use std::path::PathBuf;

const TAPS: usize = 48;
const WIDE_TAPS: usize = 80;
const PHASES: usize = 32;
const BANDS: usize = 64;
// At band 20 (step ≈2.76), both banks contain the original kernel.
const JOIN_BAND: usize = 20;
const FULL_WIDTH_BAND: usize = 14;

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
    fs::write(out.join("sampler-fir.bin"), bytes).expect("write sampler coefficients");
    fs::write(
        out.join("sampler-fir.rs"),
        format!(
            "const TAPS: usize = {TAPS};\nconst WIDE_TAPS: usize = {WIDE_TAPS};\n\
             const PHASES: usize = {PHASES};\nconst BANDS: usize = {BANDS};\n\
             const JOIN_BAND: usize = {JOIN_BAND};\n"
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
    for phase in 0..=PHASES {
        let phase = phase as f64 / PHASES as f64;
        let row: [f64; N] = std::array::from_fn(|i| {
            // At the join, pad the original row exactly, including its phase endpoints.
            let padding = (N - TAPS) / 2;
            if expansion == 0.0 && !(padding..padding + TAPS).contains(&i) {
                return 0.0;
            }
            let distance = i as f64 - (N / 2 - 1) as f64 - phase;
            if distance.abs() > width / 2.0 {
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
