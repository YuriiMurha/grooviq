//! The audio graph: a sine test tone, the synth and the channels (in insertion order), mixed
//! into the master bus. A channel holds a sampler or a synth of its own. A removed channel
//! fades out over 5 ms first: it stays in place, "leaving" (no id lookup finds it and it takes
//! no new notes), until the processor collects it for the trash queue. After each node, a NaN
//! guard silences and resets a node whose block holds NaN or infinity, so one bad device cannot
//! poison the mix. The graph also holds the pattern and the sequencer that plays it into the
//! channels, so a rebuilt graph replays the pattern from where the transport is.
//!
//! The controller builds a graph on its own thread: `Graph::new`, then the setters to replay
//! its state. Once the graph is handed to the processor, everything here runs on the audio
//! thread.

use crate::BLOCK;
use crate::channel::ChannelNode;
use crate::sequencer::{Delivery, Sequencer};
use crate::state::{ChannelId, ChannelMix, MAX_CHANNELS, PatternData};
use crate::transport::TransportClock;
use grooviq_dsp::sampler::SampleBuf;
use grooviq_dsp::{Device, NoteEvent, db_to_gain};
use std::f64::consts::TAU;
use std::sync::Arc;

/// Length of every de-click ramp: tone changes and the synth fade on `AllSoundOff`.
const RAMP_SECONDS: f32 = 0.005;
/// Channels fading out after their removal, at most. Past that, a removed channel stops at once.
const MAX_LEAVING: usize = MAX_CHANNELS;
/// Note-ons of grouped channels one block chokes with. Past that, the further note-ons of the
/// block (in channel order, then offset) choke nothing; they still play.
const MAX_CHOKES: usize = 256;

/// A note-on that chokes its group: `(group, offset, channel)`.
type Choke = (u8, u32, ChannelId);

pub(crate) struct Graph {
    /// The controller's `reconfigure` count when the graph was built.
    generation: u64,
    tone: ToneNode,
    synth: SynthNode,
    /// Created with room for [`MAX_CHANNELS`] plus [`MAX_LEAVING`], so adding and removing
    /// never reallocate. Leaving channels keep their place until collected.
    #[allow(
        clippy::vec_box,
        reason = "a node arrives and leaves as a Box: moving it in or out never allocates"
    )]
    channels: Vec<Box<ChannelNode>>,
    pattern: Box<PatternData>,
    sequencer: Sequencer,
    song: Box<crate::SongData>,
    audio: crate::audio_clip::AudioRuntime,
    audio_l: [f32; BLOCK],
    audio_r: [f32; BLOCK],
    song_sequencer: crate::song::SongSequencer,
    pub(crate) playback_mode: crate::PlaybackMode,
    pub(crate) song_loop: Option<crate::SongLoop>,
    /// The grouped note-ons of the block being rendered, and one channel's split offsets.
    chokes: [Choke; MAX_CHOKES],
    splits: [u32; MAX_CHOKES],
    /// The block of the node being rendered, checked by the guard before it joins the master.
    node_l: [f32; BLOCK],
    node_r: [f32; BLOCK],
}

/// Samples in a de-click ramp at `sample_rate`.
pub(crate) fn ramp_len(sample_rate: f32) -> u32 {
    (RAMP_SECONDS * sample_rate).round().max(1.0) as u32
}

impl Graph {
    /// Builds the tone and the synth for `sample_rate`, with no channels and an empty pattern
    /// (allocates: call it off the audio thread).
    pub(crate) fn new(sample_rate: f32, generation: u64) -> Self {
        let ramp_len = ramp_len(sample_rate);
        Self {
            generation,
            tone: ToneNode::new(sample_rate, ramp_len),
            synth: SynthNode::new(sample_rate, ramp_len),
            channels: Vec::with_capacity(MAX_CHANNELS + MAX_LEAVING),
            pattern: Box::new(PatternData::default()),
            sequencer: Sequencer::new(),
            song: Box::default(),
            audio: Default::default(),
            audio_l: [0.0; BLOCK],
            audio_r: [0.0; BLOCK],
            song_sequencer: crate::song::SongSequencer::new(),
            playback_mode: crate::PlaybackMode::Pattern,
            song_loop: None,
            chokes: [(0, 0, ChannelId(0)); MAX_CHOKES],
            splits: [0; MAX_CHOKES],
            node_l: [0.0; BLOCK],
            node_r: [0.0; BLOCK],
        }
    }

    pub(crate) fn synth_meter(&self, instance: u64) -> Option<crate::SynthMeter> {
        self.present()
            .find(|c| c.instance == instance)?
            .synth_meter()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn set_tone(&mut self, on: bool, hz: f32, gain_db: f32) {
        self.tone.set(on, hz, gain_db);
    }

    pub(crate) fn set_synth_param(&mut self, index: u32, value: f32) {
        self.synth.device.set_param(index, value);
    }

    /// Fades the tone and the synth out over 5 ms (each resets once silent) and chokes every
    /// channel: the notes it holds or has queued fade over 3 ms, and later notes play in full.
    pub(crate) fn all_sound_off(&mut self) {
        self.tone.switch_off();
        self.synth.fade_out();
        for channel in &mut self.channels {
            channel.choke_all();
        }
    }

    /// The channels that are not leaving.
    fn present(&self) -> impl Iterator<Item = &ChannelNode> {
        self.channels.iter().filter(|c| !c.leaving()).map(|c| &**c)
    }

    fn channel(&mut self, id: ChannelId) -> Option<&mut ChannelNode> {
        self.channels
            .iter_mut()
            .find(|c| c.id() == id && !c.leaving())
            .map(|c| &mut **c)
    }

    /// Points every present channel's gains at its mix, given which channels are soloed now (a
    /// leaving one counts for nothing and keeps its gains).
    fn retarget(&mut self) {
        let any_solo = self.present().any(ChannelNode::solo);
        for channel in self.channels.iter_mut().filter(|c| !c.leaving()) {
            channel.retarget(any_solo);
        }
    }

    /// Puts every channel's gains at their targets at once: for a graph being built.
    pub(crate) fn settle(&mut self) {
        let any_solo = self.present().any(ChannelNode::solo);
        for channel in &mut self.channels {
            channel.settle(any_solo);
        }
    }

    /// Appends a channel (mixed after the others). Gives it back if the graph is full or
    /// already has its id (a leaving channel's id is free again).
    pub(crate) fn add_channel(&mut self, node: Box<ChannelNode>) -> Option<Box<ChannelNode>> {
        if self.present().count() >= MAX_CHANNELS || self.present().any(|c| c.id() == node.id()) {
            return Some(node);
        }
        // Within the capacity reserved in `new` (at most MAX_CHANNELS present and MAX_LEAVING
        // leaving): no allocation.
        self.channels.push(node);
        // A soloed newcomer silences the others (it starts at its own gains already).
        self.retarget();
        None
    }

    /// Removes a channel: it fades out over 5 ms in place (taking no new notes), and
    /// [`Self::take_departed`] hands it over once silent. Its pending sequenced note-offs are
    /// dropped. With [`MAX_LEAVING`] channels already fading it is taken out at once and
    /// returned, for the caller to free elsewhere.
    pub(crate) fn remove_channel(&mut self, id: ChannelId) -> Option<Box<ChannelNode>> {
        let index = self
            .channels
            .iter()
            .position(|c| c.id() == id && !c.leaving())?;
        self.sequencer.forget_channel(id);
        self.song_sequencer.forget_channel(id);
        let leaving = self.channels.iter().filter(|c| c.leaving()).count();
        let removed = if leaving < MAX_LEAVING {
            self.channels[index].leave();
            None
        } else {
            Some(self.channels.remove(index))
        };
        self.retarget();
        removed
    }

    /// Takes out a removed channel whose fade has ended, if any, keeping the others in order;
    /// the caller frees it elsewhere.
    pub(crate) fn take_departed(&mut self) -> Option<Box<ChannelNode>> {
        let index = self.channels.iter().position(|c| c.departed())?;
        Some(self.channels.remove(index))
    }

    pub(crate) fn set_channel_mix(&mut self, id: ChannelId, mix: ChannelMix) {
        if let Some(channel) = self.channel(id) {
            channel.set_mix(mix);
            self.retarget();
        }
    }

    pub(crate) fn set_sampler_param(&mut self, id: ChannelId, index: u32, value: f32) {
        if let Some(channel) = self.channel(id) {
            channel.set_sampler_param(index, value);
        }
    }

    pub(crate) fn set_channel_param(&mut self, id: ChannelId, index: u32, value: f32) {
        if let Some(channel) = self.channel(id) {
            channel.set_param(index, value);
        }
    }

    pub(crate) fn set_choke(&mut self, id: ChannelId, group: u8) {
        if let Some(channel) = self.channel(id) {
            channel.set_choke_group(group);
        }
    }

    /// Gives the channel `sample` and returns the buffer to free elsewhere: its old sample,
    /// or `sample` itself if there is no such channel or it holds a synth.
    pub(crate) fn set_channel_sample(
        &mut self,
        id: ChannelId,
        sample: Option<Arc<SampleBuf>>,
    ) -> Option<Arc<SampleBuf>> {
        match self.channel(id) {
            Some(channel) => channel.set_sample(sample),
            None => sample,
        }
    }

    /// Whether the channel's note buffer for this block is full (false for no such channel).
    pub(crate) fn channel_events_full(&self, id: ChannelId) -> bool {
        self.present()
            .find(|c| c.id() == id)
            .is_some_and(ChannelNode::events_full)
    }

    /// Queues a note for the channel's next block. False (and dropped) if the buffer is full
    /// or there is no such channel.
    pub(crate) fn channel_note(&mut self, id: ChannelId, event: NoteEvent) -> bool {
        self.channel(id).is_some_and(|c| c.push_event(event))
    }

    /// Swaps in a pattern and returns the old one, to be freed elsewhere. Playback goes on at
    /// the same tick, in the new pattern (notes already sounding end as booked).
    pub(crate) fn set_pattern(&mut self, pattern: Box<PatternData>) -> Box<PatternData> {
        self.sequencer.reseek();
        self.song_sequencer.reseek();
        std::mem::replace(&mut self.pattern, pattern)
    }

    /// The pattern's length in ticks.
    pub(crate) fn pattern_length(&self) -> u32 {
        self.pattern.length
    }

    /// The transport started: the sequencer finds its place again.
    pub(crate) fn transport_started(&mut self) {
        self.audio.reset();
        self.sequencer.reseek();
        self.song_sequencer.reseek();
    }

    /// The transport stopped: every pending note-off goes out at the next block's start.
    pub(crate) fn transport_stopped(&mut self) {
        self.audio.reset();
        self.sequencer.stop();
        self.song_sequencer.stop();
    }

    /// Queues into the channels the sequenced notes of the block that starts at frame `f0`
    /// (call it before [`Self::render`]).
    pub(crate) fn sequence(&mut self, clock: &TransportClock, f0: u64) {
        self.audio_l.fill(0.0);
        self.audio_r.fill(0.0);
        if self.playback_mode == crate::PlaybackMode::Song {
            self.audio.render(
                &self.song.audio,
                self.song_loop,
                clock,
                f0,
                &mut self.audio_l,
                &mut self.audio_r,
            );
        }
        let channels = &mut self.channels;
        let mut sink = |id, event| match channels.iter_mut().find(|c| c.id() == id && !c.leaving())
        {
            None => Delivery::NoChannel,
            Some(channel) => {
                if channel.push_event(event) {
                    Delivery::Taken
                } else {
                    Delivery::Full
                }
            }
        };
        if self.playback_mode == crate::PlaybackMode::Song {
            self.sequencer
                .flush(clock, f0, f0 + BLOCK as u64 - 1, &mut sink);
            self.song_sequencer
                .run(&self.song, self.song_loop, clock, f0, &mut sink);
        } else {
            self.song_sequencer
                .flush(clock, f0, f0 + BLOCK as u64 - 1, &mut sink);
            self.sequencer.run(&self.pattern, clock, f0, &mut sink);
        }
    }

    pub(crate) fn set_song(&mut self, song: Box<crate::SongData>) -> Box<crate::SongData> {
        self.song_sequencer.stop();
        if !crate::audio_clip::AudioRuntime::compatible(&self.song.audio, &song.audio) {
            self.audio.reset();
        }
        std::mem::replace(&mut self.song, song)
    }

    pub(crate) fn reset_audio(&mut self) {
        self.audio.reset();
    }
    pub(crate) fn audio_active(&self) -> bool {
        self.audio.active()
    }
    pub(crate) fn keep_audio(&mut self, old: &Self) {
        if self.song_loop == old.song_loop
            && self.playback_mode == old.playback_mode
            && crate::audio_clip::AudioRuntime::compatible(&self.song.audio, &old.song.audio)
        {
            self.audio = old.audio.clone();
        }
    }
    pub(crate) fn song(&self) -> &crate::SongData {
        &self.song
    }

    pub(crate) fn position(&self, tick: u64) -> u64 {
        if self.playback_mode == crate::PlaybackMode::Song {
            if let Some(r) = self.song_loop
                && tick >= r.end_tick
            {
                return r.start_tick + (tick - r.start_tick) % (r.end_tick - r.start_tick);
            }
            tick.min(self.song.end_tick)
        } else {
            tick
        }
    }

    #[cfg(test)]
    pub(crate) fn pattern(&self) -> &PatternData {
        &self.pattern
    }

    /// The present channels' ids, in order.
    #[cfg(test)]
    pub(crate) fn channel_ids(&self) -> Vec<ChannelId> {
        self.present().map(ChannelNode::id).collect()
    }

    pub(crate) fn tone_on(&self) -> bool {
        self.tone.on
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn inject_nan(&mut self) {
        self.synth.nan_next = true;
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn inject_panic(&mut self) {
        self.synth.panic_next = true;
    }

    /// Renders one block into the master bus `l`/`r` (overwritten); `events` are sorted by
    /// offset. Returns how many nodes the NaN guard silenced.
    pub(crate) fn render(
        &mut self,
        l: &mut [f32; BLOCK],
        r: &mut [f32; BLOCK],
        events: &[NoteEvent],
    ) -> u32 {
        let mut silenced = 0;
        let (node_l, node_r) = (&mut self.node_l, &mut self.node_r);

        self.tone.render(node_l, node_r);
        if guard(node_l, node_r) {
            self.tone.reset();
            silenced += 1;
        }
        *l = *node_l;
        *r = *node_r;

        self.synth.render(node_l, node_r, events);
        if guard(node_l, node_r) {
            self.synth.reset();
            silenced += 1;
        }
        mix_in(l, r, node_l, node_r);

        let chokes = collect_chokes(&self.channels, &mut self.chokes);
        for channel in &mut self.channels {
            // Waiting to be collected: silent for good.
            if channel.departed() {
                continue;
            }
            let splits = splits_for(channel, chokes, &mut self.splits);
            // A silent channel adds nothing.
            if !channel.render(node_l, node_r, splits) {
                continue;
            }
            if guard(node_l, node_r) {
                channel.reset();
                silenced += 1;
                continue;
            }
            mix_in(l, r, node_l, node_r);
        }
        if !self.song.audio.is_empty() && self.playback_mode == crate::PlaybackMode::Song {
            if guard(&mut self.audio_l, &mut self.audio_r) {
                silenced += 1;
            }
            mix_in(l, r, &self.audio_l, &self.audio_r);
        }
        silenced
    }
}

/// Fills `chokes` with the queued note-ons of every grouped channel, in channel order (at most
/// [`MAX_CHOKES`]), and returns them.
fn collect_chokes<'a>(
    channels: &[Box<ChannelNode>],
    chokes: &'a mut [Choke; MAX_CHOKES],
) -> &'a [Choke] {
    let mut n = 0;
    for channel in channels {
        let group = channel.choke_group();
        if group == 0 {
            continue;
        }
        for offset in channel.note_on_offsets() {
            if n == MAX_CHOKES {
                return chokes;
            }
            chokes[n] = (group, offset, channel.id());
            n += 1;
        }
    }
    &chokes[..n]
}

/// The offsets, sorted and distinct, at which other channels of `channel`'s group start notes.
fn splits_for<'a>(
    channel: &ChannelNode,
    chokes: &[Choke],
    splits: &'a mut [u32; MAX_CHOKES],
) -> &'a [u32] {
    let group = channel.choke_group();
    if group == 0 || chokes.is_empty() {
        return &[];
    }
    let mut n = 0;
    for &(g, offset, id) in chokes {
        if g == group && id != channel.id() {
            splits[n] = offset;
            n += 1;
        }
    }
    let splits = &mut splits[..n];
    // In place: no allocation.
    splits.sort_unstable();
    let mut distinct = 0;
    for i in 0..n {
        if i == 0 || splits[i] != splits[distinct - 1] {
            splits[distinct] = splits[i];
            distinct += 1;
        }
    }
    &splits[..distinct]
}

/// Adds a node's block to the master bus.
fn mix_in(
    l: &mut [f32; BLOCK],
    r: &mut [f32; BLOCK],
    node_l: &[f32; BLOCK],
    node_r: &[f32; BLOCK],
) {
    for (master, node) in l.iter_mut().zip(node_l.iter()) {
        *master += node;
    }
    for (master, node) in r.iter_mut().zip(node_r.iter()) {
        *master += node;
    }
}

/// Zeroes a block that holds NaN or infinity. Returns true if it had to.
fn guard(l: &mut [f32], r: &mut [f32]) -> bool {
    if l.iter().chain(r.iter()).all(|x| x.is_finite()) {
        return false;
    }
    l.fill(0.0);
    r.fill(0.0);
    true
}

/// A linear ramp that reaches its target after a fixed number of samples.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Ramp {
    pub(crate) fn at(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            left: 0,
        }
    }

    /// Heads for `target`, arriving after `samples` samples.
    pub(crate) fn start(&mut self, target: f32, samples: u32) {
        self.target = target;
        self.left = samples.max(1);
        self.step = (target - self.value) / self.left as f32;
    }

    /// The value for the next sample.
    pub(crate) fn tick(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }

    pub(crate) fn done(&self) -> bool {
        self.left == 0
    }

    pub(crate) fn target(&self) -> f32 {
        self.target
    }

    /// Ticks `samples` times, discarding the values (bit for bit the same state).
    pub(crate) fn advance(&mut self, samples: u32) {
        for _ in 0..samples.min(self.left) {
            self.tick();
        }
    }
}

/// A sine test tone. The phase is f64 so the pitch stays exact; every change of level ramps.
struct ToneNode {
    sample_rate: f64,
    ramp_len: u32,
    on: bool,
    /// Linear gain while on.
    gain: f32,
    /// Cycles, 0..1.
    phase: f64,
    /// Cycles per sample.
    step: f64,
    level: Ramp,
}

impl ToneNode {
    fn new(sample_rate: f32, ramp_len: u32) -> Self {
        Self {
            sample_rate: f64::from(sample_rate),
            ramp_len,
            on: false,
            gain: 0.0,
            phase: 0.0,
            step: 0.0,
            level: Ramp::at(0.0),
        }
    }

    fn target(&self) -> f32 {
        if self.on { self.gain } else { 0.0 }
    }

    fn set(&mut self, on: bool, hz: f32, gain_db: f32) {
        self.on = on;
        self.step = f64::from(hz) / self.sample_rate;
        self.gain = db_to_gain(gain_db);
        self.level.start(self.target(), self.ramp_len);
    }

    fn switch_off(&mut self) {
        self.on = false;
        self.level.start(0.0, self.ramp_len);
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let level = self.level.tick();
            let s = if level == 0.0 {
                // Silent: the next start begins at phase 0.
                self.phase = 0.0;
                0.0
            } else {
                let s = libm::sin(self.phase * TAU) as f32 * level;
                self.phase = (self.phase + self.step).fract();
                s
            };
            *a = s;
            *b = s;
        }
    }

    /// Clears the phase and ramps back in from silence, keeping the settings.
    fn reset(&mut self) {
        self.phase = 0.0;
        self.level = Ramp::at(0.0);
        self.level.start(self.target(), self.ramp_len);
    }
}

/// The synth, plus the fade `AllSoundOff` starts.
struct SynthNode {
    device: Box<dyn Device>,
    ramp_len: u32,
    /// Output gain while fading out after `AllSoundOff`; the synth resets when it reaches 0.
    fade: Option<Ramp>,
    #[cfg(feature = "test-hooks")]
    nan_next: bool,
    #[cfg(feature = "test-hooks")]
    panic_next: bool,
}

impl SynthNode {
    fn new(sample_rate: f32, ramp_len: u32) -> Self {
        Self {
            device: grooviq_dsp::create("synth", sample_rate)
                .expect("grooviq-dsp always provides the synth"),
            ramp_len,
            fade: None,
            #[cfg(feature = "test-hooks")]
            nan_next: false,
            #[cfg(feature = "test-hooks")]
            panic_next: false,
        }
    }

    fn fade_out(&mut self) {
        if self.fade.is_none() {
            let mut fade = Ramp::at(1.0);
            fade.start(0.0, self.ramp_len);
            self.fade = Some(fade);
        }
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32], events: &[NoteEvent]) {
        #[cfg(feature = "test-hooks")]
        if std::mem::take(&mut self.panic_next) {
            panic!("{}", crate::INJECTED_PANIC);
        }
        self.device.process(l, r, None, events);
        if let Some(fade) = &mut self.fade {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let gain = fade.tick();
                *a *= gain;
                *b *= gain;
            }
            let silent = fade.done();
            if silent {
                // Every voice is released at once now that nothing can be heard.
                self.reset();
            }
        }
        #[cfg(feature = "test-hooks")]
        if std::mem::take(&mut self.nan_next) {
            l[0] = f32::NAN;
        }
    }

    fn reset(&mut self) {
        self.device.reset();
        self.fade = None;
    }
}
