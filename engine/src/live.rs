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
    /// Whole ticks since play started, at the start of the latest block; 0 while stopped.
    pub position_ticks: u64,
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
    position_ticks: AtomicU64,
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
            position_ticks: self.position_ticks.load(Relaxed),
        }
    }

    /// Records the output latency (written by the output layer, not the processor).
    pub fn set_output_latency_frames(&self, frames: u32) {
        self.output_latency_frames.store(frames, Relaxed);
    }

    /// The transport's state, once per block. Real-time safe: plain atomic stores.
    pub(crate) fn publish_transport(&self, playing: bool, position_ticks: u64) {
        self.playing.store(playing, Relaxed);
        self.position_ticks.store(position_ticks, Relaxed);
    }

    /// Everything else the processor owns, once per block. Real-time safe: plain atomic
    /// stores.
    pub(crate) fn publish(
        &self,
        peak: [f32; 2],
        tone: bool,
        frames: u64,
        faults: u32,
        guard_hits: u32,
    ) {
        self.peak_l.store(peak[0].to_bits(), Relaxed);
        self.peak_r.store(peak[1].to_bits(), Relaxed);
        self.tone.store(tone, Relaxed);
        self.frames.store(frames, Relaxed);
        self.faults.store(faults, Relaxed);
        self.guard_hits.store(guard_hits, Relaxed);
    }
}

/// Version byte of the live-state wire format.
pub const LIVE_VERSION: u8 = 2;
/// Bytes in one encoded live-state frame.
pub const LIVE_LEN: usize = 40;

const FLAG_PLAYING: u8 = 1;
const FLAG_TONE: u8 = 1 << 1;

/// Encodes a snapshot as one 40-byte little-endian frame (the stream's wire format, v2):
/// `[0]` version, `[1]` flags (bit 0 playing, bit 1 tone), `[2..4]` zero, `[4..8]` peak L f32,
/// `[8..12]` peak R f32, `[12..20]` frames u64, `[20..24]` faults u32, `[24..28]` guard hits
/// u32, `[28..32]` output latency frames u32, `[32..40]` position ticks u64.
pub fn encode_live(s: &LiveSnapshot) -> [u8; LIVE_LEN] {
    let mut b = [0u8; LIVE_LEN];
    b[0] = LIVE_VERSION;
    b[1] = (u8::from(s.playing) * FLAG_PLAYING) | (u8::from(s.tone) * FLAG_TONE);
    b[4..8].copy_from_slice(&s.peak[0].to_le_bytes());
    b[8..12].copy_from_slice(&s.peak[1].to_le_bytes());
    b[12..20].copy_from_slice(&s.frames.to_le_bytes());
    b[20..24].copy_from_slice(&s.faults.to_le_bytes());
    b[24..28].copy_from_slice(&s.guard_hits.to_le_bytes());
    b[28..32].copy_from_slice(&s.output_latency_frames.to_le_bytes());
    b[32..40].copy_from_slice(&s.position_ticks.to_le_bytes());
    b
}

/// Decodes a frame; `None` for a wrong length or an unknown version.
pub fn decode_live(b: &[u8]) -> Option<LiveSnapshot> {
    let b: &[u8; LIVE_LEN] = b.try_into().ok()?;
    if b[0] != LIVE_VERSION {
        return None;
    }
    let f32_at = |i: usize| f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    Some(LiveSnapshot {
        peak: [f32_at(4), f32_at(8)],
        playing: b[1] & FLAG_PLAYING != 0,
        tone: b[1] & FLAG_TONE != 0,
        frames: u64_at(12),
        faults: u32_at(20),
        guard_hits: u32_at(24),
        output_latency_frames: u32_at(28),
        position_ticks: u64_at(32),
    })
}
