//! Original parallel acoustic networks. Fixed feedback delays keep size and model automation
//! outside the energy storage: changing read speed inside a feedback loop can pump its tail.
use crate::util::{DelayLine, Smooth, clamp_param, flush_output_subnormal, sane};
use crate::{Device, NoteEvent, db_to_gain};

pub const PARAM_IDS: [&str; 13] = [
    "algorithm",
    "colour",
    "predelay",
    "decay",
    "size",
    "diffusion",
    "modulation",
    "low_cut",
    "high_cut",
    "width",
    "mix",
    "output",
    "nonlinear_duration",
];
pub const SPEC: [(f32, f32, f32); 13] = [
    (0.0, 4.0, 0.0),
    (0.0, 2.0, 0.0),
    (0.0, 250.0, 15.0),
    (0.1, 20.0, 2.0),
    (0.0, 1.0, 0.5),
    (0.0, 1.0, 0.7),
    (0.0, 1.0, 0.25),
    (20.0, 1000.0, 80.0),
    (1000.0, 20000.0, 16000.0),
    (0.0, 2.0, 1.0),
    (0.0, 1.0, 0.25),
    (-24.0, 12.0, 0.0),
    (0.05, 1.0, 0.3),
];
pub const PRESETS: &[&str] = &[
    "Small Air",
    "Long Canopy",
    "Silver Field",
    "Stone Alcove",
    "Rising Cloud",
];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let p = match name {
        "Small Air" => [
            0., 0., 8., 0.7, 0.2, 0.6, 0.1, 100., 16000., 1., 0.3, 0., 0.2,
        ],
        "Long Canopy" => [
            1., 1., 35., 4., 0.9, 0.9, 0.5, 100., 12000., 1.3, 0.35, -2., 0.4,
        ],
        "Silver Field" => [
            2., 0., 12., 2.4, 0.65, 1., 0.35, 180., 18000., 1.1, 0.3, -1., 0.3,
        ],
        "Stone Alcove" => [
            3., 2., 20., 1.3, 0.4, 0.75, 0.15, 70., 9000., 0.9, 0.35, 0., 0.3,
        ],
        "Rising Cloud" => [
            4., 1., 5., 1., 0.7, 0.8, 0.4, 120., 14000., 1.4, 0.5, -2., 0.4,
        ],
        _ => return None,
    };
    Some(
        p.into_iter()
            .enumerate()
            .map(|(i, v)| (i as u32, v))
            .collect(),
    )
}
// Distinct delay distributions and orthogonal scattering topologies, in seconds.
const TIMES: [[f32; 4]; 4] = [
    [0.0173, 0.0239, 0.0311, 0.0377],
    [0.0431, 0.0593, 0.0719, 0.0893],
    [0.0137, 0.0199, 0.0293, 0.0419],
    [0.0299, 0.0371, 0.0473, 0.0539],
];
fn tiny(x: f32) -> f32 {
    if x.abs() < 1e-24 { 0.0 } else { x }
}
// Convex interpolation only; Hermite overshoot is inappropriate for passive transitions.
fn read(d: &DelayLine, n: f32) -> f32 {
    let i = n as usize;
    let t = n - i as f32;
    let [newer, older] = d.read_pair(i);
    newer * (1.0 - t) + older * t
}
struct Network {
    lines: [DelayLine; 4],
    lengths: [usize; 4],
    damping: [f32; 4],
    gain_key: Option<(u32, u32)>,
    gains: [f32; 4],
    #[cfg(test)]
    uncached: bool,
    #[cfg(test)]
    gain_updates: usize,
}
fn network_lengths(sr: f32, model: usize) -> [usize; 4] {
    TIMES[model].map(|t| (t * sr).round() as usize)
}

impl Network {
    fn new(sr: f32, model: usize) -> Self {
        let lengths = network_lengths(sr, model);
        Self {
            lines: std::array::from_fn(|i| DelayLine::new(lengths[i] + 2)),
            lengths,
            damping: [0.; 4],
            gain_key: None,
            gains: [0.; 4],
            #[cfg(test)]
            uncached: false,
            #[cfg(test)]
            gain_updates: 0,
        }
    }
    fn tick(
        &mut self,
        input: [f32; 2],
        model: usize,
        decay: f32,
        damping: f32,
        sr: f32,
    ) -> [f32; 2] {
        // Keep the original f32 expression: reassociation changes feedback render bits.
        let key = (decay.to_bits(), sr.to_bits());
        if self.gain_key != Some(key) {
            self.gains = self
                .lengths
                .map(|length| libm::powf(10., -3. * length as f32 / (decay * sr)));
            self.gain_key = Some(key);
            #[cfg(test)]
            {
                self.gain_updates += 1;
            }
        }
        let mut x = [0.; 4];
        for (i, v) in x.iter_mut().enumerate() {
            let delayed = self.lines[i].read_int(self.lengths[i] - 1);
            self.damping[i] = tiny((1.0 - damping) * delayed + damping * self.damping[i]);
            let gain = self.gains[i];
            #[cfg(test)]
            let gain = if self.uncached {
                libm::powf(10., -3. * self.lengths[i] as f32 / (decay * sr))
            } else {
                gain
            };
            *v = self.damping[i] * gain;
        }
        let y = match model {
            0 => {
                let sum = (x[0] + x[1] + x[2] + x[3]) * 0.5;
                x.map(|v| v - sum)
            }
            1 => [
                (x[0] + x[1] + x[2] + x[3]) * 0.5,
                (x[0] - x[1] + x[2] - x[3]) * 0.5,
                (x[0] + x[1] - x[2] - x[3]) * 0.5,
                (x[0] - x[1] - x[2] + x[3]) * 0.5,
            ],
            2 => [-x[3], x[0], -x[1], x[2]],
            _ => [x[1], -x[0], x[3], x[2]],
        };
        for (i, &v) in y.iter().enumerate() {
            self.lines[i].push(tiny(v + input[i % 2] * if i < 2 { 0.25 } else { -0.25 }));
        }
        [(x[0] + x[2]) * 0.5, (x[1] - x[3]) * 0.5]
    }
    fn clear(&mut self) {
        for d in &mut self.lines {
            d.clear();
        }
        self.damping = [0.; 4];
        self.gain_key = None;
    }
}
pub struct AlgorithmicReverb {
    sr: f32,
    controls: [Smooth; 13],
    pre: [DelayLine; 2],
    diffuse: [DelayLine; 2],
    networks: [Network; 4],
    nonlinear: [DelayLine; 2],
    wet: [DelayLine; 2],
    low: [f32; 2],
    high: [f32; 2],
    phase: f64,
    seed: u64,
    fresh: bool,
}
fn stereo_delays(sr: f32) -> [usize; 4] {
    [0.251, 0.041, 1.0, 0.09].map(|seconds| (sr * seconds) as usize + 2)
}

pub(crate) fn memory_payload(sr: f32) -> crate::insert_memory::Payload {
    let mut payload = crate::insert_memory::Payload::boxed::<AlgorithmicReverb>();
    for delay in stereo_delays(sr) {
        payload.delay_lines(delay, 2);
    }
    for model in 0..TIMES.len() {
        for length in network_lengths(sr, model) {
            payload.delay_lines(length + 2, 1);
        }
    }
    payload
}

impl AlgorithmicReverb {
    pub fn new(sample_rate: f32) -> Self {
        Self::with_seed(sample_rate, 0xA17E_2026)
    }
    /// Only the seed's low 16 bits set where the modulation starts (also on `reset`), so give a
    /// seed that is uniform in its low bits (random or hashed, never a counter or raw index).
    pub fn with_seed(sample_rate: f32, seed: u64) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(22050., 192000.)
        } else {
            44100.
        };
        let [pre, diffuse, nonlinear, wet] = stereo_delays(sr);
        Self {
            sr,
            controls: SPEC.map(|(_, _, v)| Smooth::new(v, 20., sr)),
            pre: std::array::from_fn(|_| DelayLine::new(pre)),
            diffuse: std::array::from_fn(|_| DelayLine::new(diffuse)),
            networks: std::array::from_fn(|i| Network::new(sr, i)),
            nonlinear: std::array::from_fn(|_| DelayLine::new(nonlinear)),
            wet: std::array::from_fn(|_| DelayLine::new(wet)),
            low: [0.; 2],
            high: [0.; 2],
            phase: (seed % 65536) as f64 / 65536.,
            seed,
            fresh: true,
        }
    }
}
impl Device for AlgorithmicReverb {
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
        for (l, r) in l.iter_mut().zip(r) {
            let p = self.controls.each_mut().map(Smooth::next);
            let dry = [sane(*l).clamp(-16., 16.), sane(*r).clamp(-16., 16.)];
            let mut input = [0.; 2];
            for c in 0..2 {
                self.pre[c].push(dry[c]);
                let v = read(&self.pre[c], p[2] * 0.001 * self.sr);
                self.diffuse[c].push(v);
                // Two passive reflection stages; size changes density without retiming stored feedback.
                input[c] = (1.0 - p[5] * 0.5) * v
                    + p[5]
                        * 0.25
                        * (read(&self.diffuse[c], (0.003 + p[4] * 0.016) * self.sr)
                            + read(&self.diffuse[1 - c], (0.007 + p[4] * 0.031) * self.sr));
            }
            let damping = 0.05 + p[1] * 0.4;
            let mut models = [[0.; 2]; 5];
            for (m, model) in models[..4].iter_mut().enumerate() {
                *model = self.networks[m].tick(input, m, p[3], damping, self.sr);
            }
            // Finite rising reflection cloud, with a polynomial closure. No feedback or input gate:
            // every onset receives the same duration, even while other tails overlap.
            for (c, &v) in input.iter().enumerate() {
                self.nonlinear[c].push(v);
            }
            for tap in 1..=24 {
                let t = tap as f32 / 25.;
                let envelope = t * t * (1.0 - t) * 0.7;
                for c in 0..2 {
                    models[4][c] += read(&self.nonlinear[(c + tap) % 2], t * p[12] * self.sr)
                        * envelope
                        * if tap % 3 == 0 { -1. } else { 1. };
                }
            }
            let a = (p[0] as usize).min(3);
            let t = p[0] - a as f32;
            let cutoff_lo = 1.0 - libm::expf(-std::f32::consts::TAU * p[7] / self.sr);
            let cutoff_hi =
                1.0 - libm::expf(-std::f32::consts::TAU * p[8].min(self.sr * 0.45) / self.sr);
            let mut out = [0.; 2];
            for c in 0..2 {
                let v = models[a][c] * (1.0 - t) + models[a + 1][c] * t;
                self.wet[c].push(v);
                let phase = self.phase + c as f64 * 0.37;
                let mod_delay =
                    (0.002 + 0.001 * libm::sin(phase * std::f64::consts::TAU) as f32) * self.sr;
                let v = read(&self.wet[c], p[4] * 0.08 * self.sr + p[6] * mod_delay);
                self.low[c] = tiny(self.low[c] + cutoff_lo * (v - self.low[c]));
                self.high[c] = tiny(self.high[c] + cutoff_hi * (v - self.low[c] - self.high[c]));
                out[c] = self.high[c];
            }
            self.phase += 0.31 / self.sr as f64;
            if self.phase >= 1. {
                self.phase -= 1.;
            }
            let mid = (out[0] + out[1]) * 0.5;
            let side = (out[0] - out[1]) * 0.5 * p[9];
            let gain = db_to_gain(p[11]);
            *l = flush_output_subnormal((dry[0] * (1. - p[10]) + (mid + side) * p[10]) * gain);
            *r = flush_output_subnormal((dry[1] * (1. - p[10]) + (mid - side) * p[10]) * gain);
        }
    }
    fn reset(&mut self) {
        for d in self
            .pre
            .iter_mut()
            .chain(&mut self.diffuse)
            .chain(&mut self.nonlinear)
            .chain(&mut self.wet)
        {
            d.clear();
        }
        for n in &mut self.networks {
            n.clear();
        }
        self.low = [0.; 2];
        self.high = [0.; 2];
        self.phase = (self.seed % 65536) as f64 / 65536.;
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
    fn longest_linear_and_network_taps_fit_at_native_rate_limits() {
        for sr in [22_050.0, 192_000.0] {
            let mut reverb = AlgorithmicReverb::new(sr);
            for index in [2, 4, 5, 6, 12] {
                reverb.set_param(index as u32, SPEC[index].1);
            }
            // Each sample reads all networks. Peak the wet modulation on each channel in turn.
            for phase in [0.25, 1.25 - 0.37] {
                reverb.phase = phase;
                reverb.process(&mut [0.25], &mut [-0.25], None, &[]);
            }
        }
    }

    #[test]
    fn settled_gains_reuse_and_every_exact_key_change_recomputes() {
        let mut n = Network::new(48000., 0);
        for _ in 0..128 {
            n.tick([0.; 2], 0, 2., 0.5, 48000.);
        }
        assert_eq!(n.gain_updates, 1);
        let adjacent = f32::from_bits(2_f32.to_bits() + 1);
        n.tick([0.; 2], 0, adjacent, 0.5, 48000.);
        assert_eq!(n.gain_updates, 2);
        n.tick([0.; 2], 0, adjacent, 0.5, 96000.);
        assert_eq!(n.gain_updates, 3);
        n.clear();
        n.tick([0.; 2], 0, adjacent, 0.5, 96000.);
        assert_eq!(n.gain_updates, 4);
    }

    #[test]
    fn cache_matches_original_kernel_bits_across_blocks_automation_and_reset() {
        for sr in [22050., 44100., 48000., 96000., 192000.] {
            for block in [1, 7, 64, 127, 128] {
                let mut cached = AlgorithmicReverb::with_seed(sr, 937);
                let mut original = AlgorithmicReverb::with_seed(sr, 937);
                for n in &mut original.networks {
                    n.uncached = true;
                }
                for d in [&mut cached, &mut original] {
                    d.set_param(2, 0.);
                    d.set_param(10, 1.);
                }
                // Boundaries are independent of host block size, including a reset mid-ramp.
                for (segment, frames) in [4096, 3072, 2048, 4096, 2048].into_iter().enumerate() {
                    for d in [&mut cached, &mut original] {
                        match segment {
                            1 => d.set_param(3, 20.),
                            2 => d.reset(),
                            3 => {
                                d.set_param(3, 0.1);
                                d.set_param(0, 3.);
                            }
                            4 => {
                                d.set_param(3, f32::NAN);
                                d.set_param(3, f32::INFINITY);
                            }
                            _ => (),
                        }
                    }
                    let mut offset = 0;
                    while offset < frames {
                        let count = block.min(frames - offset);
                        let mut l: Vec<_> = (offset..offset + count)
                            .map(|i| libm::sinf(i as f32 * 0.071) * 0.3)
                            .collect();
                        let mut r: Vec<_> = (offset..offset + count)
                            .map(|i| libm::cosf(i as f32 * 0.037) * 0.2)
                            .collect();
                        let (mut x, mut y) = (l.clone(), r.clone());
                        cached.process(&mut l, &mut r, None, &[]);
                        original.process(&mut x, &mut y, None, &[]);
                        assert_eq!(
                            l.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                            x.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                            "left sr={sr} block={block} segment={segment} offset={offset}"
                        );
                        assert_eq!(
                            r.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                            y.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                            "right sr={sr} block={block} segment={segment} offset={offset}"
                        );
                        offset += count;
                    }
                }
            }
        }
    }

    #[test]
    fn charged_recursive_state_clears_without_subnormals() {
        for sr in [22050., 48000., 192000.] {
            for model in 0..4 {
                let mut network = Network::new(sr, model);
                let mut charged = false;
                for frame in 0..(sr * 3.) as usize {
                    network.tick(
                        if frame == 0 { [1., 0.3] } else { [0., 0.] },
                        model,
                        0.1,
                        0.85,
                        sr,
                    );
                    charged |= network.damping.iter().any(|x| x.abs() > 1e-6);
                    assert!(
                        network.damping.iter().all(|x| *x == 0. || x.is_normal()),
                        "network {model} at {sr}: subnormal recursive state"
                    );
                }
                assert!(charged, "network {model} at {sr} was never excited");
                assert_eq!(network.damping, [0.; 4], "network {model} at {sr}");
                for (line, length) in network.lines.iter().zip(network.lengths) {
                    assert!((0..length).all(|i| line.read_int(i) == 0.));
                }
            }
            let mut device = AlgorithmicReverb::new(sr);
            for (index, value) in [(1, 2.), (2, 0.), (3, 0.1), (4, 0.), (6, 0.), (10, 1.)] {
                device.set_param(index, value);
            }
            let mut filters_charged = false;
            for frame in 0..(sr * 3.) as usize {
                let (mut l, mut r) = if frame == 0 {
                    ([1.], [0.3])
                } else {
                    ([0.], [0.])
                };
                device.process(&mut l, &mut r, None, &[]);
                filters_charged |= device
                    .low
                    .iter()
                    .chain(&device.high)
                    .any(|x| x.abs() > 1e-6);
                assert!(
                    device
                        .low
                        .iter()
                        .chain(&device.high)
                        .all(|x| *x == 0. || x.is_normal())
                );
            }
            assert!(filters_charged, "output filters at {sr} were never excited");
            assert_eq!((device.low, device.high), ([0.; 2], [0.; 2]));
        }
    }
    #[test]
    fn each_control_matches_an_independent_sample_rate_ramp_on_charged_audio() {
        for index in 0..SPEC.len() {
            let mut a = AlgorithmicReverb::new(22050.);
            let mut b = AlgorithmicReverb::new(22050.);
            for d in [&mut a, &mut b] {
                d.set_param(0, if index == 12 { 4. } else { 1. });
                d.set_param(10, 0.6);
                d.set_param(index as u32, SPEC[index].0);
            }
            for n in 0..22050 {
                for d in [&mut a, &mut b] {
                    let mut l = [libm::sinf(n as f32 * 0.071) * 0.3];
                    let mut r = [libm::cosf(n as f32 * 0.037) * 0.2];
                    d.process(&mut l, &mut r, None, &[]);
                }
            }
            let mut expected = SPEC[index].0;
            let target = SPEC[index].1;
            a.set_param(index as u32, target);
            let coefficient = libm::expf(-1.0 / (0.020 * 22050.));
            for n in 0..18000 {
                let distance = (expected - target) * coefficient;
                expected = if distance.abs() < 1e-5 * (1. + target.abs()) {
                    target
                } else {
                    target + distance
                };
                // Feed the independently computed trajectory into the reference kernel.
                // No smoother remains active for this control in the reference.
                b.controls[index].cur = expected;
                b.controls[index].target = expected;
                let mut l = [libm::sinf(n as f32 * 0.071) * 0.3];
                let mut r = [libm::cosf(n as f32 * 0.037) * 0.2];
                let mut x = l;
                let mut y = r;
                a.process(&mut l, &mut r, None, &[]);
                b.process(&mut x, &mut y, None, &[]);
                assert!(
                    (l[0] - x[0]).abs() < 2e-5 && (r[0] - y[0]).abs() < 2e-5,
                    "control {index}, sample {n}: {l:?} vs {x:?}"
                );
            }
        }
    }
    // Isolate wet-delay modulation from input diffusion and size delay.
    fn charged_wet(phase: f64) -> AlgorithmicReverb {
        let mut d = AlgorithmicReverb::with_seed(48000., 16384);
        for (i, v) in [(4, 0.), (5, 0.), (6, 1.), (9, 1.), (10, 1.)] {
            d.set_param(i, v);
        }
        d.phase = phase;
        d
    }

    #[test]
    fn network_output_channels_keep_their_signed_taps() {
        for model in 0..4 {
            let mut n = Network::new(48000., model);
            // With decay effectively infinite and damping zero, the four
            // independently charged taps must project to +2 and -1.
            for (line, value) in n.lines.iter_mut().zip([1., 2., 3., 4.]) {
                for _ in 0..5000 {
                    line.push(value);
                }
            }
            let out = n.tick([0.; 2], model, 1e20, 0., 48000.);
            assert!(
                (out[0] - 2.).abs() < 1e-6 && (out[1] + 1.).abs() < 1e-6,
                "{out:?}"
            );
        }
    }

    #[test]
    fn moving_stereo_modulation_reads_independent_delay_coordinates() {
        for phase in [0.0, 0.23, 0.61] {
            let mut d = charged_wet(phase);
            // A linear history turns a delay coordinate into a known value.
            for line in &mut d.wet {
                for i in 0..5000 {
                    line.push(i as f32 * 0.0001);
                }
            }
            let mut low_state = [0.; 2];
            let mut high_state = [0.; 2];
            for frame in 0..200 {
                let expected_phase = phase + frame as f64 * 0.31 / 48000.;
                let mut expected = [0.; 2];
                for (c, value) in expected.iter_mut().enumerate() {
                    let delay = (0.002
                        + 0.001
                            * libm::sin((expected_phase + c as f64 * 0.37) * std::f64::consts::TAU)
                                as f32)
                        * 48000.;
                    // At frame 0 push adds a zero; old sample 4999 is one sample back.
                    let sample = |offset: usize| {
                        let index = 5000 + frame - offset;
                        if index < 5000 {
                            index as f32 * 0.0001
                        } else {
                            0.
                        }
                    };
                    let whole = delay as usize;
                    let fraction = delay - whole as f32;
                    let v = sample(whole) * (1. - fraction) + sample(whole + 1) * fraction;
                    let lo = 1. - libm::expf(-std::f32::consts::TAU * 80. / 48000.);
                    let hi = 1. - libm::expf(-std::f32::consts::TAU * 16000. / 48000.);
                    low_state[c] += lo * (v - low_state[c]);
                    high_state[c] += hi * (v - low_state[c] - high_state[c]);
                    *value = high_state[c];
                }
                let mut l = [0.];
                let mut r = [0.];
                d.process(&mut l, &mut r, None, &[]);
                assert!(
                    (l[0] - expected[0]).abs() < 2e-7 && (r[0] - expected[1]).abs() < 2e-7,
                    "phase {phase} frame {frame}: {l:?}/{r:?} expected {expected:?}"
                );
            }
        }
    }

    #[test]
    fn separated_seed_phases_replay_after_reset() {
        let render = |d: &mut AlgorithmicReverb| {
            let mut l = vec![0.; 12000];
            let mut r = l.clone();
            l[0] = 1.;
            r[0] = 0.37;
            d.process(&mut l, &mut r, None, &[]);
            l.extend(r);
            l
        };
        let mut outputs = Vec::new();
        for seed in [0, 16384, 49152] {
            let mut d = AlgorithmicReverb::with_seed(22050., seed);
            d.set_param(6, 1.);
            d.set_param(10, 1.);
            let first = render(&mut d);
            d.reset();
            assert_eq!(first, render(&mut d));
            outputs.push(first);
        }
        for pair in outputs.windows(2) {
            let delta: f64 = pair[0]
                .iter()
                .zip(&pair[1])
                .map(|(&a, &b)| {
                    let delta = f64::from(a - b);
                    delta * delta
                })
                .sum();
            assert!(
                delta > 1e-5,
                "separated seeds lost modulation phase: {delta}"
            );
        }
    }

    #[test]
    fn nonlinear_signed_alternating_reflections_and_diffusion_crossfeed() {
        // 25 kHz and 100 ms make tap positions integer multiples of 100
        // samples. The left impulse diffuses at 75 samples (same channel)
        // and 175 samples (opposite channel), before the finite tap cloud.
        for diffusion in [0., 1.] {
            let mut d = AlgorithmicReverb::new(25000.);
            for (i, v) in [
                (0, 4.),
                (2, 0.),
                (4, 0.),
                (5, diffusion),
                (6, 0.),
                (7, 20.),
                (8, 10000.),
                (10, 1.),
                (12, 0.1),
            ] {
                d.set_param(i, v);
            }
            let mut expected = vec![[0f64; 2]; 3000];
            for tap in 1..=24 {
                let t = tap as f64 / 25.;
                let amplitude = 0.7 * t * t * (1. - t) * if tap % 3 == 0 { -1. } else { 1. };
                let channel = tap % 2;
                expected[tap * 100][channel] += amplitude * (1. - f64::from(diffusion) * 0.5);
                expected[tap * 100 + 75][channel] += amplitude * f64::from(diffusion) * 0.25;
                expected[tap * 100 + 175][1 - channel] += amplitude * f64::from(diffusion) * 0.25;
            }
            let lo = 1. - libm::exp(-std::f64::consts::TAU * 20. / 25000.);
            let hi = 1. - libm::exp(-std::f64::consts::TAU * 10000. / 25000.);
            let mut low = [0.; 2];
            let mut high = [0.; 2];
            for (frame, raw) in expected.iter().enumerate() {
                for c in 0..2 {
                    low[c] += lo * (raw[c] - low[c]);
                    high[c] += hi * (raw[c] - low[c] - high[c]);
                }
                let mut l = [if frame == 0 { 1. } else { 0. }];
                let mut r = [0.];
                d.process(&mut l, &mut r, None, &[]);
                assert!(
                    (f64::from(l[0]) - high[0]).abs() < 2e-5
                        && (f64::from(r[0]) - high[1]).abs() < 2e-5,
                    "diffusion {diffusion} frame {frame}: {l:?}/{r:?} expected {high:?}"
                );
            }
        }
    }
}
