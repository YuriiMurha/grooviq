//! Original coloured feedback delay. BPM duration sync only; no transport phase contract.

use crate::api::{Device, NoteEvent};
use crate::util::*;

/// `(min, max, default)` per parameter, `SPEC` index order.
pub const SPEC: [(f32, f32, f32); 15] = [
    (0.0, 1.0, 1.0),           // 0 sync
    (0.0, 8.0, 4.0),           // 1 division
    (1.0, 2000.0, 250.0),      // 2 time ms
    (0.0, 0.95, 0.35),         // 3 feedback
    (0.0, 1.0, 1.0),           // 4 ping-pong
    (20.0, 2000.0, 200.0),     // 5 low cut Hz
    (1000.0, 20000.0, 7000.0), // 6 high cut Hz
    (0.0, 1.0, 0.25),          // 7 mix
    (20.0, 400.0, 140.0),      // 8 bpm
    (0.0, 2.0, 0.0),           // 9 Digital / Tape / Analogue
    (0.0, 1.0, 0.35),          // 10 character
    (0.0, 1.0, 0.0),           // 11 duck
    (0.0, 1.0, 0.0),           // 12 modulation depth
    (0.05, 5.0, 0.5),          // 13 modulation Hz
    (-24.0, 6.0, 0.0),         // 14 output dB
];

/// Division lengths in quarter-note beats, `SPEC` index order.
pub const DIVISION_BEATS: [f32; 9] = [
    0.125,
    0.25,
    0.375,
    1.0 / 3.0,
    0.5,
    0.75,
    2.0 / 3.0,
    1.0,
    2.0,
];
const CTRL: u32 = 16;
const XFADE_MS: f32 = 50.0;
/// Longest delay: a half note at 20 BPM plus modulation margin.
const MAX_MS: f32 = 6010.0;

pub struct CharacterDelay {
    sr: f32,
    colour: [Smooth; 6],
    phase: f64,
    seed: u64,
    rng: [u64; 2],
    envelope: f32,
    gate: f32,
    dark: [f32; 2],
    buf: [DelayLine; 2],
    sync: bool,
    div: usize,
    time_ms: f32,
    bpm: f32,
    target: f32,
    ta: f32,
    tb: f32,
    x: f32,
    xfading: bool,
    xf_step: f32,
    fb: Smooth,
    pingpong: bool,
    lo: Smooth, // log2 Hz
    hi: Smooth,
    c_lo: Coefs,
    c_hi: Coefs,
    st_lo: [Biquad; 2],
    st_hi: [Biquad; 2],
    dry_g: Smooth,
    wet_g: Smooth,
    ctr: u32,
    fresh: bool,
    in_m: PeakMeter,
    out_m: PeakMeter,
}

impl CharacterDelay {
    pub fn new(sample_rate: f32) -> Self {
        Self::with_seed(sample_rate, 0x748f_21a9_370c_d165)
    }

    pub fn with_seed(sample_rate: f32, seed: u64) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(22050.0, 192000.0)
        } else {
            48000.0
        };
        let max = (MAX_MS * 0.001 * sr) as usize + 8;
        let ctrl_rate = sr / CTRL as f32;
        let mut d = Self {
            sr,
            colour: std::array::from_fn(|i| Smooth::new(SPEC[9 + i].2, 15.0, sr)),
            phase: 0.0,
            seed,
            rng: [seed ^ 0xa152_78bc_31de_9f03, seed ^ 0xf197_a5e8_c352_614b],
            envelope: 0.0,
            gate: 0.0,
            dark: [0.0; 2],
            buf: [DelayLine::new(max), DelayLine::new(max)],
            sync: true,
            div: 4,
            time_ms: 250.0,
            bpm: 140.0,
            target: 1.0,
            ta: 1.0,
            tb: 1.0,
            x: 0.0,
            xfading: false,
            xf_step: 1.0 / (XFADE_MS * 0.001 * sr),
            fb: Smooth::new(0.0, 15.0, sr),
            pingpong: true,
            lo: Smooth::new(7.6, 15.0, ctrl_rate),
            hi: Smooth::new(12.8, 15.0, ctrl_rate),
            c_lo: Coefs::IDENTITY,
            c_hi: Coefs::IDENTITY,
            st_lo: [Biquad::default(); 2],
            st_hi: [Biquad::default(); 2],
            dry_g: Smooth::new(1.0, 15.0, sr),
            wet_g: Smooth::new(0.0, 15.0, sr),
            ctr: 0,
            fresh: true,
            in_m: PeakMeter::default(),
            out_m: PeakMeter::default(),
        };
        for (i, s) in SPEC.iter().enumerate() {
            d.set_param(i as u32, s.2);
        }
        d
    }

    /// Effective delay time in ms.
    pub fn effective_ms(&self) -> f32 {
        if self.sync {
            DIVISION_BEATS[self.div] * 60_000.0 / self.bpm
        } else {
            self.time_ms
        }
    }

    fn update_target(&mut self) {
        let max = self.buf[0].max_delay() as f32 - 4.0;
        self.target = (self.effective_ms() * 0.001 * self.sr).clamp(1.0, max);
        if self.fresh {
            self.ta = self.target;
            self.tb = self.target;
            self.xfading = false;
        }
    }

    fn update_filters(&mut self) {
        let sr = self.sr as f64;
        let q = std::f64::consts::FRAC_1_SQRT_2;
        self.c_lo = Coefs::highpass(libm::exp2f(self.lo.next()) as f64, q, sr);
        self.c_hi = Coefs::lowpass((libm::exp2f(self.hi.next()) as f64).min(0.45 * sr), q, sr);
    }

    fn snap_all(&mut self) {
        for s in &mut self.colour {
            s.snap();
        }
        self.fb.snap();
        self.lo.snap();
        self.hi.snap();
        self.dry_g.snap();
        self.wet_g.snap();
        self.update_filters();
    }
}

impl Device for CharacterDelay {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.sync = v >= 0.5,
            1 => self.div = (v.round() as usize).min(DIVISION_BEATS.len() - 1),
            2 => self.time_ms = v,
            3 => self.fb.set(v),
            4 => self.pingpong = v >= 0.5,
            5 => self.lo.set(libm::log2f(v)),
            6 => self.hi.set(libm::log2f(v)),
            7 => {
                let a = v * std::f32::consts::FRAC_PI_2;
                self.dry_g.set(libm::cosf(a));
                self.wet_g.set(libm::sinf(a));
            }
            8 => self.bpm = v,
            9..=14 => self.colour[index as usize - 9].set(v),
            _ => {}
        }
        if matches!(index, 0 | 1 | 2 | 8) {
            self.update_target();
        }
        if self.fresh {
            self.snap_all();
        }
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.set_param(8, bpm);
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sc: Option<(&[f32], &[f32])>,
        _ev: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        if n == 0 {
            return;
        }
        self.fresh = false;
        for i in 0..n {
            if self.ctr == 0 && (!self.lo.settled() || !self.hi.settled()) {
                self.update_filters();
            }
            self.ctr = (self.ctr + 1) % CTRL;

            let xl = sane(l[i]).clamp(-16.0, 16.0);
            let xr = sane(r[i]).clamp(-16.0, 16.0);
            let c: [f32; 6] = std::array::from_fn(|j| self.colour[j].next());
            self.phase = (self.phase + c[4] as f64 / self.sr as f64) % 1.0;
            let tape = c[0].min(1.0) * (2.0 - c[0]).min(1.0);
            let analogue = (c[0] - 1.0).max(0.0);
            let wave = libm::sin(self.phase * std::f64::consts::TAU) as f32;
            let modulation = wave * (c[3] * 0.004 + tape * c[1] * 0.0007) * self.sr;
            let attack = libm::expf(-1.0 / (0.005 * self.sr));
            let release = libm::expf(-1.0 / (0.15 * self.sr));
            let level = xl.abs().max(xr.abs());
            let ec = if level > self.envelope {
                attack
            } else {
                release
            };
            self.envelope = level + ec * (self.envelope - level);
            self.in_m.update(xl.abs().max(xr.abs()), 1, self.sr);

            if !self.xfading && self.target != self.ta {
                self.tb = self.target;
                self.x = 0.0;
                self.xfading = true;
            }
            // the line is read before this sample is pushed, so "d - 1" gives a delay of d
            let (mut dl, mut dr) = (
                self.buf[0].read_frac((self.ta + modulation).clamp(1.0, 6.005 * self.sr) - 1.0),
                self.buf[1].read_frac((self.ta + modulation).clamp(1.0, 6.005 * self.sr) - 1.0),
            );
            if self.xfading {
                self.x = (self.x + self.xf_step).min(1.0);
                let a = self.x * std::f32::consts::FRAC_PI_2;
                let (ga, gb) = (libm::cosf(a), libm::sinf(a));
                let bl =
                    self.buf[0].read_frac((self.tb + modulation).clamp(1.0, 6.005 * self.sr) - 1.0);
                let br =
                    self.buf[1].read_frac((self.tb + modulation).clamp(1.0, 6.005 * self.sr) - 1.0);
                dl = dl * ga + bl * gb;
                dr = dr * ga + br * gb;
                if self.x >= 1.0 {
                    self.ta = self.tb;
                    self.xfading = false;
                }
            }
            let fl = self.st_hi[0].run(&self.c_hi, self.st_lo[0].run(&self.c_lo, dl as f64)) as f32;
            let fr = self.st_hi[1].run(&self.c_hi, self.st_lo[1].run(&self.c_lo, dr as f64)) as f32;
            self.gate = self.gate.max(fl.abs()).max(fr.abs()) * libm::expf(-1.0 / (self.sr * 0.3));
            let mut coloured = [fl, fr];
            let pole = libm::expf(-std::f32::consts::TAU * (12000.0 - 10000.0 * c[1]) / self.sr);
            for (ch, v) in coloured.iter_mut().enumerate() {
                self.dark[ch] = flush(*v + pole * (self.dark[ch] - *v));
                self.rng[ch] = self.rng[ch].wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut bits = self.rng[ch];
                bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                bits ^= bits >> 31;
                let noise = ((bits >> 40) as f32 / 8388608.0 - 1.0) * self.gate.min(1.0) * 0.00002;
                let saturated = fast_tanh(*v * (1.0 + 3.0 * c[1])) / (1.0 + 3.0 * c[1]);
                *v = *v + tape * (saturated - *v) + analogue * (self.dark[ch] - *v + noise * c[1]);
            }
            let [fl, fr] = coloured;
            let fb = self.fb.next();
            let (wl, wr) = if self.pingpong {
                (mid(xl, xr) + fb * fr, fb * fl)
            } else {
                (xl + fb * fl, xr + fb * fr)
            };
            self.buf[0].push(flush(soft_limit(wl)));
            self.buf[1].push(flush(soft_limit(wr)));
            let dg = self.dry_g.next();
            let wg = self.wet_g.next();
            let duck = 1.0 / (1.0 + 8.0 * c[2] * self.envelope);
            let output = libm::powf(10.0, c[5] / 20.0);
            l[i] = (xl * dg + fl * wg * duck) * output;
            r[i] = (xr * dg + fr * wg * duck) * output;
            self.out_m.update(l[i].abs().max(r[i].abs()), 1, self.sr);
        }
    }

    fn reset(&mut self) {
        for b in self.buf.iter_mut() {
            b.clear();
        }
        self.phase = 0.0;
        self.rng = [
            self.seed ^ 0xa152_78bc_31de_9f03,
            self.seed ^ 0xf197_a5e8_c352_614b,
        ];
        self.dark = [0.0; 2];
        self.envelope = 0.0;
        self.gate = 0.0;
        self.st_lo = [Biquad::default(); 2];
        self.st_hi = [Biquad::default(); 2];
        self.ta = self.target;
        self.tb = self.target;
        self.xfading = false;
        self.ctr = 0;
        self.snap_all();
        self.in_m = PeakMeter::default();
        self.out_m = PeakMeter::default();
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.in_m.db(),
            1 => self.out_m.db(),
            2 => self.effective_ms(),
            _ => 0.0,
        }
    }
}

/// Original provisional voices; owner sound approval remains under #41.
pub const PRESETS: [&str; 3] = ["Glass Steps", "Soft Spools", "Dusk Circuit"];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let (model, amount, modulation, duck) = match name {
        "Glass Steps" => (0.0, 0.0, 0.0, 0.0),
        "Soft Spools" => (1.0, 0.65, 0.2, 0.25),
        "Dusk Circuit" => (2.0, 0.8, 0.1, 0.4),
        _ => return None,
    };
    let mut patch: Vec<_> = SPEC
        .iter()
        .enumerate()
        .map(|(i, s)| (i as u32, s.2))
        .collect();
    for (i, v) in [(9, model), (10, amount), (12, modulation), (11, duck)] {
        patch[i].1 = v;
    }
    Some(patch)
}
