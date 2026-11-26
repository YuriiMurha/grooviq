//! Algorithmic reverb: pre-delay -> 8-channel diffuser -> 8-line feedback delay network.
//!
//! Diffuser: 4 steps, each a set of 8 short delays (spread across the step's range), an
//! orthonormal Hadamard mix and fixed polarity flips; it turns each input into a dense cloud of
//! echoes within ~35 ms so the tail starts smooth. FDN: 8 modulated delay lines (Hermite reads,
//! slow sine LFOs of ~1-2 cents), Householder feedback, per-line gain set from the RT60:
//! g = 10^(-3 * length / (RT60 * Fs)), so the decay time is what the knob says. Damping is a
//! Jot absorption filter per line: DC decays at RT60, Nyquist at RT60 * (1 - 0.92 * damping).
//! Output: two orthogonal taps (decorrelated L/R), width (M/S), 12 dB/oct low/high cut,
//! equal-power dry/wet.
//!
//! Scalar meter 3 reports the active wet-path high cut in Hz, including log2 smoothing and the
//! sample-rate cap from [`crate::effective_high_cut`].

use crate::api::{Device, NoteEvent};
use crate::util::*;

/// `(min, max, default)` per parameter, `SPEC` index order.
pub const SPEC: [(f32, f32, f32); 8] = [
    (0.0, 1.0, 0.5),           // 0 size
    (0.1, 20.0, 2.2),          // 1 decay s
    (0.0, 250.0, 20.0),        // 2 pre-delay ms
    (0.0, 1.0, 0.5),           // 3 damping
    (0.0, 1.0, 1.0),           // 4 width
    (20.0, 1000.0, 150.0),     // 5 low cut Hz
    (1000.0, 20000.0, 9000.0), // 6 high cut Hz
    (0.0, 1.0, 0.25),          // 7 mix
];

/// Stable parameter ids in `SPEC` order.
pub const PARAM_IDS: [&str; 8] = [
    "size",
    "decay",
    "pre_delay",
    "damping",
    "width",
    "low_cut",
    "high_cut",
    "mix",
];

const LINES: usize = 8;
const CTRL: u32 = 16;
/// FDN line lengths relative to the mean (spread ~0.7..1.3, mutually incommensurate).
const RATIOS: [f32; LINES] = [0.711, 0.773, 0.857, 0.919, 1.013, 1.103, 1.187, 1.297];
/// Diffuser step ranges in ms.
const DIFF_MS: [f32; 4] = [17.0, 9.0, 5.0, 2.7];
const MOD_RATES: [f64; LINES] = [0.13, 0.19, 0.23, 0.29, 0.37, 0.43, 0.53, 0.61];
const MOD_DEPTH_MS: f32 = 0.25;
/// Orthogonal +-1 output patterns (Hadamard rows 1 and 2).
const TAP_L: [f32; LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const TAP_R: [f32; LINES] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];

pub struct Reverb {
    sr: f32,
    pre: [DelayLine; 2],
    pre_d: Smooth,
    diff: Vec<DelayLine>,
    diff_d: [[usize; LINES]; 4],
    diff_flip: [[f32; LINES]; 4],
    fdn: Vec<DelayLine>,
    len: [Smooth; LINES],
    fb: [f32; LINES],
    damp_a: [f32; LINES],
    damp_s: [f32; LINES],
    mod_ph: [f64; LINES],
    mod_inc: [f64; LINES],
    mod_depth: f32,
    size: f32,
    decay: f32,
    damping: f32,
    lo: Smooth, // log2 Hz
    hi: Smooth,
    c_lo: Coefs,
    c_hi: Coefs,
    high_cut_hz: f32,
    st_lo: [Biquad; 2],
    st_hi: [Biquad; 2],
    width: Smooth,
    dry_g: Smooth,
    wet_g: Smooth,
    norm: f32,
    ctr: u32,
    fresh: bool,
    in_m: PeakMeter,
    out_m: PeakMeter,
    tail_m: PeakMeter,
}

fn mean_len_ms(size: f32) -> f32 {
    20.0 + 110.0 * size
}

fn ms_frames(sr: f32, ms: f32) -> f32 {
    ms * 0.001 * sr
}

fn diffusers(sr: f32) -> ([[usize; LINES]; 4], [[f32; LINES]; 4]) {
    let mut rng = Rng(0x5EED_1234);
    let mut diff_d = [[1usize; LINES]; 4];
    let mut diff_flip = [[1.0f32; LINES]; 4];
    for s in 0..4 {
        for i in 0..LINES {
            let lo = DIFF_MS[s] * i as f32 / LINES as f32;
            let hi = DIFF_MS[s] * (i + 1) as f32 / LINES as f32;
            diff_d[s][i] = (ms_frames(sr, lo + (hi - lo) * rng.unit()).round() as usize).max(1);
            diff_flip[s][i] = if rng.next_u32() & 1 == 0 { 1.0 } else { -1.0 };
        }
    }
    (diff_d, diff_flip)
}

fn max_line_delay(sr: f32) -> usize {
    ms_frames(
        sr,
        mean_len_ms(1.0) * RATIOS[LINES - 1] + MOD_DEPTH_MS * 2.0,
    ) as usize
        + 8
}

fn pre_delay(sr: f32) -> usize {
    ms_frames(sr, 260.0) as usize
}

pub(crate) fn memory_payload(sr: f32) -> crate::insert_memory::Payload {
    let mut payload = crate::insert_memory::Payload::boxed::<Reverb>();
    payload.delay_lines(pre_delay(sr), 2);
    payload.array::<DelayLine>(DIFF_MS.len() * LINES, 1);
    let (diff_d, _) = diffusers(sr);
    for delay in diff_d.into_iter().flatten() {
        payload.delay_lines(delay + 1, 1);
    }
    payload.array::<DelayLine>(LINES, 1);
    payload.delay_lines(max_line_delay(sr), LINES);
    payload
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let (diff_d, diff_flip) = diffusers(sr);
        let max_line = max_line_delay(sr);
        let sm = |v: f32, t: f32| Smooth::new(v, t, sr);
        let ctrl_rate = sr / CTRL as f32;
        let mut r = Self {
            sr,
            pre: [DelayLine::new(pre_delay(sr)), DelayLine::new(pre_delay(sr))],
            pre_d: sm(0.0, 60.0),
            diff: (0..4 * LINES)
                .map(|k| DelayLine::new(diff_d[k / LINES][k % LINES] + 1))
                .collect(),
            diff_d,
            diff_flip,
            fdn: (0..LINES).map(|_| DelayLine::new(max_line)).collect(),
            len: core::array::from_fn(|_| sm(0.0, 250.0)),
            fb: [0.0; LINES],
            damp_a: [0.0; LINES],
            damp_s: [0.0; LINES],
            mod_ph: core::array::from_fn(|i| i as f64 / LINES as f64),
            mod_inc: core::array::from_fn(|i| MOD_RATES[i] / sr as f64),
            mod_depth: ms_frames(sr, MOD_DEPTH_MS),
            size: 0.5,
            decay: 2.2,
            damping: 0.5,
            lo: Smooth::new(7.0, 15.0, ctrl_rate),
            hi: Smooth::new(13.0, 15.0, ctrl_rate),
            c_lo: Coefs::IDENTITY,
            c_hi: Coefs::IDENTITY,
            high_cut_hz: 0.0,
            st_lo: [Biquad::default(); 2],
            st_hi: [Biquad::default(); 2],
            width: sm(1.0, 15.0),
            dry_g: sm(1.0, 15.0),
            wet_g: sm(0.0, 15.0),
            norm: 1.0,
            ctr: 0,
            fresh: true,
            in_m: PeakMeter::default(),
            out_m: PeakMeter::default(),
            tail_m: PeakMeter::default(),
        };
        for (i, s) in SPEC.iter().enumerate() {
            r.set_param(i as u32, s.2);
        }
        r
    }

    fn set_lengths(&mut self) {
        let mean = mean_len_ms(self.size) * 0.001 * self.sr;
        for i in 0..LINES {
            self.len[i].set(mean * RATIOS[i]);
        }
    }

    /// Per-line feedback gain and damping from the current (smoothed) lengths.
    fn update_gains(&mut self) {
        let ratio = (1.0 - 0.92 * self.damping).max(0.02);
        let mut gsum = 0.0;
        for i in 0..LINES {
            let l = self.len[i].cur + 1.0;
            let g = libm::powf(10.0, -3.0 * l / (self.decay * self.sr));
            self.fb[i] = g;
            // Jot: Nyquist gain g^(1/ratio); one-pole with DC gain 1, Nyquist gain c
            let c = libm::powf(g, 1.0 / ratio - 1.0);
            self.damp_a[i] = (1.0 - c) / (1.0 + c);
            gsum += g * g;
        }
        // partial loudness normalisation so long decays do not build up excessively
        self.norm = libm::powf((1.0 - gsum / LINES as f32).max(0.0), 0.25).max(0.2);
    }

    fn update_filters(&mut self) {
        let sr = self.sr as f64;
        let q = std::f64::consts::FRAC_1_SQRT_2;
        self.c_lo = Coefs::highpass(libm::exp2f(self.lo.next()) as f64, q, sr);
        let high_cut_hz = effective_high_cut(libm::exp2f(self.hi.next()) as f64, sr);
        self.c_hi = Coefs::lowpass(high_cut_hz, q, sr);
        self.high_cut_hz = high_cut_hz as f32;
    }

    fn snap_all(&mut self) {
        self.pre_d.snap();
        for l in self.len.iter_mut() {
            l.snap();
        }
        self.lo.snap();
        self.hi.snap();
        self.width.snap();
        self.dry_g.snap();
        self.wet_g.snap();
        self.update_gains();
        self.update_filters();
    }
}

impl Device for Reverb {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => {
                self.size = v;
                self.set_lengths();
            }
            1 => self.decay = v,
            2 => self.pre_d.set(v * 0.001 * self.sr),
            3 => self.damping = v,
            4 => self.width.set(v),
            5 => self.lo.set(libm::log2f(v)),
            6 => self.hi.set(libm::log2f(v)),
            _ => {
                let a = v * std::f32::consts::FRAC_PI_2;
                self.dry_g.set(libm::cosf(a));
                self.wet_g.set(libm::sinf(a));
            }
        }
        if self.fresh {
            self.snap_all();
        } else if index == 1 || index == 3 {
            self.update_gains();
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sc: Option<(&[f32], &[f32])>,
        _ev: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        self.fresh = false;
        let mut pin = 0.0f32;
        let mut ptail = 0.0f32;
        for i in 0..n {
            if self.ctr == 0 {
                if self.len.iter().any(|s| !s.settled()) {
                    self.update_gains();
                }
                if !self.lo.settled() || !self.hi.settled() {
                    self.update_filters();
                }
            }
            self.ctr = (self.ctr + 1) % CTRL;

            let xl = sane(l[i]);
            let xr = sane(r[i]);
            pin = pin.max(xl.abs()).max(xr.abs());

            // pre-delay
            self.pre[0].push(xl);
            self.pre[1].push(xr);
            let pd = self.pre_d.next();
            let pl = self.pre[0].read_frac(pd);
            let pr = self.pre[1].read_frac(pd);

            // diffuser
            let mut ch = [0.0f32; LINES];
            for (k, c) in ch.iter_mut().enumerate() {
                *c = if k % 2 == 0 { pl } else { pr } * 0.5;
            }
            for s in 0..4 {
                for k in 0..LINES {
                    let dl = &mut self.diff[s * LINES + k];
                    dl.push(ch[k]);
                    ch[k] = dl.read_int(self.diff_d[s][k]);
                }
                hadamard8(&mut ch);
                for k in 0..LINES {
                    ch[k] *= self.diff_flip[s][k];
                }
            }

            // FDN
            let mut o = [0.0f32; LINES];
            for k in 0..LINES {
                let ph = self.mod_ph[k] + self.mod_inc[k];
                self.mod_ph[k] = if ph >= 1.0 { ph - 1.0 } else { ph };
                let m = libm::sinf(self.mod_ph[k] as f32 * std::f32::consts::TAU) * self.mod_depth;
                o[k] = self.fdn[k].read_frac(self.len[k].next() + m);
            }
            let mut y = [0.0f32; LINES];
            let mut sum = 0.0;
            for k in 0..LINES {
                let a = self.damp_a[k];
                self.damp_s[k] = flush((1.0 - a) * o[k] + a * self.damp_s[k]);
                y[k] = self.damp_s[k] * self.fb[k];
                sum += y[k];
            }
            let hh = sum * (2.0 / LINES as f32);
            for k in 0..LINES {
                self.fdn[k].push(flush(y[k] - hh + ch[k]));
            }

            let mut wl = 0.0;
            let mut wr = 0.0;
            for k in 0..LINES {
                wl += o[k] * TAP_L[k];
                wr += o[k] * TAP_R[k];
            }
            let s = 0.353_553_39 * self.norm;
            wl *= s;
            wr *= s;
            let w = self.width.next();
            let m = 0.5 * (wl + wr);
            let sd = 0.5 * (wl - wr) * w;
            wl = m + sd;
            wr = m - sd;
            wl = self.st_hi[0].run(&self.c_hi, self.st_lo[0].run(&self.c_lo, wl as f64)) as f32;
            wr = self.st_hi[1].run(&self.c_hi, self.st_lo[1].run(&self.c_lo, wr as f64)) as f32;
            ptail = ptail.max(wl.abs()).max(wr.abs());
            let dg = self.dry_g.next();
            let wg = self.wet_g.next();
            l[i] = xl * dg + wl * wg;
            r[i] = xr * dg + wr * wg;
        }
        self.in_m.update(pin, n, self.sr);
        self.out_m
            .update(peak_of(&l[..n]).max(peak_of(&r[..n])), n, self.sr);
        self.tail_m.update(ptail, n, self.sr);
    }

    fn reset(&mut self) {
        for d in self.pre.iter_mut() {
            d.clear();
        }
        for d in self.diff.iter_mut() {
            d.clear();
        }
        for d in self.fdn.iter_mut() {
            d.clear();
        }
        self.damp_s = [0.0; LINES];
        self.st_lo = [Biquad::default(); 2];
        self.st_hi = [Biquad::default(); 2];
        self.mod_ph = core::array::from_fn(|i| i as f64 / LINES as f64);
        self.ctr = 0;
        self.snap_all();
        self.in_m = PeakMeter::default();
        self.out_m = PeakMeter::default();
        self.tail_m = PeakMeter::default();
        self.fresh = true;
    }

    /// 0–2: input, output, and wet tail peaks in dB.
    /// 3: active wet-path high cut in Hz.
    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.in_m.db(),
            1 => self.out_m.db(),
            2 => self.tail_m.db(),
            3 => self.high_cut_hz,
            _ => 0.0,
        }
    }
}
