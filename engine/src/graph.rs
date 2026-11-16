//! The audio graph: a sine test tone, the synth and the sampler channels (in insertion order),
//! mixed into the master bus. After each node, a NaN guard silences and resets a node whose
//! block holds NaN or infinity, so one bad device cannot poison the mix. The graph also holds
//! the pattern and the sequencer that plays it into the channels, so a rebuilt graph replays
//! the pattern from where the transport is.
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

pub(crate) struct Graph {
    /// The controller's `reconfigure` count when the graph was built.
    generation: u64,
    tone: ToneNode,
    synth: SynthNode,
    /// Created with room for [`MAX_CHANNELS`], so adding and removing never reallocate.
    #[allow(
        clippy::vec_box,
        reason = "a node arrives and leaves as a Box: moving it in or out never allocates"
    )]
    channels: Vec<Box<ChannelNode>>,
    pattern: Box<PatternData>,
    sequencer: Sequencer,
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
            channels: Vec::with_capacity(MAX_CHANNELS),
            pattern: Box::new(PatternData::default()),
            sequencer: Sequencer::new(),
            node_l: [0.0; BLOCK],
            node_r: [0.0; BLOCK],
        }
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

    /// Fades the tone, the synth and every channel out over 5 ms; the devices reset once
    /// silent.
    pub(crate) fn all_sound_off(&mut self) {
        self.tone.switch_off();
        self.synth.fade_out();
        for channel in &mut self.channels {
            channel.fade_out();
        }
    }

    fn channel(&mut self, id: ChannelId) -> Option<&mut ChannelNode> {
        self.channels
            .iter_mut()
            .find(|c| c.id() == id)
            .map(|c| &mut **c)
    }

    /// Points every channel's gains at its mix, given which channels are soloed now.
    fn retarget(&mut self) {
        let any_solo = self.channels.iter().any(|c| c.solo());
        for channel in &mut self.channels {
            channel.retarget(any_solo);
        }
    }

    /// Puts every channel's gains at their targets at once: for a graph being built.
    pub(crate) fn settle(&mut self) {
        let any_solo = self.channels.iter().any(|c| c.solo());
        for channel in &mut self.channels {
            channel.settle(any_solo);
        }
    }

    /// Appends a channel (mixed after the others). Gives it back if the graph is full or
    /// already has its id.
    pub(crate) fn add_channel(&mut self, node: Box<ChannelNode>) -> Option<Box<ChannelNode>> {
        if self.channels.len() >= MAX_CHANNELS || self.channels.iter().any(|c| c.id() == node.id())
        {
            return Some(node);
        }
        // Within the capacity reserved in `new`: no allocation.
        self.channels.push(node);
        // A soloed newcomer silences the others (it starts at its own gains already).
        self.retarget();
        None
    }

    /// Takes a channel out, keeping the others in order (its pending note-offs go with it);
    /// the caller frees it elsewhere.
    pub(crate) fn remove_channel(&mut self, id: ChannelId) -> Option<Box<ChannelNode>> {
        let index = self.channels.iter().position(|c| c.id() == id)?;
        let node = self.channels.remove(index);
        self.sequencer.forget_channel(id);
        self.retarget();
        Some(node)
    }

    pub(crate) fn set_channel_mix(&mut self, id: ChannelId, mix: ChannelMix) {
        if let Some(channel) = self.channel(id) {
            channel.set_mix(mix);
            self.retarget();
        }
    }

    pub(crate) fn set_sampler_param(&mut self, id: ChannelId, index: u32, value: f32) {
        if let Some(channel) = self.channel(id) {
            channel.set_param(index, value);
        }
    }

    /// Gives the channel `sample` and returns the buffer to free elsewhere: its old sample,
    /// or `sample` itself if there is no such channel.
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
        self.channels
            .iter()
            .find(|c| c.id() == id)
            .is_some_and(|c| c.events_full())
    }

    /// Queues a note for the channel's next block; dropped if the buffer is full or there is
    /// no such channel.
    pub(crate) fn channel_note(&mut self, id: ChannelId, event: NoteEvent) {
        if let Some(channel) = self.channel(id) {
            channel.push_event(event);
        }
    }

    /// Swaps in a pattern and returns the old one, to be freed elsewhere. Playback goes on at
    /// the same tick, in the new pattern (notes already sounding end as booked).
    pub(crate) fn set_pattern(&mut self, pattern: Box<PatternData>) -> Box<PatternData> {
        self.sequencer.reseek();
        std::mem::replace(&mut self.pattern, pattern)
    }

    /// The pattern's length in ticks.
    pub(crate) fn pattern_length(&self) -> u32 {
        self.pattern.length
    }

    /// The transport started: the sequencer finds its place again.
    pub(crate) fn transport_started(&mut self) {
        self.sequencer.reseek();
    }

    /// The transport stopped: every pending note-off goes out at the next block's start.
    pub(crate) fn transport_stopped(&mut self) {
        self.sequencer.stop();
    }

    /// Queues into the channels the sequenced notes of the block that starts at frame `f0`
    /// (call it before [`Self::render`]).
    pub(crate) fn sequence(&mut self, clock: &TransportClock, f0: u64) {
        let channels = &mut self.channels;
        self.sequencer.run(&self.pattern, clock, f0, |id, event| {
            match channels.iter_mut().find(|c| c.id() == id) {
                None => Delivery::NoChannel,
                Some(channel) => {
                    if channel.push_event(event) {
                        Delivery::Taken
                    } else {
                        Delivery::Full
                    }
                }
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn pattern(&self) -> &PatternData {
        &self.pattern
    }

    #[cfg(test)]
    pub(crate) fn channel_ids(&self) -> Vec<ChannelId> {
        self.channels.iter().map(|c| c.id()).collect()
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

        for channel in &mut self.channels {
            // A silent channel adds nothing.
            if !channel.render(node_l, node_r) {
                continue;
            }
            if guard(node_l, node_r) {
                channel.reset();
                silenced += 1;
                continue;
            }
            mix_in(l, r, node_l, node_r);
        }
        silenced
    }
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
                let s = (self.phase * TAU).sin() as f32 * level;
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
