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
        d.set_param(1, -100.);
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
