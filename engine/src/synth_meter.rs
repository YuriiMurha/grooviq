//! One leased synth observation. The audio writer never waits for a reader.
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};

pub const SYNTH_SCOPE_LEN: usize = 256;
#[derive(Clone, Debug, PartialEq)]
pub struct SynthMeter {
    pub instance: u64,
    /// Audio frame at capture, never the readback clock.
    pub frames: u64,
    /// Amp stage, filter stage, actual LFO phase.
    pub motion: [f32; 3],
    /// DSP meters: left/right peak, voices, LFO value, amp/filter envelope levels.
    pub levels: [f32; 6],
    /// Oldest to newest instrument-output samples, before channel mix gains.
    pub scope: [f32; SYNTH_SCOPE_LEN],
}

#[derive(Debug)]
pub(crate) struct SynthMeterShared {
    requested: AtomicU64,
    until: AtomicU64,
    sequence: AtomicU64,
    instance: AtomicU64,
    frames: AtomicU64,
    values: [AtomicU32; 6 + SYNTH_SCOPE_LEN + 3],
}
impl Default for SynthMeterShared {
    fn default() -> Self {
        Self {
            requested: AtomicU64::new(0),
            until: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            instance: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            values: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}
impl SynthMeterShared {
    pub(crate) fn request(&self, instance: u64, until: u64) {
        self.until.store(until, SeqCst);
        self.requested.store(instance, SeqCst);
    }
    pub(crate) fn selected(&self, clock: u64) -> u64 {
        if clock >= self.until.load(SeqCst) {
            0
        } else {
            self.requested.load(SeqCst)
        }
    }
    pub(crate) fn publish(&self, meter: Option<&SynthMeter>) {
        self.sequence.fetch_add(1, SeqCst);
        self.instance.store(meter.map_or(0, |m| m.instance), SeqCst);
        if let Some(m) = meter {
            self.frames.store(m.frames, SeqCst);
            for (dst, value) in self
                .values
                .iter()
                .zip(m.levels.iter().chain(m.scope.iter()).chain(m.motion.iter()))
            {
                dst.store(value.to_bits(), SeqCst);
            }
        }
        self.sequence.fetch_add(1, SeqCst);
    }
    pub(crate) fn read(&self, expected: u64) -> Option<SynthMeter> {
        let before = self.sequence.load(SeqCst);
        if !before.is_multiple_of(2) || expected == 0 || self.instance.load(SeqCst) != expected {
            return None;
        }
        let mut meter = SynthMeter {
            instance: expected,
            frames: self.frames.load(SeqCst),
            motion: [0.0; 3],
            levels: [0.0; 6],
            scope: [0.0; SYNTH_SCOPE_LEN],
        };
        for (dst, value) in meter
            .levels
            .iter_mut()
            .chain(meter.scope.iter_mut())
            .chain(meter.motion.iter_mut())
            .zip(&self.values)
        {
            *dst = f32::from_bits(value.load(SeqCst));
        }
        (before == self.sequence.load(SeqCst)).then_some(meter)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lease_expires_and_wrong_or_cleared_instances_are_never_read() {
        let shared = SynthMeterShared::default();
        let m = SynthMeter {
            instance: 7,
            frames: 3072,
            motion: [2.0, 4.0, 0.73],
            levels: [0.5; 6],
            scope: [0.25; SYNTH_SCOPE_LEN],
        };
        shared.request(7, 100);
        assert_eq!(shared.selected(99), 7);
        assert_eq!(shared.selected(100), 0);
        shared.publish(Some(&m));
        assert_eq!(shared.read(7), Some(m));
        assert_eq!(shared.read(8), None);
        shared.publish(None);
        assert_eq!(shared.read(7), None);
    }
    #[test]
    fn concurrent_reads_never_mix_capture_frame_and_fields() {
        let shared = SynthMeterShared::default();
        std::thread::scope(|threads| {
            threads.spawn(|| {
                for frames in 1..10_000 {
                    let value = frames as f32;
                    shared.publish(Some(&SynthMeter {
                        instance: 7,
                        frames,
                        levels: [value; 6],
                        scope: [value; SYNTH_SCOPE_LEN],
                        motion: [value; 3],
                    }));
                }
            });
            for _ in 0..10_000 {
                if let Some(meter) = shared.read(7) {
                    assert!(
                        meter
                            .levels
                            .iter()
                            .chain(meter.scope.iter())
                            .chain(meter.motion.iter())
                            .all(|v| *v == meter.frames as f32)
                    );
                }
            }
        });
    }

    #[test]
    fn an_in_progress_write_is_not_a_frame() {
        let shared = SynthMeterShared::default();
        shared.instance.store(7, SeqCst);
        shared.sequence.store(1, SeqCst);
        assert_eq!(shared.read(7), None);
    }
}
