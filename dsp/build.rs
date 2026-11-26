//! Generate the sampler's polyphase FIR coefficients once, at build time.
use std::env;
use std::f64::consts::TAU;
use std::fs;
use std::path::PathBuf;

const TAPS: usize = 48;
const PHASES: usize = 32;
const BANDS: usize = 64;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build directory"));
    let mut bytes = Vec::with_capacity((BANDS + 1) * (PHASES + 1) * TAPS * 4);
    for band in 0..=BANDS {
        let cutoff = 0.5 * band as f64 / BANDS as f64;
        for phase in 0..=PHASES {
            let phase = phase as f64 / PHASES as f64;
            let row: [f64; TAPS] = std::array::from_fn(|i| {
                let distance = i as f64 - (TAPS / 2 - 1) as f64 - phase;
                let angle = TAU * distance / TAPS as f64;
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
    fs::write(out.join("sampler-fir.bin"), bytes).expect("write sampler coefficients");
    fs::write(
        out.join("sampler-fir.rs"),
        format!(
            "const TAPS: usize = {TAPS};\nconst PHASES: usize = {PHASES};\nconst BANDS: usize = {BANDS};\n"
        ),
    )
    .expect("write sampler coefficient dimensions");
}
