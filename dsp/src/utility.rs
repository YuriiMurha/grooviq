//! Utility: gain, balance, M/S width, bass mono, polarity, level and correlation meters.
//!
//! Bass mono removes the side signal below the frequency with a Linkwitz-Riley 24 dB/oct
//! high-pass and runs the mid through the matching allpass, so the two stay phase-aligned (the
//! low end collapses to mono, the rest of the image is untouched). Everything is smoothed,
//! including the polarity flip and switching bass mono on/off.

use crate::api::{Device, NoteEvent, db_to_gain};
use crate::util::*;

/// `(min, max, default)` per parameter, `SPEC` index order.
pub const SPEC: [(f32, f32, f32); 5] = [
    (-36.0, 12.0, 0.0), // 0 gain dB
    (-1.0, 1.0, 0.0),   // 1 pan
    (0.0, 2.0, 1.0),    // 2 width
    (0.0, 300.0, 0.0),  // 3 mono below Hz (0 = off)
    (0.0, 1.0, 0.0),    // 4 phase invert
];

/// Stable parameter ids in `SPEC` order.
pub const PARAM_IDS: [&str; 5] = ["gain", "pan", "width", "mono_below", "phase_invert"];

const CTRL: u32 = 16;

pub struct Utility {
    sr: f32,
    gain: Smooth,
    gl: Smooth,
    gr: Smooth,
    width: Smooth,
    polarity: Smooth,
    mono_on: Smooth,
    mono_f: Smooth, // log2 Hz
    c_hp: Coefs,
    c_ap: Coefs,
    st_s: [Biquad; 2],
    st_m: Biquad,
    ctr: u32,
    fresh: bool,
    l_m: PeakMeter,
    r_m: PeakMeter,
    c_lr: f32,
    c_ll: f32,
    c_rr: f32,
    corr_coef: f32,
}

impl Utility {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let sm = |v: f32| Smooth::new(v, 10.0, sr);
        let mut u = Self {
            sr,
            gain: sm(1.0),
            gl: sm(1.0),
            gr: sm(1.0),
            width: sm(1.0),
            polarity: sm(1.0),
            mono_on: sm(0.0),
            mono_f: Smooth::new(6.0, 15.0, sr / CTRL as f32),
            c_hp: Coefs::IDENTITY,
            c_ap: Coefs::IDENTITY,
            st_s: [Biquad::default(); 2],
            st_m: Biquad::default(),
            ctr: 0,
            fresh: true,
            l_m: PeakMeter::default(),
            r_m: PeakMeter::default(),
            c_lr: 0.0,
            c_ll: 0.0,
            c_rr: 0.0,
            corr_coef: 1.0 - libm::expf(-1.0 / (0.15 * sr)),
        };
        for (i, s) in SPEC.iter().enumerate() {
            u.set_param(i as u32, s.2);
        }
        u
    }

    fn update_filters(&mut self) {
        let f = (libm::exp2f(self.mono_f.next()) as f64).min(0.45 * self.sr as f64);
        let q = std::f64::consts::FRAC_1_SQRT_2;
        self.c_hp = Coefs::highpass(f, q, self.sr as f64);
        self.c_ap = Coefs::allpass(f, q, self.sr as f64);
    }

    fn snap_all(&mut self) {
        for s in [
            &mut self.gain,
            &mut self.gl,
            &mut self.gr,
            &mut self.width,
            &mut self.polarity,
            &mut self.mono_on,
            &mut self.mono_f,
        ] {
            s.snap();
        }
        self.update_filters();
    }
}

impl Device for Utility {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.gain.set(db_to_gain(v)),
            1 => {
                // balance: 0 dB at centre, sine/cosine taper on the side being turned down
                let a = (v + 1.0) * std::f32::consts::FRAC_PI_4;
                self.gl
                    .set((std::f32::consts::SQRT_2 * libm::cosf(a)).min(1.0));
                self.gr
                    .set((std::f32::consts::SQRT_2 * libm::sinf(a)).min(1.0));
            }
            2 => self.width.set(v),
            3 => {
                if v >= 1.0 {
                    self.mono_on.set(1.0);
                    self.mono_f.set(libm::log2f(v.max(10.0)));
                } else {
                    self.mono_on.set(0.0);
                }
            }
            _ => self.polarity.set(if v >= 0.5 { -1.0 } else { 1.0 }),
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
        _ev: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        self.fresh = false;
        for i in 0..n {
            if self.ctr == 0 && !self.mono_f.settled() {
                self.update_filters();
            }
            self.ctr = (self.ctr + 1) % CTRL;
            let xl = sane(l[i]);
            let xr = sane(r[i]);
            let mut m = 0.5 * (xl + xr);
            let mut s = 0.5 * (xl - xr);
            let on = self.mono_on.next();
            if on > 0.0 {
                let hs = run2(&mut self.st_s, &self.c_hp, s as f64) as f32;
                let am = self.st_m.run(&self.c_ap, m as f64) as f32;
                s += on * (hs - s);
                m += on * (am - m);
            }
            s *= self.width.next();
            let g = self.gain.next() * self.polarity.next();
            let yl = (m + s) * self.gl.next() * g;
            let yr = (m - s) * self.gr.next() * g;
            let a = self.corr_coef;
            self.c_lr = flush(self.c_lr + (yl * yr - self.c_lr) * a);
            self.c_ll = flush(self.c_ll + (yl * yl - self.c_ll) * a);
            self.c_rr = flush(self.c_rr + (yr * yr - self.c_rr) * a);
            l[i] = yl;
            r[i] = yr;
        }
        self.l_m.update(peak_of(&l[..n]), n, self.sr);
        self.r_m.update(peak_of(&r[..n]), n, self.sr);
    }

    fn reset(&mut self) {
        self.st_s = [Biquad::default(); 2];
        self.st_m = Biquad::default();
        self.c_lr = 0.0;
        self.c_ll = 0.0;
        self.c_rr = 0.0;
        self.ctr = 0;
        self.snap_all();
        self.l_m = PeakMeter::default();
        self.r_m = PeakMeter::default();
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.l_m.db(),
            1 => self.r_m.db(),
            2 => {
                let d = (self.c_ll * self.c_rr).sqrt();
                if d > 1.0e-10 {
                    (self.c_lr / d).clamp(-1.0, 1.0)
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }
}
