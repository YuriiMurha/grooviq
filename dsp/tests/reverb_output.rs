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
