//! Commands from the control side, their validation and clamping, and the queue's message type.

use crate::channel::ChannelNode;
use crate::graph::Graph;
use crate::state::{
    BPM_RANGE, ChannelId, ChannelMix, MAX_CHOKE_GROUP, PatternData, time_signature_ok,
};
use grooviq_dsp::sampler::SampleBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Test-tone frequency range in Hz.
const TONE_HZ: (f32, f32) = (20.0, 20_000.0);
/// Test-tone level range in dBFS.
const TONE_DB: (f32, f32) = (-60.0, 0.0);

/// A note's identity from its note-on to its note-off: the instruments end the note whose
/// note-on carried the note-off's id, and a note-off whose note never started, or already ended
/// (stolen, choked, cut by a new graph), ends nothing. Notes held at once need distinct ids.
///
/// Live notes (keys, clicks, MIDI presses) take theirs from [`NoteId::live`]; the sequencer
/// numbers its own notes with [`NoteId::SEQUENCED`] set, which a live note can never carry.
///
/// 64 bits: one id per note, never handed out twice in practice (2^63 notes of each kind), so a
/// note held for any time never shares its id with a later one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteId(pub u64);

impl NoteId {
    /// The bit of the sequencer's ids. [`crate::Controller::send`] rejects a note that has it.
    pub const SEQUENCED: u64 = 1 << 63;

    /// A new id for a live note, never handed out before in this process. Its note-on and its
    /// note-off both carry it. (Counting from 1, it stays below 2^53, which JavaScript numbers
    /// hold exactly, for longer than any session lasts.)
    pub fn live() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self::live_number(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The `n`-th live id.
    fn live_number(n: u64) -> Self {
        Self(n & !Self::SEQUENCED)
    }

    /// The `n`-th id the sequencer gives.
    pub(crate) fn sequenced_number(n: u64) -> Self {
        Self(Self::SEQUENCED | (n & !Self::SEQUENCED))
    }

    fn is_live(self) -> bool {
        self.0 & Self::SEQUENCED == 0
    }
}

/// What the control side asks the engine to do.
///
/// [`Controller::send`](crate::Controller::send) clamps values into range and rejects
/// non-finite values, unknown keys or parameter indices, and channels that do not exist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Plays the pattern from tick 0, looping forever, from the start of its block. A no-op
    /// while playing.
    Play,
    SetPlaybackMode {
        mode: crate::PlaybackMode,
    },
    SetSongLoop {
        region: Option<crate::SongLoop>,
    },
    Seek {
        tick: u64,
    },
    /// Plays the pattern from tick 0 (also while playing) for `loops` loops: no note starts at
    /// or after tick `loops * length`; the transport keeps running so their note-offs land.
    /// `loops` 0 is rejected.
    PlayLoops {
        loops: u32,
    },
    /// Stops the transport: every pending sequenced note-off goes out at the start of its
    /// block, and the position rewinds to tick 0.
    Stop,
    /// Sets the tempo, clamped to [`BPM_RANGE`]. Applies at the start of its block: the tick
    /// reached there stays put and later steps follow the new tempo.
    SetTempo {
        bpm: f32,
    },
    /// Fades the tone and every synth voice out over 5 ms (each resets after the fade), chokes
    /// every channel (its sounding notes fade over 3 ms; a note after the command plays in
    /// full) and stops the transport.
    AllSoundOff,
    /// Switches recording: while it is on and the transport plays, every channel note that
    /// lands is queued for [`crate::Controller::take_recorded`]. Off by default.
    SetRecording {
        on: bool,
    },
    /// Switches the metronome, which clicks on every beat while the transport plays. Off by
    /// default; an export never has it.
    SetMetronome {
        on: bool,
    },
    /// Sets the time signature: `beats` 1..=16 in a bar, of note value `unit` 2, 4, 8 or 16
    /// (anything else is rejected). Applies at the start of its block.
    SetTimeSignature {
        beats: u8,
        unit: u8,
    },
    /// Switches the sine test tone; `hz` is clamped to 20..=20000 and `gain_db` to -60..=0.
    /// Every change ramps over 5 ms.
    TestTone {
        on: bool,
        hz: f32,
        gain_db: f32,
    },
    /// Starts or releases a synth note: `key` 0..=127, `velocity` clamped to 0..=1. A release
    /// ends the note `id` names.
    Note {
        key: u8,
        velocity: f32,
        on: bool,
        id: NoteId,
    },
    /// Sets a synth parameter by its index in `grooviq_dsp::synth::SPEC`, clamped to its range.
    SetSynthParam {
        index: u32,
        value: f32,
    },
    /// Sets a channel's mix: `volume` clamped to 0..=2, `pan` to -1..=1. The gains ramp over
    /// 5 ms.
    SetChannelMix {
        channel: ChannelId,
        mix: ChannelMix,
    },
    /// Sets a sampler channel's parameter by its index in `grooviq_dsp::sampler::SPEC`,
    /// clamped to its range. Refused for a synth channel.
    /// A complete sampler parameter snapshot, published as one callback-visible operation.
    SetSamplerTrim {
        channel: ChannelId,
        instance: u64,
        params: [f32; grooviq_dsp::sampler::SPEC.len()],
    },
    SetSamplerParam {
        channel: ChannelId,
        index: u32,
        value: f32,
    },
    /// Sets a channel's instrument parameter by its index in that instrument's `SPEC`
    /// ([`crate::InstrumentKind::spec`]), clamped to its range.
    SetChannelParam {
        channel: ChannelId,
        index: u32,
        value: f32,
    },
    /// Puts a channel in a choke group, 0..=[`MAX_CHOKE_GROUP`] (0: none; more is refused).
    /// Applies at the start of its block.
    SetChoke {
        channel: ChannelId,
        group: u8,
    },
    /// Starts or releases a note on a channel (a preview): `key` 0..=127, `velocity` clamped to
    /// 0..=1. Lands on its exact frame, like [`Command::Note`]. A release ends the note `id`
    /// names.
    ChannelNote {
        channel: ChannelId,
        key: u8,
        velocity: f32,
        on: bool,
        id: NoteId,
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
            Self::SetTempo { bpm } => bpm.is_finite().then(|| Self::SetTempo {
                bpm: bpm.clamp(BPM_RANGE.0, BPM_RANGE.1),
            }),
            Self::PlayLoops { loops } => (loops > 0).then_some(self),
            Self::Note {
                key,
                velocity,
                on,
                id,
            } => (key <= 127 && velocity.is_finite() && id.is_live()).then(|| Self::Note {
                key,
                velocity: velocity.clamp(0.0, 1.0),
                on,
                id,
            }),
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
            Self::SetSamplerTrim { params, .. } => params
                .iter()
                .zip(grooviq_dsp::sampler::SPEC)
                .all(|(v, (lo, hi, _))| v.is_finite() && *v >= lo && *v <= hi)
                .then_some(self),
            // The range depends on the channel's instrument: the controller clamps it.
            Self::SetChannelParam { value, .. } => value.is_finite().then_some(self),
            Self::SetChoke { group, .. } => (group <= MAX_CHOKE_GROUP).then_some(self),
            Self::SetTimeSignature { beats, unit } => {
                time_signature_ok(beats, unit).then_some(self)
            }
            Self::ChannelNote {
                channel,
                key,
                velocity,
                on,
                id,
            } => (key <= 127 && velocity.is_finite() && id.is_live()).then(|| Self::ChannelNote {
                channel,
                key,
                velocity: velocity.clamp(0.0, 1.0),
                on,
                id,
            }),
            other => Some(other),
        }
    }

    /// The channel the command names, if any.
    pub(crate) fn channel(self) -> Option<ChannelId> {
        match self {
            Self::SetChannelMix { channel, .. }
            | Self::SetSamplerTrim { channel, .. }
            | Self::SetSamplerParam { channel, .. }
            | Self::SetChannelParam { channel, .. }
            | Self::SetChoke { channel, .. }
            | Self::ChannelNote { channel, .. } => Some(channel),
            _ => None,
        }
    }
}

/// What travels through the command queue to the processor.
pub(crate) enum Msg {
    /// A clamped command for the block that contains frame `due` (0: the next block).
    Command {
        due: u64,
        cmd: Command,
        mode_changed: bool,
    },
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
    SetSong(Box<crate::SongData>),
    // Flags capture transitions at admission, before a rebuild installs the final graph.
    Timeline(Box<TimelineUpdate>, bool, bool),
    TimelineRebuilt {
        bpm: f32,
        mode: crate::PlaybackMode,
        region: Option<crate::SongLoop>,
        mode_changed: bool,
        loop_changed: bool,
    },
    /// A prepared mix (or LegacyDirect) and its survivor plan; whatever leaves the graph
    /// goes back through the trash queue inside the same envelope.
    SetMix(Box<crate::mixer::live::MixInstall>),
    /// Channel removals and additions, an optional timeline and the matching mix, applied
    /// together at one block boundary or not at all; everything displaced or refused
    /// returns in the same envelope.
    Sources(Box<SourceTransaction>),
    /// Queues [`crate::RecordEvent::Mark`] with this id in the recording queue.
    RecordMark(u32),
    /// Completion identity for an exhausted recovery transaction.
    RecoveryStop(u32),
}

impl Msg {
    /// Whether applying the message may hand something back through the trash queue: such a
    /// message waits while the processor holds a parked item (it has room for only one).
    /// A plain removal keeps that same ordering fence as a timeline or source removal.
    pub(crate) fn makes_trash(&self) -> bool {
        !matches!(
            self,
            Self::Command { .. }
                | Self::RecordMark(_)
                | Self::RecoveryStop(_)
                | Self::TimelineRebuilt { .. }
        )
    }
}

/// One control-allocated envelope is also the retirement container for both payloads.
#[derive(Clone, Debug)]
pub struct TimelineUpdate {
    pub pattern: Box<PatternData>,
    pub song: Box<crate::SongData>,
    pub bpm: f32,
    pub mode: crate::PlaybackMode,
    pub region: Option<crate::SongLoop>,
    pub remove_channel: Option<ChannelId>,
}

/// What is left of the queued messages, in queue order, once the graph is rebuilt from the
/// controller's state (which each message updated when it was queued), so that the rebuilt
/// graph ends up as if every message had reached the old one:
///
/// * Dropped: what only changes the graph, since the rebuilt one already has it (a graph, a
///   channel added or removed, a sample, a pattern, the tone, a synth, mix, sampler or channel
///   parameter, a choke group), and a note-on that a later message would have cut (a new graph
///   or `AllSoundOff` for any note; a removal or a new sample for a note on that channel).
/// * Kept: the transport commands, the recording, metronome and time-signature switches and the
///   recording barriers (the processor owns them, so they outlive the graph; `AllSoundOff` keeps only its `Stop`, its
///   fade, choke and tone-off being in the rebuilt graph), the other notes (note-offs always:
///   one that finds no note does nothing), and the test hooks.
pub(crate) fn rebuilt_queue(queued: Vec<Msg>) -> Vec<Msg> {
    // From the back: whether a later message cuts every note, and the channels whose notes a
    // later message cuts.
    let mut all_cut = false;
    let mut cut: Vec<ChannelId> = Vec::new();
    let mut kept = Vec::with_capacity(queued.len());
    for msg in queued.into_iter().rev() {
        let (due, cmd, mode_changed) = match msg {
            Msg::ReplaceGraph(_) => {
                all_cut = true;
                continue;
            }
            Msg::RemoveChannel(id) | Msg::SetSample { channel: id, .. } => {
                cut.push(id);
                continue;
            }
            Msg::AddChannel(_) | Msg::SetPattern(_) | Msg::SetSong(_) | Msg::SetMix(_) => {
                continue;
            }
            // The rebuilt graph already has its sources and mix; only its transport
            // chronology stays, and its removals cut earlier notes.
            Msg::Sources(t) => {
                cut.extend(t.removes.iter().copied());
                if let Some((update, mode_changed, loop_changed)) = &t.timeline {
                    kept.push(Msg::TimelineRebuilt {
                        bpm: update.bpm,
                        mode: update.mode,
                        region: update.region,
                        mode_changed: *mode_changed,
                        loop_changed: *loop_changed,
                    });
                }
                continue;
            }
            Msg::Timeline(update, mode_changed, loop_changed) => {
                if let Some(id) = update.remove_channel {
                    cut.push(id);
                }
                kept.push(Msg::TimelineRebuilt {
                    bpm: update.bpm,
                    mode: update.mode,
                    region: update.region,
                    mode_changed,
                    loop_changed,
                });
                continue;
            }
            transport @ Msg::TimelineRebuilt { .. } => {
                kept.push(transport);
                continue;
            }
            recovery @ Msg::RecoveryStop(_) => {
                kept.push(recovery);
                continue;
            }
            Msg::RecordMark(id) => {
                kept.push(Msg::RecordMark(id));
                continue;
            }
            Msg::Command {
                due,
                cmd,
                mode_changed,
            } => (due, cmd, mode_changed),
        };
        let cmd = match cmd {
            Command::TestTone { .. }
            | Command::SetSynthParam { .. }
            | Command::SetChannelMix { .. }
            | Command::SetSamplerTrim { .. }
            | Command::SetSamplerParam { .. }
            | Command::SetChannelParam { .. }
            | Command::SetChoke { .. } => continue,
            Command::AllSoundOff => {
                all_cut = true;
                Command::Stop
            }
            Command::Note { on: true, .. } if all_cut => continue,
            Command::ChannelNote {
                channel, on: true, ..
            } if all_cut || cut.contains(&channel) => continue,
            Command::Note { .. }
            | Command::ChannelNote { .. }
            | Command::SetPlaybackMode { .. }
            | Command::SetSongLoop { .. }
            | Command::Seek { .. }
            | Command::Play
            | Command::PlayLoops { .. }
            | Command::Stop
            | Command::SetTempo { .. }
            | Command::SetRecording { .. }
            | Command::SetMetronome { .. }
            | Command::SetTimeSignature { .. } => cmd,
            #[cfg(feature = "test-hooks")]
            Command::InjectNaN | Command::InjectPanic => cmd,
        };
        kept.push(Msg::Command {
            due,
            cmd,
            mode_changed,
        });
    }
    kept.reverse();
    kept
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
    Song(Box<crate::SongData>),
    Timeline(Box<TimelineUpdate>),
    Mix(Box<crate::mixer::live::MixInstall>),
    FaultedMix(Box<crate::mixer::live::LiveMix>),
    Sources(Box<SourceTransaction>),
}

/// One owning source-set transaction (see [`Msg::Sources`]). Built and fully validated on
/// the control thread with room reserved for everything the callback may hand back.
pub(crate) struct SourceTransaction {
    pub(crate) removes: Vec<ChannelId>,
    /// New nodes in order; a slot is empty once the graph took its node.
    pub(crate) adds: Vec<Option<Box<ChannelNode>>>,
    /// The timeline and its admission-time mode/loop transition flags.
    pub(crate) timeline: Option<(Box<TimelineUpdate>, bool, bool)>,
    pub(crate) mix: crate::mixer::live::MixInstall,
    pub(crate) applied: bool,
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
                id: NoteId(7),
            }
            .clamped()
        };
        let note_ok = |key, velocity| {
            Some(Command::Note {
                key,
                velocity,
                on: true,
                id: NoteId(7),
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
        let tempo = |bpm| Command::SetTempo { bpm }.clamped();
        let tempo_ok = |bpm| Some(Command::SetTempo { bpm });
        assert_eq!(tempo(1_000.0), tempo_ok(400.0));
        assert_eq!(tempo(-5.0), tempo_ok(20.0));
        assert_eq!(tempo(128.5), tempo_ok(128.5));
        assert_eq!(tempo(f32::NAN), None);
        assert_eq!(tempo(f32::INFINITY), None);
        assert_eq!(Command::PlayLoops { loops: 0 }.clamped(), None);
        let two = Command::PlayLoops { loops: 2 };
        assert_eq!(two.clamped(), Some(two));

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
                id: NoteId(7),
            }
            .clamped()
        };
        let cnote_ok = |key, velocity| {
            Some(Command::ChannelNote {
                channel: id,
                key,
                velocity,
                on: true,
                id: NoteId(7),
            })
        };
        assert_eq!(cnote(60, 5.0), cnote_ok(60, 1.0));
        assert_eq!(cnote(60, -1.0), cnote_ok(60, 0.0));
        assert_eq!(cnote(128, 0.5), None);
        assert_eq!(cnote(60, f32::NAN), None);
        // A live note never carries a sequencer id, so it can never end a sequenced note.
        let sequenced = NoteId(NoteId::SEQUENCED | 7);
        let as_sequenced = |cmd| match cmd {
            Command::ChannelNote {
                channel,
                key,
                velocity,
                on,
                ..
            } => Command::ChannelNote {
                channel,
                key,
                velocity,
                on,
                id: sequenced,
            },
            Command::Note {
                key, velocity, on, ..
            } => Command::Note {
                key,
                velocity,
                on,
                id: sequenced,
            },
            other => other,
        };
        for cmd in [cnote_ok(60, 0.5).unwrap(), note_ok(60, 0.5).unwrap()] {
            assert_eq!(cmd.clamped(), Some(cmd));
            assert_eq!(as_sequenced(cmd).clamped(), None);
        }
        assert!(NoteId::live().is_live());

        let choke = |group| Command::SetChoke { channel: id, group }.clamped();
        assert_eq!(
            choke(0),
            Some(Command::SetChoke {
                channel: id,
                group: 0
            })
        );
        assert_eq!(
            choke(MAX_CHOKE_GROUP),
            Some(Command::SetChoke {
                channel: id,
                group: MAX_CHOKE_GROUP
            })
        );
        assert_eq!(choke(MAX_CHOKE_GROUP + 1), None);
        let cparam = |value| {
            Command::SetChannelParam {
                channel: id,
                index: 3,
                value,
            }
            .clamped()
        };
        assert_eq!(
            cparam(1e9),
            Some(Command::SetChannelParam {
                channel: id,
                index: 3,
                value: 1e9
            })
        );
        assert_eq!(cparam(f32::NAN), None);

        let sig = |beats, unit| Command::SetTimeSignature { beats, unit }.clamped();
        for (beats, unit) in [(1, 2), (3, 4), (7, 8), (16, 16)] {
            assert_eq!(
                sig(beats, unit),
                Some(Command::SetTimeSignature { beats, unit })
            );
        }
        for (beats, unit) in [(0, 4), (17, 4), (4, 1), (4, 3), (4, 32)] {
            assert_eq!(sig(beats, unit), None, "{beats}/{unit}");
        }
    }

    #[test]
    fn ids_do_not_repeat_past_the_old_31_bit_wrap() {
        // Note 0 and note 2^31 (where a 31-bit count wrapped) and their neighbours: all
        // distinct, and each in its own half.
        let around = [0, 1, (1 << 31) - 1, 1 << 31, (1 << 31) + 1, 1 << 32];
        let live: Vec<NoteId> = around.iter().map(|&n| NoteId::live_number(n)).collect();
        let seq: Vec<NoteId> = around
            .iter()
            .map(|&n| NoteId::sequenced_number(n))
            .collect();
        let mut all = [live.clone(), seq.clone()].concat();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 2 * around.len(), "{live:?} {seq:?}");
        assert!(live.iter().all(|id| id.is_live()));
        assert!(seq.iter().all(|id| !id.is_live()));
    }
}
