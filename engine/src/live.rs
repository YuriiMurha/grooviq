//! Live state shared from the audio thread with any reader (the UI stream, the controller):
//! atomics only, so neither side ever waits on the other.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

/// One reading of the live state. Fields come from the same block or neighbouring ones (each
/// is atomic, the set is not), which is fine for meters and counters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveSnapshot {
    /// Master peak meter per channel (linear): instant attack, about 300 ms exponential release.
    pub peak: [f32; 2],
    /// The transport is running.
    pub playing: bool,
    /// The test tone is switched on.
    pub tone: bool,
    /// Frames the engine has rendered since it started (its sample clock, in 64-frame steps).
    pub frames: u64,
    /// Panics caught in the graph since the engine started.
    pub faults: u32,
    /// Node blocks the NaN guard silenced since the engine started.
    pub guard_hits: u32,
    /// Output latency in frames, as the output layer reports it.
    pub output_latency_frames: u32,
}

/// The atomics behind [`LiveSnapshot`]. The processor writes them once per block; the output
/// layer writes the latency.
#[derive(Debug, Default)]
pub struct LiveShared {
    peak_l: AtomicU32,
    peak_r: AtomicU32,
    playing: AtomicBool,
    tone: AtomicBool,
    frames: AtomicU64,
    faults: AtomicU32,
    guard_hits: AtomicU32,
    output_latency_frames: AtomicU32,
}

impl LiveShared {
    /// Reads every field. Never blocks; safe from any thread at any rate.
    pub fn snapshot(&self) -> LiveSnapshot {
        LiveSnapshot {
            peak: [
                f32::from_bits(self.peak_l.load(Relaxed)),
                f32::from_bits(self.peak_r.load(Relaxed)),
            ],
            playing: self.playing.load(Relaxed),
            tone: self.tone.load(Relaxed),
            frames: self.frames.load(Relaxed),
            faults: self.faults.load(Relaxed),
            guard_hits: self.guard_hits.load(Relaxed),
            output_latency_frames: self.output_latency_frames.load(Relaxed),
        }
    }

    /// Records the output latency (written by the output layer, not the processor).
    pub fn set_output_latency_frames(&self, frames: u32) {
        self.output_latency_frames.store(frames, Relaxed);
    }

    /// Everything the processor owns, once per block. Real-time safe: plain atomic stores.
    pub(crate) fn publish(
        &self,
        peak: [f32; 2],
        playing: bool,
        tone: bool,
        frames: u64,
        faults: u32,
        guard_hits: u32,
    ) {
        self.peak_l.store(peak[0].to_bits(), Relaxed);
        self.peak_r.store(peak[1].to_bits(), Relaxed);
        self.playing.store(playing, Relaxed);
        self.tone.store(tone, Relaxed);
        self.frames.store(frames, Relaxed);
        self.faults.store(faults, Relaxed);
        self.guard_hits.store(guard_hits, Relaxed);
    }
}
