//! The engine's state as data: channels, the pattern and the tempo. The controller mirrors it
//! (so a rebuilt graph matches what the commands set), a project file saves it, and
//! [`crate::engine_with`] starts from it.

use crate::controller::ControlError;
use grooviq_dsp::sampler::{SPEC, SampleBuf};
use std::sync::Arc;

/// Ticks per quarter note.
pub const PPQ: u32 = 960;
/// Ticks per step (a 1/16 note).
pub const STEP_TICKS: u32 = PPQ / 4;
/// Channels the engine holds at most.
pub const MAX_CHANNELS: usize = 256;

/// Steps in a new pattern, and the most a pattern may have.
const DEFAULT_STEPS: u32 = 16;
const MAX_STEPS: u32 = 256;
/// Tempo range in beats per minute.
pub const BPM_RANGE: (f32, f32) = (20.0, 400.0);
/// Tempo of a new state.
const DEFAULT_BPM: f32 = 140.0;
/// Channel volume range (linear gain).
const VOLUME: (f32, f32) = (0.0, 2.0);

/// Names a channel. Stable for the channel's life; never reused while it exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(pub u32);

/// A channel's mixer settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelMix {
    /// Linear gain, 0..=2 (1 = 0 dB).
    pub volume: f32,
    /// -1 (left) ..= 1 (right). Gains `sqrt(1 - pan)` left and `sqrt(1 + pan)` right: 1 at the
    /// centre, `SQRT_2` at a hard edge.
    pub pan: f32,
    pub mute: bool,
    /// While any channel is soloed, only soloed channels (that are not muted) are heard.
    pub solo: bool,
}

impl Default for ChannelMix {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

impl ChannelMix {
    /// The mix clamped into range, or `None` if a value is not finite.
    pub(crate) fn clamped(self) -> Option<Self> {
        (self.volume.is_finite() && self.pan.is_finite()).then(|| Self {
            volume: self.volume.clamp(VOLUME.0, VOLUME.1),
            pan: self.pan.clamp(-1.0, 1.0),
            ..self
        })
    }

    /// Left and right gains: volume times the pan law, or silence when not `audible`.
    pub(crate) fn gains(self, audible: bool) -> [f32; 2] {
        if !audible {
            return [0.0; 2];
        }
        // sqrt is exact in IEEE arithmetic, so this is the same on every platform.
        [
            self.volume * (1.0 - self.pan).sqrt(),
            self.volume * (1.0 + self.pan).sqrt(),
        ]
    }

    /// Whether the channel is heard, given whether any channel is soloed.
    pub(crate) fn audible(self, any_solo: bool) -> bool {
        !self.mute && (self.solo || !any_solo)
    }
}

/// Everything that makes a channel.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSetup {
    pub id: ChannelId,
    pub sample: Option<Arc<SampleBuf>>,
    /// Sampler parameters, indexed like `grooviq_dsp::sampler::SPEC`. Missing trailing values
    /// take their defaults; values past the table are dropped.
    pub sampler: Vec<f32>,
    pub mix: ChannelMix,
}

impl ChannelSetup {
    /// The setup with every value clamped and the parameter list exactly as long as the
    /// sampler's table, or `Rejected` if a value is not finite.
    pub(crate) fn checked(mut self) -> Result<Self, ControlError> {
        self.mix = self.mix.clamped().ok_or(ControlError::Rejected)?;
        let given = &self.sampler;
        if given.iter().take(SPEC.len()).any(|v| !v.is_finite()) {
            return Err(ControlError::Rejected);
        }
        self.sampler = SPEC
            .iter()
            .enumerate()
            .map(|(i, &(min, max, default))| given.get(i).map_or(default, |v| v.clamp(min, max)))
            .collect();
        Ok(self)
    }
}

/// A note in a pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeqNote {
    /// Start, in ticks from the pattern's start.
    pub tick: u32,
    pub channel: ChannelId,
    /// 0..=127.
    pub key: u8,
    /// 0..=1.
    pub velocity: f32,
    /// Ticks the note is held.
    pub length: u32,
}

/// A loop of notes.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternData {
    /// In ticks: at least 1, at most 256 steps.
    pub length: u32,
    /// Sorted by tick, then channel. Notes at or past `length` never play.
    pub notes: Vec<SeqNote>,
}

impl Default for PatternData {
    /// An empty 16-step pattern.
    fn default() -> Self {
        Self {
            length: DEFAULT_STEPS * STEP_TICKS,
            notes: Vec::new(),
        }
    }
}

impl PatternData {
    /// The pattern with velocities clamped and notes sorted by tick, then channel (notes that
    /// tie keep their order), or `Rejected` if the length is out of range, a key is above 127
    /// or a velocity is not finite.
    pub(crate) fn checked(mut self) -> Result<Self, ControlError> {
        if !(1..=MAX_STEPS * STEP_TICKS).contains(&self.length) {
            return Err(ControlError::Rejected);
        }
        for note in &mut self.notes {
            if note.key > 127 || !note.velocity.is_finite() {
                return Err(ControlError::Rejected);
            }
            note.velocity = note.velocity.clamp(0.0, 1.0);
        }
        self.notes.sort_by_key(|n| (n.tick, n.channel));
        Ok(self)
    }
}

/// The engine's musical state.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineState {
    /// Tempo, 20..=400 BPM.
    pub bpm: f32,
    /// In insertion order, which is also the order they are mixed in.
    pub channels: Vec<ChannelSetup>,
    pub pattern: PatternData,
}

impl Default for EngineState {
    /// 140 BPM, no channels, an empty 16-step pattern.
    fn default() -> Self {
        Self {
            bpm: DEFAULT_BPM,
            channels: Vec::new(),
            pattern: PatternData::default(),
        }
    }
}

impl EngineState {
    /// The state with every value clamped (see [`ChannelSetup`] and [`PatternData`]), or why
    /// it is refused: more than [`MAX_CHANNELS`] channels, two with one id, or a value that is
    /// not finite.
    pub(crate) fn checked(self) -> Result<Self, ControlError> {
        if !self.bpm.is_finite() {
            return Err(ControlError::Rejected);
        }
        if self.channels.len() > MAX_CHANNELS {
            return Err(ControlError::TooManyChannels);
        }
        let mut ids: Vec<ChannelId> = self.channels.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err(ControlError::ChannelExists);
        }
        Ok(Self {
            bpm: self.bpm.clamp(BPM_RANGE.0, BPM_RANGE.1),
            channels: self
                .channels
                .into_iter()
                .map(ChannelSetup::checked)
                .collect::<Result<_, _>>()?,
            pattern: self.pattern.checked()?,
        })
    }

    pub(crate) fn channel(&self, id: ChannelId) -> Option<&ChannelSetup> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub(crate) fn channel_mut(&mut self, id: ChannelId) -> Option<&mut ChannelSetup> {
        self.channels.iter_mut().find(|c| c.id == id)
    }

    /// Whether any channel is soloed.
    pub(crate) fn any_solo(&self) -> bool {
        self.channels.iter().any(|c| c.mix.solo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pan_law_is_unity_at_the_centre_and_sqrt_2_at_the_edges() {
        let at = |pan| {
            ChannelMix {
                pan,
                ..ChannelMix::default()
            }
            .gains(true)
        };
        assert_eq!(at(0.0), [1.0, 1.0]);
        assert_eq!(at(1.0), [0.0, std::f32::consts::SQRT_2]);
        assert_eq!(at(-1.0), [std::f32::consts::SQRT_2, 0.0]);
        // The constraint's formula, to within rounding.
        for pan in [-0.7f32, -0.2, 0.3, 0.9] {
            let [l, r] = at(pan);
            let sq = std::f32::consts::SQRT_2;
            assert!((l - ((1.0 - pan) / 2.0).sqrt() * sq).abs() < 1e-6);
            assert!((r - ((1.0 + pan) / 2.0).sqrt() * sq).abs() < 1e-6);
        }
        assert_eq!(ChannelMix::default().gains(false), [0.0, 0.0]);
    }

    #[test]
    fn setups_are_padded_clamped_and_truncated() {
        let setup = |sampler: Vec<f32>| ChannelSetup {
            id: ChannelId(0),
            sample: None,
            sampler,
            mix: ChannelMix {
                volume: -1.0,
                pan: 3.0,
                ..ChannelMix::default()
            },
        };
        let s = setup(vec![200.0, -100.0]).checked().unwrap();
        assert_eq!(s.mix.volume, 0.0);
        assert_eq!(s.mix.pan, 1.0);
        assert_eq!(s.sampler.len(), SPEC.len());
        assert_eq!(&s.sampler[..2], &[127.0, -24.0]);
        assert_eq!(s.sampler[2..], SPEC.map(|(_, _, d)| d)[2..]);
        let long = setup(vec![60.0; SPEC.len() + 3]).checked().unwrap();
        assert_eq!(long.sampler.len(), SPEC.len());
        assert_eq!(
            setup(vec![f32::INFINITY]).checked(),
            Err(ControlError::Rejected)
        );
    }

    #[test]
    fn state_checks_name_the_problem() {
        let ch = |id| ChannelSetup {
            id: ChannelId(id),
            sample: None,
            sampler: Vec::new(),
            mix: ChannelMix::default(),
        };
        let with = |channels| EngineState {
            channels,
            ..EngineState::default()
        };
        assert_eq!(
            with(vec![ch(1), ch(2), ch(1)]).checked(),
            Err(ControlError::ChannelExists)
        );
        assert_eq!(
            with((0..=MAX_CHANNELS as u32).map(ch).collect()).checked(),
            Err(ControlError::TooManyChannels)
        );
        let bpm = |bpm| EngineState {
            bpm,
            ..EngineState::default()
        };
        assert_eq!(bpm(1_000.0).checked().unwrap().bpm, 400.0);
        assert_eq!(bpm(1.0).checked().unwrap().bpm, 20.0);
        assert_eq!(bpm(f32::NAN).checked(), Err(ControlError::Rejected));
        let long = PatternData {
            length: MAX_STEPS * STEP_TICKS + 1,
            notes: Vec::new(),
        };
        assert_eq!(long.checked(), Err(ControlError::Rejected));
        let loud = PatternData {
            notes: vec![SeqNote {
                tick: 0,
                channel: ChannelId(0),
                key: 128,
                velocity: 0.5,
                length: 1,
            }],
            ..PatternData::default()
        };
        assert_eq!(loud.checked(), Err(ControlError::Rejected));
    }
}
