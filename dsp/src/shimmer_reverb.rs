//! Original four-line pitch-feedback cloud. Decay is the nominal -60 dB time of
//! a 120 ms circulation at unity Feedback, not an analytical acoustic RT60.
//! Fractional reads and all routing are convex (or signed sub-convex) combinations:
//! their sup norm is <= 1. Feedback is strictly < 1 even during automation.
//! Moving reads can alias; no alias-free or artifact-free claim is made.
use crate::util::{Smooth, clamp_param, sane};
use crate::{Device, NoteEvent};
pub const SPEC: [(f32, f32, f32); 6] = [
    (0., 1., 0.15),
    (0.2, 20., 4.),
    (0., 3., 0.),
    (0., 0.98, 0.65),
    (0., 1., 0.7),
    (0., 1., 0.2),
];
pub const PARAM_IDS: [&str; 6] = [
    "mix",
    "decay",
    "pitch",
    "feedback",
    "diffusion",
    "modulation",
];
pub const PRESETS: [&str; 5] = [
    "Init",
    "Upper canopy",
    "Fifth mist",
    "Low lantern",
    "Falling glass",
];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let values = match name {
        "Init" | "Default" => [0.15, 4., 0., 0.65, 0.7, 0.2],
        "Upper canopy" => [0.5, 12., 0., 0.9, 0.85, 0.4],
        "Fifth mist" => [0.45, 9., 1., 0.85, 0.9, 0.65],
        "Low lantern" => [0.4, 7., 2., 0.8, 0.6, 0.3],
        "Falling glass" => [0.5, 10., 3., 0.88, 0.8, 0.5],
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
const RATIOS: [f32; 4] = [2., 1.5, 0.5, 2. / 3.];
fn tiny(x: f32) -> f32 {
    if x.abs() < 1e-30 { 0. } else { x }
}
struct Line {
    data: Vec<f32>,
    pos: usize,
    phase: [f32; 4],
    initial: [f32; 4],
    base: f32,
    window: f32,
}
impl Line {
    fn read(&self, delay: f32) -> f32 {
        let x = (self.pos as f32 - delay).rem_euclid(self.data.len() as f32);
        let x = if x >= self.data.len() as f32 { 0. } else { x };
        let i = x as usize;
        let t = x - i as f32;
        // Explicit constant-sum positive interpolation, never equal-power feedback.
        self.data[i] * (1. - t) + self.data[(i + 1) % self.data.len()] * t
    }
    fn shifted(&mut self, mode: f32, modulation: f32, lfo: f32) -> f32 {
        let mut out = 0.;
        for (k, ratio) in RATIOS.iter().enumerate() {
            let p = self.phase[k];
            let q = (p + 0.5).fract();
            let w = 1. - (2. * p - 1.).abs();
            let offset = modulation * 0.002 * self.base * lfo;
            let a = self.read(self.base + p * self.window + offset);
            let b = self.read(self.base + q * self.window + offset);
            let weight = (1. - (mode - k as f32).abs()).max(0.);
            out += weight * (w * a + (1. - w) * b);
            self.phase[k] = (p + (1. - ratio) / self.window).rem_euclid(1.);
        }
        out
    }
}
pub struct ShimmerReverb {
    controls: [Smooth; 6],
    lines: [Line; 4],
    sr: f32,
    phase: [f32; 4],
    initial: [f32; 4],
    fresh: bool,
}
impl ShimmerReverb {
    pub fn new(sr: f32) -> Self {
        Self::with_seed(sr, 0x739ac51)
    }
    pub fn with_seed(sr: f32, mut seed: u64) -> Self {
        let sr = if sr.is_finite() {
            sr.clamp(22050., 192000.)
        } else {
            48000.
        };
        let mut random = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 40) as f32 / 16777216.
        };
        let initial = std::array::from_fn(|_| random());
        let lines = std::array::from_fn(|i| {
            let base = sr * [0.071, 0.089, 0.107, 0.127][i];
            let window = sr * 0.06;
            let initial = std::array::from_fn(|_| random());
            Line {
                data: vec![0.; (base + window + sr * 0.002) as usize + 4],
                pos: 0,
                phase: initial,
                initial,
                base,
                window,
            }
        });
        Self {
            controls: SPEC.map(|(_, _, v)| Smooth::new(v, 10., sr)),
            lines,
            sr,
            phase: initial,
            initial,
            fresh: true,
        }
    }
}
impl Device for ShimmerReverb {
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
        if l.is_empty() || r.is_empty() {
            return;
        }
        self.fresh = false;
        for (l, r) in l.iter_mut().zip(r.iter_mut()) {
            let c = self.controls.each_mut().map(|c| c.next());
            let input = [sane(*l).clamp(-8., 8.), sane(*r).clamp(-8., 8.)];
            let mut taps = [0.; 4];
            for (i, tap) in taps.iter_mut().enumerate() {
                let lfo = libm::sinf(self.phase[i] * std::f32::consts::TAU);
                self.phase[i] = (self.phase[i] + [0.13, 0.17, 0.19, 0.23][i] / self.sr).fract();
                let pitched = self.lines[i].shifted(c[2], c[5], lfo);
                let plain = self.lines[i].read(self.lines[i].base);
                *tap = plain * 0.4 + pitched * 0.6;
            }
            let gain = c[3] * libm::expf(-6.9077554 * 0.12 / c[1]);
            for i in 0..4 {
                // Signed cyclic propagation blended with a passive two-neighbour diffuser.
                let diffuse = (taps[(i + 1) % 4] - taps[(i + 2) % 4]) * 0.5;
                let route = (1. - c[4]) * taps[(i + 1) % 4] + c[4] * diffuse;
                let line = &mut self.lines[i];
                line.data[line.pos] = tiny(input[i % 2] * 0.1 + gain * route);
                line.pos = (line.pos + 1) % line.data.len();
            }
            *l = input[0] * (1. - c[0]) + c[0] * (taps[0] + taps[2]) * 0.5;
            *r = input[1] * (1. - c[0]) + c[0] * (taps[1] + taps[3]) * 0.5;
        }
    }
    fn reset(&mut self) {
        for line in &mut self.lines {
            line.data.fill(0.);
            line.pos = 0;
            line.phase = line.initial;
        }
        for c in &mut self.controls {
            c.snap();
        }
        self.phase = self.initial;
        self.fresh = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pitch_readers_shift_all_four_intervals() {
        // Independent musical oracle: octaves and pure 3:2 / 2:3 fifths.
        // Reading RATIOS here would make wrong production tuning self-validating.
        let expected_ratios = [2.0_f64, 1.5, 0.5, 2.0 / 3.0];
        for sr in [22050., 48000., 192000.] {
            for (carrier, frequency) in [400., 403.17].into_iter().enumerate() {
                for (k, &ratio) in expected_ratios.iter().enumerate() {
                    let mut d = ShimmerReverb::new(sr);
                    let line = &mut d.lines[0];
                    let mut projection = [0f64; 2];
                    let mut original = [0f64; 2];
                    for n in 0..sr as usize * 2 {
                        line.data[line.pos] =
                            libm::sinf(n as f32 * frequency * std::f32::consts::TAU / sr);
                        let before = line.phase[k] as f64;
                        let y = line.shifted(k as f32, 0., 0.);
                        let mut advance = line.phase[k] as f64 - before;
                        if advance > 0.5 {
                            advance -= 1.;
                        } else if advance < -0.5 {
                            advance += 1.;
                        }
                        let observed_ratio = 1. - advance * line.window as f64;
                        assert!(
                            (observed_ratio - ratio).abs() < 0.002,
                            "reader speed at {sr}, interval {k}: {observed_ratio} vs {ratio}"
                        );
                        line.pos = (line.pos + 1) % line.data.len();
                        if n > sr as usize {
                            let t = n as f64 / sr as f64 * std::f64::consts::TAU;
                            projection[0] += y as f64 * libm::cos(t * frequency as f64 * ratio);
                            projection[1] += y as f64 * libm::sin(t * frequency as f64 * ratio);
                            original[0] += y as f64 * libm::cos(t * frequency as f64);
                            original[1] += y as f64 * libm::sin(t * frequency as f64);
                        }
                    }
                    let power = |a: [f64; 2]| a[0] * a[0] + a[1] * a[1];
                    assert!(
                        power(projection) > power(original) * 10.,
                        "rate {sr}, frequency {frequency}, interval {k}"
                    );
                    // Only the whole-cycle carrier is grain-phase coherent.
                    // The noninteger carrier spreads into sidebands; its reader
                    // speed is still checked independently on every sample.
                    if carrier == 0 {
                        assert!(
                            power(projection) > (sr as f64 * 0.04) * (sr as f64 * 0.04),
                            "rate {sr}, frequency {frequency}, interval {k}: {:?}",
                            projection
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn every_control_has_ten_ms_sample_ramp_and_empty_is_inert() {
        for index in 0..SPEC.len() {
            let mut d = ShimmerReverb::new(48000.);
            d.process(&mut [0.], &mut [0.], None, &[]);
            let old = d.controls[index].cur;
            let target = if index == 2 { 3. } else { SPEC[index].1 };
            d.set_param(index as u32, target);
            d.process(&mut [], &mut [], None, &[]);
            assert_eq!(d.controls[index].cur, old);
            for n in 1..=480 {
                d.process(&mut [0.], &mut [0.], None, &[]);
                let expected = target + (old - target) * libm::expf(-(n as f32) / 480.);
                assert!(
                    (d.controls[index].cur - expected).abs() < 0.0002,
                    "control {index}, n {n}"
                );
            }
        }
    }
    #[test]
    fn charged_reset_and_tiny_recursive_states_clear() {
        let mut d = ShimmerReverb::new(22050.);
        d.process(&mut [1.; 128], &mut [-1.; 128], None, &[]);
        assert!(d.lines.iter().all(|l| l.data.iter().any(|x| *x != 0.)));
        d.set_param(2, 3.);
        d.reset();
        assert!(d.lines.iter().all(|l| l.data.iter().all(|x| *x == 0.)));
        assert_eq!(d.controls[2].cur, 3.);
        for line in &mut d.lines {
            line.data.fill(1e-35);
        }
        for _ in 0..10000 {
            d.process(&mut [0.], &mut [0.], None, &[]);
        }
        assert!(d.lines.iter().all(|l| l.data.iter().all(|x| *x == 0.)));
        // This quiet but normal material must survive the recursive clearing threshold.
        d.process(&mut [1e-22; 128], &mut [1e-22; 128], None, &[]);
        assert!(
            d.lines
                .iter()
                .any(|l| l.data.iter().any(|x| x.abs() > 1e-25))
        );
    }
    #[test]
    fn moving_pitch_feedback_is_contractive_without_output_limiter() {
        for sr in [22050., 48000., 192000.] {
            let mut d = ShimmerReverb::new(sr);
            d.set_param(0, 1.);
            d.set_param(1, 20.);
            d.set_param(3, 0.98);
            d.process(&mut [1.], &mut [-1.], None, &[]);
            let mut early = 0f64;
            let mut late = 0f64;
            for n in 0..sr as usize * 12 {
                if n % 127 == 0 {
                    d.set_param(2, (n % 3001) as f32 / 1000.);
                    d.set_param(4, if n % 254 == 0 { 0. } else { 1. });
                    d.set_param(5, if n % 254 == 0 { 1. } else { 0. });
                    d.set_param(3, if n % 254 == 0 { 0.98 } else { 0.9 });
                }
                let (mut l, mut r) = ([0.], [0.]);
                d.process(&mut l, &mut r, None, &[]);
                let e = (l[0] as f64) * (l[0] as f64) + (r[0] as f64) * (r[0] as f64);
                if n < sr as usize * 2 {
                    early += e;
                }
                if n > sr as usize * 10 {
                    late += e;
                }
            }
            assert!(early > 1e-6);
            assert!(late < early * 1e-6, "{sr}: {early} -> {late}");
        }
    }
}

#[cfg(test)]
mod routing_test {
    use super::*;
    #[test]
    fn diffusion_scatters_charge_passively() {
        for diffusion in [0., 0.5, 1.] {
            let mut d = ShimmerReverb::new(48000.);
            d.set_param(4, diffusion);
            // Uniform charge in exactly one lane isolates scattering from interpolation.
            d.lines[0].data.fill(1.);
            let gain = 0.65 * libm::expf(-6.9077554 * 0.12 / 4.);
            d.process(&mut [0.], &mut [0.], None, &[]);
            let written = d.lines.each_ref().map(|l| l.data[0]);
            assert_eq!(written[0], 0.);
            assert_eq!(written[1], 0.);
            assert!((written[2] + diffusion * 0.5 * gain).abs() < 1e-6);
            assert!((written[3] - (1. - diffusion * 0.5) * gain).abs() < 1e-6);
            assert!(written.iter().map(|x| x.abs()).sum::<f32>() <= gain + 1e-6);
        }
    }
}

#[cfg(test)]
mod passive_tests {
    use super::*;
    #[test]
    fn pitch_crossfades_cannot_amplify_constant_charge() {
        for sr in [22050., 48000., 192000.] {
            let mut d = ShimmerReverb::new(sr);
            for line in &mut d.lines {
                line.data.fill(1.);
                for mode in [0., 0.37, 1., 1.73, 2., 2.91, 3.] {
                    for n in 0..1000 {
                        let y = line.shifted(mode, 1., libm::sinf(n as f32 * 0.1));
                        assert!((y - 1.).abs() < 2e-6, "{sr}, {mode}: {y}");
                    }
                }
            }
        }
    }
    #[test]
    fn memory_at_maximum_rate() {
        let d = ShimmerReverb::new(192000.);
        let samples: usize = d.lines.iter().map(|l| l.data.len()).sum();
        println!(
            "192k ring samples {samples}, buffer bytes {}, inline bytes {}",
            samples * 4,
            std::mem::size_of::<ShimmerReverb>()
        );
        assert!(samples * 4 <= 494000);
    }
}
