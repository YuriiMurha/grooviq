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

/// Stable parameter ids in `SPEC` order.
pub const PARAM_IDS: [&str; 8] = [
    "magnitude",
    "noise",
    "wow_flutter",
    "distortion",
    "digital",
    "space",
    "dropouts",
    "noise_selection",
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
fn channel_delays(sr: f32) -> [usize; 2] {
    [(sr * 0.012) as usize + 8, (sr * 0.12) as usize + 8]
}

pub(crate) fn memory_payload(sr: f32) -> crate::insert_memory::Payload {
    let mut payload = crate::insert_memory::Payload::boxed::<LofiColour>();
    for delay in channel_delays(sr) {
        payload.delay_lines(delay, 2);
    }
    payload
}

impl Channel {
    fn new(sr: f32, seed: u32) -> Self {
        let [motion, space] = channel_delays(sr);
        Self {
            motion: DelayLine::new(motion),
            space: DelayLine::new(space),
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
    fn longest_motion_and_space_taps_fit_at_native_rate_limits() {
        for sr in [22_050.0, 192_000.0] {
            let mut device = LofiColour::new(sr);
            for index in [0, 2, 5] {
                device.set_param(index as u32, SPEC[index].1);
            }
            device.phase = [0.25, 0.25, 0.0];
            tick(&mut device, 0.25);
        }
    }

    fn tick(device: &mut LofiColour, value: f32) -> [f32; 2] {
        let mut l = [value];
        let mut r = l;
        device.process(&mut l, &mut r, None, &[]);
        [l[0], r[0]]
    }
    #[test]
    fn every_live_control_follows_ten_millisecond_ramps_and_reset_snaps() {
        for sr in [22050., 44100., 48000., 96000., 192000.] {
            for (index, spec) in SPEC.iter().enumerate() {
                let mut device = LofiColour::new(sr);
                tick(&mut device, 0.);
                for target in [spec.1, spec.0] {
                    let start = device.controls[index].cur;
                    device.set_param(index as u32, target);
                    assert_eq!(
                        device.controls[index].cur, start,
                        "live snap {index} at {sr}"
                    );
                    let tau = (sr * 0.01) as usize;
                    for frame in 1..=tau * 3 {
                        tick(&mut device, 0.);
                        if [1, tau / 4, tau, tau * 3].contains(&frame) {
                            let expected = target
                                + (start - target) * libm::expf(-(frame as f32) / (sr * 0.01));
                            assert!(
                                (device.controls[index].cur - expected).abs() < 0.0003,
                                "control {index}, {sr} Hz, frame {frame}: {} vs {expected}",
                                device.controls[index].cur
                            );
                        }
                    }
                }
                device.set_param(index as u32, spec.1);
                tick(&mut device, 0.);
                assert!(device.controls[index].cur < spec.1);
                device.reset();
                assert_eq!(device.controls[index].cur, spec.1);
                device.set_param(index as u32, spec.0);
                assert_eq!(
                    device.controls[index].cur, spec.0,
                    "reset must re-arm fresh"
                );
            }
        }
    }
    #[test]
    fn dropout_crest_depth_and_unaffected_phase_are_physical() {
        for sr in [22050., 48000., 192000.] {
            for amount in [0., 0.5, 1.] {
                let mut device = LofiColour::new(sr);
                device.set_param(0, 1.);
                device.set_param(6, amount);
                device.phase[0] = 0.25;
                let out = tick(&mut device, 0.5);
                for value in out {
                    assert!((value - 0.5 * (1. - amount)).abs() < 1e-6);
                }
            }
            let mut device = LofiColour::new(sr);
            device.set_param(0, 1.);
            device.set_param(6, 1.);
            assert_eq!(tick(&mut device, 0.5), [0.5; 2]);
        }
    }
    #[test]
    fn tiny_space_writes_clear_but_legitimate_quiet_material_survives() {
        for sr in [22050., 48000., 192000.] {
            for (input, stored) in [(1e-30, 0.), (1e-12, 1e-12)] {
                let mut device = LofiColour::new(sr);
                device.set_param(0, 1.);
                device.set_param(5, 1.);
                assert_eq!(tick(&mut device, input), [input; 2]);
                for channel in &device.channels {
                    assert_eq!(channel.space.read_int(0), stored);
                }
            }
        }
    }
    #[test]
    fn charged_space_tail_reaches_exact_zero_without_reset() {
        for sr in [22050., 48000., 192000.] {
            let mut device = LofiColour::new(sr);
            device.set_param(0, 1.);
            device.set_param(5, 1.);
            tick(&mut device, 0.5);
            assert!(device.channels.iter().all(|c| c.space.read_int(0) == 0.5));
            let mut left = [0.; 128];
            let mut right = left;
            for _ in 0..((sr * 8.) as usize).div_ceil(128) {
                left.fill(0.);
                right.fill(0.);
                device.process(&mut left, &mut right, None, &[]);
            }
            assert!(left.iter().chain(&right).all(|x| *x == 0.), "tail at {sr}");
            for channel in &device.channels {
                assert!(
                    (0..=channel.space.max_delay() + 3)
                        .all(|offset| channel.space.read_int(offset) == 0.),
                    "charged delay state at {sr}"
                );
            }
        }
    }
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
