//! The control-thread half: validates commands, mirrors the state they set, builds channels,
//! frees what the processor hands back and rebuilds the graph after a panic. May allocate and
//! block; never call it from the audio thread.

use crate::channel::ChannelNode;
use crate::command::{Command, Msg, Trash, rebuilt_queue};
use crate::graph::{Graph, ramp_len};
use crate::live::{LiveShared, LiveSnapshot};
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
        })
    }
}

impl std::error::Error for ControlError {}

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
    /// The state the queued commands lead to, to rebuild the graph from.
    tone: ToneState,
    synth: Vec<f32>,
    state: EngineState,
    /// Which channel each id is now, see [`Self::channel_instance`].
    instances: BTreeMap<ChannelId, u64>,
    next_instance: u64,
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
    reset_owed: bool,
    /// Test switch: this many replays of the state panic, as a device bug in `set_param` would.
    #[cfg(test)]
    replay_panics: std::cell::Cell<u32>,
}

impl Controller {
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
            tone: ToneState::default(),
            synth: synth_defaults(),
            state,
            instances: BTreeMap::new(),
            next_instance: 0,
            generation: 0,
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

    /// Removes a channel, cutting its notes. The processor hands it back to be freed here.
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
        self.push(Msg::RemoveChannel(id))?;
        self.state.channels.remove(index);
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
        // The old state (and the samples only it held) is freed here, on this thread.
        let old = std::mem::replace(&mut self.state, state);
        let graph = match self.try_build_graph() {
            Some(graph) => graph,
            None => {
                self.state = old;
                return Err(ControlError::BuildFailed);
            }
        };
        if self.state.playback_mode == crate::PlaybackMode::Song {
            self.recording_armed = false;
        }

        self.renew_instances();
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
    pub(crate) fn sample_rate(&self) -> f32 {
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
            let queued = self.commands.push(Msg::Command { due: 0, cmd });
            debug_assert!(queued.is_ok());
        }
        let queued = self.commands.push(Msg::RecordMark(self.record_mark));
        debug_assert!(queued.is_ok());
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
                self.attempts += 1;
                // Immediate first attempt; then 250, 500 and 1000 ms before another decision.
                self.retry_at = Some(now + Duration::from_millis(250 << (self.attempts - 1)));
                // The processor stays silent until it gets a fresh graph. If the queue is full,
                // the next call tries again.
                if let Some(graph) = self.try_build_graph() {
                    if self.commands.push(Msg::ReplaceGraph(graph)).is_ok() {
                        self.seen_faults = faults;
                        self.restart_owed = true;
                    }
                } else {
                    events.push(HealthEvent::RecoveryFailed { faults });
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

    /// Switches the engine to a new sample rate, rebuilding the graph from the controller's
    /// state (which also recovers a graph that panicked; the next `check_health` still reports
    /// the panic). Call it with the audio callback stopped (it needs the processor itself),
    /// never on the audio thread. Returns false if construction failed; the processor stays
    /// silent and the previous rate, state and queued commands are retained.
    pub fn reconfigure(&mut self, processor: &mut AudioProcessor, config: EngineConfig) -> bool {
        // The consumer may have drained while the callback was detached. A retry must
        // deliver retained take boundaries even if constructing the graph fails again.
        processor.flush_recording_boundaries();
        if self.stop_owed {
            // The callback is stopped: settle directly even when its command FIFO is full.
            // Earlier transport starts cannot overtake the exhaustion decision on rebuild.
            for msg in processor.take_queued() {
                if matches!(
                    msg,
                    Msg::Command {
                        cmd: Command::Play
                            | Command::PlayLoops { .. }
                            | Command::SetRecording { on: true },
                        ..
                    }
                ) {
                    continue;
                }
                let queued = self.commands.push(msg);
                debug_assert!(queued.is_ok());
            }
            self.record_mark = self.record_mark.wrapping_add(1);
            processor.finish_recovery_stop(self.record_mark);
            self.recording_armed = false;
            self.recovery_stop_mark = Some(self.record_mark);
            self.stop_owed = false;
        }
        // The fresh graph is built from the state every queued message already updated, so a
        // message that only changes the graph would land in it a second time: a channel built
        // for the old rate, a removal of the rebuilt channel, a parameter for a project that
        // was replaced. Those are dropped here, on this thread, with the nodes and samples
        // they carry, as are notes a later message would have cut; transport commands and the
        // other notes go back in the queue, in order (see `rebuilt_queue`). Holding both ends
        // of the queue (`&mut self` and the stopped processor), this sees every message, and
        // what goes back fits.
        let old_rate = self.sample_rate;
        self.sample_rate = config.rate();
        self.generation += 1;
        let Some(graph) = self.try_build_graph() else {
            self.sample_rate = old_rate;
            self.generation -= 1;
            processor.stop_after_failed_rebuild();
            return false;
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
        self.free_trash();
        true
    }

    /// Construction is transactional: a panic never changes the mirrored project or identities.
    fn try_build_graph(&self) -> Option<Box<Graph>> {
        catch_unwind(AssertUnwindSafe(|| self.replay())).ok()
    }

    pub(crate) fn build_graph(&mut self) -> Box<Graph> {
        self.replay()
    }

    fn replay(&self) -> Box<Graph> {
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
    pub(crate) fn free_trash(&mut self) {
        while self.trash.pop().is_ok() {}
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

    /// Publishes what the processor would after `faults` panics and `guard_hits` NaN blocks
    /// (the processor itself is never run, so nothing overwrites it).
    fn pretend(c: &Controller, faults: u32, guard_hits: u32) {
        c.live.publish([0.0; 2], false, 0, faults, guard_hits);
        c.live.publish_recovery(faults, 0);
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
        assert_eq!(c.commands.slots(), queued);
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
