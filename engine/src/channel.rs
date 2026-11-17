//! A channel in the graph: its instrument (a sampler or the synth), its mix (with 5 ms gain
//! ramps), its choke group and the notes waiting for its next block.
//!
//! Built by the controller on its own thread ([`ChannelNode::new`] allocates); everything else
//! runs on the audio thread.

use crate::BLOCK;
use crate::graph::Ramp;
use crate::state::{ChannelId, ChannelMix, ChannelSetup, Instrument};
use grooviq_dsp::sampler::{SampleBuf, Sampler};
use grooviq_dsp::synth::Synth;
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
    id: 0,
};

/// The channel's instrument.
#[allow(
    clippy::large_enum_variant,
    reason = "the node lives in a Box already; the synth is boxed for its size alone"
)]
enum Source {
    Sampler(Sampler),
    Synth(Box<Synth>),
}

impl Source {
    fn device(&mut self) -> &mut dyn Device {
        match self {
            Self::Sampler(s) => s,
            Self::Synth(s) => &mut **s,
        }
    }
}

pub(crate) struct ChannelNode {
    id: ChannelId,
    source: Source,
    mix: ChannelMix,
    /// Choke group (0: none).
    choke: u8,
    /// Left and right gain: volume, pan law and mute/solo together.
    gain: [Ramp; 2],
    ramp_len: u32,
    /// Output gain while fading out; the device resets when it reaches 0.
    fade: Option<Ramp>,
    /// Removed: fading out, then silent until the graph hands it to the trash queue.
    leaving: bool,
    /// A sampler with no voice sounding: blocks without events are skipped (the ramps and the
    /// fade still advance, so skipping never changes the output). Never set for the synth,
    /// whose LFO runs on while it is silent.
    idle: bool,
    events: [NoteEvent; CHANNEL_EVENTS],
    count: usize,
    /// Test switch: render every block, to compare with the skipping path.
    #[cfg(test)]
    never_skip: bool,
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
        let mut source = match &setup.instrument {
            Instrument::Sampler { sample, .. } => {
                let mut sampler = Sampler::new(sample_rate);
                sampler.set_sample(sample.clone());
                Source::Sampler(sampler)
            }
            Instrument::Synth { .. } => Source::Synth(Box::new(Synth::new(sample_rate))),
        };
        let device = source.device();
        for (index, &value) in (0u32..).zip(setup.instrument.params()) {
            device.set_param(index, value);
        }
        let [l, r] = setup.mix.gains(setup.mix.audible(any_solo));
        let idle = matches!(source, Source::Sampler(_));
        Self {
            id: setup.id,
            source,
            mix: setup.mix,
            choke: setup.choke,
            gain: [Ramp::at(l), Ramp::at(r)],
            ramp_len,
            fade: None,
            leaving: false,
            idle,
            events: [NO_EVENT; CHANNEL_EVENTS],
            count: 0,
            #[cfg(test)]
            never_skip: false,
        }
    }

    pub(crate) fn id(&self) -> ChannelId {
        self.id
    }

    pub(crate) fn solo(&self) -> bool {
        self.mix.solo
    }

    pub(crate) fn choke_group(&self) -> u8 {
        self.choke
    }

    pub(crate) fn set_choke_group(&mut self, group: u8) {
        self.choke = group;
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

    /// Sets a parameter of the instrument, by its index in the instrument's own `SPEC`.
    pub(crate) fn set_param(&mut self, index: u32, value: f32) {
        self.source.device().set_param(index, value);
    }

    /// Sets a sampler parameter; a synth channel ignores it.
    pub(crate) fn set_sampler_param(&mut self, index: u32, value: f32) {
        if let Source::Sampler(sampler) = &mut self.source {
            sampler.set_param(index, value);
        }
    }

    /// Swaps a sampler's sample (silencing the voices) and returns the old one, to be freed
    /// elsewhere. A synth channel takes none and hands `sample` back.
    pub(crate) fn set_sample(&mut self, sample: Option<Arc<SampleBuf>>) -> Option<Arc<SampleBuf>> {
        match &mut self.source {
            Source::Sampler(sampler) => sampler.set_sample(sample),
            Source::Synth(_) => sample,
        }
    }

    pub(crate) fn events_full(&self) -> bool {
        self.count == CHANNEL_EVENTS
    }

    /// Queues a note for the next render, in offset order (after every queued event at the
    /// same offset, so arrival order holds there); false (and dropped) if the buffer is full.
    /// Preview notes and sequenced ones arrive separately, so this keeps the block sorted.
    pub(crate) fn push_event(&mut self, event: NoteEvent) -> bool {
        if self.count == CHANNEL_EVENTS {
            return false;
        }
        let mut i = self.count;
        while i > 0 && self.events[i - 1].offset > event.offset {
            self.events[i] = self.events[i - 1];
            i -= 1;
        }
        self.events[i] = event;
        self.count += 1;
        true
    }

    /// The offsets of the note-ons queued for the next block, in order. A note-on with
    /// velocity 0 is a note-off and is not one.
    pub(crate) fn note_on_offsets(&self) -> impl Iterator<Item = u32> + '_ {
        self.events[..self.count]
            .iter()
            .filter(|e| e.on && e.velocity > 0.0)
            .map(|e| e.offset)
    }

    /// Fades the channel out over 5 ms, then silences it (a note in those 5 ms fades too).
    pub(crate) fn fade_out(&mut self) {
        if self.fade.is_none() {
            let mut fade = Ramp::at(1.0);
            fade.start(0.0, self.ramp_len);
            self.fade = Some(fade);
        }
    }

    /// The channel is removed: it fades out (the notes queued so far play into the fade).
    pub(crate) fn leave(&mut self) {
        self.leaving = true;
        self.fade_out();
    }

    pub(crate) fn leaving(&self) -> bool {
        self.leaving
    }

    /// Removed and silent for good: ready for the trash queue.
    pub(crate) fn departed(&self) -> bool {
        self.leaving && self.fade.is_none()
    }

    /// Cuts every note (`AllSoundOff`): the note-ons queued so far are dropped (their note-offs
    /// stay, finding nothing) and the instrument chokes, its voices fading over 3 ms. Notes
    /// queued after the call play in full.
    pub(crate) fn choke_all(&mut self) {
        let mut kept = 0;
        for i in 0..self.count {
            let e = self.events[i];
            if !(e.on && e.velocity > 0.0) {
                self.events[kept] = e;
                kept += 1;
            }
        }
        self.count = kept;
        self.source.device().choke();
    }

    /// Renders one block into `l`/`r` (overwritten) and clears the events. `chokes` are the
    /// offsets, sorted and distinct, at which other channels of its choke group start notes:
    /// the block renders in pieces split there, the instrument choking at each split (before
    /// this channel's own notes at that offset). Returns false, with `l`/`r` untouched, for a
    /// silent block (nothing sounds and no note arrives), so the caller can skip it.
    pub(crate) fn render(
        &mut self,
        l: &mut [f32; BLOCK],
        r: &mut [f32; BLOCK],
        chokes: &[u32],
    ) -> bool {
        let count = std::mem::take(&mut self.count);
        #[cfg(test)]
        let skip = !self.never_skip;
        #[cfg(not(test))]
        let skip = true;
        // A choke of a silent instrument does nothing.
        if skip && self.idle && count == 0 {
            // Everything that moves with time still moves, exactly as if the block of silence
            // were rendered: the gains (a mix change completes on time) and the fade (which
            // must end, not wait to fade the next note).
            for ramp in &mut self.gain {
                ramp.advance(BLOCK as u32);
            }
            if let Some(fade) = &mut self.fade {
                fade.advance(BLOCK as u32);
                if fade.done() {
                    self.reset();
                }
            }
            return false;
        }
        let device = self.source.device();
        let events = &mut self.events[..count];
        // Each piece gets the events in it, their offsets made relative to its start.
        let (mut start, mut first) = (0, 0);
        for &split in chokes {
            let split = split as usize;
            if split > start {
                let end = first + events[first..].partition_point(|e| (e.offset as usize) < split);
                for e in &mut events[first..end] {
                    e.offset -= start as u32;
                }
                device.process(
                    &mut l[start..split],
                    &mut r[start..split],
                    None,
                    &events[first..end],
                );
                (start, first) = (split, end);
            }
            device.choke();
        }
        for e in &mut events[first..] {
            e.offset -= start as u32;
        }
        device.process(&mut l[start..], &mut r[start..], None, &events[first..]);
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
        if let Source::Sampler(sampler) = &mut self.source {
            if sampler.active_voices() == 0 {
                // Settles the sampler's output smoothing, as it would on its own once idle.
                // Not a reset: a note that ended before its note-off must still take that
                // note-off, or it cuts the next hit on the key when a block ends between the
                // two.
                sampler.settle();
                self.idle = true;
            } else {
                self.idle = false;
            }
        }
        true
    }

    /// Silences every voice, keeping the sample, the parameters and the mix.
    pub(crate) fn reset(&mut self) {
        self.source.device().reset();
        self.fade = None;
        self.idle = matches!(self.source, Source::Sampler(_));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ramp_len;
    use crate::state::ChannelMix;
    use grooviq_dsp::sampler::SPEC;

    const SR: f32 = 48_000.0;
    /// The sampler's gain parameter (dB).
    const GAIN: u32 = 12;

    fn node(never_skip: bool) -> ChannelNode {
        let l: Vec<f32> = (0..200).map(|i| 0.9 * libm::powf(0.97, i as f32)).collect();
        let sample = Arc::new(SampleBuf {
            rate: SR,
            r: l.clone(),
            l,
        });
        let params = SPEC.iter().map(|&(_, _, d)| d).collect();
        let setup =
            ChannelSetup::sampler(ChannelId(1), Some(sample), params, ChannelMix::default());
        let mut node = ChannelNode::new(SR, ramp_len(SR), &setup, false);
        node.never_skip = never_skip;
        node
    }

    fn note(offset: u32) -> NoteEvent {
        NoteEvent {
            offset,
            key: 60,
            velocity: 0.9,
            on: true,
            slide: false,
            id: 60,
        }
    }

    #[test]
    fn events_queue_in_offset_order_and_keep_arrival_order_at_one_offset() {
        let mut n = node(true);
        let ev = |offset, key, on| NoteEvent {
            offset,
            key,
            velocity: 0.5,
            on,
            slide: false,
            id: u64::from(key),
        };
        for e in [
            ev(10, 1, true),
            ev(40, 2, true),
            ev(10, 1, false),
            ev(5, 3, false),
            ev(40, 2, false),
            ev(63, 4, true),
            ev(0, 5, true),
        ] {
            assert!(n.push_event(e));
        }
        let got: Vec<(u32, u8, bool)> = n.events[..n.count]
            .iter()
            .map(|e| (e.offset, e.key, e.on))
            .collect();
        assert_eq!(
            got,
            [
                (0, 5, true),
                (5, 3, false),
                (10, 1, true),
                (10, 1, false),
                (40, 2, true),
                (40, 2, false),
                (63, 4, true)
            ]
        );
        while n.push_event(ev(1, 9, true)) {}
        assert!(n.events_full());
        assert!(!n.push_event(ev(0, 9, false)));
    }

    /// Skipping silent blocks renders exactly what rendering them would: notes, fades that
    /// outlive their voice, fades started while silent, and mix changes while silent.
    #[test]
    fn skipping_silent_blocks_never_changes_the_output() {
        let (mut skip, mut full) = (node(false), node(true));
        let half = ChannelMix {
            volume: 0.5,
            pan: -0.3,
            ..ChannelMix::default()
        };
        let mut skipped = 0;
        for block in 0..200 {
            for n in [&mut skip, &mut full] {
                match block {
                    // A one-shot, faded out before it ends by itself.
                    0 => assert!(n.push_event(note(0))),
                    2 => n.fade_out(),
                    // Long after: plays in full.
                    75 => assert!(n.push_event(note(10))),
                    // AllSoundOff while silent, then a note inside its 5 ms.
                    100 => n.fade_out(),
                    101 => assert!(n.push_event(note(3))),
                    // A mix change while silent, then a note after its ramp.
                    140 => {
                        n.set_mix(half);
                        n.retarget(false);
                    }
                    141 => assert!(n.push_event(note(0))),
                    // A gain change while a note sounds; the note ends long before the gain
                    // settles, and a note after many silent blocks starts at the new gain.
                    160 => assert!(n.push_event(note(0))),
                    161 => n.set_param(GAIN, -20.0),
                    185 => assert!(n.push_event(note(0))),
                    _ => {}
                }
            }
            let (mut sl, mut sr) = ([0.0; BLOCK], [0.0; BLOCK]);
            let (mut fl, mut fr) = ([0.0; BLOCK], [0.0; BLOCK]);
            if !skip.render(&mut sl, &mut sr, &[]) {
                skipped += 1;
                sl = [0.0; BLOCK];
                sr = [0.0; BLOCK];
            }
            assert!(full.render(&mut fl, &mut fr, &[]));
            assert!(sl == fl && sr == fr, "block {block}");
        }
        assert!(skipped > 150, "the skip ran: {skipped}");
    }
}
