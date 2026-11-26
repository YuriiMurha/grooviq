use super::{GenerationState, Jobs, PoolConfig, Shared, run_job};
use std::cell::UnsafeCell;
use std::io;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const IDLE: u8 = 0;
const OFFERED: u8 = 1;
const CLAIMED: u8 = 2;
const IDLE_POLL: Duration = Duration::from_micros(25);
const HOT_POLLS: usize = 64;

pub(super) struct Command<J> {
    pub(super) generation: *const GenerationState<J>,
    pub(super) job: super::JobId,
}

// Each mailbox has a distinct cache line for the control words polled by its worker.
#[repr(align(128))]
pub(super) struct Worker<J> {
    state: AtomicU8,
    command: UnsafeCell<MaybeUninit<Command<J>>>,
    acknowledged: AtomicU64,
}

// SAFETY: only the callback writes the mailbox, while it is idle. Only the worker that
// won OFFERED -> CLAIMED can read its contents. The callback counts offered and claimed
// mail toward the pointed-to generation's lifetime until cancellation or completion.
unsafe impl<J: Jobs> Send for Worker<J> {}
unsafe impl<J: Jobs> Sync for Worker<J> {}

impl<J: Jobs> Worker<J> {
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            command: UnsafeCell::new(MaybeUninit::uninit()),
            acknowledged: AtomicU64::new(0),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.state.load(Ordering::Acquire) == IDLE
    }

    pub(super) fn offer(&self, command: Command<J>) {
        // SAFETY: Single producer, with an acquired idle mailbox.
        unsafe { (*self.command.get()).write(command) };
        self.state.store(OFFERED, Ordering::Release);
    }

    pub(super) fn cancel(&self) -> bool {
        self.state
            .compare_exchange(OFFERED, IDLE, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    pub(super) fn acknowledged(&self) -> u64 {
        self.acknowledged.load(Ordering::Acquire)
    }

    fn poll(&self, slot: usize) -> bool {
        if self.state.load(Ordering::Relaxed) != OFFERED
            || self
                .state
                .compare_exchange(OFFERED, CLAIMED, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        // SAFETY: Acquiring the offer makes its payload and block inputs visible.
        // An outstanding claim pins the entire generation until the release decrement.
        let command = unsafe { (*self.command.get()).assume_init_read() };
        let admitted = unsafe { &*command.generation };
        run_job(admitted, command.job, slot);
        admitted.done[command.job as usize].store(true, Ordering::Release);
        admitted.outstanding.fetch_sub(1, Ordering::AcqRel);
        self.state.store(IDLE, Ordering::Release);
        true
    }
}

pub(super) fn spawn<J: Jobs + 'static>(
    shared: Arc<Shared<J>>,
    config: PoolConfig,
    index: usize,
) -> io::Result<JoinHandle<()>> {
    let slot = index + 1;
    let name = format!("grooviq-worker-{slot}");
    let (ready, initialized) = mpsc::sync_channel(0);
    let thread = thread::Builder::new().name(name.clone()).spawn(move || {
        let priority = setup(&config, slot, &name);
        let priority = match priority {
            Ok(handle) => handle,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        if ready.send(Ok(())).is_ok() {
            drop(ready);
            work(&shared, index, config);
        }
        if let Some(priority) = priority {
            let _ = audio_thread_priority::demote_current_thread_from_real_time(priority);
        }
    })?;
    match initialized.recv() {
        Ok(Ok(())) => Ok(thread),
        result => {
            let _ = thread.join();
            Err(match result {
                Ok(Err(error)) => error,
                _ => io::Error::other("worker exited during startup"),
            })
        }
    }
}

fn setup(
    config: &PoolConfig,
    slot: usize,
    name: &str,
) -> io::Result<Option<audio_thread_priority::RtPriorityHandle>> {
    #[cfg(any(test, feature = "test-hooks"))]
    let normal = config.test_normal_priority;
    #[cfg(not(any(test, feature = "test-hooks")))]
    let normal = false;
    let priority = if normal {
        None
    } else {
        Some(
            audio_thread_priority::promote_current_thread_to_real_time(
                config.max_frames,
                config.sample_rate,
            )
            .map_err(|error| io::Error::other(error.to_string()))?,
        )
    };
    if let Some(join) = &config.workgroup_join
        && let Err(error) = join(slot)
    {
        if let Some(priority) = priority {
            let _ = audio_thread_priority::demote_current_thread_from_real_time(priority);
        }
        return Err(error);
    }
    #[cfg(feature = "profiling")]
    if let Some(client) = tracy_client::Client::running() {
        client.set_thread_name(name);
        let _warm = client.span(
            tracy_client::span_location!("Audio worker initialization"),
            0,
        );
    }
    let _ = name;
    Ok(priority)
}

fn work<J: Jobs>(shared: &Shared<J>, index: usize, config: PoolConfig) {
    let worker = &shared.workers[index];
    let mut hot = 0;
    let mut acknowledged = 0;
    let mut poll = || {
        let published = shared.published.load(Ordering::Acquire);
        if published != acknowledged {
            worker.acknowledged.store(published, Ordering::Release);
            acknowledged = published;
        }
        worker.poll(index + 1)
    };
    while !shared.stop.load(Ordering::Acquire) {
        #[cfg(any(test, feature = "test-hooks"))]
        let worked = match config.test_worker_context {
            Some(guard) => guard(&mut poll),
            None => poll(),
        };
        #[cfg(not(any(test, feature = "test-hooks")))]
        let worked = poll();
        if worked {
            hot = HOT_POLLS;
        } else if hot > 0 {
            hot -= 1;
            std::hint::spin_loop();
        } else {
            // The callback wakes workers by publishing to atomics only. Brief polling
            // catches adjacent levels, then sleep outside the guarded worker kernel so
            // an idle pool does not burn a core. The callback can take back pending mail.
            thread::sleep(IDLE_POLL);
        }
    }
    let _ = config;
}
