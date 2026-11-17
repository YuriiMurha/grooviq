//! The audio-thread half. It renders the graph in fixed 64-frame blocks and serves them to
//! callbacks of any size, so the output never depends on how the device splits it.
//!
//! Nothing reachable from [`AudioProcessor::process`] allocates, frees, locks or does I/O.
//! Everything removed or replaced (graphs, channels, samples, patterns) goes back to the
//! controller through the trash queue to be freed there. The
//! one exception is a panic in the graph: unwinding allocates and the panic hook may print, so
//! the processor catches it, frees its payload and stays silent until the graph is replaced.

use crate::command::{Command, Msg, Trash};
use crate::graph::Graph;
use crate::live::LiveShared;
use crate::metronome::Metronome;
use crate::record::{RecordEvent, Recorder};
use crate::transport::TransportClock;
use crate::{BLOCK, QUEUE_CAPACITY};
use grooviq_dsp::NoteEvent;
use rtrb::{Consumer, Producer, PushError};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

thread_local! {
    /// On while this thread is inside [`AudioProcessor::process`]. Const-initialised and
    /// without a destructor, so reading and writing it never allocates.
    static RENDERING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the calling thread is inside [`AudioProcessor::process`]. A panic hook calls it to
/// stay silent for a panic the engine catches on the audio thread, where printing is I/O.
///
/// This is also true during the off-stream render in `stop_sound` (the guard stop), which
/// calls [`AudioProcessor::process`] with no stream open, on the thread that looked at the
/// devices rather than the audio thread, so a panic caught there is silent too.
pub fn in_audio_callback() -> bool {
    RENDERING.with(Cell::get)
}

/// Turns [`RENDERING`] on until dropped.
struct Rendering;

impl Rendering {
    fn start() -> Self {
        RENDERING.with(|r| r.set(true));
        Self
    }
}

impl Drop for Rendering {
    fn drop(&mut self) {
        RENDERING.with(|r| r.set(false));
    }
}

/// Note events one block takes; more wait in the queue for the next block.
const MAX_EVENTS: usize = 256;
/// Meter release time constant.
const METER_RELEASE_SECONDS: f32 = 0.3;
/// Meter values below this read as silence (and never decay into denormals).
const METER_FLOOR: f32 = 1e-6;

const NO_EVENT: NoteEvent = NoteEvent {
    offset: 0,
    key: 0,
    velocity: 0.0,
    on: false,
    slide: false,
};

/// Renders audio on the audio thread. Created by [`crate::engine`].
pub struct AudioProcessor {
    commands: Consumer<Msg>,
    trash: Producer<Trash>,
    /// Something the full trash queue could not take yet; retried every block.
    unsent: Option<Trash>,
    graph: Box<Graph>,
    /// The generation of the last `restart`: replacements from earlier ones are stale.
    generation: u64,
    /// A panic unwound out of the graph: it stays untouched (and the output silent) until the
    /// controller sends a replacement.
    faulted: bool,
    live: Arc<LiveShared>,
    /// The last rendered block; `served` of its frames are already out (the FIFO between
    /// fixed blocks and callbacks of any size). `served == BLOCK` means empty.
    block_l: [f32; BLOCK],
    block_r: [f32; BLOCK],
    served: usize,
    events: [NoteEvent; MAX_EVENTS],
    /// First frame of the next block: the engine's sample clock.
    clock: u64,
    /// Play state and tempo. Owned here, not by the graph, so it survives graph swaps, as do
    /// the recording tap and the metronome.
    transport: TransportClock,
    recorder: Recorder,
    metronome: Metronome,
    faults: u32,
    guard_hits: u32,
    meter: [f32; 2],
    meter_release: f32,
}

impl AudioProcessor {
    pub(crate) fn new(
        sample_rate: f32,
        (bpm, time_signature): (f32, (u8, u8)),
        graph: Box<Graph>,
        (commands, trash, recorded): (Consumer<Msg>, Producer<Trash>, Producer<RecordEvent>),
        live: Arc<LiveShared>,
    ) -> Self {
        Self {
            commands,
            trash,
            unsent: None,
            generation: graph.generation(),
            graph,
            faulted: false,
            live,
            block_l: [0.0; BLOCK],
            block_r: [0.0; BLOCK],
            served: BLOCK,
            events: [NO_EVENT; MAX_EVENTS],
            clock: 0,
            transport: TransportClock::new(sample_rate, bpm),
            recorder: Recorder::new(recorded),
            metronome: Metronome::new(sample_rate, time_signature),
            faults: 0,
            guard_hits: 0,
            meter: [0.0; 2],
            meter_release: meter_release(sample_rate),
        }
    }

    /// Fills `out`, interleaved with `channels` channels, of any length. Channels 0 and 1 get
    /// left and right (a single channel gets their average); further channels and a trailing
    /// partial frame get silence. Real-time safe.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let _rendering = Rendering::start();
        if channels == 0 {
            out.fill(0.0);
            return;
        }
        let mut frames = out.chunks_exact_mut(channels);
        for frame in &mut frames {
            if self.served == BLOCK {
                self.render_block();
            }
            let (l, r) = (self.block_l[self.served], self.block_r[self.served]);
            self.served += 1;
            match frame {
                [mono] => *mono = 0.5 * (l + r),
                [a, b, rest @ ..] => {
                    *a = l;
                    *b = r;
                    rest.fill(0.0);
                }
                [] => {}
            }
        }
        frames.into_remainder().fill(0.0);
    }

    /// Restarts with `graph`, built for `sample_rate` in a new generation. Only
    /// [`crate::Controller::reconfigure`] calls this, with the audio callback stopped, so the
    /// old graph is freed on its thread. The transport keeps playing from the tick it reached.
    pub(crate) fn restart(&mut self, sample_rate: f32, graph: Box<Graph>) {
        self.transport.set_sample_rate(self.clock, sample_rate);
        self.metronome.set_sample_rate(sample_rate);
        // A pattern of a new length can arrive with the fresh graph (its swap was still queued,
        // or a faulted graph refused it): the place in the loop holds, as in `replace_graph`.
        self.transport
            .keep_position(self.graph.pattern_length(), graph.pattern_length());
        self.generation = graph.generation();
        self.graph = graph;
        self.unsent = None;
        self.faulted = false;
        // Leftover frames were rendered at the old rate.
        self.served = BLOCK;
        self.meter_release = meter_release(sample_rate);
    }

    /// Takes every queued message out of the queue, in order. Only
    /// [`crate::Controller::reconfigure`] calls this, with the audio callback stopped
    /// (allocates, and what it returns is freed on the caller's thread).
    pub(crate) fn take_queued(&mut self) -> Vec<Msg> {
        let mut queued = Vec::with_capacity(self.commands.slots());
        while let Ok(msg) = self.commands.pop() {
            queued.push(msg);
        }
        queued
    }

    fn render_block(&mut self) {
        let start = self.clock;
        let end = start + BLOCK as u64;
        self.retry_trash();
        let count = self.drain(start, end);
        if !self.faulted {
            let Self {
                graph,
                block_l,
                block_r,
                events,
                transport,
                ..
            } = self;
            match catch_unwind(AssertUnwindSafe(|| {
                graph.sequence(transport, start);
                graph.render(block_l, block_r, &events[..count])
            })) {
                Ok(silenced) => {
                    self.guard_hits = self.guard_hits.wrapping_add(silenced);
                    self.metronome.render(
                        &mut self.block_l,
                        &mut self.block_r,
                        &self.transport,
                        start,
                    );
                    self.collect_departed();
                }
                // Dropping the payload frees what the panic allocated: the one path that
                // touches the allocator, since unwinding itself allocates.
                Err(_payload) => self.fault(),
            }
        }
        if self.faulted {
            self.block_l = [0.0; BLOCK];
            self.block_r = [0.0; BLOCK];
        }
        // Before the next block's commands can change the tick-to-frame map: a sequencer that
        // re-seeks there starts from the first tick this block did not cover.
        self.transport.mark_covered(end);
        self.update_meter();
        self.clock = end;
        self.served = 0;
        let tone = !self.faulted && self.graph.tone_on();
        self.live
            .publish(self.meter, tone, self.clock, self.faults, self.guard_hits);
        self.live.publish_transport(
            self.transport.playing(),
            self.transport.position_ticks(start),
        );
    }

    /// Applies, in queue order, the commands due before `end` (the block covers `start..end`).
    /// Notes become events at their frame's offset in the block (never before an earlier
    /// note): synth notes in `events`, channel notes in their channel's buffer. Every other
    /// command applies now, at the block start. Returns the synth's event count.
    fn drain(&mut self, start: u64, end: u64) -> usize {
        let mut count = 0;
        let mut last_offset = 0;
        // Bounded: a producer that keeps pushing cannot hold the audio thread here.
        for _ in 0..QUEUE_CAPACITY {
            let Ok(next) = self.commands.peek() else {
                break;
            };
            let wait = match next {
                // Not due yet, and nothing behind it may overtake it.
                Msg::Command { due, .. } if *due >= end => true,
                Msg::Command {
                    cmd: Command::Note { .. },
                    ..
                } => count == MAX_EVENTS,
                // A full channel drops a note-on, but a note-off must not be lost.
                Msg::Command {
                    cmd:
                        Command::ChannelNote {
                            channel, on: false, ..
                        },
                    ..
                } => !self.faulted && self.graph.channel_events_full(*channel),
                Msg::Command { .. } => false,
                // What it replaces or removes needs a free spot to go to.
                other => other.makes_trash() && self.unsent.is_some(),
            };
            if wait {
                break;
            }
            let Ok(msg) = self.commands.pop() else {
                break;
            };
            // `due < end`, so the offset is below BLOCK.
            let mut offset = |due: u64| {
                let offset = (due.saturating_sub(start) as u32).max(last_offset);
                last_offset = offset;
                offset
            };
            match msg {
                Msg::ReplaceGraph(graph) => self.replace_graph(graph),
                Msg::AddChannel(node) => {
                    let refused = if self.faulted {
                        Some(node)
                    } else {
                        self.graph.add_channel(node)
                    };
                    if let Some(node) = refused {
                        self.discard(Trash::Channel(node));
                    }
                }
                // A removed channel fades out first: `collect_departed` hands it back after.
                Msg::RemoveChannel(id) => {
                    if !self.faulted
                        && let Some(node) = self.graph.remove_channel(id)
                    {
                        self.discard(Trash::Channel(node));
                    }
                }
                Msg::SetSample { channel, sample } => {
                    let leaving = if self.faulted {
                        sample
                    } else {
                        self.graph.set_channel_sample(channel, sample)
                    };
                    if let Some(sample) = leaving {
                        self.discard(Trash::Sample(sample));
                    }
                }
                Msg::SetPattern(pattern) => {
                    let leaving = if self.faulted {
                        pattern
                    } else {
                        let new_len = pattern.length;
                        let leaving = self.graph.set_pattern(pattern);
                        // A new length keeps the place in the loop (no jump, no lost notes).
                        self.transport.keep_position(leaving.length, new_len);
                        leaving
                    };
                    self.discard(Trash::Pattern(leaving));
                }
                Msg::Command { due, cmd } => match cmd {
                    Command::Note { key, velocity, on } => {
                        if !self.faulted {
                            self.events[count] = NoteEvent {
                                offset: offset(due),
                                key,
                                velocity,
                                on,
                                slide: false,
                            };
                            count += 1;
                        }
                    }
                    Command::ChannelNote {
                        channel,
                        key,
                        velocity,
                        on,
                    } => {
                        if !self.faulted {
                            let event = NoteEvent {
                                offset: offset(due),
                                key,
                                velocity,
                                on,
                                slide: false,
                            };
                            if self.graph.channel_note(channel, event) {
                                let at = start + u64::from(event.offset);
                                let note = (channel, key, velocity, on);
                                self.recorder.note(&self.transport, at, &self.live, note);
                            }
                        }
                    }
                    Command::SetChannelMix { channel, mix } => {
                        self.with_graph(|g| g.set_channel_mix(channel, mix));
                    }
                    Command::SetSamplerParam {
                        channel,
                        index,
                        value,
                    } => {
                        self.with_graph(|g| g.set_sampler_param(channel, index, value));
                    }
                    Command::SetChannelParam {
                        channel,
                        index,
                        value,
                    } => {
                        self.with_graph(|g| g.set_channel_param(channel, index, value));
                    }
                    Command::SetChoke { channel, group } => {
                        self.with_graph(|g| g.set_choke(channel, group));
                    }
                    Command::Play => {
                        if !self.transport.playing() {
                            self.play(start, None);
                        }
                    }
                    Command::PlayLoops { loops } => self.play(start, Some(loops)),
                    Command::Stop => self.stop(start),
                    Command::SetTempo { bpm } => self.transport.set_bpm(start, bpm),
                    Command::AllSoundOff => {
                        self.stop(start);
                        self.with_graph(Graph::all_sound_off);
                    }
                    Command::SetRecording { on } => {
                        self.recorder.set(on, &self.transport, start, &self.live);
                    }
                    Command::SetMetronome { on } => self.metronome.set_on(on),
                    Command::SetTimeSignature { beats, unit } => {
                        self.metronome.set_time_signature(beats, unit);
                    }
                    Command::TestTone { on, hz, gain_db } => {
                        self.with_graph(|g| g.set_tone(on, hz, gain_db));
                    }
                    Command::SetSynthParam { index, value } => {
                        self.with_graph(|g| g.set_synth_param(index, value));
                    }
                    #[cfg(feature = "test-hooks")]
                    Command::InjectNaN => self.with_graph(Graph::inject_nan),
                    #[cfg(feature = "test-hooks")]
                    Command::InjectPanic => self.with_graph(Graph::inject_panic),
                },
            }
        }
        count
    }

    /// Plays from tick 0 at frame `at` (a block start). While playing, the notes sounding are
    /// released first, and a take recording ends.
    fn play(&mut self, at: u64, loops: Option<u32>) {
        if self.transport.playing() {
            self.recorder.end(&self.transport, at, &self.live);
            self.with_graph(Graph::transport_stopped);
        }
        self.transport.start(at, loops);
        self.with_graph(Graph::transport_started);
    }

    /// Stops and rewinds at frame `at` (a block start); the sequenced notes sounding are
    /// released there, and a take recording ends there.
    fn stop(&mut self, at: u64) {
        self.recorder.end(&self.transport, at, &self.live);
        self.transport.stop();
        self.with_graph(Graph::transport_stopped);
    }

    /// Hands the removed channels whose fade ended to the trash queue, while it has room (one
    /// more is parked when it is full; the rest wait in the graph, silent).
    fn collect_departed(&mut self) {
        while self.unsent.is_none() {
            let Some(node) = self.graph.take_departed() else {
                break;
            };
            self.discard(Trash::Channel(node));
        }
    }

    /// Runs `f` on the graph unless it is faulted, catching a panic like a render does.
    fn with_graph(&mut self, f: impl FnOnce(&mut Graph)) {
        if self.faulted {
            return;
        }
        let graph: &mut Graph = &mut self.graph;
        if catch_unwind(AssertUnwindSafe(|| f(graph))).is_err() {
            self.fault();
        }
    }

    fn fault(&mut self) {
        self.faulted = true;
        self.faults = self.faults.wrapping_add(1);
    }

    /// Swaps in a fresh graph, unless it is from an earlier generation: a restart queued
    /// before `reconfigure` arrives after the fresh graph it made, and must not replace it
    /// (cutting held voices, or bringing back the old rate).
    fn replace_graph(&mut self, graph: Box<Graph>) {
        let leaving = if graph.generation() == self.generation {
            self.faulted = false;
            let new_len = graph.pattern_length();
            let leaving = std::mem::replace(&mut self.graph, graph);
            // A pattern swap a faulted graph refused arrives with the fresh graph.
            self.transport
                .keep_position(leaving.pattern_length(), new_len);
            leaving
        } else {
            graph
        };
        self.discard(Trash::Graph(leaving));
    }

    /// Hands an item back to the controller, or parks it in `unsent` (free, as `drain`
    /// checked) while the trash queue is full. Never frees it here.
    fn discard(&mut self, item: Trash) {
        if let Err(PushError::Full(item)) = self.trash.push(item) {
            debug_assert!(self.unsent.is_none());
            self.unsent = Some(item);
        }
    }

    fn retry_trash(&mut self) {
        if let Some(old) = self.unsent.take()
            && let Err(PushError::Full(old)) = self.trash.push(old)
        {
            self.unsent = Some(old);
        }
    }

    /// Instant attack, exponential release, once per block.
    fn update_meter(&mut self) {
        for (meter, block) in self.meter.iter_mut().zip([&self.block_l, &self.block_r]) {
            let peak = block.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            let held = *meter * self.meter_release;
            *meter = if peak >= held {
                peak
            } else if held < METER_FLOOR {
                0.0
            } else {
                held
            };
        }
    }
}

/// Per-block meter decay factor for `sample_rate`.
fn meter_release(sample_rate: f32) -> f32 {
    libm::expf(-(BLOCK as f32) / (METER_RELEASE_SECONDS * sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{CHANNEL_EVENTS, ChannelNode};
    use crate::graph::ramp_len;
    use crate::state::{ChannelId, ChannelMix, ChannelSetup, PatternData};
    use grooviq_dsp::sampler::SampleBuf;

    const SR: f32 = 48_000.0;

    /// A processor with a one-slot trash queue, and the raw ends of both queues.
    fn processor() -> (AudioProcessor, Producer<Msg>, Consumer<Trash>) {
        let (commands, commands_rx) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (trash_tx, trash) = rtrb::RingBuffer::new(1);
        let graph = Box::new(Graph::new(SR, 0));
        let live = Arc::new(LiveShared::default());
        let (recorded, _) = rtrb::RingBuffer::new(1);
        let queues = (commands_rx, trash_tx, recorded);
        let p = AudioProcessor::new(SR, (140.0, (4, 4)), graph, queues, live);
        (p, commands, trash)
    }

    #[test]
    fn a_full_trash_queue_parks_the_old_graph_until_there_is_room() {
        let (mut p, mut commands, mut trash) = processor();
        for _ in 0..3 {
            let msg = Msg::ReplaceGraph(Box::new(Graph::new(SR, 0)));
            assert!(commands.push(msg).is_ok());
        }
        let mut buf = [0.0; BLOCK * 2];
        p.process(&mut buf, 2);
        // First swap: the old graph fills the trash queue. Second: its old graph is parked.
        // Third: no spot left, so it waits in the command queue.
        assert_eq!(trash.slots(), 1);
        assert!(p.unsent.is_some());
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 1);

        // The controller frees one: the parked graph moves into the trash queue, the third
        // swap happens and its old graph is parked in turn.
        assert!(trash.pop().is_ok());
        p.process(&mut buf, 2);
        assert_eq!(trash.slots(), 1);
        assert!(p.unsent.is_some());
        assert_eq!(commands.slots(), QUEUE_CAPACITY);

        assert!(trash.pop().is_ok());
        p.process(&mut buf, 2);
        assert_eq!(trash.slots(), 1);
        assert!(p.unsent.is_none());
    }

    fn sample() -> Arc<SampleBuf> {
        Arc::new(SampleBuf {
            rate: SR,
            l: vec![0.5; 4_800],
            r: vec![0.5; 4_800],
        })
    }

    fn node(id: u32) -> Box<ChannelNode> {
        let setup = ChannelSetup::sampler(
            ChannelId(id),
            Some(sample()),
            Vec::new(),
            ChannelMix::default(),
        )
        .checked()
        .unwrap();
        Box::new(ChannelNode::new(SR, ramp_len(SR), &setup, false))
    }

    fn cmd(cmd: Command) -> Msg {
        Msg::Command { due: 0, cmd }
    }

    #[test]
    fn every_message_that_makes_trash_waits_while_an_item_is_parked() {
        let (mut p, mut commands, mut trash) = processor();
        let pattern = |length| {
            Box::new(PatternData {
                length,
                notes: Vec::new(),
            })
        };
        let new_sample = sample();
        let msgs = [
            Msg::AddChannel(node(1)),
            Msg::SetPattern(pattern(10)), // the old pattern fills the trash queue
            Msg::SetPattern(pattern(20)), // and this one's is parked
            Msg::SetSample {
                channel: ChannelId(1),
                sample: Some(Arc::clone(&new_sample)),
            },
            Msg::RemoveChannel(ChannelId(1)),
            Msg::SetPattern(pattern(30)),
        ];
        for msg in msgs {
            assert!(commands.push(msg).is_ok());
        }
        let mut buf = [0.0; BLOCK * 2];
        p.process(&mut buf, 2);
        assert_eq!(p.graph.pattern().length, 20);
        assert!(p.unsent.is_some());
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 3, "the new sample waits");
        assert_eq!(p.graph.channel_ids(), [ChannelId(1)]);

        // Each free lets one more through: the sample swap, whose old sample is parked...
        assert!(trash.pop().is_ok());
        p.process(&mut buf, 2);
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 2, "the removal waits");
        assert!(matches!(trash.pop(), Ok(Trash::Pattern(_))));
        // ...the removal, which only starts the channel's 5 ms fade, and the last pattern,
        // whose old one is parked.
        p.process(&mut buf, 2);
        assert!(p.graph.channel_ids().is_empty());
        assert_eq!(p.graph.pattern().length, 30);
        assert_eq!(commands.slots(), QUEUE_CAPACITY);
        assert!(matches!(trash.pop(), Ok(Trash::Sample(_))));
        // The parked pattern moves into the queue and stays there while the channel fades.
        p.process(&mut buf, 2);
        p.process(&mut buf, 2);
        assert!(p.unsent.is_none());
        // The fade (240 frames) ends in the fourth block: the full queue parks the node.
        p.process(&mut buf, 2);
        assert!(matches!(p.unsent, Some(Trash::Channel(_))));
        assert!(matches!(trash.pop(), Ok(Trash::Pattern(_))));
        p.process(&mut buf, 2);
        assert!(matches!(trash.pop(), Ok(Trash::Channel(_))));
        assert_eq!(Arc::strong_count(&new_sample), 1, "it left with the node");
        assert!(p.unsent.is_none());
    }

    #[test]
    fn what_a_faulted_graph_or_a_missing_channel_cannot_take_comes_back() {
        // A trash queue with room for everything, so nothing is parked.
        let (mut commands, commands_rx) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let (trash_tx, mut trash) = rtrb::RingBuffer::new(8);
        let live = Arc::new(LiveShared::default());
        let graph = Box::new(Graph::new(SR, 0));
        let (recorded, _) = rtrb::RingBuffer::new(1);
        let queues = (commands_rx, trash_tx, recorded);
        let mut p = AudioProcessor::new(SR, (140.0, (4, 4)), graph, queues, live);
        let held = sample();
        let mut buf = [0.0; BLOCK * 2];
        let mut back = |p: &mut AudioProcessor, msg: Msg| {
            assert!(commands.push(msg).is_ok());
            p.process(&mut buf, 2);
            trash.pop().ok()
        };

        // A healthy graph: a duplicate id and a missing channel give the payload back.
        assert!(back(&mut p, Msg::AddChannel(node(1))).is_none());
        let dup = back(&mut p, Msg::AddChannel(node(1)));
        assert!(matches!(dup, Some(Trash::Channel(_))), "duplicate id");
        let missing = Msg::SetSample {
            channel: ChannelId(7),
            sample: Some(Arc::clone(&held)),
        };
        let returned = back(&mut p, missing);
        assert!(
            matches!(returned, Some(Trash::Sample(_))),
            "missing channel"
        );
        assert_eq!(
            Arc::strong_count(&held),
            2,
            "alive until the controller frees it"
        );
        drop(returned);
        assert_eq!(Arc::strong_count(&held), 1);

        // A faulted graph takes nothing: every payload comes back.
        p.fault();
        let added = back(&mut p, Msg::AddChannel(node(2)));
        assert!(
            matches!(added, Some(Trash::Channel(_))),
            "added while faulted"
        );
        let set = Msg::SetSample {
            channel: ChannelId(1),
            sample: Some(Arc::clone(&held)),
        };
        let returned = back(&mut p, set);
        assert!(
            matches!(returned, Some(Trash::Sample(_))),
            "sample while faulted"
        );
        assert_eq!(Arc::strong_count(&held), 2);
        let pattern = Msg::SetPattern(Box::default());
        assert!(matches!(back(&mut p, pattern), Some(Trash::Pattern(_))));
        assert!(back(&mut p, Msg::RemoveChannel(ChannelId(1))).is_none());
        assert_eq!(
            p.graph.channel_ids(),
            [ChannelId(1)],
            "the faulted graph is untouched"
        );
    }

    #[test]
    fn a_full_channel_drops_note_ons_and_holds_note_offs() {
        let (mut p, mut commands, _trash) = processor();
        assert!(commands.push(Msg::AddChannel(node(1))).is_ok());
        let note = |on| {
            cmd(Command::ChannelNote {
                channel: ChannelId(1),
                key: 60,
                velocity: 1.0,
                on,
            })
        };
        // One block more than the buffer holds, then a note-off.
        for _ in 0..CHANNEL_EVENTS + 1 {
            assert!(commands.push(note(true)).is_ok());
        }
        assert!(commands.push(note(false)).is_ok());
        assert!(commands.push(cmd(Command::Play)).is_ok());
        let mut buf = [0.0; BLOCK * 2];
        p.process(&mut buf, 2);
        // The extra note-on was dropped; the note-off and everything behind it wait.
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 2);
        assert!(!p.transport.playing());
        p.process(&mut buf, 2);
        assert_eq!(commands.slots(), QUEUE_CAPACITY);
        assert!(p.transport.playing());
    }

    #[test]
    fn the_rendering_flag_is_off_outside_process() {
        let (mut p, _commands, _trash) = processor();
        assert!(!in_audio_callback());
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert!(!in_audio_callback());
    }

    #[test]
    fn a_restart_queued_before_reconfigure_is_discarded() {
        let (mut p, mut commands, mut trash) = processor();
        // check_health queued a graph in generation 0, then reconfigure started generation 1
        // at the same rate.
        assert!(
            commands
                .push(Msg::ReplaceGraph(Box::new(Graph::new(SR, 0))))
                .is_ok()
        );
        p.restart(SR, Box::new(Graph::new(SR, 1)));
        let mut buf = [0.0; BLOCK * 2];
        p.process(&mut buf, 2);
        assert_eq!(p.graph.generation(), 1);
        // The stale graph goes back to be freed off the audio thread.
        let Ok(Trash::Graph(stale)) = trash.pop() else {
            panic!("a graph handed back");
        };
        assert_eq!(stale.generation(), 0);
    }
}
