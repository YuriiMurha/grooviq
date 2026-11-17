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
    pace(&mut SystemClock, rate, || {
        if quit.load(Ordering::Acquire) {
            return false;
        }
        slot.process(&mut buffer, CHANNELS);
        true
    });
}

/// Time as the pacing loop sees it: tests drive the very same loop on a synthetic one.
trait Clock {
    fn now(&mut self) -> Instant;
    fn sleep(&mut self, wait: Duration);
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, wait: Duration) {
        thread::sleep(wait);
    }
}

/// Calls `render` once per callback, until it returns false, waiting out each block's time.
fn pace(clock: &mut impl Clock, rate: u32, mut render: impl FnMut() -> bool) {
    let mut pacer = Pacer::new(clock.now(), rate);
    while render() {
        let wait = pacer.block_rendered(clock.now());
        if !wait.is_zero() {
            clock.sleep(wait);
        }
    }
}

/// The pace of the callbacks: an absolute schedule, block n ends at `start + n x block
/// duration`, so sleeps that overshoot never add up to drift.
///
/// A device does not catch up. A callback that is late by more than one block (the machine
/// slept, the thread was starved) has missed its slot: the backlog is dropped and the schedule
/// restarts from now, so the transport, the playhead and the meters never run ahead of the
/// clock. Within one block a late wake-up is made up by a shorter wait, which keeps the pace
/// exact. So over any stretch the callbacks render at most the clock's frames plus two blocks:
/// the one in flight and the one a late wake-up may owe.
struct Pacer {
    start: Instant,
    /// Frames rendered since `start`.
    frames: u64,
    rate: u64,
}

impl Pacer {
    fn new(now: Instant, rate: u32) -> Self {
        Pacer {
            start: now,
            frames: 0,
            rate: u64::from(rate.max(1)),
        }
    }

    /// One block was just rendered and the clock reads `now`: how long to wait before the next.
    fn block_rendered(&mut self, now: Instant) -> Duration {
        self.frames += CALLBACK_FRAMES as u64;
        // Whole seconds move into `start`, so `frames` stays small however long it runs.
        if self.frames >= self.rate {
            self.start += Duration::from_secs(1);
            self.frames -= self.rate;
        }
        // From the frame count, so the pace never drifts.
        let due = self.start + Duration::from_nanos(self.frames * 1_000_000_000 / self.rate);
        if now > due {
            // The block just rendered ran past its whole slot, which is one block of lateness:
            // it stands as the first of the new schedule.
            self.start = now;
            self.frames = CALLBACK_FRAMES as u64;
            return self.block();
        }
        due.saturating_duration_since(now)
    }

    /// How long one callback's frames last.
    fn block(&self) -> Duration {
        Duration::from_nanos(CALLBACK_FRAMES as u64 * 1_000_000_000 / self.rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, LiveShared, engine};
    use std::cell::Cell;
    use std::rc::Rc;
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

    /// Where block `n` falls on the ideal schedule, from the frame count as the pacer does.
    fn ideal(n: u64) -> Duration {
        Duration::from_nanos(n * CALLBACK_FRAMES as u64 * 1_000_000_000 / 48_000)
    }

    /// A clock that only moves when the loop sleeps or the test says the thread was late.
    struct FakeClock(Rc<Cell<Instant>>);

    impl Clock for FakeClock {
        fn now(&mut self) -> Instant {
            self.0.get()
        }

        fn sleep(&mut self, wait: Duration) {
            self.0.set(self.0.get() + wait);
        }
    }

    /// When each of `blocks` callbacks renders, as offsets from the start, when the production
    /// pacing loop runs them on a synthetic clock and `late(n)` is how long past its wait the
    /// thread wakes for block n (a stall, a slow wake-up). No real time passes.
    fn render_times(blocks: usize, mut late: impl FnMut(usize) -> Duration) -> Vec<Duration> {
        let base = Instant::now();
        let now = Rc::new(Cell::new(base));
        let mut times = Vec::with_capacity(blocks);
        pace(&mut FakeClock(Rc::clone(&now)), 48_000, || {
            let n = times.len();
            if n == blocks {
                return false;
            }
            now.set(now.get() + late(n));
            times.push(now.get() - base);
            true
        });
        times
    }

    /// The pacing guarantee: however the thread is delayed, no stretch of callbacks renders more
    /// than the clock allows, plus the one callback in flight and the one a late wake-up may
    /// owe. Catching a stall up in a burst would break it.
    fn assert_never_ahead_of_the_clock(times: &[Duration]) {
        let slack = 2.0 * CALLBACK_FRAMES as f64;
        for (i, first) in times.iter().enumerate() {
            for (j, last) in times.iter().enumerate().skip(i) {
                let rendered = ((j - i + 1) * CALLBACK_FRAMES) as f64;
                let allowed = (*last - *first).as_secs_f64() * 48_000.0 + slack;
                assert!(
                    rendered <= allowed + 1.0,
                    "blocks {i}..={j}: {rendered} frames in {allowed} frames' time"
                );
            }
        }
    }

    #[test]
    fn a_stall_is_dropped_and_not_caught_up_in_a_burst() {
        let times = render_times(400, |n| {
            if n == 100 {
                Duration::from_millis(40)
            } else {
                Duration::ZERO
            }
        });
        assert_never_ahead_of_the_clock(&times);
    }

    #[test]
    fn random_stalls_never_put_it_ahead_of_the_clock() {
        // A fixed pseudo-random delay mix: mostly a sub-millisecond wake-up, now and then a
        // few ms, rarely a stall of up to 120 ms.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let times = render_times(1_500, |_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let micros = match seed % 100 {
                0..=79 => seed >> 8 & 0x3ff,
                80..=94 => seed >> 8 & 0x1fff,
                _ => seed >> 8 & 0x1_d4bf,
            };
            Duration::from_micros(micros)
        });
        assert_never_ahead_of_the_clock(&times);
    }

    #[test]
    fn wake_ups_a_little_late_never_make_the_pace_drift() {
        // Every wake-up up to 1 ms late, 1 900 blocks (10 s): the schedule is absolute, so block
        // n still lands within that 1 ms of n x one block. A sleep of "one block" each time
        // would be about a second behind by now.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let times = render_times(1_900, |_| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            Duration::from_micros((seed >> 54) % 1_000)
        });
        for (n, t) in times.iter().enumerate() {
            let on_time = ideal(n as u64);
            assert!(
                *t <= on_time + Duration::from_millis(1),
                "block {n} at {t:?}, schedule says {on_time:?}"
            );
        }
    }

    #[test]
    fn on_time_wake_ups_land_exactly_on_the_schedule_and_never_wait_longer() {
        // 12 s, past the seconds rollover: block n renders at exactly n x one block, so a
        // minimum sleep, or a wait of more than the schedule asks, shows at once.
        let times = render_times(2_250, |_| Duration::ZERO);
        for (n, t) in times.iter().enumerate() {
            assert_eq!(*t, ideal(n as u64), "block {n}");
        }
    }

    #[test]
    fn a_run_of_days_stays_on_the_schedule() {
        // 5 days of callbacks, 81 million: the frame count in nanoseconds would overflow a u64
        // after about 4.4 days if whole seconds were not moved out of it.
        const BLOCKS: u128 = 81_000_000;
        let base = Instant::now();
        let now = Rc::new(Cell::new(base));
        let mut rendered = 0u128;
        pace(&mut FakeClock(Rc::clone(&now)), 48_000, || {
            rendered += 1;
            rendered <= BLOCKS
        });
        let on_time = BLOCKS * CALLBACK_FRAMES as u128 * 1_000_000_000 / 48_000;
        assert_eq!((now.get() - base).as_nanos(), on_time);
    }

    #[test]
    fn a_dropped_stall_waits_one_block_and_the_new_schedule_runs_to_the_next() {
        // Stalls well over a block, alone, back to back, and one 300 blocks on (so the
        // schedule restarted by the stall rolls over a second before the next).
        let stalls = [(100, 40), (130, 12), (131, 90), (132, 7), (440, 25)];
        let times = render_times(700, |n| {
            Duration::from_millis(stalls.iter().find(|s| s.0 == n).map_or(0, |s| s.1))
        });
        for pair in stalls.windows(2) {
            let (from, to) = (pair[0].0, pair[1].0);
            for n in from..to {
                // Exactly the restarted schedule: one block's wait after the stall, then the
                // absolute pace. Waiting 100 blocks after a stall would fail the first step.
                assert_eq!(
                    times[n] - times[from],
                    ideal((n - from) as u64),
                    "block {n} after the stall at {from}"
                );
            }
        }
        // And after the last stall, to the end: more than a second of it.
        for n in 440..700 {
            assert_eq!(times[n] - times[440], ideal((n - 440) as u64), "block {n}");
        }
    }

    #[test]
    fn a_short_lag_is_absorbed_by_the_schedule() {
        let times = render_times(200, |n| {
            if n == 50 {
                Duration::from_millis(3)
            } else {
                Duration::ZERO
            }
        });
        // The 3 ms is made up by the next wait: nothing is dropped, and block 60 is on time.
        assert_eq!(times[60], ideal(60));
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
        // One stream: about 48 000 frames a second, not two or three times that. Timed here,
        // as the sleep may last longer than asked on a busy runner.
        let (before, start) = (live.snapshot().frames, Instant::now());
        thread::sleep(Duration::from_millis(500));
        let rendered = (live.snapshot().frames - before) as f64;
        let one_stream = start.elapsed().as_secs_f64() * 48_000.0;
        assert!(
            rendered < one_stream * 1.5 + 512.0,
            "{rendered} frames, one stream renders {one_stream}"
        );
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
