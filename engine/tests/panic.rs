//! Panic isolation, in its own test binary: the panic hook is process-global, so this binary
//! installs one filter, once, that silences the injected panic only (any other panic still
//! prints).

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use grooviq_engine::{Command, EngineConfig, HealthEvent, INJECTED_PANIC, engine};
use std::sync::Once;

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
            }
        }));
    });
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
