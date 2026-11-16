//! The presets keep their character: plain notes and chords sound as they did before the click
//! fixes, and a slow-attack note re-struck while it sounds keeps its level.

use grooviq_dsp::synth::{PRESETS, preset};
use grooviq_dsp::{NoteEvent, create};

const SR: f32 = 48_000.0;

fn ms(t: f32) -> usize {
    (t * 0.001 * SR) as usize
}

/// Renders a synth with `params` and notes `(frame, key, velocity, on)` in 128-frame blocks.
fn render(
    params: &[(u32, f32)],
    notes: &[(usize, u8, f32, bool)],
    n: usize,
) -> (Vec<f32>, Vec<f32>) {
    let mut s = create("synth", SR).unwrap();
    for &(i, v) in params {
        s.set_param(i, v);
    }
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut c = 0;
    while c < n {
        let e = (c + 128).min(n);
        let ev: Vec<NoteEvent> = notes
            .iter()
            .filter(|x| x.0 >= c && x.0 < e)
            .map(|&(t, key, velocity, on)| NoteEvent {
                offset: (t - c) as u32,
                key,
                velocity,
                on,
                slide: false,
            })
            .collect();
        s.process(&mut l[c..e], &mut r[c..e], None, &ev);
        c = e;
    }
    (l, r)
}

/// Notes `(frame, key, velocity, on)`.
type Notes = Vec<(usize, u8, f32, bool)>;
/// `(name, parameters, notes, frames)`.
type Scenario = (String, Vec<(u32, f32)>, Notes, usize);

/// Plain notes and a chord from silence for every preset, in its own voice mode and in poly. A
/// chord in mono mode is a run of retriggers, which the click fixes changed on purpose (the click
/// tests cover those), so mono gets the note only.
fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    for name in PRESETS {
        let own = preset(name).unwrap()[38].1;
        let modes: &[f32] = if own == 0.0 { &[0.0] } else { &[own, 0.0] };
        for &mode in modes {
            let mut p = preset(name).unwrap();
            p[38].1 = mode;
            let label = ["poly", "mono", "legato"][mode as usize];
            let note = vec![(ms(10.0), 48, 0.9, true), (ms(410.0), 48, 0.0, false)];
            out.push((format!("{name} {label} note"), p.clone(), note, ms(1000.0)));
            if mode != 1.0 {
                let mut chord = Vec::new();
                for (key, vel) in [(48u8, 0.9f32), (55, 0.7), (64, 0.5)] {
                    chord.push((ms(10.0), key, vel, true));
                    chord.push((ms(310.0), key, 0.0, false));
                }
                chord.sort_by_key(|x| (x.0, !x.3));
                out.push((format!("{name} {label} chord"), p, chord, ms(1000.0)));
            }
        }
    }
    out
}

/// Per 10 ms frame: RMS left, RMS right, peak left, peak right, in hundredths of a dB (floor
/// -120 dB).
fn frames(l: &[f32], r: &[f32]) -> Vec<[i64; 4]> {
    let db = |x: f64| (100.0 * (20.0 * x.max(1e-6).log10())).round() as i64;
    l.chunks(ms(10.0))
        .zip(r.chunks(ms(10.0)))
        .map(|(a, b)| {
            let rms = |x: &[f32]| {
                (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
            };
            let peak = |x: &[f32]| x.iter().fold(0.0f64, |m, v| m.max(v.abs() as f64));
            [db(rms(a)), db(rms(b)), db(peak(a)), db(peak(b))]
        })
        .collect()
}

const HEADER: &str = "\
# Reference for presets_sound_like_the_original_synth (dsp/tests/presets.rs).
# Per 10 ms frame: RMS left, RMS right, peak left, peak right in hundredths of a dB (floor
# -120 dB) of the scenarios in presets.rs (every preset's plain note and chord, own voice mode
# and poly, 48 kHz, 128-frame blocks).
# Generated from the synth before the click fixes (commit c480d8d):
#   git archive c480d8d | tar -x -C <dir> && cp dsp/tests/presets.rs <dir>/dsp/tests/
#   cd <dir> && GROOVIQ_WRITE_PRESET_REFERENCE=<file> \\
#     cargo test --release -p grooviq-dsp --test presets presets_sound_like
# Any later commit whose plain notes are unchanged writes the same file.
";

/// Every preset's plain notes and chords match the original synth (c480d8d): per 10 ms RMS and
/// peak of both channels within 0.02 dB, enough to absorb last-bit libm differences between
/// platforms. Set GROOVIQ_WRITE_PRESET_REFERENCE=<file> to write the reference instead.
#[test]
fn presets_sound_like_the_original_synth() {
    let mut text = String::from(HEADER);
    let mut now = Vec::new();
    for (name, p, notes, n) in scenarios() {
        let (l, r) = render(&p, &notes, n);
        let f = frames(&l, &r);
        text += &format!("> {name}\n");
        for x in &f {
            text += &format!("{} {} {} {}\n", x[0], x[1], x[2], x[3]);
        }
        now.push((name, f));
    }
    if let Ok(path) = std::env::var("GROOVIQ_WRITE_PRESET_REFERENCE") {
        std::fs::write(&path, text).unwrap();
        return;
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/preset_reference.txt"
    );
    let reference = std::fs::read_to_string(path).unwrap();
    let mut want: Vec<(String, Vec<[i64; 4]>)> = Vec::new();
    for line in reference.lines().filter(|l| !l.starts_with('#')) {
        if let Some(name) = line.strip_prefix("> ") {
            want.push((name.to_string(), Vec::new()));
        } else {
            let v: Vec<i64> = line
                .split_whitespace()
                .map(|x| x.parse().unwrap())
                .collect();
            want.last_mut().unwrap().1.push([v[0], v[1], v[2], v[3]]);
        }
    }
    let names: Vec<&String> = now.iter().map(|x| &x.0).collect();
    assert_eq!(
        names,
        want.iter().map(|x| &x.0).collect::<Vec<_>>(),
        "scenario list changed"
    );
    let mut bad = Vec::new();
    for ((name, got), (_, exp)) in now.iter().zip(&want) {
        assert_eq!(got.len(), exp.len(), "{name}: frame count");
        let off: Vec<String> = got
            .iter()
            .zip(exp)
            .enumerate()
            .filter(|(_, (g, e))| (0..4).any(|k| (g[k] - e[k]).abs() > 2))
            .map(|(i, (g, e))| format!("{} ms {:?} (reference {:?})", i * 10, g, e))
            .collect();
        if !off.is_empty() {
            bad.push(format!(
                "{name}: {} of {} frames differ, first {}",
                off.len(),
                got.len(),
                off[0]
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "presets changed (hundredths of a dB):\n  {}",
        bad.join("\n  ")
    );
}

fn rms(l: &[f32], r: &[f32]) -> f64 {
    let sum: f64 = l.iter().chain(r).map(|v| (*v as f64).powi(2)).sum();
    (sum / (l.len() + r.len()) as f64).sqrt()
}

/// Re-striking a key while a slow-attack note sounds (Dark Pad: 900 ms attack, 2.8 s release)
/// keeps its level: the envelopes go on from where they are instead of the note restarting from
/// silence while the old one releases.
#[test]
fn slow_attack_re_press_keeps_its_level() {
    let p = preset("Dark Pad").unwrap();
    let press = ms(600.0);
    let held = [(ms(20.0), 48u8, 0.8f32, true), (ms(1500.0), 48, 0.0, false)];
    let mut again = held.to_vec();
    again.push((press, 48, 0.8, true));
    again.sort_by_key(|x| x.0);
    let (pl, pr) = render(&p, &held, ms(1400.0));
    let (al, ar) = render(&p, &again, ms(1400.0));
    let before = rms(&al[press - ms(50.0)..press], &ar[press - ms(50.0)..press]);
    let mut low = (f64::INFINITY, 0.0f32);
    for w in (press..ms(1400.0) - ms(20.0)).step_by(ms(20.0)) {
        let x = w..w + ms(20.0);
        let level = rms(&al[x.clone()], &ar[x.clone()]);
        let plain = rms(&pl[x.clone()], &pr[x]);
        let ratio = (level / before).min(level / plain);
        if ratio < low.0 {
            low = (ratio, w as f32 * 1000.0 / SR);
        }
    }
    assert!(
        low.0 >= 0.9,
        "re-pressed Dark Pad dips to {:.2} of its level at {:.0} ms",
        low.0,
        low.1
    );
}
