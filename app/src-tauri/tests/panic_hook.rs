//! The app's panic hook, in its own test binary: the hook is process-global.

use grooviq_app::panic_hook::{audio_aware, audio_panics};
use grooviq_engine::{Command, EngineConfig, engine};
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};

/// Panics that reached the previous hook (the default one prints them).
static PREVIOUS: AtomicU32 = AtomicU32::new(0);

#[test]
fn a_panic_while_rendering_is_only_counted_and_others_reach_the_previous_hook() {
    std::panic::set_hook(audio_aware(Box::new(|_| {
        PREVIOUS.fetch_add(1, SeqCst);
    })));

    let (mut c, mut p) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    c.send(Command::InjectPanic).unwrap();
    p.process(&mut [0.0; 256], 2);
    assert_eq!(c.live().faults, 1, "the engine caught the panic");
    assert_eq!(audio_panics(), 1, "and the hook counted it");
    assert_eq!(PREVIOUS.load(SeqCst), 0, "without passing it on to print");

    let joined = std::thread::spawn(|| panic!("a control-thread panic")).join();
    assert!(joined.is_err());
    assert_eq!(
        PREVIOUS.load(SeqCst),
        1,
        "another thread's panic keeps the previous hook"
    );
    assert_eq!(audio_panics(), 1);

    let _ = std::panic::take_hook();
}
