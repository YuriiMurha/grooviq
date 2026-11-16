//! The real-time engine.
//!
//! [`engine`] returns two halves joined by lock-free queues. The [`AudioProcessor`] lives on the
//! audio thread: it renders the graph (a test tone, the synth and the sampler channels into the
//! master bus) in fixed [`BLOCK`]-frame blocks and serves them to callbacks of any size. The
//! [`Controller`] lives on a control thread: it validates and clamps each [`Command`], adds and
//! removes channels, mirrors the state the commands set (an [`EngineState`]), frees what the
//! processor hands back and rebuilds the graph after a panic. [`engine_with`] starts from a
//! given state.
//! [`LiveShared`] carries meters and counters from the audio thread to any reader, and
//! [`render_offline`] drives the same engine faster than real time. [`AudioOutput`] plays the
//! processor on the output the user expects: it follows the system default, stops playback
//! rather than move sound from headphones to speakers, and survives devices coming and going.
//! [`decode_sample`] and [`load_sample`] turn sample files into [`SampleBuf`]s (on a control
//! thread) for the sampler.
//!
//! Real-time rules for everything [`AudioProcessor::process`] reaches: no allocating or
//! freeing, no locks, no I/O, no logging. A panic is the one exception (unwinding allocates and
//! the panic hook may print); the processor catches it and stays silent until the controller
//! replaces the graph.

mod channel;
mod command;
mod controller;
mod graph;
mod live;
mod offline;
mod output;
mod processor;
mod sample_file;
mod state;
mod stream;

use std::sync::Arc;

pub use command::Command;
pub use controller::{ControlError, Controller, HealthEvent};
pub use grooviq_dsp::sampler::SampleBuf;
pub use live::{LIVE_LEN, LIVE_VERSION, LiveShared, LiveSnapshot, decode_live, encode_live};
pub use offline::{render_offline, render_offline_with};
pub use output::{
    AudioOutput, AudioSlot, OutputBackend, OutputChoice, OutputEvent, OutputInfo, OutputKind,
    OutputMonitor, Transport, classify, cpal_backend,
};
pub use processor::{AudioProcessor, in_audio_callback};
pub use sample_file::{
    MAX_SAMPLE_SECONDS, SampleError, decode_sample, load_sample, load_sample_with_limit,
};
pub use state::{
    ChannelId, ChannelMix, ChannelSetup, EngineState, MAX_CHANNELS, PPQ, PatternData, STEP_TICKS,
    SeqNote,
};
pub use stream::{StreamServer, start_stream};

/// Frames in every internal block. Commands apply at block starts; notes at their exact frame.
pub const BLOCK: usize = 64;
const _: () = assert!(BLOCK <= grooviq_dsp::MAX_BLOCK);

/// Messages the command queue holds.
const QUEUE_CAPACITY: usize = 1024;
/// Replaced or removed items (graphs, channels, samples, patterns) waiting for the controller
/// to free them.
const TRASH_CAPACITY: usize = 64;

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
/// the audio thread, with no channels. Nothing sounds until a command asks for it.
pub fn engine(config: EngineConfig) -> (Controller, AudioProcessor) {
    engine_with(config, EngineState::default())
}

/// Creates the engine with `state`'s channels, samples and pattern (each channel at its mix,
/// silent until a note). Values are clamped as [`Controller::load_state`] clamps them.
///
/// # Panics
///
/// If `state` is one [`Controller::load_state`] would refuse: more than [`MAX_CHANNELS`]
/// channels, two with one id, or a value that is not finite.
pub fn engine_with(config: EngineConfig, state: EngineState) -> (Controller, AudioProcessor) {
    let state = match state.checked() {
        Ok(state) => state,
        Err(e) => panic!("engine_with: the state is refused: {e}"),
    };
    let sample_rate = config.rate();
    let (commands, commands_rx) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
    let (trash_tx, trash) = rtrb::RingBuffer::new(TRASH_CAPACITY);
    let live = Arc::new(LiveShared::default());
    let mut controller = Controller::new(sample_rate, commands, trash, Arc::clone(&live), state);
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
