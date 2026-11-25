use grooviq_dsp::{Device, MAX_BLOCK, delay::Delay};

#[test]
fn legacy_delay_controls_and_processing_are_realtime_safe() {
    let mut delay = Delay::new(192_000.0);
    let finite = super::realtime(|| {
        delay.set_param(1, 8.0);
        delay.set_tempo(20.0);
        delay.set_param(3, 0.95);
        delay.set_param(5, 20.0);
        delay.set_param(6, 20_000.0);
        delay.set_param(7, 1.0);
        let mut finite = true;
        for block in 0..200 {
            let mut left = [0.0; MAX_BLOCK];
            let mut right = [0.0; MAX_BLOCK];
            left[0] = 0.25;
            match block {
                1 => delay.set_tempo(400.0),
                2 => {
                    delay.set_param(0, 0.0);
                    delay.set_param(2, 20.0);
                }
                77 => delay.set_param(2, 20.05),
                160 => delay.set_param(4, 0.0),
                _ => {}
            }
            delay.process(&mut left, &mut right, None, &[]);
            finite &= left.iter().chain(&right).all(|sample| sample.is_finite());
        }
        finite
    });
    assert!(finite);
}
