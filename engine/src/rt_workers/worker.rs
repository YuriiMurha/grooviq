use super::sync::{AtomicU8, AtomicU64, MailboxCell, Ordering};
#[cfg(feature = "profiling")]
use super::trace::WorkerTrace;
use super::{GenerationState, Jobs, PoolConfig, Shared, run_job};
use std::io;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const IDLE: u8 = 0;
const OFFERED: u8 = 1;
const CLAIMED: u8 = 2;
const WARM_POLL: Duration = Duration::from_micros(50);
const COLD_POLL: Duration = Duration::from_millis(2);
const COLD_AFTER: Duration = Duration::from_millis(8);

pub(super) struct Command<J> {
    pub(super) generation: *const GenerationState<J>,
    pub(super) job: super::JobId,
}

// Each mailbox has a distinct cache line for the control words polled by its worker.
#[repr(align(128))]
pub(super) struct Worker<J> {
    state: AtomicU8,
    command: MailboxCell<MaybeUninit<Command<J>>>,
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
            command: MailboxCell::new(MaybeUninit::uninit()),
            acknowledged: AtomicU64::new(0),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.state.load(Ordering::Acquire) == IDLE
    }

    pub(super) fn offer(&self, command: Command<J>) {
        // SAFETY: Single producer, with an acquired idle mailbox.
        self.command.with_mut(|slot| {
            unsafe { (*slot).write(command) };
        });
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

    pub(super) fn poll(
        &self,
        slot: usize,
        #[cfg(feature = "profiling")] trace: Option<&mut WorkerTrace>,
    ) -> bool {
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
        let command = self
            .command
            .with(|command| unsafe { (*command).assume_init_read() });
        let admitted = unsafe { &*command.generation };
        let run = || run_job(admitted, command.job, slot);
        #[cfg(feature = "profiling")]
        match trace {
            Some(trace) => trace.job(run),
            None => run(),
        }
        #[cfg(not(feature = "profiling"))]
        run();
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
    // Tracy can exhaust its process-wide context IDs across pool rebuilds. Tracing
    // is optional, so a failed trace lane must not prevent the worker from starting.
    #[cfg(feature = "profiling")]
    let trace =
        tracy_client::Client::running().and_then(|client| WorkerTrace::start(client, &name).ok());
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
            work(
                &shared,
                index,
                config,
                #[cfg(feature = "profiling")]
                trace,
            );
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

fn work<J: Jobs>(
    shared: &Shared<J>,
    index: usize,
    config: PoolConfig,
    #[cfg(feature = "profiling")] mut trace: Option<WorkerTrace>,
) {
    let worker = &shared.workers[index];
    let hot_period =
        Duration::from_secs_f64(f64::from(config.max_frames) / f64::from(config.sample_rate))
            .min(COLD_AFTER);
    let mut last_job: Option<Instant> = None;
    let mut acknowledged = 0;
    let mut poll = || {
        let published = shared.published.load(Ordering::Acquire);
        if published != acknowledged {
            worker.acknowledged.store(published, Ordering::Release);
            acknowledged = published;
        }
        worker.poll(
            index + 1,
            #[cfg(feature = "profiling")]
            trace.as_mut(),
        )
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
            last_job = Some(Instant::now());
            continue;
        }
        // Stay hot for a block period after work. A cold pool polls infrequently, and
        // the callback can take back an unclaimed offer without waking a parked worker.
        match last_job.map(|time| time.elapsed()) {
            Some(elapsed) if elapsed < hot_period => std::hint::spin_loop(),
            Some(elapsed) if elapsed < COLD_AFTER => thread::sleep(WARM_POLL),
            _ => thread::sleep(COLD_POLL),
        }
    }
    #[cfg(feature = "profiling")]
    if let Some(trace) = trace {
        trace.finish();
    }
}
