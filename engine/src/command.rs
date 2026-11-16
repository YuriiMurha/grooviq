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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_keeps_every_value_inside_its_range() {
        let tone = |hz, gain_db| {
            Command::TestTone {
                on: true,
                hz,
                gain_db,
            }
            .clamped()
        };
        let tone_ok = |hz, gain_db| {
            Some(Command::TestTone {
                on: true,
                hz,
                gain_db,
            })
        };
        assert_eq!(tone(1.0, -200.0), tone_ok(20.0, -60.0));
        assert_eq!(tone(1e9, 40.0), tone_ok(20_000.0, 0.0));
        assert_eq!(tone(440.0, -12.0), tone_ok(440.0, -12.0));
        assert_eq!(tone(f32::NAN, 0.0), None);
        assert_eq!(tone(440.0, f32::INFINITY), None);

        let note = |key, velocity| {
            Command::Note {
                key,
                velocity,
                on: true,
            }
            .clamped()
        };
        let note_ok = |key, velocity| {
            Some(Command::Note {
                key,
                velocity,
                on: true,
            })
        };
        assert_eq!(note(60, 5.0), note_ok(60, 1.0));
        assert_eq!(note(60, -1.0), note_ok(60, 0.0));
        assert_eq!(note(127, 0.5), note_ok(127, 0.5));
        assert_eq!(note(128, 1.0), None);
        assert_eq!(note(60, f32::NAN), None);

        let param = |index, value| Command::SetSynthParam { index, value }.clamped();
        let param_ok = |index, value| Some(Command::SetSynthParam { index, value });
        let spec = grooviq_dsp::param_spec("synth").unwrap();
        for (index, &(min, max, default)) in (0u32..).zip(spec) {
            assert_eq!(param(index, f32::MAX), param_ok(index, max));
            assert_eq!(param(index, f32::MIN), param_ok(index, min));
            assert_eq!(param(index, default), param_ok(index, default));
            assert_eq!(param(index, f32::NAN), None);
        }
        assert_eq!(param(spec.len() as u32, 0.0), None);
        assert_eq!(Command::Play.clamped(), Some(Command::Play));
    }
}
