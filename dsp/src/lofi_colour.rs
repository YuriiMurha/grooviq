//! Original colour chain. Timing is in seconds, independent of host blocks.
use crate::util::{DelayLine, Rng, Smooth, clamp_param, fast_tanh, sane};
use crate::{Device, NoteEvent};

/// Magnitude, noise, wow/flutter, distortion, digital, space, dropouts,
/// noise selection (0 vinyl / 1 tape / 2 hum; crossfaded continuously).
pub const SPEC: [(f32, f32, f32); 8] = [
    (0., 1., 0.),
    (0., 1., 0.),
    (0., 1., 0.),
    (0., 1., 0.),
    (0., 1., 0.),
    (0., 1., 0.),
    (0., 1., 0.),
    (0., 2., 1.),
];
pub const PRESETS: [&str; 3] = ["Paper Haze", "Bent Ribbon", "Small Hours"];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let p = match name {
        "Init" | "Default" => SPEC.map(|s| s.2),
        "Paper Haze" => [0.7, 0.3, 0.2, 0.3, 0.2, 0.2, 0.1, 0.],
        "Bent Ribbon" => [0.9, 0.2, 0.8, 0.6, 0.3, 0.3, 0.7, 1.],
        "Small Hours" => [0.6, 0.4, 0.3, 0.2, 0.7, 0.8, 0.3, 2.],
        _ => return None,
    };
    Some(
        p.into_iter()
            .enumerate()
            .map(|(i, v)| (i as u32, v))
            .collect(),
    )
}
fn tiny(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0. } else { x }
}
struct Channel {
    motion: DelayLine,
    space: DelayLine,
    noise: Rng,
    held: f32,
    clock: f32,
}
impl Channel {
    fn new(sr: f32, seed: u32) -> Self {
        Self {
            motion: DelayLine::new((sr * 0.012) as usize + 8),
            space: DelayLine::new((sr * 0.12) as usize + 8),
            noise: Rng(seed.max(1)),
            held: 0.,
            clock: 0.,
        }
    }
}
pub struct LofiColour {
    sr: f32,
    seed: u32,
    controls: [Smooth; 8],
    channels: [Channel; 2],
    phase: [f32; 3],
    envelope: f32,
    release: f32,
    fresh: bool,
}
impl LofiColour {
    pub fn new(sr: f32) -> Self {
        Self::with_seed(sr, 0x7139_ab45)
    }
    pub fn with_seed(sr: f32, seed: u32) -> Self {
        let sr = if sr.is_finite() {
            sr.clamp(22050., 192000.)
        } else {
            48000.
        };
        Self {
            sr,
            seed,
            controls: SPEC.map(|s| Smooth::new(s.2, 10., sr)),
            channels: [
                Channel::new(sr, seed ^ 0x9123ab71),
                Channel::new(sr, seed ^ 0x2347cd19),
            ],
            phase: [0.; 3],
            envelope: 0.,
            release: libm::expf(-1. / (sr * 0.05)),
            fresh: true,
        }
    }
}
impl Device for LofiColour {
    fn set_param(&mut self, index: u32, value: f32) {
        if let Some(v) = clamp_param(&SPEC, index, value) {
            let s = &mut self.controls[index as usize];
            s.set(v);
            if self.fresh {
                s.snap();
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
        for (l, r) in l.iter_mut().zip(r) {
            let p: [f32; 8] = std::array::from_fn(|i| self.controls[i].next());
            let dry = [sane(*l), sane(*r)];
            self.envelope = tiny(
                dry[0]
                    .abs()
                    .max(dry[1].abs())
                    .min(1.)
                    .max(self.envelope * self.release),
            );
            let wave = |p: f32| libm::sinf(p * std::f32::consts::TAU);
            let delay =
                self.sr * (0.003 + 0.002 * wave(self.phase[0]) + 0.0003 * wave(self.phase[1]));
            let dropout = (1. - (wave(self.phase[0]).max(0.8) - 0.8) * 5. * p[6]).clamp(0., 1.);
            let hum = wave(self.phase[2]);
            let mut output = [0.; 2];
            for (i, ch) in self.channels.iter_mut().enumerate() {
                let x = dry[i].clamp(-16., 16.);
                let delayed = ch.motion.read_frac(delay);
                ch.motion.push(x);
                let mut y = x + p[2] * (delayed - x);
                let driven = fast_tanh(y * (1. + 12. * p[3]));
                y += p[3] * (driven - y);
                if ch.clock <= 0. {
                    ch.held = libm::roundf(y * 32.) / 32.;
                    ch.clock += 1. + 15. * p[4];
                }
                ch.clock -= 1.;
                y += p[4] * (ch.held - y);
                let echo = ch
                    .space
                    .read_frac(self.sr * (if i == 0 { 0.073 } else { 0.097 }));
                ch.space.push(tiny(y + echo * 0.35));
                y += p[5] * 0.5 * echo;
                let n = ch.noise.bipolar();
                let vinyl = if n.abs() > 0.995 { n * 8. } else { n * 0.15 };
                let noise = if p[7] < 1. {
                    vinyl + (n - vinyl) * p[7]
                } else {
                    n + (hum - n) * (p[7] - 1.)
                };
                y = y * dropout + noise * p[1] * 0.025 * self.envelope;
                output[i] = if p[0] == 0. {
                    dry[i]
                } else {
                    dry[i] + p[0] * (y - dry[i])
                };
            }
            *l = output[0];
            *r = output[1];
            for (phase, hz) in self.phase.iter_mut().zip([0.7, 6.3, 50.]) {
                *phase += hz / self.sr;
                if *phase >= 1. {
                    *phase -= 1.;
                }
            }
        }
    }
    fn reset(&mut self) {
        for (ch, tag) in self.channels.iter_mut().zip([0x9123ab71, 0x2347cd19]) {
            ch.motion.clear();
            ch.space.clear();
            ch.noise = Rng((self.seed ^ tag).max(1));
            ch.held = 0.;
            ch.clock = 0.;
        }
        self.phase = [0.; 3];
        self.envelope = 0.;
        for s in &mut self.controls {
            s.snap();
        }
        self.fresh = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charged_recursive_state_clears() {
        let mut d = LofiColour::new(48000.);
        d.envelope = 1e-30;
        let mut l = [0.; 1];
        let mut r = l;
        d.process(&mut l, &mut r, None, &[]);
        assert_eq!(d.envelope, 0.);
        d.envelope = 0.8;
        d.channels[0].held = 0.7;
        d.channels[0].space.push(0.9);
        d.channels[0].motion.push(0.9);
        d.reset();
        assert_eq!(d.envelope, 0.);
        assert_eq!(d.channels[0].held, 0.);
        assert_eq!(d.channels[0].space.read_int(0), 0.);
        assert_eq!(d.channels[0].motion.read_int(0), 0.);
    }
    #[test]
    fn physical_rates_and_envelope() {
        for sr in [22050., 48000., 192000.] {
            let mut d = LofiColour::new(sr);
            let mut l = vec![0.; sr as usize];
            let mut r = l.clone();
            l[0] = 1.;
            d.process(&mut l, &mut r, None, &[]);
            assert!((d.phase[0] - 0.7).abs() < 0.002);
            assert!(d.phase[1] < 0.31 && d.phase[1] > 0.29);
            assert!(d.envelope < 3e-9);
        }
    }
}
