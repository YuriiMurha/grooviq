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
    for sr in [22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
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
        &vec![0.0001; 32768],
        128,
    );
    let density = a.iter().filter(|&&x| x != 0.0).count() as f32 / a.len() as f32;
    let mean = a.iter().sum::<f32>() / a.len() as f32;
    assert!((0.22..0.28).contains(&density), "{density}");
    assert!(mean.abs() < 0.005, "{mean}");
}

fn seeded(seed: u32, values: &[(u32, f32)]) -> Bitcrusher {
    let mut d = Bitcrusher::with_seed(48000.0, seed);
    for &(i, v) in values {
        d.set_param(i, v);
    }
    d
}
#[test]
fn supplied_seeds_repeat_decorrelate_and_reset_without_zero_lock() {
    let x: Vec<_> = (0..4096)
        .map(|i| 0.13 + 0.1 * libm::sinf(i as f32 * 0.09))
        .collect();
    for values in [
        vec![(0, 3.0), (1, 1.0), (2, 0.0), (4, 1.0)],
        vec![(0, 24.0), (1, 8.0), (2, 0.0), (3, 0.45)],
    ] {
        let a = run(&mut seeded(0, &values), &x, 7);
        assert_ne!(a.0, a.1);
        for seed in [0, 1, u32::MAX, 0xa511_e9b3] {
            let mut d = seeded(seed, &values);
            let b = run(&mut d, &x, 128);
            assert_eq!(b, run(&mut seeded(seed, &values), &x, 1));
            d.reset();
            assert_eq!(b, run(&mut d, &x, 64));
            if seed != 0 {
                assert_ne!(a, b);
            }
        }
    }
}
#[test]
fn exact_silence_all_presets_rates_and_tail_settles() {
    for sr in [22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
        for name in PRESETS.into_iter().chain(["Init"]) {
            let mut values = preset(name).unwrap();
            values.push((4, 1.0));
            let mut d = setup(sr, &values);
            let (a, b) = run(&mut d, &vec![0.0; sr as usize], 127);
            assert!(a.iter().chain(&b).all(|&x| x == 0.0), "{name} {sr}");
            run(&mut d, &[0.4; 128], 64);
            let (a, b) = run(&mut d, &vec![0.0; sr as usize * 2], 128);
            assert!(
                a[a.len() - 128..]
                    .iter()
                    .chain(&b[b.len() - 128..])
                    .all(|&x| x == 0.0),
                "tail {name} {sr}"
            );
        }
    }
}
#[test]
fn fractional_period_carries_exact_alternating_cadence() {
    let x: Vec<_> = (0..64).map(|i| 0.01 + i as f32 / 128.0).collect();
    let (a, _) = run(&mut setup(48000.0, &[(0, 24.0), (1, 2.5), (2, 0.0)]), &x, 7);
    for (i, &y) in a.iter().enumerate() {
        let captured = i / 5 * 5 + if i % 5 >= 3 { 3 } else { 0 };
        let want = libm::roundf(x[captured] * 8388608.0) / 8388608.0;
        assert_eq!(y, want, "sample {i}");
    }
}
#[test]
fn postfilter_attenuates_hold_images_with_low_frequency_input() {
    let n = 32768;
    let x: Vec<_> = (0..n)
        .map(|i| 0.5 * libm::sinf(std::f32::consts::TAU * 64.0 * i as f32 / n as f32))
        .collect();
    let magnitude = |a: &[f32], bin: usize| {
        let mut re = 0.0;
        let mut im = 0.0;
        for (i, &x) in a.iter().enumerate().skip(n / 2) {
            let phase = std::f64::consts::TAU * bin as f64 * i as f64 / n as f64;
            re += x as f64 * libm::cos(phase);
            im += x as f64 * libm::sin(phase);
        }
        re * re + im * im
    };
    let raw = run(
        &mut setup(48000.0, &[(0, 24.0), (1, 8.0), (2, 0.0)]),
        &x,
        128,
    )
    .0;
    let filtered = run(
        &mut setup(48000.0, &[(0, 24.0), (1, 8.0), (2, 1.0)]),
        &x,
        127,
    )
    .0;
    let image = n / 8 - 64;
    let ratio = magnitude(&filtered, image) / magnitude(&raw, image);
    assert!(ratio < 0.025, "image power ratio {ratio}");
    assert!(magnitude(&filtered, 64) / magnitude(&raw, 64) > 0.9);
}
#[test]
fn dither_stream_is_not_affected_by_jitter_draws() {
    // Independent scalar PRNG reference pins the promised channel/purpose stream;
    // sharing jitter draws changes this sample sequence even when jitter amount is zero.
    fn stream(seed: u32, tag: u32) -> u32 {
        let mut x = seed.wrapping_add(tag);
        x = (x ^ (x >> 16)).wrapping_mul(0x7feb_352d);
        x = (x ^ (x >> 15)).wrapping_mul(0x846c_a68b);
        (x ^ (x >> 16)).max(1)
    }
    fn unit(state: &mut u32) -> f32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        (*state >> 8) as f32 / 16777216.0
    }
    let mut state = stream(77, 0x1357_9bdf ^ 0xa511_e9b3);
    let x = vec![0.13; 512];
    let (a, _) = run(
        &mut seeded(77, &[(0, 3.0), (1, 1.0), (2, 0.0), (4, 1.0)]),
        &x,
        7,
    );
    for y in a {
        let noise = unit(&mut state) - unit(&mut state);
        let want = libm::roundf(0.13 * 4.0 + noise) / 4.0;
        assert_eq!(y, want);
    }
}
#[test]
fn nan_rejection_and_reset_rearm_are_observable() {
    let values = [(0, 3.0), (1, 1.0), (2, 0.0), (5, 0.5)];
    let x = [0.37; 128];
    let expected = run(&mut setup(48000.0, &values), &x, 128);
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut d = setup(48000.0, &values);
        d.set_param(5, invalid);
        assert_eq!(expected, run(&mut d, &x, 7));
    }
    let mut d = setup(48000.0, &values);
    run(&mut d, &x, 128);
    d.set_param(5, 1.0);
    run(&mut d, &x, 7);
    d.reset();
    assert_eq!(
        run(&mut d, &x, 64),
        run(
            &mut setup(48000.0, &[(0, 3.0), (1, 1.0), (2, 0.0), (5, 1.0)]),
            &x,
            1
        )
    );
    d.reset();
    d.set_param(5, 0.0);
    assert_eq!(run(&mut d, &x, 7).0, x);
}
#[test]
fn fractional_bits_and_positive_endpoint() {
    let scale = libm::exp2f(1.5);
    let x = [-1.0, -0.6, 0.2, 0.6, 1.0, 2.0];
    let (a, _) = run(&mut setup(48000.0, &[(0, 2.5), (1, 1.0), (2, 0.0)]), &x, 1);
    for (&y, &x) in a.iter().zip(&x) {
        let want = libm::roundf(x.clamp(-1.0, 1.0) * scale).clamp(-scale, scale - 1.0) / scale;
        assert!((y - want).abs() < 1e-7);
    }
    assert!((a[4] - (1.0 - 1.0 / scale)).abs() < 1e-7);
    assert_ne!(a[3], 0.5);
}
#[test]
fn shortened_period_bounds_current_wait_and_empty_calls_do_nothing() {
    let x: Vec<_> = (0..512).map(|i| 0.01 + i as f32 / 1024.0).collect();
    let render = |block| {
        let mut d = setup(48000.0, &[(0, 24.0), (1, 128.0), (2, 0.0)]);
        run(&mut d, &[0.0; 2], 1);
        d.set_param(1, 1.0);
        run(&mut d, &x, block)
    };
    let a = render(1);
    assert_eq!(a.0.iter().position(|&x| x != 0.0), Some(1));
    let edges: Vec<_> =
        a.0.windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] != w[1])
            .map(|(i, _)| i + 1)
            .collect();
    assert!(
        edges[1] - edges[0] > 80,
        "future holds still use smoothed period"
    );
    for b in [7, 64, 127, 128] {
        assert_eq!(a, render(b));
    }
    for partial in [false, true] {
        let mut d = setup(48000.0, &[]);
        if partial {
            d.process(&mut [0.4; 1], &mut [], None, &[]);
        } else {
            d.process(&mut [], &mut [], None, &[]);
        }
        d.set_param(5, 0.0);
        assert_eq!(run(&mut d, &x, 7).0, x);
    }
}

#[test]
fn silent_captures_advance_dither_and_empty_running_calls_preserve_state() {
    let values = [(0, 3.0), (1, 1.0), (2, 0.0), (4, 1.0)];
    let mut a = seeded(99, &values);
    let mut b = seeded(99, &values);
    run(&mut a, &[0.0; 128], 7);
    run(&mut b, &[0.0001; 128], 64);
    assert_eq!(run(&mut a, &[0.13; 512], 7), run(&mut b, &[0.13; 512], 128));
    let values = preset("Loose Grain").unwrap();
    let mut a = seeded(99, &values);
    let mut b = seeded(99, &values);
    run(&mut a, &[0.4; 73], 7);
    run(&mut b, &[0.4; 73], 7);
    a.process(&mut [], &mut [], None, &[]);
    let mut untouched = [0.25; 1];
    a.process(&mut untouched, &mut [], None, &[]);
    assert_eq!(untouched, [0.25]);
    assert_eq!(run(&mut a, &[0.13; 512], 7), run(&mut b, &[0.13; 512], 7));
}
#[test]
fn repeating_period_control_does_not_truncate_jittered_holds() {
    let values = [(0, 24.0), (1, 8.0), (2, 0.0), (3, 0.45)];
    let x: Vec<_> = (0..512).map(|i| 0.01 + i as f32 / 1024.0).collect();
    let expected = run(&mut setup(48000.0, &values), &x, 1);
    let mut d = setup(48000.0, &values);
    let mut a = x.clone();
    let mut b = x.clone();
    for (l, r) in a.iter_mut().zip(&mut b) {
        d.set_param(1, 8.0);
        d.process(std::slice::from_mut(l), std::slice::from_mut(r), None, &[]);
    }
    assert_eq!(expected, (a, b));
}

#[test]
fn jitter_uses_its_own_seeded_stream() {
    // Independently calculated xorshift hold timestamps for seed 77, left channel.
    // Dither draws (even at amount zero) must not change these clock timestamps.
    let captures = [
        0, 8, 18, 29, 35, 45, 53, 64, 74, 80, 88, 99, 108, 119, 126, 138, 145, 156, 167, 172, 181,
        192, 202, 207, 216, 226, 234, 244, 250,
    ];
    let x: Vec<_> = (0..256).map(|i| 0.01 + i as f32 / 512.0).collect();
    let (a, _) = run(
        &mut seeded(77, &[(0, 24.0), (1, 8.0), (2, 0.0), (3, 0.45)]),
        &x,
        7,
    );
    let mut observed = vec![0];
    observed.extend(
        a.windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] != w[1])
            .map(|(i, _)| i + 1),
    );
    assert_eq!(observed, captures);
}

#[test]
fn stream_derivation_cannot_zero_lock_any_purpose_or_channel() {
    for tag in [
        0x1357_9bdfu32,
        0x2468_ace0,
        0x1357_9bdf ^ 0xa511_e9b3,
        0x2468_ace0 ^ 0xa511_e9b3,
    ] {
        // This root makes one avalanche input exactly zero.
        let seed = tag.wrapping_neg();
        let (l, r) = run(
            &mut seeded(seed, &[(0, 3.0), (1, 1.0), (2, 0.0), (4, 1.0)]),
            &[0.13; 2048],
            128,
        );
        for channel in [&l, &r] {
            assert!(channel.windows(2).any(|w| w[0] != w[1]));
        }
        let x: Vec<_> = (0..2048).map(|i| 0.01 + i as f32 / 4096.0).collect();
        let (l, r) = run(
            &mut seeded(seed, &[(0, 24.0), (1, 8.0), (2, 0.0), (3, 0.45)]),
            &x,
            127,
        );
        for channel in [&l, &r] {
            let edges: Vec<_> = channel
                .windows(2)
                .enumerate()
                .filter(|(_, w)| w[0] != w[1])
                .map(|(i, _)| i + 1)
                .collect();
            assert!(edges.windows(2).any(|w| w[1] - w[0] != 8));
        }
    }
}
