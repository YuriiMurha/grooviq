use grooviq_dsp::{
    Device,
    saturation::{PRESETS, SPEC, Saturation, preset},
};
#[test]
fn saturation_realtime() {
    for sr in [22050., 48000., 192000.] {
        for p in PRESETS {
            let mut d = Saturation::new(sr);
            let mut reference = Saturation::new(sr);
            for (i, v) in preset(p).unwrap() {
                d.set_param(i, v);
            }
            let expected = match p {
                "Init" => [0., 0., 0., 0., 20., 20000., 1., 1., 0.],
                "Amber filament" => [0., 12., 0.2, 3., 45., 16000., 1., 0.85, 0.],
                "Soft spool" => [1., 18., -0.35, 2., 30., 11000., 1., 1., 1.],
                "Edge current" => [2., 16., 0.4, 6., 90., 18000., 1., 0.7, -2.],
                "Desk glow" => [3., 8., 0.1, 2., 35., 17000., 1., 1., 0.],
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
                    d.reset();
                }
            });
            assert!(finite);
        }
    }
}
