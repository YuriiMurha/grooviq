//! The step sequencer: turns the pattern into note events on their exact frames, one block at
//! a time. The graph owns it next to its pattern, so a rebuilt graph replays the pattern from
//! where the transport is.
//!
//! A cursor walks the pattern's notes loop after loop: a note at `tick` in loop `k` starts at
//! tick `k * length + tick` (ticks since play started), on the frame the transport maps it to.
//! Every note-on that a channel takes leaves a note-off in a fixed table of [`PENDING`] slots,
//! due at the note's end. The cursor re-seeks on Play, on a pattern swap and in a fresh graph:
//! to the first note at or after the first tick the previous blocks have not covered (which
//! the transport keeps). Seeking by tick, not by frame, keeps that exact when the tick-to-frame
//! map changed at the block start (a tempo change or a new rate in the same block), so a seek
//! neither replays a note nor skips one.
//!
//! Real-time: nothing here allocates, frees or locks.

use crate::BLOCK;
use crate::command::NoteId;
use crate::state::{ChannelId, PatternData, SeqNote};
use crate::transport::TransportClock;
use grooviq_dsp::NoteEvent;

/// Note-offs waiting for their frame. When the table is full, a note-on is skipped too.
pub(crate) const PENDING: usize = 1024;
/// Notes one block looks at; the rest start at the next block's start.
const MAX_EVENTS: usize = 256;

/// What a channel did with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    Taken,
    /// The channel's buffer for this block is full: a note-on is dropped, a note-off waits.
    Full,
    /// No such channel: the event is dropped.
    NoChannel,
}

#[derive(Clone, Copy, Debug)]
struct PendingOff {
    frame: u64,
    channel: ChannelId,
    key: u8,
    /// The id its note-on carried.
    id: u64,
}

const NO_OFF: PendingOff = PendingOff {
    frame: 0,
    channel: ChannelId(0),
    key: 0,
    id: 0,
};

pub(crate) struct Sequencer {
    /// The next note to start: its index among the pattern's notes, in loop `loop_index`.
    next: usize,
    loop_index: u64,
    /// The cursor must be placed again before the next note.
    seek: bool,
    pending: [PendingOff; PENDING],
    count: usize,
    /// No pending note-off is due before this frame (`u64::MAX` when none waits).
    earliest: u64,
    /// Numbers the notes played: each note-on and its note-off carry the id it makes, so a
    /// note-off ends its own note, not another one on its key (a short note inside a long one,
    /// a live note). Ids have [`NoteId::SEQUENCED`] set, which a live note never has.
    played: u64,
}

impl Sequencer {
    pub(crate) fn new() -> Self {
        Self {
            next: 0,
            loop_index: 0,
            seek: true,
            pending: [NO_OFF; PENDING],
            count: 0,
            earliest: u64::MAX,
            played: 0,
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
        pattern: &PatternData,
        clock: &TransportClock,
        f0: u64,
        mut sink: impl FnMut(ChannelId, NoteEvent) -> Delivery,
    ) {
        let f1 = f0 + BLOCK as u64;
        self.flush(f0, f1 - 1, &mut sink);
        if !clock.playing() {
            return;
        }
        // Notes past the end never play (the list is sorted by tick).
        let in_range = pattern.notes.partition_point(|n| n.tick < pattern.length);
        let notes = &pattern.notes[..in_range];
        if notes.is_empty() {
            return;
        }
        let length = u64::from(pattern.length);
        let end = clock.loops().map(|loops| u64::from(loops) * length);
        if std::mem::take(&mut self.seek) {
            self.seek_to(notes, length, clock.covered());
        }
        for _ in 0..MAX_EVENTS {
            let Some(&note) = notes.get(self.next) else {
                // Only a pattern swap without a re-seek could leave the cursor past the end.
                self.next = 0;
                self.loop_index += 1;
                continue;
            };
            let tick = self.loop_index * length + u64::from(note.tick);
            if end.is_some_and(|end| tick >= end) {
                break;
            }
            let frame = clock.frame_of(tick as f64);
            if frame >= f1 {
                break;
            }
            // A note the cursor has not passed is never lost: one that is late (a tempo
            // change moved it before the block, or the block was full) starts at offset 0.
            let frame = frame.max(f0);
            // A note-off at this frame goes first, so a retriggered key is not cut.
            self.flush(f0, frame, &mut sink);
            self.start(note, tick, frame - f0, clock, &mut sink);
            self.next += 1;
            if self.next == notes.len() {
                self.next = 0;
                self.loop_index += 1;
            }
        }
        // Note-offs of notes that started and end within this block.
        self.flush(f0, f1 - 1, &mut sink);
    }

    /// Sends `note`'s note-on at `offset` and books its note-off, if the table has room and the
    /// channel takes it.
    fn start(
        &mut self,
        note: SeqNote,
        tick: u64,
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
        let end = tick + u64::from(note.length.max(1));
        let frame = clock.frame_of(end as f64);
        self.pending[self.count] = PendingOff {
            frame,
            channel: note.channel,
            key: note.key,
            id,
        };
        self.count += 1;
        self.earliest = self.earliest.min(frame);
    }

    /// Sends every pending note-off due at or before frame `through` (late ones at offset 0).
    /// One whose channel is full waits for the next block.
    fn flush(
        &mut self,
        f0: u64,
        through: u64,
        sink: &mut impl FnMut(ChannelId, NoteEvent) -> Delivery,
    ) {
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

    /// Points the cursor at the first note that starts at or after tick `from` (integer
    /// arithmetic only).
    fn seek_to(&mut self, notes: &[SeqNote], length: u64, from: u64) {
        self.loop_index = from / length;
        let within = from % length;
        self.next = notes.partition_point(|n| u64::from(n.tick) < within);
        if self.next == notes.len() {
            self.next = 0;
            self.loop_index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SR: f32 = 48_000.0;

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Sent {
        frame: u64,
        channel: u32,
        key: u8,
        on: bool,
    }

    fn note(tick: u32, channel: u32, key: u8, length: u32) -> SeqNote {
        SeqNote {
            tick,
            channel: ChannelId(channel),
            key,
            velocity: 1.0,
            length,
            slide: false,
        }
    }

    /// Runs `blocks` blocks from frame `from`, recording what every channel takes, and marks
    /// each block covered as the processor does.
    fn record(
        seq: &mut Sequencer,
        pattern: &PatternData,
        clock: &mut TransportClock,
        from: u64,
        blocks: u64,
    ) -> Vec<Sent> {
        let mut sent = Vec::new();
        for b in 0..blocks {
            let f0 = from + b * BLOCK as u64;
            seq.run(pattern, clock, f0, |ch, e| {
                sent.push(Sent {
                    frame: f0 + u64::from(e.offset),
                    channel: ch.0,
                    key: e.key,
                    on: e.on,
                });
                Delivery::Taken
            });
            clock.mark_covered(f0 + BLOCK as u64);
        }
        sent
    }

    fn playing(bpm: f32) -> TransportClock {
        let mut clock = TransportClock::new(SR, bpm);
        clock.start(0, None);
        clock
    }

    #[test]
    fn adjacent_steps_release_before_they_retrigger() {
        // Steps 0 and 1 on one key, each a step long; then two 1-tick notes in one block.
        let pattern = PatternData {
            length: 16 * 240,
            notes: vec![
                note(0, 1, 60, 240),
                note(240, 1, 60, 240),
                note(960, 1, 62, 1),
                note(961, 1, 62, 1),
            ],
        };
        let mut clock = playing(120.0);
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut clock, 0, 1_000);
        let at = |frame: u64, key: u8| -> Vec<bool> {
            sent.iter()
                .filter(|s| s.frame == frame && s.key == key)
                .map(|s| s.on)
                .collect()
        };
        assert_eq!(at(0, 60), [true]);
        // Step 1 (frame 6 000): the first note ends, then the second starts.
        assert_eq!(at(6_000, 60), [false, true]);
        assert_eq!(at(12_000, 60), [false]);
        // Tick 961 is frame 24 025, in the block that tick 960 starts.
        assert_eq!(at(24_000, 62), [true]);
        assert_eq!(at(24_025, 62), [false, true]);
        assert_eq!(at(24_050, 62), [false]);
    }

    /// The ids of the note-ons and note-offs of the block at `f0`, as (key, on, id).
    fn ids(
        seq: &mut Sequencer,
        pattern: &PatternData,
        clock: &mut TransportClock,
        f0: u64,
    ) -> Vec<(u8, bool, NoteId)> {
        let mut sent = Vec::new();
        seq.run(pattern, clock, f0, |_, e| {
            sent.push((e.key, e.on, NoteId(e.id)));
            Delivery::Taken
        });
        clock.mark_covered(f0 + BLOCK as u64);
        sent
    }

    #[test]
    fn a_held_note_never_shares_its_id_with_a_later_note_past_the_old_31_bit_wrap() {
        // A long note on key 60 starts as the sequencer's first note; 2^31 notes later (where a
        // 31-bit counter wrapped) another note on that key starts while the first is still held.
        // Their ids differ, so the first one's note-off ends only the first.
        let pattern = PatternData {
            length: 16 * 240,
            notes: vec![note(0, 1, 60, 3_000), note(240, 1, 60, 240)],
        };
        let mut clock = playing(120.0);
        let mut seq = Sequencer::new();
        let first = ids(&mut seq, &pattern, &mut clock, 0);
        assert_eq!(first.len(), 1);
        // The next note is number 2^31 (the first was number 0).
        seq.played += (1 << 31) - 1;
        let mut later = Vec::new();
        for b in 1..=100 {
            later.extend(ids(&mut seq, &pattern, &mut clock, b * BLOCK as u64));
        }
        let second = later.iter().find(|e| e.1).expect("the second note starts");
        assert_ne!(second.2, first[0].2, "the held note's id was given again");
        assert!(second.2.0 & NoteId::SEQUENCED != 0);
    }

    /// A tiny xorshift: the fuzz below is the same on every run and needs no crate.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn random_pattern(rng: &mut Rng) -> PatternData {
        let length = 1 + rng.below(8 * 240) as u32;
        let mut notes: Vec<SeqNote> = (0..rng.below(60))
            .map(|_| {
                note(
                    // Some past the end, which never play.
                    rng.below(u64::from(length) + 200) as u32,
                    1 + rng.below(4) as u32,
                    // Few keys, so notes on one key overlap.
                    60 + rng.below(3) as u8,
                    match rng.below(4) {
                        0 => 0,
                        1 => 1 + rng.below(30) as u32,
                        _ => rng.below(4_000) as u32,
                    },
                )
            })
            .collect();
        notes.sort_by_key(|n| (n.tick, n.channel));
        PatternData { length, notes }
    }

    #[test]
    fn every_note_on_gets_its_note_off() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        // Events the channels took, over every round: the fuzz is not vacuous.
        let mut taken = 0u64;
        for round in 0..200 {
            let mut clock = TransportClock::new(SR, 20.0 + rng.below(380) as f32);
            let mut seq = Sequencer::new();
            let mut pattern = random_pattern(&mut rng);
            let mut present = [true; 5];
            // Ons minus offs per (channel, key).
            let mut held: HashMap<(u32, u8), i64> = HashMap::new();
            let mut f0 = 0;
            clock.start(f0, None);
            for _ in 0..1_500 {
                match rng.below(60) {
                    0 => {
                        clock.set_bpm(f0, 20.0 + rng.below(380) as f32);
                    }
                    1 => {
                        pattern = random_pattern(&mut rng);
                        seq.reseek();
                    }
                    2 => {
                        let ch = 1 + rng.below(4) as usize;
                        present[ch] = false;
                        seq.forget_channel(ChannelId(ch as u32));
                        held.retain(|k, _| k.0 != ch as u32);
                    }
                    3 => present[1 + rng.below(4) as usize] = true,
                    4 => {
                        clock.stop();
                        seq.stop();
                    }
                    5 => {
                        clock.start(f0, None);
                        seq.reseek();
                    }
                    _ => {}
                }
                // A full channel now and then: note-ons are dropped, note-offs wait.
                let full = rng.below(10) == 0;
                seq.run(&pattern, &clock, f0, |ch, e| {
                    if !present[ch.0 as usize] {
                        return Delivery::NoChannel;
                    }
                    if full {
                        return Delivery::Full;
                    }
                    *held.entry((ch.0, e.key)).or_default() += if e.on { 1 } else { -1 };
                    taken += 1;
                    Delivery::Taken
                });
                clock.mark_covered(f0 + BLOCK as u64);
                f0 += BLOCK as u64;
            }
            clock.stop();
            seq.stop();
            seq.run(&pattern, &clock, f0, |ch, e| {
                if !present[ch.0 as usize] {
                    return Delivery::NoChannel;
                }
                *held.entry((ch.0, e.key)).or_default() += if e.on { 1 } else { -1 };
                Delivery::Taken
            });
            assert_eq!(seq.pending(), 0, "round {round}");
            for (k, v) in &held {
                assert_eq!(*v, 0, "round {round}: channel {} key {}", k.0, k.1);
            }
        }
        assert!(taken > 20_000, "{taken}");
    }

    #[test]
    fn pending_note_offs_never_overflow() {
        // More notes than the table holds, all held for the whole pattern, one tick apart
        // (40 ticks a block at 400 BPM and 48 kHz: 7.5 frames a tick).
        let count = PENDING as u32 + 100;
        let length = count + 10_000;
        let pattern = PatternData {
            length,
            notes: (0..count)
                .map(|t| note(t, 1, (t % 128) as u8, length))
                .collect(),
        };
        let mut clock = playing(400.0);
        let mut seq = Sequencer::new();
        let blocks = u64::from(count) * 15 / 2 / BLOCK as u64 + 2;
        let sent = record(&mut seq, &pattern, &mut clock, 0, blocks);
        assert_eq!(seq.pending(), PENDING);
        let ons = sent.iter().filter(|s| s.on).count();
        assert_eq!(ons, PENDING, "the rest were skipped, note-on and note-off");
        assert!(sent.iter().all(|s| s.on));
        // Once the notes end, everything taken is released.
        let frames_to_end = clock.frame_of(f64::from(2 * length));
        let rest = record(
            &mut seq,
            &pattern,
            &mut clock,
            blocks * BLOCK as u64,
            frames_to_end / BLOCK as u64,
        );
        let offs = rest.iter().filter(|s| !s.on).count();
        assert!(offs >= PENDING, "{offs}");
    }

    #[test]
    fn a_seek_finds_the_note_on_the_block_start_and_skips_earlier_ones() {
        let pattern = PatternData {
            length: 3_840,
            notes: vec![note(0, 1, 60, 10), note(1_600, 1, 61, 10)],
        };
        // Tick 1 600 is frame 40 000, a block start. A fresh sequencer there (a rebuilt
        // graph) plays it, and nothing before it.
        let mut clock = playing(120.0);
        let mut first = Sequencer::new();
        let before = record(&mut first, &pattern, &mut clock, 0, 40_000 / BLOCK as u64);
        assert_eq!(before.iter().filter(|s| s.on).count(), 1, "tick 0 only");
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut clock, 40_000, 1);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].frame, sent[0].key), (40_000, 61));
        // One block later, the next is the following loop's first note.
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut clock, 40_064, 1_500);
        assert_eq!((sent[0].frame, sent[0].key), (96_000, 60));
    }

    #[test]
    fn play_loops_ends_the_notes_but_not_their_note_offs() {
        let pattern = PatternData {
            length: 960,
            notes: vec![note(0, 1, 60, 900)],
        };
        let mut clock = TransportClock::new(SR, 120.0);
        clock.start(0, Some(2));
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut clock, 0, 2_000);
        let frames: Vec<(u64, bool)> = sent.iter().map(|s| (s.frame, s.on)).collect();
        assert_eq!(
            frames,
            [(0, true), (22_500, false), (24_000, true), (46_500, false)]
        );
    }

    #[test]
    fn notes_beyond_one_blocks_share_start_at_the_next_block_instead_of_being_lost() {
        // 300 notes on tick 0: a block looks at 256, the other 44 start at the next block's
        // start.
        let pattern = PatternData {
            length: 3_840,
            notes: (0..300u32)
                .map(|i| note(0, 1 + i % 3, (i / 3) as u8, 10))
                .collect(),
        };
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut playing(120.0), 0, 2);
        let ons: Vec<u64> = sent.iter().filter(|s| s.on).map(|s| s.frame).collect();
        assert_eq!(ons.len(), 300);
        assert_eq!(ons.iter().filter(|&&f| f == 0).count(), MAX_EVENTS);
        assert_eq!(
            ons.iter().filter(|&&f| f == BLOCK as u64).count(),
            300 - MAX_EVENTS
        );
    }

    #[test]
    fn a_seek_right_after_an_overfull_block_never_replays_a_burst() {
        // 300 notes on tick 0: 256 start in the first block. A re-seek in the next block (a
        // pattern swap) moves on from the covered ticks: the 256 do not start again.
        let pattern = PatternData {
            length: 3_840,
            notes: (0..300u32)
                .map(|i| note(0, 1 + i % 3, (i / 3) as u8, 10))
                .collect(),
        };
        let mut clock = playing(120.0);
        let mut seq = Sequencer::new();
        let mut sent = record(&mut seq, &pattern, &mut clock, 0, 1);
        seq.reseek();
        sent.extend(record(&mut seq, &pattern, &mut clock, BLOCK as u64, 100));
        let ons = sent.iter().filter(|s| s.on).count();
        assert_eq!(ons, MAX_EVENTS);
    }

    #[test]
    fn a_zero_length_note_is_held_for_one_tick() {
        let pattern = PatternData {
            length: 3_840,
            notes: vec![note(0, 1, 60, 0)],
        };
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut playing(120.0), 0, 2);
        let got: Vec<(u64, bool)> = sent.iter().map(|s| (s.frame, s.on)).collect();
        // 25 frames a tick at 120 BPM and 48 kHz.
        assert_eq!(got, [(0, true), (25, false)]);
    }

    #[test]
    fn notes_at_or_past_the_pattern_end_never_play() {
        let pattern = PatternData {
            length: 960,
            notes: vec![
                note(0, 1, 60, 10),
                note(960, 1, 61, 10),
                note(1_000, 1, 62, 10),
            ],
        };
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut playing(120.0), 0, 1_000);
        assert!(sent.iter().all(|s| s.key == 60), "{sent:?}");
        assert_eq!(
            sent.iter().filter(|s| s.on).count(),
            3,
            "loops at 0, 24 000, 48 000"
        );
    }

    #[test]
    fn a_seek_after_a_speed_up_in_the_same_block_does_not_replay() {
        // Tick 23 is frame 575 at 120 BPM. At the next block (576), 140 BPM and a re-seek in
        // the same block: under the new map tick 23 lands on 576, but it has played.
        let pattern = PatternData {
            length: 3_840,
            notes: vec![note(23, 1, 60, 1), note(100, 1, 61, 1)],
        };
        let mut clock = playing(120.0);
        let mut seq = Sequencer::new();
        let mut sent = record(&mut seq, &pattern, &mut clock, 0, 9);
        clock.set_bpm(576, 140.0);
        seq.reseek();
        sent.extend(record(&mut seq, &pattern, &mut clock, 576, 40));
        let ons: Vec<(u64, u8)> = sent
            .iter()
            .filter(|s| s.on)
            .map(|s| (s.frame, s.key))
            .collect();
        let tick_100 = clock.frame_of(100.0);
        assert_eq!(ons, [(575, 60), (tick_100, 61)]);
    }

    #[test]
    fn a_seek_after_a_tempo_change_in_the_same_block_skips_nothing() {
        // A note on every tick (its channel names the tick). Tempo changes both ways, each
        // with a re-seek in the same block: every tick still plays once, in order.
        let pattern = PatternData {
            length: 960,
            notes: (0..960).map(|t| note(t, t, 60, 1)).collect(),
        };
        let mut clock = playing(140.0);
        let mut seq = Sequencer::new();
        let mut sent = Vec::new();
        let mut f0 = 0;
        for (i, bpm) in [60.0, 140.0, 61.0, 400.0, 20.0, 333.0, 59.5, 140.0]
            .into_iter()
            .enumerate()
        {
            sent.extend(record(
                &mut seq,
                &pattern,
                &mut clock,
                f0,
                37 + i as u64 * 11,
            ));
            f0 += (37 + i as u64 * 11) * BLOCK as u64;
            clock.set_bpm(f0, bpm);
            seq.reseek();
        }
        sent.extend(record(&mut seq, &pattern, &mut clock, f0, 50));
        let ticks: Vec<u32> = sent.iter().filter(|s| s.on).map(|s| s.channel).collect();
        let covered = clock.covered();
        assert!(covered > 1_500, "{covered}");
        let expected: Vec<u32> = (0..covered).map(|t| (t % 960) as u32).collect();
        assert_eq!(ticks, expected);
    }

    #[test]
    fn play_loops_releases_every_note_without_a_stop() {
        // Two notes at tick 0, ending at ticks 100 and 1 000: both note-offs arrive, with
        // no Stop to flush them.
        let pattern = PatternData {
            length: 3_840,
            notes: vec![note(0, 1, 60, 100), note(0, 1, 61, 1_000)],
        };
        let mut clock = TransportClock::new(SR, 120.0);
        clock.start(0, Some(1));
        let mut seq = Sequencer::new();
        let sent = record(&mut seq, &pattern, &mut clock, 0, 2_000);
        let offs: Vec<(u8, u64)> = sent
            .iter()
            .filter(|s| !s.on)
            .map(|s| (s.key, s.frame))
            .collect();
        assert_eq!(offs, [(60, 2_500), (61, 25_000)]);
        assert_eq!(seq.pending(), 0);
    }

    #[test]
    fn an_empty_pattern_and_a_stopped_transport_send_nothing() {
        let mut clock = playing(120.0);
        let mut seq = Sequencer::new();
        assert!(record(&mut seq, &PatternData::default(), &mut clock, 0, 100).is_empty());
        let mut stopped = TransportClock::new(SR, 120.0);
        let pattern = PatternData {
            length: 960,
            notes: vec![note(0, 1, 60, 10)],
        };
        assert!(record(&mut seq, &pattern, &mut stopped, 0, 100).is_empty());
    }
}
