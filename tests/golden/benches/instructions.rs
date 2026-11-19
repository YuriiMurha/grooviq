//! Instruction counts (Callgrind, through gungraun) of the instruments' inner loops and of the
//! reference scenes. Unlike timings they are exact and repeatable, so CI compares each pull
//! request with its base and flags a rise of more than 1 %.
//!
//! Linux only (Valgrind): `make bench-instructions`, which needs `valgrind` and
//! `gungraun-runner` at the version of the `gungraun` crate.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("instruction counts need Valgrind, which runs on Linux only");
}

#[cfg(target_os = "linux")]
#[path = "support/synth_workload.rs"]
mod synth_workload;

#[cfg(target_os = "linux")]
#[path = "support/sampler_workload.rs"]
mod sampler_workload;

#[cfg(target_os = "linux")]
mod counts {
    use super::sampler_workload::scenario;
    use std::hint::black_box;
    use std::sync::Arc;

    use grooviq_dsp::sampler::{SampleBuf, Sampler};
    use grooviq_dsp::synth::{Synth, preset};
    use grooviq_dsp::{Device, NoteEvent};
    use grooviq_engine::{AudioProcessor, Controller, EngineConfig, engine_with};
    use grooviq_golden::SAMPLE_RATE;
    use grooviq_golden::perf::perf_scenes;
    use gungraun::{library_benchmark, library_benchmark_group};

    const SR: f32 = SAMPLE_RATE as f32;
    /// The engine's block.
    const BLOCK: usize = 64;
    /// Blocks rendered before counting, so the voices are past their attacks and fade-ins.
    const SETTLE_BLOCKS: usize = 16;
    /// Blocks each instrument benchmark counts.
    const COUNTED_BLOCKS: usize = 100;

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

    fn settle<D: Device>(mut device: D, events: &[NoteEvent]) -> D {
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        device.process(&mut l, &mut r, None, events);
        for _ in 1..SETTLE_BLOCKS {
            device.process(&mut l, &mut r, None, &[]);
        }
        device
    }

    fn render<D: Device>(device: &mut D) -> f32 {
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        for _ in 0..COUNTED_BLOCKS {
            device.process(black_box(&mut l), black_box(&mut r), None, &[]);
        }
        l[BLOCK - 1] + r[BLOCK - 1]
    }

    /// A sampler with `voices` notes of a two-second stereo noise sample, from key 55 up.
    fn sampler(voices: usize) -> Sampler {
        let mut x = 0x2545_f491_u32;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32) - 0.5
        };
        let len = 2 * SAMPLE_RATE as usize;
        let l: Vec<f32> = (0..len).map(|_| next()).collect();
        let r: Vec<f32> = (0..len).map(|_| next()).collect();
        let mut s = Sampler::new(SR);
        s.set_sample(Some(Arc::new(SampleBuf { rate: SR, l, r })));
        settle(s, &chord(55, voices))
    }

    fn synth(patch: &str, voices: usize) -> Synth {
        let mut s = Synth::new(SR);
        for (index, value) in preset(patch).expect("a factory preset") {
            s.set_param(index, value);
        }
        settle(s, &chord(48, voices))
    }

    // 100 blocks of 64 frames. (Plain comments: the macro rejects doc attributes.)
    #[library_benchmark]
    #[bench::voices_16(sampler(16))]
    #[bench::filter_static_16(scenario(0.0, false))]
    #[bench::filter_envelope_16(scenario(0.7, false))]
    #[bench::filter_velocity_16(scenario(0.7, true))]
    fn sampler_blocks(mut s: Sampler) -> f32 {
        render(&mut s)
    }

    // 100 blocks of 64 frames. (Plain comments: the macro rejects doc attributes.)
    #[library_benchmark]
    #[bench::init_4(synth("Init", 4))]
    #[bench::dark_pad_4(synth("Dark Pad", 4))]
    fn synth_blocks(mut s: Synth) -> f32 {
        render(&mut s)
    }

    #[library_benchmark]
    #[bench::idle_initialized(crate::synth_workload::setup(false, false))]
    #[bench::idle_freerunning(crate::synth_workload::setup(true, false))]
    #[bench::active_modulated(crate::synth_workload::setup(true, true))]
    fn synth_idle_blocks(s: Synth) -> Synth {
        crate::synth_workload::render(s)
    }

    /// An engine playing a reference scene, and a stereo buffer of one engine block.
    fn scene(name: &str) -> (Controller, AudioProcessor, Vec<f32>) {
        let scene = perf_scenes()
            .into_iter()
            .find(|s| s.name == name)
            .expect("a reference scene");
        let config = EngineConfig { sample_rate: SR };
        let (mut controller, processor) = engine_with(config, scene.state);
        for cmd in scene.start {
            controller
                .send(cmd)
                .expect("the start commands fit the queue");
        }
        (controller, processor, vec![0.0; BLOCK * 2])
    }

    // One second of the scene, in 64-frame `process` calls.
    #[library_benchmark]
    #[bench::drums_4ch(scene("drums_4ch"))]
    #[bench::beat_16ch(scene("beat_16ch"))]
    fn scene_second(
        engine: (Controller, AudioProcessor, Vec<f32>),
    ) -> (Controller, AudioProcessor, Vec<f32>) {
        let (controller, mut processor, mut buf) = engine;
        for _ in 0..SAMPLE_RATE as usize / BLOCK {
            processor.process(black_box(&mut buf), 2);
        }
        // Returned, so dropping the engine is not counted.
        (controller, processor, buf)
    }

    library_benchmark_group!(name = dsp; benchmarks = sampler_blocks, synth_blocks, synth_idle_blocks);
    library_benchmark_group!(name = engine; benchmarks = scene_second);
}

#[cfg(target_os = "linux")]
use counts::{dsp, engine};

#[cfg(target_os = "linux")]
gungraun::main!(
    config = gungraun::LibraryBenchmarkConfig::default()
        .tool(gungraun::Callgrind::default().soft_limits([(gungraun::EventKind::Ir, 1.0)]));
    library_benchmark_groups = dsp, engine
);
