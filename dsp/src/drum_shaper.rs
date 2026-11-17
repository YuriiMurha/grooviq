//! Stereo-linked envelope contrast shaper. Modes blend continuously running detectors;
//! curves interpolate bounded transient weights, without resetting envelope history.
use crate::util::{Smooth, clamp_param, fast_tanh, flush, sane};
use crate::{Device, NoteEvent, db_to_gain};

pub const SPEC: [(f32, f32, f32); 7] = [
    (-12.0, 12.0, 0.0), // attack dB
    (-12.0, 12.0, 0.0), // sustain dB
    (0.0, 2.0, 0.0),    // Kick / Snare / Bus
    (0.0, 2.0, 1.0),    // Sharp / Natural / Smooth
    (0.0, 1.0, 0.0),    // saturation
    (0.0, 1.0, 1.0),    // mix
    (-24.0, 12.0, 0.0), // output dB
];
pub const PRESETS: &[&str] = &["Kick Punch", "Snare Body", "Bus Softening"];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let values = match name {
        "Kick Punch" => [6.0, -3.0, 0.0, 0.0, 0.1, 1.0, -2.0],
        "Snare Body" => [2.0, 6.0, 1.0, 1.0, 0.2, 0.8, -3.0],
        "Bus Softening" => [-4.0, 2.0, 2.0, 2.0, 0.05, 0.7, -1.0],
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

pub struct DrumShaper {
    controls: [Smooth; 7],
    fast: [f32; 3],
    slow: [f32; 3],
    attack: [f32; 3],
    release: [f32; 3],
    lag: [f32; 3],
    gain: f32,
    gain_coef: f32,
    fresh: bool,
}
fn blend(v: [f32; 3], position: f32) -> f32 {
    if position <= 1.0 {
        v[0] + (v[1] - v[0]) * position
    } else {
        v[1] + (v[2] - v[1]) * (position - 1.0)
    }
}
impl DrumShaper {
    pub fn new(sample_rate: f32) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(22050.0, 192000.0)
        } else {
            44100.0
        };
        let coef = |ms: f32| libm::expf(-1000.0 / (ms * sr));
        Self {
            controls: SPEC.map(|(_, _, v)| Smooth::new(v, 10.0, sr)),
            fast: [0.0; 3],
            slow: [0.0; 3],
            attack: [coef(0.3), coef(0.1), coef(2.0)],
            release: [coef(40.0), coef(15.0), coef(80.0)],
            lag: [coef(35.0), coef(12.0), coef(60.0)],
            gain: 0.0,
            gain_coef: coef(1.0),
            fresh: true,
        }
    }
}
impl Device for DrumShaper {
    fn set_param(&mut self, index: u32, value: f32) {
        if let Some(v) = clamp_param(&SPEC, index, value) {
            self.controls[index as usize].set(v);
            if self.fresh {
                self.controls[index as usize].snap();
            }
        }
    }
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        self.fresh = false;
        for (l, r) in l.iter_mut().zip(r) {
            let x = [sane(*l), sane(*r)];
            let level = x[0].abs().max(x[1].abs()).min(16.0);
            let mut weights = [0.0; 3];
            for (m, weight) in weights.iter_mut().enumerate() {
                let c = if level > self.fast[m] {
                    self.attack[m]
                } else {
                    self.release[m]
                };
                self.fast[m] = flush(level + (self.fast[m] - level) * c);
                self.slow[m] = flush(self.fast[m] + (self.slow[m] - self.fast[m]) * self.lag[m]);
                *weight =
                    ((self.fast[m] - self.slow[m]) / self.fast[m].max(1.0e-6)).clamp(0.0, 1.0);
            }
            let p = self.controls.each_mut().map(Smooth::next);
            let t = blend(weights, p[2]);
            let t = blend([libm::sqrtf(t), t, t * t], p[3]);
            let active = (level * 10000.0).min(1.0);
            let target = (p[0] * t + p[1] * (1.0 - t)) * active;
            self.gain = target + (self.gain - target) * self.gain_coef;
            let gain = db_to_gain(self.gain);
            let output = db_to_gain(p[6]);
            // Unit small-signal slope; saturation is independent of envelope shaping.
            let drive = 1.0 + 7.0 * p[4];
            let wet = x.map(|v| {
                let shaped = v * gain;
                let saturated = fast_tanh(shaped * drive) / drive;
                (shaped + p[4] * (saturated - shaped)) * output
            });
            *l = x[0] + p[5] * (wet[0] - x[0]);
            *r = x[1] + p[5] * (wet[1] - x[1]);
        }
    }
    fn reset(&mut self) {
        self.fast = [0.0; 3];
        self.slow = [0.0; 3];
        self.gain = 0.0;
        for p in &mut self.controls {
            p.snap();
        }
        self.fresh = true;
    }
}
