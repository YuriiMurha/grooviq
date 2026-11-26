use grooviq_dsp::eq::{SPEC, effective_band_hz};
use grooviq_dsp::{Device, create};

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn assert_hz(actual: f32, expected: f32) {
    // The EQ stores frequency in f32 log2 Hz, so its round trip can shift a few ulps.
    assert!(
        (actual - expected).abs() < 0.025,
        "{actual} Hz vs {expected} Hz"
    );
}

fn band_meters(eq: &dyn Device) -> [f32; 8] {
    assert_no_alloc::assert_no_alloc(|| core::array::from_fn(|band| eq.meter(2 + band as u32)))
}

#[test]
fn eq_effective_band_hz_preserves_both_clamps_and_f64_precision() {
    assert_eq!(effective_band_hz(20_000.0, 22_050.0), 10_804.5);
    assert_eq!(effective_band_hz(20_000.0, 44_100.0), 20_000.0);
    assert_eq!(effective_band_hz(20_000.0, 48_000.0), 20_000.0);
    assert_eq!(effective_band_hz(20_000.0, 22_051.0), 10_804.99);
    assert_eq!(effective_band_hz(5.0, 48_000.0), 10.0);
}

#[test]
fn eq_band_meters_report_capped_and_uncapped_frequencies() {
    for (rate, expected) in [
        (22_050.0, 10_804.5),
        (44_100.0, 20_000.0),
        (48_000.0, 20_000.0),
    ] {
        let mut eq = create("eq", rate).unwrap();
        eq.set_param(44, 20_000.0);
        let actual = band_meters(&*eq)[7];
        assert_hz(actual, expected);
        if rate == 22_050.0 {
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn eq_band_meter_indices_follow_band_order_and_fresh_parameters() {
    let mut eq = create("eq", 22_050.0).unwrap();
    for (band, actual) in band_meters(&*eq).into_iter().enumerate() {
        assert_hz(
            actual,
            effective_band_hz(SPEC[6 * band + 2].2.into(), 22_050.0) as f32,
        );
    }

    let frequencies = [40.0, 170.0, 620.0, 2300.0, 4200.0, 7400.0, 9600.0, 15_000.0];
    for (band, freq) in frequencies.into_iter().enumerate() {
        eq.set_param((6 * band + 2) as u32, freq);
    }
    for (actual, requested) in band_meters(&*eq).into_iter().zip(frequencies) {
        assert_hz(actual, effective_band_hz(requested.into(), 22_050.0) as f32);
    }
    assert_eq!([eq.meter(0), eq.meter(1), eq.meter(10)], [0.0; 3]);
    let mut bins = [0.0; 128];
    assert_eq!(eq.meter_array(0, &mut bins), 128);
    assert_eq!(eq.meter_array(1, &mut bins), 128);
    assert_eq!(eq.meter_array(2, &mut bins), 0);
}

#[test]
fn eq_band_meters_snap_on_reset() {
    let mut eq = create("eq", 22_050.0).unwrap();
    eq.process(&mut [0.0], &mut [0.0], None, &[]);
    eq.set_param(2, 6900.0);
    eq.set_param(14, 20_000.0);
    eq.reset();
    let meters = band_meters(&*eq);
    assert_hz(meters[0], effective_band_hz(6900.0, 22_050.0) as f32);
    assert_eq!(meters[2], effective_band_hz(20_000.0, 22_050.0) as f32);
}

#[test]
fn eq_band_meter_advances_on_control_ticks_and_settles_at_the_cap() {
    let rate = 22_050.0;
    let mut eq = create("eq", rate).unwrap();
    eq.set_param(14, 1000.0);
    let before = eq.meter(4);
    eq.process(&mut [0.0], &mut [0.0], None, &[]);
    eq.set_param(14, 20_000.0);
    assert_eq!(eq.meter(4), before);
    eq.process(&mut [0.0; 15], &mut [0.0; 15], None, &[]);
    assert_eq!(eq.meter(4), before);

    eq.process(&mut [0.0], &mut [0.0], None, &[]);
    let after_tick = eq.meter(4);
    assert!(after_tick > before && after_tick < 10_804.5);
    // A one-pole glide in log Hz moves a fraction 1 - exp(-elapsed / 12 ms) per tick.
    let fraction = 1.0 - libm::exp(-16.0 / (0.012 * f64::from(rate)));
    let smoothed_hz = 1000.0 * libm::pow(20.0, fraction);
    assert_hz(
        after_tick,
        effective_band_hz(smoothed_hz, rate.into()) as f32,
    );
    eq.process(&mut [0.0; 15], &mut [0.0; 15], None, &[]);
    assert_eq!(eq.meter(4), after_tick);

    for _ in 0..80 {
        eq.process(&mut [0.0; 128], &mut [0.0; 128], None, &[]);
    }
    assert_eq!(eq.meter(4), 10_804.5);
}

#[test]
fn eq_requests_above_the_same_cap_render_bit_identically() {
    fn render(requested: f32) -> [u32; 512] {
        let mut eq = create("eq", 22_050.0).unwrap();
        // Isolate a low-pass so a difference in its coefficients is audible in the impulse.
        for band in 0..8 {
            eq.set_param(6 * band, 0.0);
        }
        eq.set_param(42, 1.0);
        eq.set_param(43, 5.0);
        eq.set_param(44, requested);
        let mut left = [0.0; 512];
        left[0] = 1.0;
        let mut right = left;
        eq.process(&mut left, &mut right, None, &[]);
        left.map(f32::to_bits)
    }

    let capped = render(15_000.0);
    assert!(render(8000.0) != capped);
    // 10,900 Hz is below the biquad's own guard, so that guard cannot mask a missing EQ cap.
    assert!(render(10_900.0) == capped);
    assert!(render(20_000.0) == capped);
}

#[test]
fn eq_capped_band_notches_at_the_reported_frequency() {
    let rate = 22_050.0;
    let mut eq = create("eq", rate).unwrap();
    for band in 0..8 {
        eq.set_param(6 * band, 0.0);
    }
    eq.set_param(42, 1.0);
    eq.set_param(43, 3.0);
    eq.set_param(44, 20_000.0);
    let hz = f64::from(eq.meter(9));
    assert_eq!(hz, 10_804.5);
    let mut left = core::array::from_fn::<_, 4096, _>(|i| {
        libm::cos(core::f64::consts::TAU * hz * i as f64 / f64::from(rate)) as f32
    });
    let mut right = left;
    eq.process(&mut left, &mut right, None, &[]);
    for sample in left[2048..].iter().chain(&right[2048..]) {
        assert!(sample.abs() < 1e-6, "residual at the notch: {sample}");
    }
}
