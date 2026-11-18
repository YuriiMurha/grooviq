use grooviq_dsp::{
    Device,
    character_delay::{CharacterDelay, PRESETS, SPEC, preset},
};
fn run(d: &mut CharacterDelay, l: &mut [f32], r: &mut [f32], block: usize) {
    for (a, b) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(a, b, None, &[]);
    }
}
fn patch(sr: f32, model: f32) -> CharacterDelay {
    let mut d = CharacterDelay::new(sr);
    for (i, v) in [
        (0, 0.0),
        (2, 10.0),
        (3, 0.8),
        (4, 0.0),
        (5, 20.0),
        (6, 20000.0),
        (7, 1.0),
        (9, model),
        (10, 0.8),
    ] {
        d.set_param(i, v);
    }
    d
}
#[test]
fn rates_partitions_automation_reset_and_hostile_controls() {
    for sr in [22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
        for model in [0.0, 1.0, 2.0] {
            let input: Vec<f32> = (0..8192)
                .map(|i| {
                    if i % 127 < 16 {
                        0.7 * libm::sinf(i as f32 * 0.31)
                    } else {
                        0.0
                    }
                })
                .collect();
            let render = |block| {
                let mut d = patch(sr, model);
                let mut l = input.clone();
                let mut r: Vec<_> = input.iter().map(|x| -x * 0.6).collect();
                run(&mut d, &mut l[..2001], &mut r[..2001], block);
                for (i, s) in SPEC.iter().enumerate() {
                    d.set_param(i as u32, s.0 + (s.1 - s.0) * 0.63);
                    d.set_param(i as u32, f32::NAN);
                    d.set_param(i as u32, f32::INFINITY);
                }
                run(&mut d, &mut l[2001..], &mut r[2001..], block);
                (l, r)
            };
            let a = render(1);
            for b in [3, 17, 64, 127, 128] {
                assert_eq!(a, render(b), "sr={sr} model={model} block={b}");
            }
            let mut d = patch(sr, model);
            let mut l = input.clone();
            let mut r = input.clone();
            run(&mut d, &mut l, &mut r, 128);
            d.reset();
            let mut a = input.clone();
            let mut b = input.clone();
            run(&mut d, &mut a, &mut b, 17);
            let mut fresh = patch(sr, model);
            let mut c = input.clone();
            let mut e = input.clone();
            fresh.process(&mut [], &mut [], None, &[]);
            run(&mut fresh, &mut c, &mut e, 64);
            assert_eq!((a, b), (c, e));
            let mut l = [f32::MAX, f32::NAN, f32::INFINITY, -f32::MAX];
            let mut r = l;
            d.process(&mut l, &mut r, None, &[]);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        }
    }
}
#[test]
fn repeats_pingpong_and_progressive_colour() {
    let mut renders = Vec::new();
    for model in [0.0, 1.0, 2.0] {
        let mut d = patch(48000.0, model);
        d.set_param(4, 1.0);
        let mut l = vec![0.0; 6000];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        run(&mut d, &mut l, &mut r, 17);
        assert!(l[470..510].iter().any(|x| x.abs() > 0.05));
        assert!(r[950..990].iter().any(|x| x.abs() > 0.02));
        assert!(r[..470].iter().all(|x| x.abs() < 1e-7));
        assert!(
            r[470..900].iter().all(|x| x.abs() < 1e-4),
            "first echo must stay left"
        );
        renders.push((l, r));
    }
    assert_ne!(renders[0], renders[1]);
    assert_ne!(renders[1], renders[2]);
}
#[test]
fn seeds_silence_and_right_only() {
    let render = |seed, signal| {
        let mut d = CharacterDelay::with_seed(22050.0, seed);
        for (i, v) in [(0, 0.0), (2, 5.0), (4, 0.0), (7, 1.0), (9, 2.0)] {
            d.set_param(i, v);
        }
        let mut l = vec![0.0; 22050];
        let mut r = l.clone();
        r[0] = signal;
        run(&mut d, &mut l, &mut r, 127);
        (l, r)
    };
    assert_eq!(render(1, 0.0), (vec![0.0; 22050], vec![0.0; 22050]));
    assert_eq!(render(1, 1.0), render(1, 1.0));
    assert_ne!(render(1, 1.0), render(2, 1.0));
    assert!(render(1, 1.0).1.iter().any(|x| x.abs() > 0.1));
}
#[test]
fn physical_maximum_and_fractional_tempo() {
    for sr in [22050.0, 192000.0] {
        let mut d = CharacterDelay::new(sr);
        for (i, v) in [(1, 8.0), (8, 20.0), (4, 0.0), (7, 1.0), (3, 0.0)] {
            d.set_param(i, v);
        }
        assert_eq!(d.meter(2), 6000.0);
        let n = (sr * 6.0) as usize;
        let mut l = vec![0.0; n + 128];
        let mut r = l.clone();
        l[0] = 0.5;
        run(&mut d, &mut l, &mut r, 128);
        assert!(l[..n - 2].iter().all(|x| x.abs() < 1e-7));
        assert!(l[n..].iter().any(|x| x.abs() > 0.001));
    }
}
#[test]
fn factory_voices_are_audible_and_distinct() {
    let mut all = Vec::new();
    for name in PRESETS {
        let mut d = CharacterDelay::new(22050.0);
        for (i, v) in preset(name).unwrap() {
            d.set_param(i, v);
        }
        let mut l = vec![0.0; 22050];
        let mut r = l.clone();
        for i in 0..256 {
            l[i] = libm::sinf(i as f32 * 0.2) * 0.9;
            r[i] = l[i] * 0.7;
        }
        let dry = l.clone();
        run(&mut d, &mut l, &mut r, 64);
        assert_ne!(l, dry);
        assert!(l[5000..].iter().any(|x| x.abs() > 0.001));
        assert!(l.iter().all(|x| x.is_finite()));
        all.push(l);
    }
    assert_ne!(all[0], all[1]);
    assert_ne!(all[1], all[2]);
}

#[test]
fn ducking_filters_feedback_decay_and_queued_time_changes() {
    let render = |duck, cut, feedback| {
        let mut d = patch(22050.0, 0.0);
        d.set_param(11, duck);
        d.set_param(6, cut);
        d.set_param(3, feedback);
        let mut l: Vec<_> = (0..22050)
            .map(|i| {
                if i < 8000 {
                    0.7 * libm::sinf(i as f32 * 1.1)
                } else {
                    0.0
                }
            })
            .collect();
        let mut r = l.clone();
        run(&mut d, &mut l, &mut r, 17);
        l
    };
    let energy = |x: &[f32]| x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>();
    let dryduck = render(0.0, 20000.0, 0.0);
    let duck = render(1.0, 20000.0, 0.0);
    assert!(energy(&duck[2000..7000]) < energy(&dryduck[2000..7000]) * 0.2);
    let filtered = render(0.0, 1000.0, 0.0);
    assert!(energy(&filtered[2000..7000]) < energy(&dryduck[2000..7000]) * 0.05);
    let feedback = render(0.0, 20000.0, 0.95);
    assert!(energy(&feedback[8500..12000]) > energy(&dryduck[8500..12000]) * 10.0);
    assert!(energy(&feedback[20000..]) < energy(&feedback[8500..12000]));
    let render_changes = |block| {
        let mut d = patch(22050.0, 1.0);
        let mut l: Vec<_> = (0..10000)
            .map(|i| libm::sinf(i as f32 * 0.07) * 0.4)
            .collect();
        let mut r = l.clone();
        for (a, b, time) in [
            (0, 2001, 100.0),
            (2001, 2107, 200.0),
            (2107, 2303, 30.0),
            (2303, 10000, 70.0),
        ] {
            d.set_param(2, time);
            run(&mut d, &mut l[a..b], &mut r[a..b], block);
        }
        (l, r)
    };
    assert_eq!(render_changes(1), render_changes(127));
}

#[test]
fn reset_snaps_automated_targets_like_a_fresh_device() {
    for model in [0.0, 1.0, 2.0] {
        let mut d = patch(48000.0, model);
        let mut l = [0.3; 128];
        let mut r = [-0.4; 128];
        d.process(&mut l, &mut r, None, &[]);
        let controls = [
            (3, 0.23),
            (5, 700.0),
            (6, 2500.0),
            (7, 0.8),
            (9, 2.0 - model),
            (10, 0.9),
            (11, 0.7),
            (12, 0.8),
            (13, 3.0),
            (14, -12.0),
        ];
        let mut fresh = patch(48000.0, model);
        for (i, v) in controls {
            d.set_param(i, v);
            fresh.set_param(i, v);
        }
        d.reset();
        let mut a = vec![0.4; 8192];
        let mut b = vec![-0.2; 8192];
        let mut c = a.clone();
        let mut e = b.clone();
        run(&mut d, &mut a, &mut b, 127);
        run(&mut fresh, &mut c, &mut e, 17);
        assert_eq!((a, b), (c, e));
    }
}

#[test]
fn every_colouring_control_moves_after_live_automation() {
    for (control, value) in [
        (9, 2.0),
        (10, 0.1),
        (11, 1.0),
        (12, 0.8),
        (13, 4.0),
        (14, -12.0),
    ] {
        let render = |change| {
            let mut d = patch(22050.0, 1.0);
            d.set_param(12, 0.4);
            let mut l: Vec<_> = (0..16000)
                .map(|i| 0.8 * libm::sinf(i as f32 * 0.19))
                .collect();
            let mut r = l.clone();
            run(&mut d, &mut l[..4000], &mut r[..4000], 17);
            if change {
                d.set_param(control, value);
            }
            run(&mut d, &mut l[4000..], &mut r[4000..], 17);
            l
        };
        let a = render(true);
        let b = render(false);
        let difference: f64 = a[8000..]
            .iter()
            .zip(&b[8000..])
            .map(|(x, y)| (*x as f64 - *y as f64).abs())
            .sum();
        assert!(difference > 0.01, "control {control}: {difference}");
    }
}

#[test]
fn empty_process_preserves_fresh_configuration_and_invalid_controls_are_rejected() {
    let render = |empty, invalid| {
        let mut d = CharacterDelay::new(22050.0);
        if empty {
            d.process(&mut [], &mut [], None, &[]);
        }
        for (i, v) in [(0, 0.0), (2, 7.0), (9, 2.0), (7, 0.9), (14, -6.0)] {
            d.set_param(i, v);
        }
        if invalid {
            for i in 0..SPEC.len() {
                for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                    d.set_param(i as u32, v);
                }
            }
        }
        let mut l = vec![0.2; 8000];
        let mut r = vec![-0.4; 8000];
        run(&mut d, &mut l, &mut r, 17);
        (l, r)
    };
    assert_eq!(render(false, false), render(true, false));
    assert_eq!(render(false, false), render(false, true));
}

#[test]
fn analogue_noise_channels_are_independent_and_tail_dies() {
    let mut d = patch(22050.0, 2.0);
    d.set_param(3, 0.5);
    let mut l = vec![0.0; 22050 * 4];
    let mut r = l.clone();
    l[0] = 0.8;
    r[0] = 0.8;
    run(&mut d, &mut l, &mut r, 127);
    assert_ne!(l, r);
    let peak = |a: &[f32]| a.iter().fold(0.0f32, |x, y| x.max(y.abs()));
    assert!(peak(&l[..22050]) > 0.01);
    assert!(peak(&l[22050 * 3..]) < 1e-8);
}

#[test]
fn automated_tails_decay_against_static_energy_windows() {
    for sr in [22050., 48000., 96000.] {
        for voice in 0..6 {
            for tempo in [false, true] {
                let render = |automate| {
                    let mut d = patch(sr, (voice % 3) as f32);
                    if voice >= 3 {
                        for (i, v) in preset(PRESETS[voice - 3]).unwrap() {
                            d.set_param(i, v);
                        }
                    }
                    for (i, v) in [
                        (0, if tempo { 1. } else { 0. }),
                        (1, 0.),
                        (2, 20.),
                        (8, 120.),
                        (3, 0.95),
                        (7, 1.),
                    ] {
                        d.set_param(i, v);
                    }
                    let n = (sr * 4.) as usize;
                    let mut l: Vec<_> = (0..n)
                        .map(|i| {
                            if i < (sr * 0.1) as usize {
                                0.5 * libm::sinf(i as f32 * std::f32::consts::TAU * 300. / sr)
                            } else {
                                0.
                            }
                        })
                        .collect();
                    let mut r = l.clone();
                    for start in (0..n).step_by(64) {
                        if automate {
                            if tempo {
                                d.set_tempo(if start % 128 == 0 { 120. } else { 121. });
                            } else {
                                d.set_param(2, if start % 128 == 0 { 20. } else { 20.05 });
                            }
                        }
                        let end = (start + 64).min(n);
                        run(&mut d, &mut l[start..end], &mut r[start..end], 17);
                    }
                    let energy = |a: f32, b: f32| {
                        l[(sr * a) as usize..(sr * b) as usize]
                            .iter()
                            .map(|x| (*x as f64) * (*x as f64))
                            .sum::<f64>()
                            / (sr * (b - a)) as f64
                    };
                    (energy(0.2, 0.7), energy(2.5, 3.))
                };
                let static_e = render(false);
                let moving = render(true);
                eprintln!(
                    "decay sr={sr} voice={voice} tempo={tempo} static={static_e:?} moving={moving:?}"
                );
                assert!(moving.0 > 1e-8, "nonvacuous early tail");
                assert!(moving.1 / moving.0 < 0.02);
                assert!(moving.1 / moving.0 < (static_e.1 / static_e.0) * 10. + 1e-12);
            }
        }
    }
}

#[test]
fn live_echo_moves_to_final_time_division_and_tempo() {
    for sr in [22050., 48000., 192000.] {
        for (control, value, ms) in [(2, 40., 40.), (1, 1., 125.), (8, 240., 31.25)] {
            let mut d = patch(sr, 0.);
            d.set_param(3, 0.);
            if control != 2 {
                d.set_param(0, 1.);
                d.set_param(1, 0.);
                d.set_tempo(120.);
            }
            let mut silence = vec![0.; (sr * 0.1) as usize];
            let mut r = silence.clone();
            run(&mut d, &mut silence, &mut r, 17);
            d.set_param(control, value);
            run(&mut d, &mut silence, &mut r, 17);
            let mut l = vec![0.; (sr * 0.3) as usize];
            let mut r = l.clone();
            l[0] = 0.5;
            run(&mut d, &mut l, &mut r, 127);
            let expected = (sr * ms * 0.001) as usize;
            let peak = l
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0;
            assert!(
                peak.abs_diff(expected) <= 3,
                "sr={sr} control={control} peak={peak} expected={expected}"
            );
        }
    }
}

#[test]
fn fades_and_mode_switches_have_bounded_steps_and_sample_aligned_partitions() {
    for sr in [22050., 48000., 192000.] {
        let render = |block| {
            let mut d = patch(sr, 0.);
            for (i, v) in [(2, 20.), (3, 0.8), (10, 1.)] {
                d.set_param(i, v);
            }
            let n = (sr * 0.6) as usize;
            let mut l: Vec<_> = (0..n)
                .map(|i| 0.4 * libm::sinf(i as f32 * std::f32::consts::TAU * 100. / sr))
                .collect();
            let mut r = l.clone();
            let events = [
                (0, 2, 20.),
                ((sr * 0.1) as usize, 2, 20.05),
                ((sr * 0.17) as usize, 9, 1.),
                ((sr * 0.31) as usize, 9, 2.),
                ((sr * 0.43) as usize, 9, 0.),
            ];
            for (k, &(start, index, value)) in events.iter().enumerate() {
                d.set_param(index, value);
                let end = events.get(k + 1).map_or(n, |x| x.0);
                run(&mut d, &mut l[start..end], &mut r[start..end], block);
            }
            let step = l
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max);
            eprintln!("step sr={sr} block={block}: {step}");
            assert!(step < 0.05 * 22050. / sr);
            assert!(l.iter().any(|v| v.abs() > 0.1));
            (l, r)
        };
        let reference = render(1);
        for block in [17, 64, 127, 128] {
            assert_eq!(reference, render(block));
        }
    }
}

#[test]
fn correlated_heads_do_not_raise_wet_gain_during_fade() {
    for sr in [22050., 48000., 192000.] {
        let mut d = patch(sr, 0.);
        d.set_param(3, 0.);
        d.set_param(2, 20.);
        let mut l: Vec<_> = (0..(sr * 0.3) as usize)
            .map(|i| 0.3 * libm::sinf(i as f32 * std::f32::consts::TAU * 300. / sr))
            .collect();
        let mut r = l.clone();
        let event = (sr * 0.15) as usize;
        run(&mut d, &mut l[..event], &mut r[..event], 127);
        let before = l[(sr * 0.1) as usize..event]
            .iter()
            .fold(0.0f32, |a, b| a.max(b.abs()));
        d.set_param(2, 20.05);
        run(&mut d, &mut l[event..], &mut r[event..], 17);
        let during = l[event..event + (sr * 0.05) as usize]
            .iter()
            .fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(
            before > 0.1 && during <= before * 1.01,
            "sr={sr} before={before} during={during}"
        );
    }
}
