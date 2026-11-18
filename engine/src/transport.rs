//! The transport: whether the pattern plays, at what tempo, and where each tick falls on the
//! engine's sample clock. The processor owns it, so it survives graph swaps.
//!
//! Ticks map to frames through an anchor `(anchor_frame, anchor_tick)`: a tick lands on the
//! first frame at or after its exact time, `anchor_frame + ceil((tick - anchor_tick) *
//! frames_per_tick)`. Play anchors tick 0 at the play block's start; a tempo change and a
//! restart at a new rate re-anchor at the current block start, so the tick position runs on
//! without a jump. All maths is f64 with basic IEEE operations only (exactly rounded on every
//! platform), so the same input lands on the same frame everywhere.
//!
//! Ticks here are pattern ticks: the ticks played since Play plus a whole-tick `shift`, which a
//! pattern of a new length adds so the place in the loop stays put (see
//! [`TransportClock::keep_position`]). The shift is an integer kept apart from the anchor, so
//! a note lands on exactly the frame it would have had without the swap.

use crate::state::{PPQ, STEP_TICKS};

/// The first frame at or after `ticks` ticks from frame 0 at `bpm`:
/// `ceil(ticks * sample_rate * 60 / (bpm * PPQ))`, evaluated left to right in f64.
/// Negative or non-finite results read as 0.
pub fn ticks_to_frames(ticks: f64, bpm: f32, sample_rate: f32) -> u64 {
    let frames = exact_frames(ticks, f64::from(bpm), f64::from(sample_rate)).ceil();
    // A float-to-int `as` saturates: NaN and negatives give 0.
    frames as u64
}

/// The exact (unrounded) frame count of `ticks` ticks. [`ticks_to_frames`] and the transport
/// share it so they round the same way.
fn exact_frames(ticks: f64, bpm: f64, sample_rate: f64) -> f64 {
    ticks * sample_rate * 60.0 / (bpm * f64::from(PPQ))
}

/// Segments kept for recording: a note heard up to this many tempo, rate or pattern-length
/// changes ago is placed at the tempo it was heard at, in the loop that was playing. At most one change lands per 64-frame
/// block, so this covers at least 4 096 frames even when every block changes tempo, and any
/// latency while changes are rarer (a BPM knob dragged at 60 Hz: about a second).
pub(crate) const TEMPO_HISTORY: usize = 64;

/// A stretch of play at one tempo and rate, in one pattern length and loop shift, from frame
/// `start` on. Its ticks are counted from the playback anchor in force then (`tick` ticks had
/// played at `frame`), exactly as playback counted them: a resize starts a segment without
/// moving that origin.
#[derive(Clone, Copy, Debug, Default)]
struct Segment {
    start: u64,
    frame: u64,
    tick: f64,
    bpm: f32,
    sample_rate: f32,
    shift: u64,
    length: u32,
}

impl Segment {
    /// The exact tick played since Play at `frame` (before `self.frame` too, extrapolated).
    fn played_at(&self, frame: u64) -> f64 {
        let frames_per_tick = exact_frames(1.0, f64::from(self.bpm), f64::from(self.sample_rate));
        // Frames below 2^53 convert exactly.
        let elapsed = frame as f64 - self.frame as f64;
        self.tick + elapsed / frames_per_tick
    }
}

/// The play state and the tick-to-frame map.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TransportClock {
    playing: bool,
    bpm: f32,
    sample_rate: f32,
    anchor_frame: u64,
    anchor_tick: f64,
    /// The frame play started on: a recorded note is never placed before it.
    play_frame: u64,
    /// The pattern length (ticks) playing, as the processor last reported it.
    length: u32,
    /// Where the current segment began (a tempo, rate or length change, or Play). Only
    /// recording looks back past it.
    since: u64,
    /// The segments before the current one since Play, newest at `history_next - 1` (a ring:
    /// the oldest is overwritten). Fixed size, so a tempo change never allocates.
    history: [Segment; TEMPO_HISTORY],
    history_len: usize,
    history_next: usize,
    /// `PlayLoops`: the number of loops to play (no note starts at or after `loops * length`).
    loops: Option<u32>,
    /// The first tick the blocks so far have not covered: every note before it has had its
    /// turn. A sequencer seeks from here, so a seek is exact whatever the tick-to-frame map
    /// does at the block start.
    covered: u64,
    /// Pattern ticks minus ticks played since Play: grows when a pattern of a new length keeps
    /// the place in the loop.
    shift: u64,
}

impl TransportClock {
    pub(crate) fn bpm(&self) -> f32 {
        self.bpm
    }
    pub(crate) fn new(sample_rate: f32, bpm: f32) -> Self {
        Self {
            playing: false,
            bpm,
            sample_rate,
            anchor_frame: 0,
            anchor_tick: 0.0,
            play_frame: 0,
            length: STEP_TICKS * 16,
            since: 0,
            history: [Segment::default(); TEMPO_HISTORY],
            history_len: 0,
            history_next: 0,
            loops: None,
            covered: 0,
            shift: 0,
        }
    }

    pub(crate) fn playing(&self) -> bool {
        self.playing
    }

    pub(crate) fn loops(&self) -> Option<u32> {
        self.loops
    }

    /// Plays from tick 0 at frame `at` (a block start), for `loops` loops or forever.
    pub(crate) fn start(&mut self, at: u64, loops: Option<u32>) {
        self.playing = true;
        self.anchor_frame = at;
        self.anchor_tick = 0.0;
        self.play_frame = at;
        self.since = at;
        self.history_len = 0;
        self.loops = loops;
        self.covered = 0;
        self.shift = 0;
    }

    // Pending musical releases use this basis so pattern-resize shifts cannot move them.
    pub(crate) fn release_map(&self) -> (u64, u64, u32, u32) {
        (
            self.anchor_frame,
            self.anchor_tick.to_bits(),
            self.bpm.to_bits(),
            self.sample_rate.to_bits(),
        )
    }
    pub(crate) fn played_tick_of(&self, tick: u64) -> u64 {
        tick - self.shift
    }
    pub(crate) fn frame_of_played_tick(&self, tick: u64) -> u64 {
        self.frame_of((tick + self.shift) as f64)
    }

    pub(crate) fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Maps an exact project-frame horizon through the current musical anchor.
    pub(crate) fn song_frame_of(&self, project_frame: u64) -> u64 {
        let anchor = exact_frames(
            self.anchor_tick,
            f64::from(self.bpm),
            f64::from(self.sample_rate),
        );
        let offset = (project_frame as f64 - anchor).ceil();
        if offset >= 0.0 {
            self.anchor_frame.saturating_add(offset as u64)
        } else {
            self.anchor_frame.saturating_sub((-offset) as u64)
        }
    }

    pub(crate) fn seek(&mut self, at: u64, tick: u64) {
        self.anchor_frame = at;
        self.anchor_tick = tick as f64;
        self.covered = tick;
        self.shift = 0;
        self.history_len = 0;
        self.since = at;
    }

    pub(crate) fn finish(&mut self, at: u64, tick: u64) {
        self.seek(at, tick);
        self.playing = false;
    }

    /// Stops and rewinds to tick 0.
    pub(crate) fn stop(&mut self) {
        self.playing = false;
        self.anchor_frame = 0;
        self.anchor_tick = 0.0;
        self.play_frame = 0;
        self.history_len = 0;
        self.loops = None;
        self.covered = 0;
        self.shift = 0;
    }

    /// Changes the tempo from frame `at` (a block start) on: the tick reached there stays put.
    pub(crate) fn set_bpm(&mut self, at: u64, bpm: f32) {
        self.reanchor(at);
        self.bpm = bpm;
    }

    /// Changes the sample rate from frame `at` (a block start) on, keeping the tick position.
    pub(crate) fn set_sample_rate(&mut self, at: u64, sample_rate: f32) {
        self.reanchor(at);
        self.sample_rate = sample_rate;
    }

    fn reanchor(&mut self, at: u64) {
        if self.playing {
            self.new_segment(at);
            self.anchor_tick = self.played_at(at);
            self.anchor_frame = at;
        }
    }

    /// Keeps the segment playing until frame `at` (a block start) for recording; what changes
    /// there starts the next. Call it before changing anything.
    fn new_segment(&mut self, at: u64) {
        // A segment that lasted no frame (two changes in one block) is not worth keeping.
        if at != self.since {
            self.history[self.history_next] = self.segment();
            self.history_next = (self.history_next + 1) % TEMPO_HISTORY;
            self.history_len = (self.history_len + 1).min(TEMPO_HISTORY);
            self.since = at;
        }
    }

    /// The segment playing now.
    fn segment(&self) -> Segment {
        Segment {
            start: self.since,
            frame: self.anchor_frame,
            tick: self.anchor_tick,
            bpm: self.bpm,
            sample_rate: self.sample_rate,
            shift: self.shift,
            length: self.length,
        }
    }

    /// A pattern of `old_len` ticks gave way to one of `new_len` ticks while playing: the
    /// pattern position moves forward (by less than `new_len`) to the place in the loop the
    /// old pattern had reached, or, when that is past the new end, to where it wraps. The
    /// frames already booked do not move. Integer maths only; real-time safe.
    ///
    /// `at` is the frame (a block start) the new pattern plays from: recording keeps the old
    /// length and shift for notes heard before it. Stopped, only the length is noted.
    pub(crate) fn keep_position(&mut self, at: u64, old_len: u32, new_len: u32) {
        if new_len == 0 {
            return;
        }
        if self.playing && new_len != self.length {
            self.new_segment(at);
        }
        self.length = new_len;
        if !self.playing || old_len == new_len || old_len == 0 {
            return;
        }
        let (old, new) = (u64::from(old_len), u64::from(new_len));
        let within = self.covered % old % new;
        let forward = (within + new - self.covered % new) % new;
        self.covered += forward;
        self.shift += forward;
    }

    /// The frame pattern tick `tick` lands on: the first at or after its exact time (before
    /// the anchor for a tick before the anchor's).
    pub(crate) fn frame_of(&self, tick: f64) -> u64 {
        // Whole ticks below 2^53 subtract exactly: with no shift this is `tick` itself.
        let played = tick - self.shift as f64;
        let offset = exact_frames(
            played - self.anchor_tick,
            f64::from(self.bpm),
            f64::from(self.sample_rate),
        )
        .ceil();
        if offset >= 0.0 {
            self.anchor_frame.saturating_add(offset as u64)
        } else {
            self.anchor_frame.saturating_sub((-offset) as u64)
        }
    }

    /// The exact pattern tick at `frame`.
    pub(crate) fn tick_at(&self, frame: u64) -> f64 {
        self.played_at(frame) + self.shift as f64
    }

    /// The exact tick played since Play at `frame`: `anchor_tick + (frame - anchor_frame) /
    /// frames_per_tick`.
    fn played_at(&self, frame: u64) -> f64 {
        self.segment().played_at(frame)
    }

    /// The exact tick played since Play at `frame`, a frame of this play (at or after
    /// `play_frame`), at the tempo and rate that played there. A frame older than the kept
    /// history falls between play start (tick 0) and the oldest segment kept: it is placed on
    /// the straight line between them (their average tempo), exact at both ends.
    /// Returns that tick with the segment that played there (its loop shift and length; past
    /// the kept history, the oldest kept one's).
    fn played_then(&self, frame: u64) -> (f64, Segment) {
        if frame >= self.since {
            // The playback map itself, so a note heard now lands as the sequencer counts.
            return (self.played_at(frame), self.segment());
        }
        let mut segment = self.segment();
        for back in 1..=self.history_len {
            segment = self.history[(self.history_next + TEMPO_HISTORY - back) % TEMPO_HISTORY];
            if frame >= segment.start {
                return (segment.played_at(frame), segment);
            }
        }
        if frame < self.play_frame || segment.start == self.play_frame {
            return (segment.played_at(frame), segment);
        }
        let span = (segment.start - self.play_frame) as f64;
        let tick = segment.played_at(segment.start) * ((frame - self.play_frame) as f64 / span);
        (tick, segment)
    }

    /// The first tick not covered yet (0 before the first block of a play).
    pub(crate) fn covered(&self) -> u64 {
        self.covered
    }

    /// Records that the block ending before frame `end` is done: every tick whose frame is
    /// before `end` is covered. Call it after the block, before the next one's commands change
    /// the map.
    ///
    /// Known limit: when more than a block's worth of notes (256) are due in one block, the
    /// rest start at the next block's start; a re-seek in exactly that next block skips them
    /// rather than replay the ones already started at their tick.
    pub(crate) fn mark_covered(&mut self, end: u64) {
        if !self.playing {
            return;
        }
        // The first tick that lands at or after `end`: start two ticks before the exact
        // tick there (a tick is at least 1.25 frames, so that one lands before `end`) and
        // step forward; a handful of steps.
        let mut tick = (self.tick_at(end).floor() - 2.0).max(0.0) as u64;
        for _ in 0..64 {
            if self.frame_of(tick as f64) >= end {
                break;
            }
            tick += 1;
        }
        self.covered = tick;
    }

    /// Where a note that lands at `frame` belongs: the whole tick played since Play at
    /// `frame - latency` (what the listener was hearing when they played it), never before
    /// the frame play started on, at the tempo and rate of that frame. Ticks count straight
    /// on from Play: a pattern of a new length (the sequencer's `shift`) does not move them, so
    /// a held note keeps its length.
    ///
    /// Returns that tick and its position in the loop that was playing there: `(tick + shift)
    /// % length` with the shift and pattern length of that frame, exactly the pattern tick the
    /// sequencer played.
    pub(crate) fn record_at(&self, frame: u64, latency: u64) -> (u64, u32) {
        let heard = frame.saturating_sub(latency);
        if heard <= self.play_frame {
            // Play's first frame: tick 0 of a loop that starts there (no shift yet), whatever
            // the history kept.
            return (0, 0);
        }
        let (played, segment) = self.played_then(heard);
        // Saturating: never negative.
        let tick = played.floor() as u64;
        // Below `length`, a u32.
        let position = (tick + segment.shift) % u64::from(segment.length.max(1));
        (tick, position as u32)
    }

    /// [`Self::record_at`]'s tick alone.
    #[cfg(test)]
    pub(crate) fn record_tick(&self, frame: u64, latency: u64) -> u64 {
        self.record_at(frame, latency).0
    }

    /// The whole pattern tick at `frame` (ticks since play started, plus any shift), 0 while
    /// stopped. The UI takes it modulo the pattern length for the playhead.
    pub(crate) fn position_ticks(&self, frame: u64) -> u64 {
        if self.playing {
            // Saturating: never negative.
            self.tick_at(frame).floor() as u64
        } else {
            self.anchor_tick as u64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_land_on_the_first_frame_at_or_after_their_time() {
        // 120 BPM at 48 kHz: 25 frames a tick, exactly.
        assert_eq!(ticks_to_frames(720.0, 120.0, 48_000.0), 18_000);
        // 16 537.5 rounds up.
        assert_eq!(ticks_to_frames(720.0, 120.0, 44_100.0), 16_538);
        assert_eq!(ticks_to_frames(240.0, 140.0, 48_000.0), 5_143);
        assert_eq!(ticks_to_frames(0.0, 140.0, 48_000.0), 0);
        assert_eq!(ticks_to_frames(-5.0, 140.0, 48_000.0), 0);
    }

    #[test]
    fn a_tempo_change_keeps_the_tick_reached() {
        let mut t = TransportClock::new(48_000.0, 120.0);
        t.start(6_400, None);
        assert_eq!(t.frame_of(0.0), 6_400);
        assert_eq!(t.frame_of(240.0), 6_400 + 6_000);
        // At frame 6 400 + 25 000: tick 1 000.
        let at = 6_400 + 25_000;
        assert_eq!(t.tick_at(at), 1_000.0);
        t.set_bpm(at, 60.0);
        assert_eq!(t.tick_at(at), 1_000.0);
        // 50 frames a tick from there.
        assert_eq!(t.frame_of(1_240.0), at + 12_000);
        assert_eq!(t.frame_of(999.0), at - 50);
        assert_eq!(t.position_ticks(at + 49), 1_000);
        assert_eq!(t.position_ticks(at + 50), 1_001);
        // A new rate keeps the tick too: 100 frames a tick at 96 kHz and 60 BPM.
        t.set_sample_rate(at + 5_000, 96_000.0);
        assert_eq!(t.tick_at(at + 5_000), 1_100.0);
        assert_eq!(t.frame_of(1_101.0), at + 5_100);
        t.stop();
        assert_eq!(t.position_ticks(at + 50), 0);
        assert!(!t.playing());
    }

    #[test]
    fn recorded_ticks_use_the_kept_tempo_history_and_ignore_the_shift() {
        // Tempo flips between 120 BPM (25 frames a tick) and 60 BPM (50) every block, more
        // often than the history holds.
        let mut t = TransportClock::new(48_000.0, 120.0);
        t.start(0, None);
        let changes = TEMPO_HISTORY + 20;
        let mut tick_at_change = vec![0.0];
        for i in 1..=changes {
            let at = 64 * i as u64;
            let frames_per_tick = if i % 2 == 1 { 25.0 } else { 50.0 };
            tick_at_change.push(tick_at_change[i - 1] + 64.0 / frames_per_tick);
            t.set_bpm(at, if i % 2 == 1 { 60.0 } else { 120.0 });
        }
        let now = 64 * changes as u64 + 10;
        // Every frame inside the kept segments maps at its own tempo.
        for back in 0..TEMPO_HISTORY as u64 {
            let heard = 64 * (changes as u64 - back) - 7;
            let i = (heard / 64) as usize;
            let frames_per_tick = if i.is_multiple_of(2) { 25.0 } else { 50.0 };
            let exact = tick_at_change[i] + (heard % 64) as f64 / frames_per_tick;
            assert_eq!(
                t.record_tick(now, now - heard),
                exact.floor() as u64,
                "{back}"
            );
        }
        // A pattern of a new length shifts playback, not recording.
        t.mark_covered(now);
        let before = t.record_tick(now, 0);
        let (c, position) = (t.covered(), t.position_ticks(now));
        // From an old length of c - 1 (one tick into its loop) to 7 or 5: a shift forward.
        let new = if c % 7 == 1 { 5 } else { 7 };
        t.keep_position(now, c as u32 - 1, new);
        assert!(t.position_ticks(now) > position, "playback shifted");
        assert_eq!(t.record_tick(now, 0), before);
        // A new play forgets the history.
        t.start(now, None);
        assert_eq!(t.record_tick(now + 2_500, 5_000), 0);
        assert_eq!(t.record_tick(now + 2_500, 0), 100);
    }

    #[test]
    fn past_the_kept_history_ticks_run_straight_from_play_start() {
        // Play at 6 400 at 400 BPM (7.5 frames a tick) for 6 blocks, then 20 BPM (150), with
        // a change every block: the ring drops the 400 BPM segments.
        let mut t = TransportClock::new(48_000.0, 400.0);
        t.start(6_400, None);
        let changes = TEMPO_HISTORY as u64 + 6;
        for i in 1..=changes {
            // The same tempo again still makes a segment.
            t.set_bpm(6_400 + 64 * i, if i < 6 { 400.0 } else { 20.0 });
        }
        let now = 6_400 + 64 * (changes + 1);
        // 6 x 64 frames at 7.5: tick 51.2 at frame 6 784, the oldest segment kept. Between
        // play start and there, the tempo was steady: the line is exact.
        assert_eq!(t.record_tick(now, now - 6_528), 17);
        assert_eq!(t.record_tick(now, now - 6_784), 51);
        // Before play start: tick 0 (extrapolating the 20 BPM segment back would give 48).
        assert_eq!(t.record_tick(now, now - 6_336), 0);
        // Now (10 944): 51.2 + 4 160 / 150.
        assert_eq!(t.record_tick(now, 0), 78);
    }

    #[test]
    fn a_note_heard_before_play_is_at_the_loop_start_after_the_history_overflows() {
        // A 240-tick pattern from frame 6 400 (120 BPM: 25 frames a tick), grown to 480 at
        // 12 800 (the place in the loop moves it 240 on), then more tempo anchors than the
        // history keeps: the oldest kept segment has the shift.
        let mut t = TransportClock::new(48_000.0, 120.0);
        t.keep_position(0, 240, 240);
        t.start(6_400, None);
        t.mark_covered(12_800);
        t.keep_position(12_800, 240, 480);
        for i in 1..=66 {
            t.set_bpm(12_800 + 64 * i, 120.0);
        }
        // Heard at 6 100, before Play: the start of the loop that began playing there.
        assert_eq!(t.record_at(17_100, 11_000), (0, 0));
    }

    #[test]
    fn a_resize_alone_never_moves_a_recorded_tick() {
        // 21 BPM at 48 kHz: 142.857... frames a tick, so frame 10 000 is tick 70 only as the
        // playback map rounds it from its anchor at Play.
        let clock = |resize: bool| {
            let mut t = TransportClock::new(48_000.0, 21.0);
            t.start(0, None);
            t.mark_covered(64);
            if resize {
                t.keep_position(64, 3_840, 5_760);
            }
            t
        };
        let (plain, resized) = (clock(false), clock(true));
        assert_eq!(plain.record_at(10_000, 0), (70, 70));
        assert_eq!(resized.record_at(10_000, 0), (70, 70));
        // The playback map does not move either.
        assert_eq!(plain.tick_at(10_000), resized.tick_at(10_000));
        assert_eq!(plain.frame_of(70.0), resized.frame_of(70.0));
    }

    #[test]
    fn a_new_pattern_length_keeps_the_place_in_the_loop() {
        // 120 BPM: 25 frames a tick. Step 20 of a 16-step pattern is step 4 of loop 2.
        let step = 240u64;
        let mut t = TransportClock::new(48_000.0, 120.0);
        t.start(0, None);
        let at = 20 * step * 25 + 64;
        t.mark_covered(at);
        let covered = t.covered();
        assert_eq!(covered / step, 20);
        let step_21 = t.frame_of((21 * step) as f64);
        // 16 -> 32 steps: the place stays step 4 (plus the same part of a step).
        t.keep_position(at, 16 * 240, 32 * 240);
        assert_eq!(t.covered() % (32 * step), covered % (16 * step));
        assert_eq!(
            t.position_ticks(at) % (32 * step),
            covered % (16 * step) - 1
        );
        // Step 5 of the new loop lands where step 21 would have: no frame moved.
        let next = t.covered() / (32 * step) * (32 * step) + 5 * step;
        assert_eq!(t.frame_of(next as f64), step_21);
        // 32 -> 3 steps from step 4 of 32: past the new end, so it wraps (step 1 of 3).
        let before = t.covered();
        t.keep_position(at, 32 * 240, 3 * 240);
        assert_eq!(t.covered() % (3 * step), before % (32 * step) % (3 * step));
        assert!(t.covered() >= before && t.covered() - before < 3 * step);
        // The same length, or a stopped transport, changes nothing.
        let before = t.covered();
        t.keep_position(at, 3 * 240, 3 * 240);
        assert_eq!(t.covered(), before);
        t.stop();
        t.keep_position(at, 16 * 240, 17 * 240);
        assert_eq!(t.covered(), 0);
        // Play starts from tick 0 with no shift left over.
        t.start(0, None);
        assert_eq!(t.frame_of(240.0), 6_000);
    }
}
