//! The silent backend, for QA runs of the real app (`GROOVIQ_AUDIO=null`): one pretend device
//! that opens nothing. Its "stream" is a thread of its own that pulls audio from the slot in
//! 256-frame callbacks, paced by the clock as a device would, and throws the samples away. The
//! transport, the meters, the playhead and the latency report behave as on a device; nothing
//! is ever heard.

use super::backend::{AudioSlot, OutputBackend};
use super::{OutputInfo, OutputKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The silent device's id.
pub const NULL_OUTPUT_ID: &str = "null";
/// Its name, as the toolbar and the audio log show it.
const NAME: &str = "Silent (QA mode)";
/// The rate it runs at.
const RATE: u32 = 48_000;
/// Frames per callback: the buffer the real backend asks a device for.
const CALLBACK_FRAMES: usize = 256;
const CHANNELS: usize = 2;
/// Late by more than this (the machine slept, the thread was starved), the pacing starts
/// afresh from now, as a device drops what it missed, instead of rendering the backlog in a
/// burst.
const MAX_LAG: Duration = Duration::from_millis(50);

/// A backend with one silent device. It never lists or opens a real one.
pub fn null_backend() -> Box<dyn OutputBackend> {
    Box::new(NullBackend { stream: None })
}

struct NullBackend {
    stream: Option<Stream>,
}

/// The thread standing in for a device's callbacks.
struct Stream {
    quit: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl OutputBackend for NullBackend {
    fn outputs(&self) -> Vec<OutputInfo> {
        vec![OutputInfo {
            id: NULL_OUTPUT_ID.into(),
            name: NAME.into(),
            // Nobody hears it, so the speaker guard has nothing to protect.
            kind: OutputKind::Headphones,
            is_default: true,
            sample_rate: RATE,
        }]
    }

    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<u32, String> {
        self.close();
        if info.id != NULL_OUTPUT_ID {
            return Err(format!("the silent backend has no device {:?}", info.id));
        }
        let quit = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("grooviq-silent-audio".into())
            .spawn({
                let quit = Arc::clone(&quit);
                move || run_silent(&slot, &quit, RATE)
            })
            .map_err(|e| format!("the silent output thread did not start: {e}"))?;
        self.stream = Some(Stream { quit, thread });
        Ok(RATE)
    }

    fn close(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.quit.store(true, Ordering::Release);
            // Within one callback period. The slot's processor catches its own panics, so
            // there is nothing to pass on.
            let _ = stream.thread.join();
        }
    }
}

impl Drop for NullBackend {
    fn drop(&mut self) {
        self.close();
    }
}

/// The silent stream: until `quit`, fills a buffer from `slot` every 256 frames' worth of time
/// at `rate`, and discards it. Real-time safe, like a device callback: no allocation, lock, I/O
/// or logging (the slot only `try_lock`s). Public for the allocation test only.
#[doc(hidden)]
pub fn run_silent(slot: &AudioSlot, quit: &AtomicBool, rate: u32) {
    let mut buffer = [0f32; CALLBACK_FRAMES * CHANNELS];
    let rate = u64::from(rate.max(1));
    let mut start = Instant::now();
    let mut frames: u64 = 0;
    while !quit.load(Ordering::Acquire) {
        slot.process(&mut buffer, CHANNELS);
        frames += CALLBACK_FRAMES as u64;
        // Whole seconds move into `start`, so `frames` stays small however long it runs.
        if frames >= rate {
            start += Duration::from_secs(1);
            frames -= rate;
        }
        // From the frame count, so the pace never drifts.
        let due = start + Duration::from_nanos(frames * 1_000_000_000 / rate);
        let now = Instant::now();
        if now > due + MAX_LAG {
            start = now;
            frames = 0;
        } else if due > now {
            thread::sleep(due - now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, LiveShared, engine};
    use std::time::Instant;

    /// A slot that keeps its processor whatever the backend does, so any callback after
    /// `close` shows as frames rendered.
    fn slot() -> (Arc<AudioSlot>, Arc<LiveShared>, crate::Controller) {
        let (c, p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let live = c.live_shared();
        let slot = Arc::new(AudioSlot::new(Arc::clone(&live)));
        slot.put(p);
        (slot, live, c)
    }

    fn wait_for_frames(live: &LiveShared, more_than: u64) {
        let start = Instant::now();
        while live.snapshot().frames <= more_than {
            assert!(start.elapsed() < Duration::from_secs(5), "no callbacks");
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Frames rendered over `wait`, with nothing else touching the slot.
    fn frames_over(live: &LiveShared, wait: Duration) -> u64 {
        let before = live.snapshot().frames;
        thread::sleep(wait);
        live.snapshot().frames - before
    }

    #[test]
    fn once_close_returns_no_callback_runs() {
        let (slot, live, _c) = slot();
        let mut backend = null_backend();
        let info = backend.outputs().remove(0);
        backend.open(&info, Arc::clone(&slot)).unwrap();
        wait_for_frames(&live, 0);
        backend.close();
        assert_eq!(frames_over(&live, Duration::from_millis(40)), 0);
        // Closing again is harmless.
        backend.close();
    }

    #[test]
    fn opening_again_replaces_the_stream_rather_than_adding_one() {
        let (slot, live, _c) = slot();
        let mut backend = null_backend();
        let info = backend.outputs().remove(0);
        backend.open(&info, Arc::clone(&slot)).unwrap();
        backend.open(&info, Arc::clone(&slot)).unwrap();
        backend.open(&info, Arc::clone(&slot)).unwrap();
        wait_for_frames(&live, 0);
        // One stream: about 48 000 frames a second, not two or three times that.
        let rendered = frames_over(&live, Duration::from_millis(500));
        assert!(rendered < 24_000 + 2_048, "{rendered} frames in 0.5 s");
        drop(backend);
        assert_eq!(
            frames_over(&live, Duration::from_millis(40)),
            0,
            "Drop closes"
        );
    }

    #[test]
    fn a_device_it_does_not_have_is_refused() {
        let (slot, live, _c) = slot();
        let mut backend = null_backend();
        let mut info = backend.outputs().remove(0);
        info.id = "speakers".into();
        assert!(backend.open(&info, slot).is_err());
        assert_eq!(frames_over(&live, Duration::from_millis(40)), 0);
    }
}
