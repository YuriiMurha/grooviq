use grooviq_dsp::{
    Device,
    algorithmic_reverb::{AlgorithmicReverb, PRESETS, SPEC, preset},
};
fn run(d: &mut AlgorithmicReverb, l: &mut [f32], r: &mut [f32], block: usize) {
    for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(l, r, None, &[]);
    }
}
fn patch(sr: f32, model: f32) -> AlgorithmicReverb {
    let mut d = AlgorithmicReverb::new(sr);
    for (i, v) in [
        (0, model),
        (2, 0.),
        (10, 1.),
        (6, 0.),
        (4, 0.),
        (7, 20.),
        (8, 20000.),
    ] {
        d.set_param(i, v);
    }
    d
}
fn energy(x: &[f32]) -> f64 {
    x.iter().map(|&x| f64::from(x) * f64::from(x)).sum()
}
#[test]
fn every_model_has_a_real_stereo_tail_and_distinct_response() {
    for sr in [22050., 48000., 192000.] {
        let mut previous = Vec::new();
        for model in 0..5 {
            for channel in 0..2 {
                let mut d = patch(sr, model as f32);
                let mut l = vec![0.; sr as usize];
                let mut r = l.clone();
                if channel == 0 {
                    l[0] = 1.;
                } else {
                    r[0] = 1.;
                }
                run(&mut d, &mut l, &mut r, 127);
                assert!(
                    energy(&l) > 1e-7 && energy(&r) > 1e-7,
                    "model {model} channel {channel} at {sr}"
                );
                assert!(l.iter().chain(&r).all(|v| v.is_finite()));
                if channel == 0 {
                    assert_ne!(previous, l);
                    previous = l;
                }
            }
        }
    }
}
#[test]
fn predelay_shifts_actual_wet_onset_and_width_zero_is_mono() {
    for model in 0..5 {
        let render = |pre| {
            let mut d = patch(48000., model as f32);
            d.set_param(2, pre);
            d.set_param(9, 0.);
            let mut l = vec![0.; 48000];
            let mut r = l.clone();
            l[0] = 1.;
            run(&mut d, &mut l, &mut r, 64);
            assert_eq!(l, r);
            l
        };
        let a = render(0.);
        let b = render(50.);
        let onset = |x: &[f32]| x.iter().position(|v| v.abs() > 1e-9).unwrap();
        assert_eq!(onset(&b) - onset(&a), 2400);
    }
}
#[test]
fn decay_controls_tail_energy_in_physical_time() {
    for sr in [22050., 48000., 96000.] {
        for model in 0..4 {
            let render = |decay| {
                let mut d = patch(sr, model as f32);
                d.set_param(3, decay);
                let mut l = vec![0.; (sr * 2.) as usize];
                let mut r = l.clone();
                l[0] = 1.;
                r[0] = 0.7;
                run(&mut d, &mut l, &mut r, 128);
                l
            };
            let short = render(0.3);
            let long = render(2.);
            let start = (sr * 0.5) as usize;
            assert!(
                energy(&long[start..]) > energy(&short[start..]) * 100.,
                "model {model} at {sr}"
            );
            assert!(energy(&short[start..]) < energy(&short[..(sr * 0.2) as usize]) * 0.001);
        }
    }
}
#[test]
fn nonlinear_duration_bounds_the_reflection_cloud() {
    for duration in [0.1, 0.5] {
        let mut d = patch(48000., 4.);
        d.set_param(12, duration);
        d.set_param(5, 0.);
        let mut l = vec![0.; 48000];
        let mut r = l.clone();
        l[0] = 1.;
        run(&mut d, &mut l, &mut r, 7);
        let last = l.iter().rposition(|v| v.abs() > 1e-7).unwrap();
        assert!(last as f32 > duration * 48000. * 0.8 && (last as f32) < duration * 48000. + 3000.);
        let first = energy(&l[..(duration * 48000. * 0.2) as usize]);
        let middle =
            energy(&l[(duration * 48000. * 0.3) as usize..(duration * 48000. * 0.7) as usize]);
        assert!(middle > first * 2.);
    }
}
fn automated(block: usize, sr: f32) -> (Vec<f32>, Vec<f32>) {
    let mut d = AlgorithmicReverb::with_seed(sr, 77);
    d.set_param(10, 1.);
    let n = (sr * 0.45) as usize;
    let mut l: Vec<_> = (0..n)
        .map(|i| {
            if i < 1000 {
                libm::sinf(i as f32 * 0.17) * 0.3
            } else {
                0.
            }
        })
        .collect();
    let mut r: Vec<_> = l.iter().map(|v| v * 0.37).collect();
    let splits = [0, 997, n / 2, n];
    for (segment, ends) in splits.windows(2).enumerate() {
        if segment > 0 {
            for (i, &(lo, hi, _)) in SPEC.iter().enumerate() {
                d.set_param(i as u32, if segment == 1 { hi } else { lo });
            }
        }
        run(
            &mut d,
            &mut l[ends[0]..ends[1]],
            &mut r[ends[0]..ends[1]],
            block,
        );
    }
    (l, r)
}
#[test]
fn sample_aligned_automation_is_exact_for_odd_partitions() {
    for sr in [22050., 48000., 192000.] {
        let a = automated(128, sr);
        for block in [1, 7, 31, 64, 127] {
            assert_eq!(a, automated(block, sr));
        }
    }
}
#[test]
fn reset_during_ramps_replays_fresh_seed_and_empty_does_not_age() {
    let mut a = AlgorithmicReverb::with_seed(48000., 91);
    let mut b = AlgorithmicReverb::with_seed(48000., 91);
    a.process(&mut [], &mut [], None, &[]);
    for (i, &(_, hi, _)) in SPEC.iter().enumerate() {
        a.set_param(i as u32, hi);
        b.set_param(i as u32, hi);
    }
    let mut l = [0.2; 127];
    let mut r = [-0.1; 127];
    let mut x = l;
    let mut y = r;
    a.process(&mut l, &mut r, None, &[]);
    b.process(&mut x, &mut y, None, &[]);
    assert_eq!((l, r), (x, y));
    for (i, &(lo, _, _)) in SPEC.iter().enumerate() {
        a.set_param(i as u32, lo);
        b.set_param(i as u32, lo);
    }
    a.process(&mut l, &mut r, None, &[]);
    a.reset();
    b.reset();
    l = [0.2; 127];
    r = [-0.1; 127];
    x = l;
    y = r;
    a.process(&mut l, &mut r, None, &[]);
    b.process(&mut x, &mut y, None, &[]);
    assert_eq!((l, r), (x, y));
}
#[test]
fn hostile_audio_and_controls_remain_finite() {
    for name in PRESETS {
        let mut d = AlgorithmicReverb::new(22050.);
        for (i, v) in preset(name).unwrap() {
            d.set_param(i, v);
        }
        for i in 0..SPEC.len() {
            for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                d.set_param(i as u32, v);
            }
        }
        for n in 0..1000 {
            let mut l = [if n % 2 == 0 { f32::MAX } else { f32::NAN }; 127];
            let mut r = [f32::NEG_INFINITY; 127];
            d.process(&mut l, &mut r, None, &[]);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        }
    }
}
#[test]
fn continuously_automated_tail_eventually_decays() {
    let mut d = patch(22050., 1.);
    let mut peak = 0f32;
    let mut late = 0f32;
    for n in 0..2200 {
        let mut l = [0.; 127];
        let mut r = l;
        if n == 0 {
            l[0] = 1.;
            r[0] = 0.3;
        }
        for (i, v) in [
            (0, (n % 5) as f32),
            (1, (n % 3) as f32),
            (3, 0.1 + (n % 7) as f32 * 0.05),
            (4, (n % 11) as f32 / 10.),
        ] {
            d.set_param(i, v);
        }
        d.process(&mut l, &mut r, None, &[]);
        for v in l.into_iter().chain(r) {
            peak = peak.max(v.abs());
            if n > 2000 {
                late = late.max(v.abs());
            }
        }
    }
    assert!(peak > 1e-5 && peak < 1.);
    assert!(late < 1e-7, "late {late}");
}

#[test]
fn eq_and_colour_have_physical_frequency_response_in_both_directions() {
    let measure = |model, frequency, low, high, colour| {
        let mut d = patch(48000., model);
        d.set_param(7, low);
        d.set_param(8, high);
        d.set_param(1, colour);
        let mut sum = 0f64;
        for b in 0..750 {
            let mut l: [f32; 128] = std::array::from_fn(|i| {
                libm::sinf(std::f32::consts::TAU * frequency * (b * 128 + i) as f32 / 48000.) * 0.2
            });
            let mut r = l.map(|v| v * 0.3);
            d.process(&mut l, &mut r, None, &[]);
            if b > 500 {
                sum += energy(&l) + energy(&r);
            }
        }
        sum
    };
    for model in 0..4 {
        let bass = measure(model as f32, 73., 20., 20000., 0.);
        let bass_cut = measure(model as f32, 73., 1000., 20000., 0.);
        assert!(bass > 10. * bass_cut, "low cut model {model}");
        let bright = measure(model as f32, 6317., 20., 20000., 0.);
        let treble_cut = measure(model as f32, 6317., 20., 1000., 0.);
        let dark = measure(model as f32, 6317., 20., 20000., 2.);
        assert!(bright > 10. * treble_cut, "high cut model {model}");
        assert!(bright > 2. * dark, "colour model {model}");
    }
}
#[test]
fn every_control_changes_the_charged_path() {
    for (index, &(_, maximum, _)) in SPEC.iter().enumerate() {
        // Nonlinear duration is meaningful only in the Nonlinear algorithm.
        let model = if index == 12 { 4. } else { 1. };
        let mut a = patch(22050., model);
        let mut b = patch(22050., model);
        for d in [&mut a, &mut b] {
            d.set_param(10, 0.5);
        }
        for n in 0..180 {
            let source: [f32; 127] =
                std::array::from_fn(|i| libm::sinf((n * 127 + i) as f32 * 0.19) * 0.2);
            for d in [&mut a, &mut b] {
                let mut l = source;
                let mut r = source.map(|v| v * 0.7);
                d.process(&mut l, &mut r, None, &[]);
            }
        }
        let target = if index == 0 {
            4.
        } else if index == 8 {
            1000.
        } else {
            maximum
        };
        a.set_param(index as u32, target);
        // Compare the ramp to an unchanged, identically charged reference over delayed paths.
        let mut delta = 0f64;
        for n in 0..140 {
            let source: [f32; 127] =
                std::array::from_fn(|i| libm::sinf((n * 127 + i) as f32 * 0.23) * 0.2);
            let mut l = source;
            let mut r = source.map(|v| v * 0.7);
            let mut x = l;
            let mut y = r;
            a.process(&mut l, &mut r, None, &[]);
            b.process(&mut x, &mut y, None, &[]);
            delta += l
                .iter()
                .zip(x)
                .map(|(&v, w)| f64::from(v - w) * f64::from(v - w))
                .sum::<f64>();
        }
        assert!(delta > 1e-10, "control {index} has no effect");
    }
}

#[test]
fn reset_clears_charged_networks_and_replays_against_new_instance() {
    for model in 0..5 {
        let mut a = patch(22050., model as f32);
        for n in 0..250 {
            let mut l = [libm::sinf(n as f32) * 0.2; 127];
            let mut r = [0.1; 127];
            a.process(&mut l, &mut r, None, &[]);
        }
        a.set_param(3, 4.);
        a.set_param(6, 0.7);
        a.reset();
        let mut b = patch(22050., model as f32);
        b.set_param(3, 4.);
        b.set_param(6, 0.7);
        for n in 0..250 {
            let mut l = [if n == 0 { 0.3 } else { 0. }; 127];
            let mut r = [0.; 127];
            let mut x = l;
            let mut y = r;
            a.process(&mut l, &mut r, None, &[]);
            b.process(&mut x, &mut y, None, &[]);
            assert_eq!((l, r), (x, y), "model {model}, block {n}");
        }
    }
}

#[test]
fn size_moves_reflections_later_and_model_transitions_use_charged_wet_signal() {
    for model in 0..5 {
        let onset = |size| {
            let mut d = patch(48000., model as f32);
            d.set_param(4, size);
            let mut l = vec![0.; 24000];
            let mut r = l.clone();
            l[0] = 1.;
            run(&mut d, &mut l, &mut r, 127);
            l.iter().position(|v| v.abs() > 1e-9).unwrap()
        };
        assert!(onset(1.) > onset(0.) + 3800, "size model {model}");
    }
    let mut d = patch(22050., 0.);
    d.set_param(3, 4.);
    for _ in 0..100 {
        let mut l = [0.2; 127];
        let mut r = [-0.1; 127];
        d.process(&mut l, &mut r, None, &[]);
    }
    for model in 1..5 {
        d.set_param(0, model as f32);
        let mut total = 0.;
        for _ in 0..100 {
            let mut l = [0.; 127];
            let mut r = l;
            d.process(&mut l, &mut r, None, &[]);
            total += energy(&l) + energy(&r);
        }
        // Finite Nonlinear history has expired by the end of its transition, but the
        // preceding acoustic-to-acoustic transitions must each carry a real tail.
        if model < 4 {
            assert!(total > 1e-9, "transition {model}");
        }
    }
}

#[test]
fn acoustic_rt60_slope_is_in_seconds_and_decibels_for_uneven_tone_bursts() {
    for sr in [22050., 48000., 96000.] {
        for model in 0..4 {
            for frequency in [317., 1231.] {
                let mut d = patch(sr, model as f32);
                d.set_param(3, 0.8);
                d.set_param(5, 1.);
                let mut l: Vec<_> = (0..sr as usize)
                    .map(|i| {
                        if i < (sr * 0.1) as usize {
                            libm::sinf(std::f32::consts::TAU * frequency * i as f32 / sr) * 0.2
                        } else {
                            0.
                        }
                    })
                    .collect();
                let mut r = l.iter().map(|v| v * 0.37).collect::<Vec<_>>();
                run(&mut d, &mut l, &mut r, 127);
                let early = energy(&l[(sr * 0.25) as usize..(sr * 0.35) as usize])
                    + energy(&r[(sr * 0.25) as usize..(sr * 0.35) as usize]);
                let late = energy(&l[(sr * 0.75) as usize..(sr * 0.85) as usize])
                    + energy(&r[(sr * 0.75) as usize..(sr * 0.85) as usize]);
                let slope = 10. * libm::log10(late / early);
                assert!(
                    (-52.5..-22.5).contains(&slope),
                    "{model} {frequency} Hz at {sr}: {slope} dB per 0.5s; expected -37.5 dB, allowing modal variance/damping"
                );
            }
        }
    }
}
