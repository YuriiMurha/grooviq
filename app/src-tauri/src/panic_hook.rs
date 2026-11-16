//! A panic hook that keeps I/O off the audio thread.
//!
//! The engine catches a panic raised while it renders and stays silent until the health thread
//! restarts its graph (and reports `Restarted` with the count). The default hook would still
//! print the panic from the audio thread, and printing is I/O, which the real-time rules forbid
//! there. This hook only counts such panics; every other thread keeps the previous hook.

use grooviq_engine::in_audio_callback;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

static AUDIO_PANICS: AtomicU32 = AtomicU32::new(0);

/// Wraps `previous`: panics raised while the engine renders are only counted; any other goes
/// to `previous`.
pub fn audio_aware(previous: Hook) -> Hook {
    Box::new(move |info| {
        if in_audio_callback() {
            AUDIO_PANICS.fetch_add(1, Relaxed);
        } else {
            previous(info);
        }
    })
}

/// Installs [`audio_aware`] around the current hook. Call once, at startup.
pub fn install() {
    std::panic::set_hook(audio_aware(std::panic::take_hook()));
}

/// Panics the hook silenced on the audio thread since startup.
pub fn audio_panics() -> u32 {
    AUDIO_PANICS.load(Relaxed)
}
