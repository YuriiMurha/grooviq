use grooviq_dsp::{Device, MAX_BLOCK, delay::Delay};

const SR: f32 = 48_000.0;
const RATES: [f32; 5] = [22_050.0, 44_100.0, SR, 96_000.0, 192_000.0];

fn wet_delay(sr: f32, feedback: f32, pingpong: bool) -> Delay {
    let mut delay = Delay::new(sr);
    delay.set_param(3, feedback);
    delay.set_param(4, f32::from(pingpong));
    delay.set_param(5, 20.0);
    delay.set_param(6, 20_000.0);
    delay.set_param(7, 1.0);
    delay
}

fn render(delay: &mut Delay, left: &mut [f32], right: &mut [f32], block: usize) {
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        delay.process(l, r, None, &[]);
    }
}

fn peak(samples: impl Iterator<Item = f32>) -> f32 {
    samples.fold(0.0, |max, sample| max.max(sample.abs()))
}

fn automated_tail(sync: bool) {
    const BLOCK: usize = 64;
    const FRAMES: usize = 2 * SR as usize;
    const BURST: usize = SR as usize / 5;

    let mut failures = Vec::new();
    for pingpong in [false, true] {
        let mut delay = wet_delay(SR, 0.95, pingpong);
        delay.set_param(0, f32::from(sync));
        delay.set_param(1, 0.0);
        delay.set_param(2, 20.0);
        delay.set_tempo(120.0);
        let hz = if sync { 240.0 } else { 500.0 };
        let mut left = vec![0.0; FRAMES];
        for (i, sample) in left[..BURST].iter_mut().enumerate() {
            *sample = 0.125 * libm::sinf(std::f32::consts::TAU * hz * i as f32 / SR);
        }
        let mut right = left.clone();
        for (i, (l, r)) in left
            .chunks_mut(BLOCK)
            .zip(right.chunks_mut(BLOCK))
            .enumerate()
        {
            if i * BLOCK >= BURST {
                if sync {
                    delay.set_tempo(if i % 2 == 0 { 120.0 } else { 121.0 });
                } else {
                    delay.set_param(2, if i % 2 == 0 { 20.0 } else { 20.05 });
                }
            }
            delay.process(l, r, None, &[]);
        }
        let window = |start, end| peak(left[start..end].iter().chain(&right[start..end]).copied());
        let early = window(14_400, 26_400); // 0.30–0.55 s
        let late = window(84_000, FRAMES); // 1.75–2.00 s
        let overall = window(0, FRAMES);
        let bounded = left
            .iter()
            .chain(&right)
            .all(|v| v.is_finite() && v.abs() <= 1.1);
        let result =
            format!("pingpong={pingpong}: peak={overall:.6}, early={early:.6}, late={late:.6}");
        println!("{result}");
        if !bounded || early <= 0.01 || late >= early * 0.5 {
            failures.push(result);
        }
    }
    assert!(
        failures.is_empty(),
        "tail exceeded 1.1 or failed to decay by 6 dB: {failures:?}"
    );
}

#[test]
fn free_time_automation_tail_decays() {
    automated_tail(false);
}

#[test]
fn synced_tempo_automation_tail_decays() {
    automated_tail(true);
}

fn synced_impulse(
    sr: f32,
    bpm: f32,
    division: u32,
    frames: usize,
    block: usize,
) -> (f32, Vec<f32>, Vec<f32>) {
    let mut delay = wet_delay(sr, 0.0, false);
    delay.set_param(1, division as f32);
    delay.set_tempo(bpm);
    let mut left = vec![0.0; frames];
    left[0] = 0.8;
    let mut right = left.clone();
    render(&mut delay, &mut left, &mut right, block);
    (delay.meter(2), left, right)
}

fn echo_onset(samples: &[f32], threshold: f32) -> Option<usize> {
    // The wet-only mix has a tiny dry coefficient from cos(pi/2).
    samples.iter().position(|sample| sample.abs() > threshold)
}

fn assert_same_audio(
    left: &[f32],
    right: &[f32],
    reference: &(f32, Vec<f32>, Vec<f32>),
    sr: f32,
    block: usize,
) {
    assert!(
        left.iter()
            .chain(right)
            .zip(reference.1.iter().chain(&reference.2))
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "output bits differ at {sr} Hz with {block}-frame blocks"
    );
}

#[test]
fn minimum_tempo_reads_two_beats() {
    for sr in RATES {
        let expected = 6 * sr as usize;
        let reference = synced_impulse(sr, 20.0, 8, expected + 8, 1);
        // At 192 kHz the 20 kHz low-pass attenuates the impulse's first sample.
        let onset = echo_onset(&reference.1, 0.02);
        println!(
            "{sr} Hz: meter={}, onset={onset:?}, expected={expected}",
            reference.0
        );
        assert_eq!((reference.0, onset), (6000.0, Some(expected)), "at {sr} Hz");
        for block in [7, 64, MAX_BLOCK] {
            let (_, left, right) = synced_impulse(sr, 20.0, 8, expected + 8, block);
            assert_same_audio(&left, &right, &reference, sr, block);
        }
    }
}

#[test]
fn maximum_tempo_shortest_division_is_block_invariant() {
    for sr in RATES {
        let expected = 0.01875 * sr;
        let frames = (0.05 * sr) as usize;
        let reference = synced_impulse(sr, 400.0, 0, frames, 1);
        let onset = echo_onset(&reference.1, 0.05);
        println!(
            "{sr} Hz: meter={}, onset={onset:?}, expected={expected}",
            reference.0
        );
        assert_eq!(reference.0, 18.75, "at {sr} Hz");
        assert!(
            onset.is_some_and(|n| (n as f32 - expected).abs() <= 1.0),
            "at {sr} Hz: onset={onset:?}, expected={expected}"
        );
        for block in [7, 64, MAX_BLOCK] {
            let (_, left, right) = synced_impulse(sr, 400.0, 0, frames, block);
            assert_same_audio(&left, &right, &reference, sr, block);
        }
    }
}

#[test]
fn active_fade_finishes_and_latest_queued_target_is_used() {
    const FADE: usize = 2400; // 50 ms at SR
    const START: usize = 64;
    const FIRST_END: usize = START + FADE;
    const LAST_UPDATE: usize = START + FADE / 2;
    const DEADLINE: usize = LAST_UPDATE + 2 * FADE;
    const LAST_MS: f32 = 125.0;
    const END: usize = DEADLINE + 8;

    let render_changes = |queue: bool| {
        let mut delay = wet_delay(SR, 0.0, false);
        delay.set_param(0, 0.0);
        delay.set_param(2, 200.0);
        let mut left = [0.0; END];
        let mut right = [0.0; END];
        // The final head can read this impulse exactly at the deadline.
        left[DEADLINE - 6000] = 0.8;
        render(&mut delay, &mut left[..START], &mut right[..START], 64);
        delay.set_param(2, 30.0);
        for (start, end, ms) in [
            (START, START + 64, 30.0),
            (START + 64, LAST_UPDATE, 3.0),
            (LAST_UPDATE, END, LAST_MS),
        ] {
            if queue {
                delay.set_param(2, ms);
            }
            render(
                &mut delay,
                &mut left[start..end],
                &mut right[start..end],
                64,
            );
        }
        left
    };
    let first_only = render_changes(false);
    let queued = render_changes(true);
    assert!(peak(first_only[START..FIRST_END].iter().copied()) > 0.1);
    assert_eq!(queued[..FIRST_END], first_only[..FIRST_END]);

    let mut settled = wet_delay(SR, 0.0, false);
    settled.set_param(0, 0.0);
    settled.set_param(2, LAST_MS);
    let mut reference = [0.0; END];
    let mut right = [0.0; END];
    reference[DEADLINE - 6000] = 0.8;
    render(&mut settled, &mut reference, &mut right, 64);
    assert!(reference[DEADLINE] > 0.1);
    let error = peak(
        queued[DEADLINE..]
            .iter()
            .zip(&reference[DEADLINE..])
            .map(|(a, b)| a - b),
    );
    println!("queued echo error at frame {DEADLINE}: {error:.9}");
    assert!(
        error < 0.00001,
        "last queued target has not settled: error={error}"
    );
}

#[test]
fn max_feedback_tail_settles_without_subnormals() {
    for pingpong in [false, true] {
        let mut delay = wet_delay(SR, 0.95, pingpong);
        delay.set_param(0, 0.0);
        delay.set_param(2, 20.0);
        let mut left = vec![0.0; 40 * SR as usize];
        left[0] = 0.8;
        let mut right = left.clone();
        render(&mut delay, &mut left, &mut right, MAX_BLOCK);
        let tiny = peak(
            left[39 * SR as usize..]
                .iter()
                .chain(&right[39 * SR as usize..])
                .copied(),
        );
        let subnormals = left
            .iter()
            .chain(&right)
            .filter(|v| v.is_subnormal())
            .count();
        println!("pingpong={pingpong}: final-second peak={tiny:.3e}, subnormals={subnormals}");
        assert!(
            peak(left[1..].iter().copied()) > 0.1,
            "the delay must have sounded"
        );
        assert_eq!(subnormals, 0, "pingpong={pingpong}");
        assert!(
            tiny < 1e-22,
            "pingpong={pingpong}: final-second peak={tiny:e}"
        );
    }
}
