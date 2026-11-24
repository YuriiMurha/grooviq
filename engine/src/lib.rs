//! The real-time engine.
//!
//! [`engine`] returns two halves joined by lock-free queues. The [`AudioProcessor`] lives on the
//! audio thread: it renders the graph (a test tone, the synth and the channels, each a sampler
//! or a synth, into the master bus) in fixed [`BLOCK`]-frame blocks and serves them to
//! callbacks of any size. The [`Controller`] lives on a control thread: it validates and clamps
//! each [`Command`], adds and removes channels, mirrors the state the commands set (an
//! [`EngineState`]), frees what the processor hands back and rebuilds the graph after a panic.
//! [`engine_with`] starts from a given state.
//! [`LiveShared`] carries meters and counters from the audio thread to any reader, and
//! [`render_offline`] drives the same engine faster than real time, and [`export_wav`] writes
//! a pattern to a WAV file with an engine of its own. [`AudioOutput`] plays the
//! processor on the output the user expects: it follows the system default, stops playback
//! rather than move sound from headphones to speakers, and survives devices coming and going.
//! [`decode_sample`] and [`load_sample`] turn sample files into [`SampleBuf`]s (on a control
//! thread) for the sampler. While recording, the processor taps the live channel notes into a
//! queue ([`Controller::take_recorded`]); a metronome clicks the beats while playing.
//!
//! The transport plays the pattern at [`PPQ`] ticks per quarter note: a step sequencer in the
//! graph sends each note to its channel on the frame [`ticks_to_frames`] gives it, and
//! [`LiveSnapshot::position_ticks`] reports where playback is.
//!
//! Real-time rules for everything [`AudioProcessor::process`] reaches: no allocating or
//! freeing, no locks, no I/O, no logging. A panic is the one exception (unwinding allocates and
//! the panic hook may print); the processor catches it and stays silent until the controller
//! replaces the graph.

mod arrangement;
mod automation;
pub use automation::{
    AutomationClip, AutomationPoint, AutomationSchedule, AutomationSegment, AutomationTarget,
};
mod channel;
mod command;
mod controller;
mod export;
mod graph;
mod leftover;
mod live;
mod synth_meter;
pub use synth_meter::{SYNTH_SCOPE_LEN, SynthMeter};
mod metronome;
mod offline;
mod output;
mod processor;
mod record;
mod sample_file;
mod sequencer;
mod song;
mod state;
pub use song::{PlaybackMode, SongData, SongFrameExtent, SongLoop};
mod stream;
mod transport;

use std::sync::Arc;

pub use arrangement::{
    ArrangementData, ArrangementError, MAX_PATTERN_CLIPS, MAX_SONG_NOTES, MAX_SONG_TICKS,
    PatternClip, SongNote, compile_pattern_clips,
};
pub use command::{Command, NoteId};
pub use controller::{ControlError, Controller, HealthEvent};
pub use export::{ExportError, ExportOptions, ExportReport, WavFormat, export_wav};
#[doc(hidden)]
pub use export::{PartFiles, RealFiles, export_wav_with};
pub use grooviq_dsp::sampler::SampleBuf;
pub use leftover::{OwnedTemp, STALE_AFTER, create_owned_temp, remove_stale_owned, sibling};
pub use live::{LIVE_LEN, LIVE_VERSION, LiveShared, LiveSnapshot, decode_live, encode_live};
pub use offline::{render_offline, render_offline_with};
pub use output::{
    AudioOutput, AudioSlot, NULL_OUTPUT_ID, OutputBackend, OutputChoice, OutputEvent, OutputInfo,
    OutputKind, OutputMonitor, Transport, classify, cpal_backend, cpal_backends_created,
    null_backend, run_silent,
};
pub use processor::{AudioProcessor, in_audio_callback};
pub use record::{RECORD_CAPACITY, RecordEvent};
pub use sample_file::{
    MAX_SAMPLE_SECONDS, SampleError, decode_sample, load_sample, load_sample_with_limit,
};
pub use state::{
    BPM_RANGE, ChannelId, ChannelMix, ChannelSetup, EngineState, Instrument, InstrumentKind,
    MAX_CHANNELS, MAX_CHOKE_GROUP, PPQ, PatternData, STEP_TICKS, SeqNote, TIME_SIGNATURE_BEATS,
    TIME_SIGNATURE_UNITS,
};
pub use stream::{StreamServer, start_stream};
pub use transport::ticks_to_frames;

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
    let (recorded_tx, recorded) = rtrb::RingBuffer::new(RECORD_CAPACITY);
    let live = Arc::new(LiveShared::default());
    let mut controller = Controller::new(
        sample_rate,
        (commands, trash, recorded),
        Arc::clone(&live),
        state,
    );
    let graph = controller.build_graph();
    let timing = (controller.state().bpm, controller.state().time_signature);
    let queues = (commands_rx, trash_tx, recorded_tx);
    let processor = AudioProcessor::new(sample_rate, timing, graph, queues, live);
    (controller, processor)
}

// Each half moves to its own thread.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Controller>();
    send::<AudioProcessor>();
};

pub mod waveform;

mod audio_clip;
pub use audio_clip::{AudioAsset, AudioClip, AudioDiagnostic, AudioSchedule, MAX_AUDIO_VOICES};

pub use command::TimelineUpdate;

pub mod media_budget;

pub use sample_file::decode_sample_budgeted;

pub mod mixer;
