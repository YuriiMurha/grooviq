//! The real-time engine.
//!
//! [`engine`] returns two halves joined by lock-free queues. The [`AudioProcessor`] lives on the
//! audio thread: it renders the graph (a test tone and the synth into the master bus) in fixed
//! [`BLOCK`]-frame blocks and serves them to callbacks of any size. The [`Controller`] lives on a
//! control thread: it validates and clamps each [`Command`], mirrors the state the commands set,
//! frees the graphs the processor hands back and rebuilds the graph after a panic.
//! [`LiveShared`] carries meters and counters from the audio thread to any reader, and
//! [`render_offline`] drives the same engine faster than real time.
//!
//! Real-time rules for everything [`AudioProcessor::process`] reaches: no allocating or
//! freeing, no locks, no I/O, no logging. A panic is the one exception (unwinding allocates and
//! the panic hook may print); the processor catches it and stays silent until the controller
//! replaces the graph.

mod command;
mod controller;
mod graph;
mod live;
mod offline;
mod processor;

use std::sync::Arc;

pub use command::Command;
pub use controller::{Controller, HealthEvent};
pub use live::{LiveShared, LiveSnapshot};
pub use offline::render_offline;
pub use processor::AudioProcessor;

/// Frames in every internal block. Commands apply at block starts; notes at their exact frame.
pub const BLOCK: usize = 64;
const _: () = assert!(BLOCK <= grooviq_dsp::MAX_BLOCK);

/// Messages the command queue holds.
const QUEUE_CAPACITY: usize = 1024;
/// Replaced graphs waiting for the controller to free them.
const TRASH_CAPACITY: usize = 4;

/// Sample rates the engine runs at; others are clamped into this range.
const RATE_RANGE: (f32, f32) = (8_000.0, 384_000.0);
/// The rate used when the configured one is not a number.
const DEFAULT_RATE: f32 = 48_000.0;

/// The message of the panic [`Command::InjectPanic`] raises.
#[cfg(feature = "test-hooks")]
pub const INJECTED_PANIC: &str = "grooviq-engine: injected panic (test hook)";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    /// Frames per second, clamped to 8 kHz..=384 kHz (48 kHz if not finite).
    pub sample_rate: f32,
}

impl EngineConfig {
    /// The sample rate the engine actually runs at.
    pub(crate) fn rate(self) -> f32 {
        if self.sample_rate.is_finite() {
            self.sample_rate.clamp(RATE_RANGE.0, RATE_RANGE.1)
        } else {
            DEFAULT_RATE
        }
    }
}

/// Creates the engine: the [`Controller`] for a control thread and the [`AudioProcessor`] for
/// the audio thread. Nothing sounds until a command asks for it.
pub fn engine(config: EngineConfig) -> (Controller, AudioProcessor) {
    let sample_rate = config.rate();
    let (commands, commands_rx) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
    let (trash_tx, trash) = rtrb::RingBuffer::new(TRASH_CAPACITY);
    let live = Arc::new(LiveShared::default());
    let controller = Controller::new(sample_rate, commands, trash, Arc::clone(&live));
    let graph = controller.build_graph();
    let processor = AudioProcessor::new(sample_rate, graph, commands_rx, trash_tx, live);
    (controller, processor)
}

// Each half moves to its own thread.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Controller>();
    send::<AudioProcessor>();
};
