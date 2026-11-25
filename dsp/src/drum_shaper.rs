//! Stereo-linked envelope contrast shaper. Modes blend continuously running detectors;
//! curves interpolate bounded transient weights, without resetting envelope history.
use crate::util::{Smooth, clamp_param, fast_tanh, flush, flush64, sane};
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
    hold: [u32; 3],
    hold_samples: u32,
    slow: [f64; 3],
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
            hold: [0; 3],
            hold_samples: (sr * 0.012) as u32,
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
                // Bridge bass half-cycles without lookahead. Only falling levels wait;
                // a new onset still follows the original mode-specific attack.
                if level >= self.fast[m] {
                    self.hold[m] = self.hold_samples;
                    self.fast[m] = flush(level + (self.fast[m] - level) * self.attack[m]);
                } else if level >= self.fast[m] * 0.99 {
                    // Sampled peaks vary with phase. Refresh near a peak without raising it;
                    // a falling envelope releases after it moves below this 1% margin.
                    self.hold[m] = self.hold_samples;
                } else if self.hold[m] > 0 {
                    self.hold[m] -= 1;
                } else {
                    self.fast[m] = flush(level + (self.fast[m] - level) * self.release[m]);
                }
                // Keep the lag state in f64: f32 rounding leaves a steady residual
                // that the Sharp curve's square root turns into an audible gain offset.
                let fast = f64::from(self.fast[m]);
                self.slow[m] = flush64(fast + (self.slow[m] - fast) * f64::from(self.lag[m]));
                *weight = ((fast - self.slow[m]) / fast.max(1.0e-6)).clamp(0.0, 1.0) as f32;
            }
            let p = self.controls.each_mut().map(Smooth::next);
            let t = blend(weights, p[2]);
            let t = blend([libm::sqrtf(t), t, t * t], p[3]);
            let active = (blend(self.fast, p[2]) * 10000.0).min(1.0);
            let target = (p[0] * t + p[1] * (1.0 - t)) * active;
            self.gain = flush(target + (self.gain - target) * self.gain_coef);
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
        self.hold = [0; 3];
        self.slow = [0.0; 3];
        self.gain = 0.0;
        for p in &mut self.controls {
            p.snap();
        }
        self.fresh = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharp_retains_sub_ulp_envelope_contrast() {
        let mut d = DrumShaper::new(48000.0);
        for (index, value) in [(1, 12.0), (2, 2.0), (3, 0.0)] {
            d.set_param(index, value);
        }
        d.fast = [0.5; 3];
        // One quarter of the f32 step below 0.5. Rounding the slow state
        // before subtraction would erase this small but real contrast.
        d.slow = [0.5 - 1.0 / 134_217_728.0; 3];
        d.gain = 12.0;
        d.process(&mut [0.5; 128], &mut [0.125; 128], None, &[]);
        assert!(d.gain < 11.999 && d.gain > 11.99, "gain {}", d.gain);
    }
    #[test]
    fn slow_follower_converges_and_flushes_subnormal_state() {
        let mut d = DrumShaper::new(192000.0);
        for _ in 0..3000 {
            d.process(&mut [0.5; 128], &mut [0.125; 128], None, &[]);
        }
        for m in 0..3 {
            // The real-valued recurrence converges to fast. This tolerance is
            // loose for f64, but far below the f32 fixed-point rounding floor.
            assert!((f64::from(d.fast[m]) - d.slow[m]).abs() < 1.0e-10);
        }
        d.reset();
        d.slow = [f64::MIN_POSITIVE * 0.5; 3];
        d.process(&mut [0.0], &mut [0.0], None, &[]);
        assert_eq!(d.slow, [0.0; 3]);
    }
    #[test]
    fn near_peak_hold_still_tracks_falling_envelopes() {
        for sr in [22050.0, 48000.0, 192000.0] {
            let mut d = DrumShaper::new(sr);
            let mut l = [0.5; 128];
            let mut r = l;
            for _ in 0..(sr as usize / 128) {
                l.fill(0.5);
                r.fill(0.5);
                d.process(&mut l, &mut r, None, &[]);
            }
            // A 6 dB/s fade must keep tracking; near-peak refresh cannot freeze a peak.
            let mut last = 0.5;
            for block in 0..(sr as usize / 128) {
                for (i, (left, right)) in l.iter_mut().zip(&mut r).enumerate() {
                    last = 0.5 * db_to_gain(-6.0 * (block * 128 + i) as f32 / sr);
                    *left = last;
                    *right = last;
                }
                d.process(&mut l, &mut r, None, &[]);
            }
            assert!(d.fast[0] >= last && d.fast[0] < last * 1.08);
            // A larger drop leaves the margin immediately, then follows the normal release.
            for _ in 0..(sr as usize / 128) {
                l.fill(0.1);
                r.fill(0.1);
                d.process(&mut l, &mut r, None, &[]);
            }
            assert!(d.fast[0] >= 0.1 && d.fast[0] < 0.102);
        }
    }
    #[test]
    fn shaping_gain_flushes_subnormals_and_clears_after_tail() {
        let mut d = DrumShaper::new(48000.0);
        // Exercise the private feedback state at the normal/subnormal boundary.
        d.gain = f32::MIN_POSITIVE * 0.5;
        let mut l = [0.0; 128];
        let mut r = l;
        d.process(&mut l, &mut r, None, &[]);
        assert_eq!(d.gain, 0.0);
        d.reset();
        d.set_param(1, 12.0);
        l.fill(0.3);
        r.fill(0.3);
        for _ in 0..40 {
            l.fill(0.3);
            r.fill(0.3);
            d.process(&mut l, &mut r, None, &[]);
        }
        assert!(d.gain > 1.0);
        for _ in 0..4000 {
            l.fill(0.0);
            r.fill(0.0);
            d.process(&mut l, &mut r, None, &[]);
            assert!(d.gain == 0.0 || d.gain.is_normal());
            assert_eq!(l, [0.0; 128]);
            assert_eq!(r, [0.0; 128]);
        }
        // Shared flush arithmetic can leave a tiny normal envelope floor;
        // it must neither sustain gain audibly nor enter subnormal arithmetic.
        assert!(d.gain.abs() < 1.0e-15);
    }
}
