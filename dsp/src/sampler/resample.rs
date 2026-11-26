use wide::f32x4;

use super::{SampleBuf, Voice};

include!(concat!(env!("OUT_DIR"), "/sampler-fir.rs"));

type Band<const N: usize> = [[f32; N]; PHASES + 1];

const SHORT_BANDS: usize = BANDS + 1 - JOIN_BAND;
static FIR: [Band<TAPS>; SHORT_BANDS] = read_bands::<TAPS, SHORT_BANDS>(0);
static FIR_WIDE: [Band<WIDE_TAPS>; JOIN_BAND + 1] =
    read_bands::<WIDE_TAPS, { JOIN_BAND + 1 }>(size_of::<[Band<TAPS>; SHORT_BANDS]>());
static FIR_EXTREME: [Band<EXTREME_TAPS>; EXTREME_JOIN_BAND + 1] =
    read_bands::<EXTREME_TAPS, { EXTREME_JOIN_BAND + 1 }>(
        size_of::<[Band<TAPS>; SHORT_BANDS]>() + size_of::<[Band<WIDE_TAPS>; JOIN_BAND + 1]>(),
    );

const fn read_bands<const N: usize, const COUNT: usize>(mut offset: usize) -> [Band<N>; COUNT] {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/sampler-fir.bin"));
    let mut table = [[[0.0; N]; PHASES + 1]; COUNT];
    let mut band = 0;
    while band < COUNT {
        let mut phase = 0;
        while phase <= PHASES {
            let mut tap = 0;
            while tap < N {
                table[band][phase][tap] = f32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ]);
                tap += 1;
                offset += 4;
            }
            phase += 1;
        }
        band += 1;
    }
    table
}

/// Keep detune and 48 kHz sources at 44.1 kHz on the exact cubic path. Fade to the FIR as
/// the step increases so a glide does not abruptly change the interpolator's gain.
const FADE_START: f64 = 1.09;
const FADE_END: f64 = 1.25;

#[inline(always)]
pub(super) fn read(sample: &SampleBuf, voice: &Voice) -> (f32, f32) {
    let step = voice.step.abs();
    if step <= FADE_START {
        cubic(sample, voice)
    } else {
        band_limited(sample, voice, step)
    }
}

/// Share the phase in stereo and check a contiguous four-frame window once per channel.
#[inline(always)]
fn cubic(sample: &SampleBuf, voice: &Voice) -> (f32, f32) {
    let i = voice.pos.floor() as i64;
    let t = (voice.pos - i as f64) as f32;
    let first = (i - 1) as usize;
    if !voice.looping
        && let Some(l) = sample.l.get(first..).and_then(<[f32]>::first_chunk::<4>)
        && let Some(r) = sample.r.get(first..).and_then(<[f32]>::first_chunk::<4>)
    {
        (polynomial(*l, t), polynomial(*r, t))
    } else {
        cubic_edges(sample, voice, i, t)
    }
}

#[inline(never)]
fn cubic_edges(sample: &SampleBuf, voice: &Voice, i: i64, t: f32) -> (f32, f32) {
    let taps = [i - 1, i, i + 1, i + 2];
    let taps = if voice.looping {
        taps.map(|k| voice.loop_index(k))
    } else {
        taps
    };
    let channel =
        |x: &[f32]| polynomial(taps.map(|k| x.get(k as usize).copied().unwrap_or(0.0)), t);
    (channel(&sample.l), channel(&sample.r))
}

#[inline(always)]
fn polynomial([y0, y1, y2, y3]: [f32; 4], t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

#[inline(never)]
fn band_limited(sample: &SampleBuf, voice: &Voice, step: f64) -> (f32, f32) {
    let (sl, sr) = sinc(sample, voice, step);
    if step >= FADE_END {
        return (sl, sr);
    }
    let (l, r) = cubic(sample, voice);
    let mix = ((step - FADE_START) / (FADE_END - FADE_START)) as f32;
    (l + mix * (sl - l), r + mix * (sr - r))
}

#[inline(never)]
fn sinc(sample: &SampleBuf, voice: &Voice, step: f64) -> (f32, f32) {
    // The band index follows destination Nyquist. The narrow bands use wider kernels
    // with a transition closer to Nyquist than this nominal cutoff.
    let nyquist = 0.5 / step;
    let cutoff = nyquist - 0.025_f64.min(nyquist * 0.25);
    let band = cutoff * (2 * BANDS) as f64;
    let band_index = band as usize;
    let band_mix = f32x4::splat((band - band_index as f64) as f32);
    if band_index < JOIN_BAND {
        // The shared row is wide band 6, at step 8. Keep that row on the old path.
        if band < EXTREME_JOIN_BAND as f64 / EXTREME_SUBDIVISIONS as f64 {
            sinc_extreme(sample, voice, band)
        } else {
            convolve(sample, voice, &FIR_WIDE, band_index, band_mix)
        }
    } else {
        convolve(sample, voice, &FIR, band_index - JOIN_BAND, band_mix)
    }
}

#[inline(never)]
fn sinc_extreme(sample: &SampleBuf, voice: &Voice, band: f64) -> (f32, f32) {
    let band = band * EXTREME_SUBDIVISIONS as f64;
    let index = band as usize;
    let mix = f32x4::splat((band - index as f64) as f32);
    convolve(sample, voice, &FIR_EXTREME, index, mix)
}

#[inline(always)]
fn convolve<const N: usize>(
    sample: &SampleBuf,
    voice: &Voice,
    fir: &[Band<N>],
    band_index: usize,
    band_mix: f32x4,
) -> (f32, f32) {
    let pos = voice.pos.floor();
    let first = pos as i64 - (N / 2 - 1) as i64;
    let phase = (voice.pos - pos) * PHASES as f64;
    let phase_index = phase as usize;
    let phase_mix = f32x4::splat((phase - phase_index as f64) as f32);
    let kernels = [
        &fir[band_index][phase_index],
        &fir[band_index][phase_index + 1],
        &fir[band_index + 1][phase_index],
        &fir[band_index + 1][phase_index + 1],
    ];

    let mut edge_l = [0.0; N];
    let mut edge_r = [0.0; N];
    let first_f = first as f64;
    let last_f = first_f + (N - 1) as f64;
    let contiguous = first_f >= voice.start
        && last_f < voice.end
        && (!voice.looping || (first_f >= voice.lo && last_f < voice.hi));
    let (l, r) = if contiguous
        && let first = first as usize
        && let Some(l) = sample.l.get(first..first + N)
        && let Some(r) = sample.r.get(first..first + N)
    {
        (l, r)
    } else if N == EXTREME_TAPS
        && gather_wrapped_loop(sample, voice, first, &mut edge_l, &mut edge_r)
    {
        (&edge_l[..], &edge_r[..])
    } else {
        let lo = voice.start.ceil() as i64;
        let hi = voice.end.ceil() as i64 - 1;
        for i in 0..N {
            let k = first + i as i64;
            let k = if voice.looping {
                voice.loop_index(k)
            } else {
                k
            };
            // Extend a trim edge with its outermost frame. The wider filter must not pull
            // in material beyond the selected region or fade a constant signal at its edges.
            let k = k.clamp(lo, hi) as usize;
            edge_l[i] = sample.l.get(k).copied().unwrap_or(0.0);
            edge_r[i] = sample.r.get(k).copied().unwrap_or(0.0);
        }
        (&edge_l[..], &edge_r[..])
    };

    let [a, b, c, d] = kernels.map(|weights| weights.as_chunks::<4>().0);
    let (mut sl, mut sr) = (f32x4::ZERO, f32x4::ZERO);
    for (i, (l, r)) in l
        .as_chunks::<4>()
        .0
        .iter()
        .zip(r.as_chunks::<4>().0)
        .enumerate()
    {
        let (a, b, c, d) = (
            f32x4::new(a[i]),
            f32x4::new(b[i]),
            f32x4::new(c[i]),
            f32x4::new(d[i]),
        );
        let low = a + phase_mix * (b - a);
        let high = c + phase_mix * (d - c);
        let weight = low + band_mix * (high - low);
        sl += weight * f32x4::new(*l);
        sr += weight * f32x4::new(*r);
    }
    // Explicit lane reduction gives both platforms the same addition order.
    let sum = |x: f32x4| {
        let [a, b, c, d] = x.to_array();
        (a + b) + (c + d)
    };
    (sum(sl), sum(sr))
}

#[inline(never)]
fn gather_wrapped_loop<const N: usize>(
    sample: &SampleBuf,
    voice: &Voice,
    first: i64,
    edge_l: &mut [f32; N],
    edge_r: &mut [f32; N],
) -> bool {
    if !voice.looping || !voice.wrapped {
        return false;
    }
    let Voice {
        start,
        end,
        lo,
        hi,
        len,
        pos,
        ..
    } = *voice;
    // Keep taps and loop bounds exactly representable as f64, as in loop_index.
    let max_end = ((1_i64 << 53) - N as i64) as f64;
    if !(0.0 <= start && start <= lo && lo < hi && hi <= end && end <= max_end)
        || !(lo..hi).contains(&pos)
        || end > sample.l.len().min(sample.r.len()) as f64
        || len != hi - lo
    {
        return false;
    }
    let fraction = lo.fract();
    if fraction != hi.fract() {
        return false;
    }
    if fraction != 0.0 {
        // Equal fractional endpoints give an integral period and a tap window
        // [ceil(lo), ceil(hi)). For a 256-tap read below 2^24, the magnitude of
        // k-lo and of the quotient scaled back by len is less than 2^25.
        // Subtraction and division together then shift a period boundary by
        // less than 2^-27 frames. Leave a larger margin on either side.
        const MIN_FRACTION: f64 = 1.0 / 1_048_576.0;
        if hi > (1_u32 << 24) as f64 || !(MIN_FRACTION..=1.0 - MIN_FRACTION).contains(&fraction) {
            return false;
        }
    }
    let lo = lo.ceil() as usize;
    let hi = hi.ceil() as usize;
    let (Some(left), Some(right)) = (sample.l.get(lo..hi), sample.r.get(lo..hi)) else {
        return false;
    };

    // After the first wrap, both ends map into the same loop period in either direction.
    // Keep the taps in ascending source-index order and restart each run at the loop start.
    let period = left.len();
    let mut offset = (first - lo as i64).rem_euclid(period as i64) as usize;
    // Seed one rotated period for short loops, then double the filled prefix
    // instead of copying each tiny run. Every copy still preserves tap order.
    let seed = if period <= N / 8 { period } else { N };
    let mut written = 0;
    while written < seed {
        let count = (period - offset).min(seed - written);
        edge_l[written..written + count].copy_from_slice(&left[offset..offset + count]);
        edge_r[written..written + count].copy_from_slice(&right[offset..offset + count]);
        written += count;
        offset = 0;
    }
    while written < N {
        let count = written.min(N - written);
        let (prefix_l, tail_l) = edge_l.split_at_mut(written);
        let (prefix_r, tail_r) = edge_r.split_at_mut(written);
        tail_l[..count].copy_from_slice(&prefix_l[..count]);
        tail_r[..count].copy_from_slice(&prefix_r[..count]);
        written += count;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::hermite;

    fn alternating_sample() -> SampleBuf {
        let l: Vec<f32> = (0..128)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let r = l.iter().map(|&x| x * -0.5).collect();
        SampleBuf {
            rate: 48_000.0,
            l,
            r,
        }
    }

    #[test]
    fn wrapped_extreme_gather_matches_each_mapped_stereo_tap() {
        let sample = SampleBuf {
            rate: 384_000.0,
            l: (0..512).map(|i| i as f32).collect(),
            r: (0..512).map(|i| -(i as f32 + 0.5)).collect(),
        };
        for len in [1.0, 3.0, 32.0, 63.0, 127.0, 256.0] {
            for lo in [
                0.0,
                137.0,
                0.25,
                137.75,
                137.0 + 1.0 / 1_048_576.0,
                138.0 - 1.0 / 1_048_576.0,
            ] {
                for step in [-32.0, 32.0] {
                    for pos in [lo, lo + len - 0.25] {
                        let voice = Voice {
                            pos,
                            step,
                            start: 0.0,
                            end: 511.75,
                            looping: true,
                            wrapped: true,
                            lo,
                            hi: lo + len,
                            len,
                            ..Voice::OFF
                        };
                        let first = pos.floor() as i64 - (EXTREME_TAPS / 2 - 1) as i64;
                        let mut l = [f32::NAN; EXTREME_TAPS];
                        let mut r = l;
                        assert!(gather_wrapped_loop(&sample, &voice, first, &mut l, &mut r));
                        for i in 0..EXTREME_TAPS {
                            let k = voice
                                .loop_index(first + i as i64)
                                .clamp(voice.start.ceil() as i64, voice.end.ceil() as i64 - 1)
                                as usize;
                            assert_eq!(
                                (l[i], r[i]),
                                (sample.l[k], sample.r[k]),
                                "{voice:?}, tap {i}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn wrapped_extreme_gather_rejects_intro_ambiguous_and_invalid_bounds() {
        let sample = alternating_sample();
        let voice = Voice {
            pos: 64.0,
            step: 32.0,
            start: 0.0,
            end: 128.0,
            looping: true,
            wrapped: true,
            lo: 33.0,
            hi: 96.0,
            len: 63.0,
            ..Voice::OFF
        };
        let rounded = Voice {
            lo: f64::EPSILON,
            hi: 1.0 + f64::EPSILON,
            len: 1.0,
            pos: 0.5,
            ..voice
        };
        // At tap 3, subtraction loses EPSILON and maps to 0, outside [ceil(lo), ceil(hi)).
        assert_eq!(rounded.loop_index(3), 0);
        for rejected in [
            rounded,
            Voice {
                wrapped: false,
                ..voice
            },
            Voice {
                wrapped: false,
                step: -32.0,
                ..voice
            },
            Voice {
                looping: false,
                ..voice
            },
            Voice {
                lo: f64::EPSILON,
                hi: 63.0,
                pos: 32.0,
                ..voice
            },
            Voice {
                lo: 33.0 + 1.0e-10,
                hi: 96.0 + 1.0e-10,
                ..voice
            },
            Voice {
                lo: 33.0 - 1.0e-10,
                hi: 96.0 - 1.0e-10,
                ..voice
            },
            Voice {
                start: 34.0,
                ..voice
            },
            Voice { lo: -1.0, ..voice },
            Voice {
                end: 129.0,
                ..voice
            },
            Voice {
                end: 129.0,
                hi: 129.0,
                len: 96.0,
                ..voice
            },
            Voice {
                hi: 33.0,
                len: 0.0,
                ..voice
            },
            Voice { len: 62.0, ..voice },
            Voice { pos: 96.0, ..voice },
        ] {
            let mut l = [1.0; EXTREME_TAPS];
            let mut r = [-1.0; EXTREME_TAPS];
            assert!(!gather_wrapped_loop(
                &sample, &rejected, -63, &mut l, &mut r
            ));
            assert_eq!(l, [1.0; EXTREME_TAPS]);
            assert_eq!(r, [-1.0; EXTREME_TAPS]);
        }
    }

    #[test]
    fn detune_and_device_rate_conversion_use_exact_hermite() {
        let sample = alternating_sample();
        for step in [1.007, 48_000.0 / 44_100.0, 1.09] {
            for direction in [-1.0, 1.0] {
                // A reverse one-shot starts at `end - 1` and plays down, so its tail case
                // sits below that; a forward one-shot stays below `end`.
                let tail = if direction > 0.0 { 127.25 } else { 126.25 };
                for (pos, looping) in [
                    (0.25, false),
                    (64.25, false),
                    (tail, false),
                    (17.25, true),
                    (109.75, true),
                ] {
                    // A wrapped loop plays between the loop bounds: forward ends at `hi`,
                    // reverse starts at `lo`.
                    let (start, end) = match (looping, direction > 0.0) {
                        (true, true) => (0.0, 110.0),
                        (true, false) => (17.0, sample.len() as f64),
                        _ => (0.0, sample.len() as f64),
                    };
                    let voice = Voice {
                        pos,
                        start,
                        end,
                        step: direction * step,
                        looping,
                        // Only a loop that has played through once is wrapped.
                        wrapped: looping,
                        // An odd loop length, so the period-2 sample tells mapped and
                        // unmapped taps apart.
                        lo: 17.0,
                        hi: 110.0,
                        len: 93.0,
                        ..Voice::OFF
                    };
                    // Every case must be a state the renderer reaches.
                    let reachable = if looping {
                        voice.lo <= pos && pos < voice.hi
                    } else if direction > 0.0 {
                        voice.start <= pos && pos < voice.end
                    } else {
                        voice.start <= pos && pos <= voice.end - 1.0
                    };
                    assert!(
                        reachable,
                        "unreachable fixture: pos {pos}, step {}",
                        voice.step
                    );
                    let index = |k| if looping { voice.loop_index(k) } else { k };
                    let (l, r) = read(&sample, &voice);
                    let expected = (
                        hermite(&sample.l, pos, index).to_bits(),
                        hermite(&sample.r, pos, index).to_bits(),
                    );
                    assert_eq!(
                        (l.to_bits(), r.to_bits()),
                        expected,
                        "step {}, pos {pos}",
                        voice.step
                    );
                }
            }
        }
    }

    #[test]
    fn crossover_is_active_at_step_1_12() {
        let sample = alternating_sample();
        let voice = Voice {
            pos: 64.25,
            end: sample.len() as f64,
            step: 1.12,
            ..Voice::OFF
        };
        let actual = read(&sample, &voice).0;
        let pure_cubic = hermite(&sample.l, voice.pos, |k| k);
        let pure_sinc = sinc(&sample, &voice, voice.step).0;
        assert!(
            pure_sinc < actual && actual < pure_cubic,
            "{actual} must be inside the crossover"
        );
    }

    #[test]
    fn crossover_ends_in_pure_sinc_at_step_1_25() {
        let sample = alternating_sample();
        let at = |step: f64| {
            let voice = Voice {
                pos: 64.25,
                end: sample.len() as f64,
                step,
                ..Voice::OFF
            };
            let (l, r) = read(&sample, &voice);
            let (sl, sr) = sinc(&sample, &voice, step);
            ((l.to_bits(), r.to_bits()), (sl.to_bits(), sr.to_bits()))
        };
        // Literal steps, not FADE_END, so moving the zone's top fails here.
        let (actual, pure) = at(1.25);
        assert_eq!(actual, pure, "step 1.25 must be the pure FIR");
        let (actual, pure) = at(1.2499);
        assert_ne!(
            actual.0, pure.0,
            "step 1.2499 must still blend in the cubic read"
        );
    }

    #[test]
    fn changing_fir_width_keeps_the_read_continuous() {
        let l: Vec<f32> = (0..128).map(|i| libm::sinf(i as f32 * 0.98)).collect();
        let sample = SampleBuf {
            rate: 48_000.0,
            r: l.iter().map(|&x| -x * 0.5).collect(),
            l,
        };
        // Hold source phase fixed. At this step the nominal cutoff lies exactly on the
        // shared row. Sweeping through it must preserve the kernel's gain and alignment.
        for join_step in [80.0 / 29.0, 8.0] {
            for pos in [0.0, 0.25, 64.25, 127.25] {
                let mut voice = Voice {
                    pos,
                    end: sample.len() as f64,
                    step: join_step - 0.000_001,
                    ..Voice::OFF
                };
                let (before_l, before_r) = read(&sample, &voice);
                voice.step = join_step + 0.000_001;
                let (after_l, after_r) = read(&sample, &voice);
                assert!(
                    (after_l - before_l).abs().max((after_r - before_r).abs()) < 1e-5,
                    "read jumped at step {join_step}, pos {pos}: ({before_l}, {before_r}) to ({after_l}, {after_r})"
                );
            }
        }
    }

    #[test]
    fn glide_changes_the_interpolator_without_a_step_in_gain() {
        let sample = alternating_sample();
        // Hold the phase fixed to measure gain jumps due to the glide's step updates,
        // separately from waveform motion. This Nyquist probe has very different gains
        // under the two interpolators, so an abrupt switch is easy to detect.
        let mut voice = Voice {
            pos: 64.25,
            start: 0.0,
            end: sample.len() as f64,
            step: 1.0,
            base: 1.0,
            track: true,
            root: 60.0,
            pitch: 60.0,
            ..Voice::OFF
        };
        let original = read(&sample, &voice).0;
        let mut previous = original;
        let mut largest_jump = 0.0_f32;
        voice.glide_to(65.0, 200.0, 48_000.0);
        while voice.pitch != voice.target {
            voice.glide();
            let current = read(&sample, &voice).0;
            largest_jump = largest_jump.max((current - previous).abs());
            previous = current;
        }
        assert!(
            (previous - original).abs() > 0.5,
            "the glide reaches the FIR"
        );
        assert!(
            largest_jump < 0.004,
            "largest frame-to-frame gain jump {largest_jump}"
        );
    }
}
