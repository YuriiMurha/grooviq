use grooviq_dsp::{Device, create, effective_high_cut};

const DEVICES: [(&str, f32); 3] = [
    ("delay", 7000.0),
    ("character_delay", 7000.0),
    ("reverb", 9000.0),
];
const HIGH_CUT: u32 = 6;
const HIGH_CUT_METER: u32 = 3;

#[test]
fn effective_high_cut_preserves_the_f64_cap() {
    assert_eq!(effective_high_cut(20_000.0, 22_051.0), 9922.95);
}

fn assert_hz(id: &str, phase: &str, actual: f32, expected: f32) {
    // The existing f32 log2/exp2 round trip can shift the unclamped value by a few ulps.
    assert!(
        (actual - expected).abs() < 0.02,
        "{id} {phase}: {actual} Hz vs {expected} Hz"
    );
}

#[test]
fn high_cut_meters_report_the_sample_rate_cap() {
    for (rate, expected) in [
        (22_050.0, 9922.5),
        (44_100.0, 19_845.0),
        (48_000.0, 20_000.0),
    ] {
        for (id, _) in DEVICES {
            let mut device = create(id, rate).unwrap();
            process_silence(&mut *device, 128);
            device.set_param(HIGH_CUT, 20_000.0);
            for _ in 0..(rate as usize).div_ceil(128) {
                process_silence(&mut *device, 128);
            }
            let actual = device.meter(HIGH_CUT_METER);
            assert_hz(id, "rate cap", actual, expected);
            assert_hz(
                id,
                "public helper",
                actual,
                effective_high_cut(20_000.0, rate.into()) as f32,
            );
        }
    }
}

#[test]
fn high_cut_meters_follow_construction_fresh_params_and_reset() {
    for (id, default) in DEVICES {
        let mut device = create(id, 22_050.0).unwrap();
        let snapped = |hz| effective_high_cut(libm::exp2f(libm::log2f(hz)).into(), 22_050.0) as f32;
        let initial = device.meter(HIGH_CUT_METER);
        assert_hz(id, "construction", initial, default);
        assert_eq!(initial, snapped(default));
        device.set_param(HIGH_CUT, 4000.0);
        let fresh = device.meter(HIGH_CUT_METER);
        assert_hz(id, "fresh parameter", fresh, 4000.0);
        assert_eq!(fresh, snapped(4000.0));
        process_silence(&mut *device, 16);
        device.set_param(HIGH_CUT, 12_000.0);
        device.reset();
        assert_eq!(device.meter(HIGH_CUT_METER), 9922.5, "{id} reset");
        assert_eq!(device.meter(HIGH_CUT_METER), snapped(12_000.0));
    }
}

fn process_silence(device: &mut dyn Device, frames: usize) {
    device.process(
        &mut [0.0; 128][..frames],
        &mut [0.0; 128][..frames],
        None,
        &[],
    );
}

#[test]
fn high_cut_meters_follow_the_active_log2_glide_at_control_ticks() {
    let rate = 22_050.0;
    let initial_log = 12.0; // 4096 Hz
    let target_log = 14.0; // 16,384 Hz, above this sample rate's cap
    let decay = libm::expf(-16.0 / (0.015 * rate));
    for (id, _) in DEVICES {
        let mut device = create(id, rate).unwrap();
        device.set_param(HIGH_CUT, 4096.0);
        process_silence(&mut *device, 16);
        device.set_param(HIGH_CUT, 16_384.0);
        assert_eq!(device.meter(HIGH_CUT_METER), 4096.0, "{id} before glide");
        let mut log_hz = initial_log;
        for _ in 0..32 {
            log_hz = target_log + (log_hz - target_log) * decay;
            let expected = libm::exp2f(log_hz).min(9922.5);
            process_silence(&mut *device, 1);
            let on_tick = device.meter(HIGH_CUT_METER);
            assert_hz(id, "control tick", on_tick, expected);
            assert_hz(
                id,
                "public helper during glide",
                on_tick,
                effective_high_cut(libm::exp2f(log_hz).into(), rate.into()) as f32,
            );
            process_silence(&mut *device, 15);
            assert_eq!(device.meter(HIGH_CUT_METER), on_tick, "{id} between ticks");
        }
        assert_eq!(
            device.meter(HIGH_CUT_METER),
            9922.5,
            "{id} after hitting cap"
        );
    }
}

fn render_settled_wet(id: &str, high_cut: f32) -> (f32, Vec<u32>) {
    let mut device = create(id, 22_050.0).unwrap();
    if id != "reverb" {
        device.set_param(0, 0.0); // free-running time
    }
    device.set_param(2, 20.0); // delay or reverb pre-delay
    device.set_param(7, 1.0); // wet only
    process_silence(&mut *device, 128);
    device.set_param(HIGH_CUT, high_cut);
    for _ in 0..173 {
        process_silence(&mut *device, 128);
    }

    let mut left = [0.0; 4096];
    let mut right = [0.0; 4096];
    left[0] = 0.5;
    right[0] = -0.25;
    for (left, right) in left.chunks_mut(128).zip(right.chunks_mut(128)) {
        device.process(left, right, None, &[]);
    }
    assert!(
        left.iter().chain(&right).skip(1).any(|v| v.abs() > 1e-4),
        "{id} wet response"
    );
    (
        device.meter(HIGH_CUT_METER),
        left.into_iter().chain(right).map(f32::to_bits).collect(),
    )
}

#[test]
fn equivalent_high_cut_requests_render_bit_identically_after_settling() {
    for (id, _) in DEVICES {
        let (capped_hz, capped) = render_settled_wet(id, 20_000.0);
        // Stay below Nyquist so the biquad's own frequency limit cannot hide a missing device cap.
        let (equivalent_hz, equivalent) = render_settled_wet(id, 10_000.0);
        let (_, below_cap) = render_settled_wet(id, 4000.0);
        assert!(capped == equivalent, "{id} above-cap requests differ");
        assert!(capped != below_cap, "{id} wet response ignores the cutoff");
        assert_eq!(capped_hz, 9922.5, "{id} rendered cap");
        assert_eq!(equivalent_hz, capped_hz, "{id} equivalent cap");
    }
}
