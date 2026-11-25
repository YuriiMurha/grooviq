use grooviq_dsp::{
    Device,
    fattener::{Fattener, PRESETS, preset},
};

fn render(sr: f32, block: usize, name: &str) -> Vec<f32> {
    let mut d = Fattener::new(sr);
    for (i, v) in preset(name).unwrap() {
        d.set_param(i, v);
    }
    let mut seed = 73u32;
    let mut l = Vec::new();
    let mut r = Vec::new();
    for _ in 0..8192 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        l.push((seed as i32 as f32 / i32::MAX as f32) * 0.8);
        r.push(*l.last().unwrap() * -0.3);
    }
    for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(l, r, None, &[]);
    }
    l.into_iter().zip(r).flat_map(|(l, r)| [l, r]).collect()
}

#[test]
fn presets_are_finite_distinct_and_block_independent() {
    for sr in [22050., 48000., 192000.] {
        let init = render(sr, 128, "Init");
        for name in PRESETS {
            let expected = render(sr, 1, name);
            assert!(expected.iter().all(|x| x.is_finite() && x.abs() <= 1.));
            assert_ne!(expected, init, "{name}");
            for block in [7, 64, 127, 128] {
                assert_eq!(expected, render(sr, block, name));
            }
        }
    }
}

#[test]
fn neutral_is_identity_and_silent_channel_stays_silent() {
    let mut d = Fattener::new(48000.);
    let mut l = [0.25; 128];
    let mut r = [0.; 128];
    d.process(&mut l, &mut r, None, &[]);
    assert_eq!(l, [0.25; 128]);
    d.set_param(0, 1.);
    for _ in 0..100 {
        d.process(&mut l, &mut r, None, &[]);
    }
    assert_eq!(r, [0.; 128]);
}

#[test]
fn reset_replays_and_nonfinite_controls_are_rejected() {
    let mut d = Fattener::new(22050.);
    d.set_param(0, 1.);
    let run = |d: &mut Fattener| {
        let mut l = [0.7; 128];
        let mut r = [-0.2; 128];
        d.process(&mut l, &mut r, None, &[]);
        (l, r)
    };
    let first = run(&mut d);
    run(&mut d);
    d.reset();
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        d.set_param(0, v);
        d.set_param(1, v);
    }
    assert_eq!(first, run(&mut d));
}

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[test]
fn realtime_calls_allocate_nothing_and_extreme_inputs_stay_bounded() {
    let mut d = Fattener::new(192000.);
    let mut l = [f32::MAX; 128];
    let mut r = [-f32::MAX; 128];
    assert_no_alloc::assert_no_alloc(|| {
        d.set_param(0, 100.);
        d.set_param(1, 100.);
        d.process(&mut l, &mut r, None, &[]);
        d.reset();
    });
    assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() <= 1.));
}

#[test]
fn linked_detector_reduces_quiet_channel_when_other_channel_is_loud() {
    fn quiet_with(loud: f32) -> f32 {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 1.);
        let mut result = 0.;
        for _ in 0..200 {
            let mut l = [loud; 128];
            let mut r = [0.05; 128];
            d.process(&mut l, &mut r, None, &[]);
            result = r[127];
        }
        result
    }
    assert!(quiet_with(1.) < quiet_with(0.05) * 0.5);
}

#[test]
fn macro_jump_is_smoothed_and_silence_remains_silent() {
    for sr in [22050., 192000.] {
        let mut d = Fattener::new(sr);
        let mut l = [0.1; 128];
        let mut r = l;
        d.process(&mut l, &mut r, None, &[]);
        d.set_param(0, 1.);
        d.set_param(1, 1.);
        let mut l = [0.1; 128];
        let mut r = l;
        d.process(&mut l, &mut r, None, &[]);
        assert!((l[0] - 0.1).abs() < 0.005);
        assert!(l.windows(2).all(|v| (v[1] - v[0]).abs() < 0.005));
        d.reset();
        let mut l = [0.; 128];
        let mut r = l;
        d.process(&mut l, &mut r, None, &[]);
        assert_eq!(l, [0.; 128]);
        assert_eq!(r, l);
    }
}

#[test]
fn colour_changes_spectrum_and_channel_swap_is_symmetric() {
    fn tone(colour: f32, hz: f32) -> f32 {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 0.6);
        d.set_param(1, colour);
        let mut energy = 0.;
        for block in 0..100 {
            let mut l = std::array::from_fn::<_, 128, _>(|i| {
                0.03 * libm::sinf(
                    2. * std::f32::consts::PI * hz * (block * 128 + i) as f32 / 48000.,
                )
            });
            let mut r = l;
            d.process(&mut l, &mut r, None, &[]);
            if block > 50 {
                energy += l.iter().map(|v| v * v).sum::<f32>();
            }
        }
        energy
    }
    assert!(tone(1., 8000.) > tone(-1., 8000.) * 2.);
    assert!(tone(-1., 80.) > tone(1., 80.) * 2.);
    let mut a = Fattener::new(48000.);
    let mut b = Fattener::new(48000.);
    for d in [&mut a, &mut b] {
        d.set_param(0, 0.8);
        d.set_param(1, -0.5);
    }
    for _ in 0..50 {
        let mut l = [0.6; 128];
        let mut r = [-0.1; 128];
        let mut sl = r;
        let mut sr = l;
        a.process(&mut l, &mut r, None, &[]);
        b.process(&mut sl, &mut sr, None, &[]);
        assert_eq!(l, sr);
        assert_eq!(r, sl);
    }
    let renders: Vec<_> = PRESETS
        .iter()
        .map(|name| render(48000., 64, name))
        .collect();
    for i in 0..renders.len() {
        for j in i + 1..renders.len() {
            assert_ne!(renders[i], renders[j]);
        }
    }
}

fn sig(n: usize, ch: u32) -> Vec<f32> {
    let mut s = 0x1234_5678u32 ^ ch;
    (0..n)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            0.5 * libm::sinf(i as f32 * 0.013 * (ch + 1) as f32)
                + 0.45 * (s as i32 as f32 / i32::MAX as f32)
        })
        .collect()
}
// automation events at sample-aligned points; host splits blocks at each event
fn automated(block: usize, sr: f32) -> Vec<f32> {
    let n = 20_000;
    let (mut l, mut r) = (sig(n, 0), sig(n, 1));
    let ev: [(usize, u32, f32); 8] = [
        (0, 0, 0.3),
        (1001, 0, 1.0),
        (1001, 1, -1.0),
        (1051, 0, 0.4),
        (1107, 1, 0.3),
        (5003, 1, 0.8),
        (9999, 0, 0.0),
        (12345, 1, 0.0),
    ];
    let mut d = Fattener::new(sr);
    let mut i = 0;
    while i < n {
        for &(at, p, v) in &ev {
            if at == i {
                d.set_param(p, v);
            }
        }
        let next_ev = ev.iter().map(|e| e.0).filter(|&a| a > i).min().unwrap_or(n);
        let e = (i + block).min(next_ev).min(n);
        d.process(&mut l[i..e], &mut r[i..e], None, &[]);
        i = e;
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}
#[test]
fn sample_aligned_automation_is_bit_exact_across_blocks() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        let a = automated(1, sr);
        for b in [2, 7, 37, 64, 128, 1024] {
            assert!(
                a.iter()
                    .zip(automated(b, sr))
                    .all(|(x, y)| x.to_bits() == y.to_bits()),
                "sr {sr} block {b}"
            );
        }
    }
}
#[test]
fn reset_after_automation_matches_fresh_device() {
    let n = 4096;
    let fresh = {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 0.7);
        d.set_param(1, 0.4);
        let (mut l, mut r) = (sig(n, 0), sig(n, 1));
        d.process(&mut l, &mut r, None, &[]);
        (l, r)
    };
    let mut d = Fattener::new(48000.);
    let (mut l, mut r) = (sig(n, 2), sig(n, 3));
    d.process(&mut l[..100], &mut r[..100], None, &[]);
    d.set_param(0, 0.7);
    d.set_param(1, 0.4); // mid-ramp
    d.process(&mut l[100..150], &mut r[100..150], None, &[]);
    d.reset();
    // case A: host re-sends params after reset
    let (mut l, mut r) = (sig(n, 0), sig(n, 1));
    d.set_param(0, 0.7);
    d.set_param(1, 0.4);
    d.process(&mut l, &mut r, None, &[]);
    assert!(
        l == fresh.0 && r == fresh.1,
        "reset+resend differs from fresh"
    );
    // case B: host does not resend
    let mut d = Fattener::new(48000.);
    let (mut l, mut r) = (sig(n, 2), sig(n, 3));
    d.process(&mut l[..100], &mut r[..100], None, &[]);
    d.set_param(0, 0.7);
    d.set_param(1, 0.4);
    d.process(&mut l[100..150], &mut r[100..150], None, &[]);
    d.reset();
    let (mut l, mut r) = (sig(n, 0), sig(n, 1));
    d.process(&mut l, &mut r, None, &[]);
    assert!(
        l == fresh.0 && r == fresh.1,
        "reset without resend differs from fresh"
    );
}
#[test]
fn nonfinite_input_does_not_poison_state() {
    let mut d = Fattener::new(48000.);
    d.set_param(0, 0.8);
    d.set_param(1, 0.5);
    let mut l = [0.3f32; 64];
    let mut r = l;
    l[5] = f32::NAN;
    r[9] = f32::INFINITY;
    l[20] = f32::NEG_INFINITY;
    d.process(&mut l, &mut r, None, &[]);
    let mut l2 = [0.3f32; 4096];
    let mut r2 = l2;
    d.process(&mut l2, &mut r2, None, &[]);
    assert!(
        l.iter()
            .chain(&r)
            .chain(&l2)
            .chain(&r2)
            .all(|x| x.is_finite() && x.abs() <= 1.0)
    );
}
#[test]
fn right_only_transient_ducks_left() {
    let run = |hit: f32| {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 1.0);
        let mut l = vec![0.05f32; 4800];
        let mut r = vec![0.0f32; 4800];
        for x in &mut r[2400..2500] {
            *x = hit;
        }
        d.process(&mut l, &mut r, None, &[]);
        l[2500]
    };
    let (with, without) = (run(1.0), run(0.0));
    println!("left at 2500 with right hit {with}, without {without}");
    assert!(with < without * 0.5);
}

fn sine(hz: f32, sr: f32, n: usize, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|i| amp * libm::sinf(2. * std::f32::consts::PI * hz * i as f32 / sr))
        .collect()
}
#[test]
fn reset_then_new_values_match_fresh() {
    let n = 4096;
    let x = sine(330., 48000., n, 0.6);
    let fresh = {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 0.9);
        d.set_param(1, -0.8);
        let (mut l, mut r) = (x.clone(), x.clone());
        d.process(&mut l, &mut r, None, &[]);
        l
    };
    let mut d = Fattener::new(48000.);
    d.set_param(0, 0.1);
    d.set_param(1, 0.5);
    let (mut l, mut r) = (x.clone(), x.clone());
    d.process(&mut l, &mut r, None, &[]);
    d.reset();
    d.set_param(0, 0.9);
    d.set_param(1, -0.8);
    let (mut l, mut r) = (x.clone(), x.clone());
    d.process(&mut l, &mut r, None, &[]);
    assert!(l == fresh);
}
#[test]
fn colour_jump_is_smoothed_per_sample() {
    let x = sine(100., 48000., 9600, 0.3);
    let run = |jump: bool| {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 1.);
        d.set_param(1, -1.);
        let (mut l, mut r) = (x.clone(), x.clone());
        d.process(&mut l[..4800], &mut r[..4800], None, &[]);
        if jump {
            d.set_param(1, 1.);
        }
        d.process(&mut l[4800..], &mut r[4800..], None, &[]);
        l
    };
    let (a, b) = (run(true), run(false));
    assert!(
        a[5000..]
            .iter()
            .zip(&b[5000..])
            .any(|(a, b)| (a - b).abs() > 0.05)
    );
    assert!(
        (a[4800] - b[4800]).abs() < 0.01,
        "colour step {}",
        (a[4800] - b[4800]).abs()
    );
}
fn settle_ms(sr: f32) -> f32 {
    // Amount jump on a constant 0.1 input: ms until within 1% of final
    let n = sr as usize / 5;
    let mut d = Fattener::new(sr);
    let (mut l, mut r) = (vec![0.1f32; n], vec![0.1f32; n]);
    d.process(&mut l[..1], &mut r[..1], None, &[]);
    d.set_param(0, 1.);
    d.process(&mut l[1..], &mut r[1..], None, &[]);
    let fin = l[n - 1];
    (l.iter()
        .position(|v| (v - fin).abs() < 0.01 * fin.abs())
        .unwrap() as f32)
        * 1000.
        / sr
}
fn colour_settle_ms(sr: f32) -> f32 {
    // A quiet DC signal keeps the compressor idle. Warm the tilt filter first,
    // then measure the Colour step reaching 99% of its audible output change.
    let mut d = Fattener::new(sr);
    d.set_param(0, 1.);
    d.set_param(1, -1.);
    let n = (sr * 0.2) as usize;
    let (mut l, mut r) = (vec![0.01; n], vec![0.01; n]);
    d.process(&mut l, &mut r, None, &[]);
    let initial = l[n - 1];
    d.set_param(1, 1.);
    l.fill(0.01);
    r.fill(0.01);
    d.process(&mut l, &mut r, None, &[]);
    let final_value = l[n - 1];
    assert!((initial - final_value).abs() > 0.01);
    let tolerance = (initial - final_value).abs() * 0.01;
    (l.iter()
        .position(|v| (v - final_value).abs() <= tolerance)
        .unwrap()
        + 1) as f32
        * 1000.
        / sr
}

#[test]
fn colour_smoothing_has_physical_duration() {
    for sr in [22050., 48000., 192000.] {
        let ms = colour_settle_ms(sr);
        // The 10 ms control smoother should take roughly 4.6 time constants
        // to cover 99% of the step; allow for the nonlinear output mapping.
        assert!(
            (40. ..55.).contains(&ms),
            "Colour settle at {sr} Hz: {ms} ms"
        );
    }
}

#[test]
fn empty_process_before_preset_matches_fresh_device() {
    for sr in [22050., 48000., 192000.] {
        for name in PRESETS {
            let mut fresh = Fattener::new(sr);
            let mut empty = Fattener::new(sr);
            empty.process(&mut [], &mut [], None, &[]);
            for d in [&mut fresh, &mut empty] {
                for (i, v) in preset(name).unwrap() {
                    d.set_param(i, v);
                }
            }
            let (mut fl, mut fr) = (sig(4096, 0), sig(4096, 1));
            let (mut el, mut er) = (fl.clone(), fr.clone());
            fresh.process(&mut fl, &mut fr, None, &[]);
            empty.process(&mut el, &mut er, None, &[]);
            assert_eq!(el, fl, "{name}, {sr} Hz, left");
            assert_eq!(er, fr, "{name}, {sr} Hz, right");
        }
    }
}

fn release_ms(sr: f32) -> f32 {
    // right hit then quiet: ms for the left to recover to 90% of its steady value
    let n = sr as usize;
    let mut d = Fattener::new(sr);
    d.set_param(0, 1.);
    let mut l = vec![0.05f32; n];
    let mut r = vec![0.0f32; n];
    let h = n / 4;
    for x in &mut r[h..h + (sr as usize / 100)] {
        *x = 1.;
    }
    d.process(&mut l, &mut r, None, &[]);
    let fin = l[n - 1];
    let start = h + sr as usize / 100;
    (l[start..].iter().position(|v| *v > 0.9 * fin).unwrap() as f32) * 1000. / sr
}
fn tilt_db(sr: f32) -> f32 {
    // Amount small, colour +1 vs -1 gain at 3 kHz, in dB
    let e = |c: f32| {
        let mut d = Fattener::new(sr);
        d.set_param(0, 0.05);
        d.set_param(1, c);
        let x = sine(3000., sr, sr as usize / 2, 0.01);
        let (mut l, mut r) = (x.clone(), x.clone());
        d.process(&mut l, &mut r, None, &[]);
        let t = &l[l.len() / 2..];
        t.iter().map(|v| v * v).sum::<f32>()
    };
    10. * libm::log10f(e(1.) / e(-1.))
}
#[test]
fn time_constants_and_tilt_follow_sample_rate() {
    for sr in [22050., 48000., 192000.] {
        for (value, expected, tolerance, what) in [
            (settle_ms(sr), 50., 5., "Amount settle ms"),
            (release_ms(sr), 135., 14., "release ms"),
            (tilt_db(sr), 0.49, 0.05, "tilt dB"),
        ] {
            assert!(
                (value - expected).abs() <= tolerance,
                "{what} at {sr} Hz: {value}, expected {expected} ± {tolerance}"
            );
        }
    }
}

#[test]
fn short_transient_attack_tracks_physical_time() {
    let probe = |sr: f32| {
        let mut d = Fattener::new(sr);
        d.set_param(0, 1.);
        let n = (sr * 0.001) as usize;
        let mut l = vec![0.05; n];
        let mut r = vec![1.; n];
        d.process(&mut l, &mut r, None, &[]);
        l[n - 1]
    };
    for sr in [22050., 48000., 192000.] {
        let value = probe(sr);
        assert!(
            (value - 0.1515).abs() < 0.005,
            "1 ms attack endpoint at {sr} Hz: {value}"
        );
    }
}
#[test]
fn extreme_inputs_both_colour_signs() {
    for c in [-1., 1.] {
        let mut d = Fattener::new(48000.);
        d.set_param(0, 1.);
        d.set_param(1, c);
        let mut l = [f32::MAX; 128];
        let mut r = [-f32::MAX; 128];
        d.process(&mut l, &mut r, None, &[]);
        assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() <= 1.));
        let mut reference = Fattener::new(48000.);
        reference.set_param(0, 1.);
        reference.set_param(1, c);
        let (mut bounded_l, mut bounded_r) = ([1.; 128], [-1.; 128]);
        reference.process(&mut bounded_l, &mut bounded_r, None, &[]);
        assert_eq!(l, bounded_l);
        assert_eq!(r, bounded_r);
    }
}
