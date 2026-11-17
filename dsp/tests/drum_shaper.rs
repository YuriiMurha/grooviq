use grooviq_dsp::{
    Device,
    drum_shaper::{DrumShaper, PRESETS, preset},
};
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
fn run(sr: f32, params: &[(u32, f32)], block: usize) -> Vec<f32> {
    let mut d = DrumShaper::new(sr);
    for &(i, v) in params {
        d.set_param(i, v);
    }
    let mut l: Vec<f32> = (0..(sr as usize / 3))
        .map(|i| if i < sr as usize / 5 { 0.4 } else { 0.0 })
        .collect();
    let mut r = l.iter().map(|v| v * 0.25).collect::<Vec<_>>();
    for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        assert_no_alloc::assert_no_alloc(|| d.process(l, r, None, &[]));
        for (l, r) in l.iter().zip(r) {
            assert!(l.is_finite() && r.is_finite());
        }
    }
    l
}
fn energy(x: &[f32]) -> f32 {
    x.iter().map(|v| v * v).sum()
}
#[test]
fn attack_and_sustain_are_independent_and_bipolar() {
    let neutral = run(48000.0, &[], 128);
    for mode in 0..3 {
        for (index, range) in [(0, 0..480), (1, 4800..9000)] {
            let up = run(48000.0, &[(index, 12.0), (2, mode as f32)], 7);
            let down = run(48000.0, &[(index, -12.0), (2, mode as f32)], 64);
            let base = energy(&neutral[range.clone()]);
            assert!(energy(&up[range.clone()]) > base * 1.2);
            assert!(energy(&down[range]) < base * 0.85);
        }
    }
}
#[test]
fn presets_rates_blocks_and_reset() {
    for sr in [22050.0, 44100.0, 192000.0] {
        for name in PRESETS {
            let params = preset(name).unwrap();
            let a = run(sr, &params, 128);
            for b in [1, 7, 64, 127] {
                assert_eq!(a, run(sr, &params, b));
            }
        }
    }
    let mut d = DrumShaper::new(48000.0);
    for (i, v) in preset(PRESETS[0]).unwrap() {
        d.set_param(i, v);
    }
    let mut a = [0.2; 128];
    let mut b = [0.05; 128];
    d.process(&mut a, &mut b, None, &[]);
    d.reset();
    let mut c = [0.2; 128];
    let mut e = [0.05; 128];
    d.process(&mut c, &mut e, None, &[]);
    assert_eq!(a, c);
    assert_eq!(b, e);
}
#[test]
fn neutral_linking_nonfinite_and_switch_clicks() {
    let mut d = DrumShaper::new(48000.0);
    let mut l = [0.2; 128];
    let mut r = [0.05; 128];
    d.process(&mut l, &mut r, None, &[]);
    assert_eq!(l, [0.2; 128]);
    for i in 0..7 {
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            d.set_param(i, v);
        }
    }
    d.process(&mut l, &mut r, None, &[]);
    assert_eq!(l, [0.2; 128]);
    d.set_param(0, 12.0);
    d.set_param(1, -12.0);
    let mut last = 0.2;
    for n in 0..100 {
        d.set_param(2, (n % 3) as f32);
        d.set_param(3, ((n + 1) % 3) as f32);
        l.fill(0.2);
        r.fill(0.05);
        d.process(&mut l, &mut r, None, &[]);
        for (&x, &y) in l.iter().zip(&r) {
            assert!((x - last).abs() < 0.01);
            assert_eq!(x, y * 4.0);
            last = x;
        }
    }
    l[0] = f32::NAN;
    r[1] = f32::INFINITY;
    d.process(&mut l, &mut r, None, &[]);
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    let clean = run(48000.0, &[], 128);
    let sat = run(48000.0, &[(4, 1.0)], 128);
    assert!(energy(&sat) < energy(&clean) * 0.5);
}

#[test]
fn seeded_stereo_modes_curves_and_controls_are_distinct() {
    let render = |mode, curve, saturation| {
        let mut d = DrumShaper::new(48000.0);
        for (i, v) in [(0, 9.0), (1, -6.0), (2, mode), (3, curve), (4, saturation)] {
            d.set_param(i, v);
        }
        let mut seed = 1234567_u32;
        let mut result = Vec::new();
        for n in 0..100 {
            let mut l = [0.0; 128];
            let mut r = [0.0; 128];
            for (i, (l, r)) in l.iter_mut().zip(&mut r).enumerate() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let v = (seed as f32 / u32::MAX as f32 - 0.5) * 0.5;
                *l = if (n * 128 + i) % 2400 < 1200 {
                    v
                } else {
                    v * 0.05
                };
                *r = -*l * 0.5;
            }
            d.process(&mut l, &mut r, None, &[]);
            if saturation == 0.0 {
                for (&l, &r) in l.iter().zip(&r) {
                    assert_eq!(l, -r * 2.0);
                }
            }
            result.extend(l);
        }
        result
    };
    let base = render(0.0, 1.0, 0.0);
    assert_eq!(base, render(0.0, 1.0, 0.0));
    for other in [
        render(1.0, 1.0, 0.0),
        render(2.0, 1.0, 0.0),
        render(0.0, 0.0, 0.0),
        render(0.0, 2.0, 0.0),
        render(0.0, 1.0, 0.7),
    ] {
        let difference: f32 = base.iter().zip(other).map(|(a, b)| (a - b) * (a - b)).sum();
        assert!(
            difference / energy(&base) > 0.001,
            "difference ratio {}",
            difference / energy(&base)
        );
    }
    let mut d = DrumShaper::new(22050.0);
    let mut l = [0.25; 128];
    let mut r = [0.0; 128];
    assert_no_alloc::assert_no_alloc(|| {
        for i in 0..7 {
            d.set_param(i, 100.0);
        }
        d.process(&mut l, &mut r, None, &[]);
        d.reset();
    });
    assert_eq!(r, [0.0; 128]);
}
