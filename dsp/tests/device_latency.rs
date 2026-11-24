use grooviq_dsp::{api::Device, create};

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

// Measure the compensated dry path, not the oversampling FIR's first nonzero tap.
fn assert_impulse(device: &mut dyn Device, expected: u32) {
    assert_eq!(device.latency_frames(), expected);
    device.process(&mut [], &mut [], None, &[]);
    assert_eq!(device.latency_frames(), expected);
    device.reset();
    assert_eq!(device.latency_frames(), expected);
    let mut left = Vec::new();
    let mut right = Vec::new();
    for block in 0..6 {
        let mut l = [0.; 128];
        let mut r = [0.; 128];
        if block == 0 {
            l[0] = 0.125;
            r[0] = -0.0625;
        }
        device.process(&mut l, &mut r, None, &[]);
        left.extend(l);
        right.extend(r);
    }
    for (signal, amplitude) in [(&left, 0.125), (&right, -0.0625)] {
        for (frame, &sample) in signal.iter().enumerate() {
            let wanted = if frame == expected as usize {
                amplitude
            } else {
                0.
            };
            assert!(
                (sample - wanted).abs() < 1e-6,
                "frame {frame}: {sample} != {wanted}, latency {expected}"
            );
        }
    }
    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..1024 {
            assert_eq!(std::hint::black_box(&*device).latency_frames(), expected);
        }
    });
}

#[test]
fn drive_reports_each_mode_and_tracks_live_mode_changes() {
    for rate in [8000., 44100., 48000., 96000., 384000.] {
        let mut device = create("drive", rate).unwrap();
        device.set_param(3, 0.); // compensated dry
        for (mode, expected) in [(0., 0), (1., 43), (2., 49), (0., 0), (2., 49), (1., 43)] {
            device.set_param(5, mode);
            assert_impulse(device.as_mut(), expected);
        }
    }
}

#[test]
fn saturation_reports_compensated_dry_alignment_for_all_models() {
    for rate in [22050., 44100., 48000., 96000., 192000.] {
        let mut device = create("saturation", rate).unwrap();
        device.set_param(7, 0.);
        for model in 0..4 {
            device.set_param(0, model as f32);
            assert_impulse(device.as_mut(), 49);
        }
    }
}

#[test]
fn limiter_reports_measured_lookahead_at_supported_rate_extremes() {
    // Independently tabulated 1.5 ms lookahead minus one frame, including rounding.
    for (rate, expected) in [
        (8000., 11),
        (22050., 32),
        (44100., 65),
        (48000., 71),
        (96000., 143),
        (192000., 287),
        (384000., 575),
    ] {
        assert_impulse(create("limiter", rate).unwrap().as_mut(), expected);
    }
}

#[test]
fn default_zero_is_processing_latency_not_creative_wet_timing() {
    for id in [
        "utility",
        "eq",
        "comp",
        "delay",
        "character_delay",
        "reverb",
        "algorithmic_reverb",
        "shimmer_reverb",
    ] {
        let mut device = create(id, 48000.).unwrap();
        assert_eq!(device.latency_frames(), 0, "{id}");
        device.process(&mut [], &mut [], None, &[]);
        device.reset();
        assert_no_alloc::assert_no_alloc(|| assert_eq!(device.latency_frames(), 0, "{id}"));
    }
    assert_impulse(create("utility", 48000.).unwrap().as_mut(), 0);
}
