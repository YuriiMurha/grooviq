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
    /// One table after another (`table`): only the scalar reference and the tests read these; the
    /// shipped synth reads the interleaved copy.
    #[cfg(any(test, feature = "scalar-reference"))]
    data: Vec<f32>,
    /// The same samples interleaved for the SIMD oscillator, see [`Wavetables::quad`].
    quads: Vec<[f32; 4]>,
}

/// Number of harmonics kept at mip level `level`.
pub fn harmonics(level: usize) -> usize {
    ((1024.0 * libm::pow(2.0, -(level as f64) / 3.0)).floor() as usize).clamp(1, TABLE_LEN / 2 - 1)
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
        // Entry `i` of (shape pair `k`, level pair `l`): the four tables a morph blend reads.
        let mut quads = Vec::with_capacity((SHAPES - 1) * (LEVELS - 1) * STRIDE);
        for k in 0..SHAPES - 1 {
            for l in 0..LEVELS - 1 {
                let at = |shape: usize, level: usize| (shape * LEVELS + level) * STRIDE;
                let (a, b, c, d) = (at(k, l), at(k, l + 1), at(k + 1, l), at(k + 1, l + 1));
                for i in 0..STRIDE {
                    quads.push([data[a + i], data[b + i], data[c + i], data[d + i]]);
                }
            }
        }
        Self {
            #[cfg(any(test, feature = "scalar-reference"))]
            data,
            quads,
        }
    }

    /// Tables `k` and `k + 1` at mip levels `l` and `l + 1` interleaved: entry `i` is
    /// `[table(k, l)[i], table(k, l + 1)[i], table(k + 1, l)[i], table(k + 1, l + 1)[i]]`, so a
    /// morph blend at one phase reads two 16-byte entries (`i` and `i + 1`) instead of eight
    /// samples from four tables. Needs `k < SHAPES - 1` and `l < LEVELS - 1`.
    #[inline(always)]
    pub fn quad(&self, k: usize, l: usize) -> &[[f32; 4]; TABLE_LEN + 1] {
        let base = (k * (LEVELS - 1) + l) * STRIDE;
        self.quads[base..base + STRIDE]
            .try_into()
            .expect("a quad table has TABLE_LEN + 1 entries")
    }

    /// Table for `shape` at mip `level`, `TABLE_LEN + 1` samples (last = first, for interpolation).
    #[cfg(any(test, feature = "scalar-reference"))]
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
