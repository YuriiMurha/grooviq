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
#[path = "support/song_workload.rs"]
mod song_workload;

#[cfg(target_os = "linux")]
mod counts {
    use super::sampler_workload::scenario;
    use std::hint::black_box;
    use std::sync::Arc;

    use grooviq_dsp::sampler::{PARAM_IDS, SampleBuf, Sampler, VOICES};
    use grooviq_dsp::synth::{Synth, preset};
    use grooviq_dsp::{Device, NoteEvent};
    use grooviq_engine::{
        AudioProcessor, ChannelId, ChannelMix, ChannelSetup, Command, Controller, EngineConfig,
        EngineState, NoteId, PlaybackMode, engine_with,
    };
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

    /// A sampler playing stereo noise, with distinct note IDs for each voice.
    fn sampler(voices: usize, fixed_step: Option<f32>) -> Sampler {
        let mut x = 0x2545_f491_u32;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32) - 0.5
        };
        // Leave an extra block past the measurement so fixed-step voices stay in the interior.
        let output_frames = (SETTLE_BLOCKS + COUNTED_BLOCKS + 1) * BLOCK;
        let source_frames = output_frames as f32 * fixed_step.unwrap_or(1.0);
        let len = (2 * SAMPLE_RATE as usize).max(source_frames.ceil() as usize);
        let l: Vec<f32> = (0..len).map(|_| next()).collect();
        let r: Vec<f32> = (0..len).map(|_| next()).collect();
        let mut s = Sampler::new(SR);
        if fixed_step.is_some() {
            let pitch_track = PARAM_IDS
                .iter()
                .position(|id| *id == "pitch_track")
                .unwrap();
            s.set_param(pitch_track as u32, 0.0);
        }
        let rate = SR * fixed_step.unwrap_or(1.0);
        s.set_sample(Some(Arc::new(SampleBuf { rate, l, r })));
        let s = settle(s, &chord(55, voices));
        assert_eq!(s.active_voices(), voices);
        s
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
    #[bench::voices_0(sampler(0, None))]
    #[bench::voices_2(sampler(2, None))]
    #[bench::voices_16(sampler(16, None))]
    #[bench::step_4_voices_16(sampler(16, Some(4.0)))]
    #[bench::step_16_voices_16(sampler(16, Some(16.0)))]
    #[bench::filter_static_16(scenario(0.0, false))]
    #[bench::filter_envelope_16(scenario(0.7, false))]
    #[bench::filter_velocity_16(scenario(0.7, true))]
    fn sampler_blocks(mut s: Sampler) -> f32 {
        render(&mut s)
    }

    fn mono_glide_and_steals() -> (Sampler, [NoteEvent; 48]) {
        let mut s = sampler(1, None);
        for (id, value) in [("voice_mode", 1.0), ("glide", 2.5)] {
            let i = PARAM_IDS.iter().position(|&key| key == id).unwrap();
            s.set_param(i as u32, value);
        }
        let notes = std::array::from_fn(|i| NoteEvent {
            offset: i as u32,
            key: 55 + (i % 16) as u8,
            velocity: 0.8,
            on: true,
            slide: false,
            id: 100 + i as u64,
        });
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        for _ in 0..3 {
            s.process(&mut l, &mut r, None, &notes);
        }
        assert_eq!(s.active_voices(), VOICES * 4 + 1);
        assert!(l.iter().chain(&r).any(|v| v.abs() > 1e-6));
        (s, notes)
    }

    #[library_benchmark]
    #[bench::mono_glide_fade_pressure(mono_glide_and_steals())]
    fn sampler_event_blocks(workload: (Sampler, [NoteEvent; 48])) -> f32 {
        let (mut s, notes) = workload;
        let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
        for _ in 0..COUNTED_BLOCKS {
            s.process(black_box(&mut l), black_box(&mut r), None, &notes);
        }
        l[BLOCK - 1] + r[BLOCK - 1]
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

    fn synth_scene(with_chord: bool) -> (Controller, AudioProcessor, Vec<f32>) {
        let channels = (1..=16)
            .map(|id| ChannelSetup::synth(ChannelId(id), Vec::new(), ChannelMix::default()))
            .collect();
        let state = EngineState {
            channels,
            ..EngineState::default()
        };
        let (mut controller, mut processor) = engine_with(EngineConfig { sample_rate: SR }, state);
        if with_chord {
            for key in [48, 52, 55, 60] {
                controller
                    .send(Command::ChannelNote {
                        channel: ChannelId(1),
                        key,
                        velocity: 0.8,
                        on: true,
                        id: NoteId::live(),
                    })
                    .expect("the chord fits the command queue");
            }
        }
        let mut buf = vec![0.0; BLOCK * 2];
        for _ in 0..SETTLE_BLOCKS {
            processor.process(&mut buf, 2);
        }
        assert_eq!(buf.iter().any(|&sample| sample != 0.0), with_chord);
        (controller, processor, buf)
    }

    // One second of the scene, in 64-frame `process` calls.
    #[library_benchmark]
    #[bench::drums_4ch(scene("drums_4ch"))]
    #[bench::beat_16ch(scene("beat_16ch"))]
    #[bench::synth_16ch_idle(synth_scene(false))]
    #[bench::synth_16ch_chord(synth_scene(true))]
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

    const SONG_BLOCK: usize = 128;
    const SONG_WINDOW_TICKS: [u64; 3] = [26_860, 40_300, 134_380];
    const SONG_WINDOW_BLOCKS: usize =
        10 * SAMPLE_RATE as usize / SONG_WINDOW_TICKS.len() / SONG_BLOCK;
    type SongWindow = (Controller, AudioProcessor, [f32; SONG_BLOCK * 2]);

    fn song_windows() -> [SongWindow; SONG_WINDOW_TICKS.len()] {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::song_workload::reopen(dir.path())
            .checked_export_state()
            .unwrap();
        assert_eq!(state.playback_mode, PlaybackMode::Song);
        assert!(state.song_loop.is_none(), "expected a single Song pass");
        // Start just before the overlapping Water clips at 14, 21 and 70 s.
        // These windows include both playback directions, fades, automation priority changes
        // and both occupied and empty pattern bars. Process the transport commands before the
        // first clip starts, so seeking and starting playback are outside the measured loop.
        SONG_WINDOW_TICKS.map(|tick| {
            let (mut controller, mut processor) =
                engine_with(EngineConfig { sample_rate: SR }, state.clone());
            controller.send(Command::Play).unwrap();
            controller.send(Command::Seek { tick }).unwrap();
            let mut buf = [0.0; SONG_BLOCK * 2];
            processor.process(&mut buf, 2);
            (controller, processor, buf)
        })
    }

    #[library_benchmark]
    #[bench::song_m3(song_windows())]
    fn song_blocks(
        mut windows: [SongWindow; SONG_WINDOW_TICKS.len()],
    ) -> [SongWindow; SONG_WINDOW_TICKS.len()] {
        for (_, processor, buf) in &mut windows {
            for _ in 0..SONG_WINDOW_BLOCKS {
                processor.process(black_box(&mut buf[..]), 2);
            }
        }
        windows
    }

    library_benchmark_group!(name = dsp; benchmarks = sampler_blocks, sampler_event_blocks, synth_blocks, synth_idle_blocks);
    library_benchmark_group!(name = engine; benchmarks = scene_second, song_blocks);
}

#[cfg(target_os = "linux")]
use counts::{dsp, engine};

#[cfg(target_os = "linux")]
gungraun::main!(
    config = gungraun::LibraryBenchmarkConfig::default()
        .tool(gungraun::Callgrind::default().soft_limits([(gungraun::EventKind::Ir, 1.0)]));
    library_benchmark_groups = dsp, engine
);
