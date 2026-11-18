use grooviq_dsp::{
    Device,
    saturation::{PRESETS, SPEC, Saturation, preset},
};
fn signal(n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut seed = 719u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32 - 0.5) * 0.3
    };
    (
        (0..n)
            .map(|i| libm::sinf(i as f32 * 0.731) * 0.8 + next())
            .collect(),
        (0..n)
            .map(|i| libm::sinf(i as f32 * 0.113) * 0.6 + next())
            .collect(),
    )
}
fn render(sr: f32, block: usize, patch: &str, automation: bool) -> (Vec<f32>, Vec<f32>) {
    let mut d = Saturation::new(sr);
    for (i, v) in preset(patch).unwrap() {
        d.set_param(i, v);
    }
    let (mut l, mut r) = signal(4096);
    let mut at = 0;
    while at < l.len() {
        if automation && at == 997 {
            for (i, (lo, hi, _)) in SPEC.iter().enumerate() {
                d.set_param(i as u32, if i % 2 == 0 { *hi } else { *lo });
            }
        }
        let end = (at + block).min(if at < 997 { 997 } else { l.len() });
        d.process(&mut l[at..end], &mut r[at..end], None, &[]);
        at = end;
    }
    (l, r)
}
#[test]
fn partitions() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        for p in PRESETS {
            let a = render(sr, 1, p, true);
            for b in 1..=128 {
                assert_eq!(a, render(sr, b, p, true), "{sr} {p} {b}");
            }
        }
    }
}
#[test]
fn factory_patches_have_distinct_stereo_effect() {
    let (l, r) = signal(4096);
    let mut outputs = Vec::new();
    for p in PRESETS {
        let a = render(48000., 128, p, false);
        assert!(a.0.iter().chain(&a.1).all(|x| x.is_finite()));
        assert!(a.0.iter().map(|x| x * x).sum::<f32>() > 0.01);
        assert_ne!(a.0, l);
        assert_ne!(a.1, r);
        assert_ne!(a.0, a.1);
        assert!(!outputs.contains(&a));
        outputs.push(a);
    }
}
#[test]
fn reset_empty_and_nonfinite() {
    for sr in [22050., 192000.] {
        let mut d = Saturation::new(sr);
        let mut fresh = Saturation::new(sr);
        let (mut l, mut r) = signal(512);
        d.process(&mut l, &mut r, None, &[]);
        for (i, (_, hi, _)) in SPEC.iter().enumerate() {
            d.set_param(i as u32, *hi);
            fresh.set_param(i as u32, *hi);
        }
        d.reset();
        let (mut a, mut b) = signal(512);
        let (mut c, mut e) = (a.clone(), b.clone());
        d.process(&mut a, &mut b, None, &[]);
        fresh.process(&mut c, &mut e, None, &[]);
        assert_eq!((a, b), (c, e));
        let mut empty = Saturation::new(sr);
        empty.process(&mut [], &mut [], None, &[]);
        empty.set_param(1, 23.);
        let mut f = Saturation::new(sr);
        f.set_param(1, 23.);
        for i in 0..9 {
            for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                empty.set_param(i, v);
            }
        }
        let (mut a, mut b) = signal(512);
        let (mut c, mut e) = (a.clone(), b.clone());
        empty.process(&mut a, &mut b, None, &[]);
        f.process(&mut c, &mut e, None, &[]);
        assert_eq!((a, b), (c, e));
        for m in 0..4 {
            d.set_param(0, m as f32);
            let mut a = [f32::MAX; 128];
            let mut b = [f32::NAN; 128];
            d.process(&mut a, &mut b, None, &[]);
            assert!(a.iter().chain(&b).all(|v| v.is_finite()));
        }
    }
}
#[test]
fn dry_latency_and_silence() {
    let mut d = Saturation::new(48000.);
    assert_eq!(d.latency(), 49);
    d.set_param(7, 0.);
    let mut a = [0.; 128];
    let mut b = a;
    a[0] = 1.;
    b[0] = -0.5;
    d.process(&mut a, &mut b, None, &[]);
    for i in 0..128 {
        assert_eq!(a[i], if i == 49 { 1. } else { 0. });
        assert_eq!(b[i], if i == 49 { -0.5 } else { 0. });
    }
    for p in PRESETS {
        let mut d = Saturation::new(48000.);
        for (i, v) in preset(p).unwrap() {
            d.set_param(i, v);
        }
        let mut a = [0.; 128];
        let mut b = a;
        for _ in 0..16 {
            d.process(&mut a, &mut b, None, &[]);
            assert!(a.iter().chain(&b).all(|x| *x == 0.));
        }
    }
}
#[test]
fn every_control_changes_sound() {
    for index in 0..9 {
        let mut a = Saturation::new(48000.);
        let mut b = Saturation::new(48000.);
        a.set_param(1, 16.);
        b.set_param(1, 16.);
        a.set_param(index, SPEC[index as usize].0);
        b.set_param(index, SPEC[index as usize].1);
        let (mut l, mut r) = signal(4096);
        let (mut x, mut y) = (l.clone(), r.clone());
        a.process(&mut l, &mut r, None, &[]);
        b.process(&mut x, &mut y, None, &[]);
        assert_ne!((l, r), (x, y), "control {index}");
    }
}
#[test]
fn model_switch_is_smoothed() {
    let mut a = Saturation::new(48000.);
    let mut b = Saturation::new(48000.);
    a.set_param(1, 18.);
    b.set_param(1, 18.);
    let (mut l, mut r) = signal(2048);
    let (mut x, mut y) = (l.clone(), r.clone());
    a.process(&mut l, &mut r, None, &[]);
    b.process(&mut x, &mut y, None, &[]);
    a.set_param(0, 2.);
    let mut l = [0.7; 128];
    let mut r = [-0.3; 128];
    let (mut x, mut y) = (l, r);
    a.process(&mut l, &mut r, None, &[]);
    b.process(&mut x, &mut y, None, &[]);
    let delta = l
        .iter()
        .zip(x)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(delta < 0.012, "model snapped: {delta}");
    assert!((l[0] - x[0]).abs() < 0.001);
    assert!(l.iter().zip(x).any(|(a, b)| (a - b).abs() > 0.00001));
}

fn tone(sr: f32, hz: f32, model: f32, cut: Option<(u32, f32)>, amp: f32) -> Vec<f32> {
    let mut d = Saturation::new(sr);
    d.set_param(0, model);
    d.set_param(1, 18.);
    d.set_param(6, 0.);
    if let Some((i, v)) = cut {
        d.set_param(i, v);
    }
    let mut l: Vec<_> = (0..16384)
        .map(|i| amp * libm::sinf(std::f32::consts::TAU * hz * i as f32 / sr))
        .collect();
    let mut r = l.clone();
    d.process(&mut l, &mut r, None, &[]);
    l[8192..].to_vec()
}
fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64
}
fn harmonic(x: &[f32], sr: f32, hz: f32) -> f64 {
    let mut re = 0.;
    let mut im = 0.;
    for (i, v) in x.iter().enumerate() {
        let phase = std::f64::consts::TAU * hz as f64 * i as f64 / sr as f64;
        re += *v as f64 * libm::cos(phase);
        im += *v as f64 * libm::sin(phase);
    }
    (re * re + im * im) / (x.len() * x.len()) as f64
}
#[test]
fn characters_have_expected_harmonics_and_dc_rejection() {
    let sr = 48000.;
    let hz = sr * 64. / 8192.;
    let renders: Vec<_> = (0..4).map(|m| tone(sr, hz, m as f32, None, 0.6)).collect();
    for x in &renders {
        let dc = x.iter().sum::<f32>() / x.len() as f32;
        assert!(dc.abs() < 0.0001, "DC {dc}");
        assert!(harmonic(x, sr, hz * 3.) > 0.00001);
    }
    let second: Vec<_> = renders.iter().map(|x| harmonic(x, sr, hz * 2.)).collect();
    assert!(second[0] > second[1] * 100.);
    assert!(second[3] > second[2] * 100.);
    assert!(second[0] > second[3] * 1.2);
}
#[test]
fn cuts_scale_in_hz_and_auto_gain_compensates() {
    for sr in [22050., 48000., 192000.] {
        let base = tone(sr, 100., 0., None, 0.1);
        let cut = tone(sr, 100., 0., Some((4, 1000.)), 0.1);
        assert!(energy(&cut) < energy(&base) * 0.04);
        let base = tone(sr, 4000., 1., None, 0.1);
        let cut = tone(sr, 4000., 1., Some((5, 1000.)), 0.1);
        assert!(energy(&cut) < energy(&base) * 0.16);
        let auto = tone(sr, 4000., 1., Some((6, 1.)), 0.1);
        let ratio = energy(&auto) / energy(&base);
        assert!((ratio - libm::pow(10., -18. / 10.)).abs() < 0.00001);
    }
}
#[test]
fn controls_ramp_instead_of_snapping() {
    for index in 0..9 {
        let mut a = Saturation::new(48000.);
        let mut b = Saturation::new(48000.);
        for d in [&mut a, &mut b] {
            d.set_param(1, 18.);
        }
        let (mut l, mut r) = signal(2048);
        let (mut x, mut y) = (l.clone(), r.clone());
        a.process(&mut l, &mut r, None, &[]);
        b.process(&mut x, &mut y, None, &[]);
        let target = if index == 1 {
            0.
        } else if index == 5 {
            1000.
        } else if index == 6 || index == 7 {
            0.
        } else {
            SPEC[index as usize].1
        };
        a.set_param(index, target);
        let (mut l, mut r) = signal(512);
        let (mut x, mut y) = (l.clone(), r.clone());
        a.process(&mut l, &mut r, None, &[]);
        b.process(&mut x, &mut y, None, &[]);
        let delta = l
            .iter()
            .zip(&x)
            .chain(r.iter().zip(&y))
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(delta > 0.000001, "control {index} failed to move");
        assert!((l[0] - x[0]).abs() < 0.01, "control {index} snapped");
    }
}
#[test]
fn right_only_input_stays_independent() {
    let mut d = Saturation::new(48000.);
    let mut l = [0.; 128];
    let mut r = [0.7; 128];
    d.process(&mut l, &mut r, None, &[]);
    assert!(l.iter().all(|x| *x == 0.));
    assert!(r.iter().any(|x| x.abs() > 0.01));
}

#[test]
fn oversampling_attenuates_folded_harmonics() {
    let sr = 48000.;
    let hz = sr * 700. / 8192.;
    let alias = sr - 9. * hz;
    // Independent native-rate Transistor transfer, without the FIR stages.
    let input: Vec<_> = (0..8192)
        .map(|i| 0.6 * libm::sinf(std::f32::consts::TAU * hz * i as f32 / sr))
        .collect();
    let gain = libm::powf(10., 18. / 20.);
    let native: Vec<_> = input
        .iter()
        .map(|x| {
            let v = x * gain;
            v / (1. + v.abs())
        })
        .collect();
    let wet = tone(sr, hz, 2., None, 0.6);
    let native_ratio = harmonic(&native, sr, alias) / harmonic(&native, sr, hz);
    let wet_ratio = harmonic(&wet, sr, alias) / harmonic(&wet, sr, hz);
    assert!(
        wet_ratio < native_ratio * 0.01,
        "native {native_ratio} 4x {wet_ratio}"
    );
}

#[test]
fn mixes_are_aligned_affine_combinations() {
    let (l, r) = signal(512);
    let mut renders = Vec::new();
    for mix in [0., 0.25, 0.5, 0.75, 1.] {
        let mut d = Saturation::new(48000.);
        d.set_param(1, 12.);
        d.set_param(7, mix);
        let (mut a, mut b) = (l.clone(), r.clone());
        d.process(&mut a, &mut b, None, &[]);
        renders.push((a, b));
    }
    for (j, mix) in [0.25, 0.5, 0.75].into_iter().enumerate() {
        for i in 0..512 {
            for c in 0..2 {
                let get = |j: usize| {
                    if c == 0 {
                        renders[j].0[i]
                    } else {
                        renders[j].1[i]
                    }
                };
                let expected = get(0) + (get(4) - get(0)) * mix;
                assert!((get(j + 1) - expected).abs() < 0.000001);
            }
        }
    }
}

// Linear-region reference isolates compensation timing from nonlinear loudness.
#[test]
fn automation_compensation_tracks_delayed_audio() {
    for sr in [22050., 48000., 192000.] {
        for (index, top) in [(1, 30.), (3, 18.)] {
            for (from, to) in [(0., top), (top, 0.)] {
                for block in [1, 47, 128] {
                    let mut a = Saturation::new(sr);
                    let mut b = Saturation::new(sr);
                    a.set_param(index, from);
                    let n = sr as usize / 2;
                    let mut worst = 0f64;
                    let mut actual = Vec::new();
                    let mut reference = Vec::new();
                    for at in (0..n * 2).step_by(block) {
                        if at >= n && at < n + block {
                            a.set_param(index, to);
                        }
                        let len = block.min(n * 2 - at);
                        let mut l: Vec<_> = (at..at + len)
                            .map(|i| {
                                1e-6 * libm::sinf(i as f32 * std::f32::consts::TAU * 1000. / sr)
                            })
                            .collect();
                        let mut r = l.clone();
                        let mut x = l.clone();
                        let mut y = l.clone();
                        a.process(&mut l, &mut r, None, &[]);
                        b.process(&mut x, &mut y, None, &[]);
                        actual.extend(l);
                        reference.extend(x);
                    }
                    let window = (sr / 1000.) as usize;
                    for at in (n + 49..n * 2 - window).step_by(window) {
                        let db = 10.
                            * libm::log10(
                                energy(&actual[at..at + window])
                                    / energy(&reference[at..at + window]),
                            );
                        worst = worst.max(db.abs());
                    }
                    eprintln!("envelope {sr} {index} {from}->{to} block {block}: {worst} dB");
                    assert!(worst < 0.07, "{sr} {index} {from}->{to}: {worst} dB");
                }
            }
        }
    }
}

fn linear_response(sr: f32, hz: f32, params: &[(u32, f32)]) -> f64 {
    let mut d = Saturation::new(sr);
    for &(i, v) in params {
        d.set_param(i, v);
    }
    let n = sr as usize * 2;
    let mut l: Vec<_> = (0..n)
        .map(|i| 1e-6 * libm::sinf(std::f32::consts::TAU * hz * i as f32 / sr))
        .collect();
    let mut r = l.clone();
    d.process(&mut l, &mut r, None, &[]);
    energy(&l[n / 2..])
}
fn db_ratio(a: f64, b: f64) -> f64 {
    10. * libm::log10(a / b)
}
#[test]
fn physical_cut_tone_and_gain_response() {
    for sr in [22050., 48000., 192000.] {
        for (index, hz, neutral) in [(4, 400., 20.), (5, 2000., 20000.)] {
            let db = db_ratio(
                linear_response(sr, hz, &[(index, hz)]),
                linear_response(sr, hz, &[(index, neutral)]),
            );
            assert!((db + 3.).abs() < 0.5, "cut {sr} {index}: {db}");
        }
        for tone in [-1., 1.] {
            for (hz, expected) in [(100., -6.), (10000., 6.)] {
                let db = db_ratio(
                    linear_response(sr, hz, &[(2, tone)]),
                    linear_response(sr, hz, &[]),
                );
                assert!(
                    (db - expected * tone as f64).abs() < 0.5,
                    "tone {tone} {sr} {hz}: {db}"
                );
            }
            // The tilt pivot stays at unity while the shelves move in opposite directions.
            let db = db_ratio(
                linear_response(sr, 700., &[(2, tone)]),
                linear_response(sr, 700., &[]),
            );
            let expected = 0.;
            assert!((db - expected).abs() < 0.1, "tilt {tone} {sr}: {db}");
        }
        let db = db_ratio(
            linear_response(sr, 1000., &[(1, 12.), (3, 6.)]),
            linear_response(sr, 1000., &[]),
        );
        assert!(db.abs() < 0.1, "auto {sr}: {db}");
    }
    let a = linear_response(22050., 7000., &[(5, 20000.)]);
    let b = linear_response(22050., 7000., &[(5, 9922.5)]);
    assert_eq!(a, b);
    let reference = linear_response(22050., 7000., &[(5, 5000.)]);
    let t = libm::tan(std::f64::consts::PI * 7000. / 22050.);
    let response = |fc: f64| {
        let g = libm::tan(std::f64::consts::PI * fc / 22050.);
        g * g / (g * g + t * t)
    };
    let expected = 10. * libm::log10(response(9922.5) / response(5000.));
    assert!(
        (db_ratio(a, reference) - expected).abs() < 0.1,
        "high-cut cap"
    );
}
#[test]
fn constructor_rate_bounds_and_fallback() {
    for (given, effective) in [
        (8000., 22050.),
        (384000., 192000.),
        (f32::NAN, 48000.),
        (f32::INFINITY, 48000.),
        (f32::NEG_INFINITY, 48000.),
    ] {
        assert_eq!(
            render(given, 47, "Amber filament", true),
            render(effective, 47, "Amber filament", true)
        );
    }
}
#[test]
fn pre_gain_ramps_after_fir_latency_with_auto_gain_off() {
    for sr in [22050., 48000., 192000.] {
        for index in [1, 3] {
            for phase in [0., 0.7, 2.] {
                let mut a = Saturation::new(sr);
                let mut b = Saturation::new(sr);
                for d in [&mut a, &mut b] {
                    d.set_param(6, 0.);
                }
                let input: Vec<_> = (0..2048)
                    .map(|i| 1e-5 * libm::sinf(i as f32 * 0.13 + phase))
                    .collect();
                let mut l = input.clone();
                let mut r = l.clone();
                a.process(&mut l, &mut r, None, &[]);
                let mut x = input.clone();
                let mut y = x.clone();
                b.process(&mut x, &mut y, None, &[]);
                a.set_param(index, 18.);
                let mut l = input.clone();
                let mut r = l.clone();
                let mut x = input.clone();
                let mut y = x.clone();
                a.process(&mut l, &mut r, None, &[]);
                b.process(&mut x, &mut y, None, &[]);
                for range in [49..61, 61..100] {
                    let delta = l[range.clone()]
                        .iter()
                        .zip(&x[range])
                        .map(|(a, b)| (a - b).abs())
                        .fold(0f32, f32::max);
                    assert!(
                        delta < 5e-6,
                        "pre-gain snapped {sr} {index} {phase}: {delta}"
                    );
                }
                assert!(energy(&l[1024..]) > energy(&x[1024..]) * 1.2);
            }
        }
    }
}

#[test]
fn dc_corner_scales_with_rate() {
    for sr in [22050., 48000., 192000.] {
        let hz = 5f64;
        let w = std::f64::consts::TAU * hz / sr as f64;
        let r = 1. - std::f64::consts::TAU * 5. / sr as f64;
        let dc = (2. - 2. * libm::cos(w)) / (1. + r * r - 2. * r * libm::cos(w));
        let t = libm::tan(w / 2.);
        let hp = t * t / (t * t + libm::pow(libm::tan(std::f64::consts::PI * 20. / sr as f64), 2.));
        let expected = 0.5e-12 * dc * hp;
        let measured = linear_response(sr, 5., &[]);
        let db = db_ratio(measured, expected);
        assert!(db.abs() < 0.08, "DC corner {sr}: {db}");
    }
}
