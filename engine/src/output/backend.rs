//! [`AudioOutput`]: owns the processor and keeps it on the output the [`OutputMonitor`] picks.
//!
//! A poll thread lists the devices every 500 ms (cpal has no default-device notification), and
//! a stream error wakes it at once. While a stream is open, the processor sits in the
//! [`AudioSlot`], which the device callback only `try_lock`s. While none is, the output layer
//! holds the processor itself, so it can reconfigure it for a new rate or run it silent.

use super::{OutputChoice, OutputEvent, OutputInfo, OutputMonitor};
use crate::{AudioProcessor, BLOCK, Command, Controller, EngineConfig, LiveShared};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

/// How often the poll thread lists the devices.
const POLL_PERIOD: Duration = Duration::from_millis(500);
/// The shortest gap between two looks, for a stream that keeps failing.
const RETRY_GAP: Duration = Duration::from_millis(50);
/// How long a guard stop runs the processor with no stream open: twice the 5 ms fade of
/// `AllSoundOff`.
const GUARD_SILENCE_SECONDS: f32 = 0.010;
/// Blocks a guard stop renders at most to make room for `AllSoundOff` in a full queue. Each
/// block takes every due command but notes, and up to 256 notes, so a 1024-slot queue drains
/// in four.
const MAX_DRAIN_BLOCKS: usize = 64;

/// A way to play audio on output devices.
pub trait OutputBackend: Send {
    /// Every output device, each with the rate `open` runs it at. Never lists an input device.
    fn outputs(&self) -> Vec<OutputInfo>;
    /// Opens the device `info` describes and starts calling [`AudioSlot::process`] from its
    /// callback. Any previous stream is already closed.
    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<(), String>;
    /// Stops and closes the open stream, if any. Once this returns, its callback never runs
    /// again.
    fn close(&mut self);
}

/// What an open stream's callback pulls audio from: the processor (absent while no stream is
/// open), and where the stream reports its latency and its failures.
pub struct AudioSlot {
    processor: Mutex<Option<AudioProcessor>>,
    live: Arc<LiveShared>,
    /// The open stream reported a failure that the next look must handle.
    failed: AtomicBool,
    /// The poll thread, woken when a stream fails.
    poller: OnceLock<Thread>,
}

impl AudioSlot {
    fn new(live: Arc<LiveShared>) -> Self {
        Self {
            processor: Mutex::new(None),
            live,
            failed: AtomicBool::new(false),
            poller: OnceLock::new(),
        }
    }

    /// Fills `out` (interleaved, `channels` per frame) from the processor. It plays silence
    /// while the output layer holds the lock (during a switch) or the slot is empty. Real-time
    /// safe: it never waits for the lock.
    pub fn process(&self, out: &mut [f32], channels: usize) {
        match self.processor.try_lock() {
            Ok(mut slot) => match slot.as_mut() {
                Some(processor) => processor.process(out, channels),
                None => out.fill(0.0),
            },
            Err(_) => out.fill(0.0),
        }
    }

    /// Records the output latency: the frames between the callback and the moment its first
    /// frame is heard.
    pub fn set_output_latency_frames(&self, frames: u32) {
        self.live.set_output_latency_frames(frames);
    }

    /// Reports that the stream stopped working (its device went away or changed its rate), so
    /// the devices are checked now instead of at the next poll. Never blocks. Do not call it for
    /// an xrun: that is a glitch, not a failure.
    pub fn stream_failed(&self) {
        self.failed.store(true, Ordering::Release);
        if let Some(poller) = self.poller.get() {
            poller.unpark();
        }
    }

    fn put(&self, processor: AudioProcessor) {
        *lock(&self.processor) = Some(processor);
    }

    fn take(&self) -> Option<AudioProcessor> {
        lock(&self.processor).take()
    }
}

/// Plays the engine on the output the user wants. It follows the system default (or the
/// chosen device), applies the speaker guard, and survives devices coming and going. Dropping
/// it stops its poll thread and closes the stream.
///
/// It locks the controller while it switches devices, so never call it while holding the
/// controller's lock.
pub struct AudioOutput {
    shared: Arc<Shared>,
    poller: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    slot: Arc<AudioSlot>,
    quit: AtomicBool,
}

struct State {
    backend: Box<dyn OutputBackend>,
    controller: Arc<Mutex<Controller>>,
    monitor: OutputMonitor,
    /// The processor while no stream is open (an open stream has it in the slot).
    processor: Option<AudioProcessor>,
    /// The device whose stream is open.
    open: Option<OutputInfo>,
    on_event: Box<dyn Fn(OutputEvent) + Send>,
}

// Task 5 keeps the output in shared app state.
const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<AudioOutput>();
};

impl AudioOutput {
    /// Starts playing on the output `choice` picks, with the speaker guard on if `guard`. The
    /// first device opens before this returns; then a thread looks at the devices every
    /// 500 ms. `on_event` hears every [`OutputEvent`], once its change is carried out, on the
    /// thread that looked. It must not call back into this `AudioOutput`. If a device fails to
    /// open, its `Opened` or `Switched` is held back: each later look retries, and the one that
    /// opens it reports `Opened`.
    pub fn start(
        backend: Box<dyn OutputBackend>,
        controller: Arc<Mutex<Controller>>,
        processor: AudioProcessor,
        choice: OutputChoice,
        guard: bool,
        on_event: Box<dyn Fn(OutputEvent) + Send>,
    ) -> Self {
        let slot = Arc::new(AudioSlot::new(lock(&controller).live_shared()));
        let mut state = State {
            backend,
            controller,
            monitor: OutputMonitor::new(choice, guard),
            processor: Some(processor),
            open: None,
            on_event,
        };
        state.tick(&slot);
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            slot,
            quit: AtomicBool::new(false),
        });
        let poller = thread::Builder::new()
            .name("grooviq-output".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || poll(&shared)
            })
            .expect("the output poll thread starts");
        Self {
            shared,
            poller: Some(poller),
        }
    }

    /// Looks at the devices now, as the poll thread does every 500 ms.
    pub fn poll_now(&self) {
        self.shared.tick();
    }

    /// Every output device, listed now.
    pub fn outputs(&self) -> Vec<OutputInfo> {
        self.shared.state().backend.outputs()
    }

    /// Switches to `choice` now.
    pub fn select(&self, choice: OutputChoice) {
        let mut state = self.shared.state();
        state.monitor.set_choice(choice);
        state.tick(&self.shared.slot);
    }

    /// Turns the speaker guard on or off.
    pub fn set_guard(&self, on: bool) {
        self.shared.state().monitor.set_guard(on);
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Release);
        if let Some(poller) = self.poller.take() {
            poller.thread().unpark();
            // Each look catches its own panic, so there is nothing to pass on.
            let _ = poller.join();
        }
        self.shared.state().close(&self.shared.slot);
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn tick(&self) {
        self.state().tick(&self.slot);
    }
}

/// The poll thread: a look every 500 ms, sooner after a stream failure, until `quit`.
fn poll(shared: &Shared) {
    // Set here, before the first check of `failed`, so no wake-up is lost.
    let _ = shared.slot.poller.set(thread::current());
    let mut last = Instant::now();
    while !shared.quit.load(Ordering::Acquire) {
        let wait = if shared.slot.failed.load(Ordering::Acquire) {
            RETRY_GAP
        } else {
            POLL_PERIOD
        };
        let since = last.elapsed();
        if since < wait {
            thread::park_timeout(wait - since);
            continue;
        }
        // A panicking backend (cpal's enumeration can panic while devices churn) must not end
        // the polling: the next look tries again.
        let _ = catch_unwind(AssertUnwindSafe(|| shared.tick()));
        last = Instant::now();
    }
}

impl State {
    /// One look at the devices: decide, carry the decision out, then report it.
    fn tick(&mut self, slot: &Arc<AudioSlot>) {
        let devices = self.backend.outputs();
        let failed = slot.failed.swap(false, Ordering::AcqRel);
        let mut events = self.monitor.tick(&devices);
        let target = self.monitor.current().cloned();
        let stays = matches!((&target, &self.open), (Some(t), Some(o)) if t.id == o.id);
        if !stays || failed {
            if events
                .iter()
                .any(|e| matches!(e, OutputEvent::GuardStopped { .. }))
            {
                self.stop_sound(slot);
            } else {
                self.close(slot);
            }
            if let Some(target) = target {
                if self.open(&target, slot) {
                    if stays {
                        // Reopened after a failure, maybe at a new rate.
                        events.push(OutputEvent::Opened(target));
                    }
                } else {
                    // Nothing plays there, so claim nothing; the next look tries again.
                    events.retain(|e| {
                        !matches!(e, OutputEvent::Opened(_) | OutputEvent::Switched { .. })
                    });
                    self.monitor.forget_current();
                }
            }
        }
        for event in events {
            (self.on_event)(event);
        }
    }

    /// The guard stop: `AllSoundOff` while the old stream still runs, so a device that is
    /// still there plays the fade. Then, with the stream closed, the processor runs silent
    /// until the fade is over, so not even the fade reaches the next device.
    fn stop_sound(&mut self, slot: &AudioSlot) {
        let mut stopped = lock(&self.controller).send(Command::AllSoundOff).is_ok();
        self.close(slot);
        let Some(processor) = self.processor.as_mut() else {
            return;
        };
        // Locked from here on, so the queue only drains while AllSoundOff waits for room.
        let mut controller = lock(&self.controller);
        let mut scratch = [0f32; 2 * BLOCK];
        // A full queue: render blocks until AllSoundOff fits. (A queue that stays blocked holds
        // a restart for a crashed graph, which is silent anyway.)
        for _ in 0..MAX_DRAIN_BLOCKS {
            if stopped {
                break;
            }
            processor.process(&mut scratch, 2);
            stopped = controller.send(Command::AllSoundOff).is_ok();
        }
        // Plus a block: frames left over from the old stream's last block do not count.
        let frames = (controller.sample_rate() * GUARD_SILENCE_SECONDS).ceil() as usize + BLOCK;
        for _ in 0..frames.div_ceil(BLOCK) {
            processor.process(&mut scratch, 2);
        }
    }

    /// Closes the open stream, if any, and takes the processor back.
    fn close(&mut self, slot: &AudioSlot) {
        self.backend.close();
        if let Some(processor) = slot.take() {
            self.processor = Some(processor);
        }
        self.open = None;
        slot.set_output_latency_frames(0);
    }

    /// Opens `target`, first moving the engine to the target's rate while no stream runs. False
    /// if the backend failed (and the processor is back here).
    fn open(&mut self, target: &OutputInfo, slot: &Arc<AudioSlot>) -> bool {
        let Some(mut processor) = self.processor.take() else {
            return false;
        };
        let config = EngineConfig {
            sample_rate: target.sample_rate as f32,
        };
        {
            let mut controller = lock(&self.controller);
            if controller.sample_rate() != config.rate() {
                controller.reconfigure(&mut processor, config);
            }
        }
        slot.put(processor);
        // Failures of earlier streams no longer matter.
        slot.failed.store(false, Ordering::Release);
        match self.backend.open(target, Arc::clone(slot)) {
            Ok(()) => {
                self.open = Some(target.clone());
                true
            }
            Err(_) => {
                self.close(slot);
                false
            }
        }
    }
}

/// Locks `mutex` even if a thread panicked while holding it: the output layer must still be
/// able to stop the sound.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
