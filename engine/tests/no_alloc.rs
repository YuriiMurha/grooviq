//! The audio thread never allocates, frees, locks or does I/O: every `process` call runs inside
//! [`realtime`].
//!
//! Every platform: `assert_no_alloc` aborts this test binary on any Rust allocator call.
//! macOS and Linux with `RTSAN_ENABLE=1` set while building (the `rtsan` CI job):
//! RealtimeSanitizer also stops it on any lock, sleep, file or network access, other blocking
//! system call, or C allocation, with a stack trace. That build leaves allocations to it too, so
//! the sanitizer itself must catch them.

use std::alloc::{GlobalAlloc, Layout, System};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use grooviq_engine::{
    AudioOutput, AudioSlot, ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig,
    EngineState, NoteId, OutputBackend, OutputChoice, OutputInfo, OutputKind, PatternData,
    RecordEvent, SampleBuf, SeqNote, engine, engine_with, run_silent,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const SR: f32 = 48_000.0;

#[path = "realtime/character_delay.rs"]
mod character_delay;

#[path = "realtime/drum_shaper.rs"]
mod drum_shaper;

/// Built with `RTSAN_ENABLE` set: RealtimeSanitizer is linked in and checks [`realtime`].
const RTSAN: bool = option_env!("RTSAN_ENABLE").is_some();

/// `assert_no_alloc`'s checking allocator, except under RealtimeSanitizer, which sees every
/// allocation itself (Rust's and C's): two checkers would leave it nothing to prove.
struct Checked;

// SAFETY: forwards every call unchanged to one of two sound allocators, the same one for a
// block's whole life (`RTSAN` is a constant).
unsafe impl GlobalAlloc for Checked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract, passed on.
        unsafe {
            if RTSAN {
                System.alloc(layout)
            } else {
                AllocDisabler.alloc(layout)
            }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as above.
        unsafe {
            if RTSAN {
                System.alloc_zeroed(layout)
            } else {
                AllocDisabler.alloc_zeroed(layout)
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe {
            if RTSAN {
                System.dealloc(ptr, layout)
            } else {
                AllocDisabler.dealloc(ptr, layout)
            }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as above.
        unsafe {
            if RTSAN {
                System.realloc(ptr, layout, new_size)
            } else {
                AllocDisabler.realloc(ptr, layout, new_size)
            }
        }
    }
}

#[global_allocator]
static A: Checked = Checked;

/// Runs `f` as the audio thread does: no allocation, and under RealtimeSanitizer no blocking
/// call of any kind.
fn realtime<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc(|| {
        rtsan_standalone::realtime_enter();
        let out = f();
        rtsan_standalone::realtime_exit();
        out
    })
}

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
        realtime(|| {
            slot.set_output_latency_frames(256);
            slot.process(&mut buf, 2);
        });
    }
    assert!(c.lock().unwrap().live().peak[0] > 0.1, "it rendered sound");
    drop(out);
    realtime(|| slot.process(&mut buf, 2));
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
        id: NoteId(48),
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
        realtime(|| p.process(&mut buf, 2));
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
            id: NoteId(40),
        },
        Command::Note {
            key: 47,
            velocity: 0.5,
            on: true,
            id: NoteId(47),
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
            id: NoteId(40),
        },
        Command::AllSoundOff,
        Command::Stop,
        #[cfg(feature = "test-hooks")]
        Command::InjectNaN,
    ] {
        c.send(cmd).unwrap();
        for channels in [1, 2, 6] {
            realtime(|| p.process(&mut buf, channels));
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
    let setup = |id, sample: &Arc<SampleBuf>| {
        ChannelSetup::sampler(
            ChannelId(id),
            Some(Arc::clone(sample)),
            Vec::new(),
            ChannelMix::default(),
        )
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
        id: NoteId(60),
    };
    let mut run = |c: &mut grooviq_engine::Controller| {
        for _ in 0..4 {
            realtime(|| p.process(&mut buf, 2));
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
            slide: false,
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
    let setup = |id, sampler: Vec<f32>| {
        ChannelSetup::sampler(
            ChannelId(id),
            Some(steady(4_800)),
            sampler,
            ChannelMix::default(),
        )
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
                    slide: false,
                })
            })
            .collect(),
    };
    let state = EngineState {
        bpm: 400.0,
        channels: vec![setup(1, Vec::new()), setup(2, gate)],
        pattern: dense(3_840, 500),
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
    let mut buf = vec![0f32; 441 * 2];
    let mut run = |c: &mut grooviq_engine::Controller, blocks: usize| {
        for _ in 0..blocks {
            realtime(|| p.process(&mut buf, 2));
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
    let setup = |id, sample: &Arc<SampleBuf>| {
        ChannelSetup::sampler(
            ChannelId(id),
            Some(Arc::clone(sample)),
            Vec::new(),
            ChannelMix::default(),
        )
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
        id: NoteId(60),
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
        realtime(|| p.process(&mut buf, 2));
    }
    assert!(buf.iter().any(|x| *x != 0.0), "the note plays");
    c.check_health();
    assert_eq!(Arc::strong_count(&a), 1, "nothing holds the old sample");
    // The test, the state's two channels and the graph's two nodes.
    assert_eq!(Arc::strong_count(&b), 5);
}

#[test]
fn choke_never_allocates() {
    let sample = tone_sample(9_600);
    let hat = |id, choke| ChannelSetup {
        choke,
        ..ChannelSetup::sampler(
            ChannelId(id),
            Some(Arc::clone(&sample)),
            Vec::new(),
            ChannelMix::default(),
        )
    };
    let synth = ChannelSetup {
        choke: 1,
        ..ChannelSetup::synth(ChannelId(4), Vec::new(), ChannelMix::default())
    };
    // Every 30 ticks, a note on one of four channels of group 1, some of them slides.
    let pattern = PatternData {
        length: 3_840,
        notes: (0..128)
            .map(|i| SeqNote {
                tick: i * 30,
                channel: ChannelId(1 + i % 4),
                key: (40 + i % 12) as u8,
                velocity: 0.9,
                length: 90,
                slide: i % 3 == 0,
            })
            .collect(),
    };
    let state = EngineState {
        bpm: 400.0,
        channels: vec![hat(1, 1), hat(2, 1), hat(3, 1), synth],
        pattern,
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
    let mut buf = vec![0f32; 441 * 2];
    c.send(Command::Play).unwrap();
    for i in 0..200u32 {
        if i % 20 == 0 {
            c.send(Command::SetChoke {
                channel: ChannelId(1 + i / 20 % 4),
                group: (i / 20 % 3) as u8,
            })
            .unwrap();
            c.send(Command::SetChannelParam {
                channel: ChannelId(4),
                index: 20,
                value: 300.0 + i as f32,
            })
            .unwrap();
        }
        if i == 100 {
            // More grouped note-ons in one block than the graph records.
            for channel in 1..=3 {
                for _ in 0..100 {
                    c.send(Command::ChannelNote {
                        channel: ChannelId(channel),
                        key: 60,
                        velocity: 0.5,
                        on: true,
                        id: NoteId(60),
                    })
                    .unwrap();
                }
            }
        }
        assert_no_alloc(|| p.process(&mut buf, 2));
        c.check_health();
    }
    assert!(c.live().peak[0] > 0.05, "it sounded");
}

#[test]
fn recording_never_allocates() {
    // Also the metronome, AllSoundOff's choke and the fade of a removed channel.
    let setup = |id, sample: &Arc<SampleBuf>| {
        ChannelSetup::sampler(
            ChannelId(id),
            Some(Arc::clone(sample)),
            Vec::new(),
            ChannelMix::default(),
        )
    };
    let synth = ChannelSetup::synth(ChannelId(3), Vec::new(), ChannelMix::default());
    let a = tone_sample(48_000);
    let state = EngineState {
        bpm: 400.0,
        channels: vec![setup(1, &a), setup(2, &a), synth],
        time_signature: (7, 16),
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
    c.live_shared().set_output_latency_frames(512);
    let mut buf = vec![0f32; 441 * 2];
    let note = |id, on| Command::ChannelNote {
        channel: ChannelId(id),
        key: 60,
        velocity: 0.9,
        on,
        id: NoteId(60),
    };
    let mut run = |c: &mut grooviq_engine::Controller, callbacks: usize| {
        for _ in 0..callbacks {
            realtime(|| p.process(&mut buf, 2));
        }
        c.check_health();
    };
    c.send(Command::SetMetronome { on: true }).unwrap();
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    // More notes than the recording queue holds, never drained: the rest are counted.
    for i in 0..1_100 {
        c.send(note(1 + i % 3, i % 2 == 0)).unwrap();
        if i % 100 == 99 {
            run(&mut c, 1);
        }
    }
    run(&mut c, 20);
    assert!(c.record_dropped() > 0, "the queue filled up");
    let mut taken = Vec::new();
    c.take_recorded(&mut taken);
    // More tempo changes than the recording's tempo history keeps, with notes heard across
    // them.
    for i in 0..80 {
        c.send(Command::SetTempo {
            bpm: 90.0 + i as f32,
        })
        .unwrap();
        c.send(note(2, i % 2 == 0)).unwrap();
        // Pattern resizes while recording are segments of the history too.
        if i % 3 == 0 {
            c.set_pattern(PatternData {
                length: 240 * (1 + i % 32),
                notes: Vec::new(),
            })
            .unwrap();
        }
        run(&mut c, 1);
    }
    c.send(Command::SetTimeSignature { beats: 3, unit: 4 })
        .unwrap();
    c.send(Command::SetRecording { on: false }).unwrap();
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(note(2, true)).unwrap();
    c.send(Command::AllSoundOff).unwrap();
    c.send(note(2, true)).unwrap();
    c.send(Command::Play).unwrap();
    run(&mut c, 5);
    // A sounding channel fades out, then goes back through the trash queue.
    c.send(note(1, true)).unwrap();
    run(&mut c, 1);
    c.remove_channel(ChannelId(1)).unwrap();
    run(&mut c, 5);
    // Stopping, and switching off, fade a click that is sounding.
    c.send(Command::Stop).unwrap();
    run(&mut c, 5);
    c.send(Command::Play).unwrap();
    run(&mut c, 1);
    c.send(Command::SetMetronome { on: false }).unwrap();
    run(&mut c, 5);
    assert!(c.live().peak[0] > 0.05, "it sounded");
    assert!(c.take_recorded(&mut taken) > 0);
    assert_eq!(
        Arc::strong_count(&a),
        3,
        "the test and channel 2's state and node: the removed node was freed"
    );
}

#[test]
fn a_waiting_recording_barrier_never_allocates() {
    let state = || EngineState {
        channels: vec![ChannelSetup::synth(
            ChannelId(1),
            Vec::new(),
            ChannelMix::default(),
        )],
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state());
    let mut buf = vec![0f32; 64 * 2];
    let note = |on| Command::ChannelNote {
        channel: ChannelId(1),
        key: 60,
        velocity: 0.9,
        on,
        id: NoteId(60),
    };
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    // Fill the recording queue, never drained.
    for i in 0..1_100 {
        c.send(note(i % 2 == 0)).unwrap();
        if i % 100 == 99 {
            realtime(|| p.process(&mut buf, 2));
        }
    }
    realtime(|| p.process(&mut buf, 2));
    assert!(c.record_dropped() > 0, "the queue is full");
    // A new state parks its barrier; recording goes on in the new take.
    c.load_state(state()).unwrap();
    let mark = c.record_mark();
    c.send(Command::Play).unwrap();
    for _ in 0..4 {
        c.send(note(true)).unwrap();
        c.send(note(false)).unwrap();
        realtime(|| p.process(&mut buf, 2));
    }
    // Room again: the barrier goes out from the next push or block start.
    let mut taken = Vec::new();
    c.take_recorded(&mut taken);
    c.send(note(true)).unwrap();
    realtime(|| p.process(&mut buf, 2));
    c.check_health();
    taken.clear();
    c.take_recorded(&mut taken);
    assert_eq!(
        taken.first(),
        Some(&RecordEvent::Mark { id: mark }),
        "{taken:?}"
    );
}

#[path = "realtime/multiband.rs"]
mod multiband;
