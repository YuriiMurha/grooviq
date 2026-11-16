//! Commands from the control side, their validation and clamping, and the queue's message type.

use crate::channel::ChannelNode;
use crate::graph::Graph;
use crate::state::{ChannelId, ChannelMix, PatternData};
use grooviq_dsp::sampler::SampleBuf;
use std::sync::Arc;

/// Test-tone frequency range in Hz.
const TONE_HZ: (f32, f32) = (20.0, 20_000.0);
/// Test-tone level range in dBFS.
const TONE_DB: (f32, f32) = (-60.0, 0.0);

/// What the control side asks the engine to do.
///
/// [`Controller::send`](crate::Controller::send) clamps values into range and rejects
/// non-finite values, unknown keys or parameter indices, and channels that do not exist.
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
    /// Sets a channel's mix: `volume` clamped to 0..=2, `pan` to -1..=1. The gains ramp over
    /// 5 ms.
    SetChannelMix { channel: ChannelId, mix: ChannelMix },
    /// Sets a channel's sampler parameter by its index in `grooviq_dsp::sampler::SPEC`,
    /// clamped to its range.
    SetSamplerParam {
        channel: ChannelId,
        index: u32,
        value: f32,
    },
    /// Starts or releases a note on a channel (a preview): `key` 0..=127, `velocity` clamped to
    /// 0..=1. Lands on its exact frame, like [`Command::Note`].
    ChannelNote {
        channel: ChannelId,
        key: u8,
        velocity: f32,
        on: bool,
    },
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
            Self::SetChannelMix { channel, mix } => Some(Self::SetChannelMix {
                channel,
                mix: mix.clamped()?,
            }),
            Self::SetSamplerParam {
                channel,
                index,
                value,
            } => {
                let spec = &grooviq_dsp::sampler::SPEC;
                let &(min, max, _) = spec.get(usize::try_from(index).ok()?)?;
                value.is_finite().then(|| Self::SetSamplerParam {
                    channel,
                    index,
                    value: value.clamp(min, max),
                })
            }
            Self::ChannelNote {
                channel,
                key,
                velocity,
                on,
            } => (key <= 127 && velocity.is_finite()).then(|| Self::ChannelNote {
                channel,
                key,
                velocity: velocity.clamp(0.0, 1.0),
                on,
            }),
            other => Some(other),
        }
    }

    /// The channel the command names, if any.
    pub(crate) fn channel(self) -> Option<ChannelId> {
        match self {
            Self::SetChannelMix { channel, .. }
            | Self::SetSamplerParam { channel, .. }
            | Self::ChannelNote { channel, .. } => Some(channel),
            _ => None,
        }
    }
}

/// What travels through the command queue to the processor.
pub(crate) enum Msg {
    /// A clamped command for the block that contains frame `due` (0: the next block).
    Command { due: u64, cmd: Command },
    /// A fresh graph to swap in (after a panic, or for a new state); the old one goes back
    /// through the trash queue.
    ReplaceGraph(Box<Graph>),
    /// A channel to append to the graph.
    AddChannel(Box<ChannelNode>),
    /// Takes a channel out of the graph; it goes back through the trash queue.
    RemoveChannel(ChannelId),
    /// A channel's new sample; the old one goes back through the trash queue.
    SetSample {
        channel: ChannelId,
        sample: Option<Arc<SampleBuf>>,
    },
    /// The pattern to play; the old one goes back through the trash queue.
    SetPattern(Box<PatternData>),
}

impl Msg {
    /// Whether applying the message may hand something back through the trash queue: such a
    /// message waits while the processor holds a parked item (it has room for only one).
    pub(crate) fn makes_trash(&self) -> bool {
        !matches!(self, Self::Command { .. })
    }
}

/// What the processor hands back for the controller to free, so nothing is freed on the audio
/// thread.
#[allow(
    dead_code,
    reason = "held only to be dropped on the controller's thread"
)]
pub(crate) enum Trash {
    Graph(Box<Graph>),
    Channel(Box<ChannelNode>),
    Sample(Arc<SampleBuf>),
    Pattern(Box<PatternData>),
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

        let id = ChannelId(3);
        let mix = |volume, pan| {
            Command::SetChannelMix {
                channel: id,
                mix: ChannelMix {
                    volume,
                    pan,
                    mute: true,
                    solo: false,
                },
            }
            .clamped()
        };
        let mix_ok = |volume, pan| {
            Some(Command::SetChannelMix {
                channel: id,
                mix: ChannelMix {
                    volume,
                    pan,
                    mute: true,
                    solo: false,
                },
            })
        };
        assert_eq!(mix(5.0, -3.0), mix_ok(2.0, -1.0));
        assert_eq!(mix(-1.0, 3.0), mix_ok(0.0, 1.0));
        assert_eq!(mix(0.7, 0.2), mix_ok(0.7, 0.2));
        assert_eq!(mix(f32::NAN, 0.0), None);
        assert_eq!(mix(1.0, f32::INFINITY), None);

        let sparam = |index, value| {
            Command::SetSamplerParam {
                channel: id,
                index,
                value,
            }
            .clamped()
        };
        let sparam_ok = |index, value| {
            Some(Command::SetSamplerParam {
                channel: id,
                index,
                value,
            })
        };
        let spec = grooviq_dsp::sampler::SPEC;
        for (index, &(min, max, default)) in (0u32..).zip(&spec) {
            assert_eq!(sparam(index, f32::MAX), sparam_ok(index, max));
            assert_eq!(sparam(index, f32::MIN), sparam_ok(index, min));
            assert_eq!(sparam(index, default), sparam_ok(index, default));
            assert_eq!(sparam(index, f32::NAN), None);
        }
        assert_eq!(sparam(spec.len() as u32, 0.0), None);

        let cnote = |key, velocity| {
            Command::ChannelNote {
                channel: id,
                key,
                velocity,
                on: true,
            }
            .clamped()
        };
        let cnote_ok = |key, velocity| {
            Some(Command::ChannelNote {
                channel: id,
                key,
                velocity,
                on: true,
            })
        };
        assert_eq!(cnote(60, 5.0), cnote_ok(60, 1.0));
        assert_eq!(cnote(60, -1.0), cnote_ok(60, 0.0));
        assert_eq!(cnote(128, 0.5), None);
        assert_eq!(cnote(60, f32::NAN), None);
    }
}
