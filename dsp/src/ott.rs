//! "Squash": OTT-style 3-band upward + downward compressor.
//!
//! Bands are split with Linkwitz-Riley 24 dB/oct crossovers (cascaded Butterworth biquads); the
//! low band also runs through the high crossover's allpass so the three bands sum back flat.
//! Each band has a stereo-linked peak/RMS detector and a two-sided static curve: above the
//! downward threshold it compresses hard (66.7:1, 6 dB knee), below the upward threshold it
//! lifts quiet detail (1:4.17, capped at +24 dB and faded out below -90 dBFS so the noise floor
//! and silence are not boosted). Thresholds and ratios follow the classic OTT preset. Gain is
//! smoothed in dB with attack/release scaled by Time. Depth is the wet amount of the gain change
//! (per band, in amplitude, so it is phase coherent), then band gains and output.

use crate::api::{Device, NoteEvent, db_to_gain, gain_to_db};
use crate::util::*;

/// `(min, max, default)` per parameter, devices.json order.
pub const SPEC: [(f32, f32, f32); 10] = [
    (0.0, 1.0, 0.5),           // 0 depth
    (0.0, 1.0, 0.5),           // 1 time
    (0.0, 1.0, 1.0),           // 2 upward
    (0.0, 1.0, 1.0),           // 3 downward
    (-12.0, 12.0, 0.0),        // 4 low gain dB
    (-12.0, 12.0, 0.0),        // 5 mid gain dB
    (-12.0, 12.0, 0.0),        // 6 high gain dB
    (40.0, 1000.0, 120.0),     // 7 low/mid split Hz
    (1000.0, 12000.0, 2500.0), // 8 mid/high split Hz
    (-24.0, 12.0, 0.0),        // 9 output dB
];

const CTRL: u32 = 16;
const DOWN_T: [f32; 3] = [-33.8, -30.2, -35.5];
const UP_T: [f32; 3] = [-40.8, -41.8, -40.8];
const DOWN_R: f32 = 66.7;
const UP_R: f32 = 4.17;
const KNEE: f32 = 6.0;
const MAX_UP: f32 = 24.0;
const ATT_MS: [f32; 3] = [18.0, 10.0, 5.0];
const REL_MS: [f32; 3] = [180.0, 130.0, 90.0];
const RMS_MS: [f32; 3] = [12.0, 5.0, 2.0];

#[derive(Clone, Copy, Default)]
struct BandDyn {
    ms: f32,
    ms_coef: f32,
    g_db: f32,
    att: f32,
    rel: f32,
}

#[derive(Clone, Copy, Default)]
struct XoverState {
    lp1: [Biquad; 2],
    hp1: [Biquad; 2],
    lp2: [Biquad; 2],
    hp2: [Biquad; 2],
    ap: Biquad,
}

#[inline(always)]
fn knee_curve(d: f32, slope: f32) -> f32 {
    // slope * d above +K/2, quadratic blend inside the knee, 0 below -K/2
    if 2.0 * d < -KNEE {
        0.0
    } else if 2.0 * d.abs() <= KNEE {
        let t = d + 0.5 * KNEE;
        slope * t * t / (2.0 * KNEE)
    } else {
        slope * d
    }
}

pub struct Ott {
    sr: f32,
    depth: Smooth,
    time: f32,
    up_amt: f32,
    down_amt: f32,
    band_g: [Smooth; 3],
    out: Smooth,
    x_lo: Smooth, // log2 Hz
    x_hi: Smooth,
    c_lp1: Coefs,
    c_hp1: Coefs,
    c_lp2: Coefs,
    c_hp2: Coefs,
    c_ap2: Coefs,
    st: [XoverState; 2],
    bands: [BandDyn; 3],
    ctr: u32,
    fresh: bool,
    gain_meter: [f32; 3],
    level_meter: [f32; 3],
}

impl Ott {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let ctrl_rate = sr / CTRL as f32;
        let mut o = Self {
            sr,
            depth: Smooth::new(0.5, 15.0, sr),
            time: 0.5,
            up_amt: 1.0,
            down_amt: 1.0,
            band_g: core::array::from_fn(|_| Smooth::new(1.0, 15.0, sr)),
            out: Smooth::new(1.0, 15.0, sr),
            x_lo: Smooth::new(7.0, 15.0, ctrl_rate),
            x_hi: Smooth::new(11.3, 15.0, ctrl_rate),
            c_lp1: Coefs::IDENTITY,
            c_hp1: Coefs::IDENTITY,
            c_lp2: Coefs::IDENTITY,
            c_hp2: Coefs::IDENTITY,
            c_ap2: Coefs::IDENTITY,
            st: [XoverState::default(); 2],
            bands: [BandDyn::default(); 3],
            ctr: 0,
            fresh: true,
            gain_meter: [0.0; 3],
            level_meter: [-120.0; 3],
        };
        for b in 0..3 {
            o.bands[b].ms_coef = 1.0 - (-1.0 / (RMS_MS[b] * 0.001 * sr)).exp();
        }
        for (i, s) in SPEC.iter().enumerate() {
            o.set_param(i as u32, s.2);
        }
        o
    }

    fn update_times(&mut self) {
        let mult = 10f32.powf(1.6 * (self.time - 0.5));
        for b in 0..3 {
            self.bands[b].att = (-1.0 / (ATT_MS[b] * mult * 0.001 * self.sr)).exp();
            self.bands[b].rel = (-1.0 / (REL_MS[b] * mult * 0.001 * self.sr)).exp();
        }
    }

    fn update_xover(&mut self) {
        let sr = self.sr as f64;
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let lo = (self.x_lo.next().exp2() as f64).min(0.45 * sr);
        let hi = (self.x_hi.next().exp2() as f64)
            .min(0.45 * sr)
            .max(lo * 1.5);
        self.c_lp1 = Coefs::lowpass(lo, q, sr);
        self.c_hp1 = Coefs::highpass(lo, q, sr);
        self.c_lp2 = Coefs::lowpass(hi, q, sr);
        self.c_hp2 = Coefs::highpass(hi, q, sr);
        self.c_ap2 = Coefs::allpass(hi, q, sr);
    }

    fn snap_all(&mut self) {
        self.depth.snap();
        for g in self.band_g.iter_mut() {
            g.snap();
        }
        self.out.snap();
        self.x_lo.snap();
        self.x_hi.snap();
        self.update_xover();
    }

    #[inline(always)]
    fn split(&mut self, c: usize, x: f64) -> [f64; 3] {
        let s = &mut self.st[c];
        let lo = run2(&mut s.lp1, &self.c_lp1, x);
        let rest = run2(&mut s.hp1, &self.c_hp1, x);
        let mid = run2(&mut s.lp2, &self.c_lp2, rest);
        let hi = run2(&mut s.hp2, &self.c_hp2, rest);
        let lo = s.ap.run(&self.c_ap2, lo);
        [lo, mid, hi]
    }
}

impl Device for Ott {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.depth.set(v),
            1 => {
                self.time = v;
                self.update_times();
            }
            2 => self.up_amt = v,
            3 => self.down_amt = v,
            4..=6 => self.band_g[(index - 4) as usize].set(db_to_gain(v)),
            7 => self.x_lo.set(v.log2()),
            8 => self.x_hi.set(v.log2()),
            _ => self.out.set(db_to_gain(v)),
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
        let mut lvl_max = [-180.0f32; 3];
        for i in 0..n {
            if self.ctr == 0 && (!self.x_lo.settled() || !self.x_hi.settled()) {
                self.update_xover();
            }
            self.ctr = (self.ctr + 1) % CTRL;
            let bl = self.split(0, sane(l[i]) as f64);
            let br = self.split(1, sane(r[i]) as f64);
            let depth = self.depth.next();
            let og = self.out.next();
            let (mut yl, mut yr) = (0.0f32, 0.0f32);
            for b in 0..3 {
                let (xl, xr) = (bl[b] as f32, br[b] as f32);
                let d = &mut self.bands[b];
                let pk = xl.abs().max(xr.abs());
                d.ms = flush(d.ms + (pk * pk - d.ms) * d.ms_coef);
                let lvl = pk.max((2.0 * d.ms).sqrt());
                let lvl_db = if lvl > 1.0e-9 {
                    20.0 * lvl.log10()
                } else {
                    -180.0
                };
                let down = knee_curve(lvl_db - DOWN_T[b], 1.0 / DOWN_R - 1.0);
                let up = knee_curve(UP_T[b] - lvl_db, 1.0 - 1.0 / UP_R).min(MAX_UP)
                    * ((lvl_db + 90.0) / 20.0).clamp(0.0, 1.0);
                let target = down * self.down_amt + up * self.up_amt;
                let c = if target < d.g_db { d.att } else { d.rel };
                d.g_db = target + (d.g_db - target) * c;
                if d.g_db.abs() < 1.0e-6 {
                    d.g_db = 0.0;
                }
                lvl_max[b] = lvl_max[b].max(lvl_db);
                let g = 1.0 + depth * (10f32.powf(d.g_db * 0.05) - 1.0);
                let k = g * self.band_g[b].next();
                yl += xl * k;
                yr += xr * k;
                if i + 1 == n {
                    self.gain_meter[b] = gain_to_db(g);
                }
            }
            l[i] = yl * og;
            r[i] = yr * og;
        }
        if n > 0 {
            for b in 0..3 {
                self.level_meter[b] = lvl_max[b].max(-120.0);
            }
        }
    }

    fn reset(&mut self) {
        self.st = [XoverState::default(); 2];
        for b in self.bands.iter_mut() {
            b.ms = 0.0;
            b.g_db = 0.0;
        }
        self.gain_meter = [0.0; 3];
        self.level_meter = [-120.0; 3];
        self.ctr = 0;
        self.snap_all();
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        match index {
            0..=2 => self.level_meter[index as usize],
            3..=5 => self.gain_meter[(index - 3) as usize],
            _ => 0.0,
        }
    }
}
