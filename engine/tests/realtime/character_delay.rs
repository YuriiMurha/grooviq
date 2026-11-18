use grooviq_dsp::{
    Device,
    character_delay::{CharacterDelay, PRESETS, SPEC, preset},
};
#[test]
fn character_delay_factory_and_controls_realtime() {
    for sr in [22050.0, 192000.0] {
        for name in PRESETS {
            let patch = preset(name).unwrap();
            let mut d = CharacterDelay::with_seed(sr, 17);
            for &(i, v) in &patch {
                d.set_param(i, v);
            }
            // Independent expected factory patch; catches preset skipping even when default processes.
            let model = match name {
                "Glass Steps" => 0.0,
                "Soft Spools" => 1.0,
                _ => 2.0,
            };
            let (amount, depth, duck) = match name {
                "Glass Steps" => (0.0, 0.0, 0.0),
                "Soft Spools" => (0.65, 0.2, 0.25),
                _ => (0.8, 0.1, 0.4),
            };
            let mut reference = CharacterDelay::with_seed(sr, 17);
            reference.set_param(9, model);
            reference.set_param(10, amount);
            reference.set_param(12, depth);
            reference.set_param(11, duck);
            let mut finite = true;
            let mut same = true;
            let mut changed = false;
            let mut wet = false;
            for k in 0..(sr as usize / 64) {
                let mut l = std::array::from_fn::<_, 64, _>(|i| {
                    if k < 4 {
                        libm::sinf((k * 64 + i) as f32 * 0.2) * 0.9
                    } else {
                        0.0
                    }
                });
                let mut r = l.map(|x| x * 0.7);
                let dry = l;
                let (mut a, mut b) = (l, r);
                super::realtime(|| d.process(&mut l, &mut r, None, &[]));
                reference.process(&mut a, &mut b, None, &[]);
                finite &= l.iter().chain(&r).all(|v| v.is_finite());
                same &= l == a && r == b;
                changed |= l != dry;
                wet |= k > 100 && l.iter().any(|v| v.abs() > 0.00001);
            }
            assert!(finite && same && changed && wet, "{name} at {sr}");
            let mut finite = true;
            super::realtime(|| {
                for (i, s) in SPEC.iter().enumerate() {
                    for v in [s.0, s.1, f32::NAN, f32::INFINITY] {
                        d.set_param(i as u32, v);
                        let mut l = [0.2; 127];
                        let mut r = [-0.1; 127];
                        d.process(&mut l, &mut r, None, &[]);
                        finite &= l.iter().chain(&r).all(|x| x.is_finite());
                    }
                }
                // Exercise completed fades, active queued targets and reset under the guard.
                d.set_param(0, 0.0);
                for block in 0..(sr as usize / 64) {
                    d.set_param(2, if block % 2 == 0 { 20.0 } else { 20.05 });
                    let mut l = [0.0; 64];
                    let mut r = [0.0; 64];
                    d.process(&mut l, &mut r, None, &[]);
                    if block == 5 {
                        d.reset();
                    }
                    finite &= l.iter().chain(&r).all(|x| x.is_finite());
                }
                d.set_tempo(20.0);
                d.reset();
                d.process(&mut [], &mut [], None, &[]);
                let mut l = [0.0; 1];
                let mut r = [0.5; 1];
                d.process(&mut l, &mut r, None, &[]);
                for i in 0..8 {
                    finite &= d.meter(i).is_finite();
                }
            });
            assert!(finite);
        }
    }
}
