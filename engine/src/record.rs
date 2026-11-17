//! The recording tap: the audio thread queues every live channel note that lands while
//! recording and playing, with the pattern tick it was heard at, for the controller to drain.

use crate::live::LiveShared;
use crate::state::ChannelId;
use crate::transport::TransportClock;
use rtrb::Producer;

/// Events the recording queue holds. When it is full, a new event is dropped and counted
/// ([`crate::Controller::record_dropped`]).
pub const RECORD_CAPACITY: usize = 1024;

/// What the audio thread recorded, corrected for the output latency (placed where it was
/// heard). `tick` is the whole tick played since Play, at the tempo of each moment: a looped
/// pattern keeps counting up and a pattern of a new length does not move it, so `off.tick -
/// on.tick` is a held note's length. `position` is the tick within the pattern that was
/// playing at that moment (exactly what the sequencer played there, after any resize kept
/// the place in the loop): where a recorded note goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecordEvent {
    /// A channel note that landed on its channel.
    Note {
        channel: ChannelId,
        key: u8,
        velocity: f32,
        on: bool,
        tick: u64,
        position: u32,
    },
    /// The take ended (recording switched off, or the transport stopped or restarted): a note
    /// still held ends here.
    End { tick: u64, position: u32 },
    /// A barrier: [`crate::Controller::load_state`] queued it behind the state, so every event
    /// before it belongs to the state before, every event after it to the new one. `id` is
    /// [`crate::Controller::record_mark`]. Never lost: a mark that finds the queue full waits,
    /// and later events are dropped (counted) rather than overtake it.
    Mark { id: u32 },
}

/// The audio thread's end of the recording queue.
pub(crate) struct Recorder {
    tx: Producer<RecordEvent>,
    on: bool,
    /// A mark that did not fit in the queue yet.
    pending_mark: Option<u32>,
}

impl Recorder {
    pub(crate) fn new(tx: Producer<RecordEvent>) -> Self {
        Self {
            tx,
            on: false,
            pending_mark: None,
        }
    }

    /// Queues the barrier `id` (see [`RecordEvent::Mark`]), whatever recording does. A newer
    /// mark replaces one still waiting.
    pub(crate) fn mark(&mut self, id: u32) {
        self.pending_mark = Some(id);
        self.flush_mark();
    }

    /// Sends a waiting mark if the queue has room. Called at the start of every block and
    /// before every event, so a mark goes out as soon as the queue drains.
    pub(crate) fn flush_mark(&mut self) {
        if let Some(id) = self.pending_mark
            && self.tx.push(RecordEvent::Mark { id }).is_ok()
        {
            self.pending_mark = None;
        }
    }

    /// Whether a take is running: recording is on and the transport plays.
    pub(crate) fn taking(&self, transport: &TransportClock) -> bool {
        self.on && transport.playing()
    }

    /// Switches recording at frame `at`, ending a running take when it goes off.
    pub(crate) fn set(&mut self, on: bool, transport: &TransportClock, at: u64, live: &LiveShared) {
        if !on {
            self.end(transport, at, live);
        }
        self.on = on;
    }

    /// Queues the end of a running take at frame `at` (call it before the transport stops).
    pub(crate) fn end(&mut self, transport: &TransportClock, at: u64, live: &LiveShared) {
        if self.taking(transport) {
            let (tick, position) = transport.record_at(at, live.output_latency_frames());
            self.push(RecordEvent::End { tick, position }, live);
        }
    }

    /// Queues a note that landed at frame `at`, if a take is running.
    pub(crate) fn note(
        &mut self,
        transport: &TransportClock,
        at: u64,
        live: &LiveShared,
        note: (ChannelId, u8, f32, bool),
    ) {
        if !self.taking(transport) {
            return;
        }
        let (channel, key, velocity, on) = note;
        let (tick, position) = transport.record_at(at, live.output_latency_frames());
        let event = RecordEvent::Note {
            channel,
            key,
            velocity,
            on,
            tick,
            position,
        };
        self.push(event, live);
    }

    /// Real-time safe: a full queue drops the event and counts it, never waits. A waiting mark
    /// goes first: the controller drains on its own thread and can free room between any two
    /// pushes, so the mark is retried here, and while it still does not fit the event is
    /// dropped (counted) rather than overtake it.
    fn push(&mut self, event: RecordEvent, live: &LiveShared) {
        self.flush_mark();
        if self.pending_mark.is_some() || self.tx.push(event).is_err() {
            live.count_record_drop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: u8) -> RecordEvent {
        RecordEvent::Note {
            channel: ChannelId(1),
            key,
            velocity: 1.0,
            on: true,
            tick: 0,
            position: 0,
        }
    }

    /// The controller drains on its own thread, so room can appear between two pushes of one
    /// block: a parked mark must still go out before the next event, or that event must be
    /// refused (and counted).
    #[test]
    fn mid_block_consumer_cannot_overtake_mark() {
        let (tx, mut rx) = rtrb::RingBuffer::new(4);
        let live = LiveShared::default();
        let mut r = Recorder::new(tx);
        for key in 0..4 {
            r.push(event(key), &live);
        }
        r.mark(7);
        assert_eq!(
            rx.pop(),
            Ok(event(0)),
            "the consumer frees one slot mid-block"
        );
        r.push(event(9), &live);
        let rest: Vec<RecordEvent> = std::iter::from_fn(|| rx.pop().ok()).collect();
        assert_eq!(
            rest,
            [event(1), event(2), event(3), RecordEvent::Mark { id: 7 }],
            "the mark took the free slot; the later event did not overtake it"
        );
        assert_eq!(live.record_dropped(), 1, "the refused event is counted");
    }
}
