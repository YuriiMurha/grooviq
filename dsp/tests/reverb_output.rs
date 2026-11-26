use grooviq_dsp::{Device, algorithmic_reverb::AlgorithmicReverb, shimmer_reverb::ShimmerReverb};

#[test]
fn algorithmic_flushes_after_the_dry_mix_and_output_gain() {
    let min = f32::MIN_POSITIVE;
    for (gain_db, input, expected) in [
        (0.0, 2.0 * min, 0.0),
        // Normal following the dry mix, subnormal following the output gain.
        (-12.0, 8.0 * min, 0.0),
        // Subnormal following the dry mix, normal following the output gain.
        (12.0, 2.0 * min, (0.5 * min) * libm::powf(10.0, 12.0 / 20.0)),
    ] {
        let mut reverb = AlgorithmicReverb::new(48_000.0);
        reverb.set_param(10, 0.75);
        reverb.set_param(11, gain_db);
        let mut left = [-input];
        let mut right = [input];
        // Frame zero is inside the default 15 ms pre-delay.
        reverb.process(&mut left, &mut right, None, &[]);
        assert_eq!(
            left[0].to_bits(),
            (-expected).to_bits(),
            "left at {gain_db} dB"
        );
        assert_eq!(
            right[0].to_bits(),
            expected.to_bits(),
            "right at {gain_db} dB"
        );
    }
}

#[test]
fn shimmer_flushes_after_the_dry_mix() {
    let mut reverb = ShimmerReverb::new(48_000.0);
    reverb.set_param(0, 0.75);
    let min = f32::MIN_POSITIVE;
    let mut left = [-2.0 * min, -4.0 * min];
    let mut right = [2.0 * min, 4.0 * min];
    reverb.process(&mut left, &mut right, None, &[]);
    assert_eq!(
        left.map(f32::to_bits),
        [(-0.0_f32).to_bits(), (-min).to_bits()]
    );
    assert_eq!(right.map(f32::to_bits), [0.0_f32.to_bits(), min.to_bits()]);
}

fn static_render(mut reverb: impl Device, block: usize) -> Vec<f32> {
    let mut left = vec![0.0; 16_384];
    let mut right = left.clone();
    left[..3].copy_from_slice(&[-2.0 * f32::MIN_POSITIVE, -16.0 * f32::MIN_POSITIVE, 0.8]);
    right[..3].copy_from_slice(&[2.0 * f32::MIN_POSITIVE, 16.0 * f32::MIN_POSITIVE, -0.3]);
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        reverb.process(l, r, None, &[]);
    }
    left.into_iter()
        .zip(right)
        .flat_map(|(l, r)| [l, r])
        .collect()
}

fn digest(samples: Vec<f32>) -> u64 {
    samples
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

#[test]
fn static_reverb_renders_preserve_reference_bits() {
    // FNV-1a of the pre-fix stereo output at 48 kHz, interleaved and little-endian,
    // with only subnormal samples replaced by signed zero.
    const ALGORITHMIC: u64 = 0x6c07_d834_80ca_243b;
    const SHIMMER: u64 = 0x35ce_7997_fa0b_76b5;
    for block in [7, 128] {
        let mut algorithmic = AlgorithmicReverb::new(48_000.0);
        algorithmic.set_param(10, 0.75);
        let mut shimmer = ShimmerReverb::new(48_000.0);
        shimmer.set_param(0, 0.75);
        for (name, samples, expected) in [
            (
                "Algorithmic",
                static_render(algorithmic, block),
                ALGORITHMIC,
            ),
            ("Shimmer", static_render(shimmer, block), SHIMMER),
        ] {
            assert_eq!(digest(samples), expected, "{name} at block size {block}");
        }
    }
}
