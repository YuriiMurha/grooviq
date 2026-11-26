//! The engine's state as data: channels, the pattern and the tempo. The controller mirrors it
//! (so a rebuilt graph matches what the commands set), a project file saves it, and
//! [`crate::engine_with`] starts from it.

use crate::controller::ControlError;
use grooviq_dsp::sampler::SampleBuf;
use grooviq_dsp::{sampler, synth};
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
/// The highest choke group; group 0 is none.
pub const MAX_CHOKE_GROUP: u8 = 16;
/// Beats in a bar, and the note values a beat may be (2: half notes .. 16: sixteenths).
pub const TIME_SIGNATURE_BEATS: (u8, u8) = (1, 16);
pub const TIME_SIGNATURE_UNITS: [u8; 4] = [2, 4, 8, 16];

/// Whether `beats / unit` is a time signature the engine takes.
pub(crate) fn time_signature_ok(beats: u8, unit: u8) -> bool {
    (TIME_SIGNATURE_BEATS.0..=TIME_SIGNATURE_BEATS.1).contains(&beats)
        && TIME_SIGNATURE_UNITS.contains(&unit)
}

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

/// Which instrument a channel holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentKind {
    Sampler,
    Synth,
}

impl InstrumentKind {
    /// `(min, max, default)` of every parameter, indexed like the device's `set_param`.
    pub fn spec(self) -> &'static [(f32, f32, f32)] {
        match self {
            Self::Sampler => &sampler::SPEC,
            Self::Synth => &synth::SPEC,
        }
    }
}

/// A channel's instrument and its parameters, indexed like its [`InstrumentKind::spec`].
/// Missing trailing values take their defaults; values past the table are dropped.
#[derive(Clone, Debug, PartialEq)]
pub enum Instrument {
    Sampler {
        sample: Option<Arc<SampleBuf>>,
        params: Vec<f32>,
    },
    Synth {
        params: Vec<f32>,
    },
}

impl Instrument {
    pub fn kind(&self) -> InstrumentKind {
        match self {
            Self::Sampler { .. } => InstrumentKind::Sampler,
            Self::Synth { .. } => InstrumentKind::Synth,
        }
    }

    /// The parameter table of the instrument's device.
    pub fn spec(&self) -> &'static [(f32, f32, f32)] {
        self.kind().spec()
    }

    /// A sampler's sample; a synth has none.
    pub fn sample(&self) -> Option<&Arc<SampleBuf>> {
        match self {
            Self::Sampler { sample, .. } => sample.as_ref(),
            Self::Synth { .. } => None,
        }
    }

    pub fn params(&self) -> &[f32] {
        match self {
            Self::Sampler { params, .. } | Self::Synth { params } => params,
        }
    }

    pub(crate) fn params_mut(&mut self) -> &mut Vec<f32> {
        match self {
            Self::Sampler { params, .. } | Self::Synth { params } => params,
        }
    }
}

/// Everything that makes a channel.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSetup {
    pub id: ChannelId,
    pub instrument: Instrument,
    pub mix: ChannelMix,
    /// Choke group, 0..=[`MAX_CHOKE_GROUP`] (0: none). A note-on in a channel of group g cuts
    /// every other channel of g at its frame.
    pub choke: u8,
}

impl ChannelSetup {
    /// A sampler channel in no choke group.
    pub fn sampler(
        id: ChannelId,
        sample: Option<Arc<SampleBuf>>,
        params: Vec<f32>,
        mix: ChannelMix,
    ) -> Self {
        Self {
            id,
            instrument: Instrument::Sampler { sample, params },
            mix,
            choke: 0,
        }
    }

    /// A synth channel in no choke group.
    pub fn synth(id: ChannelId, params: Vec<f32>, mix: ChannelMix) -> Self {
        Self {
            id,
            instrument: Instrument::Synth { params },
            mix,
            choke: 0,
        }
    }

    /// The setup with every value clamped and the parameter list exactly as long as its
    /// instrument's table, or `Rejected` if a value is not finite or the choke group is past
    /// [`MAX_CHOKE_GROUP`].
    pub(crate) fn checked(mut self) -> Result<Self, ControlError> {
        self.mix = self.mix.clamped().ok_or(ControlError::Rejected)?;
        if self.choke > MAX_CHOKE_GROUP {
            return Err(ControlError::Rejected);
        }
        let spec = self.instrument.spec();
        let given = self.instrument.params_mut();
        if given.iter().take(spec.len()).any(|v| !v.is_finite()) {
            return Err(ControlError::Rejected);
        }
        *given = spec
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
    /// An FL-style slide: the channel's newest held note glides to this key instead of a new
    /// note starting.
    pub slide: bool,
}

/// A loop of notes.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternData {
    /// In ticks: at least 1, at most 256 steps.
    pub length: u32,
    /// Sorted by tick, channel, slide, then key: at one tick a slide follows the plain note it
    /// bends. Notes at or past `length` never play.
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
    /// The pattern with velocities clamped and notes sorted by tick, channel, slide, then key
    /// (notes that tie keep their order), or `Rejected` if the length is out of range, a key is
    /// above 127 or a velocity is not finite.
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
        self.notes
            .sort_by_key(|n| (n.tick, n.channel, n.slide, n.key));
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
    pub song: crate::SongData,
    pub playback_mode: crate::PlaybackMode,
    pub song_loop: Option<crate::SongLoop>,
    /// `(beats, unit)`: beats in a bar, 1..=16, and the note value of a beat, 2, 4, 8 or 16. The
    /// metronome clicks every `3840 / unit` ticks.
    pub time_signature: (u8, u8),
    /// How sources reach the output; LegacyDirect by default.
    pub mix: crate::mixer::MixSetup,
}

impl Default for EngineState {
    /// 140 BPM in 4/4, no channels, an empty 16-step pattern.
    fn default() -> Self {
        Self {
            bpm: DEFAULT_BPM,
            channels: Vec::new(),
            pattern: PatternData::default(),
            song: crate::SongData::default(),
            playback_mode: crate::PlaybackMode::Pattern,
            song_loop: None,
            time_signature: (4, 4),
            mix: crate::mixer::MixSetup::LegacyDirect,
        }
    }
}

impl EngineState {
    /// The state with every value clamped (see [`ChannelSetup`] and [`PatternData`]), or why
    /// it is refused: more than [`MAX_CHANNELS`] channels, two with one id, a value that is
    /// not finite or a time signature out of range.
    pub(crate) fn checked(self) -> Result<Self, ControlError> {
        let (beats, unit) = self.time_signature;
        if !self.bpm.is_finite() || !time_signature_ok(beats, unit) {
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
        let song = self.song.checked(&ids)?;
        song.automation.validate(&self.channels)?;
        if !song.valid_loop(self.song_loop) {
            return Err(ControlError::Rejected);
        }
        self.mix.validate_for(&self.channels)?;
        self.mix.covers(&self.channels, &song)?;
        Ok(Self {
            mix: self.mix,
            song,
            playback_mode: self.playback_mode,
            song_loop: self.song_loop,
            bpm: self.bpm.clamp(BPM_RANGE.0, BPM_RANGE.1),
            channels: self
                .channels
                .into_iter()
                .map(ChannelSetup::checked)
                .collect::<Result<_, _>>()?,
            pattern: self.pattern.checked()?,
            time_signature: self.time_signature,
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
        let mix = ChannelMix {
            volume: -1.0,
            pan: 3.0,
            ..ChannelMix::default()
        };
        let setup = |params: Vec<f32>| ChannelSetup::sampler(ChannelId(0), None, params, mix);
        let s = setup(vec![200.0, -100.0]).checked().unwrap();
        assert_eq!(s.mix.volume, 0.0);
        assert_eq!(s.mix.pan, 1.0);
        let params = s.instrument.params();
        assert_eq!(params.len(), sampler::SPEC.len());
        assert_eq!(&params[..2], &[127.0, -24.0]);
        assert_eq!(params[2..], sampler::SPEC.map(|(_, _, d)| d)[2..]);
        let long = setup(vec![60.0; sampler::SPEC.len() + 3])
            .checked()
            .unwrap();
        assert_eq!(long.instrument.params().len(), sampler::SPEC.len());
        assert_eq!(
            setup(vec![f32::INFINITY]).checked(),
            Err(ControlError::Rejected)
        );

        // A synth pads and clamps by its own table.
        let synth = ChannelSetup::synth(ChannelId(0), vec![99.0], mix)
            .checked()
            .unwrap();
        let params = synth.instrument.params();
        assert_eq!(params.len(), synth::SPEC.len());
        assert_eq!(params[0], synth::SPEC[0].1);
        assert_eq!(params[1..], synth::SPEC.map(|(_, _, d)| d)[1..]);

        // Choke groups 0..=16.
        for (choke, ok) in [
            (0, true),
            (MAX_CHOKE_GROUP, true),
            (MAX_CHOKE_GROUP + 1, false),
        ] {
            let s = ChannelSetup {
                choke,
                ..setup(Vec::new())
            };
            assert_eq!(s.checked().is_ok(), ok, "group {choke}");
        }
    }

    #[test]
    fn a_slide_sorts_after_the_plain_note_it_bends() {
        let n = |tick, channel, key, slide| SeqNote {
            tick,
            channel: ChannelId(channel),
            key,
            velocity: 0.5,
            length: 1,
            slide,
        };
        let pattern = PatternData {
            length: 3_840,
            notes: vec![
                n(0, 1, 43, true),
                n(0, 1, 36, false),
                n(0, 0, 50, true),
                n(240, 1, 30, false),
                n(0, 1, 40, false),
            ],
        };
        let sorted: Vec<(u32, u32, bool, u8)> = pattern
            .checked()
            .unwrap()
            .notes
            .iter()
            .map(|n| (n.tick, n.channel.0, n.slide, n.key))
            .collect();
        assert_eq!(
            sorted,
            [
                (0, 0, true, 50),
                (0, 1, false, 36),
                (0, 1, false, 40),
                (0, 1, true, 43),
                (240, 1, false, 30)
            ]
        );
    }

    #[test]
    fn state_checks_name_the_problem() {
        let ch = |id| ChannelSetup::sampler(ChannelId(id), None, Vec::new(), ChannelMix::default());
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
                slide: false,
            }],
            ..PatternData::default()
        };
        assert_eq!(loud.checked(), Err(ControlError::Rejected));
    }
}
