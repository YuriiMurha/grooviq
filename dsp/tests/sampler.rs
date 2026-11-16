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
    let expected: [(&str, f32, f32, f32); 27] = [
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
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
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
    for seg in x.chunks_exact(SEG) {
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
        assert!(play(block) == reference, "block {block}");
    }
}
