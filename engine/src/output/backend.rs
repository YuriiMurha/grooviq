//! [`AudioOutput`]: owns the processor and keeps it on the output the [`OutputMonitor`] picks.
//!
//! A poll thread looks at the devices every 500 ms (cpal has no default-device notification);
//! a stream error or a new choice wakes it at once. While a stream plays, the processor sits in
//! the [`AudioSlot`], which the device callback only tries to claim. Otherwise the output layer
//! holds it, so it can run it silent after a guard stop or move it to another rate. A new
//! stream starts with the slot empty, so silent; the processor goes in once it runs at the
//! stream's rate.
//!
//! A look holds the state lock throughout, even through an open that takes a second. Nothing
//! else waits for it: `outputs` and `current` read what the last look saw, `select` and
//! `set_guard` leave word for the next look, and events are delivered after the lock is
//! released.

use super::{OutputChoice, OutputEvent, OutputInfo, OutputMonitor, lock};
use crate::{AudioProcessor, BLOCK, Command, Controller, EngineConfig, LiveShared};
use std::cell::UnsafeCell;
use std::collections::{HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle, Thread, ThreadId};
use std::time::{Duration, Instant};

/// How often the poll thread looks at the devices.
const POLL_PERIOD: Duration = Duration::from_millis(500);
/// The wait after a failure before the same device is tried again, doubled after each further
/// failure up to `RETRY_MAX`.
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(8);
/// A stream that fails after playing this long is a new incident: it is reopened at once, and
/// the reopening reported. One that fails sooner is retried after a growing wait, quietly.
const STABLE: Duration = Duration::from_secs(30);
/// How long a guard stop runs the processor with no stream open, once `AllSoundOff` has been
/// applied: twice its 5 ms fade.
const GUARD_SILENCE_SECONDS: f32 = 0.010;
/// The most blocks a guard stop renders. A safety net: freeing the trash every block lets
/// even a queue of 1024 notes drain in a few.
const MAX_SILENCE_BLOCKS: usize = 512;

/// A way to play audio on output devices.
pub trait OutputBackend: Send {
    /// Every output device, each with the rate `open` asks it for. Never lists an input device.
    fn outputs(&self) -> Vec<OutputInfo>;
    /// Opens the device `info` describes and starts calling [`AudioSlot::process`] from its
    /// callback. Returns the rate the stream runs at: `info.sample_rate`, or the device's own
    /// rate if it would not switch. Any previous stream is already closed.
    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<u32, String>;
    /// Stops and closes the open stream, if any. Once this returns, its callback never runs
    /// again.
    fn close(&mut self);
}

/// What an open stream's callback pulls audio from: the processor (absent while no stream
/// plays), and where the stream reports its latency and its failures.
pub struct AudioSlot {
    processor: UnsafeCell<Option<AudioProcessor>>,
    busy: AtomicBool,
    live: Arc<LiveShared>,
    /// The open stream reported a failure that the next look must handle.
    failed: AtomicBool,
    /// The poll thread, woken when a stream fails.
    poller: OnceLock<Thread>,
}

// SAFETY: every shared access to processor is through a unique SlotClaim, obtained by
// Acquire CAS and released by a Release store. AudioProcessor is Send (checked below),
// so ownership may cross threads; it need not be Sync. References cannot outlive the
// claim's mutable borrow. Claim Drop also releases during unwinding and never touches
// the processor. The callback only borrows it: put/take and replacement destruction
// belong to control callers. Shutdown closes/joins the backend before take; surviving
// callback Arcs then hold an empty slot. Final cell destruction requires exclusive
// ownership, so cannot overlap a claim even during unwinding.
unsafe impl Sync for AudioSlot {}

const _: fn() = || {
    fn send<T: Send>() {}
    send::<AudioProcessor>();
};

struct SlotClaim<'a>(&'a AudioSlot);

impl SlotClaim<'_> {
    fn processor(&mut self) -> &mut Option<AudioProcessor> {
        // SAFETY: only the successful claimant can reach this cell; the mutable borrow
        // prevents two references from this claim, and cannot outlive its release.
        unsafe { &mut *self.0.processor.get() }
    }
}

impl Drop for SlotClaim<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

impl AudioSlot {
    pub(super) fn new(live: Arc<LiveShared>) -> Self {
        Self {
            processor: UnsafeCell::new(None),
            busy: AtomicBool::new(false),
            live,
            failed: AtomicBool::new(false),
            poller: OnceLock::new(),
        }
    }

    /// Fills `out` without waiting: a busy or empty slot plays silence.
    pub fn process(&self, out: &mut [f32], channels: usize) {
        let Some(mut claim) = self.try_claim() else {
            out.fill(0.0);
            return;
        };
        match claim.processor().as_mut() {
            Some(processor) => processor.process(out, channels),
            None => out.fill(0.0),
        }
    }

    fn try_claim(&self) -> Option<SlotClaim<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| SlotClaim(self))
    }

    // Only the control thread waits. The callback makes exactly one attempt.
    fn claim_control(&self) -> SlotClaim<'_> {
        loop {
            if let Some(claim) = self.try_claim() {
                return claim;
            }
            thread::yield_now();
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
        self.wake();
    }

    fn wake(&self) {
        if let Some(poller) = self.poller.get() {
            poller.unpark();
        }
    }

    pub(super) fn put(&self, processor: AudioProcessor) {
        let old = self.claim_control().processor().replace(processor);
        // Replacement destruction belongs to this control caller, outside the claim.
        drop(old);
    }

    fn take(&self) -> Option<AudioProcessor> {
        self.claim_control().processor().take()
    }
}

/// Plays the engine on the output the user wants. It follows the system default (or the
/// chosen device), applies the speaker guard, and survives devices coming and going. Dropping
/// it stops its poll thread and closes the stream.
pub struct AudioOutput {
    shared: Arc<Shared>,
    poller: Option<JoinHandle<()>>,
}

struct Shared {
    /// Held for a whole look.
    state: Mutex<State>,
    /// What the last look saw.
    seen: Mutex<Seen>,
    /// A choice `select` made, for the next look.
    requested: Mutex<Option<OutputChoice>>,
    guard: AtomicBool,
    /// Events waiting to be delivered, in the order the looks made them.
    pending: Mutex<VecDeque<OutputEvent>>,
    /// Held while delivering.
    on_event: Mutex<Box<dyn Fn(OutputEvent) + Send>>,
    /// The thread delivering events, so a look from inside `on_event` leaves its events to it.
    delivering: Mutex<Option<ThreadId>>,
    slot: Arc<AudioSlot>,
    quit: AtomicBool,
}

#[derive(Default)]
struct Seen {
    devices: Vec<OutputInfo>,
    current: Option<OutputInfo>,
}

struct State {
    backend: Box<dyn OutputBackend>,
    controller: Arc<Mutex<Controller>>,
    monitor: OutputMonitor,
    /// The processor, whenever it is not in the slot of a playing stream.
    processor: Option<AudioProcessor>,
    open: Option<Open>,
    /// The id of the device the last look aimed at.
    target: Option<String>,
    retry: Retry,
    /// An `Opened` for the target is owed: it was held back when the device failed to open, or
    /// the target's stream failed and its reopening is to be reported, once.
    owed: bool,
    /// Devices whose `OpenFailed` was reported in this target/choice episode.
    reported: HashSet<String>,
}

/// The stream that plays.
struct Open {
    /// Its device, with the rate it really runs at.
    info: OutputInfo,
    since: Instant,
}

/// When the target may be tried again.
#[derive(Default)]
struct Retry {
    /// Failures since the target, the choice or a stream that played for `STABLE` last reset it.
    failures: u32,
    not_before: Option<Instant>,
    /// Its stream failed recently: a further failure waits instead of reopening at once.
    streak: bool,
}

impl Retry {
    fn due(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|at| now >= at)
    }

    /// One more failure: wait 0.5 s, then 1, 2, 4 and at most 8 s.
    fn failed(&mut self, now: Instant) {
        self.failures += 1;
        let doublings = (self.failures - 1).min(4);
        self.not_before = Some(now + RETRY_FIRST.saturating_mul(1 << doublings).min(RETRY_MAX));
    }
}

// Task 5 keeps the output in shared app state.
const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<AudioOutput>();
};

impl AudioOutput {
    /// Starts playing on the output `choice` picks, with the speaker guard on if `guard`. The
    /// first device opens before this returns; then a thread looks at the devices every
    /// 500 ms.
    ///
    /// `on_event` hears every [`OutputEvent`] in order, once its change is carried out, on the
    /// thread that looked, with no lock of this output held: it may call any method here, even
    /// `poll_now`, but must not drop the output. Do not hold the controller's lock while
    /// calling `start` or `poll_now`.
    pub fn start(
        backend: Box<dyn OutputBackend>,
        controller: Arc<Mutex<Controller>>,
        processor: AudioProcessor,
        choice: OutputChoice,
        guard: bool,
        on_event: Box<dyn Fn(OutputEvent) + Send>,
    ) -> Self {
        let slot = Arc::new(AudioSlot::new(lock(&controller).live_shared()));
        let state = State {
            backend,
            controller,
            monitor: OutputMonitor::new(choice, guard),
            processor: Some(processor),
            open: None,
            target: None,
            retry: Retry::default(),
            owed: false,
            reported: HashSet::new(),
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            seen: Mutex::default(),
            requested: Mutex::new(None),
            guard: AtomicBool::new(guard),
            pending: Mutex::default(),
            on_event: Mutex::new(on_event),
            delivering: Mutex::new(None),
            slot,
            quit: AtomicBool::new(false),
        });
        shared.look();
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

    /// Looks at the devices now, as the poll thread does every 500 ms, and delivers what
    /// changed before returning (when called from `on_event`, right after it returns).
    /// Blocks on an in-progress look and on event delivery by another thread. A callback
    /// must not wait for another thread calling `poll_now`: that thread waits for the
    /// callback to return. Calling it directly within the callback is supported.
    pub fn poll_now(&self) {
        self.shared.look();
    }

    /// Every output device, as the last look saw it (at most about 500 ms ago). Never waits.
    pub fn outputs(&self) -> Vec<OutputInfo> {
        lock(&self.shared.seen).devices.clone()
    }

    /// The device playing now, with the rate it runs at; `None` while nothing is open. Never
    /// waits.
    pub fn current(&self) -> Option<OutputInfo> {
        lock(&self.shared.seen).current.clone()
    }

    /// Asks for `choice`: the poll thread switches at once, and the events follow. A move this
    /// causes to the chosen device is the user's own, so the guard lets it play on. A device
    /// that failed to open is tried again at once. Never waits.
    pub fn select(&self, choice: OutputChoice) {
        *lock(&self.shared.requested) = Some(choice);
        self.shared.slot.wake();
    }

    /// Turns the speaker guard on or off, from the next look on. Never waits.
    pub fn set_guard(&self, on: bool) {
        self.shared.guard.store(on, Ordering::Release);
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Release);
        if let Some(poller) = self.poller.take() {
            // Out of its 500 ms wait at once.
            poller.thread().unpark();
            // Its loop catches panics, so there is nothing to pass on.
            let _ = poller.join();
        }
        lock(&self.shared.state).close(&self.shared.slot);
    }
}

impl Shared {
    /// One look, then the delivery of its events (and of any other look's still waiting).
    fn look(&self) {
        {
            let mut state = lock(&self.state);
            // A panicking backend (cpal's enumeration can panic while devices come and go) must
            // not reach the caller: the next look tries again.
            if let Ok(events) = catch_unwind(AssertUnwindSafe(|| state.look(self))) {
                lock(&self.pending).extend(events);
            }
        }
        self.deliver();
    }

    /// Delivers the waiting events in order. From inside `on_event` (it looked again), the
    /// outer delivery takes them once the callback returns.
    fn deliver(&self) {
        let me = thread::current().id();
        if *lock(&self.delivering) == Some(me) {
            return;
        }
        let on_event = lock(&self.on_event);
        let _delivering = Delivering::start(&self.delivering, me);
        loop {
            let next = lock(&self.pending).pop_front();
            let Some(event) = next else {
                break;
            };
            on_event(event);
        }
    }

    fn asked(&self) -> bool {
        self.slot.failed.load(Ordering::Acquire) || lock(&self.requested).is_some()
    }
}

/// Marks the thread that delivers, until dropped (also when `on_event` panics).
struct Delivering<'a>(&'a Mutex<Option<ThreadId>>);

impl<'a> Delivering<'a> {
    fn start(cell: &'a Mutex<Option<ThreadId>>, thread: ThreadId) -> Self {
        *lock(cell) = Some(thread);
        Self(cell)
    }
}

impl Drop for Delivering<'_> {
    fn drop(&mut self) {
        *lock(self.0) = None;
    }
}

/// The poll thread: a look every 500 ms, or at once when a stream fails or a choice waits.
fn poll(shared: &Shared) {
    // Set here, before the first check, so no wake-up is lost.
    let _ = shared.slot.poller.set(thread::current());
    let mut last = Instant::now();
    while !shared.quit.load(Ordering::Acquire) {
        let since = last.elapsed();
        if since < POLL_PERIOD && !shared.asked() {
            thread::park_timeout(POLL_PERIOD - since);
            continue;
        }
        // A panicking `on_event` must not end the polling either.
        let _ = catch_unwind(AssertUnwindSafe(|| shared.look()));
        last = Instant::now();
    }
}

impl State {
    /// One look at the devices: decide, carry the decision out, then say what happened.
    fn look(&mut self, shared: &Shared) -> Vec<OutputEvent> {
        let slot = &shared.slot;
        // Taken before anything that could panic, so the poll thread cannot spin on them.
        let failed = slot.failed.swap(false, Ordering::AcqRel);
        let choice = lock(&shared.requested).take();
        let now = Instant::now();
        if failed && self.open.is_some() {
            self.stream_failed(slot, now);
        }
        if let Some(choice) = choice {
            self.monitor.set_choice(choice);
            // A device that failed may be tried, and reported, again.
            self.reported.clear();
            self.retry = Retry::default();
        }
        self.monitor.set_guard(shared.guard.load(Ordering::Acquire));
        let devices = self.backend.outputs();
        let mut events = self.monitor.tick(&devices);
        let target = self.monitor.current().cloned();
        let target_id = target.as_ref().map(|t| t.id.clone());
        if target_id != self.target {
            self.target = target_id;
            self.reported.clear();
            self.retry = Retry::default();
            self.owed = false;
        }
        if events
            .iter()
            .any(|e| matches!(e, OutputEvent::GuardStopped { .. }))
        {
            self.stop_sound(slot);
        }
        match target {
            None => self.close(slot),
            Some(mut target) if self.open.as_ref().is_some_and(|o| o.info.id == target.id) => {
                let open = self.open.as_mut().expect("matching open stream");
                // Enumeration describes the route; only opening determines the actual rate.
                target.sample_rate = open.info.sample_rate;
                open.info = target;
            }
            Some(target) => {
                self.close(slot);
                if self.retry.due(now) {
                    self.try_open(&target, slot, &mut events);
                }
            }
        }
        if let Some(processor) = self.processor.as_mut() {
            // No stream owns it: deliver retained take boundaries even with no callback
            // and no device available for another rebuild attempt.
            processor.flush_recording_boundaries();
        }
        let mut seen = lock(&shared.seen);
        seen.devices = devices;
        seen.current = self.open.as_ref().map(|open| open.info.clone());
        events
    }

    /// The open stream died (its device went away or changed its rate). A first failure is
    /// reopened at once and reported once; one soon after is retried after a growing wait,
    /// quietly.
    fn stream_failed(&mut self, slot: &AudioSlot, now: Instant) {
        let played = self.open.as_ref().map_or(Duration::ZERO, |open| {
            now.saturating_duration_since(open.since)
        });
        self.close(slot);
        if played >= STABLE {
            self.retry = Retry::default();
        }
        if self.retry.streak {
            self.retry.failed(now);
        } else {
            self.retry.streak = true;
            self.owed = true;
        }
    }

    /// Opens `target` and reports how it went: the rate it really runs at, or the failure.
    fn try_open(
        &mut self,
        target: &OutputInfo,
        slot: &Arc<AudioSlot>,
        events: &mut Vec<OutputEvent>,
    ) {
        let opened = self.open_stream(target, slot);
        // After the open, which may itself take a second or two: the wait before the next try
        // starts from here.
        let now = Instant::now();
        match opened {
            Ok(info) => {
                let mut announced = false;
                for event in events.iter_mut() {
                    match event {
                        OutputEvent::Opened(to) | OutputEvent::Switched { to, .. }
                            if to.id == info.id =>
                        {
                            to.sample_rate = info.sample_rate;
                            announced = true;
                        }
                        OutputEvent::GuardStopped { to } if to.id == info.id => {
                            to.sample_rate = info.sample_rate;
                        }
                        _ => {}
                    }
                }
                if std::mem::take(&mut self.owed) && !announced {
                    events.push(OutputEvent::Opened(info.clone()));
                }
                self.reported.remove(&info.id);
                self.open = Some(Open { info, since: now });
            }
            Err(error) => {
                self.close(slot);
                // Nothing plays there: hold back the claim until it does.
                let claimed = events.len();
                events.retain(|e| {
                    !matches!(e, OutputEvent::Opened(to) | OutputEvent::Switched { to, .. }
                        if to.id == target.id)
                });
                self.owed |= events.len() != claimed;
                if self.reported.insert(target.id.clone()) {
                    events.push(OutputEvent::OpenFailed {
                        output: target.clone(),
                        error,
                    });
                }
                self.retry.failed(now);
            }
        }
    }

    /// Opens a stream on `target`. It starts with the slot empty (silent); the engine moves to
    /// the rate the stream really runs at, then the processor goes in.
    fn open_stream(
        &mut self,
        target: &OutputInfo,
        slot: &Arc<AudioSlot>,
    ) -> Result<OutputInfo, String> {
        // Failures of earlier streams no longer matter.
        slot.failed.store(false, Ordering::Release);
        let rate = self.backend.open(target, Arc::clone(slot))?;
        let config = EngineConfig {
            sample_rate: rate as f32,
        };
        // A physical stream cannot follow the config's clamping: reject it while the slot
        // is still silent, before taking or rebuilding the parked processor.
        if config.rate() != config.sample_rate {
            return Err(format!("unsupported actual output sample rate: {rate} Hz"));
        }
        let Some(mut processor) = self.processor.take() else {
            return Err("the output has no processor to play".into());
        };
        {
            let mut controller = lock(&self.controller);
            if controller.sample_rate() != config.rate()
                && !controller.reconfigure(&mut processor, config)
            {
                self.processor = Some(processor);
                return Err("the sound engine could not rebuild for the output rate".into());
            }
        }
        slot.put(processor);
        Ok(OutputInfo {
            sample_rate: rate,
            ..target.clone()
        })
    }

    /// The guard stop: `AllSoundOff` while the old stream still runs, so a device that is
    /// still there plays the fade; then, with the stream closed, the processor runs silent
    /// until the fade is over, so not even the fade reaches the next device. That render calls
    /// `process` off the audio thread, so `in_audio_callback` is true during it as well.
    fn stop_sound(&mut self, slot: &AudioSlot) {
        let stopped = lock(&self.controller).send(Command::AllSoundOff).is_ok();
        self.close(slot);
        if let Some(processor) = self.processor.as_mut() {
            silence(&mut lock(&self.controller), processor, stopped);
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
}

/// Runs `processor`, with no stream open, until `AllSoundOff` has been applied and 10 ms more
/// rendered, so not even its fade reaches the next device. `stopped`: `AllSoundOff` is queued
/// already. The controller stays locked, so the queue only drains.
fn silence(controller: &mut Controller, processor: &mut AudioProcessor, mut stopped: bool) {
    // Plus a block: frames left over from the old stream's last block do not count.
    let needed =
        (controller.sample_rate() * GUARD_SILENCE_SECONDS / BLOCK as f32).ceil() as usize + 1;
    let mut scratch = [0f32; 2 * BLOCK];
    let mut after = 0;
    for _ in 0..MAX_SILENCE_BLOCKS {
        if !stopped {
            // The queue was full; it drains as the blocks render.
            stopped = controller.send(Command::AllSoundOff).is_ok();
        }
        // A restart waiting for room in the trash queue holds back every command behind it,
        // AllSoundOff included: make room.
        controller.free_trash();
        // One block per call.
        processor.process(&mut scratch, 2);
        if stopped && controller.commands_drained() {
            after += 1;
            if after == needed {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, engine};

    fn populated_slot() -> (crate::Controller, Arc<AudioSlot>) {
        let (mut c, p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        c.send(Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        })
        .unwrap();
        let slot = Arc::new(AudioSlot::new(c.live_shared()));
        slot.put(p);
        (c, slot)
    }

    #[test]
    fn busy_callback_returns_silence_without_advancing_processor() {
        let (c, slot) = populated_slot();
        let claim = slot.claim_control();
        let before = c.live().frames;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn({
            let slot = Arc::clone(&slot);
            move || {
                let mut out = [1.0; 128];
                slot.process(&mut out, 2);
                done_tx.send(out).unwrap();
            }
        });
        // Completion while holding the claim proves the callback does not wait.
        let out = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("callback must not wait");
        assert!(out.iter().all(|x| *x == 0.0));
        worker.join().unwrap();
        assert_eq!(c.live().frames, before);
        drop(claim);
        let mut out = [0.0; 128];
        slot.process(&mut out, 2);
        assert!(out.iter().any(|x| *x != 0.0));
        assert!(c.live().frames > before);
        drop(slot.take());
    }

    #[test]
    fn rendering_keeps_samples_alive_until_control_replaces_processor() {
        use crate::{ChannelId, ChannelMix, ChannelSetup, EngineState, SampleBuf, engine_with};
        let sample = Arc::new(SampleBuf {
            rate: 48_000.0,
            l: vec![0.5; 480],
            r: vec![0.5; 480],
        });
        let weak = Arc::downgrade(&sample);
        let (c, p) = engine_with(
            EngineConfig {
                sample_rate: 48_000.0,
            },
            EngineState {
                channels: vec![ChannelSetup::sampler(
                    ChannelId(1),
                    Some(sample),
                    Vec::new(),
                    ChannelMix::default(),
                )],
                ..EngineState::default()
            },
        );
        let slot = AudioSlot::new(c.live_shared());
        slot.put(p);
        drop(c);
        let mut out = [0.0; 128];
        for _ in 0..100 {
            slot.process(&mut out, 2);
        }
        assert!(
            weak.upgrade().is_some(),
            "callback retains the processor's sample"
        );
        let (_new_c, new_p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        slot.put(new_p);
        assert!(
            weak.upgrade().is_none(),
            "replacement on control thread destroys old graph"
        );
        drop(slot.take());
    }

    #[test]
    fn unwind_releases_claim_and_preserves_processor_ownership() {
        let (_c, slot) = populated_slot();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut claim = slot.try_claim().unwrap();
            assert!(claim.processor().is_some());
            panic!("forced unwind while borrowing processor");
        }));
        assert!(result.is_err());
        assert!(slot.try_claim().is_some(), "unwind must release exclusion");
        let processor = slot.take().expect("unwind did not lose processor");
        let mut out = [1.0; 128];
        slot.process(&mut out, 2);
        assert!(out.iter().all(|x| *x == 0.0));
        slot.put(processor);
        slot.process(&mut out, 2);
        assert!(out.iter().any(|x| *x != 0.0));
        drop(slot.take());
    }

    #[test]
    fn control_handoff_waits_for_callback_claim_and_reclaims_on_control_thread() {
        let (_c, slot) = populated_slot();
        let claim = slot.try_claim().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn({
            let slot = Arc::clone(&slot);
            move || {
                started_tx.send(()).unwrap();
                let p = slot.take().expect("one owner");
                done_tx.send(()).unwrap();
                // Repeated take/put races with callbacks; processor stays owned throughout.
                slot.put(p);
                for _ in 0..1000 {
                    let p = slot.take().expect("ownership retained");
                    slot.put(p);
                }
                slot.take().unwrap()
            }
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(claim);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut out = [0.0; 128];
        for _ in 0..1000 {
            slot.process(&mut out, 2);
            assert!(out.iter().all(|x| x.is_finite()));
        }
        let mut processor = worker.join().unwrap();
        assert!(slot.take().is_none());
        slot.process(&mut out, 2);
        assert!(out.iter().all(|x| *x == 0.0));
        processor.process(&mut out, 2);
        assert!(out.iter().any(|x| *x != 0.0));
        // Final processor destruction happens here, after the callback has finished.
        drop(processor);
    }

    #[test]
    fn a_failed_rate_rebuild_keeps_the_processor_out_of_the_stream() {
        struct At44100;
        impl OutputBackend for At44100 {
            fn outputs(&self) -> Vec<OutputInfo> {
                vec![]
            }
            fn open(&mut self, _: &OutputInfo, slot: Arc<AudioSlot>) -> Result<u32, String> {
                // Starting a callback before admission must remain silent too.
                let mut out = [1.0; BLOCK * 2];
                slot.process(&mut out, 2);
                assert!(out.iter().all(|x| *x == 0.0));
                Ok(44_100)
            }
            fn close(&mut self) {}
        }
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        c.send(Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        })
        .unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        c.fail_replays(1);
        let slot = Arc::new(AudioSlot::new(c.live_shared()));
        let controller = Arc::new(Mutex::new(c));
        let mut state = State {
            backend: Box::new(At44100),
            controller: Arc::clone(&controller),
            monitor: OutputMonitor::new(OutputChoice::SystemDefault, false),
            processor: Some(p),
            open: None,
            target: None,
            retry: Retry::default(),
            owed: false,
            reported: HashSet::new(),
        };
        let target = OutputInfo {
            id: "fake-output".into(),
            name: "Fake output".into(),
            kind: super::super::OutputKind::Headphones,
            is_default: true,
            sample_rate: 44_100,
        };
        assert!(state.open_stream(&target, &slot).is_err());
        assert!(state.processor.is_some());
        assert!(slot.claim_control().processor().is_none());
        assert_eq!(lock(&controller).sample_rate(), 48_000.0);
        let mut out = [1.0; BLOCK * 2];
        slot.process(&mut out, 2);
        assert!(out.iter().all(|x| *x == 0.0));

        // The failed construction left both the processor and tone available for retry.
        assert_eq!(
            state.open_stream(&target, &slot).unwrap().sample_rate,
            44_100
        );
        assert_eq!(lock(&controller).sample_rate(), 44_100.0);
        assert!(state.processor.is_none());
        slot.process(&mut out, 2);
        assert!(out.iter().any(|x| *x != 0.0));
    }

    #[test]
    fn the_guard_silence_gets_past_restarts_waiting_for_room() {
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let tone = Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        };
        c.send(tone).unwrap();
        // Restarts at once: the first ones fill the trash queue, the next one's old graph is
        // parked, and the last waits in the queue, with AllSoundOff behind it.
        c.queue_restarts(crate::TRASH_CAPACITY + 2);
        let stopped = c.send(Command::AllSoundOff).is_ok();
        assert!(stopped);
        silence(&mut c, &mut p, stopped);
        let mut out = vec![0f32; 4_800 * 2];
        p.process(&mut out, 2);
        assert!(out.iter().all(|x| *x == 0.0));
        assert!(!c.live().tone);
    }
    #[test]
    fn detached_processor_delivers_recording_boundaries_without_a_callback() {
        use crate::{ChannelId, ChannelMix, ChannelSetup, NoteId, RecordEvent};
        struct NoOutput;
        impl OutputBackend for NoOutput {
            fn outputs(&self) -> Vec<OutputInfo> {
                Vec::new()
            }
            fn open(&mut self, _: &OutputInfo, _: Arc<AudioSlot>) -> Result<u32, String> {
                panic!("there must be no output to open")
            }
            fn close(&mut self) {}
        }
        let (mut c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        c.add_channel(ChannelSetup::synth(
            ChannelId(1),
            Vec::new(),
            ChannelMix::default(),
        ))
        .unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; BLOCK * 2], 2);
        for _ in 0..512 {
            let id = NoteId::live();
            for on in [true, false] {
                c.send(Command::ChannelNote {
                    channel: ChannelId(1),
                    key: 60,
                    velocity: 0.8,
                    on,
                    id,
                })
                .unwrap();
            }
            p.process(&mut [0.0; BLOCK * 2], 2);
        }
        assert_eq!(c.record_dropped(), 0);
        p.finish_recovery_stop(77);
        let c = Arc::new(Mutex::new(c));
        let output = AudioOutput::start(
            Box::new(NoOutput),
            Arc::clone(&c),
            p,
            OutputChoice::SystemDefault,
            false,
            Box::new(|_| {}),
        );
        let mut events = Vec::new();
        lock(&c).take_recorded(&mut events);
        assert_eq!(events.len(), 1024);
        output.poll_now();
        lock(&c).take_recorded(&mut events);
        assert!(
            matches!(events.get(1024), Some(RecordEvent::End { .. })),
            "missing retained take End"
        );
        assert_eq!(events.get(1025), Some(&RecordEvent::Mark { id: 77 }));
        assert!(!lock(&c).live().playing);
    }
}
