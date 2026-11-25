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
    id: 0,
};

type HostSignature = (bool, u64, u64, u64, u64, u64, u64, u64);

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
    stream_generation: u64,
    host_revision: u64,
    host_last: Option<HostSignature>,
    /// Play state and tempo. Owned here, not by the graph, so it survives graph swaps, as do
    /// the recording tap and the metronome.
    transport: TransportClock,
    recorder: Recorder,
    metronome: Metronome,
    successful_blocks: u32,
    faults: u32,
    guard_hits: u32,
    meter: [f32; 2],
    meter_release: f32,
    completed_recovery_stop: Option<u32>,
}

impl AudioProcessor {
    pub(crate) fn new(
        sample_rate: f32,
        (bpm, time_signature): (f32, (u8, u8)),
        graph: Box<Graph>,
        (commands, trash, recorded): (Consumer<Msg>, Producer<Trash>, Producer<RecordEvent>),
        live: Arc<LiveShared>,
    ) -> Self {
        let mut transport = TransportClock::new(sample_rate, bpm);
        let length = graph.pattern_length();
        transport.keep_position(0, length, length);
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
            stream_generation: 0,
            host_revision: 0,
            host_last: None,
            transport,
            recorder: Recorder::new(recorded),
            metronome: Metronome::new(sample_rate, time_signature),
            successful_blocks: 0,
            faults: 0,
            guard_hits: 0,
            meter: [0.0; 2],
            meter_release: meter_release(sample_rate),
            completed_recovery_stop: None,
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
        self.stream_generation = self.stream_generation.wrapping_add(1);
        self.transport.set_sample_rate(self.clock, sample_rate);
        self.metronome.set_sample_rate(sample_rate);
        // A pattern of a new length can arrive with the fresh graph (its swap was still queued,
        // or a faulted graph refused it): the place in the loop holds, as in `replace_graph`.
        if graph.playback_mode == crate::PlaybackMode::Pattern {
            self.transport.keep_position(
                self.clock,
                self.graph.pattern_length(),
                graph.pattern_length(),
            );
        } else {
            let len = graph.pattern_length();
            self.transport.keep_position(self.clock, len, len);
        }
        self.generation = graph.generation();
        let mut graph = graph;
        // Silent faulted callbacks advance transport without advancing clip progress.
        if !self.faulted {
            graph.keep_audio(&self.graph);
        }
        self.graph = graph;
        self.unsent = None;
        self.successful_blocks = 0;
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
        let region = self.graph.song_loop;
        let signature = (
            self.transport.playing(),
            self.transport.revision(),
            region.map_or(0, |r| r.start_tick),
            self.graph.generation(),
            region.map_or(0, |r| r.end_tick),
            self.graph.playback_mode as u64,
            self.transport.shift(),
            self.graph.host_epoch,
        );
        let changed = self.host_last != Some(signature);
        if changed {
            self.host_revision = self.host_revision.wrapping_add(1);
            self.host_last = Some(signature);
        }
        let region = if self.graph.playback_mode == crate::PlaybackMode::Song {
            self.graph.song_loop.or(Some(crate::SongLoop {
                start_tick: 0,
                end_tick: 0,
            }))
        } else {
            None
        };
        self.graph.set_host(self.transport.host(
            start,
            self.stream_generation,
            self.host_revision,
            changed,
            region,
        ));
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
                graph.automate(transport, start);
                graph.sequence(transport, start);
                graph.render(block_l, block_r, &events[..count])
            })) {
                Ok(silenced) => {
                    self.successful_blocks = self.successful_blocks.saturating_add(1);
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
        if self.graph.playback_mode == crate::PlaybackMode::Song
            && self.graph.song_loop.is_none()
            && self.transport.playing()
            && !self.graph.audio_active()
            && end
                >= self
                    .transport
                    .frame_of(self.graph.song().end_tick as f64)
                    .max(self.transport.song_frame_of(
                        self.graph.song().end_frame_at(self.transport.sample_rate()),
                    ))
        {
            self.transport.finish(end, self.graph.song().end_tick);
        }
        self.update_meter();
        self.clock = end;
        self.served = 0;
        self.live
            .publish_recovery(self.faults, self.successful_blocks);
        let tone = !self.faulted && self.graph.tone_on();
        self.live
            .publish(self.meter, tone, self.clock, self.faults, self.guard_hits);
        // At most one fixed-size capture per 1024 rendered frames; no work when unleased.
        if self.clock.is_multiple_of(1024) {
            let instance = self.live.synth.selected(self.clock);
            let meter = if instance != 0 && !self.faulted {
                self.graph.synth_meter(instance)
            } else {
                None
            };
            self.live.synth.publish(meter.as_ref());
        }
        self.live.publish_transport(
            self.transport.playing(),
            self.graph.position(self.transport.position_ticks(start)),
        );
    }

    /// Applies, in queue order, the commands due before `end` (the block covers `start..end`).
    /// Notes become events at their frame's offset in the block (never before an earlier
    /// note): synth notes in `events`, channel notes in their channel's buffer. Every other
    /// command applies now, at the block start. Returns the synth's event count.
    fn drain(&mut self, start: u64, end: u64) -> usize {
        self.drain_commands(start, end, false)
    }

    fn drain_commands(&mut self, start: u64, end: u64, detached: bool) -> usize {
        // First: no event recorded in this block may go ahead of a waiting barrier.
        self.recorder.flush_mark();
        let mut count = 0;
        let mut last_offset = 0;
        // Bounded: a producer that keeps pushing cannot hold the audio thread here.
        for _ in 0..if detached { 1 } else { QUEUE_CAPACITY } {
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
            if wait && !detached {
                break;
            }
            let Ok(msg) = self.commands.pop() else {
                break;
            };
            // Exhaustion supersedes starts accepted before its transaction. A later
            // explicit start stays queued because settlement stops at RecoveryStop.
            if detached
                && matches!(
                    msg,
                    Msg::Command {
                        cmd: Command::Play
                            | Command::PlayLoops { .. }
                            | Command::SetRecording { on: true },
                        ..
                    }
                )
            {
                continue;
            }
            // `due < end`, so the offset is below BLOCK.
            let mut offset = |due: u64| {
                let offset = (due.saturating_sub(start) as u32).max(last_offset);
                last_offset = offset;
                offset
            };
            match msg {
                Msg::ReplaceGraph(graph) => self.replace_graph(graph, start),
                Msg::RecordMark(id) => self.recorder.mark(id),
                Msg::RecoveryStop(id) => {
                    self.recorder.mark(id);
                    self.completed_recovery_stop = Some(id);
                }
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
                Msg::TimelineRebuilt {
                    bpm,
                    mode,
                    region,
                    mode_changed,
                    loop_changed,
                } => {
                    if !self.faulted || detached {
                        self.timeline_transport(
                            start,
                            bpm,
                            mode,
                            region,
                            mode_changed,
                            loop_changed,
                        );
                    }
                }
                Msg::Timeline(mut update, mode_changed, loop_changed) => {
                    let mut removed = None;
                    if !self.faulted {
                        update = self.swap_timeline(start, update);
                        if let Some(id) = update.remove_channel {
                            removed = self.graph.remove_channel(id);
                        }
                        self.timeline_transport(
                            start,
                            update.bpm,
                            update.mode,
                            update.region,
                            mode_changed,
                            loop_changed,
                        );
                    }
                    // Even a faulted graph must retain accepted transport chronology.
                    // DSP payload mutations stay guarded, but its stop/disarm precedes
                    // any subsequent seek and the exhaustion completion tag.
                    else if detached {
                        self.timeline_transport(
                            start,
                            update.bpm,
                            update.mode,
                            update.region,
                            mode_changed,
                            loop_changed,
                        );
                    }
                    self.discard(Trash::Timeline(update, removed));
                }
                Msg::Sources(mut t) => {
                    if !self.faulted && self.graph.sources_admissible(&t) {
                        // One boundary: sources, timeline, then the mix that names them.
                        self.graph.apply_sources(&mut t);
                        if let Some((update, mode_changed, loop_changed)) = t.timeline.take() {
                            let update = self.swap_timeline(start, update);
                            self.timeline_transport(
                                start,
                                update.bpm,
                                update.mode,
                                update.region,
                                mode_changed,
                                loop_changed,
                            );
                            t.timeline = Some((update, mode_changed, loop_changed));
                        }
                        self.graph.apply_transaction_mix(&mut t);
                        t.applied = true;
                    } else if detached
                        && let Some((update, mode_changed, loop_changed)) = &t.timeline
                    {
                        // As for a timeline: a faulted graph keeps accepted transport order.
                        let (bpm, mode, region) = (update.bpm, update.mode, update.region);
                        let (m, l) = (*mode_changed, *loop_changed);
                        self.timeline_transport(start, bpm, mode, region, m, l);
                    }
                    self.discard(Trash::Sources(t));
                }
                Msg::SetMix(mut install) => {
                    if !self.faulted {
                        self.graph.install_mix(&mut install);
                    }
                    self.discard(Trash::Mix(install));
                }
                Msg::SetSong(song) => {
                    let old = if self.faulted {
                        song
                    } else {
                        self.graph.set_song(song)
                    };
                    self.discard(Trash::Song(old));
                }
                Msg::SetPattern(pattern) => {
                    let leaving = if self.faulted {
                        pattern
                    } else {
                        let new_len = pattern.length;
                        let leaving = self.graph.set_pattern(pattern);
                        // A new length keeps the place in the loop (no jump, no lost notes).
                        if self.graph.playback_mode == crate::PlaybackMode::Pattern {
                            self.transport.keep_position(start, leaving.length, new_len);
                        } else {
                            self.transport.keep_position(start, new_len, new_len);
                        }
                        leaving
                    };
                    self.discard(Trash::Pattern(leaving));
                }
                Msg::Command {
                    due,
                    cmd,
                    mode_changed,
                } => match cmd {
                    Command::Note {
                        key,
                        velocity,
                        on,
                        id,
                    } => {
                        if !self.faulted && !detached {
                            self.events[count] = NoteEvent {
                                offset: offset(due),
                                key,
                                velocity,
                                on,
                                slide: false,
                                id: id.0,
                            };
                            count += 1;
                        }
                    }
                    Command::ChannelNote {
                        channel,
                        key,
                        velocity,
                        on,
                        id,
                    } => {
                        if !self.faulted && !detached {
                            let event = NoteEvent {
                                offset: offset(due),
                                key,
                                velocity,
                                on,
                                slide: false,
                                id: id.0,
                            };
                            // A note-on a full channel drops is never heard, and its note-off,
                            // naming a note the instrument never started, ends nothing.
                            if self.graph.channel_note(channel, event) {
                                let at = start + u64::from(event.offset);
                                let note = (channel, key, velocity, on, id);
                                if self.graph.playback_mode == crate::PlaybackMode::Pattern {
                                    self.recorder.note(&self.transport, at, &self.live, note);
                                }
                            }
                        }
                    }
                    Command::SetChannelMix { channel, mix } => {
                        self.with_graph(|g| g.set_channel_mix(channel, mix));
                    }
                    Command::SetSamplerTrim {
                        channel,
                        instance,
                        params,
                    } => {
                        self.with_graph(|g| g.set_sampler_trim(channel, instance, &params));
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
                    Command::Seek { tick } => {
                        self.with_graph(Graph::transport_stopped);
                        self.transport.seek(start, tick);
                    }
                    Command::SetPlaybackMode { mode } => {
                        if mode_changed {
                            self.recorder.set(false, &self.transport, start, &self.live);
                            self.stop(start);
                            let len = self.graph.pattern_length();
                            self.transport.keep_position(start, len, len);
                        }
                        // Rebuild starts at the final mirrored mode; replay even an accepted
                        // no-op in queue order, without introducing transition effects.
                        self.with_graph(|g| g.playback_mode = mode);
                    }
                    Command::SetSongLoop { region } => {
                        let tick = self.graph.position(self.transport.position_ticks(start));
                        let tick =
                            region.map_or(tick, |r| tick.clamp(r.start_tick, r.end_tick - 1));
                        if self.graph.playback_mode == crate::PlaybackMode::Song {
                            self.with_graph(Graph::transport_stopped);
                            self.transport.seek(start, tick);
                        }
                        self.with_graph(|g| {
                            g.song_loop = region;
                            g.reset_audio();
                        });
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
        }
        // A finish can leave a block-boundary release pending: force it before re-anchoring.
        self.with_graph(Graph::transport_stopped);
        let cursor = self.transport.position_ticks(at);
        self.transport.start(at, loops);
        if self.graph.playback_mode == crate::PlaybackMode::Song {
            let cursor = self.graph.song_loop.map_or_else(
                || {
                    if cursor < self.graph.song().end_tick {
                        cursor
                    } else {
                        0
                    }
                },
                |r| cursor.clamp(r.start_tick, r.end_tick - 1),
            );
            self.transport.seek(at, cursor);
        }
        self.with_graph(Graph::transport_started);
    }

    /// Stops and rewinds at frame `at` (a block start); the sequenced notes sounding are
    /// released there, and a take recording ends there.
    fn stop(&mut self, at: u64) {
        self.recorder.end(&self.transport, at, &self.live);
        self.metronome.release();
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

    fn timeline_transport(
        &mut self,
        start: u64,
        bpm: f32,
        mode: crate::PlaybackMode,
        region: Option<crate::SongLoop>,
        mode_changed: bool,
        loop_changed: bool,
    ) {
        self.transport.set_bpm(start, bpm);
        if mode_changed {
            self.recorder.set(false, &self.transport, start, &self.live);
            self.stop(start);
        }
        // Rebuild starts at the final mirrored mode. Restore this message's mode in
        // queue order without inventing a transition before its accepted barrier.
        self.graph.playback_mode = mode;
        if loop_changed || self.graph.song_loop != region {
            let tick = self.graph.position(self.transport.position_ticks(start));
            self.with_graph(Graph::transport_stopped);
            self.transport.seek(
                start,
                region.map_or(tick, |r| tick.clamp(r.start_tick, r.end_tick - 1)),
            );
            self.graph.song_loop = region;
            self.with_graph(Graph::reset_audio);
        }
    }

    /// Swaps a timeline's pattern and song into the graph (the update then holds the old
    /// ones), keeping the place in the loop. Graph setters preserve same-bank audio.
    fn swap_timeline(
        &mut self,
        start: u64,
        mut update: Box<crate::command::TimelineUpdate>,
    ) -> Box<crate::command::TimelineUpdate> {
        let old_len = self.graph.pattern_length();
        let new_len = update.pattern.length;
        update.pattern = self.graph.set_pattern(update.pattern);
        update.song = self.graph.set_song(update.song);
        if self.graph.playback_mode == crate::PlaybackMode::Pattern {
            self.transport.keep_position(start, old_len, new_len);
        } else {
            self.transport.keep_position(start, new_len, new_len);
        }
        update
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

    pub(crate) fn flush_recording_boundaries(&mut self) {
        self.recorder.flush_mark();
    }

    pub(crate) fn recovery_stop_completed(&self, mark: u32) -> bool {
        self.completed_recovery_stop == Some(mark)
    }

    /// Consume one accepted prefix message without rendering or advancing the clock.
    /// The controller drains retirement between calls, so the parked trash slot is free.
    pub(crate) fn recovery_prefix_step(&mut self) -> bool {
        let before = self.commands.slots();
        self.retry_trash();
        self.drain_commands(self.clock, self.clock + BLOCK as u64, true);
        self.live.publish_transport(
            self.transport.playing(),
            self.graph
                .position(self.transport.position_ticks(self.clock)),
        );
        self.commands.slots() < before
    }

    #[cfg(test)]
    pub(crate) fn finish_recovery_stop(&mut self, mark: u32) {
        self.stop(self.clock);
        self.recorder
            .set(false, &self.transport, self.clock, &self.live);
        self.recorder.mark(mark);
        self.completed_recovery_stop = Some(mark);
        // Rebuilding may fail while the callback remains detached.
        self.live.publish_transport(
            self.transport.playing(),
            self.graph
                .position(self.transport.position_ticks(self.clock)),
        );
    }

    pub(crate) fn stop_after_failed_rebuild(&mut self) {
        self.fault();
        self.live.publish_recovery(self.faults, 0);
        self.live
            .publish(self.meter, false, self.clock, self.faults, self.guard_hits);
        self.served = BLOCK;
    }

    fn fault(&mut self) {
        self.successful_blocks = 0;
        self.faulted = true;
        self.faults = self.faults.wrapping_add(1);
    }

    /// Swaps in a fresh graph, unless it is from an earlier generation: a restart queued
    /// before `reconfigure` arrives after the fresh graph it made, and must not replace it
    /// (cutting held voices, or bringing back the old rate).
    fn replace_graph(&mut self, mut graph: Box<Graph>, at: u64) {
        self.host_last = None;
        let leaving = if graph.generation() == self.generation {
            self.successful_blocks = 0;
            let new_len = graph.pattern_length();
            if !self.faulted {
                graph.keep_audio(&self.graph);
            }
            self.faulted = false;
            let leaving = std::mem::replace(&mut self.graph, graph);
            // A pattern swap a faulted graph refused arrives with the fresh graph.
            if self.graph.playback_mode == crate::PlaybackMode::Pattern {
                self.transport
                    .keep_position(at, leaving.pattern_length(), new_len);
            } else {
                self.transport.keep_position(at, new_len, new_len);
                self.recorder.set(false, &self.transport, at, &self.live);
            }
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
    use crate::command::NoteId;
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

    #[test]
    fn timeline_bundles_park_and_resume_without_partial_publication() {
        let (mut p, mut commands, mut trash) = processor();
        for n in [10, 20, 30] {
            let update = crate::TimelineUpdate {
                pattern: Box::new(PatternData {
                    length: n,
                    notes: vec![],
                }),
                song: Box::new(crate::SongData {
                    end_tick: n as u64,
                    ..Default::default()
                }),
                bpm: 100.0 + n as f32,
                mode: crate::PlaybackMode::Pattern,
                region: None,
                remove_channel: None,
            };
            assert!(
                commands
                    .push(Msg::Timeline(Box::new(update), false, false))
                    .is_ok()
            );
        }
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert_eq!(p.graph.pattern().length, 20);
        assert_eq!(p.graph.song().end_tick, 20);
        assert!(matches!(p.unsent, Some(Trash::Timeline(_, _))));
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 1);
        assert!(matches!(trash.pop(), Ok(Trash::Timeline(_, _))));
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert_eq!(p.graph.pattern().length, 30);
        assert_eq!(p.graph.song().end_tick, 30);
        assert!(matches!(trash.pop(), Ok(Trash::Timeline(_, _))));
        p.process(&mut [0.0; BLOCK * 2], 2);
        assert!(matches!(trash.pop(), Ok(Trash::Timeline(_, _))));
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
        Msg::Command {
            due: 0,
            cmd,
            mode_changed: matches!(cmd, Command::SetPlaybackMode { .. }),
        }
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
                id: NoteId(60),
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
    fn tone_mix() -> crate::mixer::MixSetup {
        use crate::mixer::*;
        MixSetup::Routed(RoutedMix {
            mixer: Mixer {
                tracks: vec![Track {
                    id: MASTER,
                    output: None,
                    inserts: vec![Insert {
                        id: InsertId(1),
                        latency_frames: 0,
                        memory_bytes: 0,
                    }],
                }],
                sources: vec![Source {
                    id: SourceId::System(SystemSource::Tone),
                    destination: MASTER,
                }],
                ..Mixer::default()
            },
            gains: vec![],
            devices: vec![InsertDevice {
                insert: InsertId(1),
                kind: "utility".into(),
                seed: 0,
                params: vec![],
            }],
        })
    }

    fn install(generation: u64, base: u64, revision: u64) -> Msg {
        Msg::SetMix(Box::new(crate::mixer::live::MixInstall {
            generation,
            base,
            revision,
            live: crate::mixer::live::prepare(&tone_mix(), SR, &[]).unwrap(),
            transfers: vec![],
            params: vec![],
            installed: false,
        }))
    }

    /// The retired envelope: whether it installed, and whether it carries a mix.
    fn retired(trash: &mut Consumer<Trash>) -> (bool, bool) {
        match trash.pop() {
            Ok(Trash::Mix(m)) => (m.installed, m.live.is_some()),
            _ => panic!("a mix install always retires its envelope"),
        }
    }

    #[test]
    fn stale_or_foreign_mix_installs_are_refused_whole() {
        let (mut p, mut commands, mut trash) = processor();
        let mut buf = [0.0; BLOCK * 2];
        // Prepared against a revision the graph does not run.
        assert!(commands.push(install(0, 5, 6)).is_ok());
        p.process(&mut buf, 2);
        assert_eq!(p.graph.mix_revision(), 0);
        assert_eq!(retired(&mut trash), (false, true));
        // Prepared for another generation (rate).
        assert!(commands.push(install(1, 0, 1)).is_ok());
        p.process(&mut buf, 2);
        assert_eq!(p.graph.mix_revision(), 0);
        assert_eq!(retired(&mut trash), (false, true));
        // A faulted graph takes nothing; its replacement carries the mirrored mix.
        p.faulted = true;
        assert!(commands.push(install(0, 0, 1)).is_ok());
        p.process(&mut buf, 2);
        assert_eq!(p.graph.mix_revision(), 0);
        assert_eq!(retired(&mut trash), (false, true));
        p.faulted = false;
        // Current: installs, and the leaving LegacyDirect (no mix) retires.
        assert!(commands.push(install(0, 0, 1)).is_ok());
        p.process(&mut buf, 2);
        assert_eq!(p.graph.mix_revision(), 1);
        assert_eq!(retired(&mut trash), (true, false));
        // The next one carries the displaced routed mix back, never dropping it here.
        assert!(commands.push(install(0, 1, 2)).is_ok());
        p.process(&mut buf, 2);
        assert_eq!(p.graph.mix_revision(), 2);
        assert_eq!(retired(&mut trash), (true, true));
    }

    fn synth_node(id: u32) -> Box<ChannelNode> {
        let setup = ChannelSetup::synth(ChannelId(id), vec![], ChannelMix::default());
        Box::new(ChannelNode::new(SR, ramp_len(SR), &setup, false))
    }

    fn sources(
        generation: u64,
        base: u64,
        removes: Vec<ChannelId>,
        adds: Vec<u32>,
    ) -> Box<crate::command::SourceTransaction> {
        let Msg::SetMix(mix) = install(generation, base, base + 1) else {
            unreachable!()
        };
        Box::new(crate::command::SourceTransaction {
            removed: Vec::with_capacity(removes.len()),
            removes,
            adds: adds.into_iter().map(|id| Some(synth_node(id))).collect(),
            timeline: Some((
                Box::new(crate::TimelineUpdate {
                    pattern: Box::new(PatternData {
                        length: 480,
                        notes: vec![],
                    }),
                    song: Box::default(),
                    bpm: 90.0,
                    mode: crate::PlaybackMode::Pattern,
                    region: None,
                    remove_channel: None,
                }),
                false,
                false,
            )),
            mix: *mix,
            applied: false,
        })
    }

    fn retired_sources(trash: &mut Consumer<Trash>) -> (bool, usize, usize) {
        match trash.pop() {
            Ok(Trash::Sources(t)) => (t.applied, t.adds.iter().flatten().count(), t.removed.len()),
            _ => panic!("a source transaction always retires its envelope"),
        }
    }

    #[test]
    fn a_source_transaction_applies_whole_or_not_at_all() {
        let (mut p, mut commands, mut trash) = processor();
        let mut buf = [0.0; BLOCK * 2];
        assert!(p.graph.add_channel(synth_node(1)).is_none());
        assert!(p.graph.add_channel(synth_node(2)).is_none());
        let state = |p: &AudioProcessor| {
            (
                p.graph.channel_ids(),
                p.graph.pattern_length(),
                p.graph.mix_revision(),
            )
        };
        let before = state(&p);
        // Stale base, foreign generation, an ID still taken, an absent removal, a faulted
        // graph: each refused before any part (sources, timeline, mix) changes.
        for (t, faulted, adds) in [
            (sources(0, 4, vec![ChannelId(1)], vec![1]), false, 1),
            (sources(3, 0, vec![ChannelId(1)], vec![1]), false, 1),
            (sources(0, 0, vec![], vec![2]), false, 1),
            (sources(0, 0, vec![ChannelId(9)], vec![]), false, 0),
            (sources(0, 0, vec![ChannelId(1)], vec![1]), true, 1),
        ] {
            p.faulted = faulted;
            assert!(commands.push(Msg::Sources(t)).is_ok());
            p.process(&mut buf, 2);
            p.faulted = false;
            assert_eq!(state(&p), before);
            let (applied, held, _) = retired_sources(&mut trash);
            assert!(!applied);
            assert_eq!(held, adds, "refused nodes come back");
        }
        // Replace 1 (it fades), add 3, new timeline and mix: one boundary.
        assert!(
            commands
                .push(Msg::Sources(sources(0, 0, vec![ChannelId(1)], vec![1, 3])))
                .is_ok()
        );
        p.process(&mut buf, 2);
        assert_eq!(
            p.graph.channel_ids(),
            vec![ChannelId(2), ChannelId(1), ChannelId(3)]
        );
        assert_eq!((p.graph.pattern_length(), p.graph.mix_revision()), (480, 1));
        assert_eq!(retired_sources(&mut trash), (true, 0, 0));
    }

    #[test]
    fn a_rebuild_keeps_only_a_source_transactions_transport_chronology() {
        let t = sources(0, 0, vec![ChannelId(5)], vec![6]);
        let note = |channel| Msg::Command {
            due: 0,
            cmd: Command::ChannelNote {
                channel: ChannelId(channel),
                key: 60,
                velocity: 1.0,
                on: true,
                id: NoteId(1),
            },
            mode_changed: false,
        };
        let kept = crate::command::rebuilt_queue(vec![note(5), note(7), Msg::Sources(t)]);
        assert_eq!(kept.len(), 2, "the removed channel's earlier note is cut");
        assert!(matches!(
            kept[0],
            Msg::Command {
                cmd: Command::ChannelNote {
                    channel: ChannelId(7),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(kept[1], Msg::TimelineRebuilt { bpm: 90.0, .. }));
    }

    #[test]
    fn a_mix_install_waits_for_trash_room() {
        let (mut p, mut commands, mut trash) = processor();
        let mut buf = [0.0; BLOCK * 2];
        for _ in 0..2 {
            assert!(
                commands
                    .push(Msg::ReplaceGraph(Box::new(Graph::new(SR, 0))))
                    .is_ok()
            );
        }
        assert!(commands.push(install(0, 0, 1)).is_ok());
        p.process(&mut buf, 2);
        // One graph in the queue, one parked: the install waits, nothing changed.
        assert!(p.unsent.is_some());
        assert_eq!(p.graph.mix_revision(), 0);
        assert_eq!(commands.slots(), QUEUE_CAPACITY - 1);
        assert!(trash.pop().is_ok());
        p.process(&mut buf, 2);
        assert_eq!(p.graph.mix_revision(), 1);
        assert!(matches!(trash.pop(), Ok(Trash::Graph(_))));
        p.process(&mut buf, 2);
        assert_eq!(retired(&mut trash), (true, false));
    }
}

#[cfg(test)]
mod transport_delivery_tests {
    use super::*;
    use crate::*;
    use std::sync::{Arc, Mutex};
    struct Observe {
        pending: Option<grooviq_dsp::HostTransport>,
        seen: Arc<Mutex<Vec<grooviq_dsp::HostTransport>>>,
    }
    impl grooviq_dsp::Device for Observe {
        fn set_param(&mut self, _: u32, _: f32) {}
        fn reset(&mut self) {}
        fn set_transport(&mut self, h: &grooviq_dsp::HostTransport) {
            assert!(self.pending.replace(*h).is_none());
        }
        fn process(
            &mut self,
            l: &mut [f32],
            r: &mut [f32],
            _: Option<(&[f32], &[f32])>,
            _: &[grooviq_dsp::NoteEvent],
        ) {
            let h = self
                .pending
                .take()
                .expect("actual host producer must deliver facts");
            assert_eq!(h.frames as usize, l.len());
            self.seen.lock().unwrap().push(h);
            l.fill(0.);
            r.fill(0.);
        }
    }
    #[test]
    fn processor_delivers_grid_targets_and_stop_restores_base() {
        let ch = ChannelSetup::sampler(ChannelId(1), None, vec![], ChannelMix::default());
        let schedule = AutomationSchedule::build(
            vec![AutomationClip {
                target: AutomationTarget::ChannelVolume(ch.id),
                priority: 1,
                start_tick: 0,
                offset_tick: 0,
                length_ticks: 960,
                points: vec![
                    AutomationPoint {
                        tick: 0,
                        value: 0.,
                        segment: AutomationSegment::Linear,
                    },
                    AutomationPoint {
                        tick: 960,
                        value: 2.,
                        segment: AutomationSegment::Hold,
                    },
                ],
            }],
            std::slice::from_ref(&ch),
        )
        .unwrap();
        let (mut c, mut p) = engine_with(
            EngineConfig {
                sample_rate: 48000.,
            },
            EngineState {
                bpm: 120.,
                channels: vec![ch],
                playback_mode: PlaybackMode::Song,
                song: SongData {
                    automation: schedule,
                    end_tick: 1000,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        p.graph.observe_synth(Box::new(Observe {
            pending: None,
            seen: seen.clone(),
        }));
        c.send(Command::Play).unwrap();
        p.process(&mut [0.; 2], 2);
        assert_eq!(p.graph.channel_gain_target(), 0.);
        // Consume the cached remainder, then enter the next absolute block using a one-frame callback.
        p.process(&mut [0.; 126], 2);
        p.process(&mut [0.; 2], 2);
        assert_eq!(p.graph.channel_gain_target(), (2. * 2.56 / 960.) as f32);
        p.process(&mut [0.; 126], 2);
        c.send(Command::Stop).unwrap();
        p.process(&mut [0.; 128], 2);
        assert_eq!(p.graph.channel_gain_target(), 1.);
        let facts = seen.lock().unwrap();
        assert_eq!(
            facts.iter().map(|h| h.frame).collect::<Vec<_>>(),
            [0, 64, 128]
        );
        assert!(facts[0].playing);
        assert!(!facts[2].playing);
        assert_ne!(facts[0].discontinuity, facts[2].discontinuity);
    }
    #[test]
    fn processor_loop_edge_matches_scheduled_frame() {
        let (mut c, mut p) = engine_with(
            EngineConfig { sample_rate: 8000. },
            EngineState {
                bpm: 120.,
                playback_mode: PlaybackMode::Song,
                song: SongData {
                    end_tick: 30,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        p.graph.observe_synth(Box::new(Observe {
            pending: None,
            seen: seen.clone(),
        }));
        c.send(Command::SetSongLoop {
            region: Some(SongLoop {
                start_tick: 0,
                end_tick: 30,
            }),
        })
        .unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.; 256], 2);
        let facts = seen.lock().unwrap();
        let first_wrap = facts
            .iter()
            .flat_map(|h| {
                h.edges()
                    .map(move |e| (h.frame + u64::from(e.offset), e.iteration))
            })
            .find(|(_, i)| *i == 1)
            .unwrap();
        assert_eq!(first_wrap.0, 125);
    }
    #[test]
    fn processor_delivers_command_and_rate_transition_contexts() {
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48000.,
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        p.graph.observe_synth(Box::new(Observe {
            pending: None,
            seen: seen.clone(),
        }));
        for command in [
            Command::Play,
            Command::SetTempo { bpm: 137. },
            Command::Stop,
            Command::Play,
            Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            },
            Command::Seek { tick: 0 },
            Command::Seek { tick: 0 },
        ] {
            c.send(command).unwrap();
            p.process(&mut [0.; 128], 2);
        }
        {
            let facts = seen.lock().unwrap();
            assert_eq!(facts.len(), 7);
            assert_eq!(facts[1].bpm, 137.);
            assert_eq!(facts[0].discontinuity, facts[1].discontinuity);
            assert!(!facts[2].playing);
            assert_ne!(facts[2].discontinuity, facts[3].discontinuity);
            assert_eq!(facts[4].domain, grooviq_dsp::HostDomain::Song);
            assert_ne!(facts[5].discontinuity, facts[6].discontinuity);
        }
        c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 44100.,
            },
        );
        p.graph.observe_synth(Box::new(Observe {
            pending: None,
            seen: seen.clone(),
        }));
        p.process(&mut [0.; 128], 2);
        let facts = seen.lock().unwrap();
        assert_eq!(facts[7].sample_rate, 44100.);
        assert_ne!(facts[6].stream_generation, facts[7].stream_generation);
    }
    #[test]
    fn faulted_rebuild_preserves_automation_and_current_host_stop() {
        let channel = ChannelSetup::sampler(ChannelId(1), None, vec![], ChannelMix::default());
        let automated = SongData {
            end_tick: 9600,
            automation: AutomationSchedule::build(
                vec![AutomationClip {
                    target: AutomationTarget::ChannelVolume(channel.id),
                    priority: 1,
                    start_tick: 0,
                    offset_tick: 0,
                    length_ticks: 9600,
                    points: vec![AutomationPoint {
                        tick: 0,
                        value: 0.5,
                        segment: AutomationSegment::Hold,
                    }],
                }],
                std::slice::from_ref(&channel),
            )
            .unwrap(),
            ..Default::default()
        };
        let (mut control, mut processor) = engine_with(
            EngineConfig {
                sample_rate: 48000.0,
            },
            EngineState {
                channels: vec![channel],
                song: automated.clone(),
                ..Default::default()
            },
        );
        let mut block = [0.0; BLOCK * 2];
        control
            .send(Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            })
            .unwrap();
        control.send(Command::SetTempo { bpm: 137.0 }).unwrap();
        control.send(Command::Play).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        processor.fault();
        assert!(control.reconfigure(
            &mut processor,
            EngineConfig {
                sample_rate: 44100.0
            }
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        processor.graph.observe_synth(Box::new(Observe {
            pending: None,
            seen: seen.clone(),
        }));
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        let running = *seen.lock().unwrap().last().unwrap();
        assert!(running.playing);
        assert_eq!(running.bpm, 137.0);
        assert_eq!(running.sample_rate, 44100.0);
        assert_eq!(running.domain, grooviq_dsp::HostDomain::Song);
        // Exercise direct exhaustion settlement with no callback to publish Stop.
        processor.finish_recovery_stop(71);
        assert!(!control.live().playing);
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 1.0);
        let stopped = *seen.lock().unwrap().last().unwrap();
        assert!(!stopped.playing);
        assert_ne!(running.discontinuity, stopped.discontinuity);
        assert_eq!(stopped.bpm, running.bpm);
        assert_eq!(stopped.domain, running.domain);
    }
    #[test]
    fn idle_automation_transitions_restore_and_resume_current_targets() {
        let channel = ChannelSetup::sampler(ChannelId(1), None, vec![], ChannelMix::default());
        let automated = SongData {
            end_tick: 9600,
            automation: AutomationSchedule::build(
                vec![AutomationClip {
                    target: AutomationTarget::ChannelVolume(channel.id),
                    priority: 1,
                    start_tick: 0,
                    offset_tick: 0,
                    length_ticks: 9600,
                    points: vec![AutomationPoint {
                        tick: 0,
                        value: 0.5,
                        segment: AutomationSegment::Hold,
                    }],
                }],
                std::slice::from_ref(&channel),
            )
            .unwrap(),
            ..Default::default()
        };
        let (mut control, mut processor) = engine_with(
            EngineConfig {
                sample_rate: 48000.0,
            },
            EngineState {
                channels: vec![channel],
                song: automated.clone(),
                ..Default::default()
            },
        );
        let mut block = [0.0; BLOCK * 2];
        // No automation in Pattern, including a manual base edit while idle.
        control.send(Command::Play).unwrap();
        control
            .send(Command::SetChannelMix {
                channel: ChannelId(1),
                mix: ChannelMix {
                    volume: 0.25,
                    ..Default::default()
                },
            })
            .unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.25);
        control
            .send(Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            })
            .unwrap();
        control.send(Command::Play).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        // Clear a live schedule: restore once, then remain at the updated base.
        control
            .set_song(SongData {
                end_tick: 9600,
                ..Default::default()
            })
            .unwrap();
        for _ in 0..3 {
            processor.process(&mut block, 2);
            assert_eq!(processor.graph.channel_gain_target(), 0.25);
        }
        control.set_song(automated).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        control
            .send(Command::SetPlaybackMode {
                mode: PlaybackMode::Pattern,
            })
            .unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.25);
        control
            .send(Command::SetChannelMix {
                channel: ChannelId(1),
                mix: ChannelMix {
                    volume: 0.75,
                    ..Default::default()
                },
            })
            .unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.75);
        control
            .send(Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            })
            .unwrap();
        control.send(Command::Play).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        control.send(Command::Stop).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.75);
        control.send(Command::Play).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        control.reconfigure(
            &mut processor,
            EngineConfig {
                sample_rate: 44100.0,
            },
        );
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.5);
        control.send(Command::Stop).unwrap();
        processor.process(&mut block, 2);
        assert_eq!(processor.graph.channel_gain_target(), 0.75);
    }
}
