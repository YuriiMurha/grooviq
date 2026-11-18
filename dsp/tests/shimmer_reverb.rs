use grooviq_dsp::{
    Device,
    shimmer_reverb::{PRESETS, SPEC, ShimmerReverb, preset},
};
fn render(sr: f32, block: usize, seed: u64, patch: &str) -> Vec<f32> {
    let mut d = ShimmerReverb::with_seed(sr, seed);
    for (i, v) in preset(patch).unwrap() {
        d.set_param(i, v);
    }
    let mut l = vec![0.; sr as usize];
    let mut r = l.clone();
    for n in 0..l.len() / 3 {
        l[n] = libm::sinf(n as f32 * 0.131) * 0.3;
        r[n] = libm::cosf(n as f32 * 0.217) * 0.2;
    }
    // Control events occur at fixed sample indices, independently of host block sizes.
    for start in (0..l.len()).step_by(997) {
        d.set_param(2, (start % 3000) as f32 / 1000.);
        let end = (start + 997).min(l.len());
        for (a, b) in l[start..end]
            .chunks_mut(block)
            .zip(r[start..end].chunks_mut(block))
        {
            d.process(a, b, None, &[]);
        }
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}
#[test]
fn sample_events_partitions_and_seed_streams() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        for p in PRESETS {
            let a = render(sr, 128, 77, p);
            for b in [1, 7, 64, 127] {
                assert_eq!(a, render(sr, b, 77, p));
            }
            assert_ne!(a, render(sr, 128, 78, p));
            assert!(a.iter().all(|x| x.is_finite()));
        }
    }
}
#[test]
fn hostile_inputs_controls_and_reset_match_fresh() {
    for sr in [f32::NAN, -1., 22050., 48000., 192000., f32::INFINITY] {
        let mut d = ShimmerReverb::new(sr);
        for n in 0..500 {
            for (i, (lo, hi, _)) in SPEC.iter().enumerate() {
                d.set_param(i as u32, if n % 2 == 0 { *lo } else { *hi });
                d.set_param(i as u32, f32::NAN);
            }
            let mut l = [
                f32::MAX,
                f32::NAN,
                f32::INFINITY,
                -f32::MAX,
                1e-22,
                0.,
                -0.1,
            ];
            let mut r = l;
            d.process(&mut l, &mut r, None, &[]);
            assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() <= 8.));
        }
        for (i, v) in preset("Fifth mist").unwrap() {
            d.set_param(i, v);
        }
        d.reset();
        let mut fresh = ShimmerReverb::new(sr);
        for (i, v) in preset("Fifth mist").unwrap() {
            fresh.set_param(i, v);
        }
        for _ in 0..1000 {
            let (mut a, mut b, mut c, mut e) = ([0.3; 7], [-0.1; 7], [0.3; 7], [-0.1; 7]);
            d.process(&mut a, &mut b, None, &[]);
            fresh.process(&mut c, &mut e, None, &[]);
            assert_eq!((a, b), (c, e));
        }
    }
}

fn impulse(sr: f32, feedback: f32, decay: f32, diffusion: f32, modulation: f32) -> Vec<f32> {
    let mut d = ShimmerReverb::new(sr);
    for (i, v) in [
        (0, 1.),
        (1, decay),
        (2, 1.37),
        (3, feedback),
        (4, diffusion),
        (5, modulation),
    ] {
        d.set_param(i, v);
    }
    let mut l = vec![0.; sr as usize * 3];
    let mut r = l.clone();
    l[0] = 1.;
    r[0] = -0.7;
    for (a, b) in l.chunks_mut(127).zip(r.chunks_mut(127)) {
        d.process(a, b, None, &[]);
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}
fn energy(a: &[f32]) -> f64 {
    a.iter().map(|x| (*x as f64) * (*x as f64)).sum()
}
#[test]
fn physical_wet_diffusion_modulation_and_feedback_decay() {
    for sr in [22050., 48000., 192000.] {
        let dry_feedback = impulse(sr, 0., 20., 0., 0.);
        let short = impulse(sr, 0.98, 0.2, 0., 0.);
        let long = impulse(sr, 0.98, 20., 0., 0.);
        let tail = sr as usize;
        assert_eq!(energy(&dry_feedback[tail..]), 0.);
        assert!(energy(&long[tail..]) > energy(&short[tail..]) * 100. + 1e-10);
        let low = impulse(sr, 0.4, 20., 0., 0.);
        assert!(energy(&long[tail..]) > energy(&low[tail..]) * 2.);
        let moving = impulse(sr, 0.98, 20., 0., 1.);
        let difference: Vec<_> = moving.iter().zip(&long).map(|(a, b)| a - b).collect();
        assert!(energy(&difference) > energy(&long) * 0.01);
        // Wet-only has physical propagation delay and no direct impulse leakage.
        assert!(long[..(sr * 0.069) as usize * 2].iter().all(|v| *v == 0.));
        assert!(energy(&long) > 1e-6);
    }
}

#[test]
fn worst_endpoint_combinations_decay() {
    for sr in [22050., 48000., 192000.] {
        for pitch in [0., 1., 2., 3., 1.37] {
            for diffusion in [0., 1.] {
                for modulation in [0., 1.] {
                    let mut d = ShimmerReverb::new(sr);
                    for (i, v) in [
                        (0, 1.),
                        (1, 20.),
                        (2, pitch),
                        (3, 0.98),
                        (4, diffusion),
                        (5, modulation),
                    ] {
                        d.set_param(i, v);
                    }
                    let mut early = 0.;
                    let mut late = 0.;
                    for n in 0..(sr as usize * 8 / 128) {
                        let (mut l, mut r) = ([0.; 128], [0.; 128]);
                        if n == 0 {
                            l[0] = 1.;
                            r[0] = -0.7;
                        }
                        d.process(&mut l, &mut r, None, &[]);
                        let e = energy(&l) + energy(&r);
                        if n < sr as usize / 128 {
                            early += e;
                        }
                        if n > sr as usize * 7 / 128 {
                            late += e;
                        }
                    }
                    assert!(early > 1e-7);
                    assert!(
                        late < early * 1e-4,
                        "{sr} pitch {pitch} diff {diffusion} mod {modulation}: {early} -> {late}"
                    );
                }
            }
        }
    }
}

#[test]
fn mix_is_linear_and_empty_calls_preserve_fresh_preset() {
    let sr = 22050.;
    let mut devices: [ShimmerReverb; 3] = std::array::from_fn(|_| ShimmerReverb::new(sr));
    for (d, mix) in devices.iter_mut().zip([0., 0.37, 1.]) {
        d.process(&mut [], &mut [], None, &[]);
        d.set_param(0, mix);
    }
    let mut wet_energy = 0.;
    for n in 0..22050 {
        let input = [
            libm::sinf(n as f32 * 0.31) * 0.2,
            libm::cosf(n as f32 * 0.17) * 0.3,
        ];
        let mut out = [[0.; 2]; 3];
        for (d, o) in devices.iter_mut().zip(&mut out) {
            let (mut l, mut r) = ([input[0]], [input[1]]);
            d.process(&mut l, &mut r, None, &[]);
            *o = [l[0], r[0]];
        }
        assert_eq!(out[0], input);
        for ch in 0..2 {
            assert!((out[1][ch] - (0.63 * input[ch] + 0.37 * out[2][ch])).abs() < 1e-7);
        }
        wet_energy += energy(&out[2]);
    }
    assert!(wet_energy > 0.01);
}
