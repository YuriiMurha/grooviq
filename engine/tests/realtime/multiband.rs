use grooviq_dsp::{
    Device,
    ott::{Ott, PRESETS, SPEC, preset},
};

// Independent of preset(): skipping or misapplying a factory patch must fail, even though
// the default device also changes dry input. Keep all 17 controls explicit.
const EXPECTED: [[f32; 17]; 7] = [
    [
        0.35, 0.6, 1.0, 0.4, 0.0, 0.0, 1.5, 120.0, 2500.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ],
    [
        0.6, 0.25, 0.5, 1.0, 1.0, 0.0, 2.0, 100.0, 3500.0, -2.0, 0.0, 1.0, 0.8, 0.6, 1.0, 1.0, 1.0,
    ],
    [
        0.5, 0.7, 1.0, 0.3, 0.0, 0.0, 3.0, 150.0, 5000.0, -2.0, 0.0, 0.2, 0.6, 1.0, 0.2, 0.4, 0.2,
    ],
    [
        0.7, 0.5, 0.6, 1.0, 2.0, -1.0, -2.0, 160.0, 2000.0, -1.0, 0.0, 0.5, 0.3, 0.0, 1.0, 1.0, 1.0,
    ],
    [
        1.0, 0.4, 1.0, 1.0, 2.0, -1.0, 3.0, 120.0, 2500.0, -6.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ],
    [
        0.8, 0.5, 1.0, 0.0, 0.0, 0.0, 1.0, 120.0, 2500.0, -2.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ],
    [
        0.45, 0.35, 0.0, 1.0, 0.0, 0.0, 0.0, 120.0, 2500.0, 0.0, 2.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ],
];

fn input(rate: f32) -> ([f32; 2048], [f32; 2048]) {
    let channel = |shift: f32| {
        core::array::from_fn(|i| {
            let t = i as f32 / rate;
            let amp = if i % 512 < 128 { 0.55 } else { 0.006 };
            amp * (libm::sinf(t * (440.0 + shift))
                + 0.5 * libm::sinf(t * (6000.0 + shift))
                + 0.3 * libm::sinf(t * (36000.0 + shift)))
        })
    };
    (channel(0.0), channel(123.0))
}

#[test]
fn multiband_presets_controls_and_reset_are_realtime_safe() {
    for rate in [22050.0, 48000.0, 192000.0] {
        let dry = input(rate);
        for (patch, name) in PRESETS.iter().enumerate() {
            let params = preset(name).unwrap();
            let mut reference = Ott::new(rate);
            for (index, value) in EXPECTED[patch].iter().enumerate() {
                reference.set_param(index as u32, *value);
            }
            let mut expected = dry;
            reference.process(&mut expected.0, &mut expected.1, None, &[]);
            let mut device = Ott::new(rate);
            let mut actual = dry;
            let mut finite = true;
            let mut matches = true;
            let mut changed = [false; 2];
            let mut band_has_signal = [false; 3];
            let mut meter_min = [f32::INFINITY; 6];
            let mut meter_max = [f32::NEG_INFINITY; 6];
            super::realtime(|| {
                for &(index, value) in &params {
                    device.set_param(index, value);
                }
                // Check before reset or control stress can conceal a bad preset result.
                device.process(&mut actual.0, &mut actual.1, None, &[]);
                for (channel, (out, want, source)) in [
                    (&actual.0, &expected.0, &dry.0),
                    (&actual.1, &expected.1, &dry.1),
                ]
                .into_iter()
                .enumerate()
                {
                    for i in 0..out.len() {
                        finite &= out[i].is_finite();
                        matches &= out[i].to_bits() == want[i].to_bits();
                        changed[channel] |= (out[i] - source[i]).abs() > 1e-4;
                    }
                }
                for (band, has_signal) in band_has_signal.iter_mut().enumerate() {
                    *has_signal = device.meter(band as u32) > -100.0;
                }
                for (index, &(min, max, default)) in SPEC.iter().enumerate() {
                    for value in [
                        min,
                        max,
                        default,
                        f32::NAN,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                    ] {
                        device.set_param(index as u32, value);
                        for size in [1, 7, 64, 127, 128] {
                            let mut l = dry.0;
                            let mut r = dry.1;
                            device.process(&mut l[..size], &mut r[..size], None, &[]);
                            finite &= l[..size].iter().chain(&r[..size]).all(|x| x.is_finite());
                            for meter in 0..6 {
                                let v = device.meter(meter);
                                finite &= v.is_finite();
                                meter_min[meter as usize] = meter_min[meter as usize].min(v);
                                meter_max[meter as usize] = meter_max[meter as usize].max(v);
                            }
                        }
                    }
                }
                device.reset();
                for meter in 0..6 {
                    finite &= device.meter(meter).is_finite();
                }
                let mut l = dry.0;
                let mut r = dry.1;
                device.process(&mut l, &mut r, None, &[]);
                finite &= l.iter().chain(&r).all(|x| x.is_finite());
            });
            assert!(
                (0..6).all(|i| meter_max[i] - meter_min[i] > 0.01),
                "{name} at {rate}: one or more meters did not vary under stress"
            );
            assert!(finite, "{name} at {rate}: nonfinite output/meter");
            assert!(
                matches,
                "{name} at {rate}: explicit preset reference differs"
            );
            assert!(
                changed.into_iter().all(|x| x),
                "{name} at {rate}: one or both channels were left dry"
            );
            assert!(
                band_has_signal.into_iter().all(|x| x),
                "{name} at {rate}: one or more bands carried no measurable signal"
            );
        }
    }
}
