use wide::f32x4;

use super::{SampleBuf, Voice, hermite};

include!(concat!(env!("OUT_DIR"), "/sampler-fir.rs"));

type Phase = [f32; TAPS];
type Band = [Phase; PHASES + 1];

static FIR: [Band; BANDS + 1] = {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/sampler-fir.bin"));
    let mut table = [[[0.0; TAPS]; PHASES + 1]; BANDS + 1];
    let mut band = 0;
    let mut offset = 0;
    while band <= BANDS {
        let mut phase = 0;
        while phase <= PHASES {
            let mut tap = 0;
            while tap < TAPS {
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
};

/// Keep detune and 48 kHz sources at 44.1 kHz on the exact cubic path. Fade to the FIR as
/// the step increases so a glide does not abruptly change the interpolator's gain.
const FADE_START: f64 = 1.09;
const FADE_END: f64 = 1.25;

#[inline]
pub(super) fn read(sample: &SampleBuf, voice: &Voice) -> (f32, f32) {
    let step = voice.step.abs();
    if step >= FADE_END {
        return sinc(sample, voice, step);
    }
    let (l, r) = if voice.looping {
        let at = |k| voice.loop_index(k);
        (
            hermite(&sample.l, voice.pos, at),
            hermite(&sample.r, voice.pos, at),
        )
    } else {
        (
            hermite(&sample.l, voice.pos, |k| k),
            hermite(&sample.r, voice.pos, |k| k),
        )
    };
    if step <= FADE_START {
        return (l, r);
    }
    let (sl, sr) = sinc(sample, voice, step);
    let mix = ((step - FADE_START) / (FADE_END - FADE_START)) as f32;
    (l + mix * (sl - l), r + mix * (sr - r))
}

#[inline(never)]
fn sinc(sample: &SampleBuf, voice: &Voice, step: f64) -> (f32, f32) {
    let pos = voice.pos.floor();
    let first = pos as i64 - (TAPS / 2 - 1) as i64;
    let phase = (voice.pos - pos) * PHASES as f64;
    let phase_index = phase as usize;
    let phase_mix = f32x4::splat((phase - phase_index as f64) as f32);

    // Put the center of the FIR transition below the destination Nyquist. Limit the
    // offset at very high steps so the cutoff keeps tracking the available bandwidth.
    let nyquist = 0.5 / step;
    let cutoff = nyquist - 0.025_f64.min(nyquist * 0.25);
    let band = cutoff * (2 * BANDS) as f64;
    let band_index = band as usize;
    let band_mix = f32x4::splat((band - band_index as f64) as f32);
    let kernels = [
        &FIR[band_index][phase_index],
        &FIR[band_index][phase_index + 1],
        &FIR[band_index + 1][phase_index],
        &FIR[band_index + 1][phase_index + 1],
    ];

    let mut edge_l = [0.0; TAPS];
    let mut edge_r = [0.0; TAPS];
    let first_f = first as f64;
    let last_f = first_f + (TAPS - 1) as f64;
    let contiguous = first_f >= voice.start
        && last_f < voice.end
        && (!voice.looping || (first_f >= voice.lo && last_f < voice.hi));
    let (l, r) = if contiguous
        && let first = first as usize
        && let Some(l) = sample.l.get(first..first + TAPS)
        && let Some(r) = sample.r.get(first..first + TAPS)
    {
        (l, r)
    } else {
        let lo = voice.start.ceil() as i64;
        let hi = voice.end.ceil() as i64 - 1;
        for i in 0..TAPS {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn detune_and_device_rate_conversion_use_exact_hermite() {
        let sample = alternating_sample();
        for step in [1.007, 48_000.0 / 44_100.0, 1.09] {
            for direction in [-1.0, 1.0] {
                let voice = Voice {
                    pos: 64.25,
                    end: sample.len() as f64,
                    step: direction * step,
                    ..Voice::OFF
                };
                let (l, r) = read(&sample, &voice);
                assert_eq!(
                    l.to_bits(),
                    hermite(&sample.l, voice.pos, |k| k).to_bits(),
                    "step {}",
                    voice.step
                );
                assert_eq!(
                    r.to_bits(),
                    hermite(&sample.r, voice.pos, |k| k).to_bits(),
                    "step {}",
                    voice.step
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
            largest_jump < 0.01,
            "largest frame-to-frame gain jump {largest_jump}"
        );
    }
}
