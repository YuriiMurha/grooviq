//! Panic isolation, in its own test binary: the panic hook is process-global, and this binary
//! silences it for the injected panic only (any other panic still prints).

#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

use grooviq_engine::{Command, EngineConfig, HealthEvent, INJECTED_PANIC, engine};

#[test]
fn a_panicking_node_is_isolated_and_the_graph_restarts() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.payload_as_str() != Some(INJECTED_PANIC) {
            default_hook(info);
        }
    }));
    let (mut c, mut p) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    c.send(Command::TestTone {
        on: true,
        hz: 440.0,
        gain_db: -12.0,
    })
    .unwrap();
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
    // The swap hands the old graph back to the controller: nothing is freed on the audio thread.
    let mut buf = vec![0f32; 4096 * 2];
    assert_no_alloc::assert_no_alloc(|| p.process(&mut buf, 2));
    let peak = buf.iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.2, "peak {peak}");
}
