//! A sampler channel in the graph: the sampler, its mix (with 5 ms gain ramps) and the notes
//! waiting for its next block.
//!
//! Built by the controller on its own thread ([`ChannelNode::new`] allocates); everything else
//! runs on the audio thread.

use crate::BLOCK;
use crate::graph::Ramp;
use crate::state::{ChannelId, ChannelMix, ChannelSetup};
use grooviq_dsp::sampler::{SampleBuf, Sampler};
use grooviq_dsp::{Device, NoteEvent};
use std::sync::Arc;

/// Note events one channel takes per block. When full, a note-on is dropped and a note-off
/// waits in the queue for the next block.
pub(crate) const CHANNEL_EVENTS: usize = 128;

const NO_EVENT: NoteEvent = NoteEvent {
    offset: 0,
    key: 0,
    velocity: 0.0,
    on: false,
    slide: false,
};

pub(crate) struct ChannelNode {
    id: ChannelId,
    sampler: Sampler,
    mix: ChannelMix,
    /// Left and right gain: volume, pan law and mute/solo together.
    gain: [Ramp; 2],
    ramp_len: u32,
    /// Output gain while fading out after `AllSoundOff`; the sampler resets when it reaches 0.
    fade: Option<Ramp>,
    /// No voice sounds and nothing is smoothing: blocks without events are skipped.
    idle: bool,
    events: [NoteEvent; CHANNEL_EVENTS],
    count: usize,
}

impl ChannelNode {
    /// A channel from a checked `setup`, its gains already at their targets for `any_solo`
    /// (allocates: call it off the audio thread).
    pub(crate) fn new(
        sample_rate: f32,
        ramp_len: u32,
        setup: &ChannelSetup,
        any_solo: bool,
    ) -> Self {
        let mut sampler = Sampler::new(sample_rate);
        for (index, &value) in (0u32..).zip(&setup.sampler) {
            sampler.set_param(index, value);
        }
        sampler.set_sample(setup.sample.clone());
        let [l, r] = setup.mix.gains(setup.mix.audible(any_solo));
        Self {
            id: setup.id,
            sampler,
            mix: setup.mix,
            gain: [Ramp::at(l), Ramp::at(r)],
            ramp_len,
            fade: None,
            idle: true,
            events: [NO_EVENT; CHANNEL_EVENTS],
            count: 0,
        }
    }

    pub(crate) fn id(&self) -> ChannelId {
        self.id
    }

    pub(crate) fn solo(&self) -> bool {
        self.mix.solo
    }

    /// Stores the mix; [`Self::retarget`] then starts the ramps (solo depends on every
    /// channel).
    pub(crate) fn set_mix(&mut self, mix: ChannelMix) {
        self.mix = mix;
    }

    /// Ramps the gains toward the mix's (when they change), for `any_solo`.
    pub(crate) fn retarget(&mut self, any_solo: bool) {
        let targets = self.mix.gains(self.mix.audible(any_solo));
        for (ramp, target) in self.gain.iter_mut().zip(targets) {
            if ramp.target() != target {
                ramp.start(target, self.ramp_len);
            }
        }
    }

    /// Puts the gains at their targets at once (for a graph being built).
    pub(crate) fn settle(&mut self, any_solo: bool) {
        let [l, r] = self.mix.gains(self.mix.audible(any_solo));
        self.gain = [Ramp::at(l), Ramp::at(r)];
    }

    pub(crate) fn set_param(&mut self, index: u32, value: f32) {
        self.sampler.set_param(index, value);
    }

    /// Swaps the sample (silencing the voices) and returns the old one, to be freed elsewhere.
    pub(crate) fn set_sample(&mut self, sample: Option<Arc<SampleBuf>>) -> Option<Arc<SampleBuf>> {
        self.sampler.set_sample(sample)
    }

    pub(crate) fn events_full(&self) -> bool {
        self.count == CHANNEL_EVENTS
    }

    /// Queues a note for the next render; false (and dropped) if the buffer is full.
    pub(crate) fn push_event(&mut self, event: NoteEvent) -> bool {
        let Some(slot) = self.events.get_mut(self.count) else {
            return false;
        };
        *slot = event;
        self.count += 1;
        true
    }

    /// Fades the channel out over 5 ms, then silences it. Nothing to do if nothing sounds and
    /// no note waits.
    pub(crate) fn fade_out(&mut self) {
        if self.fade.is_none() && !(self.idle && self.count == 0) {
            let mut fade = Ramp::at(1.0);
            fade.start(0.0, self.ramp_len);
            self.fade = Some(fade);
        }
    }

    /// Renders one block into `l`/`r` (overwritten) and clears the events. Returns false, with
    /// `l`/`r` untouched, for a silent block (nothing sounds and no note arrives), so the
    /// caller can skip it.
    pub(crate) fn render(&mut self, l: &mut [f32; BLOCK], r: &mut [f32; BLOCK]) -> bool {
        let events = &self.events[..self.count];
        self.count = 0;
        if self.idle && events.is_empty() {
            // The gains still move, so a mix change completes on time.
            for ramp in &mut self.gain {
                ramp.advance(BLOCK as u32);
            }
            return false;
        }
        self.sampler.process(l, r, None, events);
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            *a *= self.gain[0].tick();
            *b *= self.gain[1].tick();
        }
        if let Some(fade) = &mut self.fade {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let gain = fade.tick();
                *a *= gain;
                *b *= gain;
            }
            if fade.done() {
                // Every voice stops at once now that nothing can be heard.
                self.reset();
            }
        }
        if self.sampler.active_voices() == 0 {
            // Settles the sampler's output smoothing too, as it would on its own once idle.
            self.sampler.reset();
            self.idle = true;
        } else {
            self.idle = false;
        }
        true
    }

    /// Silences every voice, keeping the sample, the parameters and the mix.
    pub(crate) fn reset(&mut self) {
        self.sampler.reset();
        self.fade = None;
        self.idle = true;
    }
}
