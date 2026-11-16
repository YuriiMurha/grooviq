//! The transport: whether the pattern plays, at what tempo, and where each tick falls on the
//! engine's sample clock. The processor owns it, so it survives graph swaps.
//!
//! Ticks map to frames through an anchor `(anchor_frame, anchor_tick)`: a tick lands on the
//! first frame at or after its exact time, `anchor_frame + ceil((tick - anchor_tick) *
//! frames_per_tick)`. Play anchors tick 0 at the play block's start; a tempo change and a
//! restart at a new rate re-anchor at the current block start, so the tick position runs on
//! without a jump. All maths is f64 with basic IEEE operations only (exactly rounded on every
//! platform), so the same input lands on the same frame everywhere.

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
    /// `PlayLoops`: the number of loops to play (no note starts at or after `loops * length`).
    loops: Option<u32>,
}

impl TransportClock {
    pub(crate) fn new(sample_rate: f32, bpm: f32) -> Self {
        Self {
            playing: false,
            bpm,
            sample_rate,
            anchor_frame: 0,
            anchor_tick: 0.0,
            loops: None,
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
        self.loops = loops;
    }

    /// Stops and rewinds to tick 0.
    pub(crate) fn stop(&mut self) {
        self.playing = false;
        self.anchor_frame = 0;
        self.anchor_tick = 0.0;
        self.loops = None;
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
            self.anchor_tick = self.tick_at(at);
            self.anchor_frame = at;
        }
    }

    /// The frame `tick` lands on: the first at or after its exact time (before the anchor
    /// for a tick before the anchor's).
    pub(crate) fn frame_of(&self, tick: f64) -> u64 {
        let offset = exact_frames(
            tick - self.anchor_tick,
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

    /// The exact tick at `frame`: `anchor_tick + (frame - anchor_frame) / frames_per_tick`.
    pub(crate) fn tick_at(&self, frame: u64) -> f64 {
        let frames_per_tick = exact_frames(1.0, f64::from(self.bpm), f64::from(self.sample_rate));
        // Frames below 2^53 convert exactly.
        let elapsed = frame as f64 - self.anchor_frame as f64;
        self.anchor_tick + elapsed / frames_per_tick
    }

    /// Whole ticks since play started at `frame`, 0 while stopped.
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
}
