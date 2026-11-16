//! The audio thread never allocates or frees: every `process` call runs inside
//! `assert_no_alloc`, which aborts this test binary on any allocator call.

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use assert_no_alloc::assert_no_alloc;
use grooviq_engine::{
    AudioOutput, AudioSlot, Command, EngineConfig, OutputBackend, OutputChoice, OutputInfo,
    OutputKind, engine,
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

    fn open(&mut self, _info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(slot);
        Ok(())
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
