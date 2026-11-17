//! The synth's choke, and its slide timing against the sampler's. Every render is offline.

use grooviq_dsp::sampler::{self, SampleBuf, Sampler};
use grooviq_dsp::synth::{self, Synth};
use grooviq_dsp::{Device, NoteEvent};
use std::sync::Arc;

const SR: f32 = 48_000.0;
const BLOCK: usize = 64;
/// Frames a choke fade lasts (3 ms).
const FADE: usize = SR as usize * 3 / 1000;

fn index(ids: &[&str], name: &str) -> u32 {
    ids.iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no parameter {name}")) as u32
}

fn on(offset: u32, key: u8, slide: bool) -> NoteEvent {
    NoteEvent {
        offset,
        key,
        velocity: 1.0,
        on: true,
        slide,
        id: u32::from(key),
    }
}

/// A plain sine: oscillator 1 only, shape 0, the filter wide open and unmodulated.
fn sine_synth() -> Synth {
    let mut s = Synth::new(SR);
    for (name, value) in [
        ("osc1_shape", 0.0),
        ("osc1_level", 1.0),
        ("osc2_level", 0.0),
        ("sub_level", 0.0),
        ("noise_level", 0.0),
        ("filter_cutoff", 20_000.0),
        ("filter_res", 0.0),
        ("filter_env_amt", 0.0),
        ("filter_keytrack", 0.0),
        ("amp_attack", 0.5),
        ("amp_sustain", 1.0),
        ("lfo_amount", 0.0),
    ] {
        s.set_param(index(&synth::PARAM_IDS, name), value);
    }
    s
}

/// Renders `frames` frames (left channel) in BLOCK-frame blocks; `events` carry absolute
/// offsets.
fn render(d: &mut dyn Device, frames: usize, events: &[NoteEvent]) -> Vec<f32> {
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
        d.process(&mut l[start..end], &mut r[start..end], None, &block);
        start = end;
    }
    l
}

fn voices(s: &Synth) -> f32 {
    s.meter(2)
}

#[test]
fn synth_choke_fades_every_voice_in_3_ms() {
    let chord = [on(0, 48, false), on(10, 55, false), on(20, 60, false)];
    // Twins: one choked, one not. Every voice fades with the same straight line, so the choked
    // output is the other one scaled by it.
    let mut choked = sine_synth();
    let mut free = sine_synth();
    render(&mut choked, 4_800, &chord);
    render(&mut free, 4_800, &chord);
    assert_eq!(voices(&choked), 3.0);
    choked.choke();
    let a = render(&mut choked, FADE + 1, &[]);
    let b = render(&mut free, FADE + 1, &[]);
    assert!(b.iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.1);
    for k in 0..FADE {
        let gain = 1.0 - (k + 1) as f32 / FADE as f32;
        assert!(
            (a[k] - b[k] * gain).abs() < 1e-5,
            "frame {k}: {} vs {}",
            a[k],
            b[k] * gain
        );
    }
    assert!(a[FADE..].iter().all(|v| v.abs() < 1e-6));
    assert_eq!(voices(&choked), 0.0, "every voice freed");

    // A note at the next frame plays as on a fresh synth; the choked voices only add their fade.
    let mut s = sine_synth();
    render(&mut s, 4_800, &chord);
    s.choke();
    let with_note = render(&mut s, 4_800, &[on(0, 64, false)]);
    let mut fresh = sine_synth();
    let alone = render(&mut fresh, 4_800, &[on(0, 64, false)]);
    for k in FADE..4_800 {
        assert!(
            (with_note[k] - alone[k]).abs() < 1e-6,
            "frame {k}: {} vs {}",
            with_note[k],
            alone[k]
        );
    }
}

/// Pitch over time from rising zero crossings: `(frame at the period's centre, key)`.
fn pitch_track(x: &[f32]) -> Vec<(f64, f64)> {
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
            ((w[0] + w[1]) / 2.0, 69.0 + 12.0 * libm::log2(hz / 440.0))
        })
        .collect()
}

/// The frame at which a straight line fitted through the track points whose key lies in
/// `lo..hi` reaches key `at`.
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
    mt + (at - mk) / (cov / var)
}

#[test]
fn the_sampler_and_synth_agree_on_slide_timing() {
    // Key 72 held, slide 79 at frame 9 600: both glide over the default 60 ms and reach the
    // target at the same frame.
    let events = [on(0, 72, false), on(9_600, 79, true)];
    let mut synth = sine_synth();
    let a = render(&mut synth, 20_000, &events);

    let len = 2 * SR as usize;
    let x: Vec<f32> = (0..len)
        .map(|i| libm::sin(2.0 * std::f64::consts::PI * 440.0 * i as f64 / SR as f64) as f32)
        .collect();
    let mut s = Sampler::new(SR);
    s.set_sample(Some(Arc::new(SampleBuf {
        rate: SR,
        r: x.clone(),
        l: x,
    })));
    s.set_param(index(&sampler::PARAM_IDS, "root"), 69.0);
    s.set_param(index(&sampler::PARAM_IDS, "one_shot"), 0.0);
    let b = render(&mut s, 20_000, &events);

    let synth_end = line_reaches(&pitch_track(&a), 73.0, 78.0, 79.0);
    let sampler_end = line_reaches(&pitch_track(&b), 73.0, 78.0, 79.0);
    let expected = 9_600.0 + 0.060 * SR as f64;
    assert!(
        (synth_end - expected).abs() < 10.0,
        "the synth arrives at {synth_end}"
    );
    assert!(
        (sampler_end - synth_end).abs() <= 1.0,
        "sampler {sampler_end}, synth {synth_end}"
    );
}

fn off(offset: u32, key: u8) -> NoteEvent {
    NoteEvent {
        offset,
        key,
        velocity: 0.0,
        on: false,
        slide: false,
        id: u32::from(key),
    }
}

/// A note-on and note-off that name note `id` (the helpers above name each key's one note).
fn on_n(offset: u32, key: u8, id: u32) -> NoteEvent {
    NoteEvent {
        id,
        ..on(offset, key, false)
    }
}

fn off_n(offset: u32, key: u8, id: u32) -> NoteEvent {
    NoteEvent {
        id,
        ..off(offset, key)
    }
}

fn sine_synth_in(mode: f32) -> Synth {
    let mut s = sine_synth();
    s.set_param(index(&synth::PARAM_IDS, "voice_mode"), mode);
    s
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn synth_notes_after_a_choke_start_on_their_frame() {
    // A note right after a choke sounds from its own frame, as on a fresh synth: in poly mode
    // with all 16 voices sounding, and in mono and legato modes (slot 0 just choked).
    for (mode, notes) in [(0.0, 16u8), (1.0, 1), (2.0, 1)] {
        let chord: Vec<NoteEvent> = (0..notes).map(|i| on(i as u32, 40 + i, false)).collect();
        let mut s = sine_synth_in(mode);
        render(&mut s, 4_800, &chord);
        assert_eq!(voices(&s), notes as f32, "mode {mode}");
        s.choke();
        let with_note = render(&mut s, 4_800, &[on(0, 72, false)]);
        let mut fresh = sine_synth_in(mode);
        let alone = render(&mut fresh, 4_800, &[on(0, 72, false)]);
        for k in FADE..4_800 {
            assert!(
                (with_note[k] - alone[k]).abs() < 1e-6,
                "mode {mode}, frame {k}: {} vs {}",
                with_note[k],
                alone[k]
            );
        }
    }
}

#[test]
fn synth_choke_retires_held_keys_from_priority() {
    // Mono and legato: 48 is down when the synth is choked. A new 60 then plays alone: its
    // note-off releases it instead of returning to the choked 48.
    for mode in [1.0, 2.0] {
        let mut s = sine_synth_in(mode);
        render(&mut s, 4_800, &[on(0, 48, false)]);
        s.choke();
        render(&mut s, 48_000, &[on(0, 60, false), off(4_800, 60)]);
        assert_eq!(voices(&s), 0.0, "mode {mode}: released");
    }
}

#[test]
fn synth_a_late_note_off_after_a_choke_leaves_the_newer_note_alone() {
    // A choked note's note-off arrives after a new note on its key started. It belongs to the
    // choked note, so the new one plays on until its own.
    for mode in [0.0, 1.0, 2.0] {
        let mut s = sine_synth_in(mode);
        render(&mut s, 4_800, &[on_n(0, 48, 1)]);
        s.choke();
        let l = render(&mut s, 48_000, &[on_n(0, 48, 2), off_n(100, 48, 1)]);
        assert!(
            peak(&l[47_000..]) > 0.1,
            "mode {mode}: the newer note sounds"
        );
        let l = render(&mut s, 48_000, &[off_n(0, 48, 2)]);
        assert!(
            peak(&l[47_000..]) < 1e-4,
            "mode {mode}: its own note-off ends it"
        );
    }
}

#[test]
fn synth_a_re_struck_key_keeps_its_priority() {
    // Mono and legato: 48, 55, then 48 struck again while the first 48 is still down. The first
    // 48's note-off belongs to that note: the new 48 keeps sounding and keeps its place in the
    // priority stack (a 60 played and released returns to it), and releasing 55 and then the new
    // 48 ends the note.
    for mode in [1.0, 2.0] {
        let mut s = sine_synth_in(mode);
        let events = [
            on_n(0, 48, 1),
            on_n(4_800, 55, 2),
            on_n(9_600, 48, 3),
            off_n(14_400, 48, 1),
            on_n(19_200, 60, 4),
            off_n(24_000, 60, 4),
            off_n(28_800, 55, 2),
            off_n(33_600, 48, 3),
        ];
        let l = render(&mut s, 81_600, &events);
        let track = pitch_track(&l[..33_600]);
        for (from, to) in [(15_000.0, 19_000.0), (24_600.0, 28_600.0)] {
            let keys: Vec<f64> = track
                .iter()
                .filter(|(t, _)| (from..to).contains(t))
                .map(|(_, k)| *k)
                .collect();
            assert!(keys.len() > 10);
            assert!(
                keys.iter().all(|k| (k - 48.0).abs() < 0.05),
                "mode {mode}: 48 between {from} and {to}: {keys:?}"
            );
        }
        assert_eq!(voices(&s), 0.0, "mode {mode}: released");
    }
}

/// Chords of 16 notes (keys 40..55), each choked `gap` frames after it starts; the first
/// sustains 4 800 frames.
fn choked_synth_chords(chords: usize, gap: usize) -> Synth {
    let mut s = sine_synth();
    let chord: Vec<NoteEvent> = (0..16).map(|i| on(0, 40 + i, false)).collect();
    render(&mut s, 4_800, &chord);
    s.choke();
    for _ in 1..chords {
        render(&mut s, gap, &chord);
        s.choke();
    }
    s
}

#[test]
fn synth_three_chokes_do_not_delay_the_next_note() {
    // Three chords choked one frame apart: the note right after the last choke sounds from its
    // own frame, as on a fresh synth.
    let mut s = choked_synth_chords(3, 1);
    let with_note = render(&mut s, 4_800, &[on(0, 72, false)]);
    let mut fresh = sine_synth();
    let alone = render(&mut fresh, 4_800, &[on(0, 72, false)]);
    assert!(alone[FADE].abs() > 0.01);
    for k in FADE..4_800 {
        assert!(
            (with_note[k] - alone[k]).abs() < 1e-6,
            "frame {k}: {} vs {}",
            with_note[k],
            alone[k]
        );
    }
}

#[test]
fn synth_a_full_fade_pool_replaces_only_its_quietest_fades() {
    // Five chords choked 10 frames apart: the first four fill all 64 fade slots, so the fifth
    // replaces the first chord's fades (the nearest their end). Just after the first chord's
    // fades would have ended, chords 2 to 5 still fade (64) beside the new note; and the new
    // note sounds as on a fresh synth.
    let mut s = choked_synth_chords(5, 10);
    let with_note = render(&mut s, FADE + 5 - 40, &[on(0, 72, false)]);
    assert_eq!(
        voices(&s),
        65.0,
        "the first chord's fades were the ones replaced"
    );
    let rest = render(&mut s, 4_800, &[]);
    let mut fresh = sine_synth();
    let alone = render(&mut fresh, FADE + 5 - 40 + 4_800, &[on(0, 72, false)]);
    let all: Vec<f32> = with_note.into_iter().chain(rest).collect();
    for k in FADE..all.len() {
        assert!((all[k] - alone[k]).abs() < 1e-6, "frame {k}");
    }
}
