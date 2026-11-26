//! 8-band parametric EQ.
//!
//! RBJ biquads in transposed direct form II with f64 state. Cuts are 24 dB/oct (two cascaded
//! sections; Q shapes the knee, 0.71 = Butterworth). Each band can work on stereo, mid or side.
//! Frequency/gain/Q glide on a 16-sample control grid (coefficients are recomputed there; the
//! grid is a global sample counter so results do not depend on block size). Switching a band on
//! or off crossfades it in/out.
//!
//! Array meters 0/1: 128 log-spaced bins (20 Hz..20 kHz), dB, pre/post EQ. Both spectra come from one
//! 2048-point complex FFT (pre in the real part, post in the imaginary part), Hann-windowed,
//! ~30 updates per second, with peak-hold-and-fall smoothing.
//! Scalar meters 2–9: effective frequencies in Hz for bands 1–8, in band order. Each reports
//! the frequency used for the current coefficients, including control-rate smoothing.

use crate::api::{Device, db_to_gain};
use crate::fft::Fft;
use crate::util::*;

const BANDS: usize = 8;
const CTRL: u32 = 16;
const FFT_N: usize = 2048;
const BINS: usize = 128;
const OUTPUT: u32 = 48;

/// (on, type, freq, gain, q) per band, in `SPEC` index order.
const BAND_DEFAULTS: [(bool, u8, f32, f32, f32); BANDS] = [
    (true, 0, 30.0, 0.0, 0.71),
    (true, 1, 120.0, 0.0, 0.71),
    (true, 2, 400.0, 0.0, 1.0),
    (true, 2, 1200.0, 0.0, 1.0),
    (true, 2, 3500.0, 0.0, 1.0),
    (false, 2, 6000.0, 0.0, 1.0),
    (true, 4, 9000.0, 0.0, 0.71),
    (false, 5, 18000.0, 0.0, 0.71),
];

const fn eq_spec() -> [(f32, f32, f32); 49] {
    let mut s = [(0.0, 0.0, 0.0); 49];
    let mut n = 0;
    while n < BANDS {
        let d = BAND_DEFAULTS[n];
        let b = n * 6;
        s[b] = (0.0, 1.0, if d.0 { 1.0 } else { 0.0 });
        s[b + 1] = (0.0, 5.0, d.1 as f32);
        s[b + 2] = (20.0, 20000.0, d.2);
        s[b + 3] = (-24.0, 24.0, d.3);
        s[b + 4] = (0.1, 18.0, d.4);
        s[b + 5] = (0.0, 2.0, 0.0);
        n += 1;
    }
    s[48] = (-24.0, 24.0, 0.0);
    s
}

/// `(min, max, default)` per parameter index (band n uses n*6+k, 48 = output).
pub const SPEC: [(f32, f32, f32); 49] = eq_spec();

/// Stable parameter ids in `SPEC` order.
pub const PARAM_IDS: [&str; 49] = [
    "band_1_on",
    "band_1_type",
    "band_1_frequency",
    "band_1_gain",
    "band_1_q",
    "band_1_channel",
    "band_2_on",
    "band_2_type",
    "band_2_frequency",
    "band_2_gain",
    "band_2_q",
    "band_2_channel",
    "band_3_on",
    "band_3_type",
    "band_3_frequency",
    "band_3_gain",
    "band_3_q",
    "band_3_channel",
    "band_4_on",
    "band_4_type",
    "band_4_frequency",
    "band_4_gain",
    "band_4_q",
    "band_4_channel",
    "band_5_on",
    "band_5_type",
    "band_5_frequency",
    "band_5_gain",
    "band_5_q",
    "band_5_channel",
    "band_6_on",
    "band_6_type",
    "band_6_frequency",
    "band_6_gain",
    "band_6_q",
    "band_6_channel",
    "band_7_on",
    "band_7_type",
    "band_7_frequency",
    "band_7_gain",
    "band_7_q",
    "band_7_channel",
    "band_8_on",
    "band_8_type",
    "band_8_frequency",
    "band_8_gain",
    "band_8_q",
    "band_8_channel",
    "output",
];

pub(crate) fn is_stepped(index: u32) -> bool {
    index < OUTPUT && matches!(index % 6, 0 | 1 | 5)
}

/// Clamps a frequency in Hz to the EQ coefficient range at the device sample rate.
/// Use the rate accepted by [`Eq::new`] after its 8–384 kHz clamp.
/// Pass the smoothed frequency after conversion from log2 Hz to reproduce the active value.
/// A requested frequency estimates the settled value because the device stores it in f32 log2 Hz.
pub fn effective_band_hz(freq_hz: f64, sample_rate: f64) -> f64 {
    freq_hz.clamp(10.0, 0.49 * sample_rate)
}

struct Band {
    on: bool,
    typ: u8,
    ch: u8,
    freq: Smooth, // log2 Hz
    gain: Smooth, // dB
    q: Smooth,    // log2 Q
    wet: Smooth,  // 0..1, per sample
    effective_hz: f32,
    c: [Coefs; 2],
    nsec: usize,
    st: [[Biquad; 2]; 2], // [channel][section]
    dirty: bool,
}

impl Band {
    fn new(sr: f32) -> Self {
        let ctrl_rate = sr / CTRL as f32;
        Self {
            on: false,
            typ: 2,
            ch: 0,
            freq: Smooth::new(10.0, 12.0, ctrl_rate),
            gain: Smooth::new(0.0, 12.0, ctrl_rate),
            q: Smooth::new(0.0, 12.0, ctrl_rate),
            wet: Smooth::new(0.0, 8.0, sr),
            effective_hz: 0.0,
            c: [Coefs::IDENTITY; 2],
            nsec: 1,
            st: [[Biquad::default(); 2]; 2],
            dirty: true,
        }
    }

    fn snap(&mut self) {
        self.freq.snap();
        self.gain.snap();
        self.q.snap();
        self.wet.snap();
    }

    fn compute(&mut self, sr: f64) {
        let f = effective_band_hz(libm::exp2f(self.freq.cur) as f64, sr);
        self.effective_hz = f as f32;
        let g = self.gain.cur as f64;
        let q = (libm::exp2f(self.q.cur) as f64).clamp(0.1, 18.0);
        let qs = q / std::f64::consts::FRAC_1_SQRT_2;
        match self.typ {
            0 => {
                self.nsec = 2;
                self.c = [
                    Coefs::highpass(f, 0.5412 * qs, sr),
                    Coefs::highpass(f, 1.3066 * qs, sr),
                ];
            }
            1 => {
                self.nsec = 1;
                self.c[0] = Coefs::low_shelf(f, q, g, sr);
            }
            2 => {
                self.nsec = 1;
                self.c[0] = Coefs::peak(f, q, g, sr);
            }
            3 => {
                self.nsec = 1;
                self.c[0] = Coefs::notch(f, q, sr);
            }
            4 => {
                self.nsec = 1;
                self.c[0] = Coefs::high_shelf(f, q, g, sr);
            }
            _ => {
                self.nsec = 2;
                self.c = [
                    Coefs::lowpass(f, 0.5412 * qs, sr),
                    Coefs::lowpass(f, 1.3066 * qs, sr),
                ];
            }
        }
    }

    #[inline(always)]
    fn filt(c: &[Coefs; 2], st: &mut [Biquad; 2], nsec: usize, x: f64) -> f64 {
        let y = st[0].run(&c[0], x);
        if nsec == 2 { st[1].run(&c[1], y) } else { y }
    }

    #[inline(always)]
    fn process(&mut self, l: &mut f64, r: &mut f64) {
        let w = self.wet.next() as f64;
        if w == 0.0 {
            return;
        }
        let [s0, s1] = &mut self.st;
        match self.ch {
            0 => {
                let yl = Self::filt(&self.c, s0, self.nsec, *l);
                let yr = Self::filt(&self.c, s1, self.nsec, *r);
                *l += w * (yl - *l);
                *r += w * (yr - *r);
            }
            1 => {
                let m = 0.5 * (*l + *r);
                let s = 0.5 * (*l - *r);
                let ym = Self::filt(&self.c, s0, self.nsec, m);
                let m2 = m + w * (ym - m);
                *l = m2 + s;
                *r = m2 - s;
            }
            _ => {
                let m = 0.5 * (*l + *r);
                let s = 0.5 * (*l - *r);
                let ys = Self::filt(&self.c, s0, self.nsec, s);
                let s2 = s + w * (ys - s);
                *l = m + s2;
                *r = m - s2;
            }
        }
    }

    fn reset_state(&mut self) {
        for ch in self.st.iter_mut() {
            for b in ch.iter_mut() {
                b.reset();
            }
        }
    }
}

pub struct Eq {
    sr: f32,
    bands: [Band; BANDS],
    out: Smooth,
    ctr: u32,
    fresh: bool,
    fft: Fft,
    window: Vec<f64>,
    ring_in: Vec<f32>,
    ring_out: Vec<f32>,
    ring_pos: usize,
    hop: usize,
    hop_ctr: usize,
    re: Vec<f64>,
    im: Vec<f64>,
    pw_in: Vec<f64>,
    pw_out: Vec<f64>,
    /// Per display bin: (centre in FFT bins, first FFT bin, last FFT bin, above Nyquist).
    bin_map: Vec<(f64, usize, usize, bool)>,
    spec_in: [f32; BINS],
    spec_out: [f32; BINS],
}

pub(crate) fn memory_payload() -> crate::insert_memory::Payload {
    let mut payload = crate::insert_memory::Payload::boxed::<Eq>();
    Fft::add_payload(FFT_N, &mut payload);
    payload.array::<f32>(FFT_N, 2); // Input and output rings.
    payload.array::<f64>(FFT_N, 3); // Window, real and imaginary FFT components.
    payload.array::<f64>(FFT_N / 2 + 1, 2); // Power spectra.
    payload.array::<(f64, usize, usize, bool)>(BINS, 1);
    payload
}

impl Eq {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let window = (0..FFT_N)
            .map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / FFT_N as f64))
            .collect();
        let bin_map = (0..BINS)
            .map(|j| {
                let f = |x: f64| 20.0 * libm::pow(1000.0, x / (BINS - 1) as f64);
                let to_bin = |hz: f64| hz / sr as f64 * FFT_N as f64;
                let kc = to_bin(f(j as f64));
                let lo = to_bin(f(j as f64 - 0.5)).ceil() as usize;
                let hi = (to_bin(f(j as f64 + 0.5)).floor() as usize).min(FFT_N / 2);
                (kc, lo, hi, f(j as f64) >= 0.5 * sr as f64)
            })
            .collect();
        let mut eq = Self {
            sr,
            bands: core::array::from_fn(|_| Band::new(sr)),
            out: Smooth::new(1.0, 10.0, sr),
            ctr: 0,
            fresh: true,
            fft: Fft::new(FFT_N),
            window,
            ring_in: vec![0.0; FFT_N],
            ring_out: vec![0.0; FFT_N],
            ring_pos: 0,
            hop: ((sr / 30.0).round() as usize).max(1),
            hop_ctr: 0,
            re: vec![0.0; FFT_N],
            im: vec![0.0; FFT_N],
            pw_in: vec![0.0; FFT_N / 2 + 1],
            pw_out: vec![0.0; FFT_N / 2 + 1],
            bin_map,
            spec_in: [-120.0; BINS],
            spec_out: [-120.0; BINS],
        };
        for (n, d) in BAND_DEFAULTS.iter().enumerate() {
            let base = (n * 6) as u32;
            eq.set_param(base, if d.0 { 1.0 } else { 0.0 });
            eq.set_param(base + 1, d.1 as f32);
            eq.set_param(base + 2, d.2);
            eq.set_param(base + 3, d.3);
            eq.set_param(base + 4, d.4);
            eq.set_param(base + 5, 0.0);
        }
        eq.set_param(OUTPUT, 0.0);
        eq.snap_all();
        eq
    }

    fn snap_all(&mut self) {
        let sr = self.sr as f64;
        for b in self.bands.iter_mut() {
            b.snap();
            b.compute(sr);
            b.dirty = false;
        }
        self.out.snap();
    }

    fn control(&mut self) {
        let sr = self.sr as f64;
        for b in self.bands.iter_mut() {
            if b.dirty || !b.freq.settled() || !b.gain.settled() || !b.q.settled() {
                b.freq.next();
                b.gain.next();
                b.q.next();
                b.compute(sr);
                b.dirty = false;
            }
        }
    }

    fn analyze(&mut self) {
        for j in 0..FFT_N {
            let idx = (self.ring_pos + j) & (FFT_N - 1);
            self.re[j] = self.ring_in[idx] as f64 * self.window[j];
            self.im[j] = self.ring_out[idx] as f64 * self.window[j];
        }
        self.fft.run(&mut self.re, &mut self.im, false);
        // separate the two real spectra: A = (X_k + conj X_{N-k}) / 2, B = (X_k - conj X_{N-k}) / 2i
        let norm = {
            let s = 4.0 / FFT_N as f64;
            s * s
        };
        for k in 0..=FFT_N / 2 {
            let m = (FFT_N - k) & (FFT_N - 1);
            let (xr, xi, yr, yi) = (self.re[k], self.im[k], self.re[m], self.im[m]);
            let (ar, ai) = (0.5 * (xr + yr), 0.5 * (xi - yi));
            let (br, bi) = (0.5 * (xi + yi), -0.5 * (xr - yr));
            self.pw_in[k] = (ar * ar + ai * ai) * norm;
            self.pw_out[k] = (br * br + bi * bi) * norm;
        }
        for (j, &(kc, lo, hi, above)) in self.bin_map.iter().enumerate() {
            let read = |pw: &[f64]| -> f32 {
                if above {
                    return -120.0;
                }
                let p = if hi >= lo {
                    pw[lo..=hi].iter().fold(0.0f64, |a, &b| a.max(b))
                } else {
                    let k0 = (kc.floor() as usize).min(FFT_N / 2 - 1);
                    let t = kc - k0 as f64;
                    pw[k0] * (1.0 - t) + pw[k0 + 1] * t
                };
                (10.0 * libm::log10(p + 1.0e-12)).max(-120.0) as f32
            };
            let vi = read(&self.pw_in);
            let vo = read(&self.pw_out);
            let s = &mut self.spec_in[j];
            *s = if vi > *s {
                0.5 * (*s + vi)
            } else {
                vi.max(*s - 1.5)
            };
            let s = &mut self.spec_out[j];
            *s = if vo > *s {
                0.5 * (*s + vo)
            } else {
                vo.max(*s - 1.5)
            };
        }
    }
}

impl Device for Eq {
    fn set_param(&mut self, index: u32, value: f32) {
        if !value.is_finite() {
            return;
        }
        if index == OUTPUT {
            self.out.set(db_to_gain(value.clamp(-24.0, 24.0)));
        } else if index < OUTPUT {
            let b = &mut self.bands[(index / 6) as usize];
            match index % 6 {
                0 => {
                    let on = value >= 0.5;
                    if on && !b.on && b.wet.cur == 0.0 {
                        b.reset_state();
                    }
                    b.on = on;
                    b.wet.set(if on { 1.0 } else { 0.0 });
                }
                1 => {
                    let t = value.round().clamp(0.0, 5.0) as u8;
                    if t != b.typ {
                        b.typ = t;
                        b.dirty = true;
                    }
                }
                2 => b.freq.set(libm::log2f(value.clamp(20.0, 20_000.0))),
                3 => b.gain.set(value.clamp(-24.0, 24.0)),
                4 => b.q.set(libm::log2f(value.clamp(0.1, 18.0))),
                _ => {
                    let c = value.round().clamp(0.0, 2.0) as u8;
                    if c != b.ch {
                        b.ch = c;
                        b.reset_state();
                    }
                }
            }
        } else {
            return;
        }
        if self.fresh {
            self.snap_all();
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sc: Option<(&[f32], &[f32])>,
        _ev: &[crate::api::NoteEvent],
    ) {
        let n = l.len().min(r.len());
        self.fresh = false;
        for i in 0..n {
            if self.ctr == 0 {
                self.control();
            }
            self.ctr = (self.ctr + 1) % CTRL;
            let il = sane(l[i]);
            let ir = sane(r[i]);
            let mut xl = il as f64;
            let mut xr = ir as f64;
            for b in self.bands.iter_mut() {
                b.process(&mut xl, &mut xr);
            }
            let g = self.out.next() as f64;
            let ol = (xl * g) as f32;
            let or = (xr * g) as f32;
            self.ring_in[self.ring_pos] = mid(il, ir);
            self.ring_out[self.ring_pos] = mid(ol, or);
            self.ring_pos = (self.ring_pos + 1) & (FFT_N - 1);
            self.hop_ctr += 1;
            if self.hop_ctr >= self.hop {
                self.hop_ctr = 0;
                self.analyze();
            }
            l[i] = ol;
            r[i] = or;
        }
    }

    fn reset(&mut self) {
        for b in self.bands.iter_mut() {
            b.reset_state();
        }
        self.ring_in.fill(0.0);
        self.ring_out.fill(0.0);
        self.spec_in = [-120.0; BINS];
        self.spec_out = [-120.0; BINS];
        self.ctr = 0;
        self.hop_ctr = 0;
        self.snap_all();
        self.fresh = true;
    }

    /// Scalar meters 2–9 report the current effective frequencies in Hz for bands 1–8.
    /// Array meters 0/1 are the pre/post EQ spectra.
    fn meter(&self, index: u32) -> f32 {
        match index {
            2..=9 => self.bands[(index - 2) as usize].effective_hz,
            _ => 0.0,
        }
    }

    fn meter_array(&self, index: u32, out: &mut [f32]) -> usize {
        let src = match index {
            0 => &self.spec_in,
            1 => &self.spec_out,
            _ => return 0,
        };
        let n = out.len().min(BINS);
        out[..n].copy_from_slice(&src[..n]);
        n
    }
}
