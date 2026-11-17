//! Original character saturator. Fixed 4x FIR oversampling, 49 samples of latency.
//! Auto gain compensates drive; it does not promise equal perceived loudness.
use crate::api::{Device, NoteEvent, db_to_gain};
use crate::oversampling::{Down, Up};
use crate::util::*;

pub const PARAM_IDS: [&str; 9] = [
    "model",
    "drive",
    "tone",
    "boost",
    "low_cut",
    "high_cut",
    "auto_gain",
    "mix",
    "output",
];
/// Model: Tube, Tape, Transistor, Console. Boost is dB; cuts are Hz.
pub const SPEC: [(f32, f32, f32); 9] = [
    (0., 3., 0.),
    (0., 30., 0.),
    (-1., 1., 0.),
    (0., 18., 0.),
    (20., 2000., 20.),
    (1000., 20000., 20000.),
    (0., 1., 1.),
    (0., 1., 1.),
    (-24., 12., 0.),
];
pub const PRESETS: [&str; 5] = [
    "Init",
    "Amber filament",
    "Soft spool",
    "Edge current",
    "Desk glow",
];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let values = match name {
        "Init" => [0., 0., 0., 0., 20., 20000., 1., 1., 0.],
        "Amber filament" => [0., 12., 0.2, 3., 45., 16000., 1., 0.85, 0.],
        "Soft spool" => [1., 18., -0.35, 2., 30., 11000., 1., 1., 1.],
        "Edge current" => [2., 16., 0.4, 6., 90., 18000., 1., 0.7, -2.],
        "Desk glow" => [3., 8., 0.1, 2., 35., 17000., 1., 1., 0.],
        _ => return None,
    };
    Some(
        values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i as u32, v))
            .collect(),
    )
}
fn curve(model: usize, x: f32) -> f32 {
    let t = fast_tanh(x);
    match model {
        0 => t + 0.18 * t * t,
        1 => {
            let u = fast_tanh(x * 0.8);
            u * 1.25 - 0.10 * u * u * u
        }
        2 => x / (1. + x.abs()),
        _ => {
            let u = fast_tanh(x * 0.65);
            (u + 0.06 * u * u) * (1. / 0.65)
        }
    }
}
struct Channel {
    up_a: Up,
    up_b: Up,
    down_a: Down,
    down_b: Down,
    z: f32,
    dry: DelayLine,
    hp: OnePole,
    lp: OnePole,
    tilt: OnePole,
    dc: DcBlock,
}
impl Channel {
    fn new() -> Self {
        Self {
            up_a: Up::new(22, 8.6),
            up_b: Up::new(6, 9.),
            down_a: Down::new(22, 8.6),
            down_b: Down::new(6, 9.),
            z: 0.,
            dry: DelayLine::new(50),
            hp: OnePole::default(),
            lp: OnePole::default(),
            tilt: OnePole::default(),
            dc: DcBlock::default(),
        }
    }
    fn clear(&mut self) {
        for h in [
            &mut self.up_a.h,
            &mut self.up_b.h,
            &mut self.down_a.e,
            &mut self.down_a.o,
            &mut self.down_b.e,
            &mut self.down_b.o,
        ] {
            h.clear();
        }
        self.z = 0.;
        self.dry.clear();
        self.hp.reset();
        self.lp.reset();
        self.tilt.reset();
        self.dc.reset();
    }
    fn nonlinear(&mut self, x: f32, w: [f32; 4]) -> f32 {
        let shape = |v: f32| {
            let mut y = 0.;
            for (m, g) in w.iter().enumerate() {
                y += curve(m, v) * g;
            }
            y
        };
        let (a, b) = self.up_a.run(x);
        let (a1, a2) = self.up_b.run(a);
        let (b1, b2) = self.up_b.run(b);
        let ya = self.down_b.run(shape(a1), shape(a2));
        let yb = self.down_b.run(shape(b1), shape(b2));
        let prev = self.z;
        self.z = yb;
        self.down_a.run(prev, ya)
    }
}
pub struct Saturation {
    sr: f32,
    controls: [Smooth; 8],
    models: [Smooth; 4],
    ch: [Channel; 2],
    fresh: bool,
}
impl Saturation {
    pub fn new(sample_rate: f32) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(22050., 192000.)
        } else {
            48000.
        };
        let mut d = Self {
            sr,
            controls: [Smooth::new(0., 10., sr); 8],
            models: [Smooth::new(0., 10., sr); 4],
            ch: [Channel::new(), Channel::new()],
            fresh: true,
        };
        for (i, s) in SPEC.iter().enumerate() {
            d.set_param(i as u32, s.2);
        }
        d
    }
    pub fn latency(&self) -> usize {
        49
    }
    fn snap(&mut self) {
        for s in self.controls.iter_mut().chain(self.models.iter_mut()) {
            s.snap();
        }
    }
}
impl Device for Saturation {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        if index == 0 {
            let m = v.round() as usize;
            for (i, s) in self.models.iter_mut().enumerate() {
                s.set(if i == m { 1. } else { 0. });
            }
        } else {
            self.controls[index as usize - 1].set(v);
        }
        if self.fresh {
            self.snap();
        }
    }
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        if n == 0 {
            return;
        }
        self.fresh = false;
        for i in 0..n {
            let p = self.controls.each_mut().map(Smooth::next);
            let w = self.models.each_mut().map(Smooth::next);
            let gain = db_to_gain(p[0] + p[2]);
            let compensation = db_to_gain(-(p[0] + p[2]) * p[5]);
            let hp = onepole_gain(p[3].min(self.sr * 0.45), self.sr);
            let lp = onepole_gain(p[4].min(self.sr * 0.45), self.sr);
            let tilt = onepole_gain(700., self.sr);
            let lo = db_to_gain(-6. * p[1]);
            let hi = db_to_gain(6. * p[1]);
            let out = db_to_gain(p[7]);
            for (ch, x) in self.ch.iter_mut().zip([&mut l[i], &mut r[i]]) {
                // Bound finite hostile input before gains and FIR accumulation can overflow.
                let input = sane(*x).clamp(-64., 64.);
                ch.dry.push(input);
                let dry = ch.dry.read_int(49);
                let mut y = ch.nonlinear(input * gain, w);
                y = ch.dc.run(y, dc_coef(5., self.sr));
                y -= ch.hp.lp(y, hp);
                y = ch.lp.lp(y, lp);
                let low = ch.tilt.lp(y, tilt);
                y = (low * lo + (y - low) * hi) * compensation;
                *x = sane((dry + (y - dry) * p[6]) * out);
            }
        }
    }
    fn reset(&mut self) {
        for ch in &mut self.ch {
            ch.clear();
        }
        self.snap();
        self.fresh = true;
    }
}
