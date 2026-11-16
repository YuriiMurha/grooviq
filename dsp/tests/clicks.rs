//! Click tests for the Basic Synth.
//!
//! A click is a short broadband burst that stands out from the sound around it. The detector
//! high-passes the output (4th-order Butterworth at 5 kHz), measures the energy in 1 ms windows
//! and compares each window with the loudest window in the surrounding context (4..25 ms before
//! and after). Comparing against the loudest neighbour (not the average) keeps impulsive but
//! periodic timbres (a low saw has a jump every cycle) from reading as clicks, and comparing
//! against the context after the window keeps a filter envelope opening on a pluck from reading
//! as one. A floor relative to the loudest 10 ms of the render ignores bursts too quiet to hear.
//!
//! Calibration (sines, see `detector_is_calibrated`): a hard step reads about +41 dB, a 1 ms
//! linear fade about +9 dB, 2 ms about +3 dB, 3 ms or longer 0 dB or less.
//!
//! Pass criterion: every burst at or below LIMIT_DB (+6 dB, just above a 2 ms fade). The one
//! exception is the attack of a fresh note: Pluck and Bell Keys have an intentionally sharp
//! 0.5 ms attack (the "knock") that reads up to about +8 dB. A scenario may therefore reach the
//! loudest fresh attack of its own notes (each played alone from silence) plus KNOCK_MARGIN_DB,
//! and a fresh attack itself must stay within KNOCK_MAX_DB.
//!
//! To print the worst burst of every scenario:
//! `CLICKS_VERBOSE=1 cargo test --release -p grooviq-dsp --test clicks -- --nocapture --test-threads=1`

use std::collections::HashMap;

use grooviq_dsp::synth::PRESETS;
use grooviq_dsp::{Device, NoteEvent, create};

const SR: f32 = 48_000.0;

// ---------------------------------------------------------------------------------------------
// Detector

const HP_HZ: f64 = 5000.0;
const WIN_MS: f32 = 1.0;
const HOP_MS: f32 = 0.25;
const NEAR_MS: f32 = 4.0;
const FAR_MS: f32 = 25.0;
/// Bursts below this level (relative to the loudest 10 ms of the render) are inaudible.
const FLOOR_DB: f64 = -60.0;

#[derive(Clone, Copy, Debug)]
struct Click {
    /// Sample index of the window centre.
    at: usize,
    /// Channel (0 = left, 1 = right).
    ch: usize,
    /// How far the window's high-band energy exceeds max(context, floor), in dB.
    score_db: f32,
    /// High-band energy of the window relative to the loudest 10 ms of the render, in dB.
    level_db: f32,
}

fn highpass4(x: &[f32], sr: f32) -> Vec<f64> {
    let mut y: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    for q in [0.541_196_100_146_197, 1.306_562_964_876_376_6] {
        let w0 = 2.0 * std::f64::consts::PI * HP_HZ / sr as f64;
        let (c, s) = (w0.cos(), w0.sin());
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

/// Loudest 10 ms mean power of the render (both channels).
fn reference_power(l: &[f32], r: &[f32], sr: f32) -> f64 {
    let n = l.len();
    let w = ((0.01 * sr) as usize).max(1);
    let mut pre = vec![0.0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + 0.5 * ((l[i] as f64).powi(2) + (r[i] as f64).powi(2));
    }
    let mut best = 1.0e-30f64;
    let mut i = 0;
    while i + w <= n {
        best = best.max((pre[i + w] - pre[i]) / w as f64);
        i += w / 4;
    }
    best
}

/// Every window whose score exceeds `min_score_db`, strongest per 5 ms neighbourhood.
fn scan(l: &[f32], r: &[f32], sr: f32, min_score_db: f32) -> Vec<Click> {
    let n = l.len().min(r.len());
    // (an absolute minimum, -200 dB, so digital silence scores very low instead of -inf or NaN)
    let floor = (reference_power(l, r, sr) * 10f64.powf(FLOOR_DB / 10.0)).max(1e-20);
    let hop = ((HOP_MS * 0.001 * sr).round() as usize).max(1);
    let win = ((WIN_MS * 0.001 * sr).round() as usize).max(2);
    let near = (NEAR_MS / HOP_MS).round() as usize;
    let far = (FAR_MS / HOP_MS).round() as usize;
    let mut out: Vec<Click> = Vec::new();
    for (ch, x) in [&l[..n], &r[..n]].into_iter().enumerate() {
        let hp = highpass4(x, sr);
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
            let score = (10.0 * ((w[j] + 1e-30) / (ctx + floor)).log10()) as f32;
            if score > min_score_db {
                let c = Click {
                    at: centres[j],
                    ch,
                    score_db: score,
                    level_db: (10.0 * ((w[j] + 1e-30) / (floor * 1e6)).log10()) as f32,
                };
                match out.last_mut() {
                    Some(p) if p.ch == ch && c.at - p.at < (0.005 * sr) as usize => {
                        if c.score_db > p.score_db {
                            *p = c;
                        }
                    }
                    _ => out.push(c),
                }
            }
        }
    }
    out
}

/// Strongest burst whose window centre lies in `from..to` (samples).
fn worst_in(l: &[f32], r: &[f32], from: usize, to: usize) -> Option<Click> {
    scan(l, r, SR, f32::NEG_INFINITY)
        .into_iter()
        .filter(|c| c.at >= from && c.at < to)
        .max_by(|a, b| a.score_db.total_cmp(&b.score_db))
}

fn describe(c: &[Click]) -> String {
    c.iter()
        .take(4)
        .map(|c| {
            format!(
                "{:.1} ms ch{} {:+.1} dB (burst {:.1} dB re loudest)",
                c.at as f32 * 1000.0 / SR,
                c.ch,
                c.score_db,
                c.level_db
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

// ---------------------------------------------------------------------------------------------
// Scenario rendering

#[derive(Clone, Copy, Debug)]
enum Act {
    On(u8, f32),
    Off(u8),
    Slide(u8, f32),
    Param(u32, f32),
}

/// Renders `n` frames. Note events land on their exact sample; parameter changes apply before
/// the block that starts at their time (blocks are split there).
fn render_with(
    dev: &mut dyn Device,
    acts: &[(usize, Act)],
    n: usize,
    block: usize,
) -> (Vec<f32>, Vec<f32>) {
    let mut params: Vec<(usize, u32, f32)> = acts
        .iter()
        .filter_map(|&(t, a)| {
            if let Act::Param(p, v) = a {
                Some((t, p, v))
            } else {
                None
            }
        })
        .collect();
    params.sort_by_key(|p| p.0);
    let mut notes: Vec<(usize, NoteEvent)> = acts
        .iter()
        .filter_map(|&(t, a)| {
            let ev = |key, velocity, on, slide| {
                Some((
                    t,
                    NoteEvent {
                        offset: 0,
                        key,
                        velocity,
                        on,
                        slide,
                    },
                ))
            };
            match a {
                Act::On(k, v) => ev(k, v, true, false),
                Act::Slide(k, v) => ev(k, v, true, true),
                Act::Off(k) => ev(k, 0.0, false, false),
                Act::Param(..) => None,
            }
        })
        .collect();
    notes.sort_by_key(|e| e.0);
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    let (mut i, mut pi) = (0, 0);
    while i < n {
        while pi < params.len() && params[pi].0 <= i {
            dev.set_param(params[pi].1, params[pi].2);
            pi += 1;
        }
        let mut e = (i + block).min(n);
        if pi < params.len() {
            e = e.min(params[pi].0);
        }
        let ev: Vec<NoteEvent> = notes
            .iter()
            .filter(|(t, _)| *t >= i && *t < e)
            .map(|(t, x)| NoteEvent {
                offset: (*t - i) as u32,
                ..*x
            })
            .collect();
        dev.process(&mut l[i..e], &mut r[i..e], None, &ev);
        i = e;
    }
    (l, r)
}

fn synth_with(params: &[(u32, f32)]) -> Box<dyn Device> {
    let mut s = create("synth", SR).unwrap();
    for &(p, v) in params {
        s.set_param(p, v);
    }
    s
}

fn preset(name: &str) -> Vec<(u32, f32)> {
    grooviq_dsp::synth::preset(name).expect(name)
}

fn ms(t: f32) -> usize {
    (t * 0.001 * SR) as usize
}

const MODES: [(&str, f32); 3] = [("poly", 0.0), ("mono", 1.0), ("legato", 2.0)];
const VELS: [f32; 4] = [1.0, 0.45, 0.8, 0.25];

// parameter indices used by the scenarios
const OSC1_UNI: usize = 5;
const SUB_LEVEL: usize = 16;
const CUTOFF: usize = 20;
const RES: usize = 21;
const ENV_AMT: usize = 23;
const KEYTRACK: usize = 24;
const AMP_S: usize = 27;
const AMP_R: usize = 28;
const FLT_S: usize = 31;
const LFO_RATE: usize = 34;
const LFO_AMOUNT: usize = 35;
const LFO_TARGET: usize = 36;
const LFO_SHAPE: usize = 33;
const SUB_SHAPE: usize = 17;
const VOICE_MODE: usize = 38;

fn with_mode(name: &str, mode: f32) -> Vec<(u32, f32)> {
    let mut p = preset(name);
    p[VOICE_MODE].1 = mode;
    p
}

fn set(p: &mut [(u32, f32)], i: usize, v: f32) {
    p[i].1 = v;
}

/// A bright, sine-based patch: steps in the waveform are not hidden by a filter or harmonics.
fn probe() -> Vec<(u32, f32)> {
    let mut p = preset("Init");
    for (i, v) in [
        (0usize, 0.0f32),
        (4, 0.6),
        (8, 0.0),
        (9, 1.0),
        (12, 0.3),
        (19, 0.0),
        (20, 20000.0),
        (21, 0.0),
        (23, 0.0),
        (24, 0.0),
        (25, 5.0),
        (27, 1.0),
        (28, 300.0),
        (39, -6.0),
    ] {
        set(&mut p, i, v);
    }
    p
}

// ---------------------------------------------------------------------------------------------
// Pass criterion

/// Worst allowed burst over its context (a 2 ms linear fade reads about +3 dB, 1 ms +9 dB).
const LIMIT_DB: f32 = 6.0;
/// A scenario may reach the loudest fresh-note attack of its own notes plus this margin.
const KNOCK_MARGIN_DB: f32 = 1.0;
/// A fresh note's attack (the intended "knock" of plucky presets) must stay within this. Pluck's
/// knock reads up to +9.5 dB over the keys these scenarios play (36..82); Bell Keys up to +7.7.
const KNOCK_MAX_DB: f32 = 10.0;

/// Renders scenarios, checks them against the criterion and collects every failure, so one run
/// reports all of them.
struct Checker {
    family: &'static str,
    /// Fresh-attack score per (params without the voice mode, key, velocity).
    knocks: HashMap<(Vec<u32>, u8, u32), f32>,
    failures: Vec<String>,
    count: usize,
    worst: (f32, String),
    verbose: bool,
}

impl Checker {
    fn new(family: &'static str) -> Self {
        Self {
            family,
            knocks: HashMap::new(),
            failures: Vec::new(),
            count: 0,
            worst: (f32::NEG_INFINITY, String::new()),
            verbose: std::env::var_os("CLICKS_VERBOSE").is_some(),
        }
    }

    /// Loudest burst in the first 40 ms of a note played alone from silence.
    fn knock(&mut self, params: &[(u32, f32)], key: u8, vel: f32) -> f32 {
        // the first note is a fresh start in every voice mode
        let id: Vec<u32> = params
            .iter()
            .filter(|p| p.0 as usize != VOICE_MODE)
            .map(|p| p.1.to_bits())
            .collect();
        *self
            .knocks
            .entry((id, key, vel.to_bits()))
            .or_insert_with(|| {
                let mut s = synth_with(params);
                let (l, r) =
                    render_with(s.as_mut(), &[(ms(20.0), Act::On(key, vel))], ms(150.0), 128);
                worst_in(&l, &r, ms(18.0), ms(60.0)).map_or(f32::NEG_INFINITY, |c| c.score_db)
            })
    }

    /// Renders `acts` over `len` samples and checks every burst from sample `from` on. With
    /// `knock`, the limit rises to the loudest fresh attack of the scenario's notes.
    fn check(
        &mut self,
        tag: &str,
        params: &[(u32, f32)],
        acts: &[(usize, Act)],
        len: usize,
        from: usize,
        knock: bool,
    ) {
        self.count += 1;
        let mut limit = LIMIT_DB;
        if knock {
            let mut seen: Vec<(u8, u32)> = Vec::new();
            let mut k = f32::NEG_INFINITY;
            for &(_, a) in acts {
                if let Act::On(key, vel) | Act::Slide(key, vel) = a
                    && !seen.contains(&(key, vel.to_bits()))
                {
                    seen.push((key, vel.to_bits()));
                    k = k.max(self.knock(params, key, vel));
                }
            }
            if k > KNOCK_MAX_DB {
                self.failures.push(format!("{tag}: a fresh note's attack reads {k:+.1} dB (knock budget {KNOCK_MAX_DB:+.1} dB)"));
            }
            limit = limit.max(k + KNOCK_MARGIN_DB);
        }
        let mut s = synth_with(params);
        let (l, r) = render_with(s.as_mut(), acts, len, 128);
        assert!(
            l.iter().chain(&r).all(|v| v.is_finite()),
            "{tag}: non-finite output"
        );
        let Some(w) = worst_in(&l, &r, from, len) else {
            return;
        };
        if self.verbose {
            println!(
                "{:14} {tag:52} worst {:+6.1} dB at {:7.1} ms (limit {:+.1})",
                self.family,
                w.score_db,
                w.at as f32 * 1000.0 / SR,
                limit
            );
        }
        if w.score_db > self.worst.0 {
            self.worst = (
                w.score_db,
                format!("{tag} at {:.1} ms", w.at as f32 * 1000.0 / SR),
            );
        }
        if w.score_db > limit {
            let over: Vec<Click> = scan(&l, &r, SR, limit)
                .into_iter()
                .filter(|c| c.at >= from)
                .collect();
            self.failures
                .push(format!("{tag}: {} (limit {limit:+.1} dB)", describe(&over)));
        }
    }

    fn finish(self) {
        println!(
            "{}: {} scenarios, worst {:+.1} dB ({})",
            self.family, self.count, self.worst.0, self.worst.1
        );
        assert!(
            self.failures.is_empty(),
            "{}: {} of {} scenarios click:\n  {}",
            self.family,
            self.failures.len(),
            self.count,
            self.failures.join("\n  ")
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Detector calibration

/// A sine with a smooth 20 ms fade-in (so the onset itself is clean) times `amp(i)`.
fn sine_with(f: f32, n: usize, amp: impl Fn(usize) -> f32) -> (Vec<f32>, Vec<f32>) {
    let fade = ms(20.0);
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let w = if i < fade {
                0.5 - 0.5 * (std::f32::consts::PI * i as f32 / fade as f32).cos()
            } else {
                1.0
            };
            w * amp(i) * (std::f32::consts::TAU * f * i as f32 / SR).sin()
        })
        .collect();
    (l.clone(), l)
}

/// First sample at or after `from` where a sine of `f` Hz is at `deg` degrees of its cycle.
fn at_phase(f: f32, from: usize, deg: f32) -> usize {
    let period = SR / f;
    let ph = (from as f32 / period).fract();
    let want = deg / 360.0;
    from + (((want - ph).rem_euclid(1.0)) * period) as usize
}

#[test]
fn detector_is_calibrated() {
    let n = ms(600.0);
    for f in [41.2f32, 110.0, 440.0, 1000.0] {
        // cut 60 degrees into a cycle after 300 ms
        let cut = at_phase(f, ms(300.0), 60.0);
        let fade = |len_ms: f32| {
            let len = ms(len_ms).max(1);
            let (l, r) = sine_with(f, n, |i| {
                if i < cut {
                    0.5
                } else if i < cut + len {
                    0.5 * (1.0 - (i - cut) as f32 / len as f32)
                } else {
                    0.0
                }
            });
            worst_in(&l, &r, 0, n).unwrap().score_db
        };
        let (step, f1, f2, f3, f10) = (fade(0.0), fade(1.0), fade(2.0), fade(3.0), fade(10.0));
        println!(
            "{f:7.1} Hz: step {step:+.1}, 1 ms {f1:+.1}, 2 ms {f2:+.1}, 3 ms {f3:+.1}, 10 ms {f10:+.1} dB"
        );
        assert!(step > 30.0, "{f} Hz: a hard step reads only {step:+.1} dB");
        assert!(
            f1 > LIMIT_DB - 0.5,
            "{f} Hz: a 1 ms fade reads only {f1:+.1} dB"
        );
        assert!(
            f2 <= LIMIT_DB - 1.0,
            "{f} Hz: a 2 ms fade reads {f2:+.1} dB"
        );
        assert!(
            f3 <= 1.5 && f10 <= 0.0,
            "{f} Hz: slow fades read {f3:+.1} / {f10:+.1} dB"
        );
        // a -6 dB gain step is a click, the same change over 3 ms is not
        let (l, r) = sine_with(f, n, |i| if i < cut { 0.5 } else { 0.25 });
        assert!(worst_in(&l, &r, 0, n).unwrap().score_db > 25.0);
        let (l, r) = sine_with(f, n, |i| {
            if i < cut {
                0.5
            } else {
                0.5 - 0.25 * ((i - cut) as f32 / ms(3.0) as f32).min(1.0)
            }
        });
        assert!(worst_in(&l, &r, 0, n).unwrap().score_db < 0.0);
    }
    // digital silence does not break the detector
    let z = vec![0.0f32; ms(100.0)];
    assert!(worst_in(&z, &z, 0, z.len()).unwrap().score_db < -90.0);
}

// ---------------------------------------------------------------------------------------------
// Playing

/// Striking a key again while its note still sounds (released and ringing, or still held),
/// at equal and very different velocities; and late, when the tail is nearly silent.
#[test]
fn repress_while_ringing_is_click_free() {
    let mut c = Checker::new("re-press");
    for name in PRESETS {
        for (mname, mode) in MODES {
            let p = with_mode(name, mode);
            for (v1, v2) in [(1.0f32, 1.0f32), (1.0, 0.3), (0.3, 1.0)] {
                let acts = [
                    (ms(20.0), Act::On(48, v1)),
                    (ms(400.0), Act::Off(48)),
                    (ms(450.0), Act::On(48, v2)),
                    (ms(900.0), Act::Off(48)),
                ];
                c.check(
                    &format!("{name}/{mname} ringing {v1}->{v2}"),
                    &p,
                    &acts,
                    ms(1500.0),
                    0,
                    true,
                );
                let acts = [
                    (ms(20.0), Act::On(72, v1)),
                    (ms(450.0), Act::On(72, v2)),
                    (ms(900.0), Act::Off(72)),
                ];
                c.check(
                    &format!("{name}/{mname} held {v1}->{v2}"),
                    &p,
                    &acts,
                    ms(1500.0),
                    0,
                    true,
                );
            }
        }
        // mono: around the point where the tail is quiet enough for a fresh start
        let p = with_mode(name, 1.0);
        let rel = p[AMP_R].1;
        for frac in [0.7f32, 0.85, 1.0] {
            let again = 400.0 + rel * frac;
            let acts = [
                (ms(20.0), Act::On(48, 1.0)),
                (ms(400.0), Act::Off(48)),
                (ms(again), Act::On(48, 1.0)),
                (ms(again + 300.0), Act::Off(48)),
            ];
            c.check(
                &format!("{name}/mono late re-press at {frac} x release"),
                &p,
                &acts,
                ms(again + 900.0),
                0,
                true,
            );
        }
    }
    c.finish();
}

/// The same key repeated fast with varying velocity (each new note lands on a ringing one).
#[test]
fn fast_repeated_notes_are_click_free() {
    let mut c = Checker::new("fast repeats");
    for name in PRESETS {
        for (mname, mode) in MODES {
            let p = with_mode(name, mode);
            for (gate, period, count) in [(50.0f32, 100.0f32, 12usize), (30.0, 50.0, 20)] {
                let mut acts = Vec::new();
                for k in 0..count {
                    let t = 20.0 + k as f32 * period;
                    acts.push((ms(t), Act::On(48, VELS[k % 4])));
                    acts.push((ms(t + gate), Act::Off(48)));
                }
                let len = ms(20.0 + count as f32 * period + 600.0);
                c.check(
                    &format!("{name}/{mname} {gate} ms notes every {period} ms"),
                    &p,
                    &acts,
                    len,
                    0,
                    true,
                );
            }
        }
    }
    c.finish();
}

/// Overlapping notes (legato lines), a held note with others played over it, and slides.
#[test]
fn overlapping_lines_are_click_free() {
    let mut c = Checker::new("overlapping");
    let keys = [48u8, 51, 55, 53, 48, 46, 50, 48];
    for name in PRESETS {
        for (mname, mode) in MODES {
            let p = with_mode(name, mode);
            let mut acts = Vec::new();
            for (k, &key) in keys.iter().enumerate() {
                let t = 20.0 + k as f32 * 150.0;
                acts.push((ms(t), Act::On(key, VELS[k % 4])));
                acts.push((ms(t + 210.0), Act::Off(key)));
            }
            c.check(
                &format!("{name}/{mname} overlapping line"),
                &p,
                &acts,
                ms(2000.0),
                0,
                true,
            );
            let acts = [
                (ms(20.0), Act::On(48, 1.0)),
                (ms(300.0), Act::On(55, 0.3)),
                (ms(600.0), Act::Off(55)),
                (ms(900.0), Act::On(60, 1.0)),
                (ms(1100.0), Act::Off(60)),
                (ms(1300.0), Act::Off(48)),
            ];
            c.check(
                &format!("{name}/{mname} held + others"),
                &p,
                &acts,
                ms(2000.0),
                0,
                true,
            );
            let mut acts = vec![(ms(20.0), Act::On(48, 0.9))];
            for (k, &key) in keys.iter().enumerate().skip(1) {
                acts.push((ms(20.0 + k as f32 * 150.0), Act::Slide(key, VELS[k % 4])));
                acts.push((ms(20.0 + k as f32 * 150.0 + 40.0), Act::Off(keys[k - 1])));
            }
            acts.push((ms(1300.0), Act::Off(48)));
            c.check(
                &format!("{name}/{mname} slide line"),
                &p,
                &acts,
                ms(2000.0),
                0,
                true,
            );
        }
    }
    c.finish();
}

/// More notes than voices: held chords and a fast arpeggio over long release tails.
#[test]
fn voice_stealing_is_click_free() {
    let mut c = Checker::new("stealing");
    for name in PRESETS {
        let p = with_mode(name, 0.0);
        let mut acts = Vec::new();
        for k in 0..24 {
            acts.push((ms(20.0 + k as f32 * 25.0), Act::On(36 + k as u8 * 2, 0.8)));
            acts.push((ms(1200.0 + k as f32 * 10.0), Act::Off(36 + k as u8 * 2)));
        }
        c.check(&format!("{name} 24 held"), &p, &acts, ms(2000.0), 0, true);
        let mut acts = Vec::new();
        for k in 0..40 {
            let key = 48 + [0u8, 3, 7, 10, 12, 15, 19, 22][k % 8] + (k / 8) as u8;
            acts.push((ms(20.0 + k as f32 * 40.0), Act::On(key, VELS[k % 4])));
            acts.push((ms(20.0 + k as f32 * 40.0 + 30.0), Act::Off(key)));
        }
        c.check(
            &format!("{name} fast arpeggio"),
            &p,
            &acts,
            ms(2400.0),
            0,
            true,
        );
    }
    c.finish();
}

// ---------------------------------------------------------------------------------------------
// Knobs

/// (param, label, values applied at 400, 700 and 1000 ms while two notes sound)
const MOVES: &[(u32, &str, [f32; 3])] = &[
    (0, "osc1 shape", [3.0, 0.0, 1.5]),
    (8, "osc2 shape", [0.0, 3.0, 1.0]),
    (5, "osc1 unison", [7.0, 1.0, 4.0]),
    (13, "osc2 unison", [5.0, 1.0, 3.0]),
    (7, "osc1 spread", [1.0, 0.0, 0.5]),
    (6, "osc1 detune", [1.0, 0.0, 0.5]),
    (15, "osc2 spread", [1.0, 0.0, 0.5]),
    (19, "filter type", [1.0, 2.0, 3.0]),
    (19, "filter type b", [2.0, 0.0, 1.0]),
    (19, "filter type c", [3.0, 1.0, 0.0]),
    (20, "cutoff jump", [200.0, 12000.0, 800.0]),
    (21, "res jump", [1.0, 0.0, 0.7]),
    (22, "drive jump", [1.0, 0.0, 0.5]),
    (38, "voice mode", [1.0, 2.0, 0.0]),
    (1, "osc1 octave", [1.0, -2.0, 0.0]),
    (2, "osc1 semi", [7.0, -5.0, 0.0]),
    (3, "osc1 fine", [100.0, -100.0, 0.0]),
    (9, "osc2 octave", [2.0, -3.0, 0.0]),
    (39, "master", [6.0, -36.0, 0.0]),
    (17, "sub shape", [1.0, 0.0, 1.0]),
    (16, "sub level", [1.0, 0.0, 0.5]),
    (36, "lfo target", [3.0, 1.0, 2.0]),
    (36, "lfo target b", [0.0, 3.0, 1.0]),
    (33, "lfo shape", [3.0, 2.0, 4.0]),
    // At 20 Hz a deep cutoff LFO makes every brightness peak 50 ms from the next, outside the
    // detector's 25 ms context, so the peaks themselves read as bursts; 5 Hz still tests the
    // rate jump (which is phase-continuous) without that artefact.
    (34, "lfo rate", [5.0, 0.05, 2.0]),
    (35, "lfo amount", [1.0, 0.0, 0.5]),
    (23, "env amt", [1.0, -1.0, 0.5]),
    (24, "keytrack", [1.0, 0.0, 0.3]),
    (27, "amp sustain", [0.0, 1.0, 0.5]),
    (25, "amp attack", [0.5, 5000.0, 1.0]),
    (4, "osc1 level", [0.0, 1.0, 0.5]),
    (12, "osc2 level", [1.0, 0.0, 0.5]),
    (18, "noise level", [1.0, 0.0, 0.3]),
    (37, "glide", [500.0, 0.0, 100.0]),
];

/// Makes a move audible on `p`: LFO moves need an LFO, the sub shape a sub.
fn prepare(p: &mut [(u32, f32)], param: u32) {
    let i = param as usize;
    if matches!(i, LFO_TARGET | LFO_SHAPE | LFO_RATE) {
        set(p, LFO_AMOUNT, 0.6);
    }
    if i == SUB_SHAPE {
        set(p, SUB_LEVEL, 0.8);
    }
}

fn check_moves(c: &mut Checker, tag: &str, base: &[(u32, f32)], keys: &[u8]) {
    for &(param, label, vals) in MOVES {
        let mut p = base.to_vec();
        prepare(&mut p, param);
        let mut acts = Vec::new();
        for &k in keys {
            acts.push((ms(20.0), Act::On(k, 0.9)));
            acts.push((ms(1300.0), Act::Off(k)));
        }
        for (k, v) in vals.iter().enumerate() {
            acts.push((ms(400.0 + 300.0 * k as f32), Act::Param(param, *v)));
        }
        c.check(
            &format!("{tag}: {label}"),
            &p,
            &acts,
            ms(1700.0),
            ms(350.0),
            false,
        );
    }
}

/// Every knob moved in jumps while notes sound, on every preset (sustain raised so the plucks
/// still sound when the knobs move) and on bare sine patches where nothing hides a step.
#[test]
fn knob_moves_while_sounding_are_click_free() {
    let mut c = Checker::new("knobs");
    for name in PRESETS {
        let mut p = with_mode(name, 0.0);
        if p[AMP_S].1 < 0.3 {
            set(&mut p, AMP_S, 0.6);
        }
        check_moves(&mut c, name, &p, &[48, 55]);
    }
    let mut res = probe();
    for (i, v) in [(CUTOFF, 3000.0), (RES, 0.6), (ENV_AMT, 0.5), (FLT_S, 0.5)] {
        set(&mut res, i, v);
    }
    let mut dark = probe();
    for (i, v) in [
        (0, 2.0),
        (OSC1_UNI, 4.0),
        (CUTOFF, 600.0),
        (RES, 0.7),
        (ENV_AMT, 0.3),
        (KEYTRACK, 0.5),
        (FLT_S, 0.5),
    ] {
        set(&mut dark, i, v);
    }
    check_moves(&mut c, "probe", &probe(), &[40, 47]);
    check_moves(&mut c, "probe+res", &res, &[40, 47]);
    check_moves(&mut c, "probe dark saw low", &dark, &[40]);
    check_moves(&mut c, "probe dark saw high", &dark, &[76]);
    c.finish();
}

// ---------------------------------------------------------------------------------------------
// Envelopes

/// Very short releases, from a held note and from a short note (still in its attack or early
/// decay). A whole note shorter than the detector's context is itself a burst, so the short note
/// lasts 30 ms and only its release is judged.
#[test]
fn short_releases_are_click_free() {
    let mut c = Checker::new("releases");
    let mut bases: Vec<(&str, Vec<(u32, f32)>)> =
        PRESETS.iter().map(|&n| (n, with_mode(n, 0.0))).collect();
    bases.push(("probe", probe()));
    for (name, base) in bases {
        for rel in [1.0f32, 2.0, 3.0, 5.0, 10.0, 20.0] {
            let mut p = base.clone();
            set(&mut p, AMP_R, rel);
            if p[AMP_S].1 < 0.3 {
                set(&mut p, AMP_S, 0.6);
            }
            for key in [48u8, 72] {
                let acts = [(ms(20.0), Act::On(key, 1.0)), (ms(500.0), Act::Off(key))];
                c.check(
                    &format!("{name} key {key} release {rel} ms"),
                    &p,
                    &acts,
                    ms(800.0),
                    ms(400.0),
                    false,
                );
            }
            let acts = [(ms(20.0), Act::On(48, 1.0)), (ms(50.0), Act::Off(48))];
            c.check(
                &format!("{name} 30 ms note release {rel} ms"),
                &p,
                &acts,
                ms(400.0),
                ms(48.0),
                false,
            );
        }
    }
    c.finish();
}

#[test]
#[ignore]
fn tmp_diag() {
    let p = with_mode("Pluck", 1.0);
    let mut line = String::from("retrig at 820+d ms (prev vel 0.25):");
    for d in 0..12 {
        let t = 820.0 + d as f32 * 0.7;
        let acts = [
            (ms(720.0), Act::On(48, 0.25)),
            (ms(770.0), Act::Off(48)),
            (ms(t), Act::On(48, 1.0)),
            (ms(t + 50.0), Act::Off(48)),
        ];
        let mut s = synth_with(&p);
        let (l, r) = render_with(s.as_mut(), &acts, ms(1000.0), 128);
        let w = worst_in(&l, &r, ms(800.0), ms(900.0)).unwrap();
        line += &format!(" {:+.1}", w.score_db);
    }
    println!("{line}");
    let mut c = Checker::new("tmp");
    let mut line = String::from("fresh knock by key:");
    for key in 40u8..=56 {
        line += &format!(" {key}:{:+.1}", c.knock(&p, key, 1.0));
    }
    println!("{line}");
    // fresh notes with osc 1 detuned slightly (moves its edges relative to the filter peak)
    let mut line = String::from("fresh knock key 48, osc1 fine:");
    for fine in [-40.0f32, -30.0, -20.0, -10.0, 10.0, 20.0, 30.0, 40.0] {
        let mut q = p.clone();
        q[3].1 = fine;
        line += &format!(" {fine}:{:+.1}", c.knock(&q, 48, 1.0));
    }
    println!("{line}");
}
