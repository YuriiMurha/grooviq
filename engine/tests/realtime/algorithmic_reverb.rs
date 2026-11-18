use grooviq_dsp::{
    Device,
    algorithmic_reverb::{AlgorithmicReverb, PRESETS, SPEC, preset},
};
#[test]
fn charged_presets_controls_odd_blocks_and_reset() {
    for sr in [22050., 48000., 192000.] {
        for (model, name) in PRESETS.iter().enumerate() {
            let params = preset(name).unwrap();
            let mut d = AlgorithmicReverb::new(sr);
            let mut reference = AlgorithmicReverb::new(sr);
            // Independent literal factory patches catch a skipped preset application.
            let manual: [[f32; 13]; 5] = [
                [
                    0., 0., 8., 0.7, 0.2, 0.6, 0.1, 100., 16000., 1., 0.3, 0., 0.2,
                ],
                [
                    1., 1., 35., 4., 0.9, 0.9, 0.5, 100., 12000., 1.3, 0.35, -2., 0.4,
                ],
                [
                    2., 0., 12., 2.4, 0.65, 1., 0.35, 180., 18000., 1.1, 0.3, -1., 0.3,
                ],
                [
                    3., 2., 20., 1.3, 0.4, 0.75, 0.15, 70., 9000., 0.9, 0.35, 0., 0.3,
                ],
                [
                    4., 1., 5., 1., 0.7, 0.8, 0.4, 120., 14000., 1.4, 0.5, -2., 0.4,
                ],
            ];
            for (i, v) in manual[model].into_iter().enumerate() {
                reference.set_param(i as u32, v);
            }
            let mut finite = true;
            let mut equal = true;
            let mut wet_peak = 0f32;
            super::realtime(|| {
                for &(i, v) in &params {
                    d.set_param(i, v);
                }
            });
            // Reach real feedback/reflections even at maximum rate; compare before stress.
            for block in 0..(sr as usize * 2 / 127) {
                let src: [f32; 127] =
                    std::array::from_fn(|i| libm::sinf((block * 127 + i) as f32 * 0.13) * 0.2);
                let mut l = src;
                let mut r = src.map(|v| v * -0.3);
                let mut x = l;
                let mut y = r;
                reference.process(&mut x, &mut y, None, &[]);
                super::realtime(|| {
                    d.process(&mut l, &mut r, None, &[]);
                });
                equal &= l == x && r == y;
                finite &= l.iter().chain(&r).all(|v| v.is_finite());
            }
            assert!(equal && finite, "preset {name} at {sr}");
            super::realtime(|| {
                d.set_param(10, 1.);
                d.set_param(2, 250.);
            });
            for block in 0..(sr as usize / 127) {
                let mut l = std::array::from_fn::<_, 127, _>(|i| {
                    if block < sr as usize / 254 {
                        libm::sinf((block * 127 + i) as f32 * 0.19) * 0.3
                    } else {
                        0.0
                    }
                });
                let mut r = l.map(|v| v * 0.2);
                super::realtime(|| {
                    d.process(&mut l, &mut r, None, &[]);
                });
                if block > sr as usize / 254 {
                    for &v in l.iter().chain(&r) {
                        wet_peak = wet_peak.max(v.abs());
                        finite &= v.is_finite();
                    }
                }
            }
            assert!(finite && wet_peak > 1e-5, "wet {name} at {sr}");
            super::realtime(|| {
                for n in 0..100 {
                    for (i, &(lo, hi, _)) in SPEC.iter().enumerate() {
                        d.set_param(i as u32, if n % 2 == 0 { lo } else { hi });
                    }
                    let len = [1, 7, 64, 127, 128][n % 5];
                    let mut l = [0.1; 128];
                    let mut r = [-0.2; 128];
                    d.process(&mut l[..len], &mut r[..len], None, &[]);
                    finite &= l.iter().chain(&r).all(|v| v.is_finite());
                    if n % 9 == 0 {
                        d.reset();
                    }
                }
                d.reset();
            });
            assert!(finite);
        }
    }
}
