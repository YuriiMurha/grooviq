//! The control-thread half: validates commands, mirrors the state they set, builds channels,
//! frees what the processor hands back and rebuilds the graph after a panic. May allocate and
//! block; never call it from the audio thread.

use crate::channel::ChannelNode;
use crate::command::{Command, Msg, Trash, rebuilt_queue};
use crate::graph::{Graph, MAX_LEAVING, ramp_len};
use crate::live::{LiveShared, LiveSnapshot};
use crate::mixer::{MASTER, MixerTrackId, memory::MixerMemory};
use crate::mixer_meter::{MIXER_METER_MAX_TRACKS, MeterGraph, MixerMeter};
use crate::processor::AudioProcessor;
use crate::record::RecordEvent;
use crate::state::{
    ChannelId, ChannelSetup, EngineState, Instrument, InstrumentKind, MAX_CHANNELS, PatternData,
};
use crate::{EngineConfig, QUEUE_CAPACITY};
use grooviq_dsp::sampler::SampleBuf;
use rtrb::{Consumer, Producer};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What [`Controller::check_health`] found since its previous call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthEvent {
    /// The graph panicked and was rebuilt (by `check_health`, or by `reconfigure` if it came
    /// first) from the exact controller state; `faults` counts every panic so far.
    Restarted { faults: u32 },
    /// Automatic recovery exhausted its budget; explicit load or reconfigure can retry.
    Stopped { faults: u32 },
    /// A constructor failed. The mirrored state was preserved.
    RecoveryFailed { faults: u32 },
    /// Successful processing or an explicit recovery renewed the retry budget.
    RecoveryReset,
    /// The NaN guard silenced and reset a node; `total` counts every such block so far.
    DeviceSilenced { total: u32 },
}

/// Why a command or a change was refused. Nothing is queued or changed when it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// A value was not finite, or a key, parameter index or pattern length is out of range.
    Rejected,
    /// Valid state could not be built; the previous state is preserved.
    BuildFailed,
    /// Fixed-second audio exceeds the admitted overlap budget.
    AudioVoiceLimit,
    /// The queue had no room.
    Full,
    /// No channel has that id.
    UnknownChannel,
    /// A channel with that id exists already.
    ChannelExists,
    /// The engine already holds [`MAX_CHANNELS`] channels.
    TooManyChannels,
    /// The channel's instrument has no such thing (a sample or a sampler parameter for a synth
    /// channel).
    WrongInstrument,
    /// A removal needs more fade room, or a mix change would displace a fading route.
    /// Retry once the fades are collected.
    Busy,
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AudioVoiceLimit => "audio needs more than 256 simultaneous instances at 400 BPM; shorten trims, remove overlaps or lengthen the loop",
            Self::Rejected => "a value is out of range or not a number",
            Self::BuildFailed => "the sound engine could not build the project",
            Self::Full => "the engine's command queue is full",
            Self::UnknownChannel => "no channel has that id",
            Self::ChannelExists => "a channel with that id exists already",
            Self::TooManyChannels => "the engine holds as many channels as it can",
            Self::WrongInstrument => "the channel's instrument has no such setting",
            Self::Busy => "a removed channel is still fading out through the mix; try again",
        })
    }
}

impl std::error::Error for ControlError {}

#[derive(Debug)]
pub(crate) enum ReconfigureError {
    InsertRate(crate::mixer::UnsupportedInsertRate),
    MixRefused(ControlError),
    BuildFailed,
}

impl std::fmt::Display for ReconfigureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsertRate(error) => error.fmt(f),
            Self::MixRefused(ControlError::Full) => f.write_str("routed mixer memory is busy"),
            Self::MixRefused(error) => write!(
                f,
                "mixer preparation at the new output rate failed: {error}"
            ),
            Self::BuildFailed => {
                f.write_str("the sound engine could not rebuild for the output rate")
            }
        }
    }
}

/// A removed channel instance's fade route (see `Controller::fades`).
#[derive(Clone, Copy, Debug)]
struct Fade {
    instance: u64,
    /// None retains the LegacyDirect sink; Some retains a routed track.
    track: Option<crate::mixer::MixerTrackId>,
    /// The graph build that was newest when it was removed: it fades in that graph or a
    /// later one, so retiring any graph up to `epoch` cannot retire it early.
    epoch: u64,
}

/// One atomic change of the engine's source set and mix (see
/// [`Controller::set_sources_and_mix`]).
#[derive(Clone, Debug)]
pub struct SourceEdit {
    /// Channels removed first; each fades out on the route it had.
    pub remove: Vec<ChannelId>,
    /// Channels added after the removals, in order (an ID just removed may be reused).
    pub add: Vec<ChannelSetup>,
    /// A new timeline; its `remove_channel` must be `None` (use `remove`).
    pub timeline: Option<crate::TimelineUpdate>,
    /// The mix for the resulting sources.
    pub mix: crate::mixer::MixSetup,
}

/// The test tone as last set.
#[derive(Clone, Copy, Debug)]
struct ToneState {
    on: bool,
    hz: f32,
    gain_db: f32,
}

impl Default for ToneState {
    fn default() -> Self {
        Self {
            on: false,
            hz: 440.0,
            gain_db: -12.0,
        }
    }
}

/// Every synth parameter at its default.
fn synth_defaults() -> Vec<f32> {
    grooviq_dsp::param_spec("synth")
        .unwrap_or_default()
        .iter()
        .map(|&(_, _, default)| default)
        .collect()
}

/// Sends commands to the [`AudioProcessor`], adds and removes its channels and watches its
/// health. Created by [`crate::engine`] or [`crate::engine_with`].
pub struct Controller {
    commands: Producer<Msg>,
    trash: Consumer<Trash>,
    recorded: Consumer<RecordEvent>,
    live: Arc<LiveShared>,
    sample_rate: f32,
    mixer_memory: MixerMemory,
    /// The state the queued commands lead to, to rebuild the graph from.
    tone: ToneState,
    synth: Vec<f32>,
    state: EngineState,
    /// Which channel each id is now, see [`Self::channel_instance`].
    instances: BTreeMap<ChannelId, u64>,
    next_instance: u64,
    /// Removed channel instances that may still be fading through a routed mix, on the
    /// track each had: every prepared mix carries a route for them until they retire.
    fades: Vec<Fade>,
    /// Counts graph builds; each graph and each fade remembers the build it belongs to.
    graph_epoch: u64,
    /// The revision of the mix the queued messages lead to; never reused (a loaded state
    /// takes a new one), so an install prepared against any other revision is stale.
    mix_revision: u64,
    /// Counts `reconfigure` calls. Every graph carries the generation it was built in, and the
    /// processor drops a replacement from an earlier one.
    generation: u64,
    /// The id of the last recording barrier `load_state` queued (see [`Self::record_mark`]).
    record_mark: u32,
    recording_armed: bool,
    /// Faults already answered with a restart (queued by `check_health`, or done by
    /// `reconfigure`).
    seen_faults: u32,
    /// A restart happened that `check_health` has not reported yet.
    restart_owed: bool,
    seen_guard_hits: u32,
    attempts: u8,
    retry_at: Option<Instant>,
    stopped: bool,
    stop_owed: bool,
    recovery_stop_mark: Option<u32>,
    pending_recovery_stop: Option<u32>,
    reset_owed: bool,
    /// Test switch: this many replays of the state panic, as a device bug in `set_param` would.
    #[cfg(test)]
    replay_panics: std::cell::Cell<u32>,
}

impl Controller {
    #[cfg(test)]
    pub(crate) fn set_mixer_memory_limit(&mut self, limit: u64) {
        self.mixer_memory.set_limit(limit);
    }

    #[cfg(test)]
    pub(crate) fn fail_replays(&self, count: u32) {
        self.replay_panics.set(count);
    }

    pub(crate) fn new(
        sample_rate: f32,
        (commands, trash, recorded): (Producer<Msg>, Consumer<Trash>, Consumer<RecordEvent>),
        live: Arc<LiveShared>,
        state: EngineState,
    ) -> Self {
        let mut controller = Self {
            commands,
            trash,
            recorded,
            live,
            sample_rate,
            mixer_memory: MixerMemory::default(),
            tone: ToneState::default(),
            synth: synth_defaults(),
            state,
            instances: BTreeMap::new(),
            next_instance: 0,
            generation: 0,
            mix_revision: 0,
            fades: Vec::new(),
            graph_epoch: 0,
            record_mark: 0,
            recording_armed: false,
            seen_faults: 0,
            restart_owed: false,
            seen_guard_hits: 0,
            attempts: 0,
            retry_at: None,
            stopped: false,
            stop_owed: false,
            recovery_stop_mark: None,
            pending_recovery_stop: None,
            reset_owed: false,
            #[cfg(test)]
            replay_panics: std::cell::Cell::new(0),
        };
        controller.renew_instances();
        controller
    }

    /// Gives every channel of the state a new instance number.
    fn renew_instances(&mut self) {
        self.instances.clear();
        for channel in &self.state.channels {
            self.next_instance += 1;
            self.instances.insert(channel.id, self.next_instance);
        }
    }

    /// Which channel `id` is now: a number that is new for every channel added (also by
    /// [`Self::load_state`]) and never reused, so a caller that kept it can tell the channel it
    /// meant from another that took the same id later. `None` if there is no such channel.
    pub fn channel_instance(&self, id: ChannelId) -> Option<u64> {
        self.state.channel(id)?;
        self.instances.get(&id).copied()
    }

    /// Queues `cmd` for the next block, with its values clamped into range. Gives it back if
    /// it is rejected (a value not finite, an unknown key, parameter index or channel) or the
    /// queue is full.
    // The bounded inline sampler snapshot avoids allocation/free on the callback.
    #[allow(clippy::result_large_err)]
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        self.send_at(0, cmd).map_err(|_| cmd)
    }

    /// Queues `cmd` for the block that contains frame `due` (0: the next block); a note lands
    /// on that exact frame. The processor holds back everything behind a command that is not
    /// due yet, so send in frame order. For tests and the offline renderer: not part of the
    /// API.
    #[doc(hidden)]
    pub fn send_at(&mut self, due: u64, cmd: Command) -> Result<(), ControlError> {
        self.flush_recovery_stop();
        if self.stop_owed {
            return Err(ControlError::Full);
        }
        let mut cmd = cmd.clamped().ok_or(ControlError::Rejected)?;
        if let Command::Seek { tick } = &mut cmd
            && let Some(r) = self.state.song_loop
        {
            *tick = (*tick).clamp(r.start_tick, r.end_tick - 1);
        }
        if let Command::SetSongLoop { region } = cmd {
            self.state.song.audio.validate_loop(region)?;
        }
        match cmd {
            Command::Seek { tick }
                if self.state.playback_mode != crate::PlaybackMode::Song
                    || tick > self.state.song.end_tick =>
            {
                return Err(ControlError::Rejected);
            }
            Command::SetSongLoop { region } if !self.state.song.valid_loop(region) => {
                return Err(ControlError::Rejected);
            }
            Command::SetRecording { on: true }
                if self.state.playback_mode == crate::PlaybackMode::Song =>
            {
                return Err(ControlError::Rejected);
            }
            Command::PlayLoops { .. } if self.state.playback_mode == crate::PlaybackMode::Song => {
                return Err(ControlError::Rejected);
            }
            _ => {}
        }
        if let Some(id) = cmd.channel() {
            let kind = self
                .state
                .channel(id)
                .ok_or(ControlError::UnknownChannel)?
                .instrument
                .kind();
            match &mut cmd {
                Command::SetSamplerTrim { instance, .. }
                    if kind != InstrumentKind::Sampler
                        || self.instances.get(&id) != Some(instance) =>
                {
                    return Err(ControlError::Rejected);
                }
                Command::SetSamplerParam { .. } if kind != InstrumentKind::Sampler => {
                    return Err(ControlError::WrongInstrument);
                }
                Command::SetChannelParam { index, value, .. } => {
                    let &(min, max, _) = usize::try_from(*index)
                        .ok()
                        .and_then(|i| kind.spec().get(i))
                        .ok_or(ControlError::Rejected)?;
                    *value = value.clamp(min, max);
                }
                _ => {}
            }
        }
        // Capture the accepted transition before the mirror moves; rebuilt graphs already
        // contain the final mode, but queued stop/disarm effects still belong in order.
        let mode_changed =
            matches!(cmd, Command::SetPlaybackMode { mode } if mode != self.state.playback_mode);
        self.commands
            .push(Msg::Command {
                due,
                cmd,
                mode_changed,
            })
            .map_err(|_| ControlError::Full)?;
        self.mirror(cmd);
        Ok(())
    }

    /// The state the queued commands and changes lead to.
    pub fn state(&self) -> &EngineState {
        &self.state
    }

    /// Moves every recorded event waiting into `out` (appended, in the order they happened)
    /// and returns how many. Never blocks.
    pub fn take_recorded(&mut self, out: &mut Vec<RecordEvent>) -> usize {
        let n = self.recorded.slots();
        out.reserve(n);
        // Only this side pops: the `n` waiting are there.
        out.extend(std::iter::from_fn(|| self.recorded.pop().ok()).take(n));
        n
    }

    /// Recorded events dropped so far because the queue was full (wraps).
    pub fn record_dropped(&self) -> u32 {
        self.live.record_dropped()
    }

    /// Adds a channel after the others, at its mix from the first frame (no fade-in). Values
    /// are clamped, missing instrument parameters take their defaults and extra ones are
    /// dropped; a choke group past [`crate::MAX_CHOKE_GROUP`] is `Rejected`.
    pub fn add_channel(&mut self, setup: ChannelSetup) -> Result<(), ControlError> {
        // A routed mix needs a route for the new instance in the same installation.
        if self.state.mix.routed().is_some() {
            let mix = self.state.mix.clone();
            return self
                .set_sources_and_mix(SourceEdit {
                    remove: Vec::new(),
                    add: vec![setup],
                    timeline: None,
                    mix,
                })
                .map(|_| ());
        }
        if self.state.channel(setup.id).is_some() {
            return Err(ControlError::ChannelExists);
        }
        if self.state.channels.len() >= MAX_CHANNELS {
            return Err(ControlError::TooManyChannels);
        }
        let setup = setup.checked()?;
        let any_solo = self.state.any_solo();
        let sample_rate = self.sample_rate;
        // A device bug in `set_param` must not unwind into the caller (and a mutex it holds).
        let node = catch_unwind(AssertUnwindSafe(|| {
            Box::new(ChannelNode::new(
                sample_rate,
                ramp_len(sample_rate),
                &setup,
                any_solo,
            ))
        }))
        .map_err(|_| ControlError::Rejected)?;
        let mut node = node;
        node.instance = self.next_instance + 1;
        self.push(Msg::AddChannel(node))?;
        self.next_instance += 1;
        self.instances.insert(setup.id, self.next_instance);
        self.state.channels.push(setup);
        Ok(())
    }

    /// Removes a channel with a 5 ms fade, or returns `Busy` if there is no fade room.
    /// The processor hands the channel back to be freed here.
    pub fn remove_channel(&mut self, id: ChannelId) -> Result<(), ControlError> {
        let index = self.channel_index(id)?;
        if self
            .state
            .song
            .arrangement
            .notes
            .iter()
            .any(|n| n.channel == id)
            || self.state.song.automation.references(id)
        {
            return Err(ControlError::Rejected);
        }
        self.check_fade_room(1)?;
        self.push(Msg::RemoveChannel(id))?;
        self.state.channels.remove(index);
        self.record_fade(id);
        self.instances.remove(&id);
        Ok(())
    }

    /// Gives a sampler channel a new sample (or none), silencing its notes. The processor hands
    /// the old one back to be freed here. A synth channel is `WrongInstrument`.
    pub fn set_channel_sample(
        &mut self,
        id: ChannelId,
        sample: Option<Arc<SampleBuf>>,
    ) -> Result<(), ControlError> {
        let index = self.channel_index(id)?;
        if self.state.channels[index].instrument.kind() != InstrumentKind::Sampler {
            return Err(ControlError::WrongInstrument);
        }
        self.push(Msg::SetSample {
            channel: id,
            sample: sample.clone(),
        })?;
        if let Instrument::Sampler { sample: held, .. } = &mut self.state.channels[index].instrument
        {
            *held = sample;
        }
        Ok(())
    }

    /// Replaces the pattern. Velocities are clamped and notes sorted by tick, then channel;
    /// a length outside 1 tick..=256 steps, a key above 127 or a velocity that is not a number
    /// is `Rejected`.
    pub fn set_song(&mut self, song: crate::SongData) -> Result<(), ControlError> {
        let ids: Vec<_> = self.state.channels.iter().map(|c| c.id).collect();
        let song = song.checked(&ids)?;
        song.automation.validate(&self.state.channels)?;
        song.audio.validate_loop(self.state.song_loop)?;
        self.state.mix.covers(&self.state.channels, &song)?;
        if !song.valid_loop(self.state.song_loop) {
            return Err(ControlError::Rejected);
        }
        self.push(Msg::SetSong(Box::new(song.clone())))?;
        self.state.song = song;
        Ok(())
    }

    /// Publishes a complete musical candidate through one queue admission.
    pub fn set_timeline(&mut self, update: crate::TimelineUpdate) -> Result<(), ControlError> {
        if let Some(id) = update.remove_channel {
            self.channel_index(id)?;
        }
        let channels: Vec<_> = self
            .state
            .channels
            .iter()
            .filter(|c| Some(c.id) != update.remove_channel)
            .cloned()
            .collect();
        let ids: Vec<_> = channels.iter().map(|c| c.id).collect();
        let pattern = (*update.pattern).clone().checked()?;
        let song = (*update.song).clone().checked(&ids)?;
        song.automation.validate(&channels)?;
        if !update.bpm.is_finite()
            || !(20.0..=400.0).contains(&update.bpm)
            || !song.valid_loop(update.region)
        {
            return Err(ControlError::Rejected);
        }
        self.state.mix.covers(&channels, &song)?;
        self.check_fade_room(usize::from(update.remove_channel.is_some()))?;
        self.push(Msg::Timeline(
            Box::new(crate::TimelineUpdate {
                pattern: Box::new(pattern.clone()),
                song: Box::new(song.clone()),
                ..update.clone()
            }),
            self.state.playback_mode != update.mode,
            self.state.song_loop != update.region,
        ))?;
        if let Some(id) = update.remove_channel {
            self.state.channels.retain(|c| c.id != id);
            self.record_fade(id);
            self.instances.remove(&id);
        }
        self.state.pattern = pattern;
        self.state.song = song;
        self.state.bpm = update.bpm;
        if self.state.playback_mode != update.mode {
            self.recording_armed = false;
        }
        self.state.playback_mode = update.mode;
        self.state.song_loop = update.region;
        Ok(())
    }

    /// Replaces how sources reach the output, without rebuilding the graph: instruments,
    /// held notes, song audio voices and the transport carry on. The new mix is prepared
    /// here (devices constructed and set, buffers reserved) and installed at the next block
    /// boundary as one queue message. An insert that keeps its ID, kind, seed and latency
    /// keeps its live device and only receives the parameters that changed.
    /// Refused as a whole (`Rejected` for an invalid mix, `BuildFailed` if a device panics,
    /// `Full` when the command queue or shared mixer allowance is occupied): then nothing
    /// changes. Returns the new revision.
    pub fn set_mix(&mut self, mix: crate::mixer::MixSetup) -> Result<u64, ControlError> {
        use crate::mixer::live;
        mix.covers(&self.state.channels, &self.state.song)?;
        let aliases = aliases(&mix, &self.instances, &self.fades)?;
        let rate = self.sample_rate;
        let prepared = catch_unwind(AssertUnwindSafe(|| {
            live::prepare(&mix, rate, aliases, &self.mixer_memory)
        }))
        .map_err(|_| ControlError::BuildFailed)??;
        let changes = live::plan(&self.state.mix, &mix, &self.mixer_memory)?;
        let revision = self.mix_revision + 1;
        self.push(Msg::SetMix(Box::new(live::MixInstall {
            generation: self.generation,
            base: self.mix_revision,
            revision,
            live: prepared,
            changes,
            installed: false,
        })))?;
        self.mix_revision = revision;
        self.state.mix = mix;
        Ok(revision)
    }

    /// Changes the source set and the mix in one queue admission: channel removals, then
    /// additions, an optional timeline (pattern, song, tempo, mode, loop) and the mix that
    /// routes the result, all applied at one block boundary. Nothing is rebuilt: other
    /// channels keep their notes, song audio on a kept bank keeps its voices, inserts that
    /// survive keep their state, and removed channels fade on the route they had.
    ///
    /// Everything is validated and prepared here first; any refusal (`Rejected`,
    /// `UnknownChannel`, `ChannelExists`, `TooManyChannels`, `Busy`, `BuildFailed`, `Full`)
    /// changes nothing. A routed mix must name every resulting channel and every playlist
    /// track the song's audio clips use. Returns the new mix revision.
    pub fn set_sources_and_mix(&mut self, edit: SourceEdit) -> Result<u64, ControlError> {
        use crate::mixer::{SourceId, live};
        let SourceEdit {
            remove,
            add,
            timeline,
            mix,
        } = edit;
        if timeline
            .as_ref()
            .is_some_and(|u| u.remove_channel.is_some())
        {
            return Err(ControlError::Rejected);
        }
        let mut channels = self.state.channels.clone();
        let mut instances = self.instances.clone();
        for (i, id) in remove.iter().enumerate() {
            if remove[..i].contains(id) {
                return Err(ControlError::Rejected);
            }
            let at = channels
                .iter()
                .position(|c| c.id == *id)
                .ok_or(ControlError::UnknownChannel)?;
            channels.remove(at);
            instances.remove(id);
        }
        let mut next = self.next_instance;
        let mut added = Vec::with_capacity(add.len());
        for setup in add {
            if channels.iter().any(|c| c.id == setup.id) {
                return Err(ControlError::ChannelExists);
            }
            if channels.len() >= MAX_CHANNELS {
                return Err(ControlError::TooManyChannels);
            }
            let setup = setup.checked()?;
            next += 1;
            instances.insert(setup.id, next);
            channels.push(setup.clone());
            added.push((setup, next));
        }
        let ids: Vec<_> = channels.iter().map(|c| c.id).collect();
        let candidate = match &timeline {
            Some(u) => {
                if !u.bpm.is_finite() || !(20.0..=400.0).contains(&u.bpm) {
                    return Err(ControlError::Rejected);
                }
                (
                    (*u.pattern).clone().checked()?,
                    (*u.song).clone().checked(&ids)?,
                    u.bpm,
                    u.mode,
                    u.region,
                )
            }
            None => (
                self.state.pattern.clone(),
                self.state.song.clone().checked(&ids)?,
                self.state.bpm,
                self.state.playback_mode,
                self.state.song_loop,
            ),
        };
        let (pattern, song, bpm, mode, region) = candidate;
        song.automation.validate(&channels)?;
        if !song.valid_loop(region) {
            return Err(ControlError::Rejected);
        }
        mix.covers(&channels, &song)?;
        self.check_fade_room(remove.len())?;
        let mut fades = self.fades.clone();
        fades.extend(remove.iter().map(|id| {
            Fade {
                instance: self.instances.get(id).copied().unwrap_or(0),
                track: self
                    .state
                    .mix
                    .destination(SourceId::Channel(u64::from(id.0))),
                epoch: self.graph_epoch,
            }
        }));
        let aliases = aliases(&mix, &instances, &fades)?;
        let rate = self.sample_rate;
        let ramp = ramp_len(rate);
        let any_solo = channels.iter().any(|c| c.mix.solo);
        let built = catch_unwind(AssertUnwindSafe(|| {
            let live = live::prepare(&mix, rate, aliases, &self.mixer_memory)?;
            let nodes: Vec<_> = added
                .iter()
                .map(|(setup, instance)| {
                    let mut node = Box::new(ChannelNode::new(rate, ramp, setup, any_solo));
                    node.instance = *instance;
                    Some(node)
                })
                .collect();
            Ok::<_, ControlError>((nodes, live))
        }))
        .map_err(|_| ControlError::BuildFailed)??;
        let (adds, prepared) = built;
        let changes = live::plan(&self.state.mix, &mix, &self.mixer_memory)?;
        let revision = self.mix_revision + 1;
        let transaction = crate::command::SourceTransaction {
            removes: remove,
            adds,
            timeline: timeline.as_ref().map(|_| {
                (
                    Box::new(crate::TimelineUpdate {
                        pattern: Box::new(pattern.clone()),
                        song: Box::new(song.clone()),
                        bpm,
                        mode,
                        region,
                        remove_channel: None,
                    }),
                    self.state.playback_mode != mode,
                    self.state.song_loop != region,
                )
            }),
            mix: live::MixInstall {
                generation: self.generation,
                base: self.mix_revision,
                revision,
                live: prepared,
                changes,
                installed: false,
            },
            applied: false,
        };
        self.push(Msg::Sources(Box::new(transaction)))?;
        // Admitted: only now does the mirror change, all of it.
        self.state.channels = channels;
        self.instances = instances;
        self.next_instance = next;
        self.fades = fades;
        if timeline.is_some() {
            self.state.pattern = pattern;
            self.state.song = song;
            self.state.bpm = bpm;
            if self.state.playback_mode != mode {
                self.recording_armed = false;
            }
            self.state.playback_mode = mode;
            self.state.song_loop = region;
        }
        self.state.mix = mix;
        self.mix_revision = revision;
        Ok(revision)
    }

    /// Charge every accepted removal until its node or graph comes back, including
    /// removals still in the command queue and silent nodes waiting for trash space.
    fn check_fade_room(&mut self, removals: usize) -> Result<(), ControlError> {
        if removals > 0 {
            self.free_trash();
            if self.fades.len() + removals > MAX_LEAVING {
                return Err(ControlError::Busy);
            }
        }
        Ok(())
    }

    /// Records the fade route of channel `id`'s current instance, which is being removed.
    fn record_fade(&mut self, id: ChannelId) {
        let source = crate::mixer::SourceId::Channel(u64::from(id.0));
        if let Some(&instance) = self.instances.get(&id) {
            self.fades.push(Fade {
                instance,
                track: self.state.mix.destination(source),
                epoch: self.graph_epoch,
            });
        }
    }

    /// The revision of the latest accepted mix (see [`Self::set_mix`]).
    pub fn mix_revision(&self) -> u64 {
        self.mix_revision
    }

    pub fn set_pattern(&mut self, pattern: PatternData) -> Result<(), ControlError> {
        let pattern = pattern.checked()?;
        self.push(Msg::SetPattern(Box::new(pattern.clone())))?;
        self.state.pattern = pattern;
        Ok(())
    }

    /// Replaces the whole state: stops the transport, sets the tempo and the time signature,
    /// queues a recording barrier ([`Self::record_mark`]), then swaps in one graph
    /// built from `state` (cutting every note; the test tone and the synth keep their
    /// settings). Checked
    /// like [`Self::add_channel`] and [`Self::set_pattern`], and refused as a whole
    /// (`ChannelExists` for two channels with one id, `TooManyChannels` for more than
    /// [`MAX_CHANNELS`], `Rejected` for a value that is not finite, or `BuildFailed`
    /// if constructing the graph fails; the previous state is preserved).
    pub fn load_state(&mut self, state: EngineState) -> Result<(), ControlError> {
        let state = state.checked()?;
        if self.commands.slots() < 5 {
            return Err(ControlError::Full);
        }
        let mut next_instance = self.next_instance;
        let instances = state
            .channels
            .iter()
            .map(|channel| {
                next_instance += 1;
                (channel.id, next_instance)
            })
            .collect();
        let aliases = aliases(&state.mix, &instances, &[])?;
        let mix = catch_unwind(AssertUnwindSafe(|| {
            crate::mixer::live::prepare(&state.mix, self.sample_rate, aliases, &self.mixer_memory)
        }))
        .map_err(|_| ControlError::BuildFailed)??;
        let old = std::mem::replace(&mut self.state, state);
        let old_instances = std::mem::replace(&mut self.instances, instances);
        self.mix_revision += 1;
        let graph = match self.try_replay_with_mix(mix) {
            Ok(graph) => graph,
            Err(error) => {
                self.state = old;
                self.instances = old_instances;
                self.mix_revision -= 1;
                return Err(error);
            }
        };
        self.next_instance = next_instance;
        if self.state.playback_mode == crate::PlaybackMode::Song {
            self.recording_armed = false;
        }

        self.reset_recovery();
        self.seen_faults = self.live.recovery().0;
        // Only this thread pushes, and the queue had room for all five.
        let command = |cmd| Msg::Command {
            due: 0,
            cmd,
            mode_changed: false,
        };
        let tempo = Command::SetTempo {
            bpm: self.state.bpm,
        };
        let (beats, unit) = self.state.time_signature;
        let signature = Command::SetTimeSignature { beats, unit };
        self.record_mark = self.record_mark.wrapping_add(1);
        let queued = self.commands.push(command(Command::Stop)).is_ok()
            && self.commands.push(command(tempo)).is_ok()
            && self.commands.push(command(signature)).is_ok()
            && self.push(Msg::ReplaceGraph(graph)).is_ok()
            && self.push(Msg::RecordMark(self.record_mark)).is_ok();
        debug_assert!(queued);
        Ok(())
    }

    /// Queues `cmd` (checked as [`Self::send`] does), then a recording barrier, both or
    /// neither. In the recording queue, everything recorded before the command applied comes
    /// before [`crate::RecordEvent::Mark`] with the returned id (the End a `SetRecording { on:
    /// false }` makes included), and everything after it after.
    pub fn send_marked(&mut self, cmd: Command) -> Result<u32, ControlError> {
        self.flush_recovery_stop();
        if self.commands.slots() < 2 {
            return Err(ControlError::Full);
        }
        self.send_at(0, cmd)?;
        let id = self.record_mark.wrapping_add(1);
        self.record_mark = id;
        // Only this thread pushes, and the queue had room for both.
        let queued = self.push(Msg::RecordMark(id));
        debug_assert!(queued.is_ok());
        Ok(id)
    }

    /// Barrier of the latest exhaustion Stop/disarm transaction, once queued.
    pub fn recovery_stop_mark(&self) -> Option<u32> {
        self.recovery_stop_mark
    }

    /// Accepted queued recording state, including disarm on a mode transition (even stopped).
    pub fn recording_armed(&self) -> bool {
        self.recording_armed
    }

    /// The id of the last recording barrier queued (by [`Self::load_state`] or
    /// [`Self::send_marked`]): in the recording queue, every event before
    /// [`crate::RecordEvent::Mark`] with this id came before it, every event after it after.
    pub fn record_mark(&self) -> u32 {
        self.record_mark
    }

    fn channel_index(&self, id: ChannelId) -> Result<usize, ControlError> {
        self.state
            .channels
            .iter()
            .position(|c| c.id == id)
            .ok_or(ControlError::UnknownChannel)
    }

    fn push(&mut self, msg: Msg) -> Result<(), ControlError> {
        self.commands.push(msg).map_err(|_| ControlError::Full)
    }

    /// Sends a factory preset as `SetSynthParam` for every synth parameter. False if the name
    /// is unknown or the queue has no room for the whole preset (then nothing is sent).
    pub fn synth_preset(&mut self, name: &str) -> bool {
        let Some(params) = grooviq_dsp::synth::preset(name) else {
            return false;
        };
        // The queue only gains room while we push, so this check holds for the whole loop.
        if self.commands.slots() < params.len() {
            return false;
        }
        params
            .into_iter()
            .all(|(index, value)| self.send(Command::SetSynthParam { index, value }).is_ok())
    }

    /// Sends `SetChannelParam` for every `(index, value)` of `params`, all or none: refused
    /// (nothing sent) if the channel is unknown, an index is not one of its instrument's, a value
    /// is not finite, or the queue has no room for all of them.
    pub fn set_channel_params(
        &mut self,
        channel: ChannelId,
        params: &[(u32, f32)],
    ) -> Result<(), ControlError> {
        let kind = self
            .state
            .channel(channel)
            .ok_or(ControlError::UnknownChannel)?
            .instrument
            .kind();
        let spec = kind.spec();
        if params
            .iter()
            .any(|&(index, value)| index as usize >= spec.len() || !value.is_finite())
        {
            return Err(ControlError::Rejected);
        }
        if self.commands.slots() < params.len() {
            return Err(ControlError::Full);
        }
        for &(index, value) in params {
            // Checked above, and the queue only gains room while we push.
            let sent = self.send_at(
                0,
                Command::SetChannelParam {
                    channel,
                    index,
                    value,
                },
            );
            debug_assert!(sent.is_ok(), "{sent:?}");
        }
        Ok(())
    }

    /// Renews the visible-track lease for half a second of rendered audio and reads a
    /// completed capture for this selection and the current mix and graph. First takes up
    /// to 64 distinct known non-Master IDs in caller order, then canonicalizes their order.
    /// An empty selection cancels the lease. Call off the audio thread.
    ///
    /// A pending graph or selection, a fault, or a racing publication can return `None`.
    /// When callbacks stall the frame clock also stalls: displays can expire a capture by
    /// tracking how long its `frames` value has stayed unchanged.
    pub fn mixer_meter(&mut self, tracks: &[MixerTrackId]) -> Option<MixerMeter> {
        let mut selected = [MASTER; MIXER_METER_MAX_TRACKS];
        let mut count = 0;
        if self.state.mix.routed().is_some() {
            for &id in tracks {
                if id != MASTER && !selected[..count].contains(&id) && self.state.mix.has_track(id)
                {
                    selected[count] = id;
                    count += 1;
                    if count == selected.len() {
                        break;
                    }
                }
            }
        }
        let selected = &mut selected[..count];
        selected.sort_unstable();
        self.live.mixer_meter.poll(
            MeterGraph {
                revision: self.mix_revision,
                epoch: self.graph_epoch,
            },
            self.live.frame_clock(),
            (self.sample_rate * 0.5) as u64,
            selected,
        )
    }

    /// The latest live state.
    pub fn live(&self) -> LiveSnapshot {
        self.live.snapshot()
    }

    /// The live state's atomics, for readers on other threads.
    pub fn live_shared(&self) -> Arc<LiveShared> {
        Arc::clone(&self.live)
    }

    /// The rate the engine runs at (after clamping): the output layer compares it with a
    /// device's before deciding to reconfigure.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Frees graphs the processor replaced, restarts the graph after a panic and reports what
    /// happened since the previous call. Call it every 250 ms from a control thread.
    pub fn check_health(&mut self) -> Vec<HealthEvent> {
        self.check_health_at(Instant::now())
    }

    fn flush_recovery_stop(&mut self) {
        if !self.stop_owed || self.commands.slots() < 3 {
            return;
        }
        // Reserve the complete Stop/disarm/barrier transaction before publishing it.
        self.record_mark = self.record_mark.wrapping_add(1);
        for cmd in [Command::Stop, Command::SetRecording { on: false }] {
            let queued = self.commands.push(Msg::Command {
                due: 0,
                cmd,
                mode_changed: false,
            });
            debug_assert!(queued.is_ok());
        }
        let queued = self.commands.push(Msg::RecoveryStop(self.record_mark));
        debug_assert!(queued.is_ok());
        self.pending_recovery_stop = Some(self.record_mark);
        self.recording_armed = false;
        self.recovery_stop_mark = Some(self.record_mark);
        self.stop_owed = false;
    }

    fn reset_recovery(&mut self) {
        self.reset_owed |= self.attempts > 0 || self.stopped;
        self.attempts = 0;
        self.retry_at = None;
        self.stopped = false;
    }

    fn check_health_at(&mut self, now: Instant) -> Vec<HealthEvent> {
        self.free_trash();
        self.flush_recovery_stop();
        let live = self.live.snapshot();
        let (faults, successful_blocks) = self.live.recovery();
        let mut events = Vec::new();
        // Two seconds of actual uninterrupted graph processing, never silent callbacks.
        if faults == self.seen_faults
            && successful_blocks as f64 * crate::BLOCK as f64 >= self.sample_rate as f64 * 2.0
        {
            self.reset_recovery();
        }
        if faults != self.seen_faults && !self.stopped && self.retry_at.is_none_or(|at| now >= at) {
            if self.attempts == 3 {
                self.stopped = true;
                self.stop_owed = true;
                self.flush_recovery_stop();
                events.push(HealthEvent::Stopped { faults });
            } else if self.commands.slots() > 0 {
                // Queue and mixer memory backpressure can clear when the callback hands
                // back its owned payloads. It does not spend the panic retry budget.
                match self.try_build_graph() {
                    Err(ControlError::Full) => {}
                    result => {
                        self.attempts += 1;
                        self.retry_at =
                            Some(now + Duration::from_millis(250 << (self.attempts - 1)));
                        if let Ok(graph) = result {
                            if self.commands.push(Msg::ReplaceGraph(graph)).is_ok() {
                                self.seen_faults = faults;
                                self.restart_owed = true;
                            }
                        } else {
                            events.push(HealthEvent::RecoveryFailed { faults });
                        }
                    }
                }
            }
        }
        if std::mem::take(&mut self.reset_owed) {
            events.push(HealthEvent::RecoveryReset);
        }
        if std::mem::take(&mut self.restart_owed) {
            events.push(HealthEvent::Restarted {
                faults: self.seen_faults,
            });
        }
        if live.guard_hits != self.seen_guard_hits {
            self.seen_guard_hits = live.guard_hits;
            events.push(HealthEvent::DeviceSilenced {
                total: live.guard_hits,
            });
        }
        events
    }

    /// Called only while the output coordinator exclusively owns the detached processor.
    pub(crate) fn settle_recovery_stop(&mut self, processor: &mut AudioProcessor) {
        self.reclaim_detached_trash(processor);
        processor.flush_recording_boundaries();
        if self.stop_owed {
            // Make room in FIFO order until the original transaction can be admitted.
            while self.commands.slots() < 3 {
                self.free_trash();
                assert!(
                    processor.recovery_prefix_step(),
                    "missing pending recovery transaction"
                );
            }
            self.flush_recovery_stop();
        }
        if let Some(mark) = self.pending_recovery_stop {
            while !processor.recovery_stop_completed(mark) {
                self.free_trash();
                assert!(
                    processor.recovery_prefix_step(),
                    "missing pending recovery transaction"
                );
            }
            self.pending_recovery_stop = None;
        }
        processor.flush_recording_boundaries();
        self.free_trash();
    }

    /// Switches the engine to a new sample rate, rebuilding the graph from the controller's
    /// state (which also recovers a graph that panicked; the next `check_health` still reports
    /// the panic). Call it with the audio callback stopped (it needs the processor itself),
    /// never on the audio thread. Returns false without changing the engine if an insert
    /// or compensation plan cannot run at the new rate. If the rest of the graph fails to
    /// build, the processor stays silent and the previous rate, state and queue are retained.
    pub fn reconfigure(&mut self, processor: &mut AudioProcessor, config: EngineConfig) -> bool {
        self.try_reconfigure(processor, config).is_ok()
    }

    pub(crate) fn try_reconfigure(
        &mut self,
        processor: &mut AudioProcessor,
        config: EngineConfig,
    ) -> Result<(), ReconfigureError> {
        let rate = config.rate();
        self.state
            .mix
            .check_sample_rate(rate)
            .map_err(ReconfigureError::InsertRate)?;
        self.reclaim_detached_trash(processor);
        // A refusal to prepare the mixer at the new rate leaves the running graph and its
        // histories alone, including queued updates. Build the rest only after admission.
        let mix = catch_unwind(AssertUnwindSafe(|| self.prepare_replay_mix(rate)))
            .map_err(|_| ReconfigureError::BuildFailed)?
            .map_err(ReconfigureError::MixRefused)?;
        self.settle_recovery_stop(processor);
        // The fresh graph is built from the state every queued message already updated, so a
        // message that only changes the graph would land in it a second time: a channel built
        // for the old rate, a removal of the rebuilt channel, a parameter for a project that
        // was replaced. Those are dropped here, on this thread, with the nodes and samples
        // they carry, as are notes a later message would have cut; transport commands and the
        // other notes go back in the queue, in order (see `rebuilt_queue`). Holding both ends
        // of the queue (`&mut self` and the stopped processor), this sees every message, and
        // what goes back fits.
        let old_rate = self.sample_rate;
        self.sample_rate = rate;
        self.generation += 1;
        let Ok(graph) = self.try_replay_with_mix(mix) else {
            self.sample_rate = old_rate;
            self.generation -= 1;
            processor.stop_after_failed_rebuild();
            return Err(ReconfigureError::BuildFailed);
        };
        self.reset_recovery();
        for msg in rebuilt_queue(processor.take_queued()) {
            let queued = self.commands.push(msg).is_ok();
            debug_assert!(queued, "the queue was just emptied");
        }

        // The fresh graph also recovers from every fault so far, so `check_health` must not
        // restart it again; a fault it has not reported yet is still owed. (The processor
        // publishes its count after every block, and its callback is stopped now.)
        let faults = self.live.snapshot().faults;
        if faults != self.seen_faults {
            self.seen_faults = faults;
            self.restart_owed = true;
        }
        processor.restart(self.sample_rate, graph);
        // The old graph, and every channel fading in it, is gone; the queue no longer
        // holds a removal (see `rebuilt_queue`).
        self.fades.clear();
        self.free_trash();
        Ok(())
    }

    fn try_build_graph(&mut self) -> Result<Box<Graph>, ControlError> {
        let mix = catch_unwind(AssertUnwindSafe(|| {
            self.prepare_replay_mix(self.sample_rate)
        }))
        .map_err(|_| ControlError::BuildFailed)??;
        self.try_replay_with_mix(mix)
    }

    fn try_replay_with_mix(
        &mut self,
        mix: Option<Box<crate::mixer::live::LiveMix>>,
    ) -> Result<Box<Graph>, ControlError> {
        self.graph_epoch += 1;
        catch_unwind(AssertUnwindSafe(|| self.replay_with_mix(mix))).map_err(|_| {
            self.graph_epoch -= 1;
            ControlError::BuildFailed
        })
    }

    pub(crate) fn build_graph(&mut self) -> Box<Graph> {
        self.try_build_graph().expect("the mirrored graph prepares")
    }

    fn prepare_replay_mix(
        &self,
        rate: f32,
    ) -> Result<Option<Box<crate::mixer::live::LiveMix>>, ControlError> {
        // A fresh graph has no fading channels, only the current channel instances.
        let aliases = aliases(&self.state.mix, &self.instances, &[])?;
        crate::mixer::live::prepare(&self.state.mix, rate, aliases, &self.mixer_memory)
    }

    fn replay_with_mix(&self, mix: Option<Box<crate::mixer::live::LiveMix>>) -> Box<Graph> {
        #[cfg(test)]
        if self.replay_panics.get() > 0 {
            self.replay_panics.set(self.replay_panics.get() - 1);
            panic!("replaying the controller's state panicked (test switch)");
        }
        let mut graph = Box::new(Graph::new(self.sample_rate, self.generation));
        let ToneState { on, hz, gain_db } = self.tone;
        graph.set_tone(on, hz, gain_db);
        for (index, &value) in (0u32..).zip(&self.synth) {
            graph.set_synth_param(index, value);
        }
        let any_solo = self.state.any_solo();
        let ramp_len = ramp_len(self.sample_rate);
        for setup in &self.state.channels {
            let node = Box::new(ChannelNode::new(
                self.sample_rate,
                ramp_len,
                setup,
                any_solo,
            ));
            let mut node = node;
            node.instance = self.instances.get(&setup.id).copied().unwrap_or(0);
            let refused = graph.add_channel(node);
            debug_assert!(refused.is_none(), "the state holds valid channels only");
        }
        graph.set_pattern(Box::new(self.state.pattern.clone()));
        graph.set_song(Box::new(self.state.song.clone()));
        graph.playback_mode = self.state.playback_mode;
        graph.song_loop = self.state.song_loop;
        graph.set_initial_mix(mix, self.mix_revision);
        graph.set_epoch(self.graph_epoch);
        // Every channel starts at its mix: nothing ramps in a fresh graph.
        graph.settle();
        graph
    }

    fn mirror(&mut self, cmd: Command) {
        match cmd {
            Command::SetPlaybackMode { mode } => {
                if mode != self.state.playback_mode {
                    self.recording_armed = false;
                }
                self.state.playback_mode = mode;
            }
            Command::SetRecording { on } => self.recording_armed = on,
            Command::SetSongLoop { region } => self.state.song_loop = region,
            Command::TestTone { on, hz, gain_db } => self.tone = ToneState { on, hz, gain_db },
            Command::AllSoundOff => self.tone.on = false,
            Command::SetTempo { bpm } => self.state.bpm = bpm,
            Command::SetTimeSignature { beats, unit } => {
                self.state.time_signature = (beats, unit);
            }
            Command::SetSynthParam { index, value } => {
                if let Some(param) = self.synth.get_mut(index as usize) {
                    *param = value;
                }
            }
            Command::SetChannelMix { channel, mix } => {
                if let Some(c) = self.state.channel_mut(channel) {
                    c.mix = mix;
                }
            }
            Command::SetSamplerTrim {
                channel, params, ..
            } => {
                if let Some(c) = self.state.channel_mut(channel) {
                    c.instrument.params_mut().copy_from_slice(&params);
                }
            }
            // `send_at` let these through for a channel whose instrument has the parameter.
            Command::SetSamplerParam {
                channel,
                index,
                value,
            }
            | Command::SetChannelParam {
                channel,
                index,
                value,
            } => {
                if let Some(param) = self
                    .state
                    .channel_mut(channel)
                    .and_then(|c| c.instrument.params_mut().get_mut(index as usize))
                {
                    *param = value;
                }
            }
            Command::SetChoke { channel, group } => {
                if let Some(c) = self.state.channel_mut(channel) {
                    c.choke = group;
                }
            }
            _ => {}
        }
    }

    /// Frees what the processor handed back (graphs, channels, samples, patterns). The output layer also calls it while it runs
    /// the processor itself, so a restart waiting for room in the trash queue can go through.
    /// Fade routes retire with their departed node, or with every graph built up to theirs.
    pub(crate) fn free_trash(&mut self) {
        while let Ok(item) = self.trash.pop() {
            match &item {
                // A refused add can share its instance with a node in a replacement graph.
                Trash::Channel(node) if node.leaving() => {
                    self.fades.retain(|f| f.instance != node.instance);
                }
                Trash::Graph(graph) => {
                    let epoch = graph.epoch();
                    self.fades.retain(|f| f.epoch > epoch);
                }
                _ => {}
            }
        }
    }

    fn reclaim_detached_trash(&mut self, processor: &mut AudioProcessor) {
        self.free_trash();
        processor.return_detached_trash();
        self.free_trash();
    }

    /// Every queued command has reached the processor.
    pub(crate) fn commands_drained(&self) -> bool {
        self.commands.slots() == QUEUE_CAPACITY
    }

    /// Test helper: queues `n` graph replacements, as `n` restarts would.
    #[cfg(test)]
    pub(crate) fn queue_restarts(&mut self, n: usize) {
        for _ in 0..n {
            let graph = self.build_graph();
            assert!(self.commands.push(Msg::ReplaceGraph(graph)).is_ok());
        }
    }
}

/// The engine-owned fade routes a mix prepared now must carry: one per present channel
/// instance the mix routes (in case it is removed while this mix runs) and one per recorded
/// fade, on its own track. A different output mode or a missing fading track is `Busy`:
/// refuse the edit until retirement rather than move or drop the fade.
fn aliases<'a>(
    mix: &'a crate::mixer::MixSetup,
    instances: &'a BTreeMap<ChannelId, u64>,
    fades: &'a [Fade],
) -> Result<impl Iterator<Item = (u64, crate::mixer::MixerTrackId)> + Clone + 'a, ControlError> {
    use crate::mixer::{MixSetup, SourceId};
    if fades.iter().any(|f| match mix {
        MixSetup::LegacyDirect => f.track.is_some(),
        MixSetup::Routed(_) | MixSetup::Controlled { .. } => {
            f.track.is_none_or(|track| !mix.has_track(track))
        }
    }) {
        return Err(ControlError::Busy);
    }
    let fading = fades.iter().filter_map(|f| Some((f.instance, f.track?)));
    let current = instances.iter().filter_map(move |(id, &instance)| {
        let track = mix.destination(SourceId::Channel(u64::from(id.0)))?;
        Some((instance, track))
    });
    Ok(fading.chain(current))
}

#[cfg(test)]
pub(crate) fn restricted_rate_test_mix() -> crate::mixer::MixSetup {
    use crate::mixer::*;

    let insert = InsertId(1);
    MixSetup::Routed(RoutedMix {
        mixer: Mixer {
            tracks: vec![Track {
                id: MASTER,
                output: None,
                inserts: vec![describe_builtin_insert(insert, "bitcrusher").unwrap()],
            }],
            sources: vec![Source {
                id: SourceId::System(SystemSource::Tone),
                destination: MASTER,
            }],
            ..Mixer::default()
        },
        gains: vec![],
        devices: vec![InsertDevice {
            insert,
            kind: "bitcrusher".into(),
            seed: 1,
            params: vec![],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::NoteId;
    use crate::engine;

    fn controller() -> (Controller, AudioProcessor) {
        engine(EngineConfig {
            sample_rate: 48_000.0,
        })
    }

    #[test]
    fn a_compound_removal_needing_two_slots_cannot_take_the_last_one() {
        let setup = |id| ChannelSetup::synth(ChannelId(id), vec![], Default::default());
        let (mut c, mut p) = crate::engine_with(
            EngineConfig {
                sample_rate: 48_000.0,
            },
            EngineState {
                channels: (0..MAX_CHANNELS as u32).map(setup).collect(),
                ..EngineState::default()
            },
        );
        let first = MAX_CHANNELS as u32;
        let second = first + 1;
        let revision = c
            .set_sources_and_mix(SourceEdit {
                remove: (1..first).map(ChannelId).collect(),
                add: vec![setup(first), setup(second)],
                timeline: None,
                mix: Default::default(),
            })
            .unwrap();
        let mut block = [0.0; crate::BLOCK * 2];
        p.process(&mut block, 2);
        assert!(c.commands_drained());

        let timeline = crate::TimelineUpdate {
            pattern: Box::new(PatternData {
                length: 480,
                notes: vec![],
            }),
            song: Box::default(),
            bpm: 110.0,
            mode: crate::PlaybackMode::Song,
            region: None,
            remove_channel: None,
        };
        let before = c.state().clone();
        let instances = c.instances.clone();
        let next_instance = c.next_instance;
        assert_eq!(
            c.set_sources_and_mix(SourceEdit {
                remove: vec![ChannelId(first), ChannelId(second)],
                add: vec![setup(first)],
                timeline: Some(timeline.clone()),
                mix: Default::default(),
            }),
            Err(ControlError::Busy)
        );
        assert!(c.commands_drained(), "refusal consumes no command slot");
        assert_eq!(c.mix_revision(), revision);
        assert_eq!(c.state(), &before);
        assert_eq!(c.instances, instances);
        assert_eq!(c.next_instance, next_instance);

        assert_eq!(
            c.set_sources_and_mix(SourceEdit {
                remove: vec![ChannelId(first)],
                add: vec![setup(first)],
                timeline: Some(timeline),
                mix: Default::default(),
            }),
            Ok(revision + 1)
        );
        assert_eq!(c.state().pattern.length, 480);
        assert_eq!(
            c.channel_instance(ChannelId(first)),
            Some(next_instance + 1)
        );
        assert_eq!(
            c.channel_instance(ChannelId(second)),
            instances.get(&ChannelId(second)).copied()
        );
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn a_refused_add_does_not_retire_the_replacement_graphs_fade() {
        let (mut c, mut p) = controller();
        let mut block = [0.0; crate::BLOCK * 2];
        c.send(Command::InjectPanic).unwrap();
        p.process(&mut block, 2);

        let id = ChannelId(1);
        c.add_channel(ChannelSetup::synth(id, vec![], Default::default()))
            .unwrap();
        let instance = c.channel_instance(id).unwrap();
        assert!(
            c.check_health()
                .contains(&HealthEvent::Restarted { faults: 1 })
        );
        c.send(Command::ChannelNote {
            channel: id,
            key: 60,
            velocity: 1.0,
            on: true,
            id: NoteId(1),
        })
        .unwrap();
        c.remove_channel(id).unwrap();

        // The old graph refuses the add. The new graph already contains the same instance
        // from the mirror, sounds the queued note, and starts its fade.
        p.process(&mut block, 2);
        assert!(block.iter().any(|x| x.abs() > 1e-6));
        assert!(matches!(c.trash.peek(), Ok(Trash::Channel(n)) if n.instance == instance));
        c.free_trash();
        assert_eq!(
            c.fades.iter().map(|f| f.instance).collect::<Vec<_>>(),
            [instance],
            "a refused add is not a completed fade"
        );
        for _ in 0..3 {
            p.process(&mut block, 2);
        }
        c.free_trash();
        assert!(c.fades.is_empty());
    }

    /// Publishes what the processor would after `faults` panics and `guard_hits` NaN blocks
    /// (the processor itself is never run, so nothing overwrites it).
    fn pretend(c: &Controller, faults: u32, guard_hits: u32) {
        c.live.publish([0.0; 2], false, 0, faults, guard_hits);
        c.live.publish_recovery(faults, 0);
    }

    #[test]
    fn reconfigure_rejects_insert_rate_without_disturbing_playback_or_the_queue() {
        let config = |sample_rate| EngineConfig { sample_rate };
        let (mut c, mut p) = crate::engine_with(
            config(48_000.0),
            EngineState {
                mix: restricted_rate_test_mix(),
                ..EngineState::default()
            },
        );
        c.send(Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        })
        .unwrap();
        let mut out = [0.0; crate::BLOCK * 2];
        p.process(&mut out, 2);
        c.send(Command::Play).unwrap();
        let before = (
            c.state().clone(),
            c.generation,
            c.graph_epoch,
            c.commands.slots(),
            c.live(),
        );

        let reconfigured = c.try_reconfigure(&mut p, config(16_000.0));
        assert!(
            c.check_health().is_empty(),
            "an unsupported rate is not a fault"
        );
        let error = reconfigured.unwrap_err();
        assert!(matches!(&error, ReconfigureError::InsertRate(_)));
        assert_eq!(
            error.to_string(),
            "Mixer insert bitcrusher (22050–192000 Hz) cannot run at 16000 Hz"
        );
        assert_eq!(c.sample_rate(), 48_000.0);
        assert_eq!(
            (
                c.state().clone(),
                c.generation,
                c.graph_epoch,
                c.commands.slots(),
                c.live()
            ),
            before
        );

        p.process(&mut out, 2);
        assert!(c.live().playing, "the queued Play command was retained");
        assert!(out.iter().any(|&sample| sample != 0.0));
        assert!(c.reconfigure(&mut p, config(44_100.0)));
        assert_eq!(c.sample_rate(), 44_100.0);
        assert_eq!(c.state(), &before.0);
        assert!(c.check_health().is_empty());
    }

    #[test]
    fn instance_storage_is_bounded_by_the_live_channels() {
        let (mut c, mut p) = controller();
        for id in 1000..2000 {
            c.free_trash();
            c.add_channel(setup(id)).unwrap();
            p.process(&mut [0.0; 512], 2);
            c.remove_channel(ChannelId(id)).unwrap();
            p.process(&mut [0.0; 512], 2);
        }
        assert!(
            c.instances.len() <= MAX_CHANNELS,
            "{} dead instance entries kept",
            c.instances.len()
        );
    }

    #[test]
    fn a_channel_is_a_new_instance_each_time_its_id_is_added() {
        let (mut c, _p) = controller();
        assert_eq!(c.channel_instance(ChannelId(1)), None);
        c.add_channel(setup(1)).unwrap();
        c.add_channel(setup(2)).unwrap();
        let (first, other) = (
            c.channel_instance(ChannelId(1)).unwrap(),
            c.channel_instance(ChannelId(2)).unwrap(),
        );
        assert_ne!(first, other);
        // Settings and notes do not make a channel another one.
        c.send(Command::SetChannelMix {
            channel: ChannelId(1),
            mix: crate::ChannelMix::default(),
        })
        .unwrap();
        assert_eq!(c.channel_instance(ChannelId(1)), Some(first));
        // Removed and added again under the same id: another channel.
        c.remove_channel(ChannelId(1)).unwrap();
        assert_eq!(c.channel_instance(ChannelId(1)), None);
        c.add_channel(setup(1)).unwrap();
        let again = c.channel_instance(ChannelId(1)).unwrap();
        assert!(again != first && again != other);
        assert_eq!(c.channel_instance(ChannelId(2)), Some(other));
        // A new state makes every channel new, even one with the same id.
        let state = EngineState {
            channels: vec![setup(1)],
            ..EngineState::default()
        };
        c.load_state(state).unwrap();
        let replaced = c.channel_instance(ChannelId(1)).unwrap();
        assert!(replaced != again && replaced != first && replaced != other);
        assert_eq!(c.channel_instance(ChannelId(2)), None);
    }

    #[test]
    fn channel_params_are_sent_all_or_none() {
        let (mut c, _p) = controller();
        let id = ChannelId(1);
        c.add_channel(ChannelSetup::synth(
            id,
            Vec::new(),
            crate::ChannelMix::default(),
        ))
        .unwrap();
        let params: Vec<(u32, f32)> = (0..40).map(|i| (i, -1e9)).collect();
        c.set_channel_params(id, &params).unwrap();
        let spec = InstrumentKind::Synth.spec();
        let mins: Vec<f32> = spec.iter().map(|&(min, _, _)| min).collect();
        assert_eq!(
            c.state().channels[0].instrument.params(),
            mins.as_slice(),
            "clamped"
        );

        let before = c.state().clone();
        let free = c.commands.slots();
        for bad in [vec![(0, 1.0), (40, 1.0)], vec![(0, 1.0), (1, f32::NAN)]] {
            assert_eq!(c.set_channel_params(id, &bad), Err(ControlError::Rejected));
        }
        assert_eq!(
            c.set_channel_params(ChannelId(9), &[(0, 1.0)]),
            Err(ControlError::UnknownChannel)
        );
        // Fill the queue until 39 slots are left: 40 values do not fit, so none is sent.
        while c.commands.slots() > 39 {
            c.send(Command::SetTempo { bpm: 120.0 }).unwrap();
        }
        let tempo = c.state().bpm;
        assert_eq!(c.set_channel_params(id, &params), Err(ControlError::Full));
        assert_eq!(c.commands.slots(), 39);
        assert_eq!(c.state().channels, before.channels);
        assert_eq!(tempo, 120.0);
        assert!(free > 39);
    }

    #[cfg(feature = "test-hooks")]
    fn fault(c: &mut Controller, p: &mut AudioProcessor) {
        c.send(Command::InjectPanic).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
    }

    #[test]
    #[cfg(feature = "test-hooks")]
    fn exhausted_recovery_stops_and_disarms_after_full_queue() {
        let (mut c, mut p) = controller();
        c.send(Command::Play).unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        c.attempts = 3;
        fault(&mut c, &mut p);
        while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
        assert_eq!(
            c.check_health_at(Instant::now()),
            [HealthEvent::Stopped { faults: 1 }]
        );
        assert!(c.stop_owed);
        assert!(c.check_health().is_empty());
        c.replay_panics.set(1);
        assert!(!c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 96_000.0
            }
        ));
        assert!(!c.stop_owed, "the stopped callback settled the owed stop");
        assert!(c.check_health().is_empty());
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        c.check_health();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert!(!c.live().playing);
        assert!(!c.recording_armed());
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert!(events.iter().any(|e| matches!(e, RecordEvent::End { .. })));
        assert!(events.iter().any(|e| matches!(e, RecordEvent::Mark { .. })));
        assert!(c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 48_000.0
            }
        ));
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert!(!c.live().playing);
    }

    #[test]
    #[cfg(feature = "test-hooks")]
    fn explicit_recovery_settles_full_queue_before_old_play_commands() {
        let (mut c, mut p) = controller();
        c.send(Command::Play).unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        c.attempts = 3;
        fault(&mut c, &mut p);
        while c.send(Command::PlayLoops { loops: 2 }).is_ok() {}
        assert_eq!(c.check_health().len(), 1);
        assert!(c.stop_owed);
        assert!(c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 48_000.0
            }
        ));
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert!(!c.live().playing);
        assert!(!c.recording_armed());
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert!(events.iter().any(|e| matches!(e, RecordEvent::End { .. })));
        assert!(events.iter().any(|e| matches!(e, RecordEvent::Mark { .. })));
    }

    #[test]
    #[cfg(feature = "test-hooks")]
    fn recovery_ceiling_backoff_and_explicit_load() {
        let (mut c, mut p) = controller();
        let start = Instant::now();
        for (attempt, ms) in [0, 250, 750].into_iter().enumerate() {
            fault(&mut c, &mut p);
            let at = start + Duration::from_millis(ms);
            if attempt > 0 {
                let slots = c.commands.slots();
                assert!(c.check_health_at(at - Duration::from_millis(1)).is_empty());
                assert_eq!(c.commands.slots(), slots);
            }
            assert_eq!(
                c.check_health_at(at),
                [HealthEvent::Restarted {
                    faults: attempt as u32 + 1
                }]
            );
            p.process(&mut [0.0; crate::BLOCK * 2], 2);
        }
        fault(&mut c, &mut p);
        assert_eq!(
            c.check_health_at(start + Duration::from_millis(1750)),
            [HealthEvent::Stopped { faults: 4 }]
        );
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        let slots = c.commands.slots();
        for seconds in [3, 60, 86400] {
            assert!(
                c.check_health_at(start + Duration::from_secs(seconds))
                    .is_empty()
            );
            assert_eq!(c.commands.slots(), slots);
            let mut out = [1.0; crate::BLOCK * 2];
            p.process(&mut out, 2);
            assert_eq!(out, [0.0; crate::BLOCK * 2]);
        }
        c.load_state(c.state.clone()).unwrap();
        assert_eq!(c.check_health_at(start), [HealthEvent::RecoveryReset]);
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        fault(&mut c, &mut p);
        assert_eq!(
            c.check_health_at(start),
            [HealthEvent::Restarted { faults: 5 }]
        );
    }

    #[test]
    #[cfg(feature = "test-hooks")]
    fn stable_processing_renews_budget_but_wall_time_and_silent_callbacks_do_not() {
        let (mut c, mut p) = controller();
        let now = Instant::now();
        fault(&mut c, &mut p);
        assert_eq!(
            c.check_health_at(now),
            [HealthEvent::Restarted { faults: 1 }]
        );
        assert!(c.check_health_at(now + Duration::from_secs(100)).is_empty());
        assert_eq!(c.attempts, 1);
        // Exactly two seconds of real graph renders at 48 kHz.
        p.process(&mut vec![0.0; (96_000 - crate::BLOCK) * 2], 2);
        assert!(c.check_health_at(now).is_empty());
        assert_eq!(c.attempts, 1);
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert_eq!(c.check_health_at(now), [HealthEvent::RecoveryReset]);
        assert_eq!(c.attempts, 0);
        assert!(c.check_health_at(now).is_empty());
        fault(&mut c, &mut p);
        assert_eq!(
            c.check_health_at(now),
            [HealthEvent::Restarted { faults: 2 }]
        );
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        fault(&mut c, &mut p);
        p.process(&mut vec![0.0; 96_000 * 2], 2);
        assert!(c.check_health_at(now).is_empty());
        assert_eq!(c.attempts, 1);
    }

    #[test]
    #[cfg(feature = "test-hooks")]
    fn constructor_panics_are_bounded_and_explicit_reconfigure_can_recover() {
        let (mut c, mut p) = controller();
        c.add_channel(setup(1)).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        let state = c.state.clone();
        let instances = c.instances.clone();
        fault(&mut c, &mut p);
        c.replay_panics.set(u32::MAX);
        let now = Instant::now();
        for ms in [0, 250, 750] {
            assert_eq!(
                c.check_health_at(now + Duration::from_millis(ms)),
                [HealthEvent::RecoveryFailed { faults: 1 }]
            );
        }
        assert_eq!(
            c.check_health_at(now + Duration::from_millis(1750)),
            [HealthEvent::Stopped { faults: 1 }]
        );
        assert_eq!(c.state, state);
        assert_eq!(c.instances, instances);
        c.send(Command::Stop).unwrap(); // Unrelated command after the recovery barrier.
        let queued = c.commands.slots();
        assert_eq!(
            c.load_state(EngineState::default()),
            Err(ControlError::BuildFailed)
        );
        assert_eq!(c.state, state);
        assert_eq!(c.instances, instances);
        assert_eq!(c.commands.slots(), queued);
        let generation = c.generation;
        c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 44_100.0,
            },
        );
        assert_eq!(c.sample_rate, 48_000.0);
        assert_eq!(c.generation, generation);
        assert_eq!(c.state, state);
        // Only the admitted Stop/disarm/completion-tag prefix was consumed.
        assert_eq!(c.commands.slots(), queued + 3);
        assert_eq!(c.pending_recovery_stop, None);
        c.replay_panics.set(0);
        c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 44_100.0,
            },
        );
        assert_eq!(
            c.check_health_at(now),
            [
                HealthEvent::RecoveryReset,
                HealthEvent::Restarted { faults: 2 }
            ]
        );
        assert_eq!(c.instances, instances);
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        fault(&mut c, &mut p);
        assert_eq!(
            c.check_health_at(now),
            [HealthEvent::Restarted { faults: 3 }]
        );
    }

    #[test]
    fn failed_recovery_preserves_project_and_channel_identity() {
        let (mut c, mut p) = controller();
        c.add_channel(setup(1)).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        let before = c.state.clone();
        let instances = c.instances.clone();
        c.replay_panics.set(u32::MAX);
        pretend(&c, 1, 0);
        c.check_health();
        assert_eq!(c.state, before);
        assert_eq!(c.instances, instances);
    }

    #[test]
    fn each_health_event_is_reported_once_with_one_restart_per_fault() {
        let (mut c, _p) = controller();
        let free = c.commands.slots();
        pretend(&c, 1, 2);
        assert_eq!(
            c.check_health(),
            [
                HealthEvent::Restarted { faults: 1 },
                HealthEvent::DeviceSilenced { total: 2 }
            ]
        );
        assert_eq!(c.commands.slots(), free - 1, "one replacement queued");
        assert!(c.check_health().is_empty());
        assert_eq!(c.commands.slots(), free - 1, "and no second one");
    }

    #[test]
    fn reconfigure_makes_restarts_queued_before_it_stale() {
        let (mut c, mut p) = controller();
        pretend(&c, 1, 0);
        // Queues a replacement built with the tone off.
        assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
        let tone = Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        };
        c.send(tone).unwrap();
        // Same rate: only the queue's age tells the queued graph is stale.
        c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 48_000.0,
            },
        );
        // Reconfigure freed the queued graph (and the tone command, already in its state).
        assert!(c.commands_drained());
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        // The one it built (tone on) plays, and nothing came back to free.
        assert!(c.live().tone);
        assert!(c.trash.pop().is_err());
    }

    fn setup(id: u32) -> ChannelSetup {
        ChannelSetup::sampler(
            ChannelId(id),
            None,
            Vec::new(),
            crate::ChannelMix::default(),
        )
    }

    #[test]
    fn refused_channel_calls_queue_and_change_nothing() {
        let (mut c, _p) = controller();
        c.add_channel(setup(1)).unwrap();
        let free = c.commands.slots();
        let before = c.state.clone();
        let note = Command::ChannelNote {
            channel: ChannelId(2),
            key: 60,
            velocity: 1.0,
            on: true,
            id: NoteId(60),
        };
        assert_eq!(c.send_at(0, note), Err(ControlError::UnknownChannel));
        assert_eq!(
            c.remove_channel(ChannelId(2)),
            Err(ControlError::UnknownChannel)
        );
        assert_eq!(
            c.set_channel_sample(ChannelId(2), None),
            Err(ControlError::UnknownChannel)
        );
        assert_eq!(c.add_channel(setup(1)), Err(ControlError::ChannelExists));
        let nan = ChannelSetup::sampler(
            ChannelId(2),
            None,
            vec![f32::NAN],
            crate::ChannelMix::default(),
        );
        assert_eq!(c.add_channel(nan), Err(ControlError::Rejected));
        let empty = PatternData {
            length: 0,
            notes: Vec::new(),
        };
        assert_eq!(c.set_pattern(empty), Err(ControlError::Rejected));
        let dup = EngineState {
            channels: vec![setup(3), setup(3)],
            ..EngineState::default()
        };
        assert_eq!(c.load_state(dup), Err(ControlError::ChannelExists));
        assert_eq!(c.commands.slots(), free, "nothing queued");
        assert_eq!(c.state, before, "nothing changed");

        // A full queue: every change is refused whole.
        while c.commands.slots() > 2 {
            c.send(Command::Play).unwrap();
        }
        assert_eq!(
            c.load_state(EngineState::default()),
            Err(ControlError::Full)
        );
        c.send(Command::Play).unwrap();
        c.send(Command::Play).unwrap();
        assert_eq!(c.add_channel(setup(2)), Err(ControlError::Full));
        assert_eq!(c.remove_channel(ChannelId(1)), Err(ControlError::Full));
        assert_eq!(
            c.set_pattern(PatternData::default()),
            Err(ControlError::Full)
        );
        assert_eq!(c.state, before, "nothing changed");
    }

    #[test]
    fn song_load_construction_failure_keeps_recording_and_song_state_transactional() {
        let (mut c, mut p) = controller();
        c.add_channel(setup(1)).unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        let before = c.state.clone();
        let instances = c.instances.clone();
        let mark = c.record_mark;
        let slots = c.commands.slots();
        let mut song = before.clone();
        song.playback_mode = crate::PlaybackMode::Song;
        song.song.end_tick = 960;
        song.song.arrangement.length = 960;
        song.song.arrangement.notes = vec![crate::SongNote {
            tick: 0,
            length: 120,
            channel: ChannelId(1),
            key: 60,
            velocity: 0.8,
            slide: false,
            clip_id: 1,
        }];
        c.fail_replays(1);
        assert_eq!(c.load_state(song.clone()), Err(ControlError::BuildFailed));
        assert_eq!(c.state, before);
        assert_eq!(c.instances, instances);
        assert_eq!(c.record_mark, mark);
        assert_eq!(c.commands.slots(), slots);
        assert!(c.recording_armed());

        c.load_state(song.clone()).unwrap();
        assert!(!c.recording_armed());
        assert_eq!(c.state, song);
        let graph = c.build_graph();
        assert_eq!(graph.song(), &song.song);
        assert_eq!(graph.playback_mode, crate::PlaybackMode::Song);
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert_eq!(c.state.song, song.song);
        assert!(!c.live().playing);
        let slots = c.commands.slots();
        let instances = c.instances.clone();
        c.fail_replays(1);
        assert_eq!(
            c.load_state(EngineState::default()),
            Err(ControlError::BuildFailed)
        );
        assert_eq!(c.state, song);
        assert_eq!(c.instances, instances);
        assert_eq!(c.commands.slots(), slots);
        assert!(!c.recording_armed());
    }

    #[test]
    fn an_unknown_preset_or_a_full_queue_sends_nothing() {
        let (mut c, _p) = controller();
        let free = c.commands.slots();
        assert!(!c.synth_preset("nope"));
        assert_eq!(c.commands.slots(), free);
        // Room for 39 commands, one short of a preset: all or nothing.
        while c.commands.slots() > 39 {
            c.send(Command::Play).unwrap();
        }
        let before = c.synth.clone();
        assert!(!c.synth_preset("Reese"));
        assert_eq!(c.commands.slots(), 39);
        assert_eq!(c.synth, before);
    }
    #[test]
    #[cfg(feature = "test-hooks")]
    fn direct_recovery_stop_publishes_before_failed_reconstruction() {
        let (mut c, mut p) = controller();
        c.send(Command::Play).unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        c.attempts = 3;
        fault(&mut c, &mut p);
        while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
        assert_eq!(c.check_health().len(), 1);
        assert!(c.stop_owed);
        c.replay_panics.set(1);
        assert!(!c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 96_000.0
            }
        ));
        assert!(!c.recording_armed());
        assert!(c.recovery_stop_mark().is_some());
        // A failed output-rate rebuild keeps the callback detached indefinitely.
        assert!(
            !c.live().playing,
            "R5: directly settled stop must publish stopped transport without another callback"
        );
    }
}

#[cfg(test)]
mod sampler_trim_tests {
    use super::*;
    #[test]
    fn pair_publication_uses_exactly_one_queue_slot() {
        let (mut c, _p) = crate::engine(crate::EngineConfig {
            sample_rate: 48000.0,
        });
        let setup = crate::ChannelSetup::sampler(
            ChannelId(1),
            None,
            Vec::new(),
            crate::ChannelMix::default(),
        );
        c.add_channel(setup).unwrap();
        let instance = c.channel_instance(ChannelId(1)).unwrap();
        let before = c.commands.slots();
        let mut params = grooviq_dsp::sampler::SPEC.map(|(_, _, d)| d);
        params[3] = 0.2;
        params[4] = 0.7;
        c.send_at(
            0,
            Command::SetSamplerTrim {
                channel: ChannelId(1),
                instance,
                params,
            },
        )
        .unwrap();
        assert_eq!(
            c.commands.slots(),
            before - 1,
            "a multi-message prefix must never become callback-visible"
        );
    }
}

#[cfg(all(test, feature = "test-hooks"))]
mod admitted_stop_red_v2 {
    use super::*;
    use crate::{
        AudioOutput, AudioSlot, BLOCK, ChannelId, ChannelMix, ChannelSetup, EngineConfig,
        HealthEvent, LiveSnapshot, NoteId, OutputBackend, OutputChoice, OutputInfo, PPQ,
        RecordEvent, engine,
    };
    use std::sync::{Arc, Mutex};

    struct NoOutput;
    impl OutputBackend for NoOutput {
        fn outputs(&self) -> Vec<OutputInfo> {
            Vec::new()
        }
        fn open(&mut self, _: &OutputInfo, _: Arc<AudioSlot>) -> Result<u32, String> {
            panic!("this fake NoOutput must never open a device")
        }
        fn close(&mut self) {}
    }

    fn admitted_failed_rebuild() -> (Controller, AudioProcessor, u32, u64) {
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        c.send(Command::SetTempo { bpm: 120.0 }).unwrap();
        c.add_channel(ChannelSetup::synth(
            ChannelId(1),
            Vec::new(),
            ChannelMix::default(),
        ))
        .unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        let id = NoteId::live();
        c.send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 76,
            velocity: 0.8,
            on: true,
            id,
        })
        .unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        let mut accepted = Vec::new();
        c.take_recorded(&mut accepted);
        assert!(
            matches!(accepted.as_slice(), [RecordEvent::Note {
                key: 76, on: true, id: got, tick: 2, position: 2, take: 1, ..
            }] if *got == id),
            "the original held note must have been positively recorded"
        );
        c.attempts = 3;
        c.send(Command::InjectPanic).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert_eq!(c.live().frames, (3 * BLOCK) as u64);
        assert!(
            c.commands.slots() >= 3,
            "exercise available command FIFO, not the old full-FIFO branch"
        );
        assert_eq!(c.check_health(), [HealthEvent::Stopped { faults: 1 }]);
        assert!(!c.stop_owed, "the exhaustion transaction has been ADMITTED");
        let mark = c.recovery_stop_mark().expect("original exhaustion mark");
        assert_eq!(c.record_mark(), mark);
        assert!(!c.recording_armed());
        c.fail_replays(1);
        assert!(
            !c.reconfigure(
                &mut p,
                EngineConfig {
                    sample_rate: 96_000.0
                }
            ),
            "this is an intentionally refused build, not recovery success"
        );
        // 48kHz,120BPM, zero latency, no seek/resize/tempo change: original stop at frame192.
        let end_tick = (3 * BLOCK) as u64 * 120 * u64::from(PPQ) / (60 * 48_000);
        (c, p, mark, end_tick)
    }

    #[test]
    fn admitted_stop_retry_keeps_original_identity_and_later_play() {
        let (mut c, mut p, mark, tick) = admitted_failed_rebuild();
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert_eq!(
            events,
            vec![
                RecordEvent::End {
                    tick,
                    position: tick as u32
                },
                RecordEvent::Mark { id: mark }
            ]
        );
        c.send(Command::Play).unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        c.fail_replays(2);
        for _ in 0..2 {
            assert!(!c.reconfigure(
                &mut p,
                EngineConfig {
                    sample_rate: 96_000.0
                }
            ));
            assert_eq!(c.record_mark(), mark);
            c.settle_recovery_stop(&mut p);
        }
        assert!(c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 48_000.0
            }
        ));
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert!(
            c.live().playing,
            "explicitly accepted later Play survives retries"
        );
        assert!(c.recording_armed());
        events.clear();
        c.take_recorded(&mut events);
        assert!(
            events.is_empty(),
            "the original End and Mark are not repeated"
        );
    }

    #[test]
    fn callback_completed_stop_does_not_stop_later_play_on_reconfigure() {
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        c.attempts = 3;
        c.send(Command::InjectPanic).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert_eq!(c.check_health().len(), 1);
        let mark = c.recovery_stop_mark().unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert!(!c.live().playing);
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert!(c.live().playing);
        assert!(c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 96_000.0
            }
        ));
        assert!(
            c.live().playing,
            "completed identity must not trigger another Stop"
        );
        assert_eq!(c.record_mark(), mark);
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert_eq!(events, vec![RecordEvent::Mark { id: mark }]);
    }

    #[test]
    fn detached_stop_preserves_full_record_fifo_end_before_original_mark() {
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        c.send(Command::SetTempo { bpm: 120.0 }).unwrap();
        c.add_channel(ChannelSetup::synth(
            ChannelId(1),
            Vec::new(),
            ChannelMix::default(),
        ))
        .unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        for _ in 0..crate::record::RECORD_CAPACITY {
            c.send(Command::ChannelNote {
                channel: ChannelId(1),
                key: 60,
                velocity: 0.8,
                on: true,
                id: NoteId::live(),
            })
            .unwrap();
            p.process(&mut [0.0; BLOCK * 2], 2);
        }
        assert_eq!(c.recorded.slots(), crate::record::RECORD_CAPACITY);
        c.attempts = 3;
        c.send(Command::InjectPanic).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert_eq!(c.check_health().len(), 1);
        let mark = c.recovery_stop_mark().unwrap();
        let frames = c.live().frames;
        let tick = frames * 120 * u64::from(PPQ) / (60 * 48_000);
        c.settle_recovery_stop(&mut p);
        assert!(!c.live().playing);
        let mut prefix = Vec::new();
        c.take_recorded(&mut prefix);
        assert_eq!(prefix.len(), crate::record::RECORD_CAPACITY);
        assert!(prefix.iter().all(|e| matches!(e, RecordEvent::Note { .. })));
        c.settle_recovery_stop(&mut p);
        c.settle_recovery_stop(&mut p);
        let mut ending = Vec::new();
        c.take_recorded(&mut ending);
        assert_eq!(
            ending,
            vec![
                RecordEvent::End {
                    tick,
                    position: tick as u32
                },
                RecordEvent::Mark { id: mark }
            ]
        );
        assert_eq!(c.live().frames, frames);
    }

    #[test]
    fn detached_prefix_keeps_barrier_mode_tempo_seek_chronology() {
        let (mut c, mut p) = crate::engine_with(
            EngineConfig {
                sample_rate: 48_000.0,
            },
            EngineState {
                bpm: 120.0,
                channels: vec![ChannelSetup::synth(
                    ChannelId(1),
                    vec![],
                    ChannelMix::default(),
                )],
                song: crate::SongData {
                    end_tick: 960,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        c.send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 76,
            velocity: 0.8,
            on: true,
            id: NoteId(7001),
        })
        .unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert!(matches!(
            events.as_slice(),
            [RecordEvent::Note {
                id: NoteId(7001),
                ..
            }]
        ));
        c.attempts = 3;
        c.send(Command::InjectPanic).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        let before = c.send_marked(Command::SetTempo { bpm: 60.0 }).unwrap();
        c.set_timeline(crate::TimelineUpdate {
            pattern: Box::new(c.state.pattern.clone()),
            song: Box::new(c.state.song.clone()),
            bpm: 90.0,
            mode: crate::PlaybackMode::Song,
            region: None,
            remove_channel: None,
        })
        .unwrap();
        c.send(Command::Seek { tick: 96 }).unwrap();
        c.check_health();
        let mark = c.recovery_stop_mark().unwrap();
        c.fail_replays(1);
        assert!(!c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 96_000.0
            }
        ));
        events.clear();
        c.take_recorded(&mut events);
        assert_eq!(
            events,
            vec![
                RecordEvent::Mark { id: before },
                RecordEvent::End {
                    tick: 7,
                    position: 7
                },
                RecordEvent::Mark { id: mark }
            ],
            "mode transition ends original take BEFORE later seek, without a callback"
        );
        assert!(!c.live().playing);
        assert!(!c.recording_armed());
        assert_eq!(c.live().frames, (3 * BLOCK) as u64);
    }

    fn detached_facts() -> (LiveSnapshot, bool, Vec<RecordEvent>, u32, u64) {
        let (c, p, mark, tick) = admitted_failed_rebuild();
        let shared = Arc::new(Mutex::new(c));
        let output = AudioOutput::start(
            Box::new(NoOutput),
            Arc::clone(&shared),
            p,
            OutputChoice::SystemDefault,
            false,
            Box::new(|_| {}),
        );
        output.poll_now();
        output.poll_now();
        let facts = {
            let mut c = shared.lock().unwrap();
            let mut events = Vec::new();
            c.take_recorded(&mut events);
            (c.live(), c.recording_armed(), events, mark, tick)
        };
        // Join the actual coordinator poller BEFORE any behavioral assertion can unwind.
        drop(output);
        facts
    }

    #[test]
    fn admitted_failed_rebuild_stops_without_callback() {
        let (live, armed, _, _, _) = detached_facts();
        assert!(
            !live.playing,
            "R6 admitted Stop must COMPLETE without another callback"
        );
        assert!(!armed);
        assert_eq!(
            live.frames,
            (3 * BLOCK) as u64,
            "no synthetic callback or clock advancement"
        );
    }

    #[test]
    fn admitted_failed_rebuild_ends_original_take_without_callback() {
        let (_, armed, events, mark, tick) = detached_facts();
        // Deliberately no playing assertion here: it must not mask the independent End witness.
        assert_eq!(
            events,
            vec![
                RecordEvent::End {
                    tick,
                    position: tick as u32
                },
                RecordEvent::Mark { id: mark }
            ],
            "R6 original take End must precede its ORIGINAL Mark exactly once"
        );
        assert!(!armed);
    }
}
