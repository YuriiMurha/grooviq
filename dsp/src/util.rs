//! Shared building blocks: parameter smoothing, denormal flushing, saturation, biquads,
//! one-pole filters, delay lines with fractional reads, meters and a tiny PRNG.
//! Nothing here allocates after construction.

use crate::api::gain_to_db;
use std::f64::consts::PI as PI64;

/// Flushes denormals (anything below ~1e-25) to zero. Branch-free; used on every feedback path.
#[inline(always)]
pub fn flush(x: f32) -> f32 {
    (x + 1.0e-18) - 1.0e-18
}

/// f64 variant of [`flush`] (flushes below ~1e-216, far above the f64 denormal range).
#[inline(always)]
pub fn flush64(x: f64) -> f64 {
    (x + 1.0e-200) - 1.0e-200
}

/// Replaces NaN/inf by 0. Used on inputs so a bad host buffer cannot poison filter state.
#[inline(always)]
pub fn sane(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// Rational tanh (Lambert continued fraction, 7/6). Max abs error ~2e-6 up to the clamp.
#[inline(always)]
pub fn fast_tanh(x: f32) -> f32 {
    let x = x.clamp(-4.97, 4.97);
    let x2 = x * x;
    let num = x * (135135.0 + x2 * (17325.0 + x2 * (378.0 + x2)));
    let den = 135135.0 + x2 * (62370.0 + x2 * (3150.0 + 28.0 * x2));
    num / den
}

/// Unity-gain below |x| = 1, then a tanh knee that never exceeds 2. Safety net for feedback loops.
#[inline(always)]
pub fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= 1.0 {
        x
    } else {
        (1.0 + fast_tanh(a - 1.0)).copysign(x)
    }
}

/// One-pole exponential parameter smoother. `next()` is called once per sample (or once per
/// control tick); it snaps to the target once the remaining distance is negligible so that
/// `settled()` becomes true and callers can skip work.
#[derive(Clone, Copy, Debug)]
pub struct Smooth {
    pub cur: f32,
    pub target: f32,
    coef: f32,
}

impl Smooth {
    /// `time_ms` is the time constant, `rate` the number of `next()` calls per second.
    pub fn new(value: f32, time_ms: f32, rate: f32) -> Self {
        Self {
            cur: value,
            target: value,
            coef: libm::expf(-1000.0 / (time_ms * rate).max(1.0e-3)),
        }
    }
    #[inline]
    pub fn set(&mut self, v: f32) {
        self.target = v;
    }
    #[inline]
    pub fn snap(&mut self) {
        self.cur = self.target;
    }
    #[inline]
    pub fn settled(&self) -> bool {
        self.cur == self.target
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.cur != self.target {
            let d = (self.cur - self.target) * self.coef;
            self.cur = if d.abs() < 1.0e-5 * (1.0 + self.target.abs()) {
                self.target
            } else {
                self.target + d
            };
        }
        self.cur
    }
}

/// Normalised biquad coefficients (a0 = 1), f64. RBJ cookbook designs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Default for Coefs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Coefs {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        let ia = 1.0 / a0;
        Self {
            b0: b0 * ia,
            b1: b1 * ia,
            b2: b2 * ia,
            a1: a1 * ia,
            a2: a2 * ia,
        }
    }
    fn w(f: f64, sr: f64) -> (f64, f64) {
        let w0 = 2.0 * PI64 * (f / sr).clamp(1.0e-6, 0.4999);
        (libm::cos(w0), libm::sin(w0))
    }
    pub fn lowpass(f: f64, q: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        Self::norm(
            (1.0 - c) * 0.5,
            1.0 - c,
            (1.0 - c) * 0.5,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }
    pub fn highpass(f: f64, q: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        Self::norm(
            (1.0 + c) * 0.5,
            -(1.0 + c),
            (1.0 + c) * 0.5,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }
    pub fn notch(f: f64, q: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        Self::norm(1.0, -2.0 * c, 1.0, 1.0 + al, -2.0 * c, 1.0 - al)
    }
    pub fn allpass(f: f64, q: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        Self::norm(1.0 - al, -2.0 * c, 1.0 + al, 1.0 + al, -2.0 * c, 1.0 - al)
    }
    pub fn peak(f: f64, q: f64, db: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        let a = libm::pow(10.0, db / 40.0);
        Self::norm(
            1.0 + al * a,
            -2.0 * c,
            1.0 - al * a,
            1.0 + al / a,
            -2.0 * c,
            1.0 - al / a,
        )
    }
    pub fn low_shelf(f: f64, q: f64, db: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        let a = libm::pow(10.0, db / 40.0);
        let k = 2.0 * a.sqrt() * al;
        Self::norm(
            a * ((a + 1.0) - (a - 1.0) * c + k),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - k),
            (a + 1.0) + (a - 1.0) * c + k,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - k,
        )
    }
    pub fn high_shelf(f: f64, q: f64, db: f64, sr: f64) -> Self {
        let (c, s) = Self::w(f, sr);
        let al = s / (2.0 * q.max(0.01));
        let a = libm::pow(10.0, db / 40.0);
        let k = 2.0 * a.sqrt() * al;
        Self::norm(
            a * ((a + 1.0) + (a - 1.0) * c + k),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - k),
            (a + 1.0) - (a - 1.0) * c + k,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - k,
        )
    }
}

/// Transposed direct form II biquad state (f64). Coefficients are passed in so channels can
/// share one set.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    z1: f64,
    z2: f64,
}

impl Biquad {
    #[inline(always)]
    pub fn run(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = flush64(c.b1 * x - c.a1 * y + self.z2);
        self.z2 = flush64(c.b2 * x - c.a2 * y);
        y
    }
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Two identical biquads in series (e.g. a Linkwitz-Riley 24 dB/oct section).
#[inline(always)]
pub fn run2(st: &mut [Biquad; 2], c: &Coefs, x: f64) -> f64 {
    let y = st[0].run(c, x);
    st[1].run(c, y)
}

/// `G = g / (1 + g)` for a TPT one-pole at `fc`.
pub fn onepole_gain(fc: f32, sr: f32) -> f32 {
    let g = libm::tanf(std::f32::consts::PI * (fc / sr).clamp(1.0e-6, 0.49));
    g / (1.0 + g)
}

/// Topology-preserving (trapezoidal) one-pole low-pass; `hp = x - lp`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    s: f32,
}

impl OnePole {
    #[inline(always)]
    pub fn lp(&mut self, x: f32, gg: f32) -> f32 {
        let v = (x - self.s) * gg;
        let y = v + self.s;
        self.s = flush(y + v);
        y
    }
    pub fn reset(&mut self) {
        self.s = 0.0;
    }
}

/// First-order DC blocker.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline(always)]
    pub fn run(&mut self, x: f32, r: f32) -> f32 {
        let y = x - self.x1 + r * self.y1;
        self.x1 = x;
        self.y1 = flush(y);
        y
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Coefficient for [`DcBlock`] with its corner at `fc`.
pub fn dc_coef(fc: f32, sr: f32) -> f32 {
    1.0 - 2.0 * std::f32::consts::PI * fc / sr
}

/// 4-point, 3rd-order Hermite interpolation between `x0` (t = 0) and `x1` (t = 1).
#[inline(always)]
pub fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

/// Power-of-two ring buffer. `push` then `read_*(d)` gives the sample from `d` pushes ago
/// (`d = 0` is the sample just pushed).
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl DelayLine {
    pub fn new(max_delay: usize) -> Self {
        let n = (max_delay + 4).next_power_of_two();
        Self {
            buf: vec![0.0; n],
            mask: n - 1,
            pos: 0,
        }
    }
    /// Largest delay that can be read safely.
    pub fn max_delay(&self) -> usize {
        self.buf.len() - 4
    }
    #[inline(always)]
    pub fn push(&mut self, x: f32) {
        self.pos = (self.pos + 1) & self.mask;
        self.buf[self.pos] = x;
    }
    #[inline(always)]
    pub fn read_int(&self, d: usize) -> f32 {
        self.buf[self.pos.wrapping_sub(d) & self.mask]
    }
    /// Fractional read: Hermite for d >= 1, linear between the two newest samples below that.
    #[inline(always)]
    pub fn read_frac(&self, d: f32) -> f32 {
        let d = d.clamp(0.0, self.max_delay() as f32 - 1.0);
        let i = d as usize;
        let t = d - i as f32;
        let x0 = self.read_int(i);
        let x1 = self.read_int(i + 1);
        if i == 0 {
            return x0 + (x1 - x0) * t;
        }
        hermite(self.read_int(i - 1), x0, x1, self.read_int(i + 2), t)
    }
    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Peak-hold level meter with a ~26 dB/s fall, updated once per block.
#[derive(Clone, Copy, Debug, Default)]
pub struct PeakMeter {
    pub value: f32,
}

impl PeakMeter {
    pub fn update(&mut self, block_peak: f32, frames: usize, sr: f32) {
        let fall = libm::expf(-(frames as f32) / (0.33 * sr));
        let v = self.value * fall;
        self.value = if block_peak.is_finite() && block_peak > v {
            block_peak
        } else {
            v
        };
        if self.value < 1.0e-7 {
            self.value = 0.0;
        }
    }
    pub fn db(&self) -> f32 {
        gain_to_db(self.value)
    }
}

/// Largest absolute value in a buffer (NaN-tolerant).
pub fn peak_of(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// xorshift32 PRNG (deterministic, allocation-free).
#[derive(Clone, Copy, Debug)]
pub struct Rng(pub u32);

impl Rng {
    #[inline(always)]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in [-1, 1).
    #[inline(always)]
    pub fn bipolar(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }
    /// Uniform in [0, 1).
    #[inline(always)]
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }
}

/// Clamps `value` into `(min, max)` from a spec table and rejects NaN/inf.
pub fn clamp_param(spec: &[(f32, f32, f32)], index: u32, value: f32) -> Option<f32> {
    let (lo, hi, _) = *spec.get(index as usize)?;
    if !value.is_finite() {
        return None;
    }
    Some(value.clamp(lo, hi))
}

/// Mono level of a stereo pair for scopes/analysers.
#[inline(always)]
pub fn mid(l: f32, r: f32) -> f32 {
    0.5 * (l + r)
}

/// In-place 8-point fast Walsh-Hadamard transform, orthonormal (scaled by 1/sqrt(8)).
#[inline(always)]
pub fn hadamard8(x: &mut [f32; 8]) {
    let mut h = 1;
    while h < 8 {
        let mut i = 0;
        while i < 8 {
            for j in i..i + h {
                let a = x[j];
                let b = x[j + h];
                x[j] = a + b;
                x[j + h] = a - b;
            }
            i += h * 2;
        }
        h *= 2;
    }
    const S: f32 = 0.353_553_39;
    for v in x.iter_mut() {
        *v *= S;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_tanh_is_accurate_and_bounded() {
        let mut worst = 0.0f32;
        let mut x = -8.0f32;
        while x < 8.0 {
            let e = (fast_tanh(x) - libm::tanhf(x)).abs();
            worst = worst.max(e);
            assert!(fast_tanh(x).abs() <= 1.0);
            x += 0.001;
        }
        assert!(worst < 1.0e-4, "worst error {worst}");
    }

    #[test]
    fn flush_kills_denormals_only() {
        assert_eq!(flush(1.0e-30), 0.0);
        assert_eq!(flush(f32::MIN_POSITIVE * 0.5), 0.0);
        assert!((flush(1.0e-6) - 1.0e-6).abs() < 1.0e-12);
        assert_eq!(flush64(1.0e-300), 0.0);
    }
}
