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

use crate::state::PPQ;

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
    pub(crate) fn new(sample_rate: f32, bpm: f32) -> Self {
        Self {
            playing: false,
            bpm,
            sample_rate,
            anchor_frame: 0,
            anchor_tick: 0.0,
            play_frame: 0,
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
        self.loops = loops;
        self.covered = 0;
        self.shift = 0;
    }

    /// Stops and rewinds to tick 0.
    pub(crate) fn stop(&mut self) {
        self.playing = false;
        self.anchor_frame = 0;
        self.anchor_tick = 0.0;
        self.play_frame = 0;
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
            self.anchor_tick = self.played_at(at);
            self.anchor_frame = at;
        }
    }

    /// A pattern of `old_len` ticks gave way to one of `new_len` ticks while playing: the
    /// pattern position moves forward (by less than `new_len`) to the place in the loop the
    /// old pattern had reached, or, when that is past the new end, to where it wraps. The
    /// frames already booked do not move. Integer maths only; real-time safe.
    pub(crate) fn keep_position(&mut self, old_len: u32, new_len: u32) {
        if !self.playing || old_len == new_len || old_len == 0 || new_len == 0 {
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
        let frames_per_tick = exact_frames(1.0, f64::from(self.bpm), f64::from(self.sample_rate));
        // Frames below 2^53 convert exactly.
        let elapsed = frame as f64 - self.anchor_frame as f64;
        self.anchor_tick + elapsed / frames_per_tick
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

    /// Where a note heard at `frame` belongs: the whole pattern tick at `frame - latency`
    /// frames (what was playing when the listener heard the note's sound), never before the
    /// frame play started on. Before a tempo change since then, the tick counts at the new
    /// tempo: off by at most `latency` frames' worth of the change.
    pub(crate) fn record_tick(&self, frame: u64, latency: u64) -> u64 {
        let heard = frame.saturating_sub(latency).max(self.play_frame);
        // Saturating: never negative.
        self.tick_at(heard).floor() as u64
    }

    /// The whole pattern tick at `frame` (ticks since play started, plus any shift), 0 while
    /// stopped. The UI takes it modulo the pattern length for the playhead.
    pub(crate) fn position_ticks(&self, frame: u64) -> u64 {
        if self.playing {
            // Saturating: never negative.
            self.tick_at(frame).floor() as u64
        } else {
            0
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
        t.keep_position(16 * 240, 32 * 240);
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
        t.keep_position(32 * 240, 3 * 240);
        assert_eq!(t.covered() % (3 * step), before % (32 * step) % (3 * step));
        assert!(t.covered() >= before && t.covered() - before < 3 * step);
        // The same length, or a stopped transport, changes nothing.
        let before = t.covered();
        t.keep_position(3 * 240, 3 * 240);
        assert_eq!(t.covered(), before);
        t.stop();
        t.keep_position(16 * 240, 17 * 240);
        assert_eq!(t.covered(), 0);
        // Play starts from tick 0 with no shift left over.
        t.start(0, None);
        assert_eq!(t.frame_of(240.0), 6_000);
    }
}
