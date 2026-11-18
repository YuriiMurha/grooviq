use grooviq_dsp::{
    Device,
    fattener::{Fattener, PRESETS, SPEC, preset},
};

#[test]
fn presets_live_controls_and_reset_are_realtime_safe() {
    for sr in [22050.0, 48000.0, 192000.0] {
        for name in PRESETS {
            let mut device = Fattener::new(sr);
            let params = preset(name).expect("a factory preset");
            let source_l: [f32; 128] = std::array::from_fn(|i| {
                libm::sinf(i as f32 * 0.29) * if i < 16 { 0.8 } else { 0.1 }
            });
            let source_r: [f32; 128] = std::array::from_fn(|i| source_l[127 - i] * 0.25);
            let (mut l, mut r) = ([0.0; 128], [0.0; 128]);
            let mut peak = 0.0_f32;
            let mut finite = true;
            let mut preset_delta = [0.0_f32; 2];
            super::realtime(|| {
                for &(index, value) in &params {
                    device.set_param(index, value);
                }
                l.copy_from_slice(&source_l);
                r.copy_from_slice(&source_r);
                device.process(&mut l, &mut r, None, &[]);
                for ((left, right), (dry_l, dry_r)) in
                    l.iter().zip(&r).zip(source_l.iter().zip(&source_r))
                {
                    finite &= left.is_finite() && right.is_finite();
                    preset_delta[0] = preset_delta[0].max((left - dry_l).abs());
                    preset_delta[1] = preset_delta[1].max((right - dry_r).abs());
                }
                for (n, block) in [1, 7, 64, 127, 128]
                    .into_iter()
                    .cycle()
                    .take(100)
                    .enumerate()
                {
                    for (index, &(lo, hi, _)) in SPEC.iter().enumerate() {
                        device.set_param(index as u32, if n % 2 == 0 { lo } else { hi });
                    }
                    l.copy_from_slice(&source_l);
                    r.copy_from_slice(&source_r);
                    device.process(&mut l[..block], &mut r[..block], None, &[]);
                    for &sample in l[..block].iter().chain(&r[..block]) {
                        finite &= sample.is_finite();
                        peak = peak.max(sample.abs());
                    }
                    if n % 9 == 0 {
                        device.reset();
                    }
                }
                device.reset();
            });
            assert!(finite, "{name} at {sr} Hz produced nonfinite output");
            assert!(
                preset_delta.iter().all(|delta| *delta > 1.0e-3),
                "{name} at {sr} Hz did not change the source signal"
            );
            assert!(peak > 0.01, "{name} at {sr} Hz did not process sound");
        }
    }
}
