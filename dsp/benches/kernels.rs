//! Micro-benchmarks of the instruments' inner loops: one 64-frame block (the engine's block)
//! with 1, 4 or 16 voices sounding. `cargo bench -p grooviq-dsp`; divan also counts the
//! allocations inside each timed block, which must stay at zero.

use std::sync::Arc;

use divan::{Bencher, black_box};
use grooviq_dsp::sampler::{SampleBuf, Sampler};
use grooviq_dsp::synth::{Synth, preset};
use grooviq_dsp::{Device, NoteEvent};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

const SR: f32 = 48_000.0;
const BLOCK: usize = 64;
const VOICES: [usize; 3] = [1, 4, 16];
/// Blocks rendered before timing, so the voices are past their attacks and fade-ins.
const SETTLE_BLOCKS: usize = 16;

fn main() {
    divan::main();
}

/// Note-ons at the block start on `voices` different keys from `first`.
fn chord(first: u8, voices: usize) -> Vec<NoteEvent> {
    (0..voices)
        .map(|i| NoteEvent {
            offset: 0,
            key: first + i as u8,
            velocity: 0.8,
            on: true,
            slide: false,
        })
        .collect()
}

fn settle(device: &mut impl Device, events: &[NoteEvent]) -> ([f32; BLOCK], [f32; BLOCK]) {
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    device.process(&mut l, &mut r, None, events);
    for _ in 1..SETTLE_BLOCKS {
        device.process(&mut l, &mut r, None, &[]);
    }
    (l, r)
}

/// Ten seconds of stereo noise (a fixed sequence), longer than any voice plays here.
fn noise_sample() -> Arc<SampleBuf> {
    let mut x = 0x2545_f491_u32;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x as f32 / u32::MAX as f32) - 0.5
    };
    let len = 10 * SR as usize;
    let l: Vec<f32> = (0..len).map(|_| next()).collect();
    let r: Vec<f32> = (0..len).map(|_| next()).collect();
    Arc::new(SampleBuf { rate: SR, l, r })
}

/// A sampler playing a long sample: keys from 55 up, so most voices read between frames
/// (the cubic interpolation path). Each timed block gets a fresh, settled sampler, because a
/// voice ends with its sample.
#[divan::bench(args = VOICES)]
fn sampler_block(bencher: Bencher, voices: usize) {
    let sample = noise_sample();
    let notes = chord(55, voices);
    bencher
        .with_inputs(|| {
            let mut s = Sampler::new(SR);
            s.set_sample(Some(Arc::clone(&sample)));
            let (l, r) = settle(&mut s, &notes);
            (s, l, r)
        })
        .bench_local_refs(|(s, l, r)| s.process(black_box(l), black_box(r), None, &[]));
}

fn synth_block(bencher: Bencher, patch: &str, voices: usize) {
    let mut synth = Synth::new(SR);
    for (index, value) in preset(patch).expect("a factory preset") {
        synth.set_param(index, value);
    }
    // Held notes: the amp envelope sustains, so the voices sound for as long as it runs.
    let (mut l, mut r) = settle(&mut synth, &chord(48, voices));
    bencher.bench_local(|| synth.process(black_box(&mut l), black_box(&mut r), None, &[]));
}

/// The default patch: two oscillators per voice, no unison, through the filter.
#[divan::bench(args = VOICES)]
fn synth_init_block(bencher: Bencher, voices: usize) {
    synth_block(bencher, "Init", voices);
}

/// The heaviest polyphonic preset: 7 + 5 unison oscillators per voice.
#[divan::bench(args = VOICES)]
fn synth_dark_pad_block(bencher: Bencher, voices: usize) {
    synth_block(bencher, "Dark Pad", voices);
}

/// Trap Lead played poly with seven unison voices on oscillator 1: the unison inner loop at its
/// heaviest lead setting (seven wavetable reads per voice and sample, plus oscillator 2).
#[divan::bench(args = VOICES)]
fn synth_trap_lead_uni7_block(bencher: Bencher, voices: usize) {
    let mut synth = Synth::new(SR);
    let mut patch = preset("Trap Lead").expect("a factory preset");
    patch.push((5, 7.0)); // OSC1_UNI
    patch.push((38, 0.0)); // VOICE_MODE: poly (the preset is a legato lead: one voice)
    for (index, value) in patch {
        synth.set_param(index, value);
    }
    let (mut l, mut r) = settle(&mut synth, &chord(48, voices));
    bencher.bench_local(|| synth.process(black_box(&mut l), black_box(&mut r), None, &[]));
}
