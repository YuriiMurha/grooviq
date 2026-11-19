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

/// What [`Controller::check_health`] found since its previous call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthEvent {
    /// The graph panicked and was rebuilt (by `check_health`, or by `reconfigure` if it came
    /// first) from the controller's state, or with default parameters if replaying that state
    /// panicked too; `faults` counts every panic so far.
    Restarted { faults: u32 },
    /// The NaN guard silenced and reset a node; `total` counts every such block so far.
    DeviceSilenced { total: u32 },
}

/// Why a command or a change was refused. Nothing is queued or changed when it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// A value was not finite, or a key, parameter index or pattern length is out of range.
    Rejected,
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
    /// Test switch: this many replays of the state panic, as a device bug in `set_param` would.
    #[cfg(test)]
    replay_panics: std::cell::Cell<u32>,
}

impl Controller {
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
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        self.send_at(0, cmd).map_err(|_| cmd)
    }

    /// Queues `cmd` for the block that contains frame `due` (0: the next block); a note lands
    /// on that exact frame. The processor holds back everything behind a command that is not
    /// due yet, so send in frame order. For tests and the offline renderer: not part of the
    /// API.
    #[doc(hidden)]
    pub fn send_at(&mut self, due: u64, cmd: Command) -> Result<(), ControlError> {
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
        self.commands
            .push(Msg::Command { due, cmd })
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
    /// [`MAX_CHANNELS`], `Rejected` for a value that is not finite).
    pub fn load_state(&mut self, state: EngineState) -> Result<(), ControlError> {
        let state = state.checked()?;
        if self.commands.slots() < 5 {
            return Err(ControlError::Full);
        }
        // The old state (and the samples only it held) is freed here, on this thread.
        let _old = std::mem::replace(&mut self.state, state);
        if self.state.playback_mode == crate::PlaybackMode::Song {
            self.recording_armed = false;
        }
        self.renew_instances();
        let graph = self.build_graph();
        // Only this thread pushes, and the queue had room for all five.
        let command = |cmd| Msg::Command { due: 0, cmd };
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
        self.free_trash();
        let live = self.live.snapshot();
        let mut events = Vec::new();
        if live.faults != self.seen_faults {
            // The processor stays silent until it gets a fresh graph. If the queue is full,
            // the next call tries again.
            let graph = self.build_graph();
            if self.commands.push(Msg::ReplaceGraph(graph)).is_ok() {
                self.seen_faults = live.faults;
                self.restart_owed = true;
            }
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
    /// never on the audio thread.
    pub fn reconfigure(&mut self, processor: &mut AudioProcessor, config: EngineConfig) {
        // The fresh graph is built from the state every queued message already updated, so a
        // message that only changes the graph would land in it a second time: a channel built
        // for the old rate, a removal of the rebuilt channel, a parameter for a project that
        // was replaced. Those are dropped here, on this thread, with the nodes and samples
        // they carry, as are notes a later message would have cut; transport commands and the
        // other notes go back in the queue, in order (see `rebuilt_queue`). Holding both ends
        // of the queue (`&mut self` and the stopped processor), this sees every message, and
        // what goes back fits.
        for msg in rebuilt_queue(processor.take_queued()) {
            let queued = self.commands.push(msg).is_ok();
            debug_assert!(queued, "the queue was just emptied");
        }
        self.sample_rate = config.rate();
        // Replacements queued before now are stale: the processor drops them.
        self.generation += 1;
        // The fresh graph also recovers from every fault so far, so `check_health` must not
        // restart it again; a fault it has not reported yet is still owed. (The processor
        // publishes its count after every block, and its callback is stopped now.)
        let faults = self.live.snapshot().faults;
        if faults != self.seen_faults {
            self.seen_faults = faults;
            self.restart_owed = true;
        }
        let graph = self.build_graph();
        processor.restart(self.sample_rate, graph);
        self.free_trash();
    }

    /// A graph for the current sample rate and generation, in the state the queued commands
    /// lead to. If replaying that state panics (a device bug in `set_param`), it is replayed
    /// again with every device parameter at its default (channels, samples, mix and pattern
    /// kept); if that panics too, without the channels. The state is reset to match, so a
    /// restart still happens, the panic stays off the caller (and any mutex it holds), and it
    /// cannot recur.
    pub(crate) fn build_graph(&mut self) -> Box<Graph> {
        if let Ok(graph) = catch_unwind(AssertUnwindSafe(|| self.replay())) {
            return graph;
        }
        self.tone = ToneState::default();
        self.synth = synth_defaults();
        for channel in &mut self.state.channels {
            channel.instrument.reset_params();
        }
        if let Ok(graph) = catch_unwind(AssertUnwindSafe(|| self.replay())) {
            return graph;
        }
        self.state.channels.clear();
        self.state.song.arrangement.notes.clear();
        self.instances.clear();
        let mut graph = Box::new(Graph::new(self.sample_rate, self.generation));
        graph.set_pattern(Box::new(self.state.pattern.clone()));
        graph.set_song(Box::new(self.state.song.clone()));
        graph.playback_mode = self.state.playback_mode;
        graph.song_loop = self.state.song_loop;
        graph
    }

    fn replay(&self) -> Box<Graph> {
        let mut graph = Box::new(Graph::new(self.sample_rate, self.generation));
        #[cfg(test)]
        if self.replay_panics.get() > 0 {
            self.replay_panics.set(self.replay_panics.get() - 1);
            panic!("replaying the controller's state panicked (test switch)");
        }
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
    fn a_rebuild_that_panics_falls_back_to_defaults_and_still_restarts() {
        let (mut c, mut p) = controller();
        let tone = Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        };
        c.send(tone).unwrap();
        let cutoff = Command::SetSynthParam {
            index: 20,
            value: 300.0,
        };
        c.send(cutoff).unwrap();
        c.replay_panics.set(u32::MAX);
        pretend(&c, 1, 0);
        assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
        // The replacement plays with default parameters, and the state now says so too.
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert!(!c.live().tone);
        assert!(!c.tone.on);
        let default_cutoff = grooviq_dsp::param_spec("synth").unwrap()[20].2;
        assert_eq!(c.synth[20], default_cutoff);
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
    fn a_rebuild_that_panics_once_keeps_the_channels_with_default_sampler_params() {
        let (mut c, mut p) = controller();
        c.add_channel(setup(1)).unwrap();
        c.add_channel(setup(2)).unwrap();
        let tune = Command::SetSamplerParam {
            channel: ChannelId(2),
            index: 1,
            value: 5.0,
        };
        c.send(tune).unwrap();
        c.set_pattern(PatternData {
            length: 7,
            notes: Vec::new(),
        })
        .unwrap();
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        c.replay_panics.set(1);
        pretend(&c, 1, 0);
        assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        let defaults: Vec<f32> = grooviq_dsp::sampler::SPEC.iter().map(|s| s.2).collect();
        assert!(
            c.state
                .channels
                .iter()
                .all(|ch| ch.instrument.params() == defaults)
        );
        assert_eq!(c.state.channels.len(), 2);
        // The graph the state builds holds both channels and the pattern.
        let graph = c.build_graph();
        assert_eq!(graph.channel_ids(), [ChannelId(1), ChannelId(2)]);
        assert_eq!(graph.pattern().length, 7);

        // Panicking twice leaves no channels, but keeps the pattern.
        c.state.song.end_tick = 1;
        c.state.song.arrangement.length = 1;
        c.state.song.arrangement.notes = vec![crate::SongNote {
            tick: 0,
            length: 1,
            channel: c.state.channels[0].id,
            key: 60,
            velocity: 1.0,
            slide: false,
            clip_id: 1,
        }];
        c.replay_panics.set(2);
        pretend(&c, 2, 0);
        assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 2 }]);
        assert!(c.state.channels.is_empty());
        assert!(c.state.song.arrangement.notes.is_empty());
        assert!(c.instances.is_empty(), "no channel, no instance");
        assert_eq!(c.state.pattern.length, 7);
        assert!(c.send(tune).is_err());
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
}
