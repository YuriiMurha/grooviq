use super::realtime;
use grooviq_dsp::{
    Device,
    lofi_colour::{LofiColour, SPEC, preset},
};

// An independent parameter fixture keeps a skipped/misapplied preset observable,
// even when the default device would also produce a finite, non-dry signal.
const PATCHES: [(&str, [f32; 8]); 3] = [
    ("Paper Haze", [0.7, 0.3, 0.2, 0.3, 0.2, 0.2, 0.1, 0.]),
    ("Bent Ribbon", [0.9, 0.2, 0.8, 0.6, 0.3, 0.3, 0.7, 1.]),
    ("Small Hours", [0.6, 0.4, 0.3, 0.2, 0.7, 0.8, 0.3, 2.]),
];

fn source() -> ([f32; 128], [f32; 128]) {
    let l = std::array::from_fn(|i| {
        let envelope = if i < 12 { 0.8 } else { 0.15 };
        envelope * libm::sinf(i as f32 * 0.31)
    });
    let r = std::array::from_fn(|i| {
        let envelope = if i < 23 { 0.6 } else { 0.12 };
        envelope * libm::cosf(i as f32 * 0.19)
    });
    (l, r)
}

#[test]
fn seeded_presets_controls_and_reset_are_realtime() {
    let (source_l, source_r) = source();
    for rate in [22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
        for seed in [0, 77, u32::MAX] {
            for (name, values) in PATCHES {
                let mut reference = LofiColour::with_seed(rate, seed);
                for (i, value) in values.into_iter().enumerate() {
                    reference.set_param(i as u32, value);
                }
                let (mut expected_l, mut expected_r) = (source_l, source_r);
                reference.process(&mut expected_l, &mut expected_r, None, &[]);
                let patch = preset(name).expect("factory patch");
                let mut device = LofiColour::with_seed(rate, seed);
                let (mut l, mut r) = (source_l, source_r);
                realtime(|| {
                    for &(i, value) in &patch {
                        device.set_param(i, value);
                    }
                    // Baseline runs before stress can overwrite the preset settings.
                    device.process(&mut l, &mut r, None, &[]);
                });
                // All reporting/formatting lives outside the realtime guard.
                assert!(
                    l.iter().chain(&r).all(|v| v.is_finite()),
                    "{name} {rate} {seed}: baseline finite"
                );
                for (wet, dry) in [(&l, &source_l), (&r, &source_r)] {
                    assert!(wet.iter().any(|v| v.abs() > 1e-6), "{name}: active output");
                    assert!(
                        wet.iter().zip(dry).any(|(a, b)| (a - b).abs() > 1e-5),
                        "{name}: effect must change dry input"
                    );
                }
                assert_eq!(l, expected_l, "{name} {rate} {seed}: left patch");
                assert_eq!(r, expected_r, "{name} {rate} {seed}: right patch");
                let mut finite = true;
                realtime(|| {
                    for block in [1, 7, 64, 127, 128] {
                        for field in 0..3 {
                            for (i, &(lo, hi, default)) in SPEC.iter().enumerate() {
                                let value = match field {
                                    0 => lo,
                                    1 => hi,
                                    _ => default,
                                };
                                device.set_param(i as u32, value);
                            }
                            // Fresh audio, continuously evolving state and smoothed controls.
                            l.copy_from_slice(&source_l);
                            r.copy_from_slice(&source_r);
                            device.process(&mut l[..block], &mut r[..block], None, &[]);
                            finite &= l[..block].iter().chain(&r[..block]).all(|v| v.is_finite());
                            device.reset();
                            l.copy_from_slice(&source_l);
                            r.copy_from_slice(&source_r);
                            device.process(&mut l[..block], &mut r[..block], None, &[]);
                            finite &= l[..block].iter().chain(&r[..block]).all(|v| v.is_finite());
                        }
                    }
                    for i in 0..SPEC.len() {
                        device.set_param(i as u32, f32::NAN);
                    }
                    device.process(&mut [], &mut [], None, &[]);
                    for &(i, value) in &patch {
                        device.set_param(i, value);
                    }
                    device.reset();
                    l.copy_from_slice(&source_l);
                    r.copy_from_slice(&source_r);
                    device.process(&mut l, &mut r, None, &[]);
                    finite &= l.iter().chain(&r).all(|v| v.is_finite());
                });
                assert!(finite, "{name} {rate} {seed}: stress finite");
                assert_eq!(l, expected_l, "{name} {rate} {seed}: reset left");
                assert_eq!(r, expected_r, "{name} {rate} {seed}: reset right");
                realtime(|| {
                    device.reset();
                    device.set_param(4, 1.0);
                    l.fill(0.0);
                    r.fill(0.0);
                    device.process(&mut l, &mut r, None, &[]);
                });
                assert!(
                    l.iter().chain(&r).all(|&v| v == 0.0),
                    "{name} {rate} {seed}: max-dither silence"
                );
            }
        }
    }
}
