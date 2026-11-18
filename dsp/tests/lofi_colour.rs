use grooviq_dsp::{
    Device,
    lofi_colour::{LofiColour, SPEC},
};
fn run(d: &mut LofiColour, n: usize, value: f32) -> Vec<f32> {
    let mut l = vec![value; n];
    let mut r = l.clone();
    d.process(&mut l, &mut r, None, &[]);
    l
}
#[test]
fn neutral_hostile_reset_and_modules() {
    for sr in [1., 22050., 44100., 48000., 96000., 192000., 1e9, f32::NAN] {
        let mut d = LofiColour::new(sr);
        assert_eq!(run(&mut d, 400, 0.37), vec![0.37; 400]);
        for i in 0..8 {
            d.set_param(i, f32::NAN);
            d.set_param(i, f32::INFINITY);
        }
        d.set_param(999, 1.);
        assert!(run(&mut d, 128, f32::NAN).iter().all(|x| x.is_finite()));
        for module in 1..=6 {
            let mut d = LofiColour::new(sr);
            d.set_param(0, 1.);
            d.set_param(module, 1.);
            let out = run(&mut d, 48000, 0.37);
            assert!(
                out.iter().any(|x| (*x - 0.37).abs() > 1e-4),
                "module {module}"
            );
            d.reset();
            assert!(run(&mut d, 48000, 0.).iter().all(|x| *x == 0.));
            let a = run(&mut d, 2048, 0.37);
            d.reset();
            run(&mut d, 48000, 0.);
            assert_eq!(a, run(&mut d, 2048, 0.37));
        }
    }
}
#[test]
fn exact_events_and_reset_ramps() {
    fn render(block: usize) -> Vec<f32> {
        let mut d = LofiColour::with_seed(48000., 0);
        let mut out = Vec::new();
        for event in 0..5 {
            for (i, s) in SPEC.iter().enumerate() {
                d.set_param(i as u32, if event % 2 == 0 { s.1 } else { s.0 });
            }
            let mut at = 0;
            while at < 3001 {
                let n = block.min(3001 - at);
                out.extend(run(&mut d, n, 0.23));
                d.process(&mut [], &mut [], None, &[]);
                at += n;
            }
            if event == 2 {
                d.reset();
            }
        }
        out
    }
    let a = render(1);
    for n in [7, 64, 127, 128] {
        assert_eq!(a, render(n));
    }
}

#[test]
fn independent_endpoints_and_physical_echo() {
    for sr in [22050., 48000., 192000.] {
        let mut d = LofiColour::new(sr);
        d.set_param(0, 1.);
        d.set_param(4, 1.);
        let a = run(&mut d, 32, 0.37);
        assert!(a.iter().all(|&v| (v - 12. / 32.).abs() < 1e-6));
        let mut d = LofiColour::new(sr);
        d.set_param(0, 1.);
        d.set_param(3, 1.);
        let a = run(&mut d, 32, 0.2);
        assert!(a.iter().all(|&v| v > 0.98 && v < 1.));
        let mut d = LofiColour::new(sr);
        d.set_param(0, 1.);
        d.set_param(5, 1.);
        let mut l = vec![0.; (sr * 0.11) as usize];
        let mut r = l.clone();
        l[0] = 1.;
        r[0] = 1.;
        d.process(&mut l, &mut r, None, &[]);
        let peak = |x: &[f32]| {
            x.iter()
                .enumerate()
                .skip(1)
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0
        };
        assert!((peak(&l) as f32 / sr - 0.073).abs() < 2. / sr);
        assert!((peak(&r) as f32 / sr - 0.097).abs() < 2. / sr);
    }
    let noise = |mode: f32, seed: u32| {
        let mut d = LofiColour::with_seed(48000., seed);
        d.set_param(0, 1.);
        d.set_param(1, 1.);
        d.set_param(7, mode);
        run(&mut d, 4096, 0.37)
    };
    assert_ne!(noise(0., 0), noise(1., 0));
    assert_ne!(noise(1., 0), noise(2., 0));
    assert_ne!(noise(1., 0), noise(1., 1));
    for (a, b) in [(1., 22050.), (1e9, 192000.), (f32::NAN, 48000.)] {
        let mut x = LofiColour::new(a);
        let mut y = LofiColour::new(b);
        for i in 0..8 {
            x.set_param(i, 0.8);
            y.set_param(i, 0.8);
        }
        assert_eq!(run(&mut x, 4096, 0.37), run(&mut y, 4096, 0.37));
    }
}

#[test]
fn maximum_digital_amount_holds_sixteen_changing_frames_in_both_channels() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        let mut device = LofiColour::new(sr);
        device.set_param(0, 1.);
        device.set_param(4, 1.);
        let mut left = [0.625; 32];
        let mut right = [0.5; 32];
        left[0] = 0.375;
        right[0] = -0.25;
        device.process(&mut left, &mut right, None, &[]);
        assert_eq!(&left[..16], &[0.375; 16]);
        assert_eq!(&right[..16], &[-0.25; 16]);
        assert_eq!(&left[16..], &[0.625; 16]);
        assert_eq!(&right[16..], &[0.5; 16]);
    }
}

#[test]
fn space_second_echo_has_point_three_five_feedback_at_every_rate() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        let mut device = LofiColour::new(sr);
        device.set_param(0, 1.);
        device.set_param(5, 1.);
        let mut left = vec![0.; (sr * 0.21) as usize];
        let mut right = left.clone();
        left[0] = 1.;
        right[0] = 1.;
        device.process(&mut left, &mut right, None, &[]);
        for (signal, seconds) in [(&left, 0.073), (&right, 0.097)] {
            // Read-before-write adds one frame; summing each echo includes the
            // signed Hermite lobes at rates with fractional delay lengths.
            let delay = sr * seconds + 1.;
            let area = |repeat: f32| {
                let center = (repeat * delay) as usize;
                signal[center - 8..=center + 8].iter().sum::<f32>()
            };
            let (first, second) = (area(1.), area(2.));
            assert!((first - 0.5).abs() < 1e-4, "first echo at {sr}");
            assert!((second / first - 0.35).abs() < 1e-4, "feedback at {sr}");
        }
    }
}
