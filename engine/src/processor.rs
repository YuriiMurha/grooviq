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
    playing: bool,
    faults: u32,
    guard_hits: u32,
    meter: [f32; 2],
    meter_release: f32,
}

impl AudioProcessor {
    pub(crate) fn new(
        sample_rate: f32,
        graph: Box<Graph>,
        commands: Consumer<Msg>,
        trash: Producer<Trash>,
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
            playing: false,
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
    /// old graph is freed on its thread.
    pub(crate) fn restart(&mut self, sample_rate: f32, graph: Box<Graph>) {
        self.generation = graph.generation();
        self.graph = graph;
        self.unsent = None;
        self.faulted = false;
        // Leftover frames were rendered at the old rate.
        self.served = BLOCK;
        self.meter_release = meter_release(sample_rate);
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
                ..
            } = self;
            match catch_unwind(AssertUnwindSafe(|| {
                graph.render(block_l, block_r, &events[..count])
            })) {
                Ok(silenced) => self.guard_hits = self.guard_hits.wrapping_add(silenced),
                // Dropping the payload frees what the panic allocated: the one path that
                // touches the allocator, since unwinding itself allocates.
                Err(_payload) => self.fault(),
            }
        }
        if self.faulted {
            self.block_l = [0.0; BLOCK];
            self.block_r = [0.0; BLOCK];
        }
        self.update_meter();
        self.clock = end;
        self.served = 0;
        let tone = !self.faulted && self.graph.tone_on();
        self.live.publish(
            self.meter,
            self.playing,
            tone,
            self.clock,
            self.faults,
            self.guard_hits,
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
                        self.graph.set_pattern(pattern)
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
                            self.graph.channel_note(channel, event);
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
                    Command::Play => self.playing = true,
                    Command::Stop => self.playing = false,
                    Command::AllSoundOff => {
                        self.playing = false;
                        self.with_graph(Graph::all_sound_off);
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
            std::mem::replace(&mut self.graph, graph)
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
    (-(BLOCK as f32) / (METER_RELEASE_SECONDS * sample_rate)).exp()
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
        let p = AudioProcessor::new(SR, graph, commands_rx, trash_tx, live);
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

    fn node(id: u32) -> Box<ChannelNode> {
        let sample = SampleBuf {
            rate: SR,
            l: vec![0.5; 48_000],
            r: vec![0.5; 48_000],
        };
        let setup = ChannelSetup {
            id: ChannelId(id),
            sample: Some(Arc::new(sample)),
            sampler: Vec::new(),
            mix: ChannelMix::default(),
        }
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
        let msgs = [
            Msg::AddChannel(node(1)),
            Msg::SetPattern(pattern(10)), // the old pattern fills the trash queue
            Msg::SetPattern(pattern(20)), // and this one's is parked
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
        assert_eq!(p.graph.channel_ids(), [ChannelId(1)], "the removal waits");
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 2);

        // Each free lets one more through.
        assert!(trash.pop().is_ok());
        p.process(&mut buf, 2);
        assert!(p.graph.channel_ids().is_empty());
        assert!(matches!(trash.pop(), Ok(Trash::Pattern(_))));
        p.process(&mut buf, 2);
        assert!(matches!(trash.pop(), Ok(Trash::Channel(_))));
        assert_eq!(p.graph.pattern().length, 30);
        assert_eq!(commands.slots(), QUEUE_CAPACITY);
        // The last old pattern was parked, and goes once there is room.
        p.process(&mut buf, 2);
        assert!(matches!(trash.pop(), Ok(Trash::Pattern(_))));
        assert!(p.unsent.is_none());
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
        assert!(!p.playing);
        p.process(&mut buf, 2);
        assert_eq!(commands.slots(), QUEUE_CAPACITY);
        assert!(p.playing);
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
