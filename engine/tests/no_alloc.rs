//! The audio thread never allocates or frees: every `process` call runs inside
//! `assert_no_alloc`, which aborts this test binary on any allocator call.

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use assert_no_alloc::assert_no_alloc;
use grooviq_engine::{
    AudioOutput, AudioSlot, ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig,
    EngineState, OutputBackend, OutputChoice, OutputInfo, OutputKind, PatternData, SampleBuf,
    SeqNote, engine, engine_with, run_silent,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const SR: f32 = 48_000.0;

/// One fake output that keeps the slot it is opened with.
struct OneDevice(Arc<Mutex<Option<Arc<AudioSlot>>>>);

impl OutputBackend for OneDevice {
    fn outputs(&self) -> Vec<OutputInfo> {
        vec![OutputInfo {
            id: "speakers".into(),
            name: "Speakers".into(),
            kind: OutputKind::Speakers,
            is_default: true,
            sample_rate: 48_000,
        }]
    }

    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<u32, String> {
        *self.0.lock().unwrap() = Some(slot);
        Ok(info.sample_rate)
    }

    fn close(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

#[test]
fn the_device_callback_path_is_allocation_free() {
    let (mut c, p) = engine(EngineConfig { sample_rate: SR });
    c.send(Command::TestTone {
        on: true,
        hz: 440.0,
        gain_db: -12.0,
    })
    .unwrap();
    let opened = Arc::new(Mutex::new(None));
    let backend = Box::new(OneDevice(Arc::clone(&opened)));
    let c = Arc::new(Mutex::new(c));
    let out = AudioOutput::start(
        backend,
        Arc::clone(&c),
        p,
        OutputChoice::SystemDefault,
        true,
        Box::new(|_| {}),
    );
    let slot = opened.lock().unwrap().clone().expect("the device opened");
    // What the cpal callback runs: with the processor in the slot, then (below) emptied.
    let mut buf = vec![0f32; 441 * 2];
    for _ in 0..100 {
        assert_no_alloc(|| {
            slot.set_output_latency_frames(256);
            slot.process(&mut buf, 2);
        });
    }
    assert!(c.lock().unwrap().live().peak[0] > 0.1, "it rendered sound");
    drop(out);
    assert_no_alloc(|| slot.process(&mut buf, 2));
    assert!(buf.iter().all(|x| *x == 0.0), "an emptied slot is silent");
}

#[test]
fn the_silent_output_loop_is_allocation_free() {
    let (mut c, p) = engine(EngineConfig { sample_rate: SR });
    c.send(Command::TestTone {
        on: true,
        hz: 440.0,
        gain_db: -12.0,
    })
    .unwrap();
    let opened = Arc::new(Mutex::new(None));
    let backend = Box::new(OneDevice(Arc::clone(&opened)));
    let c = Arc::new(Mutex::new(c));
    let out = AudioOutput::start(
        backend,
        Arc::clone(&c),
        p,
        OutputChoice::SystemDefault,
        true,
        Box::new(|_| {}),
    );
    let slot = opened.lock().unwrap().clone().expect("the device opened");
    // The loop the silent backend runs on its own thread, here on this one: paced callbacks
    // until told to quit, once the stopper has seen 8 of them (or after 10 s, so a loop that
    // renders nothing fails below instead of hanging).
    let live = c.lock().unwrap().live_shared();
    let start = live.snapshot().frames;
    let quit = Arc::new(AtomicBool::new(false));
    let stopper = thread::spawn({
        let (quit, live) = (Arc::clone(&quit), Arc::clone(&live));
        move || {
            let since = Instant::now();
            while live.snapshot().frames < start + 8 * 256
                && since.elapsed() < Duration::from_secs(10)
            {
                thread::sleep(Duration::from_millis(1));
            }
            quit.store(true, Ordering::Release);
        }
    });
    assert_no_alloc(|| run_silent(&slot, &quit, 48_000));
    stopper.join().unwrap();
    let live = c.lock().unwrap().live();
    assert!(
        live.frames >= start + 8 * 256,
        "it rendered: {} frames",
        live.frames - start
    );
    assert!(live.peak[0] > 0.1, "it rendered sound");
    drop(out);
}

#[test]
fn process_never_allocates() {
    let (mut c, mut p) = engine(EngineConfig { sample_rate: SR });
    c.send(Command::TestTone {
        on: true,
        hz: 440.0,
        gain_db: -12.0,
    })
    .unwrap();
    c.send(Command::Note {
        key: 48,
        velocity: 0.8,
        on: true,
    })
    .unwrap();
    let mut buf = vec![0f32; 441 * 2];
    let callbacks = 2 * SR as usize / 441 + 1; // 2 s
    for i in 0..callbacks {
        if i % 10 == 0 {
            let cutoff = 200.0 + 40.0 * i as f32;
            c.send(Command::SetSynthParam {
                index: 20,
                value: cutoff,
            })
            .unwrap();
        }
        assert_no_alloc(|| p.process(&mut buf, 2));
    }
    assert!(c.live().peak[0] > 0.1, "it rendered sound");
}

#[test]
fn every_command_and_channel_layout_is_allocation_free() {
    let (mut c, mut p) = engine(EngineConfig { sample_rate: SR });
    assert!(c.synth_preset("Reese"));
    let mut buf = vec![0f32; 1023];
    for cmd in [
        Command::Play,
        Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        },
        Command::Note {
            key: 40,
            velocity: 1.0,
            on: true,
        },
        Command::Note {
            key: 47,
            velocity: 0.5,
            on: true,
        },
        // A voice-mode change releases the sounding voices.
        Command::SetSynthParam {
            index: 38,
            value: 0.0,
        },
        Command::Note {
            key: 40,
            velocity: 0.0,
            on: false,
        },
        Command::AllSoundOff,
        Command::Stop,
        #[cfg(feature = "test-hooks")]
        Command::InjectNaN,
    ] {
        c.send(cmd).unwrap();
        for channels in [1, 2, 6] {
            assert_no_alloc(|| p.process(&mut buf, channels));
        }
    }
}

fn tone_sample(len: usize) -> Arc<SampleBuf> {
    let l: Vec<f32> = (0..len)
        .map(|i| libm::sinf(i as f32 * 0.05) * 0.5)
        .collect();
    Arc::new(SampleBuf {
        rate: SR,
        r: l.clone(),
        l,
    })
}

#[test]
fn channel_messages_never_allocate_on_the_audio_thread() {
    let setup = |id, sample: &Arc<SampleBuf>| ChannelSetup {
        id: ChannelId(id),
        sample: Some(Arc::clone(sample)),
        sampler: Vec::new(),
        mix: ChannelMix::default(),
    };
    let (a, b) = (tone_sample(48_000), tone_sample(4_800));
    let state = EngineState {
        channels: vec![setup(1, &a), setup(2, &b)],
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
    let mut buf = vec![0f32; 441 * 2];
    let note = |id, on| Command::ChannelNote {
        channel: ChannelId(id),
        key: 60,
        velocity: 0.9,
        on,
    };
    let mut run = |c: &mut grooviq_engine::Controller| {
        for _ in 0..4 {
            assert_no_alloc(|| p.process(&mut buf, 2));
        }
        // Frees what the processor handed back, outside the check.
        c.check_health();
    };
    c.send(note(1, true)).unwrap();
    c.send(note(2, true)).unwrap();
    run(&mut c);
    let solo = ChannelMix {
        volume: 1.5,
        pan: -0.4,
        mute: false,
        solo: true,
    };
    c.send(Command::SetChannelMix {
        channel: ChannelId(2),
        mix: solo,
    })
    .unwrap();
    c.send(Command::SetSamplerParam {
        channel: ChannelId(1),
        index: 1,
        value: 7.0,
    })
    .unwrap();
    run(&mut c);
    c.add_channel(setup(3, &a)).unwrap();
    c.send(note(3, true)).unwrap();
    run(&mut c);
    c.set_channel_sample(ChannelId(1), Some(tone_sample(9_000)))
        .unwrap();
    c.set_channel_sample(ChannelId(3), None).unwrap();
    c.send(note(1, true)).unwrap();
    run(&mut c);
    c.set_pattern(PatternData {
        length: 3_840,
        notes: vec![SeqNote {
            tick: 0,
            channel: ChannelId(1),
            key: 60,
            velocity: 0.8,
            length: 240,
        }],
    })
    .unwrap();
    c.send(Command::AllSoundOff).unwrap();
    run(&mut c);
    c.remove_channel(ChannelId(2)).unwrap();
    c.send(note(1, false)).unwrap();
    run(&mut c);
    c.load_state(EngineState::default()).unwrap();
    run(&mut c);
    assert_eq!(
        Arc::strong_count(&a),
        1,
        "every node came back and was freed"
    );
    assert_eq!(Arc::strong_count(&b), 1);
}

#[test]
fn sequencing_never_allocates() {
    // Steady, so the default de-click fade-in still lets it be heard.
    let steady = |len: usize| {
        Arc::new(SampleBuf {
            rate: SR,
            l: vec![0.5; len],
            r: vec![0.5; len],
        })
    };
    let gate = {
        // Gate mode, so held notes wait for their note-offs.
        let mut p: Vec<f32> = grooviq_dsp::sampler::SPEC
            .iter()
            .map(|&(_, _, d)| d)
            .collect();
        p[7] = 0.0;
        p
    };
    let setup = |id, sampler: Vec<f32>| ChannelSetup {
        id: ChannelId(id),
        sample: Some(steady(4_800)),
        sampler,
        mix: ChannelMix::default(),
    };
    // Dense: every tick of 60 on 2 channels, held across each other.
    let dense = |length: u32, hold: u32| PatternData {
        length,
        notes: (0..length / 60)
            .flat_map(|i| {
                [1, 2].map(|ch| SeqNote {
                    tick: i * 60,
                    channel: ChannelId(ch),
                    key: (36 + i % 24) as u8,
                    velocity: 0.9,
                    length: hold,
                })
            })
            .collect(),
    };
    let state = EngineState {
        bpm: 400.0,
        channels: vec![setup(1, Vec::new()), setup(2, gate)],
        pattern: dense(3_840, 500),
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
    let mut buf = vec![0f32; 441 * 2];
    let mut run = |c: &mut grooviq_engine::Controller, blocks: usize| {
        for _ in 0..blocks {
            assert_no_alloc(|| p.process(&mut buf, 2));
        }
        c.check_health();
    };
    c.send(Command::Play).unwrap();
    run(&mut c, 20);
    assert!(c.live().position_ticks > 0, "it played");
    assert!(c.live().peak[0] > 0.1, "it sounded");
    c.send(Command::SetTempo { bpm: 77.0 }).unwrap();
    run(&mut c, 20);
    c.set_pattern(dense(1_920, 2_000)).unwrap();
    run(&mut c, 20);
    c.remove_channel(ChannelId(2)).unwrap();
    run(&mut c, 10);
    c.send(Command::Stop).unwrap();
    run(&mut c, 5);
    c.send(Command::PlayLoops { loops: 2 }).unwrap();
    run(&mut c, 40);
    c.send(Command::Play).unwrap();
    c.send(Command::AllSoundOff).unwrap();
    run(&mut c, 5);
    c.load_state(EngineState::default()).unwrap();
    c.send(Command::Play).unwrap();
    run(&mut c, 5);
}

#[test]
fn messages_queued_before_a_rate_change_never_free_on_the_audio_thread() {
    let setup = |id, sample: &Arc<SampleBuf>| ChannelSetup {
        id: ChannelId(id),
        sample: Some(Arc::clone(sample)),
        sampler: Vec::new(),
        mix: ChannelMix::default(),
    };
    let (a, b) = (tone_sample(4_800), tone_sample(9_000));
    let state = EngineState {
        channels: vec![setup(1, &a), setup(2, &a)],
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
    let note = Command::ChannelNote {
        channel: ChannelId(1),
        key: 60,
        velocity: 0.9,
        on: true,
    };
    // Queued, never processed: a remove, an add, a new sample, a new pattern, a note.
    c.remove_channel(ChannelId(1)).unwrap();
    c.add_channel(setup(1, &b)).unwrap();
    c.set_channel_sample(ChannelId(2), Some(Arc::clone(&b)))
        .unwrap();
    c.set_pattern(PatternData {
        length: 1_920,
        notes: Vec::new(),
    })
    .unwrap();
    c.send(note).unwrap();
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44_100.0,
        },
    );
    let mut buf = vec![0f32; 441 * 2];
    for _ in 0..4 {
        assert_no_alloc(|| p.process(&mut buf, 2));
    }
    assert!(buf.iter().any(|x| *x != 0.0), "the note plays");
    c.check_health();
    assert_eq!(Arc::strong_count(&a), 1, "nothing holds the old sample");
    // The test, the state's two channels and the graph's two nodes.
    assert_eq!(Arc::strong_count(&b), 5);
}
