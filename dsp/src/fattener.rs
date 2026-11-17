//! Original two-macro saturator with stereo-linked compression. Amount zero is neutral
//! (within the +/-1 sample ceiling); Colour tilts around 700 Hz before saturation.
//! Fixed 2 ms attack / 80 ms release, no lookahead or true-peak claim. This inexpensive
//! base-rate nonlinear stage can alias; it is a colour effect, not a transparent limiter.
use crate::util::{OnePole, Smooth, clamp_param, fast_tanh, onepole_gain, sane};
use crate::{Device, NoteEvent};

/// Amount, Colour: `(minimum, maximum, neutral startup)`.
pub const SPEC: [(f32, f32, f32); 2] = [(0., 1., 0.), (-1., 1., 0.)];
pub const PARAM_IDS: [&str; 2] = ["amount", "colour"];
pub const PRESETS: [&str; 4] = ["Soft Body", "Drum Weight", "Bright Edge", "Dense Centre"];

/// Allocating host-side factory settings. Never call from the audio callback.
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let v = match name {
        "Init" | "Default" => [0., 0.],
        "Soft Body" => [0.25, -0.3],
        "Drum Weight" => [0.6, -0.6],
        "Bright Edge" => [0.7, 0.65],
        "Dense Centre" => [1., 0.],
        _ => return None,
    };
    Some(
        v.into_iter()
            .enumerate()
            .map(|(i, v)| (i as u32, v))
            .collect(),
    )
}

pub struct Fattener {
    amount: Smooth,
    colour: Smooth,
    filters: [OnePole; 2],
    tilt: f32,
    attack: f32,
    release: f32,
    envelope: f32,
    fresh: bool,
}
impl Fattener {
    pub fn new(sample_rate: f32) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(22050., 192000.)
        } else {
            48000.
        };
        Self {
            amount: Smooth::new(0., 10., sr),
            colour: Smooth::new(0., 10., sr),
            filters: [OnePole::default(); 2],
            tilt: onepole_gain(700., sr),
            attack: libm::expf(-1. / (0.002 * sr)),
            release: libm::expf(-1. / (0.08 * sr)),
            envelope: 0.,
            fresh: true,
        }
    }
}
impl Device for Fattener {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.amount.set(v),
            1 => self.colour.set(v),
            _ => {}
        }
        if self.fresh {
            self.amount.snap();
            self.colour.snap();
        }
    }
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sc: Option<(&[f32], &[f32])>,
        _ev: &[NoteEvent],
    ) {
        if l.is_empty() || r.is_empty() {
            return;
        }
        self.fresh = false;
        for (l, r) in l.iter_mut().zip(r.iter_mut()) {
            let a = self.amount.next();
            let c = self.colour.next();
            // Clamp before intermediate arithmetic: even f32::MAX cannot overflow the detector.
            let input = [sane(*l).clamp(-1., 1.), sane(*r).clamp(-1., 1.)];
            let mut wet = [0.; 2];
            for ch in 0..2 {
                let low = self.filters[ch].lp(input[ch], self.tilt);
                wet[ch] = input[ch] + c * 0.5 * (input[ch] - 2. * low);
            }
            let peak = wet[0].abs().max(wet[1].abs());
            let coef = if peak > self.envelope {
                self.attack
            } else {
                self.release
            };
            self.envelope = peak + (self.envelope - peak) * coef;
            let threshold = 0.8 - 0.6 * a;
            let gain = if self.envelope > threshold {
                threshold / self.envelope
            } else {
                1.
            };
            for (ch, out) in [l, r].into_iter().enumerate() {
                let y = fast_tanh(wet[ch] * gain * (1. + 5. * a));
                *out = (input[ch] + a * (y - input[ch])).clamp(-1., 1.);
            }
        }
    }
    fn reset(&mut self) {
        self.filters = [OnePole::default(); 2];
        self.envelope = 0.;
        self.amount.snap();
        self.colour.snap();
        self.fresh = true;
    }
}
