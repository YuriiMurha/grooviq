//! Multiband upward/downward compressor (`ott` module): level behaviour, flat band split, gains,
//! silence and denormals, parameter sweeps (no NaN, no clicks) and allocation freedom.

use grooviq_dsp::ott::{PARAM_IDS, PRESETS, SPEC, effective_splits, preset};
use grooviq_dsp::{Device, create};

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const SR: f32 = 48_000.0;

fn device() -> Box<dyn Device> {
    create("ott", SR).unwrap()
}

fn with(params: &[(u32, f32)]) -> Box<dyn Device> {
    let mut d = device();
    for &(i, v) in params {
        d.set_param(i, v);
    }
    d
}

/// Processes `l`/`r` in place in chunks of `block`.
fn run(dev: &mut dyn Device, l: &mut [f32], r: &mut [f32], block: usize) {
    let n = l.len();
    let mut i = 0;
    while i < n {
        let e = (i + block).min(n);
        dev.process(&mut l[i..e], &mut r[i..e], None, &[]);
        i = e;
    }
}

fn sine(freq: f32, n: usize, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            amp * libm::sin(2.0 * std::f64::consts::PI * freq as f64 * i as f64 / SR as f64) as f32
        })
        .collect()
}

fn db_amp(db: f32) -> f32 {
    libm::powf(10.0, db / 20.0)
}

fn rms_db(x: &[f32]) -> f32 {
    let p = x
        .iter()
        .map(|v| {
            let d = *v as f64;
            d * d
        })
        .sum::<f64>()
        / x.len() as f64;
    (10.0 * libm::log10(p.max(1e-24))) as f32
}

/// Level change in dB (RMS of the last half second) of a sine of `freq` at `amp_db` dBFS peak.
fn gain_for(params: &[(u32, f32)], freq: f32, amp_db: f32) -> f32 {
    let n = SR as usize * 2;
    let x = sine(freq, n, db_amp(amp_db));
    let (mut l, mut r) = (x.clone(), x.clone());
    run(&mut *with(params), &mut l, &mut r, 128);
    let tail = n - SR as usize / 2..;
    rms_db(&l[tail.clone()]) - rms_db(&x[tail])
}

fn settled_crossovers(rate: f32, low: f32, high: f32) -> Box<dyn Device> {
    let mut d = create("ott", rate).unwrap();
    // Start processing before setting the splits to exercise smoothing instead of the fresh snap.
    d.process(&mut [0.0], &mut [0.0], None, &[]);
    d.set_param(7, low);
    d.set_param(8, high);
    for _ in 0..(rate as usize).div_ceil(128) {
        d.process(&mut [0.0; 128], &mut [0.0; 128], None, &[]);
    }
    d
}

fn assert_crossover_hz(actual: f32, expected: f32) {
    // The existing f32 log2 and exp2 round trip can shift a frequency by a few ulps.
    assert!(
        (actual - expected).abs() < 0.01,
        "{actual} Hz vs {expected} Hz"
    );
}

/// Meters 6/7 report the snapped splits with no processing: after construction, after
/// parameters are set before the first block (project load), and after `reset()`.
#[test]
fn crossover_meters_follow_the_fresh_snap_and_reset() {
    let rate = 22_050.0f32;
    let expect = |low: f32, high: f32| effective_splits(low as f64, high as f64, rate as f64);
    let check = |d: &dyn Device, (low, high): (f64, f64)| {
        assert_crossover_hz(d.meter(6), low as f32);
        assert_crossover_hz(d.meter(7), high as f32);
    };
    let mut d = create("ott", rate).unwrap();
    check(&*d, expect(SPEC[7].2, SPEC[8].2));
    d.set_param(7, 1000.0);
    d.set_param(8, 1000.0);
    check(&*d, expect(1000.0, 1000.0));
    d.set_param(8, 12_000.0);
    check(&*d, expect(1000.0, 12_000.0));
    // Once processing has started a change glides; reset() snaps it, and the meters follow.
    d.process(&mut [0.0; 128], &mut [0.0; 128], None, &[]);
    d.set_param(7, 300.0);
    d.set_param(8, 4000.0);
    d.reset();
    check(&*d, expect(300.0, 4000.0));
}

#[test]
fn crossover_meters_report_spacing_without_moving_the_low_split() {
    for (low, high, effective_high) in [
        (1000.0, 1000.0, 1500.0),
        (800.0, 1000.0, 1200.0),
        (350.0, 3000.0, 3000.0),
    ] {
        let d = settled_crossovers(SR, low, high);
        assert_crossover_hz(d.meter(6), low);
        assert_crossover_hz(d.meter(7), effective_high);
        let (expected_low, expected_high) = effective_splits(low.into(), high.into(), SR.into());
        assert_crossover_hz(d.meter(6), expected_low as f32);
        assert_crossover_hz(d.meter(7), expected_high as f32);
    }
}

#[test]
fn crossover_meters_report_the_sample_rate_cap() {
    for (rate, effective_high) in [(22_050.0, 9922.5), (44_100.0, 12_000.0), (SR, 12_000.0)] {
        let d = settled_crossovers(rate, 350.0, 12_000.0);
        assert_crossover_hz(d.meter(6), 350.0);
        assert_crossover_hz(d.meter(7), effective_high);
        let (_, expected_high) = effective_splits(350.0, 12_000.0, rate.into());
        assert_crossover_hz(d.meter(7), expected_high as f32);
    }
}

#[test]
fn crossover_targets_preserve_clamp_order_and_precision() {
    assert_eq!(
        effective_splits(350.0, 12_000.0, 22_051.0),
        (350.0, 9922.95)
    );
    // An out-of-range low request exposes the original order: cap low, then space high above it.
    assert_eq!(effective_splits(5000.0, 12_000.0, 8000.0), (3600.0, 5400.0));
}

#[test]
fn crossover_meters_follow_the_active_smoothed_state() {
    let mut d = settled_crossovers(SR, 350.0, 3000.0);
    let before = (d.meter(6), d.meter(7));
    d.set_param(7, 800.0);
    d.set_param(8, 5000.0);
    assert_eq!((d.meter(6), d.meter(7)), before);
    d.process(&mut [0.0; 16], &mut [0.0; 16], None, &[]);
    assert!(d.meter(6) > before.0 && d.meter(6) < 800.0);
    assert!(d.meter(7) > before.1 && d.meter(7) < 5000.0);
}

#[test]
fn crossover_requests_with_the_same_effective_split_render_bit_identically() {
    for (rate, low, high, equivalent_high) in [
        // A request of 1500 Hz itself rounds slightly above the spacing floor after log2/exp2.
        (SR, 1000.0, 1000.0, 1499.0),
        (22_050.0, 350.0, 12_000.0, 10_000.0),
    ] {
        let mut device = settled_crossovers(rate, low, high);
        let mut equivalent = settled_crossovers(rate, low, equivalent_high);
        let mut l = sine(1300.0, 4096, 0.2);
        let mut r = sine(1700.0, 4096, 0.1);
        let (mut expected_l, mut expected_r) = (l.clone(), r.clone());
        run(&mut *device, &mut l, &mut r, 128);
        run(&mut *equivalent, &mut expected_l, &mut expected_r, 128);
        for (actual, expected) in l.iter().chain(&r).zip(expected_l.iter().chain(&expected_r)) {
            assert_eq!(actual.to_bits(), expected.to_bits(), "{rate} Hz, {high} Hz");
        }
    }
}

#[test]
fn crossover_meters_match_the_audible_band_boundaries() {
    let mut d = settled_crossovers(SR, 1000.0, 1000.0);
    let mut l = sine(d.meter(6), SR as usize / 4, 0.3);
    let mut r = l.clone();
    run(&mut *d, &mut l, &mut r, 128);
    // At the low crossover the low-pass contributes half the input amplitude, with unity
    // magnitude through the following allpass.
    let low = d.meter(0);
    assert!(
        (low - 20.0 * libm::log10f(0.15)).abs() < 0.1,
        "low {low} dB"
    );

    let mut l = sine(d.meter(7), SR as usize / 4, 0.3);
    let mut r = l.clone();
    run(&mut *d, &mut l, &mut r, 128);
    // The two Linkwitz-Riley branches split the same signal equally at the high crossover.
    let mid = d.meter(1);
    let high = d.meter(2);
    assert!(
        mid > -30.0 && (mid - high).abs() < 0.1,
        "mid {mid} dB, high {high} dB"
    );
}

#[test]
fn param_ids_match_the_spec() {
    assert_eq!(PARAM_IDS.len(), SPEC.len());
    for (i, id) in PARAM_IDS.iter().enumerate() {
        assert!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
        );
        assert!(!PARAM_IDS[..i].contains(id), "duplicate id {id}");
    }
    // existing indices keep their meaning
    assert_eq!(
        &PARAM_IDS[..10],
        &[
            "depth",
            "time",
            "upward",
            "downward",
            "low_gain",
            "mid_gain",
            "high_gain",
            "low_mid_split",
            "mid_high_split",
            "output_gain"
        ]
    );
    assert_eq!(grooviq_dsp::param_spec("ott").unwrap().len(), SPEC.len());
}

#[test]
fn a_quiet_signal_is_lifted_and_a_loud_one_pushed_down() {
    let full = [(0u32, 1.0f32), (1, 0.5)];
    // -60 dBFS: well under the upward threshold, so about +13.8 dB (24 dB of headroom below it).
    let quiet = gain_for(&full, 1000.0, -60.0);
    assert!(
        (11.0..=16.0).contains(&quiet),
        "quiet signal changed by {quiet:+.1} dB"
    );
    // -6 dBFS: far over the downward threshold, so it lands near the threshold.
    let loud = gain_for(&full, 1000.0, -6.0);
    assert!(
        (-27.0..=-20.0).contains(&loud),
        "loud signal changed by {loud:+.1} dB"
    );
    // a signal between the thresholds is left alone (within the knee's reach)
    let mid = gain_for(&full, 1000.0, -36.0);
    assert!(
        mid.abs() < 2.0,
        "signal between the thresholds changed by {mid:+.1} dB"
    );
}

#[test]
fn the_amounts_scale_each_direction_and_each_band() {
    let base = |up: f32, down: f32, extra: &[(u32, f32)]| {
        let mut p = vec![(0u32, 1.0f32), (2, up), (3, down)];
        p.extend_from_slice(extra);
        p
    };
    // upward off: a quiet signal stays put; downward off: a loud one stays put
    assert!(gain_for(&base(0.0, 1.0, &[]), 1000.0, -60.0).abs() < 0.5);
    assert!(gain_for(&base(1.0, 0.0, &[]), 1000.0, -6.0).abs() < 0.5);
    // half the upward amount is about half the lift in dB
    let full = gain_for(&base(1.0, 1.0, &[]), 1000.0, -60.0);
    let half = gain_for(&base(0.5, 1.0, &[]), 1000.0, -60.0);
    assert!(
        (half - 0.5 * full).abs() < 1.0,
        "half {half:+.1} vs full {full:+.1}"
    );
    // 1 kHz sits in the mid band (120 Hz..2.5 kHz): the mid amounts act, the others do not
    for (idx, amp_db) in [(12u32, -60.0f32), (15, -6.0)] {
        let on = gain_for(&base(1.0, 1.0, &[]), 1000.0, amp_db);
        let off = gain_for(&base(1.0, 1.0, &[(idx, 0.0)]), 1000.0, amp_db);
        assert!(
            on.abs() > 8.0 && off.abs() < 0.5,
            "param {idx}: on {on:+.1}, off {off:+.1}"
        );
    }
    for idx in [11u32, 13, 14, 16] {
        let off = gain_for(&base(1.0, 1.0, &[(idx, 0.0)]), 1000.0, -60.0);
        let on = gain_for(&base(1.0, 1.0, &[]), 1000.0, -60.0);
        assert!(
            (on - off).abs() < 0.5,
            "param {idx} leaks into the mid band: {on:+.1} vs {off:+.1}"
        );
    }
}

/// The low (params 11, 14) and high (13, 16) band amounts act on their own band: an 80 Hz sine
/// sits in the low band and an 8 kHz sine in the high band. The splits are moved to 400 Hz and
/// 2 kHz so each sine is two octaves or more from the nearest crossover (the 24 dB/oct slopes
/// then keep it out of the neighbouring band's detector).
#[test]
fn the_low_and_high_band_amounts_act_on_their_own_band() {
    let base = [(0u32, 1.0f32), (7, 400.0), (8, 2000.0)];
    // (parameter, frequency, level in dBFS: -60 for upward, -6 for downward)
    for (idx, hz, amp_db) in [
        (11u32, 80.0f32, -60.0f32),
        (14, 80.0, -6.0),
        (13, 8000.0, -60.0),
        (16, 8000.0, -6.0),
    ] {
        let on = gain_for(&base, hz, amp_db);
        let off = gain_for(&[base[0], base[1], base[2], (idx, 0.0)], hz, amp_db);
        assert!(
            on.abs() > 8.0 && off.abs() < 0.5,
            "{} at {hz} Hz, {amp_db} dBFS: on {on:+.1} dB, off {off:+.1} dB",
            PARAM_IDS[idx as usize]
        );
    }
}

/// The crossovers sum flat: with depth 0 the output level equals the input level for sines from
/// 20 Hz to 20 kHz, at the default splits and at the extremes of both split controls.
#[test]
fn depth_zero_is_the_flat_band_split() {
    const TOL_DB: f32 = 0.05;
    let freqs = [
        20.0, 31.5, 63.0, 100.0, 120.0, 150.0, 250.0, 500.0, 1000.0, 2000.0, 2500.0, 3000.0,
        5000.0, 8000.0, 12000.0, 16000.0, 20000.0,
    ];
    for splits in [
        (120.0, 2500.0),
        (40.0, 1000.0),
        (1000.0, 12000.0),
        (500.0, 1500.0),
    ] {
        let p = [(0u32, 0.0f32), (7, splits.0), (8, splits.1)];
        for f in freqs {
            // 20 Hz needs a long window: measure whole cycles after the filters settle
            let n = SR as usize * 3;
            let x = sine(f, n, 0.3);
            let (mut l, mut r) = (x.clone(), x.clone());
            run(&mut *with(&p), &mut l, &mut r, 128);
            let w = SR as usize * 2..;
            let g = rms_db(&l[w.clone()]) - rms_db(&x[w]);
            assert!(
                g.abs() < TOL_DB,
                "splits {splits:?}: {f} Hz changed by {g:+.3} dB"
            );
        }
    }
}

#[test]
fn input_and_output_gain_move_the_level_by_the_set_db() {
    // depth 0 keeps the path linear
    for &(idx, db) in &[
        (9u32, -12.0f32),
        (9, 6.0),
        (10, -12.0),
        (10, 6.0),
        (10, 12.0),
    ] {
        let g = gain_for(&[(0, 0.0), (idx, db)], 440.0, -20.0);
        assert!(
            (g - db).abs() < 0.05,
            "param {idx} = {db}: moved {g:+.3} dB"
        );
    }
    let both = gain_for(&[(0, 0.0), (9, -6.0), (10, 6.0)], 440.0, -20.0);
    assert!(both.abs() < 0.05, "in +6 and out -6 moved {both:+.3} dB");
    // with the compressor working, input gain pushes the signal into the curve: a quiet signal
    // lifted by the compressor gets quieter in the output when the input is turned down
    let hot = gain_for(&[(0, 1.0), (10, 0.0)], 1000.0, -50.0);
    let cold = gain_for(&[(0, 1.0), (10, -12.0)], 1000.0, -50.0);
    assert!(
        hot - cold < 12.0 - 0.5,
        "the compressor should give back some of a -12 dB input cut"
    );
}

#[test]
fn silence_and_denormals_stay_silent_and_finite() {
    for params in [
        vec![],
        vec![(0u32, 1.0f32)],
        vec![(0, 1.0), (10, 12.0), (9, 12.0), (4, 12.0)],
    ] {
        // exact silence
        let (mut l, mut r) = (vec![0.0f32; 4800], vec![0.0f32; 4800]);
        run(&mut *with(&params), &mut l, &mut r, 128);
        assert!(
            l.iter().chain(&r).all(|&x| x == 0.0),
            "silence in, not silence out"
        );
        // denormal-level noise
        let x: Vec<f32> = (0..4800)
            .map(|i| if i % 2 == 0 { 1.0e-40 } else { -1.0e-41 })
            .collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        run(&mut *with(&params), &mut l, &mut r, 37);
        assert!(
            l.iter().chain(&r).all(|&y| y.is_finite() && y.abs() < 1e-9),
            "denormals grew"
        );
        // the signal stops: the tail decays to exact silence or below 1e-9
        let n = SR as usize;
        let mut l = sine(300.0, n, 0.5);
        l.extend(vec![0.0; n]);
        let mut r = l.clone();
        run(&mut *with(&params), &mut l, &mut r, 128);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        let end = &l[2 * n - 4800..];
        assert!(
            end.iter().all(|&y| y.abs() < 1e-9),
            "the tail does not decay to silence"
        );
    }
}

#[test]
fn bad_input_and_parameter_values_are_ignored() {
    let mut d = device();
    for i in 0..SPEC.len() as u32 {
        d.set_param(i, f32::NAN);
        d.set_param(i, f32::INFINITY);
        d.set_param(i, 1.0e9);
        d.set_param(i, -1.0e9);
    }
    d.set_param(99, 1.0);
    let (mut l, mut r) = (vec![f32::NAN; 256], vec![f32::INFINITY; 256]);
    run(&mut *d, &mut l, &mut r, 128);
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
}

#[test]
fn presets_are_complete_and_in_range() {
    assert!((5..=7).contains(&PRESETS.len()));
    for name in PRESETS {
        let p = preset(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(p.len(), SPEC.len(), "{name}");
        for (k, &(i, v)) in p.iter().enumerate() {
            assert_eq!(i as usize, k);
            let (lo, hi, _) = SPEC[k];
            assert!((lo..=hi).contains(&v), "{name}: {} = {v}", PARAM_IDS[k]);
        }
        assert!(
            preset(name).unwrap() != preset("Init").unwrap(),
            "{name} equals Init"
        );
        // each preset renders loudly enough to hear and finite
        let n = SR as usize / 2;
        let x = sine(220.0, n, 0.5);
        let (mut l, mut r) = (x.clone(), x);
        run(&mut *with(&p), &mut l, &mut r, 128);
        assert!(l.iter().all(|v| v.is_finite() && v.abs() < 4.0), "{name}");
        assert!(rms_db(&l) > -40.0, "{name} is nearly silent");
    }
    assert_eq!(preset("Init").unwrap().len(), SPEC.len());
    assert!(preset("nope").is_none());
    let mut sorted = PRESETS.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), PRESETS.len(), "duplicate preset names");
}

#[test]
fn process_never_allocates() {
    let mut d = device();
    let n = 128;
    let x = sine(440.0, n, 0.5);
    let (mut l, mut r) = (x.clone(), x);
    for k in 0..200 {
        // parameter changes of every kind (all indices, several values each, plus out-of-range
        // and non-finite values and an unknown index), including the crossover frequencies
        assert_no_alloc::assert_no_alloc(|| {
            for (i, &(lo, hi, _)) in SPEC.iter().enumerate() {
                let t = ((k * 7 + i * 13) % 20) as f32 / 19.0;
                d.set_param(i as u32, lo + (hi - lo) * t);
                d.set_param(i as u32, hi * 2.0);
                d.set_param(i as u32, f32::NAN);
                d.set_param(i as u32, lo);
                d.set_param(i as u32, hi);
            }
            d.set_param(SPEC.len() as u32, 1.0);
        });
        assert_no_alloc::assert_no_alloc(|| {
            d.process(&mut l, &mut r, None, &[]);
            d.meter(0);
        });
    }
    assert_no_alloc::assert_no_alloc(|| d.reset());
}

// ---------------------------------------------------------------------------------------------
// Sweeps: every parameter across its range at several block sizes. No NaN/inf, and no clicks.
//
// The detector is the one in `clicks.rs` (4th-order Butterworth high-pass at 5 kHz, 1 ms windows
// against the loudest neighbour 4..25 ms either side, floor 60 dB under the loudest 10 ms),
// restricted to what these tests need. A burst reads +41 dB for a hard step, about +3 dB for a
// 2 ms fade; the limit here is the same +6 dB.

const HP_HZ: f64 = 5000.0;
const WIN_MS: f32 = 1.0;
const HOP_MS: f32 = 0.25;
const NEAR_MS: f32 = 4.0;
const FAR_MS: f32 = 25.0;
const FLOOR_DB: f64 = -60.0;
const LIMIT_DB: f32 = 6.0;

fn highpass4(x: &[f32]) -> Vec<f64> {
    let mut y: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    for q in [0.541_196_100_146_197, 1.306_562_964_876_376_6] {
        let w0 = 2.0 * std::f64::consts::PI * HP_HZ / SR as f64;
        let (c, s) = (libm::cos(w0), libm::sin(w0));
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = ((1.0 + c) * 0.5 / a0, -(1.0 + c) / a0, (1.0 + c) * 0.5 / a0);
        let (a1, a2) = (-2.0 * c / a0, (1.0 - alpha) / a0);
        let (mut z1, mut z2) = (0.0f64, 0.0f64);
        for v in y.iter_mut() {
            let x0 = *v;
            let out = b0 * x0 + z1;
            z1 = b1 * x0 - a1 * out + z2;
            z2 = b2 * x0 - a2 * out;
            *v = out;
        }
    }
    y
}

fn reference_power(l: &[f32], r: &[f32]) -> f64 {
    let n = l.len();
    let w = ((0.01 * SR) as usize).max(1);
    let mut pre = vec![0.0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + 0.5 * ((l[i] as f64) * (l[i] as f64) + (r[i] as f64) * (r[i] as f64));
    }
    let mut best = 1.0e-30f64;
    let mut i = 0;
    while i + w <= n {
        best = best.max((pre[i + w] - pre[i]) / w as f64);
        i += w / 4;
    }
    best
}

/// Worst burst score in dB and where (ms, channel).
// (the context loops index windows by position, as in `clicks.rs`)
#[allow(clippy::needless_range_loop)]
fn worst_click(l: &[f32], r: &[f32]) -> (f32, f32, usize) {
    let n = l.len().min(r.len());
    let floor = (reference_power(l, r) * libm::pow(10.0, FLOOR_DB / 10.0)).max(1e-20);
    let hop = ((HOP_MS * 0.001 * SR).round() as usize).max(1);
    let win = ((WIN_MS * 0.001 * SR).round() as usize).max(2);
    let near = (NEAR_MS / HOP_MS).round() as usize;
    let far = (FAR_MS / HOP_MS).round() as usize;
    let mut worst = (f32::NEG_INFINITY, 0.0f32, 0usize);
    for (ch, x) in [&l[..n], &r[..n]].into_iter().enumerate() {
        let hp = highpass4(x);
        let mut pre = vec![0.0f64; n + 1];
        for i in 0..n {
            pre[i + 1] = pre[i] + hp[i] * hp[i];
        }
        let centres: Vec<usize> = (0..n).step_by(hop).collect();
        let w: Vec<f64> = centres
            .iter()
            .map(|&c| {
                let a = c.saturating_sub(win / 2);
                let b = (c + win / 2).min(n);
                (pre[b] - pre[a]) / win as f64
            })
            .collect();
        for j in 0..w.len() {
            let mut ctx = 0.0f64;
            for k in j.saturating_sub(far)..j.saturating_sub(near) {
                ctx = ctx.max(w[k]);
            }
            for k in (j + near).min(w.len())..(j + far + 1).min(w.len()) {
                ctx = ctx.max(w[k]);
            }
            let score = (10.0 * libm::log10((w[j] + 1e-30) / (ctx + floor))) as f32;
            if score > worst.0 {
                worst = (score, centres[j] as f32 * 1000.0 / SR, ch);
            }
        }
    }
    worst
}

/// A steady two-tone signal, 0.1 s of silence-free lead-in, then `secs` of a sweep of `index`
/// from its minimum to its maximum and back. The parameter is set before each block.
fn sweep_render(index: u32, block: usize, secs: f32) -> (Vec<f32>, Vec<f32>) {
    let lead = SR as usize / 10;
    let n = lead + (secs * SR) as usize;
    // two tones whose level swings 54 dB at 1.5 Hz (-60..-6 dBFS), so the compressor is working
    // through both of its curves while the parameter moves
    let mut l: Vec<f32> = sine(220.0, n, 1.0)
        .iter()
        .zip(sine(1800.0, n, 0.5))
        .enumerate()
        .map(|(i, (a, b))| {
            let lfo = libm::sin(2.0 * std::f64::consts::PI * 1.5 * i as f64 / SR as f64);
            let amp = libm::pow(10.0, (-33.0 + 27.0 * lfo) / 20.0) as f32;
            (a + b) * amp * 0.67
        })
        .collect();
    // a 30 ms fade-in: the onset from silence is the host's business, not the device's
    for (i, v) in l.iter_mut().enumerate().take(SR as usize * 30 / 1000) {
        let t = i as f32 / (SR * 0.03);
        *v *= 0.5 - 0.5 * libm::cosf(std::f32::consts::PI * t);
    }
    let mut r = l.clone();
    let (lo, hi, def) = SPEC[index as usize];
    let mut d = with(&[(index, def)]);
    // the compressor engages (depth 1) so every parameter has an audible effect, except when
    // the parameter under test is depth itself
    if index != 0 {
        d.set_param(0, 1.0);
    }
    let mut i = 0;
    while i < n {
        let e = (i + block).min(n);
        if i >= lead {
            let t = (i - lead) as f32 / (n - lead) as f32; // 0..1
            let tri = 1.0 - (2.0 * t - 1.0).abs(); // 0..1..0
            d.set_param(index, lo + (hi - lo) * tri);
        }
        d.process(&mut l[i..e], &mut r[i..e], None, &[]);
        i = e;
    }
    (l, r)
}

#[test]
fn every_parameter_sweeps_without_nan_or_clicks() {
    let mut report = Vec::new();
    for index in 0..SPEC.len() as u32 {
        for block in [1usize, 37, 128] {
            let (l, r) = sweep_render(index, block, 2.0);
            assert!(
                l.iter().chain(&r).all(|v| v.is_finite()),
                "{} at block {block}: NaN or inf",
                PARAM_IDS[index as usize]
            );
            let (score, at_ms, ch) = worst_click(&l, &r);
            if std::env::var("CLICKS_VERBOSE").is_ok() {
                println!(
                    "{} block {block}: worst {score:+.1} dB at {at_ms:.1} ms ch{ch}",
                    PARAM_IDS[index as usize]
                );
            }
            if score > LIMIT_DB {
                report.push(format!(
                    "{} at block {block}: {score:+.1} dB at {at_ms:.1} ms (ch {ch})",
                    PARAM_IDS[index as usize]
                ));
            }
        }
    }
    assert!(
        report.is_empty(),
        "clicks over +{LIMIT_DB} dB:\n{}",
        report.join("\n")
    );
}

/// The detector sees what it should: a hard step in this device's output reads far above the limit.
#[test]
fn the_click_detector_catches_a_step() {
    let (mut l, mut r) = sweep_render(0, 128, 0.5);
    let (clean, _, _) = worst_click(&l, &r);
    assert!(
        clean <= LIMIT_DB,
        "the steady render already clicks: {clean:+.1} dB"
    );
    let at = l.len() / 2;
    for i in at..l.len() {
        l[i] += 0.2;
        r[i] += 0.2;
    }
    let (score, _, _) = worst_click(&l, &r);
    assert!(score > 20.0, "a hard step read only {score:+.1} dB");
}
