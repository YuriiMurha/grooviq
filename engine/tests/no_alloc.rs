//! The audio thread never allocates or frees: every `process` call runs inside
//! `assert_no_alloc`, which aborts this test binary on any allocator call.

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use assert_no_alloc::assert_no_alloc;
use grooviq_engine::{
    AudioOutput, AudioSlot, ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig,
    EngineState, OutputBackend, OutputChoice, OutputInfo, OutputKind, PatternData, SampleBuf,
    SeqNote, engine, engine_with,
};
use std::sync::{Arc, Mutex};

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
    let l: Vec<f32> = (0..len).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
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
