//! Mip-mapped, band-limited wavetables for the synth's morphing oscillator.
//!
//! Four shapes (sine, triangle, saw, square) at 31 levels spaced a third of an octave apart.
//! Level `l` keeps the first `floor(1024 * 2^(-l/3))` harmonics, built exactly with an inverse
//! FFT, so each table is perfectly band-limited. The oscillator picks the level from its pitch
//! and crossfades to the next one, which keeps glides smooth (no harmonics popping in or out).
//! Tables are built once per process and shared by every synth instance.

use crate::fft::Fft;
use std::sync::OnceLock;

pub const TABLE_LEN: usize = 2048;
pub const LEVELS: usize = 31;
pub const SHAPES: usize = 4;
const STRIDE: usize = TABLE_LEN + 1;

pub struct Wavetables {
    data: Vec<f32>,
}

/// Number of harmonics kept at mip level `level`.
pub fn harmonics(level: usize) -> usize {
    ((1024.0 * 2f64.powf(-(level as f64) / 3.0)).floor() as usize).clamp(1, TABLE_LEN / 2 - 1)
}

/// Sine-series coefficient of harmonic `k` for `shape`. All shapes have a positive fundamental
/// so neighbouring shapes reinforce (not cancel) when crossfaded.
fn coef(shape: usize, k: usize) -> f64 {
    use std::f64::consts::PI;
    let kf = k as f64;
    match shape {
        0 => {
            if k == 1 {
                1.0
            } else {
                0.0
            }
        }
        1 => {
            if k % 2 == 1 {
                let sign = if (k / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
                sign * 8.0 / (PI * PI * kf * kf)
            } else {
                0.0
            }
        }
        2 => 2.0 / (PI * kf),
        _ => {
            if k % 2 == 1 {
                4.0 / (PI * kf)
            } else {
                0.0
            }
        }
    }
}

impl Wavetables {
    fn build() -> Self {
        let fft = Fft::new(TABLE_LEN);
        let mut data = vec![0.0f32; SHAPES * LEVELS * STRIDE];
        let mut re = vec![0.0f64; TABLE_LEN];
        let mut im = vec![0.0f64; TABLE_LEN];
        for s in 0..SHAPES {
            for l in 0..LEVELS {
                re.fill(0.0);
                im.fill(0.0);
                for k in 1..=harmonics(l) {
                    let b = coef(s, k);
                    // x[n] = sum b_k sin(2 pi k n / N) via an unscaled inverse FFT.
                    im[k] = -0.5 * b;
                    im[TABLE_LEN - k] = 0.5 * b;
                }
                fft.run(&mut re, &mut im, true);
                let base = (s * LEVELS + l) * STRIDE;
                for i in 0..TABLE_LEN {
                    data[base + i] = re[i] as f32;
                }
                data[base + TABLE_LEN] = data[base];
            }
        }
        Self { data }
    }

    /// Table for `shape` at mip `level`, `TABLE_LEN + 1` samples (last = first, for interpolation).
    #[inline(always)]
    pub fn table(&self, shape: usize, level: usize) -> &[f32] {
        let base = (shape * LEVELS + level) * STRIDE;
        &self.data[base..base + STRIDE]
    }
}

/// Process-wide shared tables (built on first use, outside the audio thread: in `Synth::new`).
pub fn shared() -> &'static Wavetables {
    static TABLES: OnceLock<Wavetables> = OnceLock::new();
    TABLES.get_or_init(Wavetables::build)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_have_expected_shape() {
        let t = shared();
        // sine
        let s = t.table(0, 0);
        assert!((s[TABLE_LEN / 4] - 1.0).abs() < 1e-5);
        // saw starts at 0 (centre of the jump) and is ~0.5 a quarter of the way through
        let saw = t.table(2, 0);
        assert!(saw[0].abs() < 1e-5);
        assert!((saw[TABLE_LEN / 4] - 0.5).abs() < 0.01);
        // square ~ +1 in the first half
        let sq = t.table(3, 0);
        assert!((sq[TABLE_LEN / 4] - 1.0).abs() < 0.01);
        // top level is a pure sine for every shape
        for shape in 0..SHAPES {
            let top = t.table(shape, LEVELS - 1);
            let amp = top.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(amp > 0.5 && amp < 1.3);
        }
    }
}
