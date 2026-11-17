//! The synth's SIMD voices against the scalar code they replaced: every sample, every meter and
//! the scope must match bit for bit (the export is byte-identical on macOS and Windows only if
//! every lane performs exactly the scalar sequence of operations). All renders are offline.
//!
//! Needs the `scalar-reference` feature (`cargo test --all-features`).

use grooviq_dsp::synth::{self, PRESETS, SPEC, Synth, preset};
use grooviq_dsp::{Device, NoteEvent};

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// xorshift64*: a fixed fuzz sequence.
struct Fuzz(u64);

impl Fuzz {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / 16_777_216.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 33) as usize % n
    }
    fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

/// A random value for parameter `i`: uniform, log-uniform over wide positive ranges (cutoff,
/// times), or one of the range's ends, so the extremes get rendered too.
fn random_param(f: &mut Fuzz, i: usize) -> f32 {
    let (lo, hi, def) = SPEC[i];
    match f.below(10) {
        0 => lo,
        1 => hi,
        2 => def,
        _ if lo > 0.0 && hi / lo > 20.0 => lo * libm::powf(hi / lo, f.unit()),
        _ => lo + (hi - lo) * f.unit(),
    }
}

#[derive(Clone, Copy)]
enum Step {
    Block(usize),
    Param(u32, f32),
    Choke,
}

/// One fuzz case: a random patch, a random score and random block sizes and edits.
struct Case {
    sr: f32,
    patch: Vec<(u32, f32)>,
    /// (absolute frame, event)
    events: Vec<(usize, NoteEvent)>,
    steps: Vec<Step>,
}

fn random_case(f: &mut Fuzz) -> Case {
    let sr = [48_000.0, 48_000.0, 44_100.0, 96_000.0, 22_050.0][f.below(5)];
    let patch: Vec<(u32, f32)> = (0..synth::NUM_PARAMS)
        .map(|i| (i as u32, random_param(f, i)))
        .collect();
    let frames = 2_000 + f.below(26_000);
    let voices = 1 + f.below(20);
    let mut events = Vec::new();
    for _ in 0..voices {
        let key = 24 + f.below(72) as u8;
        let vel = if f.chance(0.3) {
            1.0
        } else {
            0.02 + 0.98 * f.unit()
        };
        let at = f.below(frames * 6 / 10);
        let slide = f.chance(0.12);
        events.push((
            at,
            NoteEvent {
                offset: 0,
                key,
                velocity: vel,
                on: true,
                slide,
                id: u32::from(key),
            },
        ));
        // most notes end, some ring through the whole case
        if f.chance(0.8) {
            let end = (at + 20 + f.below(frames)).min(frames - 1);
            events.push((
                end,
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
    }
    events.sort_by_key(|e| e.0);

    // block sizes 1..=128; parameter edits and chokes land on block boundaries
    let mut steps = Vec::new();
    let mut at = 0;
    while at < frames {
        if f.chance(0.04) {
            let i = f.below(synth::NUM_PARAMS);
            steps.push(Step::Param(i as u32, random_param(f, i)));
        }
        if f.chance(0.004) {
            steps.push(Step::Choke);
        }
        let n = if f.chance(0.2) {
            1 + f.below(8)
        } else {
            1 + f.below(128)
        };
        let n = n.min(frames - at);
        steps.push(Step::Block(n));
        at += n;
    }
    Case {
        sr,
        patch,
        events,
        steps,
    }
}

fn bits(x: &[f32]) -> Vec<u32> {
    x.iter().map(|v| v.to_bits()).collect()
}

/// Plays `case` through both synths block by block and returns the first difference, if any.
fn first_difference(case: &Case) -> Option<String> {
    let mut simd = Synth::new(case.sr);
    let mut scalar = Synth::scalar_reference(case.sr);
    for &(i, v) in &case.patch {
        simd.set_param(i, v);
        scalar.set_param(i, v);
    }
    let mut at = 0;
    let mut next_event = 0;
    for (n_step, step) in case.steps.iter().enumerate() {
        match *step {
            Step::Param(i, v) => {
                simd.set_param(i, v);
                scalar.set_param(i, v);
            }
            Step::Choke => {
                simd.choke();
                scalar.choke();
            }
            Step::Block(n) => {
                let mut block = Vec::new();
                while next_event < case.events.len() && case.events[next_event].0 < at + n {
                    let (frame, mut e) = case.events[next_event];
                    e.offset = (frame.max(at) - at) as u32;
                    block.push(e);
                    next_event += 1;
                }
                let (mut al, mut ar) = (vec![0.0f32; n], vec![0.0f32; n]);
                let (mut bl, mut br) = (vec![0.0f32; n], vec![0.0f32; n]);
                simd.process(&mut al, &mut ar, None, &block);
                scalar.process(&mut bl, &mut br, None, &block);
                for (name, a, b) in [("left", &al, &bl), ("right", &ar, &br)] {
                    if bits(a) != bits(b) {
                        let k = (0..n).find(|&k| a[k].to_bits() != b[k].to_bits()).unwrap();
                        return Some(format!(
                            "{name} frame {} (block {n_step}, offset {k} of {n}): simd {:e} ({:#010x}), scalar {:e} ({:#010x})",
                            at + k,
                            a[k],
                            a[k].to_bits(),
                            b[k],
                            b[k].to_bits()
                        ));
                    }
                }
                for m in 0..6 {
                    if simd.meter(m).to_bits() != scalar.meter(m).to_bits() {
                        return Some(format!(
                            "meter {m} after frame {}: simd {:e}, scalar {:e}",
                            at + n,
                            simd.meter(m),
                            scalar.meter(m)
                        ));
                    }
                }
                let (mut sa, mut sb) = ([0.0f32; 256], [0.0f32; 256]);
                simd.meter_array(6, &mut sa);
                scalar.meter_array(6, &mut sb);
                if bits(&sa) != bits(&sb) {
                    return Some(format!("scope after frame {}", at + n));
                }
                at += n;
            }
        }
    }
    None
}

#[test]
fn simd_matches_the_scalar_reference_bit_for_bit() {
    let mut f = Fuzz(0x9E37_79B9_7F4A_7C15);
    for case_no in 0..200 {
        let case = random_case(&mut f);
        if let Some(d) = first_difference(&case) {
            panic!("case {case_no} (sample rate {}): {d}", case.sr);
        }
    }
}

/// The patches most likely to break a lane: every unison count on both oscillators, every filter
/// type, every voice mode, with the layout changing under sounding voices (the crossfades).
#[test]
fn unison_counts_filters_and_modes_match_the_scalar_reference() {
    let mut f = Fuzz(0x0123_4567_89AB_CDEF);
    for uni1 in 1..=7 {
        for uni2 in [1, 2, 4, 5, 7] {
            for filter in 0..4 {
                let mut case = random_case(&mut f);
                for (i, v) in case.patch.iter_mut() {
                    *v = match *i as usize {
                        5 => uni1 as f32,
                        13 => uni2 as f32,
                        19 => filter as f32,
                        4 | 12 => 0.5,
                        _ => *v,
                    };
                }
                // change the layout and the filter type under the sounding notes
                let at = case.steps.len() / 2;
                case.steps
                    .insert(at, Step::Param(5, (1 + f.below(7)) as f32));
                case.steps.insert(at, Step::Param(19, f.below(4) as f32));
                if let Some(d) = first_difference(&case) {
                    panic!("unison {uni1}/{uni2}, filter {filter}: {d}");
                }
            }
        }
    }
}

#[test]
fn every_preset_matches_the_scalar_reference() {
    for name in std::iter::once("Init").chain(PRESETS) {
        for block in [37usize, 64, 128] {
            let patch = if name == "Init" {
                SPEC.iter()
                    .enumerate()
                    .map(|(i, s)| (i as u32, s.2))
                    .collect()
            } else {
                preset(name).expect("a factory preset")
            };
            let mut events = Vec::new();
            for (n, key) in [
                36u8, 43, 48, 52, 55, 60, 64, 67, 72, 76, 79, 84, 40, 45, 50, 57, 62,
            ]
            .into_iter()
            .enumerate()
            {
                events.push((
                    n * 3_000,
                    NoteEvent {
                        offset: 0,
                        key,
                        velocity: 0.4 + 0.035 * n as f32,
                        on: true,
                        slide: false,
                        id: u32::from(key),
                    },
                ));
                events.push((
                    n * 3_000 + 40_000,
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
            events.sort_by_key(|e| e.0);
            let frames = 120_000;
            let mut steps = Vec::new();
            let mut at = 0;
            while at < frames {
                let n = block.min(frames - at);
                steps.push(Step::Block(n));
                at += n;
            }
            let case = Case {
                sr: 48_000.0,
                patch,
                events,
                steps,
            };
            if let Some(d) = first_difference(&case) {
                panic!("preset {name}, block {block}: {d}");
            }
        }
    }
}

fn index(name: &str) -> u32 {
    synth::PARAM_IDS
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("no parameter {name}")) as u32
}

fn chord(n: usize, on: bool) -> Vec<NoteEvent> {
    (0..n)
        .map(|i| NoteEvent {
            offset: (i % 5) as u32,
            key: 40 + 3 * i as u8,
            velocity: 0.8,
            on,
            slide: false,
            id: u32::from(40 + 3 * i as u8),
        })
        .collect()
}

#[test]
fn simd_process_never_allocates() {
    let mut s = Synth::new(48_000.0);
    for (i, v) in preset("Dark Pad").expect("a factory preset") {
        s.set_param(i, v);
    }
    s.set_param(index("osc1_unison"), 7.0);
    s.set_param(index("osc2_unison"), 5.0);
    s.set_param(index("filter_type"), 1.0);
    s.set_param(index("voice_mode"), 0.0);
    let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
    let on = chord(16, true);
    let off = chord(16, false);
    assert_no_alloc::assert_no_alloc(|| {
        s.process(&mut l, &mut r, None, &on);
        for block in 0..400 {
            s.process(&mut l, &mut r, None, &[]);
            if block == 100 {
                // a filter type crossfade, a unison change and a choke under the voices
                s.set_param(index("filter_type"), 2.0);
                s.set_param(index("osc1_unison"), 3.0);
            }
            if block == 200 {
                s.choke();
            }
            if block == 220 {
                s.process(&mut l, &mut r, None, &on);
            }
        }
        s.process(&mut l, &mut r, None, &off);
        for _ in 0..50 {
            s.process(&mut l, &mut r, None, &[]);
        }
    });
    assert!(l.iter().chain(&r).all(|x| x.is_finite()));
}

/// Release tails must not be slower than sustained notes (denormal numbers cost 10-100x on x86):
/// the explicit `flush()` calls keep every feedback path out of that range.
#[test]
#[cfg_attr(debug_assertions, ignore = "timing: release builds only")]
fn denormal_tails_stay_fast() {
    use std::time::Instant;
    const SR: f32 = 48_000.0;
    const BLOCK: usize = 64;
    let blocks = 10 * SR as usize / BLOCK;
    let run = |release_at: Option<usize>| -> f64 {
        let mut s = Synth::new(SR);
        for (i, v) in preset("Dark Pad").expect("a factory preset") {
            s.set_param(i, v);
        }
        s.set_param(index("voice_mode"), 0.0);
        s.set_param(index("filter_type"), 1.0);
        s.set_param(index("filter_res"), 0.9);
        s.set_param(index("amp_sustain"), 1.0);
        s.set_param(index("amp_release"), 8000.0);
        s.set_param(index("flt_release"), 8000.0);
        let (mut l, mut r) = ([0.0f32; BLOCK], [0.0f32; BLOCK]);
        let (on, off) = (chord(16, true), chord(16, false));
        let t = Instant::now();
        s.process(&mut l, &mut r, None, &on);
        for b in 0..blocks {
            let ev: &[NoteEvent] = if release_at == Some(b) { &off } else { &[] };
            s.process(&mut l, &mut r, None, ev);
        }
        t.elapsed().as_secs_f64()
    };
    let best = |release_at: Option<usize>| (0..3).map(|_| run(release_at)).fold(f64::MAX, f64::min);
    let sustained = best(None);
    let tail = best(Some(100));
    assert!(
        tail <= 2.0 * sustained,
        "10 s of decaying tails took {tail:.3} s, sustained notes {sustained:.3} s"
    );
}
