//! Instrument kernels at 64 frames with 1, 4 or 16 voices, and effect kernels at 64/128 frames.
//! `cargo bench -p grooviq-dsp`; divan also reports allocations inside each timed block,
//! which must stay at zero (the realtime suite enforces that requirement).

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

/// Continuous device state, fresh deterministic input on every timed block. The small input
/// copies are included in the timing; construction, preset allocation and warmup are not.
fn drum_shaper_block(bencher: Bencher, block: usize, name: &str) {
    use grooviq_dsp::drum_shaper::{DrumShaper, preset};

    let mut device = DrumShaper::new(SR);
    for (index, value) in preset(name).expect("a factory preset") {
        device.set_param(index, value);
    }
    let source_l: [f32; 128] = std::array::from_fn(|i| {
        let envelope = if i < 12 { 0.8 } else { 0.15 };
        libm::sinf(i as f32 * 0.31) * envelope
    });
    let source_r: [f32; 128] = std::array::from_fn(|i| source_l[127 - i] * 0.5);
    let (mut l, mut r) = ([0.0; 128], [0.0; 128]);
    for _ in 0..SETTLE_BLOCKS {
        l.copy_from_slice(&source_l);
        r.copy_from_slice(&source_r);
        device.process(&mut l[..block], &mut r[..block], None, &[]);
    }
    bencher.bench_local(|| {
        l[..block].copy_from_slice(&source_l[..block]);
        r[..block].copy_from_slice(&source_r[..block]);
        device.process(
            black_box(&mut l[..block]),
            black_box(&mut r[..block]),
            None,
            &[],
        );
        black_box((&l[..block], &r[..block]));
    });
}

#[divan::bench(args = [64, 128])]
fn drum_shaper_kick_block(bencher: Bencher, block: usize) {
    drum_shaper_block(bencher, block, "Kick Punch");
}

#[divan::bench(args = [64, 128])]
fn drum_shaper_snare_block(bencher: Bencher, block: usize) {
    drum_shaper_block(bencher, block, "Snare Body");
}

#[divan::bench(args = [64, 128])]
fn drum_shaper_bus_block(bencher: Bencher, block: usize) {
    drum_shaper_block(bencher, block, "Bus Softening");
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
            id: u64::from(first + i as u8),
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

/// Continuous device state, fresh transient stereo input per call. Copies are timed;
/// construction, preset allocation and warmup are outside the measurement.
fn bitcrusher_block(bencher: Bencher, block: usize, name: &str) {
    use grooviq_dsp::bitcrusher::{Bitcrusher, preset};
    let mut device = Bitcrusher::with_seed(SR, 77);
    for (index, value) in preset(name).expect("a factory preset") {
        device.set_param(index, value);
    }
    let source_l: [f32; 128] = std::array::from_fn(|i| {
        let envelope = if i < 12 { 0.8 } else { 0.15 };
        envelope * libm::sinf(i as f32 * 0.31)
    });
    let source_r: [f32; 128] = std::array::from_fn(|i| {
        let envelope = if i < 23 { 0.6 } else { 0.12 };
        envelope * libm::cosf(i as f32 * 0.19)
    });
    let (mut l, mut r) = ([0.0; 128], [0.0; 128]);
    for _ in 0..SETTLE_BLOCKS {
        l.copy_from_slice(&source_l);
        r.copy_from_slice(&source_r);
        device.process(&mut l[..block], &mut r[..block], None, &[]);
    }
    bencher.bench_local(|| {
        l[..block].copy_from_slice(&source_l[..block]);
        r[..block].copy_from_slice(&source_r[..block]);
        device.process(
            black_box(&mut l[..block]),
            black_box(&mut r[..block]),
            None,
            &[],
        );
        black_box((&l[..block], &r[..block]));
    });
}
#[divan::bench(args = [64, 128])]
fn bitcrusher_clean_block(bencher: Bencher, block: usize) {
    bitcrusher_block(bencher, block, "Clean Steps");
}
#[divan::bench(args = [64, 128])]
fn bitcrusher_filtered_block(bencher: Bencher, block: usize) {
    bitcrusher_block(bencher, block, "Filtered Dust");
}
#[divan::bench(args = [64, 128])]
fn bitcrusher_grain_block(bencher: Bencher, block: usize) {
    bitcrusher_block(bencher, block, "Loose Grain");
}

// Keep the device continuous, but replace the in-place output with fresh stereo input on
// every timed call. Input synthesis, setup, preset allocation and warmup are not timed.
#[divan::bench(args = grooviq_dsp::ott::PRESETS, consts = [64, 128])]
fn multiband_block<const N: usize>(bencher: Bencher, name: &str) {
    let mut device = grooviq_dsp::ott::Ott::new(SR);
    for (index, value) in grooviq_dsp::ott::preset(name).unwrap() {
        device.set_param(index, value);
    }
    // A cycling bank keeps the changing phase and loud/quiet quarters without timing sinf.
    let inputs: Vec<([f32; N], [f32; N])> = (0..64)
        .map(|block| {
            let mut l = [0.0; N];
            let mut r = [0.0; N];
            for i in 0..N {
                let t = (block * N + i) as f32 / SR;
                let envelope = if i < N / 4 { 0.65 } else { 0.025 };
                l[i] = envelope
                    * (libm::sinf(t * 439.823)
                        + 0.5 * libm::sinf(t * 6283.185)
                        + 0.3 * libm::sinf(t * 43982.3));
                r[i] = envelope
                    * (libm::sinf(t * 552.92)
                        + 0.4 * libm::sinf(t * 9424.778)
                        + 0.25 * libm::sinf(t * 50265.484));
            }
            (l, r)
        })
        .collect();
    let mut next = 0;
    let mut l = [0.0; N];
    let mut r = [0.0; N];
    let mut render = || {
        l.copy_from_slice(&inputs[next].0);
        r.copy_from_slice(&inputs[next].1);
        next = (next + 1) % inputs.len();
        device.process(black_box(&mut l), black_box(&mut r), None, &[]);
    };
    for _ in 0..SETTLE_BLOCKS {
        render();
    }
    bencher.bench_local(render);
}
