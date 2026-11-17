//! Original sample-and-hold crusher. Filter blends two cascaded low-pass poles before
//! sampling and after quantization; it reduces aliases/images, not an alias-free promise.
use crate::util::{OnePole, Rng, Smooth, clamp_param, onepole_gain, sane};
use crate::{Device, NoteEvent};

/// Bits, hold period (host samples), filtering, jitter (fraction of period),
/// triangular dither (LSBs), dry/wet. Continuous bits interpolate the step size.
pub const SPEC: [(f32, f32, f32); 6] = [
    (2.0, 24.0, 12.0),
    (1.0, 128.0, 4.0),
    (0.0, 1.0, 1.0),
    (0.0, 0.45, 0.0),
    (0.0, 1.0, 0.0),
    (0.0, 1.0, 1.0),
];
pub const PRESETS: [&str; 3] = ["Clean Steps", "Filtered Dust", "Loose Grain"];
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let values = match name {
        "Init" | "Default" => SPEC.map(|s| s.2),
        "Clean Steps" => [6.0, 4.0, 0.0, 0.0, 0.0, 1.0],
        "Filtered Dust" => [8.0, 8.0, 1.0, 0.0, 0.5, 1.0],
        "Loose Grain" => [4.0, 12.0, 0.5, 0.4, 1.0, 0.8],
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
struct Channel {
    pre: [OnePole; 2],
    post: [OnePole; 2],
    remaining: f32,
    held: f32,
    jitter: Rng,
    dither: Rng,
}
// Avalanche each stream tag; xorshift32 must never start at zero.
fn stream_seed(seed: u32, tag: u32) -> u32 {
    let mut x = seed.wrapping_add(tag);
    x = (x ^ (x >> 16)).wrapping_mul(0x7feb_352d);
    x = (x ^ (x >> 15)).wrapping_mul(0x846c_a68b);
    (x ^ (x >> 16)).max(1)
}
impl Channel {
    fn new(seed: u32, tag: u32) -> Self {
        Self {
            pre: [OnePole::default(); 2],
            post: [OnePole::default(); 2],
            remaining: 0.0,
            held: 0.0,
            jitter: Rng(stream_seed(seed, tag)),
            dither: Rng(stream_seed(seed, tag ^ 0xa511_e9b3)),
        }
    }
}
pub struct Bitcrusher {
    sr: f32,
    seed: u32,
    controls: [Smooth; 6],
    channels: [Channel; 2],
    fresh: bool,
}
impl Bitcrusher {
    /// Fixed default seed (0x1234_abcd) for repeatable fixtures. Hosts can supply
    /// a stable per-slot seed with [`Self::with_seed`].
    pub fn new(sample_rate: f32) -> Self {
        Self::with_seed(sample_rate, 0x1234_abcd)
    }

    /// Independent deterministic channel/purpose streams, including for seed zero.
    pub fn with_seed(sample_rate: f32, seed: u32) -> Self {
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(22050.0, 192000.0)
        } else {
            48000.0
        };
        Self {
            sr,
            seed,
            controls: SPEC.map(|s| Smooth::new(s.2, 10.0, sr)),
            channels: [
                Channel::new(seed, 0x1357_9bdf),
                Channel::new(seed, 0x2468_ace0),
            ],
            fresh: true,
        }
    }
}
impl Device for Bitcrusher {
    fn set_param(&mut self, index: u32, value: f32) {
        if let Some(v) = clamp_param(&SPEC, index, value) {
            let s = &mut self.controls[index as usize];
            let shortening = index == 1 && v < s.target;
            s.set(v);
            if shortening {
                // Shortening bounds the outstanding wait without clearing the held sample
                // or discarding negative fractional overshoot. Future holds stay smoothed.
                for ch in &mut self.channels {
                    ch.remaining = ch.remaining.min(v);
                }
            }
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
        for (left, right) in l.iter_mut().zip(r) {
            let [bits, period, filter, jitter, dither, mix] =
                std::array::from_fn(|i| self.controls[i].next());
            let scale = libm::exp2f(bits - 1.0);
            let g = onepole_gain(0.4 * self.sr / period, self.sr);
            for (ch, x) in self.channels.iter_mut().zip([left, right]) {
                let dry = sane(*x).clamp(-16.0, 16.0);
                let a = ch.pre[0].lp(dry, g);
                let b = ch.pre[1].lp(a, g);
                // TPT state flushing can leave subaudible residue. Once a silent
                // input's prefilter tail is below -240 dBFS, treat it as digital zero
                // so dither cannot turn that residue into a permanent noise floor.
                let input = if dry == 0.0 && b.abs() < 1.0e-12 {
                    0.0
                } else {
                    dry + filter * (b - dry)
                };
                if ch.remaining <= 0.0 {
                    // Advance even on silence so gating cannot shift the future noise stream.
                    let draw = ch.dither.unit() - ch.dither.unit();
                    let noise = if input == 0.0 { 0.0 } else { draw * dither };
                    ch.held = libm::roundf(input.clamp(-1.0, 1.0) * scale + noise)
                        .clamp(-scale, scale - 1.0)
                        / scale;
                    // Overshoot carries forward: fractional periods do not drift with blocks.
                    ch.remaining += (period * (1.0 + jitter * ch.jitter.bipolar())).max(1.0);
                }
                ch.remaining -= 1.0;
                let a = ch.post[0].lp(ch.held, g);
                let b = ch.post[1].lp(a, g);
                let wet = if ch.held == 0.0 && b.abs() < 1.0e-12 {
                    0.0
                } else {
                    ch.held + filter * (b - ch.held)
                };
                *x = dry + mix * (wet - dry);
            }
        }
    }
    fn reset(&mut self) {
        self.channels = [
            Channel::new(self.seed, 0x1357_9bdf),
            Channel::new(self.seed, 0x2468_ace0),
        ];
        for s in &mut self.controls {
            s.snap();
        }
        self.fresh = true;
    }
}
