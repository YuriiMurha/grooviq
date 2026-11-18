//! Behavioural tests for every built-in device.

use grooviq_dsp::{Device, NoteEvent, create, param_spec};

const IDS: [&str; 14] = [
    "synth",
    "eq",
    "comp",
    "limiter",
    "drive",
    "reverb",
    "delay",
    "ott",
    "utility",
    "drum_shaper",
    "saturation",
    "fattener",
    "bitcrusher",
    "lofi_colour",
];
const EFFECTS: [&str; 13] = [
    "eq",
    "comp",
    "limiter",
    "drive",
    "reverb",
    "delay",
    "ott",
    "utility",
    "drum_shaper",
    "saturation",
    "fattener",
    "bitcrusher",
    "lofi_colour",
];
const RATES: [f32; 5] = [22050.0, 44100.0, 48000.0, 96000.0, 192000.0];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn bipolar(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn sine(freq: f32, sr: f32, n: usize, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|i| amp * libm::sinf(2.0 * std::f32::consts::PI * freq * i as f32 / sr))
        .collect()
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

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

fn db(x: f32) -> f32 {
    20.0 * libm::log10f(x.max(1e-12))
}

fn note(offset: u32, key: u8, on: bool, slide: bool) -> NoteEvent {
    NoteEvent {
        offset,
        key,
        velocity: 1.0,
        on,
        slide,
        id: u64::from(key),
    }
}

#[test]
fn silence_in_silence_out_every_device_and_rate() {
    for &sr in &RATES {
        for id in IDS {
            let mut dev = create(id, sr).unwrap();
            let n = sr as usize; // one second
            let mut l = vec![0.0f32; n];
            let mut r = vec![0.0f32; n];
            run(dev.as_mut(), &mut l, &mut r, 128);
            let peak = l.iter().chain(&r).fold(0.0f32, |m, v| {
                assert!(v.is_finite(), "{id} @ {sr}: non-finite output");
                m.max(v.abs())
            });
            assert!(peak < 1e-6, "{id} @ {sr}: silence produced {peak}");
            for m in 0..8 {
                assert!(dev.meter(m).is_finite(), "{id}: meter {m} not finite");
            }
        }
    }
}

#[test]
fn eq_bell_plus_6db_at_centre() {
    for &sr in &[44100.0f32, 48000.0, 96000.0] {
        let mut eq = create("eq", sr).unwrap();
        // band 3: bell at 1 kHz, +6 dB, Q 1
        eq.set_param(3 * 6 + 1, 2.0);
        eq.set_param(3 * 6 + 2, 1000.0);
        eq.set_param(3 * 6 + 3, 6.0);
        eq.set_param(3 * 6 + 4, 1.0);
        let n = sr as usize;
        let input = sine(1000.0, sr, n, 0.25);
        let mut l = input.clone();
        let mut r = input.clone();
        run(eq.as_mut(), &mut l, &mut r, 128);
        let tail = n / 2;
        let gain = db(rms(&l[tail..]) / rms(&input[tail..]));
        assert!(
            (gain - 6.0).abs() <= 0.5,
            "bell gain at centre = {gain} dB @ {sr}"
        );
    }
}

#[test]
fn eq_spectrum_meter_reads_a_full_scale_sine() {
    let sr = 48000.0;
    let mut eq = create("eq", sr).unwrap();
    let n = sr as usize / 2;
    let mut l = sine(1000.0, sr, n, 1.0);
    let mut r = l.clone();
    run(eq.as_mut(), &mut l, &mut r, 128);
    let mut spec = [0.0f32; 128];
    assert_eq!(eq.meter_array(0, &mut spec), 128);
    let (bin, peak) =
        spec.iter().enumerate().fold(
            (0, -200.0f32),
            |a, (i, &v)| if v > a.1 { (i, v) } else { a },
        );
    let f = 20.0 * libm::powf(1000.0, bin as f32 / 127.0);
    assert!(libm::log2f(f / 1000.0).abs() < 0.1, "peak bin at {f} Hz");
    assert!((peak).abs() < 2.0, "full-scale sine reads {peak} dB");
    assert!(spec[10] < -60.0, "far bins should be quiet: {}", spec[10]);
}

#[test]
fn compressor_static_curve_matches_threshold_and_ratio() {
    let sr = 48000.0;
    // (input dBFS peak, expected output dBFS peak) for threshold -18, ratio 4, knee 6
    for (in_db, ratio, expect) in [
        (0.0f32, 4.0f32, -13.5f32),
        (-6.0, 4.0, -15.0),
        (-30.0, 4.0, -30.0),
        (-6.0, 10.0, -16.8),
    ] {
        let mut c = create("comp", sr).unwrap();
        c.set_param(1, ratio);
        let n = sr as usize;
        let amp = libm::powf(10.0, in_db / 20.0);
        let mut l = sine(1000.0, sr, n, amp);
        let mut r = l.clone();
        run(c.as_mut(), &mut l, &mut r, 128);
        let tail = &l[n - n / 4..];
        let out_db = db(tail.iter().fold(0.0f32, |m, v| m.max(v.abs())));
        assert!(
            (out_db - expect).abs() <= 1.0,
            "in {in_db} dB ratio {ratio}: out {out_db} dB, expected {expect}"
        );
        assert!(c.meter(2) <= 0.0);
    }
}

#[test]
fn compressor_external_sidechain_ducks() {
    let sr = 48000.0;
    let mut c = create("comp", sr).unwrap();
    c.set_param(7, 1.0);
    c.set_param(0, -30.0);
    let n = 24000;
    let mut l = sine(500.0, sr, n, 0.1);
    let mut r = l.clone();
    let key = sine(60.0, sr, n, 1.0);
    let mut i = 0;
    while i < n {
        let e = (i + 128).min(n);
        c.process(
            &mut l[i..e],
            &mut r[i..e],
            Some((&key[i..e], &key[i..e])),
            &[],
        );
        i = e;
    }
    // the quiet input (-20 dB) is far below threshold on its own; the loud key must duck it
    let out = db(l[n - 4800..].iter().fold(0.0f32, |m, v| m.max(v.abs())));
    assert!(out < -35.0, "sidechain did not duck: {out} dB");
}

#[test]
fn limiter_output_never_exceeds_ceiling() {
    for &sr in &[44100.0f32, 48000.0, 192000.0] {
        let mut lim = create("limiter", sr).unwrap();
        lim.set_param(0, 12.0); // +12 dB input gain
        lim.set_param(1, -1.0); // ceiling
        lim.set_param(2, 20.0);
        let ceil = libm::powf(10.0, -1.0 / 20.0);
        let mut rng = Rng(7);
        let n = sr as usize * 2;
        let mut l: Vec<f32> = (0..n)
            .map(|i| {
                let burst = if (i / 3000) % 5 == 0 { 4.0 } else { 0.5 };
                let imp = if i % 9973 == 0 { 8.0 } else { 0.0 };
                rng.bipolar() * burst + imp
            })
            .collect();
        let mut r: Vec<f32> = l.iter().map(|v| -0.7 * v).collect();
        let mut i = 0;
        while i < n {
            let e = (i + 1 + rng.below(128)).min(n);
            lim.process(&mut l[i..e], &mut r[i..e], None, &[]);
            i = e;
        }
        let peak = l.iter().chain(&r).fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            peak <= ceil + 1e-6,
            "limiter peak {peak} > ceiling {ceil} @ {sr}"
        );
        assert!(
            lim.meter(2) < -1.0,
            "gain reduction meter should show reduction"
        );
    }
}

#[test]
fn delay_echo_lands_on_the_beat_at_140_bpm() {
    let sr = 48000.0;
    // (division index, beats)
    for (div, beats) in [(4usize, 0.5f32), (2, 0.375), (3, 1.0 / 3.0), (7, 1.0)] {
        for pingpong in [0.0, 1.0] {
            let mut d = create("delay", sr).unwrap();
            d.set_tempo(140.0);
            d.set_param(1, div as f32);
            d.set_param(3, 0.0); // no feedback
            d.set_param(4, pingpong);
            d.set_param(5, 20.0);
            d.set_param(6, 20000.0);
            d.set_param(7, 1.0); // wet only
            let n = sr as usize;
            let mut l = vec![0.0f32; n];
            let mut r = vec![0.0f32; n];
            l[0] = 1.0;
            r[0] = 1.0;
            run(d.as_mut(), &mut l, &mut r, 128);
            let expect = beats * 60.0 / 140.0 * sr;
            let (pos, _) = l.iter().enumerate().fold((0, 0.0f32), |a, (i, &v)| {
                if v.abs() > a.1 { (i, v.abs()) } else { a }
            });
            assert!(
                (pos as f32 - expect).abs() <= 2.0,
                "div {div}: echo at {pos}, expected {expect}"
            );
            assert!((d.meter(2) - beats * 60_000.0 / 140.0).abs() < 0.01);
        }
    }
}

#[test]
fn delay_free_time_and_feedback_repeats() {
    let sr = 48000.0;
    let mut d = create("delay", sr).unwrap();
    d.set_param(0, 0.0);
    d.set_param(2, 100.0);
    d.set_param(3, 0.5);
    d.set_param(4, 0.0);
    d.set_param(5, 20.0);
    d.set_param(6, 20000.0);
    d.set_param(7, 1.0);
    let n = 24000;
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    l[0] = 1.0;
    r[0] = 1.0;
    run(d.as_mut(), &mut l, &mut r, 128);
    let near = |c: usize| l[c - 3..c + 4].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let (e1, e2, e3) = (near(4800), near(9600), near(14400));
    assert!(
        e1 > 0.5 && (e2 / e1 - 0.5).abs() < 0.08 && (e3 / e2 - 0.5).abs() < 0.08,
        "{e1} {e2} {e3}"
    );
}

/// RT60 from the Schroeder backward integral (fit between -5 and -35 dB).
fn rt60(x: &[f32], sr: f32) -> f32 {
    let mut energy: Vec<f64> = x.iter().map(|v| (*v as f64) * (*v as f64)).collect();
    for i in (0..energy.len() - 1).rev() {
        energy[i] += energy[i + 1];
    }
    let e0 = energy[0];
    let level = |i: usize| 10.0 * libm::log10(energy[i] / e0);
    let t5 = (0..energy.len()).find(|&i| level(i) < -5.0).unwrap();
    let t35 = (0..energy.len()).find(|&i| level(i) < -35.0).unwrap();
    (t35 - t5) as f32 / sr * 2.0
}

#[test]
fn reverb_decays_with_the_requested_rt60() {
    let sr = 48000.0;
    for decay in [0.6f32, 1.5, 3.0] {
        let mut rv = create("reverb", sr).unwrap();
        rv.set_param(1, decay);
        rv.set_param(2, 0.0);
        rv.set_param(3, 0.0);
        rv.set_param(5, 20.0);
        rv.set_param(6, 20000.0);
        rv.set_param(7, 1.0);
        let n = (sr * (decay * 2.5 + 0.5)) as usize;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        l[0] = 1.0;
        r[0] = 1.0;
        run(rv.as_mut(), &mut l, &mut r, 128);
        let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
        let t = rt60(&mono, sr);
        assert!(
            (t / decay - 1.0).abs() < 0.2,
            "decay {decay}s measured RT60 {t}s"
        );
        let early = rms(&mono[2400..12000]);
        let late = rms(&mono[n - 4800..]);
        assert!(
            db(late / early) < -40.0,
            "tail did not decay: {} dB",
            db(late / early)
        );
    }
}

/// Frequency of the strongest component between lo and hi (Hann-windowed DFT, fine search).
fn peak_freq(x: &[f32], sr: f32, lo: f32, hi: f32) -> f32 {
    let n = x.len();
    let w: Vec<f64> = (0..n)
        .map(|i| {
            (0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / n as f64)) * x[i] as f64
        })
        .collect();
    let mag = |f: f64| {
        let wk = 2.0 * std::f64::consts::PI * f / sr as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in w.iter().enumerate() {
            let a = wk * i as f64;
            re += v * libm::cos(a);
            im -= v * libm::sin(a);
        }
        re * re + im * im
    };
    let mut best = lo as f64;
    let mut f = lo as f64;
    while f <= hi as f64 {
        if mag(f) > mag(best) {
            best = f;
        }
        f += 1.0;
    }
    let mut fine = best;
    let mut f = best - 1.0;
    while f <= best + 1.0 {
        if mag(f) > mag(fine) {
            fine = f;
        }
        f += 0.02;
    }
    fine as f32
}

#[test]
fn synth_default_patch_plays_a440_at_key_69() {
    for &sr in &[44100.0f32, 48000.0] {
        let mut s = create("synth", sr).unwrap();
        let n = (sr * 1.5) as usize;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        let mut i = 0;
        while i < n {
            let e = (i + 128).min(n);
            let ev = if i == 0 {
                vec![note(0, 69, true, false)]
            } else {
                vec![]
            };
            s.process(&mut l[i..e], &mut r[i..e], None, &ev);
            i = e;
        }
        let seg = &l[(sr * 0.5) as usize..];
        let f = peak_freq(seg, sr, 400.0, 480.0);
        assert!((f - 440.0).abs() < 1.0, "pitch {f} Hz @ {sr}");
        // osc 2 is a square one octave down (+7 cents)
        let f2 = peak_freq(seg, sr, 200.0, 240.0);
        let expect2 = 220.0 * libm::powf(2.0, 7.0 / 1200.0);
        assert!((f2 - expect2).abs() < 1.0, "osc2 {f2} Hz");
        assert!(s.meter(0) > 0.05 && s.meter(2) == 1.0);
    }
}

fn render_synth(
    s: &mut dyn Device,
    n: usize,
    events: &[(usize, NoteEvent)],
) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    let mut i = 0;
    while i < n {
        let e = (i + 128).min(n);
        let ev: Vec<NoteEvent> = events
            .iter()
            .filter(|(t, _)| *t >= i && *t < e)
            .map(|(t, ev)| NoteEvent {
                offset: (*t - i) as u32,
                ..*ev
            })
            .collect();
        s.process(&mut l[i..e], &mut r[i..e], None, &ev);
        i = e;
    }
    (l, r)
}

#[test]
fn synth_presets_load_and_play_cleanly() {
    let sr = 48000.0;
    for name in grooviq_dsp::synth::PRESETS {
        let p = grooviq_dsp::synth::preset(name).expect(name);
        assert_eq!(p.len(), 40);
        let mut s = create("synth", sr).unwrap();
        for (i, v) in p {
            s.set_param(i, v);
        }
        let ev = [
            (0, note(0, 48, true, false)),
            (6000, note(0, 55, true, false)),
            (9000, note(0, 48, false, false)),
            (20000, note(0, 60, true, true)),
            (30000, note(0, 55, false, false)),
            (30000, note(0, 60, false, false)),
        ];
        let (l, r) = render_synth(s.as_mut(), 96000, &ev);
        let peak = l.iter().chain(&r).fold(0.0f32, |m, v| {
            assert!(v.is_finite());
            m.max(v.abs())
        });
        assert!(peak > 0.05 && peak < 2.0, "{name}: peak {peak}");
        assert!(grooviq_dsp::synth::preset("nope").is_none());
    }
}

#[test]
fn synth_808_glide_slides_pitch_without_retriggering() {
    let sr = 48000.0;
    let mut s = create("synth", sr).unwrap();
    for (i, v) in grooviq_dsp::synth::preset("808 Glide").unwrap() {
        s.set_param(i, v);
    }
    // key 48 with osc octave -1 -> 65.4 Hz; overlapping key 55 slides to 98 Hz
    let ev = [
        (0, note(0, 48, true, false)),
        (24000, note(0, 55, true, false)),
        (26000, note(0, 48, false, false)),
    ];
    let (l, _) = render_synth(s.as_mut(), 60000, &ev);
    let f1 = peak_freq(&l[6000..22000], sr, 50.0, 80.0);
    let f2 = peak_freq(&l[36000..60000], sr, 85.0, 110.0);
    assert!((f1 - 65.41).abs() < 1.0, "808 first note {f1}");
    assert!((f2 - 98.0).abs() < 1.0, "808 slid note {f2}");
    // legato: no new attack, the level keeps decaying across the slide
    let before = rms(&l[22000..24000]);
    let after = rms(&l[25000..27000]);
    assert!(after < before * 1.1, "retriggered: {before} -> {after}");
}

#[test]
fn synth_voice_stealing_stays_click_free_and_bounded() {
    let sr = 48000.0;
    // Pure sines make clicks measurable: the 2nd difference of a <= 440 Hz sine sum is tiny,
    // while cutting a voice (or starting one) mid-cycle produces a large spike.
    let mut s = create("synth", sr).unwrap();
    for (p, v) in [
        (0u32, 0.0f32),
        (12, 0.0),
        (19, 0.0),
        (20, 20000.0),
        (23, 0.0),
        (25, 0.5),
        (27, 1.0),
        (39, -12.0),
    ] {
        s.set_param(p, v);
    }
    // 24 held notes: voices 17..24 must steal held voices; then release and retrigger some
    let mut ev: Vec<(usize, NoteEvent)> = (0..24)
        .map(|k| (1000 + k * 397, note(0, 45 + k as u8, true, false)))
        .collect();
    ev.push((14000, note(0, 50, false, false)));
    ev.push((14500, note(0, 50, true, false)));
    let (l, r) = render_synth(s.as_mut(), 24000, &ev);
    assert_eq!(s.meter(2), 16.0);
    let mut worst = 0.0f32;
    for i in 2..l.len() {
        assert!(l[i].is_finite() && r[i].is_finite());
        worst = worst.max((l[i] - 2.0 * l[i - 1] + l[i - 2]).abs());
    }
    let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(
        worst < 0.02 * peak.max(0.1),
        "click: 2nd difference {worst} (peak {peak})"
    );
    // the same without stealing protection would be a step of the voice amplitude
    let mut scope = [0.0f32; 256];
    assert_eq!(s.meter_array(6, &mut scope), 256);
}

#[test]
fn synth_unison_supersaw_is_bounded() {
    let sr = 48000.0;
    let mut s = create("synth", sr).unwrap();
    s.set_param(5, 7.0);
    s.set_param(13, 7.0);
    let ev: Vec<(usize, NoteEvent)> = (0..40)
        .map(|k| (k * 300, note(0, 40 + (k % 30) as u8, true, false)))
        .collect();
    let (l, r) = render_synth(s.as_mut(), 24000, &ev);
    assert!(s.meter(2) <= 16.0);
    let peak = l.iter().chain(&r).fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak.is_finite() && peak < 4.0, "peak {peak}");
}

#[test]
fn drive_dry_wet_is_latency_aligned() {
    let sr = 48000.0;
    for os in [0.0, 1.0, 2.0] {
        let mut d = create("drive", sr).unwrap();
        d.set_param(0, 1.0); // soft clip
        d.set_param(1, 0.0); // no extra drive
        d.set_param(3, 0.5);
        d.set_param(5, os);
        let n = 8000;
        let input = sine(440.0, sr, n, 0.01); // tiny level: tanh is ~linear
        let mut l = input.clone();
        let mut r = input.clone();
        run(d.as_mut(), &mut l, &mut r, 64);
        let lat = match os as u8 {
            0 => 0,
            1 => 43,
            _ => 49,
        };
        let err = (2000..n)
            .map(|i| (l[i] - input[i - lat]).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 2e-4, "os {os}: misaligned dry/wet, err {err}");
    }
}

#[test]
fn drive_oversampling_reduces_aliasing() {
    let sr = 48000.0;
    let alias_db = |os: f32| {
        let mut d = create("drive", sr).unwrap();
        d.set_param(0, 0.0); // hard clip
        d.set_param(1, 24.0);
        d.set_param(5, os);
        let n = 48000;
        let mut l = sine(4100.0, sr, n, 0.5);
        let mut r = l.clone();
        run(d.as_mut(), &mut l, &mut r, 128);
        let seg = &l[8000..8000 + 16384];
        // 9th harmonic 36.9 kHz aliases to 11.1 kHz; compare against the fundamental
        let fund = peak_freq_mag(seg, sr, 4100.0);
        let alias = peak_freq_mag(seg, sr, 48000.0 - 9.0 * 4100.0);
        db(alias / fund)
    };
    let a1 = alias_db(0.0);
    let a4 = alias_db(2.0);
    assert!(
        a4 < a1 - 20.0 && a4 < -60.0,
        "1x alias {a1} dB, 4x alias {a4} dB"
    );
}

fn peak_freq_mag(x: &[f32], sr: f32, f: f32) -> f32 {
    let n = x.len();
    let wk = 2.0 * std::f64::consts::PI * f as f64 / sr as f64;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / n as f64);
        re += w * *v as f64 * libm::cos(wk * i as f64);
        im -= w * *v as f64 * libm::sin(wk * i as f64);
    }
    ((re * re + im * im).sqrt() / n as f64 * 4.0) as f32
}

#[test]
fn ott_lifts_quiet_and_tames_loud() {
    let sr = 48000.0;
    let level = |amp: f32, depth: f32| {
        let mut o = create("ott", sr).unwrap();
        o.set_param(0, depth);
        let n = 48000;
        let mut l = sine(1000.0, sr, n, amp);
        let mut r = l.clone();
        run(o.as_mut(), &mut l, &mut r, 128);
        db(rms(&l[n / 2..]) / (amp / 2f32.sqrt()))
    };
    assert!(level(0.003, 1.0) > 6.0, "quiet signal not lifted");
    assert!(level(0.9, 1.0) < -6.0, "loud signal not compressed");
    assert!(level(0.5, 0.0).abs() < 0.2, "depth 0 must be transparent");
}

#[test]
fn utility_correlation_and_width() {
    let sr = 48000.0;
    let n = 24000;
    let mut u = create("utility", sr).unwrap();
    let mut l = sine(300.0, sr, n, 0.5);
    let mut r = l.clone();
    run(u.as_mut(), &mut l, &mut r, 128);
    assert!(u.meter(2) > 0.99);
    let mut u = create("utility", sr).unwrap();
    let mut l = sine(300.0, sr, n, 0.5);
    let mut r: Vec<f32> = l.iter().map(|v| -v).collect();
    run(u.as_mut(), &mut l, &mut r, 128);
    assert!(u.meter(2) < -0.99);
    // width 0 collapses an anti-phase signal to silence
    let mut u = create("utility", sr).unwrap();
    u.set_param(2, 0.0);
    let mut l = sine(300.0, sr, n, 0.5);
    let mut r: Vec<f32> = l.iter().map(|v| -v).collect();
    run(u.as_mut(), &mut l, &mut r, 128);
    assert!(rms(&l[1000..]) < 1e-4);
    // bass mono: a 50 Hz side signal disappears, a 3 kHz one survives
    for (f, keep) in [(50.0f32, false), (3000.0, true)] {
        let mut u = create("utility", sr).unwrap();
        u.set_param(3, 200.0);
        let mut l = sine(f, sr, n, 0.5);
        let mut r: Vec<f32> = l.iter().map(|v| -v).collect();
        run(u.as_mut(), &mut l, &mut r, 128);
        let side = rms(&l[n / 2..]
            .iter()
            .zip(&r[n / 2..])
            .map(|(a, b)| 0.5 * (a - b))
            .collect::<Vec<_>>());
        if keep {
            assert!(side > 0.3, "3 kHz side removed: {side}");
        } else {
            assert!(side < 0.02, "50 Hz side kept: {side}");
        }
    }
}

fn random_signal(rng: &mut Rng, n: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(n);
    let mut mode = 0;
    for i in 0..n {
        if i % 4096 == 0 {
            mode = rng.below(6);
        }
        out.push(match mode {
            0 => 0.0,
            1 => rng.bipolar() * 0.3,
            2 => rng.bipolar() * 6.0,
            3 => libm::sinf(i as f32 * 0.05) * 0.9,
            4 => {
                if i % 1000 == 0 {
                    10.0
                } else {
                    0.0
                }
            }
            _ => 1.0,
        });
    }
    out
}

#[test]
fn random_parameter_sweeps_never_produce_nan() {
    for (k, id) in IDS.iter().enumerate() {
        for &sr in &[22050.0f32, 48000.0, 192000.0] {
            let mut rng = Rng(0xABCD_1234 + k as u64 * 977 + sr as u64);
            let spec = param_spec(id).unwrap();
            let mut dev = create(id, sr).unwrap();
            let n = 60_000;
            let mut l = random_signal(&mut rng, n);
            let mut r = random_signal(&mut rng, n);
            let mut i = 0;
            while i < n {
                for _ in 0..rng.below(4) {
                    let p = rng.below(spec.len());
                    let (lo, hi, _) = spec[p];
                    let v = match rng.below(10) {
                        0 => lo,
                        1 => hi,
                        2 => hi * 10.0 + 1.0, // out of range: must clamp
                        _ => lo + (hi - lo) * rng.unit(),
                    };
                    dev.set_param(p as u32, v);
                }
                dev.set_param(rng.below(64) as u32 + 100, 1.0); // unknown index: ignored
                if rng.below(20) == 0 {
                    dev.set_tempo(40.0 + rng.unit() * 260.0);
                }
                let e = (i + 1 + rng.below(128)).min(n);
                let len = e - i;
                let mut ev = Vec::new();
                if *id == "synth" {
                    for _ in 0..rng.below(3) {
                        ev.push(NoteEvent {
                            offset: rng.below(len) as u32,
                            key: rng.below(128) as u8,
                            velocity: rng.unit(),
                            on: rng.below(2) == 0,
                            slide: rng.below(4) == 0,
                            id: u64::from(rng.below(128) as u8),
                        });
                    }
                    ev.sort_by_key(|e| e.offset);
                }
                let sc = (l[i..e].to_vec(), r[i..e].to_vec());
                dev.process(&mut l[i..e], &mut r[i..e], Some((&sc.0, &sc.1)), &ev);
                for j in i..e {
                    assert!(
                        l[j].is_finite() && r[j].is_finite(),
                        "{id} @ {sr}: NaN/inf at {j}"
                    );
                    // eight stacked +24 dB EQ bells are a legitimate +192 dB, so no bound for eq
                    let bound = if *id == "eq" { f32::MAX } else { 1000.0 };
                    assert!(
                        l[j].abs() < bound && r[j].abs() < bound,
                        "{id} @ {sr}: blew up ({}) at {j}",
                        l[j]
                    );
                }
                i = e;
            }
            for m in 0..8 {
                assert!(dev.meter(m).is_finite(), "{id}: meter {m}");
            }
            let mut arr = [0.0f32; 256];
            for m in 0..8 {
                let c = dev.meter_array(m, &mut arr);
                assert!(arr[..c].iter().all(|v| v.is_finite()));
            }
            // stability: after neutral settings and silence every device must settle
            dev.reset();
            for (p, (_, _, d)) in spec.iter().enumerate() {
                dev.set_param(p as u32, *d);
            }
            let mut z = vec![0.0f32; sr as usize];
            let mut z2 = z.clone();
            run(dev.as_mut(), &mut z, &mut z2, 128);
            assert!(
                z.iter().chain(&z2).all(|v| v.abs() < 1e-6),
                "{id} @ {sr}: not silent after reset"
            );
        }
    }
}

/// A parameter set per effect that exercises its interesting paths.
fn interesting(id: &str) -> Vec<(u32, f32)> {
    match id {
        "lofi_colour" => vec![
            (0, 0.8),
            (1, 0.4),
            (2, 0.6),
            (3, 0.3),
            (4, 0.7),
            (5, 0.5),
            (6, 0.4),
        ],
        "saturation" => vec![
            (0, 1.),
            (1, 18.),
            (2, 0.5),
            (3, 6.),
            (4, 100.),
            (5, 12000.),
            (7, 0.7),
        ],
        "bitcrusher" => vec![(0, 5.7), (1, 23.3), (2, 0.6), (3, 0.4), (4, 1.0), (5, 0.8)],
        "eq" => vec![
            (2 * 6 + 3, 9.0),
            (3 * 6 + 5, 1.0),
            (3 * 6 + 3, -6.0),
            (4 * 6 + 5, 2.0),
            (4 * 6 + 3, 4.0),
            (5 * 6, 1.0),
            (7 * 6, 1.0),
            (48, -3.0),
        ],
        "comp" => vec![(0, -30.0), (2, 1.0), (3, 50.0), (5, 6.0), (6, 0.7)],
        "fattener" => vec![(0, 0.8), (1, 0.5)],
        "limiter" => vec![(0, 12.0), (1, -2.0), (2, 30.0)],
        "drive" => vec![(0, 3.0), (1, 18.0), (2, 0.4), (3, 0.8), (5, 2.0)],
        "reverb" => vec![(0, 0.8), (1, 3.0), (2, 35.0), (3, 0.3), (4, 0.7), (7, 0.5)],
        "delay" => vec![(1, 2.0), (3, 0.6), (4, 1.0), (7, 0.5)],
        "ott" => vec![(0, 0.8), (1, 0.3), (4, 3.0), (6, -2.0)],
        "drum_shaper" => vec![
            (0, 9.0),
            (1, -6.0),
            (2, 0.0),
            (3, 0.0),
            (4, 0.2),
            (5, 0.8),
            (6, -2.0),
        ],
        "utility" => vec![(0, -3.0), (1, 0.3), (2, 1.5), (3, 120.0)],
        _ => vec![],
    }
}

/// Changes applied at sample 4736 (a multiple of both 128 and 37) to test smoothing paths.
fn changes(id: &str) -> Vec<(u32, f32)> {
    match id {
        "lofi_colour" => vec![
            (0, 0.3),
            (1, 0.8),
            (2, 0.1),
            (3, 0.9),
            (4, 0.2),
            (5, 0.7),
            (6, 0.8),
            (7, 2.0),
        ],
        "saturation" => vec![
            (0, 3.),
            (1, 8.),
            (2, -0.5),
            (3, 0.),
            (4, 300.),
            (5, 6000.),
            (6, 0.),
            (7, 0.3),
            (8, -3.),
        ],
        "bitcrusher" => vec![(0, 2.5), (1, 2.5), (2, 1.0), (3, 0.2), (4, 0.7), (5, 0.3)],
        "eq" => vec![
            (2 * 6 + 2, 2000.0),
            (2 * 6 + 3, -12.0),
            (5 * 6, 0.0),
            (48, 2.0),
        ],
        "comp" => vec![(0, -12.0), (5, 0.0), (6, 1.0)],
        "fattener" => vec![(0, 0.2), (1, -0.9)],
        "limiter" => vec![(0, 3.0)],
        "drive" => vec![(1, 6.0), (2, -0.5), (3, 0.3)],
        "reverb" => vec![(0, 0.2), (2, 90.0), (5, 400.0), (7, 0.9)],
        "delay" => vec![(1, 5.0), (3, 0.2), (6, 3000.0)],
        "ott" => vec![(7, 300.0), (8, 5000.0), (0, 0.3)],
        "drum_shaper" => vec![
            (0, -9.0),
            (1, 8.0),
            (2, 2.0),
            (3, 2.0),
            (4, 0.8),
            (5, 0.3),
            (6, 3.0),
        ],
        "utility" => vec![(1, -0.6), (3, 0.0), (4, 1.0)],
        _ => vec![],
    }
}

#[test]
fn effects_are_block_size_invariant() {
    let sr = 48000.0;
    let n = 37 * 128 * 12; // 1.18 s
    let switch_at = 37 * 128;
    let mut rng = Rng(99);
    let input_l: Vec<f32> = (0..n)
        .map(|i| {
            0.4 * libm::sinf(i as f32 * 0.031)
                + 0.2 * rng.bipolar()
                + if i % 7001 == 0 { 0.9 } else { 0.0 }
        })
        .collect();
    let input_r: Vec<f32> = (0..n)
        .map(|i| 0.4 * libm::cosf(i as f32 * 0.017) + 0.2 * rng.bipolar())
        .collect();
    for id in EFFECTS {
        let mut outs = Vec::new();
        for block in [128usize, 37] {
            let mut dev = create(id, sr).unwrap();
            for (p, v) in interesting(id) {
                dev.set_param(p, v);
            }
            let mut l = input_l.clone();
            let mut r = input_r.clone();
            let mut i = 0;
            while i < n {
                if i == switch_at {
                    for (p, v) in changes(id) {
                        dev.set_param(p, v);
                    }
                }
                let e = (i + block).min(n);
                dev.process(&mut l[i..e], &mut r[i..e], None, &[]);
                i = e;
            }
            outs.push((l, r));
        }
        let diff = (0..n)
            .map(|i| {
                (outs[0].0[i] - outs[1].0[i])
                    .abs()
                    .max((outs[0].1[i] - outs[1].1[i]).abs())
            })
            .fold(0.0f32, f32::max);
        assert!(diff <= 1e-4, "{id}: block 128 vs 37 differ by {diff}");
    }
}

#[test]
fn synth_param_ids_match_the_spec_table() {
    use grooviq_dsp::synth::{PARAM_IDS, SPEC};
    assert_eq!(PARAM_IDS.len(), 40);
    assert_eq!(PARAM_IDS.len(), SPEC.len());
    for (i, id) in PARAM_IDS.iter().enumerate() {
        assert!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !id.starts_with('_')
                && !id.ends_with('_'),
            "bad id {id:?}"
        );
        assert!(!PARAM_IDS[..i].contains(id), "duplicate id {id}");
    }
    assert_eq!(PARAM_IDS[20], "filter_cutoff");
    assert_eq!(PARAM_IDS[39], "master");
}
