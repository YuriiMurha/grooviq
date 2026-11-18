use grooviq_dsp::{
    Device,
    shimmer_reverb::{PRESETS, SPEC, ShimmerReverb, preset},
};
#[test]
fn shimmer_reverb_realtime() {
    for sr in [22050., 48000., 192000.] {
        for p in PRESETS {
            let mut d = ShimmerReverb::new(sr);
            let mut reference = ShimmerReverb::new(sr);
            for (i, v) in preset(p).unwrap() {
                d.set_param(i, v);
            }
            let expected = match p {
                "Init" => [0.15, 4., 0., 0.65, 0.7, 0.2],
                "Upper canopy" => [0.5, 12., 0., 0.9, 0.85, 0.4],
                "Fifth mist" => [0.45, 9., 1., 0.85, 0.9, 0.65],
                "Low lantern" => [0.4, 7., 2., 0.8, 0.6, 0.3],
                "Falling glass" => [0.5, 10., 3., 0.88, 0.8, 0.5],
                _ => unreachable!(),
            };
            for (i, v) in expected.into_iter().enumerate() {
                reference.set_param(i as u32, v);
            }
            let mut l = [0.; 128];
            let mut r = l;
            for i in 0..128 {
                l[i] = libm::sinf(i as f32 * 0.31) * 0.8;
                r[i] = libm::cosf(i as f32 * 0.17) * 0.6;
            }
            let dry = l;
            let (mut a, mut b) = (l, r);
            reference.process(&mut a, &mut b, None, &[]);
            super::realtime(|| d.process(&mut l, &mut r, None, &[]));
            assert_eq!((l, r), (a, b));
            assert_ne!(l, dry);
            assert!(l.iter().chain(&r).all(|x| x.is_finite()));
            assert!(l.iter().any(|x| x.abs() > 0.0001));
            // Exercise charged wet paths before stress: dry attenuation alone is insufficient.
            d.set_param(0, 1.);
            reference.set_param(0, 1.);
            let mut wet_energy = 0.;
            for round in 0..(sr as usize / 128) {
                l.fill(if round < 8 { 0.3 } else { 0. });
                r.fill(if round < 8 { -0.2 } else { 0. });
                let (mut a, mut b) = (l, r);
                reference.process(&mut a, &mut b, None, &[]);
                super::realtime(|| d.process(&mut l, &mut r, None, &[]));
                assert_eq!((l, r), (a, b));
                if round > sr as usize / 128 / 3 {
                    wet_energy += l.iter().chain(&r).map(|v| v * v).sum::<f32>();
                }
            }
            assert!(wet_energy > 1e-8, "{p} wet path is silent");
            let mut finite = true;
            super::realtime(|| {
                for round in 0..32 {
                    for (i, (lo, hi, _)) in SPEC.iter().enumerate() {
                        d.set_param(i as u32, if round % 2 == 0 { *lo } else { *hi });
                        d.set_param(i as u32, f32::NAN);
                    }
                    for n in [1, 3, 37, 64, 127, 128] {
                        for i in 0..n {
                            l[i] = libm::sinf((round * 128 + i) as f32 * 0.31);
                            r[i] = libm::cosf((round * 128 + i) as f32 * 0.11);
                        }
                        d.process(&mut l[..n], &mut r[..n], None, &[]);
                        finite &= l[..n].iter().chain(&r[..n]).all(|x| x.is_finite());
                    }
                    d.process(&mut [], &mut [], None, &[]);
                    d.reset();
                }
            });
            assert!(finite);
        }
    }
}
