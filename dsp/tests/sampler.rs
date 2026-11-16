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
        .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin() as f32)
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
    1200.0 * (measured / expected).log2()
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
    let expected: [(&str, f32, f32, f32); 15] = [
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
    ];
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
        let expected = 440.0 * 2f64.powf(semis / 12.0);
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
    let step = 2f64.powf(7.0 / 12.0);
    let err = (10..20_000)
        .map(|i| {
            let ideal = (2.0 * std::f64::consts::PI * 2_000.0 * i as f64 * step / SR as f64).sin();
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
        20.0 * level.max(1e-12).log10() < -60.0,
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
    assert!(20.0 * level.max(1e-12).log10() < -60.0, "{level}");
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
    let next = buf(SR, noise(10_000));
    let mut l = vec![0.0; BLOCK];
    let mut r = vec![0.0; BLOCK];
    let events = [on(0, 60, 0.9), on(5, 72, 0.5), off(40, 60), on(63, 48, 1.0)];
    let mut swapped = None;
    for i in 0..1000u32 {
        let incoming = (i == 500).then(|| Arc::clone(&next));
        let old = assert_no_alloc(|| {
            s.set_param(i % 15, (i % 7) as f32 * 300.0 - 600.0);
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
