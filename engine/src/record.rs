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
}

/// The audio thread's end of the recording queue.
pub(crate) struct Recorder {
    tx: Producer<RecordEvent>,
    on: bool,
}

impl Recorder {
    pub(crate) fn new(tx: Producer<RecordEvent>) -> Self {
        Self { tx, on: false }
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

    /// Real-time safe: a full queue drops the event and counts it, never waits.
    fn push(&mut self, event: RecordEvent, live: &LiveShared) {
        if self.tx.push(event).is_err() {
            live.count_record_drop();
        }
    }
}
