use grooviq_dsp::{Device, reverb::Reverb};

const BLOCK: usize = 128;

fn render_short_tail(sr: f32, frames: usize, mut inspect: impl FnMut(usize, [f32; 2])) {
    let mut reverb = Reverb::new(sr);
    for (index, value) in [(1, 0.1), (2, 0.0), (5, 1_000.0), (6, 1_000.0), (7, 1.0)] {
        reverb.set_param(index, value);
    }
    reverb.reset();

    for start in (0..frames).step_by(BLOCK) {
        let n = BLOCK.min(frames - start);
        let mut left = [0.0; BLOCK];
        let mut right = [0.0; BLOCK];
        if start == 0 {
            left[0] = 0.8;
        }
        reverb.process(&mut left[..n], &mut right[..n], None, &[]);
        for i in 0..n {
            inspect(start + i, [left[i], right[i]]);
        }
    }
}

#[test]
fn short_fully_wet_tail_has_no_subnormals() {
    let mut failures = Vec::new();
    for sr in [44_100.0, 48_000.0, 96_000.0] {
        let mut counts = [0; 2];
        let mut first = [None; 2];
        let mut peak = [0.0_f32; 2];
        render_short_tail(sr, 8 * sr as usize, |frame, samples| {
            for (channel, sample) in samples.into_iter().enumerate() {
                if sample.is_subnormal() {
                    counts[channel] += 1;
                    first[channel].get_or_insert(frame);
                }
                if frame > 0 {
                    peak[channel] = peak[channel].max(sample.abs());
                }
            }
        });
        assert!(peak.iter().all(|&v| v > 0.0001), "{sr} Hz: {peak:?}");
        if counts != [0, 0] {
            failures.push(format!("{sr} Hz: counts {counts:?}, first {first:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn static_tail_preserves_the_reference_bits() {
    // FNV-1a of the pre-fix stereo impulse response at 48 kHz, little-endian and
    // interleaved, with each subnormal replaced by the zero of the same sign.
    const REFERENCE: u64 = 0xd2cf_1214_238d_c78f;
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    render_short_tail(48_000.0, 54_000, |_, samples| {
        for byte in samples.into_iter().flat_map(f32::to_le_bytes) {
            digest = (digest ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    });
    assert_eq!(digest, REFERENCE);
}

#[test]
fn attenuation_flushes_only_after_the_dry_gain() {
    let mut reverb = Reverb::new(48_000.0);
    reverb.set_param(7, 0.75);
    // Before the first reflection, the dry gain is about 0.383. Only frame 0 underflows
    // into the subnormal range. Frame 1 is still a normal sample and must be left alone.
    let mut left = [-2.0 * f32::MIN_POSITIVE, -4.0 * f32::MIN_POSITIVE];
    let mut right = [2.0 * f32::MIN_POSITIVE, 4.0 * f32::MIN_POSITIVE];
    reverb.process(&mut left, &mut right, None, &[]);

    assert_eq!(left[0].to_bits(), (-0.0_f32).to_bits());
    assert_eq!(right[0].to_bits(), 0.0_f32.to_bits());
    assert!(left[1].is_normal() && left[1].is_sign_negative());
    assert!(right[1].is_normal() && right[1].is_sign_positive());
}
