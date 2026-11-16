//! Behavioural tests for the sampler. Every render is offline and every sound is made here.

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use assert_no_alloc::assert_no_alloc;
use grooviq_dsp::sampler::{PARAM_IDS, SPEC, SampleBuf, Sampler, VOICES};
use grooviq_dsp::{Device, NoteEvent, create, param_spec};
use std::f32::consts::SQRT_2;
use std::sync::Arc;

const SR: f32 = 48_000.0;
const BLOCK: usize = 64;

// ---------------------------------------------------------------------------------------------
// Helpers

fn p(name: &str) -> u32 {
    PARAM_IDS
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no parameter {name}")) as u32
}

fn buf(rate: f32, l: Vec<f32>) -> Arc<SampleBuf> {
    Arc::new(SampleBuf {
        rate,
        r: l.clone(),
        l,
    })
}

/// One full-scale sample, then zeros.
fn click(len: usize) -> Vec<f32> {
    let mut x = vec![0.0; len];
    x[0] = 1.0;
    x
}

fn sine(rate: f32, hz: f64, len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| libm::sin(2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64) as f32)
        .collect()
}

/// Deterministic non-zero noise in (-1, 1) without exact zeros.
fn noise(len: usize) -> Vec<f32> {
    let mut s = 0x1234_5678u32;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            ((s >> 8) as f32 / (1u32 << 24) as f32) * 1.8 - 0.9 + 1e-3
        })
        .collect()
}

fn on(offset: u32, key: u8, velocity: f32) -> NoteEvent {
    NoteEvent {
        offset,
        key,
        velocity,
        on: true,
        slide: false,
    }
}

fn off(offset: u32, key: u8) -> NoteEvent {
    NoteEvent {
        offset,
        key,
        velocity: 0.0,
        on: false,
        slide: false,
    }
}

/// A sampler at `rate` with `sample` loaded and declick off unless a test turns it on.
fn sampler(rate: f32, sample: Arc<SampleBuf>) -> Sampler {
    let mut s = Sampler::new(rate);
    s.set_param(p("declick"), 0.0);
    assert!(s.set_sample(Some(sample)).is_none());
    s
}

/// Renders `frames` frames in BLOCK-frame blocks. `events` carry absolute frame offsets.
fn render(s: &mut Sampler, frames: usize, events: &[NoteEvent]) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    let mut start = 0;
    while start < frames {
        let end = (start + BLOCK).min(frames);
        let block: Vec<NoteEvent> = events
            .iter()
            .filter(|e| (start..end).contains(&(e.offset as usize)))
            .map(|e| NoteEvent {
                offset: e.offset - start as u32,
                ..*e
            })
            .collect();
        s.process(&mut l[start..end], &mut r[start..end], None, &block);
        start = end;
    }
    (l, r)
}

/// Frequency from the mean period between rising zero crossings, located by linear
/// interpolation (sub-sample accurate).
fn pitch(x: &[f32], rate: f32) -> f64 {
    let mut crossings = Vec::new();
    for i in 1..x.len() {
        let (a, b) = (x[i - 1] as f64, x[i] as f64);
        if a < 0.0 && b >= 0.0 {
            crossings.push((i - 1) as f64 + a / (a - b));
        }
    }
    assert!(crossings.len() > 10, "too few zero crossings to measure");
    let periods = (crossings.len() - 1) as f64;
    rate as f64 * periods / (crossings[crossings.len() - 1] - crossings[0])
}

fn cents(measured: f64, expected: f64) -> f64 {
    1200.0 * libm::log2(measured / expected)
}

/// Index one past the last non-zero frame.
fn played_len(x: &[f32]) -> usize {
    x.iter().rposition(|v| *v != 0.0).map_or(0, |i| i + 1)
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

// ---------------------------------------------------------------------------------------------
// Tests

#[test]
fn the_parameter_table_matches_the_contract() {
    let expected: [(&str, f32, f32, f32); 29] = [
        ("root", 0.0, 127.0, 60.0),
        ("tune", -24.0, 24.0, 0.0),
        ("fine", -100.0, 100.0, 0.0),
        ("start", 0.0, 1.0, 0.0),
        ("end", 0.0, 1.0, 1.0),
        ("reverse", 0.0, 1.0, 0.0),
        ("pitch_track", 0.0, 1.0, 1.0),
        ("one_shot", 0.0, 1.0, 1.0),
        ("attack", 0.0, 5000.0, 0.0),
        ("decay", 1.0, 10000.0, 500.0),
        ("sustain", 0.0, 1.0, 1.0),
        ("release", 1.0, 10000.0, 40.0),
        ("gain", -60.0, 12.0, 0.0),
        ("pan", -1.0, 1.0, 0.0),
        ("declick", 0.0, 1.0, 1.0),
        ("loop", 0.0, 1.0, 0.0),
        ("loop_start", 0.0, 1.0, 0.0),
        ("filter_type", 0.0, 3.0, 0.0),
        ("cutoff", 20.0, 20000.0, 20000.0),
        ("res", 0.0, 1.0, 0.0),
        ("filter_env", -1.0, 1.0, 0.0),
        ("flt_attack", 0.0, 5000.0, 0.0),
        ("flt_decay", 1.0, 10000.0, 300.0),
        ("flt_sustain", 0.0, 1.0, 0.0),
        ("flt_release", 1.0, 10000.0, 40.0),
        ("vel_amp", 0.0, 1.0, 1.0),
        ("vel_filter", 0.0, 1.0, 0.0),
        ("voice_mode", 0.0, 2.0, 0.0),
        ("glide", 0.0, 1000.0, 0.0),
    ];
    assert_eq!(PARAM_IDS.len(), expected.len());
    assert_eq!(SPEC.len(), expected.len());
    for (i, (id, lo, hi, def)) in expected.into_iter().enumerate() {
        assert_eq!(PARAM_IDS[i], id);
        assert_eq!(SPEC[i], (lo, hi, def), "{id}");
    }
    assert_eq!(VOICES, 16);
    assert_eq!(param_spec("sampler"), Some(&SPEC[..]));
    assert!(create("sampler", SR).is_some());
}

#[test]
fn plays_the_sample_unchanged_at_the_root_key() {
    for x in [click(500), noise(3000)] {
        let mut s = sampler(SR, buf(SR, x.clone()));
        let (l, r) = render(&mut s, x.len() + 300, &[on(0, 60, 1.0)]);
        assert_eq!(&l[..x.len()], &x[..], "left equals the buffer");
        assert_eq!(l, r, "both channels equal");
        assert!(
            l[x.len()..].iter().all(|v| *v == 0.0),
            "silent after the end"
        );
        assert_eq!(s.active_voices(), 0);
    }
}

#[test]
fn pitch_is_accurate_within_one_cent() {
    let x = sine(SR, 440.0, SR as usize);
    for key in [57u8, 62, 69, 76, 81] {
        let mut s = sampler(SR, buf(SR, x.clone()));
        s.set_param(p("root"), 69.0);
        s.set_param(p("tune"), 7.0);
        s.set_param(p("fine"), 50.0);
        let (l, _) = render(&mut s, 12_000, &[on(0, key, 1.0)]);
        let semis = key as f64 - 69.0 + 7.0 + 0.5;
        let expected = 440.0 * libm::pow(2.0, semis / 12.0);
        let c = cents(pitch(&l, SR), expected);
        assert!(c.abs() < 1.0, "key {key}: {c} cents off");
    }
    for key in [57u8, 62, 69, 76, 81] {
        let mut s = sampler(SR, buf(SR, x.clone()));
        s.set_param(p("root"), 69.0);
        s.set_param(p("pitch_track"), 0.0);
        let (l, _) = render(&mut s, 12_000, &[on(0, key, 1.0)]);
        let c = cents(pitch(&l, SR), 440.0);
        assert!(c.abs() < 1.0, "pitch track off, key {key}: {c} cents off");
    }
}

#[test]
fn a_44k1_sample_plays_at_its_pitch_in_a_48k_engine() {
    for (sample_rate, engine_rate) in [(44_100.0f32, 48_000.0f32), (48_000.0, 44_100.0)] {
        let len = sample_rate as usize / 2;
        let mut s = sampler(engine_rate, buf(sample_rate, sine(sample_rate, 440.0, len)));
        s.set_param(p("root"), 69.0);
        let (l, _) = render(&mut s, engine_rate as usize, &[on(0, 69, 1.0)]);
        let c = cents(pitch(&l, engine_rate), 440.0);
        assert!(
            c.abs() < 1.0,
            "{sample_rate} in {engine_rate}: {c} cents off"
        );

        let mut s = sampler(engine_rate, buf(sample_rate, vec![1.0; len]));
        let (l, _) = render(&mut s, engine_rate as usize, &[on(0, 60, 1.0)]);
        let expected = len as f64 * engine_rate as f64 / sample_rate as f64;
        let got = played_len(&l) as f64;
        assert!(
            (got - expected).abs() <= 1.0,
            "{sample_rate} in {engine_rate}: played {got} frames, expected {expected}"
        );
    }
}

#[test]
fn reads_between_samples_with_a_cubic() {
    // A 2 kHz sine played 7 semitones up reads between samples almost everywhere. Max error
    // against the ideal sine: cubic Hermite 2.9e-4, linear interpolation 8.5e-3.
    let x = sine(SR, 2_000.0, SR as usize);
    let mut s = sampler(SR, buf(SR, x));
    let (l, _) = render(&mut s, 20_000, &[on(0, 67, 1.0)]);
    let step = libm::pow(2.0, 7.0 / 12.0);
    let err = (10..20_000)
        .map(|i| {
            let ideal =
                libm::sin(2.0 * std::f64::consts::PI * 2_000.0 * i as f64 * step / SR as f64);
            (l[i] as f64 - ideal).abs()
        })
        .fold(0.0, f64::max);
    assert!(err < 1.5e-3, "max interpolation error {err}");
}

#[test]
fn start_end_and_reverse_play_the_chosen_region() {
    let len = 1000;
    let ramp: Vec<f32> = (0..len).map(|i| i as f32 / len as f32).collect();
    let mut s = sampler(SR, buf(SR, ramp.clone()));
    s.set_param(p("start"), 0.25);
    s.set_param(p("end"), 0.5);
    let (l, _) = render(&mut s, len, &[on(0, 60, 1.0)]);
    assert_eq!(played_len(&l), 250);
    assert_eq!(&l[..250], &ramp[250..500], "forward plays 0.25..0.5");

    s.set_param(p("reverse"), 1.0);
    let (l, _) = render(&mut s, len, &[on(0, 60, 1.0)]);
    assert_eq!(played_len(&l), 250);
    let backwards: Vec<f32> = ramp[250..500].iter().rev().copied().collect();
    assert_eq!(&l[..250], &backwards[..], "reverse plays 0.5 down to 0.25");
}

#[test]
fn a_switch_at_exactly_one_half_is_on() {
    // A scenario each switch changes: a ramp played an octave up, released early.
    let len = 4_800;
    let ramp: Vec<f32> = (0..len).map(|i| i as f32 / len as f32).collect();
    let play = |switch: &str, value: f32| {
        let mut s = sampler(SR, buf(SR, ramp.clone()));
        s.set_param(p("release"), 1.0);
        s.set_param(p(switch), value);
        render(&mut s, len, &[on(0, 72, 1.0), off(500, 72)]).0
    };
    for switch in ["reverse", "pitch_track", "one_shot", "declick"] {
        let (off_, half, on_) = (play(switch, 0.0), play(switch, 0.5), play(switch, 1.0));
        assert_ne!(
            off_, on_,
            "{switch}: the scenario must depend on the switch"
        );
        assert_eq!(half, on_, "{switch} at 0.5 is on");
    }
}

#[test]
fn one_shot_ignores_note_off_and_gate_mode_releases() {
    let x = noise(SR as usize);
    let mut a = sampler(SR, buf(SR, x.clone()));
    let mut b = sampler(SR, buf(SR, x.clone()));
    let (with_off, _) = render(&mut a, 30_000, &[on(0, 60, 1.0), off(4_800, 60)]);
    let (without, _) = render(&mut b, 30_000, &[on(0, 60, 1.0)]);
    assert_eq!(with_off, without, "one-shot: note-off changes nothing");

    let dc = vec![1.0; SR as usize];
    let mut s = sampler(SR, buf(SR, dc));
    s.set_param(p("one_shot"), 0.0);
    s.set_param(p("release"), 40.0);
    let (l, _) = render(&mut s, 20_000, &[on(0, 60, 1.0), off(4_800, 60)]);
    assert!(l[4_700] > 0.99, "sustaining before note-off");
    let after = 4_800 + 4_800; // 100 ms later
    let level = peak(&l[after..]);
    assert!(
        20.0 * libm::log10f(level.max(1e-12)) < -60.0,
        "100 ms after note-off: {level}"
    );
}

#[test]
fn a_note_off_releases_only_the_oldest_held_voice_on_its_key() {
    // Two overlapping notes on one key, told apart by velocity: the first note-off releases
    // the first note only, the second note-off the second.
    let mut s = sampler(SR, buf(SR, vec![1.0; SR as usize]));
    s.set_param(p("one_shot"), 0.0);
    s.set_param(p("release"), 40.0);
    let events = [
        on(0, 60, 0.25),
        on(1_000, 60, 0.5),
        off(2_000, 60),
        off(12_000, 60),
    ];
    let (l, _) = render(&mut s, 24_000, &events);
    assert!((l[1_900] - 0.75).abs() < 1e-3, "both sound: {}", l[1_900]);
    // 100 ms after the first note-off: the first note is gone, the second still held.
    assert!(
        (l[11_900] - 0.5).abs() < 1e-3,
        "the newer note sounds: {}",
        l[11_900]
    );
    // 100 ms after the second note-off: silence.
    let level = peak(&l[12_000 + 4_800..]);
    assert!(20.0 * libm::log10f(level.max(1e-12)) < -60.0, "{level}");
}

#[test]
fn a_late_note_off_of_a_finished_voice_leaves_the_newer_note_alone() {
    // A 100 ms sample: the first note ends by itself at frame 4 800, the second starts at
    // 4 900, then the first note's note-off arrives (a swung step longer than its sample).
    // It belongs to the first note: the second plays on.
    let mut s = sampler(SR, buf(SR, vec![1.0; 4_800]));
    s.set_param(p("one_shot"), 0.0);
    s.set_param(p("release"), 1.0);
    let events = [
        on(0, 60, 1.0),
        on(4_900, 60, 1.0),
        off(5_000, 60),
        off(9_000, 60),
    ];
    let (l, _) = render(&mut s, 12_000, &events);
    assert_eq!(l[4_850], 0.0, "the first note ended by itself");
    assert!(
        (l[6_000] - 1.0).abs() < 1e-6,
        "the second still sounds: {}",
        l[6_000]
    );
    // Its own note-off (1 ms release) ends it.
    assert!(peak(&l[9_000 + 480..]) < 1e-3);
}

#[test]
fn attack_and_decay_shape_the_level() {
    let dc = vec![1.0; SR as usize];
    let mut s = sampler(SR, buf(SR, dc.clone()));
    s.set_param(p("attack"), 10.0);
    let (l, _) = render(&mut s, 2_000, &[on(0, 60, 1.0)]);
    let at_5ms = l[240];
    assert!((0.45..=0.55).contains(&at_5ms), "level at 5 ms: {at_5ms}");
    assert_eq!(l[576], 1.0, "level at 12 ms");

    let mut s = sampler(SR, buf(SR, dc));
    s.set_param(p("decay"), 100.0);
    s.set_param(p("sustain"), 0.5);
    let (l, _) = render(&mut s, 24_000, &[on(0, 60, 1.0)]);
    assert!(l[0] > 0.99, "decay starts at full level");
    // The decay falls by e every quarter of its time: 25 ms (1200 frames) into a 100 ms decay
    // the distance to sustain is 1/e of where it started. Frame i has had i + 1 decay steps.
    let quarter = 0.5 + 0.5 / std::f32::consts::E;
    assert!(
        (l[1_199] - quarter).abs() < 1e-3,
        "level a quarter into the decay: {} (expected {quarter})",
        l[1_199]
    );
    let settled = &l[20_000..];
    assert!(
        settled.iter().all(|v| (v - 0.5).abs() < 1e-3),
        "settles to the sustain level: {}",
        settled[0]
    );
}

#[test]
fn velocity_gain_and_pan_scale_the_output() {
    let x = noise(2_000);
    let reference = {
        let mut s = sampler(SR, buf(SR, x.clone()));
        render(&mut s, 2_000, &[on(0, 60, 1.0)]).0
    };
    let close = |a: &[f32], b: &[f32], scale: f32| {
        a.iter()
            .zip(b)
            .all(|(a, b)| (a - b * scale).abs() <= 1e-5 * b.abs().max(1e-3))
    };

    let mut s = sampler(SR, buf(SR, x.clone()));
    let (l, r) = render(&mut s, 2_000, &[on(0, 60, 0.5)]);
    assert!(
        close(&l, &reference, 0.5) && close(&r, &reference, 0.5),
        "velocity"
    );

    let mut s = sampler(SR, buf(SR, x.clone()));
    s.set_param(p("gain"), -6.0206);
    let (l, r) = render(&mut s, 2_000, &[on(0, 60, 1.0)]);
    assert!(
        close(&l, &reference, 0.5) && close(&r, &reference, 0.5),
        "gain"
    );

    let mut s = sampler(SR, buf(SR, x.clone()));
    s.set_param(p("pan"), 1.0);
    let (l, r) = render(&mut s, 2_000, &[on(0, 60, 1.0)]);
    assert!(l.iter().all(|v| *v == 0.0), "pan right: left is silent");
    assert!(close(&r, &reference, SQRT_2), "pan right: right is +3 dB");
}

#[test]
fn declick_fades_the_region_edges() {
    let len = 4_800;
    let mut s = sampler(SR, buf(SR, vec![1.0; len]));
    s.set_param(p("declick"), 1.0);
    let (l, _) = render(&mut s, len + 200, &[on(0, 60, 1.0)]);
    assert_eq!(l[0], 0.0, "first output sample");
    assert!(l[1] > 0.0 && l[1] < 0.05, "fade-in starts low: {}", l[1]);
    assert_eq!(l[len / 2], 1.0, "full level mid-region");
    let fade = (0.004 * SR) as usize;
    let tail = &l[len - fade..len];
    assert!(tail[0] > 0.99, "fade-out starts at full level: {}", tail[0]);
    assert!(
        tail.windows(2).all(|w| w[1] <= w[0]),
        "fade-out is monotone"
    );
    assert_eq!(
        tail[fade - 1],
        0.0,
        "the last frame of the region is silent"
    );
    assert!(l[len..].iter().all(|v| *v == 0.0));
}

#[test]
fn the_seventeenth_note_steals_the_oldest_voice() {
    let mut s = sampler(SR, buf(SR, noise(SR as usize)));
    let notes: Vec<NoteEvent> = (0..17).map(|i| on(i * 10, 40 + i as u8, 1.0)).collect();
    s.set_param(p("one_shot"), 0.0);
    s.set_param(p("release"), 1.0);
    render(&mut s, 1_024, &notes);
    assert_eq!(s.active_voices(), VOICES);
    // Key 40 started first, so its voice went to key 56: releasing 40 frees nothing.
    render(&mut s, 2_048, &[off(0, 40)]);
    assert_eq!(
        s.active_voices(),
        VOICES,
        "the oldest note was the one stolen"
    );
    render(&mut s, 2_048, &[off(0, 41)]);
    assert_eq!(
        s.active_voices(),
        VOICES - 1,
        "the second note still sounded"
    );
}

#[test]
fn set_sample_returns_the_old_buffer_and_silences_voices() {
    let first = buf(SR, noise(SR as usize));
    let mut s = sampler(SR, Arc::clone(&first));
    render(&mut s, 256, &[on(0, 60, 1.0), on(10, 64, 1.0)]);
    assert_eq!(s.active_voices(), 2);

    let second = buf(SR, noise(1_000));
    let old = s
        .set_sample(Some(Arc::clone(&second)))
        .expect("the old buffer");
    assert!(Arc::ptr_eq(&old, &first), "returns the previous buffer");
    assert_eq!(s.active_voices(), 0, "every voice is silenced");
    assert!(Arc::ptr_eq(s.sample().unwrap(), &second));
    let (l, r) = render(&mut s, 256, &[]);
    assert!(l.iter().chain(&r).all(|v| *v == 0.0), "nothing plays on");

    let back = s.set_sample(None).expect("the second buffer");
    assert!(Arc::ptr_eq(&back, &second));
    assert!(s.sample().is_none());
}

#[test]
fn no_sample_is_silent() {
    let mut s = Sampler::new(SR);
    assert!(s.sample().is_none());
    let mut l = vec![1.0; BLOCK];
    let mut r = vec![1.0; BLOCK];
    s.process(&mut l, &mut r, None, &[on(0, 60, 1.0), off(10, 60)]);
    assert!(l.iter().chain(&r).all(|v| *v == 0.0));
    assert_eq!(s.active_voices(), 0);

    // Neither does a buffer whose rate is unusable (it could never advance).
    for rate in [0.0, f32::NAN] {
        s.set_sample(Some(buf(rate, vec![1.0; 100])));
        s.process(&mut l, &mut r, None, &[on(0, 60, 1.0)]);
        assert!(l.iter().chain(&r).all(|v| *v == 0.0), "rate {rate}");
        assert_eq!(s.active_voices(), 0);
    }
}

#[test]
fn process_never_allocates() {
    let mut s = Sampler::new(SR);
    s.set_sample(Some(buf(44_100.0, noise(44_100))));
    // Every M2 feature on: loop, a resonant filter swept by its envelope, velocity mapping.
    for (name, value) in [
        ("loop", 1.0),
        ("loop_start", 0.4),
        ("filter_type", 1.0),
        ("cutoff", 900.0),
        ("res", 0.6),
        ("filter_env", 0.8),
        ("flt_attack", 2.0),
        ("flt_decay", 80.0),
        ("flt_sustain", 0.3),
        ("flt_release", 50.0),
        ("vel_amp", 0.5),
        ("vel_filter", 0.7),
    ] {
        s.set_param(p(name), value);
    }
    let next = buf(SR, noise(10_000));
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    let events = [on(0, 60, 0.9), on(5, 72, 0.5), off(40, 60), on(63, 48, 1.0)];
    let mut swapped = None;
    for i in 0..1000u32 {
        let incoming = (i == 500).then(|| Arc::clone(&next));
        let old = assert_no_alloc(|| {
            s.set_param(i % 27, (i % 7) as f32 * 300.0 - 600.0);
            s.set_param(1000, 1.0);
            s.process(&mut l, &mut r, None, &events);
            incoming.map(|b| s.set_sample(Some(b)))
        });
        if let Some(old) = old {
            swapped = old;
        }
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }
    assert!(swapped.is_some(), "the old buffer came back out");
}

/// The M1 reference scene: a ramp played at keys 55, 60 and 67 in one-shot and gate mode
/// through attack, decay, sustain and release, with gain, pan, tune and fine tune, a start/end
/// region and reverse. Declick is on only where the sample plays at its own rate (Grooviq M2
/// times the edge fade in output frames, which only changes a pitched note). Returns the
/// sections concatenated, stereo interleaved.
fn m1_scene() -> Vec<f32> {
    let len = 4_800;
    let ramp: Vec<f32> = (0..len)
        .map(|i| 2.0 * i as f32 / len as f32 - 1.0)
        .collect();
    let section = |params: &[(&str, f32)], events: &[NoteEvent], frames: usize| {
        let mut s = Sampler::new(SR);
        s.set_sample(Some(buf(SR, ramp.clone())));
        for &(name, value) in params {
            s.set_param(p(name), value);
        }
        let (l, r) = render(&mut s, frames, events);
        l.into_iter()
            .zip(r)
            .flat_map(|(l, r)| [l, r])
            .collect::<Vec<f32>>()
    };
    let mut out = Vec::new();
    // Defaults: one-shot, declick on, at the root key.
    out.extend(section(&[], &[on(0, 60, 1.0), on(1_000, 60, 0.7)], 6_000));
    // Gate mode, a chord with attack, decay to a sustain, and release.
    out.extend(section(
        &[
            ("declick", 0.0),
            ("one_shot", 0.0),
            ("attack", 5.0),
            ("decay", 50.0),
            ("sustain", 0.6),
            ("release", 30.0),
        ],
        &[
            on(0, 55, 1.0),
            on(10, 60, 0.8),
            on(20, 67, 0.5),
            off(1_500, 60),
            off(2_000, 55),
            off(2_500, 67),
        ],
        6_000,
    ));
    // Gain, pan, tune and fine tune in one-shot mode.
    out.extend(section(
        &[
            ("declick", 0.0),
            ("gain", -6.0),
            ("pan", 0.4),
            ("tune", 2.0),
            ("fine", 30.0),
        ],
        &[on(0, 55, 1.0), on(700, 67, 0.9)],
        6_000,
    ));
    // A start/end region, reversed, with declick at the root key.
    out.extend(section(
        &[
            ("start", 0.2),
            ("end", 0.7),
            ("reverse", 1.0),
            ("pan", -0.5),
        ],
        &[on(0, 60, 1.0), on(3_000, 60, 0.6)],
        6_000,
    ));
    // The same region pitched, without declick, in gate mode.
    out.extend(section(
        &[
            ("declick", 0.0),
            ("start", 0.7),
            ("end", 0.2),
            ("reverse", 1.0),
            ("one_shot", 0.0),
            ("release", 10.0),
        ],
        &[
            on(0, 67, 1.0),
            on(100, 55, 1.0),
            off(900, 67),
            off(1_800, 55),
        ],
        6_000,
    ));
    out
}

#[test]
fn defaults_leave_the_m1_sound_unchanged() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sampler_m1.f32");
    let scene = m1_scene();
    if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
        let bytes: Vec<u8> = scene.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(path, bytes).expect("write the M1 reference");
    }
    let bytes = std::fs::read(path).expect("the M1 reference (create it with GOLDEN_UPDATE=1)");
    let reference: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(scene.len(), reference.len(), "length");
    assert!(peak(&reference) > 0.1, "the reference is not silent");
    if let Some(i) = (0..scene.len()).find(|&i| scene[i].to_bits() != reference[i].to_bits()) {
        panic!(
            "first difference at sample {i} (frame {}): {} vs reference {}",
            i / 2,
            scene[i],
            reference[i]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// M2: loop, filter, velocity mapping and the #73 fixes

/// Renders `frames` frames in `block`-frame blocks, applying each `(frame, param, value)`
/// change before the block that starts at its frame. `events` carry absolute frame offsets.
fn render_split(
    s: &mut Sampler,
    frames: usize,
    events: &[NoteEvent],
    block: usize,
    changes: &[(usize, &str, f32)],
) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    let mut start = 0;
    while start < frames {
        for &(at, name, value) in changes {
            assert_eq!(at % block, 0, "changes land on block boundaries");
            if at == start {
                s.set_param(p(name), value);
            }
        }
        let end = (start + block).min(frames);
        let block_events: Vec<NoteEvent> = events
            .iter()
            .filter(|e| (start..end).contains(&(e.offset as usize)))
            .map(|e| NoteEvent {
                offset: e.offset - start as u32,
                ..*e
            })
            .collect();
        s.process(&mut l[start..end], &mut r[start..end], None, &block_events);
        start = end;
    }
    (l, r)
}

/// Deterministic white noise in [-1, 1) from a seeded xorshift.
fn white(len: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}

/// Power at `hz` (Goertzel over a Hann window), averaged over consecutive 1024-frame segments.
fn power_at(x: &[f32], hz: f64) -> f64 {
    const SEG: usize = 1024;
    let w = 2.0 * std::f64::consts::PI * hz / SR as f64;
    let coeff = 2.0 * libm::cos(w);
    let window: Vec<f64> = (0..SEG)
        .map(|i| 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / SEG as f64))
        .collect();
    let mut total = 0.0;
    let mut count = 0;
    for seg in x.as_chunks::<SEG>().0 {
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for (v, w) in seg.iter().zip(&window) {
            let s0 = *v as f64 * w + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        total += s1 * s1 + s2 * s2 - coeff * s1 * s2;
        count += 1;
    }
    assert!(count > 0, "at least one segment");
    total / count as f64
}

/// Gain in dB of `wet` against `dry` (the same note unfiltered) at `hz`.
fn gain_db(wet: &[f32], dry: &[f32], hz: f64) -> f64 {
    10.0 * libm::log10(power_at(wet, hz) / power_at(dry, hz))
}

/// Energy of `x` between `lo` and `hi` Hz (100 Hz steps), one Hann-windowed segment.
fn band_energy(x: &[f32], lo: f64, hi: f64) -> f64 {
    let n = x.len();
    let mut total = 0.0;
    let mut hz = lo;
    while hz <= hi {
        let w = 2.0 * std::f64::consts::PI * hz / SR as f64;
        let coeff = 2.0 * libm::cos(w);
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let win = 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / n as f64);
            let s0 = *v as f64 * win + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        total += s1 * s1 + s2 * s2 - coeff * s1 * s2;
        hz += 100.0;
    }
    total
}

fn max_step(x: &[f32]) -> f32 {
    x.windows(2).fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

/// A sampler playing `x` at its own rate with the named parameters set.
fn configured(x: &[f32], params: &[(&str, f32)]) -> Sampler {
    let mut s = sampler(SR, buf(SR, x.to_vec()));
    for &(name, value) in params {
        s.set_param(p(name), value);
    }
    s
}

#[test]
fn a_loop_repeats_until_release_then_releases() {
    let len = 1_000;
    let ramp: Vec<f32> = (0..len).map(|i| (i + 1) as f32 / len as f32).collect();
    for reverse in [false, true] {
        let mut s = configured(
            &ramp,
            &[
                ("loop", 1.0),
                ("loop_start", 0.5),
                ("reverse", if reverse { 1.0 } else { 0.0 }),
            ],
        );
        assert!(
            SPEC[p("one_shot") as usize].2 >= 0.5,
            "one-shot is on: the loop overrides it"
        );
        let (l, _) = render(&mut s, 12_000, &[on(0, 60, 1.0), off(5_000, 60)]);
        // While held: the whole region once, then its second half (in play order) again.
        let played: Vec<f32> = if reverse {
            ramp.iter().rev().copied().collect()
        } else {
            ramp.clone()
        };
        for (f, v) in l[..5_000].iter().enumerate() {
            let expected = if f < len {
                played[f]
            } else {
                played[500 + (f - len) % 500]
            };
            assert_eq!(*v, expected, "reverse {reverse}, frame {f}");
        }
        // After note-off it releases (40 ms) instead of stopping or looping on.
        assert!(
            l[5_000].abs() > 0.0,
            "the release starts where the loop was"
        );
        let tail = peak(&l[5_000 + 4_800..]);
        assert!(
            20.0 * libm::log10f(tail.max(1e-12)) < -60.0,
            "reverse {reverse}: 100 ms after note-off: {tail}"
        );
        assert_eq!(s.active_voices(), 0, "reverse {reverse}: released");
    }
}

#[test]
fn the_loop_seam_is_click_free_with_declick() {
    for (key, reverse) in [(60u8, false), (67, false), (55, true)] {
        let mut s = configured(
            &[1.0; 1_000],
            &[
                ("declick", 1.0),
                ("loop", 1.0),
                ("loop_start", 0.5),
                ("reverse", if reverse { 1.0 } else { 0.0 }),
            ],
        );
        let (l, _) = render(&mut s, 20_000, &[on(0, key, 1.0)]);
        // Past the 2 ms fade-in the level is flat across every seam.
        let held = &l[200..];
        assert!(held.iter().all(|v| *v > 0.99), "key {key}: still sounding");
        let step = max_step(held);
        assert!(step <= 1e-6, "key {key} reverse {reverse}: step {step}");
    }
}

#[test]
fn filters_shape_the_spectrum() {
    let x = white(2 * SR as usize, 0x9E37_79B9);
    let frames = x.len() - 1_024;
    let play = |params: &[(&str, f32)]| {
        let mut s = configured(&x, params);
        render(&mut s, frames, &[on(0, 60, 1.0)]).0
    };
    let dry = play(&[]);
    let filtered = |kind: f32| play(&[("filter_type", kind), ("cutoff", 1_000.0)]);

    let lp = filtered(1.0);
    let lp_drop = gain_db(&lp, &dry, 8_000.0) - gain_db(&lp, &dry, 250.0);
    assert!(lp_drop <= -18.0, "low-pass: 8 kHz vs 250 Hz {lp_drop} dB");

    let hp = filtered(2.0);
    let hp_drop = gain_db(&hp, &dry, 250.0) - gain_db(&hp, &dry, 8_000.0);
    assert!(hp_drop <= -18.0, "high-pass: 250 Hz vs 8 kHz {hp_drop} dB");

    let bp = filtered(3.0);
    let (mut best, mut best_hz) = (f64::NEG_INFINITY, 0.0);
    for i in 0..=72 {
        let hz = 125.0 * libm::exp2(i as f64 / 12.0);
        let g = gain_db(&bp, &dry, hz);
        if g > best {
            (best, best_hz) = (g, hz);
        }
    }
    let off = libm::log2(best_hz / 1_000.0).abs();
    assert!(off <= 1.0 / 3.0, "band-pass peaks at {best_hz} Hz");

    // Type 0 is no filter at all, whatever the other filter settings.
    let bypass = play(&[
        ("filter_type", 0.0),
        ("cutoff", 300.0),
        ("res", 0.9),
        ("filter_env", -0.5),
        ("vel_filter", 1.0),
    ]);
    assert_eq!(bypass, dry, "filter_type 0 is bit-identical to no filter");

    // Resonance lifts the low-pass response at the cutoff (Q 0.707 to Q 20).
    let resonant = play(&[("filter_type", 1.0), ("cutoff", 1_000.0), ("res", 1.0)]);
    let lift = gain_db(&resonant, &dry, 1_000.0) - gain_db(&lp, &dry, 1_000.0);
    assert!(lift >= 12.0, "res 1 vs 0 at the cutoff: {lift} dB");
}

#[test]
fn the_filter_envelope_sweeps_the_cutoff() {
    let x = white(SR as usize, 0x2545_F491);
    let mut s = configured(
        &x,
        &[
            ("filter_type", 1.0),
            ("cutoff", 200.0),
            ("filter_env", 1.0),
            ("flt_decay", 100.0),
        ],
    );
    let (l, _) = render(&mut s, 20_000, &[on(0, 60, 1.0)]);
    let mut dry = configured(&x, &[]);
    let (d, _) = render(&mut dry, 20_000, &[on(0, 60, 1.0)]);
    let window = 480; // 10 ms
    let at = 14_400; // 300 ms
    let early =
        band_energy(&l[..window], 2_000.0, 20_000.0) / band_energy(&d[..window], 2_000.0, 20_000.0);
    let late = band_energy(&l[at..at + window], 2_000.0, 20_000.0)
        / band_energy(&d[at..at + window], 2_000.0, 20_000.0);
    let db = 10.0 * libm::log10(early / late);
    assert!(db >= 12.0, "above 2 kHz, first 10 ms vs 300 ms: {db} dB");

    // Held at full level, the envelope keeps the cutoff up until note-off, then its release
    // brings it down while the note (a long amp release) still sounds.
    let gate = [("one_shot", 0.0), ("release", 1_000.0)];
    let mut s = configured(
        &x,
        &[
            gate[0],
            gate[1],
            ("filter_type", 1.0),
            ("cutoff", 200.0),
            ("filter_env", 1.0),
            ("flt_sustain", 1.0),
            ("flt_release", 20.0),
        ],
    );
    let events = [on(0, 60, 1.0), off(4_800, 60)];
    let (l, _) = render(&mut s, 12_000, &events);
    let mut dry = configured(&x, &gate);
    let (d, _) = render(&mut dry, 12_000, &events);
    let bright = |at: usize| {
        band_energy(&l[at..at + window], 2_000.0, 20_000.0)
            / band_energy(&d[at..at + window], 2_000.0, 20_000.0)
    };
    let db = 10.0 * libm::log10(bright(4_800 - window) / bright(9_600));
    assert!(
        db >= 12.0,
        "above 2 kHz, before note-off vs 100 ms after: {db} dB"
    );
}

#[test]
fn the_filter_envelope_attack_opens_the_cutoff_over_its_time() {
    // A 200 ms filter attack (the amp attack stays 0): the cutoff climbs from 200 Hz towards
    // 12.8 kHz over the whole attack, then holds at full envelope (sustain 1).
    let x = white(SR as usize, 0x6C07_8965);
    let mut s = configured(
        &x,
        &[
            ("filter_type", 1.0),
            ("cutoff", 200.0),
            ("filter_env", 1.0),
            ("flt_attack", 200.0),
            ("flt_sustain", 1.0),
        ],
    );
    let (l, _) = render(&mut s, 20_000, &[on(0, 60, 1.0)]);
    let mut dry = configured(&x, &[]);
    let (d, _) = render(&mut dry, 20_000, &[on(0, 60, 1.0)]);
    let window = 480; // 10 ms
    let bright = |at: usize| {
        band_energy(&l[at..at + window], 2_000.0, 20_000.0)
            / band_energy(&d[at..at + window], 2_000.0, 20_000.0)
    };
    let db = |a: f64, b: f64| 10.0 * libm::log10(a / b);
    let (start, middle, end) = (bright(0), bright(4_560), bright(14_400));
    let opens = db(end, start);
    assert!(opens >= 12.0, "300 ms vs the first 10 ms: {opens} dB");
    // Halfway through (95..105 ms) the cutoff is still on its way up.
    let rising = db(end, middle);
    assert!(rising >= 6.0, "300 ms vs 100 ms: {rising} dB");
}

#[test]
fn velocity_maps_to_level_and_filter() {
    let x = noise(SR as usize);
    let play = |params: &[(&str, f32)], velocity: f32| {
        let mut s = configured(&x, params);
        render(&mut s, 30_000, &[on(0, 60, velocity)]).0
    };
    assert_eq!(
        play(&[("vel_amp", 0.0)], 0.25),
        play(&[("vel_amp", 0.0)], 1.0),
        "vel_amp 0: velocity leaves the level alone"
    );
    let full = play(&[("vel_amp", 1.0)], 1.0);
    let half = play(&[("vel_amp", 1.0)], 0.5);
    assert!(
        full.iter()
            .zip(&half)
            .all(|(f, h)| (f * 0.5 - h).abs() <= 1e-6),
        "vel_amp 1 halves the level at velocity 0.5"
    );
    let partial = play(&[("vel_amp", 0.5)], 0.5);
    assert!(
        full.iter()
            .zip(&partial)
            .all(|(f, h)| (f * 0.75 - h).abs() <= 1e-6),
        "vel_amp 0.5 at velocity 0.5 plays at 0.75"
    );

    let lp = [
        ("vel_amp", 0.0),
        ("filter_type", 1.0),
        ("cutoff", 4_000.0),
        ("vel_filter", 1.0),
    ];
    let bright = play(&lp, 1.0);
    let dark = play(&lp, 0.5);
    let db = 10.0 * libm::log10(power_at(&dark, 8_000.0) / power_at(&bright, 8_000.0));
    assert!(db <= -6.0, "vel_filter 1 at velocity 0.5: 8 kHz {db} dB");
}

#[test]
fn cutoff_changes_are_smooth() {
    let x: Vec<f32> = sine(SR, 3_000.0, SR as usize)
        .iter()
        .map(|v| 0.5 + 0.4 * v)
        .collect();
    let play = |filter: f32| {
        let mut s = configured(&x, &[("declick", 1.0), ("filter_type", filter)]);
        render_split(
            &mut s,
            9_600,
            &[on(0, 60, 1.0)],
            BLOCK,
            &[(4_800, "cutoff", 200.0)],
        )
        .0
    };
    let dry = max_step(&play(0.0));
    let lp = play(1.0);
    assert!(peak(&lp[9_000..]) < 0.6, "the cutoff did fall");
    let step = max_step(&lp);
    assert!(step <= dry + 0.05, "largest step {step}, unfiltered {dry}");
    // The cutoff glides down over 5 ms rather than jumping: 1 ms after the change the 3 kHz
    // tone still passes; 10 ms after it, it is gone.
    let swing = |x: &[f32]| {
        let (lo, hi) = x
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        hi - lo
    };
    let early = swing(&lp[4_848..4_896]);
    assert!(early > 0.5, "1 ms after the change: peak-to-peak {early}");
    let late = swing(&lp[5_280..5_328]);
    assert!(late < 0.05, "10 ms after the change: peak-to-peak {late}");
}

#[test]
fn changing_sustain_while_held_does_not_click() {
    let mut s = configured(&[1.0; SR as usize], &[("one_shot", 0.0), ("decay", 10.0)]);
    let (l, _) = render_split(
        &mut s,
        24_000,
        &[on(0, 60, 1.0)],
        BLOCK,
        &[(4_800, "sustain", 0.2)],
    );
    assert!((l[20_000] - 0.2).abs() < 1e-4, "reaches the new sustain");
    let step = max_step(&l[1..]);
    let limit = 1.0 / (0.005 * SR);
    assert!(step < limit, "max step {step}, limit {limit}");
}

#[test]
fn the_edge_fade_lasts_4_ms_at_any_pitch() {
    let fade = (0.004 * SR) as i64;
    for key in [48u8, 84] {
        let mut s = configured(&[1.0; SR as usize], &[("declick", 1.0)]);
        let (l, _) = render(&mut s, 2 * SR as usize, &[on(0, key, 1.0)]);
        let end = played_len(&l);
        let start = (end / 2..end).find(|&f| l[f] < 1.0 - 1e-6).expect("a fade");
        let len = (end - start) as i64;
        assert!(
            (len - fade).abs() <= 1,
            "key {key}: the fade-out lasts {len} frames, expected {fade}"
        );
    }
}

#[test]
fn a_silent_sustain_zero_voice_frees_its_slot() {
    let x = noise(10 * SR as usize);
    for one_shot in [1.0, 0.0] {
        let mut s = configured(
            &x,
            &[("one_shot", one_shot), ("sustain", 0.0), ("decay", 10.0)],
        );
        render(&mut s, 9_600, &[on(0, 60, 1.0)]);
        assert_eq!(s.active_voices(), 0, "one_shot {one_shot}: freed in 200 ms");
    }
    // A freed gate note still owns its note-off: it does not end a newer note on its key.
    let mut s = configured(
        &[1.0; 10 * SR as usize],
        &[("one_shot", 0.0), ("sustain", 0.0), ("decay", 10.0)],
    );
    render(&mut s, 9_600, &[on(0, 60, 1.0)]);
    assert_eq!(s.active_voices(), 0);
    s.set_param(p("sustain"), 1.0);
    let (l, _) = render(&mut s, 9_600, &[on(0, 60, 1.0), off(100, 60)]);
    assert!(
        (l[9_000] - 1.0).abs() < 1e-6,
        "the newer note sounds: {}",
        l[9_000]
    );
    assert_eq!(s.active_voices(), 1);
}

#[test]
fn block_splitting_never_changes_the_output() {
    let x = noise(SR as usize);
    let params = [
        ("declick", 1.0),
        ("one_shot", 0.0),
        ("loop", 1.0),
        ("loop_start", 0.3),
        ("filter_type", 1.0),
        ("cutoff", 800.0),
        ("res", 0.5),
        ("filter_env", 0.7),
        ("flt_attack", 3.0),
        ("flt_decay", 60.0),
        ("flt_sustain", 0.2),
        ("flt_release", 30.0),
        ("vel_filter", 0.5),
        ("sustain", 0.5),
        ("decay", 40.0),
    ];
    let events = [
        on(3, 55, 0.9),
        on(500, 67, 0.6),
        off(2_000, 55),
        on(2_100, 60, 0.8),
        off(9_000, 67),
        off(12_000, 60),
    ];
    // At frame 4 480 (a multiple of 1, 7, 64 and 128) the cutoff, sustain and type change.
    let changes = [
        (4_480, "cutoff", 3_000.0),
        (4_480, "sustain", 0.9),
        (4_480, "filter_type", 3.0),
    ];
    let play = |block: usize| {
        let mut s = configured(&x, &params);
        render_split(&mut s, 16_000, &events, block, &changes)
    };
    let reference = play(1);
    assert!(peak(&reference.0) > 0.01);
    for block in [7, 64, 128] {
        assert!(same_bits(&play(block), &reference), "block {block}");
    }
}

/// Both renders hold the same bits in every sample of both channels.
fn same_bits(a: &(Vec<f32>, Vec<f32>), b: &(Vec<f32>, Vec<f32>)) -> bool {
    let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<u32>>();
    bits(&a.0) == bits(&b.0) && bits(&a.1) == bits(&b.1)
}

#[test]
fn block_splitting_never_changes_a_loop_across_its_seams() {
    // A 1 000-frame source looped for 12 000 frames or more: dozens of wraps in each direction, at
    // pitches that read between samples, through each filter type, then released.
    let x = noise(1_000);
    let events = [
        on(0, 55, 1.0),
        on(5, 60, 0.7),
        on(11, 67, 0.9),
        off(12_000, 55),
        off(13_000, 60),
        off(14_000, 67),
    ];
    for reverse in [0.0, 1.0] {
        for loop_start in [0.0, 0.5, 1.0] {
            for filter in [0.0, 1.0, 2.0, 3.0] {
                let params = [
                    ("declick", 1.0),
                    ("loop", 1.0),
                    ("loop_start", loop_start),
                    ("reverse", reverse),
                    ("filter_type", filter),
                    ("cutoff", 1_500.0),
                    ("res", 0.8),
                    ("filter_env", 0.5),
                    ("flt_decay", 200.0),
                ];
                let play = |block: usize| {
                    let mut s = configured(&x, &params);
                    let out = render_split(&mut s, 20_000, &events, block, &[]);
                    assert_eq!(s.active_voices(), 0, "released by the end");
                    out
                };
                let reference = play(1);
                let what = format!("reverse {reverse}, loop_start {loop_start}, filter {filter}");
                // A one-frame loop is DC, which the high- and band-pass remove.
                let dc_removed = loop_start == 1.0 && filter >= 2.0;
                assert!(
                    dc_removed || peak(&reference.0[10_000..12_000]) > 0.01,
                    "{what}: still looping at 11 000 frames"
                );
                for block in [7, 64, 128] {
                    assert!(same_bits(&play(block), &reference), "{what}: block {block}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// M2: voice modes, glide, slide notes and choke

/// Frames a choke or steal fade lasts (3 ms).
const FADE: usize = SR as usize * 3 / 1000;

fn slide(offset: u32, key: u8, velocity: f32) -> NoteEvent {
    NoteEvent {
        slide: true,
        ..on(offset, key, velocity)
    }
}

/// A one-second full-scale DC sample.
fn dc() -> Vec<f32> {
    vec![1.0; SR as usize]
}

/// A two-second 1 kHz sine: with `root` 48, key 48 plays it at 1 kHz.
fn sine_1k() -> Vec<f32> {
    sine(SR, 1000.0, 2 * SR as usize)
}

/// Pitch over time, one point per period between rising zero crossings (sub-sample accurate):
/// `(frame at the period's centre, key)`, where key `root` plays at `root_hz`.
fn pitch_track(x: &[f32], root: f64, root_hz: f64) -> Vec<(f64, f64)> {
    let mut crossings = Vec::new();
    for i in 1..x.len() {
        let (a, b) = (x[i - 1] as f64, x[i] as f64);
        if a < 0.0 && b >= 0.0 {
            crossings.push((i - 1) as f64 + a / (a - b));
        }
    }
    crossings
        .windows(2)
        .map(|w| {
            let hz = SR as f64 / (w[1] - w[0]);
            ((w[0] + w[1]) / 2.0, root + 12.0 * libm::log2(hz / root_hz))
        })
        .collect()
}

/// Mean key of the track points between frames `from` and `to`.
fn key_between(track: &[(f64, f64)], from: f64, to: f64) -> f64 {
    let keys: Vec<f64> = track
        .iter()
        .filter(|(t, _)| (from..to).contains(t))
        .map(|(_, k)| *k)
        .collect();
    assert!(keys.len() > 3, "too few periods between {from} and {to}");
    keys.iter().sum::<f64>() / keys.len() as f64
}

/// The frame at which a straight line fitted through the track points whose key lies in
/// `lo..hi` (the middle of a glide) reaches key `at`.
fn line_reaches(track: &[(f64, f64)], lo: f64, hi: f64, at: f64) -> f64 {
    let pts: Vec<(f64, f64)> = track
        .iter()
        .copied()
        .filter(|(_, k)| (lo..hi).contains(k))
        .collect();
    assert!(pts.len() > 3, "too few periods between keys {lo} and {hi}");
    let n = pts.len() as f64;
    let mt = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let mk = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let cov = pts.iter().map(|p| (p.0 - mt) * (p.1 - mk)).sum::<f64>();
    let var = pts.iter().map(|p| (p.0 - mt) * (p.0 - mt)).sum::<f64>();
    let slope = cov / var;
    mt + (at - mk) / slope
}

/// Renders like `render_split`, calling `choke` before the block that starts at `choke_at`.
fn render_choked(
    s: &mut Sampler,
    frames: usize,
    events: &[NoteEvent],
    block: usize,
    choke_at: usize,
) -> (Vec<f32>, Vec<f32>) {
    assert_eq!(choke_at % block, 0, "the choke lands on a block boundary");
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    let mut start = 0;
    while start < frames {
        if start == choke_at {
            s.choke();
        }
        let end = (start + block).min(frames);
        let block_events: Vec<NoteEvent> = events
            .iter()
            .filter(|e| (start..end).contains(&(e.offset as usize)))
            .map(|e| NoteEvent {
                offset: e.offset - start as u32,
                ..*e
            })
            .collect();
        s.process(&mut l[start..end], &mut r[start..end], None, &block_events);
        start = end;
    }
    (l, r)
}

#[test]
fn mono_cuts_the_previous_note_with_a_fade() {
    // DC with declick: the new note fades in over 2 ms while the cut one fades out over 3 ms, so
    // no frame moves by more than a full-scale 3 ms fade does.
    let mut s = configured(&dc(), &[("declick", 1.0), ("voice_mode", 1.0)]);
    let events = [on(0, 60, 1.0), on(4_800, 64, 1.0)];
    let (mut l, _) = render(&mut s, 4_800 + FADE - 2, &events);
    assert_eq!(s.active_voices(), 2, "the cut note is still fading");
    let (rest, _) = render(&mut s, 3, &[]);
    l.extend(rest);
    assert_eq!(
        s.active_voices(),
        1,
        "3 ms + 1 frame later only the new note sounds"
    );
    let (rest, _) = render(&mut s, 4_800, &[]);
    l.extend(rest);
    let slope = 1.0 / FADE as f32;
    let step = max_step(&l[4_700..]);
    assert!(
        step <= slope + 1e-6,
        "step {step} above the fade slope {slope}"
    );
    assert!(
        (l[l.len() - 1] - 1.0).abs() < 1e-6,
        "one note sounds, not two: {}",
        l[l.len() - 1]
    );
}

#[test]
fn legato_glides_without_retriggering() {
    // Key 55 joins while 48 is held: with a 100 ms glide the pitch moves in a straight line from
    // 48 to 55 over 100 ms, and the 20 ms attack does not start again.
    let mut s = configured(
        &sine_1k(),
        &[
            ("root", 48.0),
            ("one_shot", 0.0),
            ("attack", 20.0),
            ("voice_mode", 2.0),
            ("glide", 100.0),
        ],
    );
    let events = [
        on(0, 48, 1.0),
        on(9_600, 55, 1.0),
        off(30_000, 48),
        off(30_000, 55),
    ];
    let (l, _) = render(&mut s, 36_000, &events);
    let track = pitch_track(&l, 48.0, 1000.0);
    let before = key_between(&track, 2_000.0, 9_500.0);
    let after = key_between(&track, 15_000.0, 29_500.0);
    assert!((before - 48.0).abs() < 0.01, "held at {before}");
    assert!((after - 55.0).abs() < 0.01, "arrives at {after}");
    let glide: Vec<f64> = track
        .iter()
        .filter(|(t, _)| (9_000.0..16_000.0).contains(t))
        .map(|(_, k)| *k)
        .collect();
    assert!(
        glide.windows(2).all(|w| w[1] >= w[0] - 1e-3),
        "not monotonic: {glide:?}"
    );
    let start = line_reaches(&track, 49.0, 54.0, 48.0);
    let end = line_reaches(&track, 49.0, 54.0, 55.0);
    assert!(
        (start - 9_600.0).abs() <= 48.0,
        "the glide starts at {start}"
    );
    assert!((end - 14_400.0).abs() <= 48.0, "the glide ends at {end}");
    let held = peak(&l[8_000..9_600]);
    for (i, w) in l[9_600..29_500].chunks(240).enumerate() {
        assert!(
            peak(w) >= 0.9 * held,
            "dip at {}: {} of {held}",
            9_600 + i * 240,
            peak(w)
        );
    }
    assert_eq!(s.active_voices(), 0, "released by the end");
}

#[test]
fn releasing_the_top_key_returns_to_the_held_one() {
    // Mono, glide 0: 55 takes over from the held 48; releasing 55 jumps back to 48 without a
    // new attack, and releasing 48 ends the note.
    let mut s = configured(
        &sine_1k(),
        &[
            ("root", 48.0),
            ("one_shot", 0.0),
            ("attack", 20.0),
            ("voice_mode", 1.0),
        ],
    );
    let events = [on(0, 48, 1.0), on(9_600, 55, 1.0), off(19_200, 55)];
    let (l, _) = render(&mut s, 28_800, &events);
    let track = pitch_track(&l, 48.0, 1000.0);
    let top = key_between(&track, 12_000.0, 19_100.0);
    let back = key_between(&track, 19_300.0, 28_700.0);
    assert!((top - 55.0).abs() < 0.01, "plays the top key: {top}");
    assert!((back - 48.0).abs() < 0.01, "back to the held key: {back}");
    assert_eq!(s.active_voices(), 1);
    let held = peak(&l[15_000..19_200]);
    for (i, w) in l[19_200..28_800].chunks(240).enumerate() {
        assert!(
            peak(w) >= 0.9 * held,
            "dip at {}: {} of {held}",
            19_200 + i * 240,
            peak(w)
        );
    }
    let (l, _) = render(&mut s, 9_600, &[off(0, 48)]);
    assert!(peak(&l[4_800..]) < 1e-3, "released: {}", peak(&l[4_800..]));
    assert_eq!(s.active_voices(), 0);
}

#[test]
fn a_slide_note_bends_the_held_note() {
    // Poly mode, key 48 held, slide 52: the held voice glides to 52 over the default 60 ms and
    // becomes that note. The 48 note-off belongs to the note that slid away; 52's ends it.
    let mut s = configured(&sine_1k(), &[("root", 48.0), ("one_shot", 0.0)]);
    let events = [on(0, 48, 1.0), slide(9_600, 52, 1.0), off(19_200, 48)];
    let (l, _) = render(&mut s, 24_000, &events);
    assert_eq!(s.active_voices(), 1, "the slide moved the held voice");
    let track = pitch_track(&l, 48.0, 1000.0);
    let before = key_between(&track, 2_000.0, 9_500.0);
    let after = key_between(&track, 13_000.0, 23_900.0);
    assert!((before - 48.0).abs() < 0.01, "held at {before}");
    assert!((after - 52.0).abs() < 0.01, "slid to {after}");
    let end = line_reaches(&track, 48.5, 51.5, 52.0);
    assert!((end - 12_480.0).abs() <= 48.0, "the slide ends at {end}");
    assert!(
        peak(&l[21_000..24_000]) > 0.9,
        "the 48 note-off left it alone"
    );
    let (l, _) = render(&mut s, 9_600, &[off(0, 52)]);
    assert!(peak(&l[4_800..]) < 1e-3, "52's note-off released it");
    assert_eq!(s.active_voices(), 0);
}

#[test]
fn a_late_note_off_of_a_slid_note_leaves_the_newer_note_alone() {
    // 48 slides to 52, then a new 48 starts before the first 48's note-off arrives. That
    // note-off belongs to the note that slid away: the new 48 plays on until its own.
    let mut s = configured(&dc(), &[("one_shot", 0.0), ("release", 1.0)]);
    let events = [
        on(0, 48, 1.0),
        slide(4_800, 52, 1.0),
        on(9_600, 48, 1.0),
        off(12_000, 48),
    ];
    let (l, _) = render(&mut s, 14_400, &events);
    assert!((l[14_399] - 2.0).abs() < 1e-6, "both sound: {}", l[14_399]);
    let (l, _) = render(&mut s, 4_800, &[off(0, 48)]);
    assert!(
        (l[4_799] - 1.0).abs() < 1e-6,
        "its own note-off: {}",
        l[4_799]
    );
    let (l, _) = render(&mut s, 4_800, &[off(0, 52)]);
    assert!(peak(&l[480..]) < 1e-3);
    assert_eq!(s.active_voices(), 0);
}

#[test]
fn a_slide_with_nothing_held_glides_from_the_last_pitch() {
    // 48 played and released; a slide to 52 later starts a new note at 48's pitch that glides to
    // 52 over 60 ms.
    let mut s = configured(&sine_1k(), &[("root", 48.0), ("one_shot", 0.0)]);
    let events = [on(0, 48, 1.0), off(4_800, 48), slide(14_400, 52, 1.0)];
    let (l, _) = render(&mut s, 30_000, &events);
    assert!(peak(&l[12_000..14_400]) < 1e-3, "silent before the slide");
    let track = pitch_track(&l[14_400..], 48.0, 1000.0);
    let shifted: Vec<(f64, f64)> = track.iter().map(|(t, k)| (t + 14_400.0, *k)).collect();
    let start = line_reaches(&shifted, 48.5, 51.5, 48.0);
    let end = line_reaches(&shifted, 48.5, 51.5, 52.0);
    assert!(
        (start - 14_400.0).abs() <= 48.0,
        "the glide starts at {start}"
    );
    assert!((end - 17_280.0).abs() <= 48.0, "the glide ends at {end}");
    let after = key_between(&shifted, 18_000.0, 29_900.0);
    assert!((after - 52.0).abs() < 0.01, "arrives at {after}");
}

#[test]
fn choke_fades_every_voice_in_3_ms() {
    // Three DC voices sum to 3. Choked, they fall in a straight line to silence over 3 ms.
    let chords = [on(0, 48, 1.0), on(10, 60, 1.0), on(20, 72, 1.0)];
    let mut s = configured(&dc(), &[]);
    render(&mut s, 4_800, &chords);
    s.choke();
    let (l, _) = render(&mut s, FADE + 1, &[]);
    for (k, v) in l.iter().enumerate() {
        let expected = 3.0 * FADE.saturating_sub(k + 1) as f32 / FADE as f32;
        assert!(
            (v - expected).abs() < 1e-5,
            "frame {k}: {v}, expected {expected}"
        );
    }
    assert!(l[FADE - 1..].iter().all(|v| v.abs() < 1e-6));
    assert_eq!(s.active_voices(), 0);

    // A note at the next frame plays as on a fresh sampler; the choked voices only add their fade.
    let mut s = configured(&dc(), &[]);
    render(&mut s, 4_800, &chords);
    s.choke();
    let (l, _) = render(&mut s, 4_800, &[on(0, 60, 0.5)]);
    let mut fresh = configured(&dc(), &[]);
    let (f, _) = render(&mut fresh, 4_800, &[on(0, 60, 0.5)]);
    for k in 0..FADE {
        let tail = 3.0 * FADE.saturating_sub(k + 1) as f32 / FADE as f32;
        assert!((l[k] - f[k] - tail).abs() < 1e-5, "frame {k}");
    }
    assert_eq!(&l[FADE..], &f[FADE..], "unattenuated after the fade");
}

#[test]
fn a_late_note_off_after_a_choke_leaves_the_newer_note_alone() {
    // Gate mode: a choked note's note-off arrives after a new note on its key started. It belongs
    // to the choked note, so the new one plays on until its own note-off.
    for mode in [0.0, 1.0, 2.0] {
        let mut s = configured(
            &dc(),
            &[("one_shot", 0.0), ("release", 1.0), ("voice_mode", mode)],
        );
        render(&mut s, 4_800, &[on(0, 60, 1.0)]);
        s.choke();
        let (l, _) = render(&mut s, 4_800, &[on(0, 60, 0.5), off(100, 60)]);
        assert!(
            (l[4_799] - 0.5).abs() < 1e-6,
            "mode {mode}: the newer note sounds: {}",
            l[4_799]
        );
        let (l, _) = render(&mut s, 4_800, &[off(0, 60)]);
        assert!(
            peak(&l[480..]) < 1e-3,
            "mode {mode}: its own note-off ends it"
        );
        assert_eq!(s.active_voices(), 0, "mode {mode}");
    }
}

#[test]
fn a_stolen_voice_fades_instead_of_clicking() {
    // 17 overlapping DC notes with 10 ms attacks: the 17th steals the first, which fades out
    // over 3 ms instead of dropping out at once.
    let mut s = configured(&dc(), &[("declick", 1.0), ("attack", 10.0)]);
    let notes: Vec<NoteEvent> = (0..17).map(|i| on(i * 480, 40 + i as u8, 1.0)).collect();
    let at = 16 * 480;
    let (l, _) = render(&mut s, at + 4_800, &notes);
    let slope = 1.0 / FADE as f32;
    let step = max_step(&l);
    assert!(
        step <= slope + 1e-6,
        "step {step} above the fade slope {slope}"
    );
    assert!((l[at - 1] - 16.0).abs() < 1e-3, "16 voices: {}", l[at - 1]);
    assert!(
        (l[l.len() - 1] - 16.0).abs() < 1e-3,
        "still 16: {}",
        l[l.len() - 1]
    );
    assert_eq!(s.active_voices(), VOICES);
}

#[test]
fn voice_mode_and_glide_never_allocate() {
    let mut s = Sampler::new(SR);
    s.set_sample(Some(buf(44_100.0, noise(44_100))));
    s.set_param(p("one_shot"), 0.0);
    s.set_param(p("filter_type"), 1.0);
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    let events = [
        on(0, 60, 0.9),
        slide(5, 67, 0.8),
        on(9, 48, 0.7),
        on(12, 48, 0.6),
        off(20, 60),
        slide(30, 72, 1.0),
        off(40, 67),
        off(50, 48),
        off(55, 48),
        on(60, 36, 0.5),
        off(63, 72),
    ];
    for i in 0..1_500u32 {
        assert_no_alloc(|| {
            s.set_param(p("voice_mode"), ((i / 100) % 3) as f32);
            s.set_param(p("glide"), ((i % 5) * 100) as f32);
            s.process(&mut l, &mut r, None, &events);
            if i % 50 == 49 {
                s.choke();
            }
        });
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }
}

#[test]
fn block_splitting_never_changes_glides() {
    let x = noise(SR as usize);
    let events = [
        on(3, 48, 0.9),
        on(700, 55, 0.8),
        slide(1_500, 60, 1.0),
        off(2_000, 55),
        on(2_500, 43, 0.7),
        slide(3_100, 50, 0.6),
        off(3_900, 48),
        on(5_000, 62, 0.8),
        slide(5_300, 57, 0.9),
        off(6_500, 60),
        off(7_000, 43),
        off(7_200, 62),
        off(8_000, 50),
        off(9_000, 57),
    ];
    for mode in [0.0, 1.0, 2.0] {
        for one_shot in [0.0, 1.0] {
            let params = [
                ("declick", 1.0),
                ("one_shot", one_shot),
                ("voice_mode", mode),
                ("glide", 37.0),
                ("filter_type", 1.0),
                ("cutoff", 2_000.0),
                ("filter_env", 0.5),
            ];
            // 4 480 is a multiple of 1, 7, 64 and 128.
            let play = |block: usize| {
                let mut s = configured(&x, &params);
                render_choked(&mut s, 12_000, &events, block, 4_480)
            };
            let reference = play(1);
            let what = format!("mode {mode}, one_shot {one_shot}");
            assert!(peak(&reference.0) > 0.01, "{what}: plays");
            for block in [7, 64, 128] {
                assert!(same_bits(&play(block), &reference), "{what}: block {block}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 1: key ownership, choked priority, never-evicted fades

#[test]
fn a_slide_bends_a_held_one_shot_note() {
    // One-shot mode (the default): key 48 is down, so a slide to 52 bends its voice without a new
    // attack, exactly as in gate mode.
    let mut s = configured(&sine_1k(), &[("root", 48.0), ("attack", 20.0)]);
    let (l, _) = render(&mut s, 24_000, &[on(0, 48, 1.0), slide(9_600, 52, 1.0)]);
    assert_eq!(s.active_voices(), 1, "the slide moved the held voice");
    let track = pitch_track(&l, 48.0, 1000.0);
    let after = key_between(&track, 13_000.0, 23_900.0);
    assert!((after - 52.0).abs() < 0.01, "slid to {after}");
    let held = peak(&l[8_000..9_600]);
    for (i, w) in l[9_600..24_000].chunks(240).enumerate() {
        assert!(peak(w) >= 0.9 * held, "dip at {}", 9_600 + i * 240);
    }
    // Once 48 is up, a slide has nothing held: it starts a note of its own.
    let (_, _) = render(&mut s, 4_800, &[off(0, 52), slide(2_400, 55, 1.0)]);
    assert_eq!(s.active_voices(), 2, "a released one-shot is not bent");
}

#[test]
fn a_full_mono_stack_keeps_every_note_off_owed() {
    // 33 overlapping notes on one key: more than the priority stack holds. The oldest 32
    // note-offs end the oldest 32 notes; the 33rd plays on until its own.
    for mode in [1.0, 2.0] {
        let mut s = configured(
            &dc(),
            &[("one_shot", 0.0), ("release", 1.0), ("voice_mode", mode)],
        );
        let mut events: Vec<NoteEvent> = (0..33).map(|i| on(i * 10, 60, 1.0)).collect();
        events.extend((0..32).map(|i| off(1_000 + i * 10, 60)));
        let (l, _) = render(&mut s, 4_800, &events);
        assert!(
            (l[4_799] - 1.0).abs() < 1e-6,
            "mode {mode}: the 33rd note sounds: {}",
            l[4_799]
        );
        let (l, _) = render(&mut s, 4_800, &[off(0, 60)]);
        assert!(
            peak(&l[480..]) < 1e-3,
            "mode {mode}: its own note-off ends it"
        );
        assert_eq!(s.active_voices(), 0, "mode {mode}");
    }
}

#[test]
fn a_note_off_finds_its_note_after_the_fade_slots_fill() {
    // 16 held notes, then 32 steals one frame apart fill every fade slot. The first stolen note
    // (key 20) still owns its note-off: when it arrives after a new key-20 note started, the new
    // note plays on. (Every key here plays the DC sample for longer than the test runs.)
    let mut s = configured(&dc(), &[("one_shot", 0.0), ("release", 1.0)]);
    let mut events: Vec<NoteEvent> = (0..16).map(|i| on(i, 20 + i as u8, 1.0)).collect();
    events.extend((0..32).map(|i| on(960 + i, 40 + i as u8, 1.0)));
    render(&mut s, 4_800, &events);
    let (l, _) = render(&mut s, 4_800, &[on(0, 20, 0.5), off(100, 20)]);
    let level = l[4_799];
    // Whatever else sounds, the new key-40 note (0.5) is part of it: the late note-off left it.
    let (l2, _) = render(&mut s, 4_800, &[off(0, 20)]);
    assert!(
        (level - l2[4_799] - 0.5).abs() < 1e-6,
        "the new note sounded until its own note-off: {level} then {}",
        l2[4_799]
    );
}

#[test]
fn a_choke_retires_held_keys_from_priority() {
    // Mono and legato: 48 is down when the channel is choked. A new note 60 then plays alone:
    // releasing it ends the note instead of returning to the choked 48.
    for mode in [1.0, 2.0] {
        let mut s = configured(
            &dc(),
            &[("one_shot", 0.0), ("release", 1.0), ("voice_mode", mode)],
        );
        render(&mut s, 4_800, &[on(0, 48, 1.0)]);
        s.choke();
        let (l, _) = render(&mut s, 9_600, &[on(0, 60, 1.0), off(4_800, 60)]);
        assert!(
            peak(&l[4_800 + 480..]) < 1e-3,
            "mode {mode}: 60's note-off ends it"
        );
        assert_eq!(s.active_voices(), 0, "mode {mode}");
        // 48's own note-off, late, finds nothing to end.
        render(&mut s, 480, &[off(0, 48)]);
        assert_eq!(s.active_voices(), 0, "mode {mode}");
    }
}

/// 16 sustained DC voices with 10 ms attacks (level 16), then `steals` note-ons one frame
/// apart from frame 4 800.
fn saturated(steals: u32) -> (Sampler, Vec<NoteEvent>) {
    let s = configured(&dc(), &[("one_shot", 0.0), ("attack", 10.0)]);
    let mut events: Vec<NoteEvent> = (0..16).map(|i| on(i, 20 + i as u8, 1.0)).collect();
    events.extend((0..steals).map(|i| on(4_800 + i, 40 + (i % 32) as u8, 1.0)));
    (s, events)
}

#[test]
fn a_choke_during_saturated_stealing_never_cuts_a_fade() {
    // All 16 voices stolen at once (or twice, filling every fade slot), then a choke 48 frames
    // later: up to 48 voices fade together, the last 16 where they are. No fade is cut short:
    // no frame moves by more than all those full-scale fades do together.
    for steals in [16u32, 32] {
        let (mut s, mut events) = saturated(0);
        events.extend((0..steals).map(|i| on(4_800, 40 + i as u8, 1.0)));
        let (mut l, _) = render(&mut s, 4_848, &events);
        s.choke();
        let (rest, _) = render(&mut s, FADE, &[]);
        l.extend(rest);
        assert_eq!(
            s.active_voices(),
            0,
            "{steals} steals: every voice freed after 3 ms"
        );
        let limit = (16 + steals) as f32 / FADE as f32;
        let step = max_step(&l[4_700..]);
        assert!(step <= limit, "{steals} steals: step {step} above {limit}");
        assert!(
            l[l.len() - 1].abs() < 1e-6,
            "{steals} steals: silent after the fades"
        );
    }
}

#[test]
fn rapid_steals_never_cut_a_fade() {
    // 48 steals one frame apart: more fades than there are fade slots. None is cut short.
    let (mut s, events) = saturated(48);
    let (l, _) = render(&mut s, 9_600, &events);
    let limit = 0.3;
    let step = max_step(&l[4_700..]);
    assert!(step <= limit, "step {step} above {limit}");
    assert_eq!(s.active_voices(), VOICES);
}
