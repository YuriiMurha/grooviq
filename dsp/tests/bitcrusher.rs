use grooviq_dsp::{
    Device,
    bitcrusher::{Bitcrusher, PRESETS, preset},
};
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
fn setup(sr: f32, values: &[(u32, f32)]) -> Bitcrusher {
    let mut d = Bitcrusher::new(sr);
    for &(i, v) in values {
        d.set_param(i, v);
    }
    d
}
fn run(d: &mut Bitcrusher, input: &[f32], block: usize) -> (Vec<f32>, Vec<f32>) {
    let mut l = input.to_vec();
    let mut r = l.clone();
    for (a, b) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(a, b, None, &[]);
    }
    (l, r)
}
#[test]
fn quantization_and_hold_cadence() {
    let input: Vec<_> = (0..128).map(|i| -0.9 + i as f32 / 80.0).collect();
    let (l, r) = run(
        &mut setup(48000.0, &[(0, 3.0), (1, 4.0), (2, 0.0)]),
        &input,
        7,
    );
    assert_eq!(l, r);
    for (i, &v) in l.iter().enumerate() {
        let x = input[i / 4 * 4];
        assert_eq!(v, libm::roundf(x * 4.0).clamp(-4.0, 3.0) / 4.0);
    }
}
#[test]
fn presets_boundaries_blocks_and_reset() {
    let input: Vec<_> = (0..4096)
        .map(|i| libm::sinf(i as f32 * 0.07) * 0.7)
        .collect();
    for sr in [22050.0, 48000.0, 192000.0] {
        for name in PRESETS {
            let values = preset(name).unwrap();
            let expected = run(&mut setup(sr, &values), &input, 1);
            for block in [7, 64, 127, 128] {
                let mut d = setup(sr, &values);
                assert_eq!(expected, run(&mut d, &input, block));
                d.reset();
                assert_eq!(expected, run(&mut d, &input, block));
            }
            assert!(expected.0.iter().all(|v| v.is_finite()));
        }
    }
}
#[test]
fn noise_streams_are_independent_and_repeatable() {
    for values in [
        vec![(0, 3.0), (1, 1.0), (2, 0.0), (4, 1.0)],
        vec![(0, 24.0), (1, 8.0), (2, 0.0), (3, 0.45)],
    ] {
        let input: Vec<_> = (0..4096)
            .map(|i| 0.13 + 0.1 * libm::sinf(i as f32 * 0.09))
            .collect();
        let a = run(&mut setup(48000.0, &values), &input, 7);
        assert_ne!(a.0, a.1);
        assert_eq!(a, run(&mut setup(48000.0, &values), &input, 128));
    }
}
#[test]
fn filtering_reduces_high_frequency_energy() {
    let energy = |freq: f32, filter: f32| {
        let x: Vec<_> = (0..16384)
            .map(|i| 0.5 * libm::sinf(i as f32 * freq))
            .collect();
        let (a, _) = run(
            &mut setup(48000.0, &[(0, 24.0), (1, 8.0), (2, filter)]),
            &x,
            128,
        );
        a[2048..].iter().map(|v| v * v).sum::<f32>()
    };
    let low = energy(0.01, 1.0) / energy(0.01, 0.0);
    let high = energy(1.0, 1.0) / energy(1.0, 0.0);
    assert!(low > 0.8, "{low}");
    assert!(high < 0.01, "{high}");
}
#[test]
fn controls_are_safe_smoothed_and_allocation_free() {
    let mut d = setup(48000.0, &[(5, 0.0)]);
    let mut l = [0.37; 128];
    let mut r = l;
    assert_no_alloc::assert_no_alloc(|| {
        d.process(&mut l, &mut r, None, &[]);
        d.set_param(5, 1.0);
        d.set_param(0, 2.0);
        for i in 0..6 {
            for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                d.set_param(i, v);
            }
        }
        d.process(&mut l, &mut r, None, &[]);
    });
    assert!((l[0] - 0.37).abs() < 0.002);
    assert!(l.iter().all(|v| v.is_finite() && v.abs() < 1.0));
    assert_no_alloc::assert_no_alloc(|| d.reset());
}
#[test]
fn silent_channel_has_no_crosstalk() {
    let mut d = setup(48000.0, &[]);
    let mut l = [0.8; 128];
    let mut r = [0.0; 128];
    d.process(&mut l, &mut r, None, &[]);
    assert!(r.iter().all(|&v| v == 0.0));
}

#[test]
fn hostile_controls_and_audio_do_not_poison_state() {
    for sr in [22050.0, 192000.0, f32::NAN] {
        let mut d = Bitcrusher::new(sr);
        for value in [-f32::MAX, f32::MAX] {
            for i in 0..6 {
                d.set_param(i, value);
            }
            let mut l = [f32::MAX; 128];
            let mut r = [f32::NAN; 128];
            d.process(&mut l, &mut r, None, &[]);
            assert!(l.iter().chain(&r).all(|x| x.is_finite()));
        }
    }
}

#[test]
fn mix_jump_is_smoothed_and_reaches_target() {
    let mut d = setup(48000.0, &[(0, 2.0), (1, 1.0), (2, 0.0), (5, 0.0)]);
    let mut l = [0.37; 128];
    let mut r = l;
    d.process(&mut l, &mut r, None, &[]);
    d.set_param(5, 1.0);
    let input = vec![0.37; 8192];
    let (out, _) = run(&mut d, &input, 127);
    assert!((out[0] - 0.37).abs() < 0.001);
    assert!(out.windows(2).all(|w| (w[1] - w[0]).abs() < 0.001));
    assert!((out[out.len() - 1] - 0.5).abs() < 1e-5);
}

#[test]
fn jitter_has_bounded_variable_intervals_and_dither_has_expected_density() {
    let ramp: Vec<_> = (0..4096).map(|i| i as f32 / 8192.0).collect();
    let (a, _) = run(
        &mut setup(48000.0, &[(0, 24.0), (1, 8.0), (2, 0.0), (3, 0.45)]),
        &ramp,
        7,
    );
    let edges: Vec<_> = a
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] != w[1])
        .map(|(i, _)| i + 1)
        .collect();
    let intervals: Vec<_> = edges.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(intervals.iter().all(|&n| (4..=12).contains(&n)));
    assert!(intervals.iter().any(|&n| n < 8) && intervals.iter().any(|&n| n > 8));
    let (a, _) = run(
        &mut setup(48000.0, &[(0, 3.0), (1, 1.0), (2, 0.0), (4, 1.0)]),
        &vec![0.0; 32768],
        128,
    );
    let density = a.iter().filter(|&&x| x != 0.0).count() as f32 / a.len() as f32;
    let mean = a.iter().sum::<f32>() / a.len() as f32;
    assert!((0.22..0.28).contains(&density), "{density}");
    assert!(mean.abs() < 0.005, "{mean}");
}
