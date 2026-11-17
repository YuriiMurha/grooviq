//! Renders every synth preset (a short 140 BPM trap phrase) and every effect (on a synthetic
//! drum + keys loop) to 32-bit float WAV files for listening and A/B comparisons.
//!
//!     cargo run --release -p grooviq-dsp --example render -- <out_dir>

use grooviq_dsp::{NoteEvent, create};
use std::io::Write;

const SR: f32 = 48000.0;
const BPM: f32 = 140.0;

fn beat(b: f32) -> usize {
    (b * 60.0 / BPM * SR) as usize
}

fn write_wav(path: &std::path::Path, l: &[f32], r: &[f32]) {
    let n = l.len();
    let data_len = (n * 8) as u32;
    let mut out = Vec::with_capacity(44 + n * 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(SR as u32).to_le_bytes());
    out.extend_from_slice(&(SR as u32 * 8).to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        out.extend_from_slice(&l[i].to_le_bytes());
        out.extend_from_slice(&r[i].to_le_bytes());
    }
    std::fs::File::create(path)
        .and_then(|mut f| f.write_all(&out))
        .expect("write wav");
    println!("wrote {}", path.display());
}

/// (start beat, length in beats, key, slide)
type Phrase = Vec<(f32, f32, u8, bool)>;

fn phrase_for(name: &str) -> Phrase {
    match name {
        "808 Glide" | "Sub Bass" | "Reese" => vec![
            (0.0, 1.5, 36, false),
            (1.5, 0.5, 36, false),
            (2.0, 1.75, 36, false),
            (3.5, 0.5, 43, true), // overlapping slide up
            (4.0, 1.0, 39, false),
            (5.0, 1.0, 41, false),
            (6.0, 1.5, 34, false),
            (7.25, 0.75, 46, true),
        ],
        "Bell Keys" | "Dark Pad" => {
            let mut v = Vec::new();
            for (b, chord) in [(0.0, [60u8, 63, 67, 70]), (4.0, [56, 60, 63, 67])] {
                for k in chord {
                    v.push((b, 3.5, k, false));
                }
            }
            v
        }
        _ => vec![
            (0.0, 0.5, 72, false),
            (0.75, 0.5, 75, false),
            (1.5, 0.5, 79, false),
            (2.0, 1.0, 77, false),
            (3.0, 0.5, 75, false),
            (3.5, 0.75, 72, true),
            (4.5, 0.5, 70, false),
            (5.0, 2.0, 72, false),
        ],
    }
}

fn render_synth(params: &[(u32, f32)], phrase: &Phrase, beats: f32) -> (Vec<f32>, Vec<f32>) {
    let mut s = create("synth", SR).unwrap();
    for &(p, v) in params {
        s.set_param(p, v);
    }
    let n = beat(beats);
    let mut ev: Vec<(usize, NoteEvent)> = Vec::new();
    for &(start, len, key, slide) in phrase {
        ev.push((
            beat(start),
            NoteEvent {
                offset: 0,
                key,
                velocity: 0.85,
                on: true,
                slide,
                id: u32::from(key),
            },
        ));
        ev.push((
            beat(start + len),
            NoteEvent {
                offset: 0,
                key,
                velocity: 0.0,
                on: false,
                slide: false,
                id: u32::from(key),
            },
        ));
    }
    ev.sort_by_key(|e| e.0);
    let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
    let mut i = 0;
    while i < n {
        let e = (i + 128).min(n);
        let block: Vec<NoteEvent> = ev
            .iter()
            .filter(|(t, _)| *t >= i && *t < e)
            .map(|(t, x)| NoteEvent {
                offset: (*t - i) as u32,
                ..*x
            })
            .collect();
        s.process(&mut l[i..e], &mut r[i..e], None, &block);
        i = e;
    }
    (l, r)
}

/// Kick, clap and hats (two bars, 140 BPM) plus a bell chord, as an effect test signal.
fn drum_loop() -> (Vec<f32>, Vec<f32>) {
    let n = beat(8.0);
    let mut l = vec![0.0f32; n];
    let mut seed = 0x1234_5678u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed >> 8) as f32 / 8_388_608.0 - 1.0
    };
    let mut add = |at: usize, len: usize, f: &mut dyn FnMut(f32) -> f32| {
        for i in 0..len.min(n.saturating_sub(at)) {
            l[at + i] += f(i as f32 / SR);
        }
    };
    for bar in 0..2 {
        let b0 = bar as f32 * 4.0;
        for kb in [0.0, 1.75, 2.5] {
            let mut ph = 0.0f32;
            add(beat(b0 + kb), beat(1.0), &mut |t| {
                let f = 48.0 + 110.0 * libm::expf(-t * 30.0);
                ph += f / SR;
                libm::sinf(ph * std::f32::consts::TAU) * libm::expf(-t * 5.0) * 0.8
            });
        }
        for cb in [1.0, 3.0] {
            add(beat(b0 + cb), beat(0.5), &mut |t| {
                noise() * libm::expf(-t * 22.0) * 0.35
            });
        }
        for h in 0..16 {
            add(beat(b0 + h as f32 * 0.25), beat(0.12), &mut |t| {
                let x = noise();
                (x - 0.0) * libm::expf(-t * 90.0) * 0.12
            });
        }
    }
    let r = l.clone();
    let keys = render_synth(
        &grooviq_dsp::synth::preset("Bell Keys").unwrap(),
        &phrase_for("Bell Keys"),
        8.0,
    );
    let l: Vec<f32> = l.iter().zip(&keys.0).map(|(a, b)| a + 0.5 * b).collect();
    let r: Vec<f32> = r.iter().zip(&keys.1).map(|(a, b)| a + 0.5 * b).collect();
    (l, r)
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "renders".into()));
    std::fs::create_dir_all(&dir).expect("output dir");
    for name in grooviq_dsp::synth::PRESETS {
        let (l, r) = render_synth(
            &grooviq_dsp::synth::preset(name).unwrap(),
            &phrase_for(name),
            9.0,
        );
        write_wav(
            &dir.join(format!(
                "synth_{}.wav",
                name.to_lowercase().replace(' ', "_")
            )),
            &l,
            &r,
        );
    }
    let (dl, dr) = drum_loop();
    write_wav(&dir.join("fx_dry.wav"), &dl, &dr);
    let settings: [(&str, &[(u32, f32)]); 8] = [
        // band n param k = n * 6 + k: low cut to 40 Hz, +3 dB low shelf, -4 dB at 400 Hz, +4 dB air
        ("eq", &[(2, 40.0), (9, 3.0), (15, -4.0), (39, 4.0)]),
        (
            "comp",
            &[(0, -24.0), (1, 4.0), (2, 5.0), (3, 80.0), (5, 6.0)],
        ),
        ("limiter", &[(0, 9.0), (1, -0.3)]),
        ("drive", &[(0, 2.0), (1, 14.0), (2, 0.2), (5, 2.0)]),
        ("reverb", &[(0, 0.7), (1, 2.5), (7, 0.35)]),
        ("delay", &[(1, 5.0), (3, 0.45), (7, 0.3)]),
        ("ott", &[(0, 0.5)]),
        ("utility", &[(2, 1.6), (3, 120.0)]),
    ];
    for (id, params) in settings {
        let mut d = create(id, SR).unwrap();
        d.set_tempo(BPM);
        for &(p, v) in params {
            d.set_param(p, v);
        }
        let (mut l, mut r) = (dl.clone(), dr.clone());
        let mut i = 0;
        while i < l.len() {
            let e = (i + 128).min(l.len());
            d.process(&mut l[i..e], &mut r[i..e], None, &[]);
            i = e;
        }
        write_wav(&dir.join(format!("fx_{id}.wav")), &l, &r);
    }
}
