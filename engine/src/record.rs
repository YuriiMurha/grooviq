//! The recording tap: the audio thread queues every live channel note that lands while
//! recording and playing, with the pattern tick it was heard at, for the controller to drain.

use crate::command::NoteId;
use crate::live::LiveShared;
use crate::state::ChannelId;
use crate::transport::TransportClock;
use rtrb::Producer;

/// Events the recording queue holds. When it is full, a new note is dropped and counted
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
        /// The take it was played in: every note of one take has the same number, and the
        /// next take's is one more (wrapping). An End that finds the queue full is retained
        /// ahead of subsequent notes, preserving the accepted take's ending tick.
        take: u32,
        /// The press (see [`NoteId`]): a note-off carries its note-on's, so presses of one key
        /// held at once pair up by it, whichever is let go first.
        id: NoteId,
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
    /// A note accepted in the current take, not a later backpressured empty take.
    take_has_note: bool,
    /// A take end that must precede the next barrier and all later notes.
    pending_end: Option<RecordEvent>,
    /// An accepted barrier already ahead of the original retained End.
    preceding_end_mark: Option<u32>,
    /// A mark that did not fit in the queue yet.
    pending_mark: Option<u32>,
    /// The running (or next) take's number (see [`RecordEvent::Note`]).
    take: u32,
}

impl Recorder {
    pub(crate) fn new(tx: Producer<RecordEvent>) -> Self {
        Self {
            tx,
            on: false,
            take_has_note: false,
            pending_end: None,
            preceding_end_mark: None,
            pending_mark: None,
            take: 0,
        }
    }

    /// Queues the barrier `id` (see [`RecordEvent::Mark`]), whatever recording does. A newer
    /// mark replaces one still waiting.
    pub(crate) fn mark(&mut self, id: u32) {
        self.pending_mark = Some(id);
        self.flush_mark();
    }

    /// Flushes a retained take End before its barrier. Called before each block/event and
    /// by the control poller while detached, so consumer progress can release both.
    pub(crate) fn flush_mark(&mut self) {
        if let Some(id) = self.preceding_end_mark {
            if self.tx.push(RecordEvent::Mark { id }).is_err() {
                return;
            }
            self.preceding_end_mark = None;
        }
        if let Some(end) = self.pending_end {
            if self.tx.push(end).is_err() {
                return;
            }
            self.pending_end = None;
        }
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

    /// Switches recording at frame `at`, ending a running take when it goes off. Every
    /// switch on starts a new take number, even if recording was on already (a switch-off the
    /// engine never got was cancelled).
    pub(crate) fn set(&mut self, on: bool, transport: &TransportClock, at: u64, live: &LiveShared) {
        if on {
            self.take_has_note = false;
            self.take = self.take.wrapping_add(1);
        } else {
            self.end(transport, at, live);
        }
        self.on = on;
    }

    /// Queues the end of a running take at frame `at` (call it before the transport stops).
    pub(crate) fn end(&mut self, transport: &TransportClock, at: u64, live: &LiveShared) {
        if self.taking(transport) {
            let (tick, position) = transport.record_at(at, live.output_latency_frames());
            self.flush_mark();
            // No later note can pass a parked boundary. A subsequent empty take needs
            // no new End; preserve the first accepted take's original ending tick.
            if self.pending_end.is_none() && (self.pending_mark.is_none() || self.take_has_note) {
                // A same-take barrier may itself be backpressured. Retain it
                // ahead of this End, and keep the later completion mark behind.
                self.preceding_end_mark = self.pending_mark.take();
                let end = RecordEvent::End { tick, position };
                if self.preceding_end_mark.is_some() || self.tx.push(end).is_err() {
                    self.pending_end = Some(end);
                }
            }
            self.take_has_note = false;
            // The next take's notes carry the next number.
            self.take = self.take.wrapping_add(1);
        }
    }

    /// Queues a note (channel, key, velocity, on, press) that landed at frame `at`, if a take
    /// is running.
    pub(crate) fn note(
        &mut self,
        transport: &TransportClock,
        at: u64,
        live: &LiveShared,
        note: (ChannelId, u8, f32, bool, NoteId),
    ) {
        if !self.taking(transport) {
            return;
        }
        let (channel, key, velocity, on, id) = note;
        let (tick, position) = transport.record_at(at, live.output_latency_frames());
        let event = RecordEvent::Note {
            channel,
            key,
            velocity,
            on,
            tick,
            position,
            take: self.take,
            id,
        };
        self.push(event, live);
    }

    /// Real-time safe: a full queue drops the note and counts it, never waits. A retained End
    /// and waiting mark go first: the controller drains on its own thread and can free room between any two
    /// pushes, so the mark is retried here, and while it still does not fit the event is
    /// dropped (counted) rather than overtake it.
    fn push(&mut self, event: RecordEvent, live: &LiveShared) {
        self.flush_mark();
        if self.pending_end.is_some() || self.pending_mark.is_some() || self.tx.push(event).is_err()
        {
            live.count_record_drop();
        } else {
            self.take_has_note = true;
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
            take: 0,
            id: NoteId(u64::from(key)),
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
    #[test]
    fn full_fifo_retains_end_before_mark_and_later_notes() {
        let (tx, mut rx) = rtrb::RingBuffer::new(2);
        let live = LiveShared::default();
        let mut recorder = Recorder::new(tx);
        let mut clock = TransportClock::new(48_000.0, 120.0);
        clock.start(0, None);
        recorder.set(true, &clock, 0, &live);
        for on in [true, false] {
            recorder.note(&clock, 0, &live, (ChannelId(1), 60, 0.8, on, NoteId(1)));
        }
        recorder.end(&clock, 64, &live);
        clock.stop();
        recorder.set(false, &clock, 64, &live);
        recorder.mark(7);
        assert_eq!(live.record_dropped(), 0);
        assert!(matches!(rx.pop(), Ok(RecordEvent::Note { on: true, .. })));
        // The next take cannot insert a note or a later End ahead of the first End/Mark.
        clock.start(128, None);
        recorder.set(true, &clock, 128, &live);
        recorder.note(&clock, 128, &live, (ChannelId(1), 61, 0.8, true, NoteId(2)));
        recorder.end(&clock, 256, &live);
        assert!(matches!(rx.pop(), Ok(RecordEvent::Note { on: false, .. })));
        assert_eq!(
            rx.pop(),
            Ok(RecordEvent::End {
                tick: 2,
                position: 2
            })
        );
        recorder.flush_mark();
        assert_eq!(rx.pop(), Ok(RecordEvent::Mark { id: 7 }));
        assert!(rx.pop().is_err());
        assert_eq!(
            live.record_dropped(),
            1,
            "only the later refused note is dropped"
        );
    }
}

#[cfg(test)]
mod boundary_witness_v2 {
    use super::*;

    #[test]
    fn accepted_take_end_precedes_mark_with_backpressure() {
        let (tx, mut rx) = rtrb::RingBuffer::new(2);
        let live = LiveShared::default();
        let mut recorder = Recorder::new(tx);
        let mut clock = TransportClock::new(48_000.0, 120.0);
        clock.start(0, None);
        recorder.set(true, &clock, 0, &live);
        recorder.note(
            &clock,
            100,
            &live,
            (ChannelId(1), 76, 0.8, true, NoteId(701)),
        );
        recorder.note(
            &clock,
            100,
            &live,
            (ChannelId(1), 77, 0.8, true, NoteId(702)),
        );
        recorder.end(&clock, 200, &live);
        clock.stop();
        recorder.set(false, &clock, 200, &live);
        recorder.mark(41);
        assert!(
            matches!(
                rx.pop(),
                Ok(RecordEvent::Note {
                    id: NoteId(701),
                    on: true,
                    ..
                })
            ),
            "positively accepted original held note"
        );
        recorder.flush_mark(); // One free slot delivers original End, Mark remains parked.
        clock.start(400, None);
        recorder.set(true, &clock, 400, &live);
        recorder.note(
            &clock,
            400,
            &live,
            (ChannelId(1), 78, 0.8, true, NoteId(703)),
        );
        assert!(matches!(
            rx.pop(),
            Ok(RecordEvent::Note {
                id: NoteId(702),
                ..
            })
        ));
        recorder.flush_mark();
        assert_eq!(
            rx.pop(),
            Ok(RecordEvent::End {
                tick: 8,
                position: 8
            })
        );
        assert_eq!(rx.pop(), Ok(RecordEvent::Mark { id: 41 }));
        assert!(rx.pop().is_err());
        assert_eq!(
            live.record_dropped(),
            1,
            "only later note is explicitly refused"
        );
    }
}

#[cfg(test)]
mod recovery_prefix_boundary_tests {
    use super::*;
    #[test]
    fn full_record_fifo_keeps_earlier_barrier_before_original_end_and_recovery_mark() {
        for earlier in [&[10_u32][..], &[9_u32, 10][..]] {
            let (tx, mut rx) = rtrb::RingBuffer::new(2);
            let live = LiveShared::default();
            let mut recorder = Recorder::new(tx);
            let mut clock = TransportClock::new(48_000.0, 120.0);
            clock.start(0, None);
            recorder.set(true, &clock, 0, &live);
            for id in [1, 2] {
                recorder.note(
                    &clock,
                    100,
                    &live,
                    (ChannelId(1), 76, 0.8, true, NoteId(id)),
                );
            }
            for id in earlier {
                recorder.mark(*id);
            } // Existing pending-mark coalescing remains.
            recorder.flush_mark(); // Zero free slots cannot deliver a boundary.
            recorder.end(&clock, 200, &live);
            clock.stop();
            recorder.set(false, &clock, 200, &live);
            recorder.mark(11); // Recovery completion must not swallow the original End.
            assert!(matches!(
                rx.pop(),
                Ok(RecordEvent::Note { id: NoteId(1), .. })
            ));
            recorder.flush_mark(); // One free slot: preceding barrier only.
            assert!(matches!(
                rx.pop(),
                Ok(RecordEvent::Note { id: NoteId(2), .. })
            ));
            recorder.flush_mark();
            assert_eq!(rx.pop(), Ok(RecordEvent::Mark { id: 10 }));
            assert_eq!(
                rx.pop(),
                Ok(RecordEvent::End {
                    tick: 8,
                    position: 8
                })
            );
            recorder.flush_mark();
            assert_eq!(rx.pop(), Ok(RecordEvent::Mark { id: 11 }));
            assert!(rx.pop().is_err());
            assert_eq!(live.record_dropped(), 0);
        }
    }
}
