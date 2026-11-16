//! Commands from the control side, their validation and clamping, and the queue's message type.

use crate::graph::Graph;

/// Test-tone frequency range in Hz.
const TONE_HZ: (f32, f32) = (20.0, 20_000.0);
/// Test-tone level range in dBFS.
const TONE_DB: (f32, f32) = (-60.0, 0.0);

/// What the control side asks the engine to do.
///
/// [`Controller::send`](crate::Controller::send) clamps values into range and rejects
/// non-finite values and unknown keys or parameter indices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Starts the transport.
    Play,
    /// Stops the transport.
    Stop,
    /// Fades the tone and every synth voice out over 5 ms (the synth resets after the fade) and
    /// stops the transport.
    AllSoundOff,
    /// Switches the sine test tone; `hz` is clamped to 20..=20000 and `gain_db` to -60..=0.
    /// Every change ramps over 5 ms.
    TestTone { on: bool, hz: f32, gain_db: f32 },
    /// Starts or releases a synth note: `key` 0..=127, `velocity` clamped to 0..=1.
    Note { key: u8, velocity: f32, on: bool },
    /// Sets a synth parameter by its index in `grooviq_dsp::synth::SPEC`, clamped to its range.
    SetSynthParam { index: u32, value: f32 },
    /// Test hook: the synth's next block contains a NaN.
    #[cfg(feature = "test-hooks")]
    InjectNaN,
    /// Test hook: the synth panics in its next block, with the message [`crate::INJECTED_PANIC`].
    #[cfg(feature = "test-hooks")]
    InjectPanic,
}

impl Command {
    /// The command with its values clamped into range, or `None` if a value is not finite or
    /// the key or parameter index does not exist.
    pub(crate) fn clamped(self) -> Option<Self> {
        match self {
            Self::TestTone { on, hz, gain_db } => {
                (hz.is_finite() && gain_db.is_finite()).then(|| Self::TestTone {
                    on,
                    hz: hz.clamp(TONE_HZ.0, TONE_HZ.1),
                    gain_db: gain_db.clamp(TONE_DB.0, TONE_DB.1),
                })
            }
            Self::Note { key, velocity, on } => {
                (key <= 127 && velocity.is_finite()).then(|| Self::Note {
                    key,
                    velocity: velocity.clamp(0.0, 1.0),
                    on,
                })
            }
            Self::SetSynthParam { index, value } => {
                let spec = grooviq_dsp::param_spec("synth")?;
                let &(min, max, _) = spec.get(usize::try_from(index).ok()?)?;
                value.is_finite().then(|| Self::SetSynthParam {
                    index,
                    value: value.clamp(min, max),
                })
            }
            other => Some(other),
        }
    }
}

/// What travels through the command queue to the processor.
pub(crate) enum Msg {
    /// A clamped command for the block that contains frame `due` (0: the next block).
    Command { due: u64, cmd: Command },
    /// A fresh graph to swap in after a panic; the old one goes back through the trash queue.
    ReplaceGraph(Box<Graph>),
}
