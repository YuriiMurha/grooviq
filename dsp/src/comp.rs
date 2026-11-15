//! Feed-forward compressor.
//!
//! Detector: stereo-linked, high-passed (sidechain HPF, also applied to the internal key),
//! level = max(instant peak, sqrt(2 * mean square over ~10 ms)) so a sine reads its peak level
//! whether the peak or the RMS branch wins. Gain computer with a quadratic soft knee
//! (Giannoulis/Massberg/Reiss), then attack/release smoothing of the gain in the dB domain
//! ("smooth branching" detector). Makeup gain and a phase-coherent dry/wet mix.

use crate::api::{Device, NoteEvent};
use crate::util::*;

/// `(min, max, default)` per parameter, `SPEC` index order.
pub const SPEC: [(f32, f32, f32); 9] = [
    (-60.0, 0.0, -18.0),  // 0 threshold dB
    (1.0, 20.0, 4.0),     // 1 ratio
    (0.05, 200.0, 10.0),  // 2 attack ms
    (5.0, 2000.0, 120.0), // 3 release ms
    (0.0, 24.0, 6.0),     // 4 knee dB
    (-12.0, 24.0, 0.0),   // 5 makeup dB
    (0.0, 1.0, 1.0),      // 6 mix
    (0.0, 1.0, 0.0),      // 7 external sidechain
    (20.0, 500.0, 20.0),  // 8 sidechain HPF Hz
];

pub struct Comp {
    sr: f32,
    thr: f32,
    ratio: f32,
    knee: f32,
    att: f32,
    rel: f32,
    makeup: Smooth, // dB
    mix: Smooth,
    ext: bool,
    hpf_c: Coefs,
    hpf: [Biquad; 2],
    ms: f32,
    ms_coef: f32,
    gr: f32,
    fresh: bool,
    in_m: PeakMeter,
    out_m: PeakMeter,
    gr_m: f32,
}

impl Comp {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let mut c = Self {
            sr,
            thr: -18.0,
            ratio: 4.0,
            knee: 6.0,
            att: 0.0,
            rel: 0.0,
            makeup: Smooth::new(0.0, 10.0, sr),
            mix: Smooth::new(1.0, 10.0, sr),
            ext: false,
            hpf_c: Coefs::IDENTITY,
            hpf: [Biquad::default(); 2],
            ms: 0.0,
            ms_coef: 1.0 - (-1.0 / (0.010 * sr)).exp(),
            gr: 0.0,
            fresh: true,
            in_m: PeakMeter::default(),
            out_m: PeakMeter::default(),
            gr_m: 0.0,
        };
        for (i, s) in SPEC.iter().enumerate() {
            c.set_param(i as u32, s.2);
        }
        c
    }

    fn coef(ms: f32, sr: f32) -> f32 {
        (-1.0 / (ms * 0.001 * sr).max(1.0e-3)).exp()
    }

    /// Static curve: gain change in dB (<= 0) for a detector level in dB.
    #[inline(always)]
    fn curve(&self, x: f32) -> f32 {
        let slope = 1.0 / self.ratio - 1.0;
        let d = x - self.thr;
        let w = self.knee;
        if 2.0 * d < -w {
            0.0
        } else if w > 0.0 && 2.0 * d.abs() <= w {
            let t = d + 0.5 * w;
            slope * t * t / (2.0 * w)
        } else {
            slope * d
        }
    }
}

impl Device for Comp {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.thr = v,
            1 => self.ratio = v,
            2 => self.att = Self::coef(v, self.sr),
            3 => self.rel = Self::coef(v, self.sr),
            4 => self.knee = v,
            5 => self.makeup.set(v),
            6 => self.mix.set(v),
            7 => self.ext = v >= 0.5,
            _ => {
                self.hpf_c =
                    Coefs::highpass(v as f64, std::f64::consts::FRAC_1_SQRT_2, self.sr as f64)
            }
        }
        if self.fresh {
            self.makeup.snap();
            self.mix.snap();
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        sidechain: Option<(&[f32], &[f32])>,
        _ev: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        self.fresh = false;
        let mut pin = 0.0f32;
        let mut gr_min = 0.0f32;
        let sc = if self.ext { sidechain } else { None };
        for i in 0..n {
            let xl = sane(l[i]);
            let xr = sane(r[i]);
            pin = pin.max(xl.abs()).max(xr.abs());
            let (dl, dr) = match sc {
                Some((a, b)) => (
                    sane(a.get(i).copied().unwrap_or(0.0)),
                    sane(b.get(i).copied().unwrap_or(0.0)),
                ),
                None => (xl, xr),
            };
            let hl = self.hpf[0].run(&self.hpf_c, dl as f64) as f32;
            let hr = self.hpf[1].run(&self.hpf_c, dr as f64) as f32;
            let pk = hl.abs().max(hr.abs());
            self.ms = flush(self.ms + (pk * pk - self.ms) * self.ms_coef);
            let lvl = pk.max((2.0 * self.ms).sqrt());
            let lvl_db = if lvl > 1.0e-9 {
                20.0 * lvl.log10()
            } else {
                -180.0
            };
            let target = self.curve(lvl_db);
            let c = if target < self.gr { self.att } else { self.rel };
            self.gr = target + (self.gr - target) * c;
            if self.gr.abs() < 1.0e-6 {
                self.gr = 0.0;
            }
            gr_min = gr_min.min(self.gr);
            let g = 10f32.powf((self.gr + self.makeup.next()) * 0.05);
            let mix = self.mix.next();
            let k = 1.0 - mix + mix * g;
            l[i] = xl * k;
            r[i] = xr * k;
        }
        self.in_m.update(pin, n, self.sr);
        self.out_m
            .update(peak_of(&l[..n]).max(peak_of(&r[..n])), n, self.sr);
        self.gr_m = gr_min;
    }

    fn reset(&mut self) {
        self.hpf = [Biquad::default(); 2];
        self.ms = 0.0;
        self.gr = 0.0;
        self.gr_m = 0.0;
        self.in_m = PeakMeter::default();
        self.out_m = PeakMeter::default();
        self.makeup.snap();
        self.mix.snap();
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.in_m.db(),
            1 => self.out_m.db(),
            2 => self.gr_m,
            _ => 0.0,
        }
    }
}
