//! Panic isolation, in its own test binary: the panic hook is process-global, so this binary
//! installs one filter, once, that silences the injected panic only (any other panic still
//! prints).

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig, EngineState, HealthEvent,
    INJECTED_PANIC, NoteId, SampleBuf, engine, engine_with, in_audio_callback,
};
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Once};

/// Injected panics the hook saw with the rendering flag on, and with it off.
static INSIDE: AtomicU32 = AtomicU32::new(0);
static OUTSIDE: AtomicU32 = AtomicU32::new(0);

const SR: f32 = 48_000.0;

fn cfg() -> EngineConfig {
    EngineConfig { sample_rate: SR }
}

fn tone(on: bool) -> Command {
    Command::TestTone {
        on,
        hz: 440.0,
        gain_db: -12.0,
    }
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, x| m.max(x.abs()))
}

/// Installs the filtering hook before the first injected panic (tests run in parallel).
fn silence_injected_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload_as_str() != Some(INJECTED_PANIC) {
                default_hook(info);
            } else if in_audio_callback() {
                INSIDE.fetch_add(1, SeqCst);
            } else {
                OUTSIDE.fetch_add(1, SeqCst);
            }
        }));
    });
}

#[test]
fn a_panic_while_rendering_sees_the_rendering_flag() {
    silence_injected_panics();
    let (mut c, mut p) = engine(cfg());
    c.send(Command::InjectPanic).unwrap();
    let before = INSIDE.load(SeqCst);
    p.process(&mut [0f32; 128], 2);
    assert!(
        INSIDE.load(SeqCst) > before,
        "the hook ran inside `process`"
    );
    assert_eq!(
        OUTSIDE.load(SeqCst),
        0,
        "every injected panic is raised while rendering"
    );
    assert!(
        !in_audio_callback(),
        "and the flag is off again after `process`"
    );
}

#[test]
fn a_panicking_node_is_isolated_and_the_graph_restarts() {
    silence_injected_panics();
    let (mut c, mut p) = engine(cfg());
    c.send(tone(true)).unwrap();
    c.send(Command::InjectPanic).unwrap();
    let mut buf = vec![0f32; 1024 * 2];
    p.process(&mut buf, 2);
    // The panic hits the first block, so everything from there on is silent.
    assert!(buf.iter().all(|x| *x == 0.0));
    assert_eq!(c.live().faults, 1);
    assert!(
        c.check_health()
            .contains(&HealthEvent::Restarted { faults: 1 })
    );
    // Reported once.
    assert!(c.check_health().is_empty());
    // The swap hands the old graph back to the controller: nothing is freed on the audio thread.
    let mut buf = vec![0f32; 4096 * 2];
    assert_no_alloc::assert_no_alloc(|| p.process(&mut buf, 2));
    assert!(peak(&buf) > 0.2, "peak {}", peak(&buf));
}

#[test]
fn a_restart_restores_the_state_the_commands_set() {
    silence_injected_panics();
    // Audible values on parameters across the table (none touch the LFO, whose phase depends
    // on how long a synth has run), then the tone on and everything off again.
    let setup = [
        Command::SetSynthParam {
            index: 0,
            value: 3.0,
        }, // osc 1 square
        Command::SetSynthParam {
            index: 5,
            value: 7.0,
        }, // osc 1 seven unison voices
        Command::SetSynthParam {
            index: 20,
            value: 300.0,
        }, // dark cutoff
        Command::SetSynthParam {
            index: 27,
            value: 1.0,
        }, // full sustain
        Command::SetSynthParam {
            index: 39,
            value: 6.0,
        }, // master +6 dB
        tone(true),
        Command::AllSoundOff,
    ];
    let note = Command::Note {
        key: 48,
        velocity: 1.0,
        on: true,
        id: NoteId(48),
    };
    let mut buf = vec![0f32; 1024 * 2]; // 16 blocks: the AllSoundOff fade and reset are over

    // Reference: the same commands to an engine that never panicked.
    let (mut rc, mut rp) = engine(cfg());
    for cmd in setup {
        rc.send(cmd).unwrap();
    }
    rp.process(&mut buf, 2);
    rc.send(note).unwrap();
    let mut expected = vec![0f32; 4096 * 2];
    rp.process(&mut expected, 2);
    assert!(peak(&expected) > 0.05, "the note is audible");

    let (mut c, mut p) = engine(cfg());
    for cmd in setup {
        c.send(cmd).unwrap();
    }
    c.send(Command::InjectPanic).unwrap();
    p.process(&mut buf, 2);
    assert!(
        c.check_health()
            .contains(&HealthEvent::Restarted { faults: 1 })
    );
    p.process(&mut buf, 2);
    assert!(!c.live().tone, "AllSoundOff switched the tone off");
    c.send(note).unwrap();
    let mut actual = vec![0f32; 4096 * 2];
    p.process(&mut actual, 2);
    assert!(
        actual == expected,
        "the rebuilt graph plays like the original"
    );
}

#[test]
fn reconfigure_restarts_a_faulted_graph_without_check_health() {
    silence_injected_panics();
    let (mut c, mut p) = engine(cfg());
    c.send(tone(true)).unwrap();
    c.send(Command::InjectPanic).unwrap();
    let mut buf = vec![0f32; 64 * 2];
    p.process(&mut buf, 2);
    assert_eq!(c.live().faults, 1);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44_100.0,
        },
    );
    let mut buf = vec![0f32; 4_410 * 2];
    p.process(&mut buf, 2);
    assert!(peak(&buf) > 0.2, "peak {}", peak(&buf));
}

#[test]
fn a_fault_cleared_by_reconfigure_is_reported_but_never_restarts_the_new_graph() {
    silence_injected_panics();
    let (mut c, mut p) = engine(cfg());
    c.send(Command::InjectPanic).unwrap();
    let mut buf = vec![0f32; 64 * 2];
    p.process(&mut buf, 2);
    assert_eq!(c.live().faults, 1);
    // The output device changes (same rate) before the next health check.
    c.reconfigure(&mut p, cfg());
    let note = Command::Note {
        key: 48,
        velocity: 1.0,
        on: true,
        id: NoteId(48),
    };
    c.send(note).unwrap();
    let mut buf = vec![0f32; 1024 * 2];
    p.process(&mut buf, 2);
    assert!(peak(&buf) > 0.05, "the note sounds");
    // The panic is still reported, once, but reconfigure already restarted the graph...
    assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
    assert!(c.check_health().is_empty());
    // ...so no second restart cuts the held note.
    let mut buf = vec![0f32; 4096 * 2];
    p.process(&mut buf, 2);
    assert!(peak(&buf) > 0.05, "held note: {}", peak(&buf));
}

#[test]
fn a_panic_restart_restores_channels_samples_and_mix() {
    silence_injected_panics();
    let l: Vec<f32> = (0..2_000)
        .map(|i| libm::sinf(i as f32 * 0.03) * 0.7)
        .collect();
    let r: Vec<f32> = l.iter().map(|x| -0.5 * x).collect();
    let first = Arc::new(SampleBuf {
        rate: SR,
        l: vec![0.3; 100],
        r: vec![0.3; 100],
    });
    let sample = Arc::new(SampleBuf { rate: SR, l, r });
    let state = EngineState {
        channels: vec![ChannelSetup::sampler(
            ChannelId(4),
            Some(Arc::clone(&first)),
            Vec::new(),
            ChannelMix::default(),
        )],
        ..EngineState::default()
    };
    let swap = |c: &mut grooviq_engine::Controller| {
        c.set_channel_sample(ChannelId(4), Some(Arc::clone(&sample)))
            .unwrap();
    };
    // Changed after creation, so only the controller's mirror carries them into the rebuild.
    let setup = [
        Command::SetChannelMix {
            channel: ChannelId(4),
            mix: ChannelMix {
                volume: 0.5,
                pan: 0.6,
                mute: false,
                solo: false,
            },
        },
        // Tune +7 semitones, release 300 ms, gate mode.
        Command::SetSamplerParam {
            channel: ChannelId(4),
            index: 1,
            value: 7.0,
        },
        Command::SetSamplerParam {
            channel: ChannelId(4),
            index: 11,
            value: 300.0,
        },
        Command::SetSamplerParam {
            channel: ChannelId(4),
            index: 7,
            value: 0.0,
        },
    ];
    let note = Command::ChannelNote {
        channel: ChannelId(4),
        key: 64,
        velocity: 0.7,
        on: true,
        id: NoteId(64),
    };
    let mut buf = vec![0f32; 1024 * 2];

    let (mut rc, mut rp) = engine_with(cfg(), state.clone());
    swap(&mut rc);
    for cmd in setup {
        rc.send(cmd).unwrap();
    }
    rp.process(&mut buf, 2);
    rc.send(note).unwrap();
    let mut expected = vec![0f32; 4096 * 2];
    rp.process(&mut expected, 2);
    assert!(peak(&expected) > 0.05, "the note is audible");

    let (mut c, mut p) = engine_with(cfg(), state);
    swap(&mut c);
    for cmd in setup {
        c.send(cmd).unwrap();
    }
    c.send(Command::InjectPanic).unwrap();
    p.process(&mut buf, 2);
    assert!(
        c.check_health()
            .contains(&HealthEvent::Restarted { faults: 1 })
    );
    p.process(&mut buf, 2);
    c.send(note).unwrap();
    let mut actual = vec![0f32; 4096 * 2];
    p.process(&mut actual, 2);
    assert!(
        actual == expected,
        "the rebuilt channel plays like the original"
    );
    // The faulted graph went back to the controller, so only the test, both states and both
    // live graphs hold the sample now (after the next free); the reference engine still holds
    // the replaced sample in its trash queue.
    c.check_health();
    assert_eq!(Arc::strong_count(&sample), 5);
    assert_eq!(Arc::strong_count(&first), 2);
}

#[test]
fn a_panic_restart_restores_synth_channels() {
    silence_injected_panics();
    let mut reese: Vec<f32> = grooviq_dsp::synth::SPEC.iter().map(|s| s.2).collect();
    for (index, value) in grooviq_dsp::synth::preset("Reese").unwrap() {
        reese[index as usize] = value;
    }
    let state = EngineState {
        channels: vec![
            ChannelSetup::synth(ChannelId(2), reese, ChannelMix::default()),
            ChannelSetup::synth(ChannelId(3), Vec::new(), ChannelMix::default()),
        ],
        ..EngineState::default()
    };
    // Changed after creation, so only the controller's mirror carries them into the rebuild:
    // a cutoff and a release on channel 2, and both channels in choke group 5.
    let setup = [
        Command::SetChannelParam {
            channel: ChannelId(2),
            index: 20,
            value: 700.0,
        },
        Command::SetChannelParam {
            channel: ChannelId(2),
            index: 28,
            value: 900.0,
        },
        Command::SetChoke {
            channel: ChannelId(2),
            group: 5,
        },
        Command::SetChoke {
            channel: ChannelId(3),
            group: 5,
        },
    ];
    let note = |channel, key| Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.9,
        on: true,
        id: NoteId(u32::from(key)),
    };
    let mut buf = vec![0f32; 1024 * 2];
    // Channel 2 sounds, then channel 3 chokes it.
    let play = |c: &mut grooviq_engine::Controller, p: &mut grooviq_engine::AudioProcessor| {
        c.send(note(2, 40)).unwrap();
        let mut out = vec![0f32; 4096 * 2];
        p.process(&mut out[..2048 * 2], 2);
        c.send(note(3, 64)).unwrap();
        p.process(&mut out[2048 * 2..], 2);
        out
    };

    let (mut rc, mut rp) = engine_with(cfg(), state.clone());
    for cmd in setup {
        rc.send(cmd).unwrap();
    }
    rp.process(&mut buf, 2);
    let expected = play(&mut rc, &mut rp);
    assert!(peak(&expected[..2048 * 2]) > 0.05, "the note is audible");

    let (mut c, mut p) = engine_with(cfg(), state);
    for cmd in setup {
        c.send(cmd).unwrap();
    }
    c.send(Command::InjectPanic).unwrap();
    p.process(&mut buf, 2);
    assert!(
        c.check_health()
            .contains(&HealthEvent::Restarted { faults: 1 })
    );
    p.process(&mut buf, 2);
    let actual = play(&mut c, &mut p);
    assert!(
        actual == expected,
        "the rebuilt synth channels play like the originals"
    );
}
