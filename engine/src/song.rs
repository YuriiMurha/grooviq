//! Immutable, control-validated musical song schedule.
use crate::{ArrangementData, ChannelId, ControlError, MAX_SONG_NOTES, MAX_SONG_TICKS};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackMode {
    #[default]
    Pattern,
    Song,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SongLoop {
    pub start_tick: u64,
    pub end_tick: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SongData {
    pub arrangement: ArrangementData,
    /// All-clip musical horizon, including future audio and automation schedules.
    pub end_tick: u64,
    /// Exact project-frame horizon at frame_rate; control integration updates tempo-dependent placement.
    pub end_frame: u64,
    /// Reference rate of end_frame; conversion happens once at the render rate.
    pub frame_rate: f32,
}
impl SongData {
    pub(crate) fn checked(self, channels: &[ChannelId]) -> Result<Self, ControlError> {
        if !self.frame_rate.is_finite()
            || !(8000.0..=384000.0).contains(&self.frame_rate)
            || self.end_frame > 46_080_000_000
            || self.end_tick > MAX_SONG_TICKS
            || self.arrangement.length > self.end_tick
            || self.arrangement.notes.len() > MAX_SONG_NOTES
        {
            return Err(ControlError::Rejected);
        }
        let mut previous = None;
        for n in &self.arrangement.notes {
            let order = (n.tick, n.clip_id, n.channel, n.slide, n.key);
            if previous.is_some_and(|p| p > order)
                || n.tick >= self.end_tick
                || n.length == 0
                || n.tick
                    .checked_add(n.length)
                    .is_none_or(|end| end > self.end_tick)
                || n.key > 127
                || !n.velocity.is_finite()
                || !(0.0..=1.0).contains(&n.velocity)
                || !channels.contains(&n.channel)
            {
                return Err(ControlError::Rejected);
            }
            previous = Some(order);
        }
        Ok(self)
    }
    pub(crate) fn end_frame_at(&self, rate: f32) -> u64 {
        (self.end_frame as f64 * f64::from(rate) / f64::from(self.frame_rate)).ceil() as u64
    }
    pub(crate) fn valid_loop(&self, region: Option<SongLoop>) -> bool {
        region.is_none_or(|r| r.start_tick < r.end_tick && r.end_tick <= self.end_tick)
    }
}

use crate::BLOCK;
use crate::SongNote;
use crate::command::NoteId;
use crate::transport::TransportClock;
use grooviq_dsp::NoteEvent;

/// Note-offs waiting for their frame. When the table is full, a note-on is skipped too.
pub(crate) const PENDING: usize = 1024;
/// Notes one block looks at; the rest start at the next block's start.
const MAX_EVENTS: usize = 256;

use crate::sequencer::Delivery;

#[derive(Clone, Copy, Debug)]
struct PendingOff {
    frame: u64,
    played_tick: u64,
    forced: bool,
    channel: ChannelId,
    key: u8,
    /// The id its note-on carried.
    id: u64,
}

const NO_OFF: PendingOff = PendingOff {
    frame: 0,
    played_tick: 0,
    forced: false,
    channel: ChannelId(0),
    key: 0,
    id: 0,
};

pub(crate) struct SongSequencer {
    /// The next note to start: its index among the pattern's notes, in loop `loop_index`.
    next: usize,
    loop_index: u64,
    /// The cursor must be placed again before the next note.
    seek: bool,
    pending: [PendingOff; PENDING],
    count: usize,
    /// No pending note-off is due before this frame (`u64::MAX` when none waits).
    earliest: u64,
    release_map: Option<(u64, u64, u32, u32)>,
    /// Numbers the notes played: each note-on and its note-off carry the id it makes, so a
    /// note-off ends its own note, not another one on its key (a short note inside a long one,
    /// a live note). Ids have [`NoteId::SEQUENCED`] set, which a live note never has.
    played: u64,
}

impl SongSequencer {
    pub(crate) fn new() -> Self {
        Self {
            next: 0,
            loop_index: 0,
            seek: true,
            pending: [NO_OFF; PENDING],
            count: 0,
            earliest: u64::MAX,
            release_map: None,
            played: 1 << 62,
        }
    }

    /// The cursor moves to the first note the transport has not covered yet: after Play or a
    /// pattern swap.
    pub(crate) fn reseek(&mut self) {
        self.seek = true;
    }

    /// Makes every pending note-off due at once (the next [`Self::run`] sends them at the
    /// start of its block) and re-seeks: the transport stopped.
    pub(crate) fn stop(&mut self) {
        for off in &mut self.pending[..self.count] {
            off.frame = 0;
            off.forced = true;
        }
        if self.count > 0 {
            self.earliest = 0;
        }
        self.seek = true;
    }

    /// Drops the note-offs for a removed channel (its notes went with it).
    pub(crate) fn forget_channel(&mut self, id: ChannelId) {
        let mut kept = 0;
        for i in 0..self.count {
            if self.pending[i].channel != id {
                self.pending[kept] = self.pending[i];
                kept += 1;
            }
        }
        self.count = kept;
    }

    /// Note-offs waiting.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.count
    }

    /// Sends what falls in the block that starts at frame `f0`: note-offs due in it (and any
    /// left over), and, while `clock` plays, the notes that start in it. Events go to `sink`
    /// with their offset in the block; at one frame, note-offs go before note-ons.
    pub(crate) fn run(
        &mut self,
        song: &SongData,
        region: Option<SongLoop>,
        clock: &TransportClock,
        f0: u64,
        mut sink: impl FnMut(ChannelId, NoteEvent) -> Delivery,
    ) {
        let f1 = f0 + BLOCK as u64;
        self.flush(clock, f0, f1 - 1, &mut sink);
        if !clock.playing() {
            return;
        }
        let notes = &song.arrangement.notes;
        let (start, end) = region.map_or((0, song.end_tick), |r| (r.start_tick, r.end_tick));
        let lo = notes.partition_point(|n| n.tick < start);
        let hi = notes.partition_point(|n| n.tick < end);
        if std::mem::take(&mut self.seek) {
            let from = clock.covered();
            self.loop_index = if region.is_some() && from >= end {
                (from - start) / (end - start)
            } else {
                0
            };
            let within = from.saturating_sub(self.loop_index * (end - start));
            self.next = notes.partition_point(|n| n.tick < within);
        }
        if region.is_some() && lo == hi {
            return;
        }
        for _ in 0..MAX_EVENTS {
            if self.next >= hi {
                if region.is_none() {
                    break;
                }
                self.next = lo;
                self.loop_index += 1;
            }
            let note = notes[self.next];
            let tick = note.tick + self.loop_index * (end - start);
            let frame = clock.frame_of(tick as f64);
            if frame >= f1 {
                break;
            }
            let frame = frame.max(f0);
            self.flush(clock, f0, frame, &mut sink);
            let boundary = end + self.loop_index * (end - start);
            self.start(note, tick, boundary, frame - f0, clock, &mut sink);
            self.next += 1;
        }
        self.flush(clock, f0, f1 - 1, &mut sink);
    }

    /// Sends `note`'s note-on at `offset` and books its note-off, if the table has room and the
    /// channel takes it.
    fn start(
        &mut self,
        note: SongNote,
        tick: u64,
        boundary: u64,
        offset: u64,
        clock: &TransportClock,
        sink: &mut impl FnMut(ChannelId, NoteEvent) -> Delivery,
    ) {
        if self.count == PENDING {
            return;
        }
        let id = NoteId::sequenced_number(self.played).0;
        self.played = self.played.wrapping_add(1);
        let on = NoteEvent {
            offset: offset as u32,
            key: note.key,
            velocity: note.velocity,
            on: true,
            slide: note.slide,
            id,
        };
        if sink(note.channel, on) != Delivery::Taken {
            return;
        }
        // At least one tick, so the note-off always follows its note-on.
        let end = tick.saturating_add(note.length.max(1)).min(boundary);
        let frame = clock.frame_of(end as f64);
        self.pending[self.count] = PendingOff {
            frame,
            played_tick: clock.played_tick_of(end),
            forced: false,
            channel: note.channel,
            key: note.key,
            id,
        };
        self.count += 1;
        self.earliest = self.earliest.min(frame);
    }

    /// Sends every pending note-off due at or before frame `through` (late ones at offset 0).
    /// One whose channel is full waits for the next block.
    pub(crate) fn flush(
        &mut self,
        clock: &TransportClock,
        f0: u64,
        through: u64,
        sink: &mut impl FnMut(ChannelId, NoteEvent) -> Delivery,
    ) {
        let map = clock.release_map();
        if self.release_map != Some(map) {
            self.earliest = u64::MAX;
            for off in &mut self.pending[..self.count] {
                if !off.forced {
                    off.frame = clock.frame_of_played_tick(off.played_tick);
                }
                self.earliest = self.earliest.min(off.frame);
            }
            self.release_map = Some(map);
        }
        if self.earliest > through {
            return;
        }
        let mut earliest = u64::MAX;
        let mut i = self.count;
        while i > 0 {
            i -= 1;
            let off = self.pending[i];
            if off.frame <= through {
                let event = NoteEvent {
                    offset: (off.frame.max(f0) - f0) as u32,
                    key: off.key,
                    velocity: 0.0,
                    on: false,
                    slide: false,
                    id: off.id,
                };
                if sink(off.channel, event) != Delivery::Full {
                    // Every slot above `i` was already visited, so the one moved in is too.
                    self.count -= 1;
                    self.pending[i] = self.pending[self.count];
                    continue;
                }
            }
            earliest = earliest.min(off.frame);
        }
        self.earliest = earliest;
    }
}

impl Default for SongData {
    fn default() -> Self {
        Self {
            arrangement: ArrangementData {
                length: 0,
                notes: Vec::new(),
            },
            end_tick: 0,
            end_frame: 0,
            frame_rate: 48000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn song() -> SongData {
        SongData {
            arrangement: ArrangementData {
                length: 1000,
                notes: vec![SongNote {
                    tick: 240,
                    length: 120,
                    channel: ChannelId(1),
                    key: 60,
                    velocity: 1.0,
                    slide: false,
                    clip_id: 1,
                }],
            },
            end_tick: 1000,
            end_frame: 0,
            frame_rate: 48000.0,
        }
    }
    #[test]
    fn scalar_frame_reference_across_tempos_and_rates() {
        for bpm in [120.0, 140.0] {
            for rate in [44100.0, 48000.0, 96000.0] {
                let mut clock = TransportClock::new(rate, bpm);
                clock.start(0, None);
                let mut seq = SongSequencer::new();
                let mut events = Vec::new();
                for f in (0..100000).step_by(BLOCK) {
                    seq.run(&song(), None, &clock, f, |_, e| {
                        events.push((f + u64::from(e.offset), e.on, e.id));
                        Delivery::Taken
                    });
                    clock.mark_covered(f + BLOCK as u64);
                }
                let frame = |tick: f64| {
                    (tick * 60.0 * f64::from(rate) / (f64::from(bpm) * 960.0)).ceil() as u64
                };
                assert_eq!(events.len(), 2);
                assert_eq!(events[0].0, frame(240.0));
                assert_eq!(events[1].0, frame(360.0));
                assert_eq!(events[0].2, events[1].2);
                assert_eq!(seq.pending(), 0);
            }
        }
    }
    #[test]
    fn nonzero_loop_excludes_end_and_clips_release_with_unique_instances() {
        let mut data = song();
        data.arrangement.notes[0].length = 500;
        data.arrangement.notes.push(SongNote {
            tick: 480,
            length: 1,
            ..data.arrangement.notes[0]
        });
        let mut clock = TransportClock::new(48000.0, 120.0);
        clock.start(0, None);
        clock.seek(0, 240);
        let mut seq = SongSequencer::new();
        let mut events = Vec::new();
        for f in (0..13000).step_by(BLOCK) {
            seq.run(
                &data,
                Some(SongLoop {
                    start_tick: 240,
                    end_tick: 480,
                }),
                &clock,
                f,
                |_, e| {
                    events.push((f + u64::from(e.offset), e.on, e.id));
                    Delivery::Taken
                },
            );
            clock.mark_covered(f + BLOCK as u64);
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| e.1)
                .map(|e| e.0)
                .collect::<Vec<_>>(),
            [0, 6000, 12000]
        );
        assert!(events.contains(&(6000, false, events[0].2)));
        assert_ne!(
            events[0].2,
            events.iter().find(|e| e.1 && e.0 == 6000).unwrap().2
        );
    }
    #[test]
    fn seek_is_onset_only_and_stop_retries_full_releases() {
        let mut clock = TransportClock::new(48000.0, 120.0);
        clock.start(0, None);
        clock.seek(0, 300);
        let mut seq = SongSequencer::new();
        let mut count = 0;
        seq.run(&song(), None, &clock, 0, |_, _| {
            count += 1;
            Delivery::Taken
        });
        assert_eq!(count, 0);
        clock.seek(64, 240);
        seq.reseek();
        seq.run(&song(), None, &clock, 64, |_, _| Delivery::Taken);
        assert_eq!(seq.pending(), 1);
        seq.stop();
        clock.stop();
        seq.run(&song(), None, &clock, 128, |_, _| Delivery::Full);
        assert_eq!(seq.pending(), 1);
        seq.run(&song(), None, &clock, 192, |_, e| {
            assert!(!e.on);
            Delivery::Taken
        });
        assert_eq!(seq.pending(), 0);
    }
    #[test]
    fn off_table_and_one_tick_extreme_loop_have_bounded_work() {
        let mut data = song();
        data.arrangement.notes[0].tick = 0;
        data.arrangement.notes[0].length = 1;
        let mut clock = TransportClock::new(8000.0, 400.0);
        clock.start(0, None);
        let mut seq = SongSequencer::new();
        let mut delivered = 0;
        seq.run(
            &data,
            Some(SongLoop {
                start_tick: 0,
                end_tick: 1,
            }),
            &clock,
            0,
            |_, _| {
                delivered += 1;
                Delivery::Taken
            },
        );
        assert!(delivered > 50 && delivered < 256);
        data.arrangement.notes = vec![
            SongNote {
                tick: 0,
                length: 1000,
                channel: ChannelId(1),
                key: 60,
                velocity: 1.0,
                slide: false,
                clip_id: 1
            };
            PENDING + 500
        ];
        let mut seq = SongSequencer::new();
        let mut started = 0;
        for i in 0..8 {
            seq.run(&data, None, &clock, i * 64, |_, e| {
                if e.on {
                    started += 1;
                }
                Delivery::Taken
            });
        }
        assert_eq!(
            started, PENDING,
            "every accepted onset must retain its release slot"
        );
        assert_eq!(seq.pending(), PENDING);
    }
}
#[cfg(test)]
mod release_order_tests {
    use super::*;
    #[test]
    fn same_block_release_precedes_retrigger_at_its_exact_frame() {
        let n = SongNote {
            tick: 0,
            length: 1,
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            slide: false,
            clip_id: 1,
        };
        let data = SongData {
            arrangement: ArrangementData {
                length: 2,
                notes: vec![n, SongNote { tick: 1, ..n }],
            },
            end_tick: 2,
            ..SongData::default()
        };
        let mut clock = TransportClock::new(8000.0, 400.0);
        clock.start(0, None);
        let mut seq = SongSequencer::new();
        let mut events = Vec::new();
        seq.run(&data, None, &clock, 0, |_, e| {
            events.push((e.offset, e.on));
            Delivery::Taken
        });
        assert_eq!(events, vec![(0, true), (2, false), (2, true), (3, false)]);
    }
}
#[cfg(test)]
mod rate_release_tests {
    use super::*;
    #[test]
    fn held_song_release_uses_the_new_rate_and_forced_release_stays_due() {
        let note = SongNote {
            tick: 0,
            length: 1920,
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            slide: false,
            clip_id: 1,
        };
        let data = SongData {
            arrangement: ArrangementData {
                length: 3840,
                notes: vec![note],
            },
            end_tick: 3840,
            ..SongData::default()
        };
        let mut clock = TransportClock::new(48000.0, 120.0);
        clock.start(0, None);
        let mut seq = SongSequencer::new();
        let mut offs = Vec::new();
        for f in (0..80000).step_by(BLOCK) {
            if f == 24000 {
                clock.set_sample_rate(f, 96000.0);
            }
            seq.run(&data, None, &clock, f, |_, e| {
                if !e.on {
                    offs.push(f + u64::from(e.offset));
                }
                Delivery::Taken
            });
            clock.mark_covered(f + 64);
        }
        assert_eq!(offs, vec![72000]);
        clock.start(80000, None);
        seq.reseek();
        seq.run(&data, None, &clock, 80000, |_, _| Delivery::Taken);
        seq.stop();
        clock.seek(80064, 1500);
        clock.set_bpm(80064, 20.0);
        seq.run(&data, None, &clock, 80064, |_, _| Delivery::Full);
        let mut released = false;
        seq.run(&data, None, &clock, 80128, |_, e| {
            assert!(!e.on);
            assert_eq!(e.offset, 0);
            released = true;
            Delivery::Taken
        });
        assert!(released);
    }
}
