//! Persistent workers for prepared, stateful audio jobs.
//!
//! On `Complete`, every job in the block happens-before the callback returns. Worker
//! completion is a release store observed by the callback with acquire ordering. Level
//! barriers collect those stores before publishing the next level. Callback writes between
//! blocks happen-before the next block's jobs through release/acquire mailbox publication.
//!
//! A `Faulted` return covers completed work only. A still-claimed job can keep accessing its
//! node, slot scratch and generation-owned block inputs. `Admitted::jobs_mut` denies exclusive
//! access while a worker can still borrow them. Recovery acquires completion and returns before any job runs, so
//! the caller can reset the affected node. A block with multiple late workers reports each
//! recovered job on a separate call before execution resumes.

mod schedule;
mod worker;

pub use schedule::{Schedule, ScheduleError};

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::io;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering, fence};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use worker::{Command, Worker};

pub type JobId = u32;
pub type WorkerSlot = usize;

/// Stateful render jobs, owned by their admitted generation.
///
/// # Safety
///
/// Jobs must run without panic, allocation, deallocation, blocking or I/O. Each invocation
/// may mutate only its own job state and scratch for its slot. It may read only immutable
/// block inputs and outputs of its declared dependencies. Resources reachable from different
/// generations must permit overlap if an old worker is late during a swap.
pub unsafe trait Jobs: Sync {
    fn run(&self, job: JobId, slot: WorkerSlot);
}

#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub type WorkerContext = fn(&mut dyn FnMut() -> bool) -> bool;

pub type WorkgroupJoin = Arc<dyn Fn(WorkerSlot) -> io::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct PoolConfig {
    pub workers: usize,
    pub max_jobs: usize,
    pub max_deps: usize,
    pub sample_rate: u32,
    pub max_frames: u32,
    /// Called on the worker after priority setup and before any jobs are accepted.
    pub workgroup_join: Option<WorkgroupJoin>,
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub test_normal_priority: bool,
    #[cfg(any(test, feature = "test-hooks"))]
    #[doc(hidden)]
    pub test_worker_context: Option<WorkerContext>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            max_jobs: 16_384,
            max_deps: 65_536,
            sample_rate: 48_000,
            max_frames: 128,
            workgroup_join: None,
            #[cfg(any(test, feature = "test-hooks"))]
            test_normal_priority: false,
            #[cfg(any(test, feature = "test-hooks"))]
            test_worker_context: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityError {
    Jobs,
    Dependencies,
    Allocation,
    Generations,
}

/// A handle is specific to the pool that admitted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation {
    pool: u64,
    number: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Serial,
    Pool,
}

/// In canonical order. Includes each late job and all transitive successors.
#[derive(Clone, Copy, Debug)]
pub struct NotRun<'a> {
    progress: &'a [Progress],
}

impl NotRun<'_> {
    pub fn contains(&self, job: JobId) -> bool {
        self.progress.get(job as usize) == Some(&Progress::Blocked)
    }

    pub fn iter(&self) -> impl Iterator<Item = JobId> + '_ {
        self.progress
            .iter()
            .enumerate()
            .filter_map(|(job, &state)| (state == Progress::Blocked).then_some(job as JobId))
    }
}

#[derive(Debug)]
pub enum BlockOutcome<'a> {
    Complete,
    Faulted {
        job: JobId,
        not_run: NotRun<'a>,
    },
    Recovered {
        job: JobId,
    },
    /// The handle belongs to another pool or the generation has been superseded.
    Inactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Progress {
    Waiting,
    Done,
    Blocked,
}

struct BlockState {
    progress: Vec<Progress>,
    late: Vec<JobId>,
    recovery: usize,
}

struct GenerationState<J> {
    id: Generation,
    schedule: Schedule,
    jobs: UnsafeCell<J>,
    done: Vec<AtomicBool>,
    outstanding: AtomicUsize,
    block: UnsafeCell<BlockState>,
}

// SAFETY: only the unique BlockRunner accesses block state. Workers access jobs through
// the Jobs contract, and use only atomics for the completion and lifetime handoffs.
unsafe impl<J: Jobs> Sync for GenerationState<J> {}

struct Shared<J> {
    active: AtomicPtr<GenerationState<J>>,
    callback: AtomicPtr<GenerationState<J>>,
    published: AtomicU64,
    workers: Vec<Worker<J>>,
    stop: AtomicBool,
}

/// The control-thread owner. Start, admit, reclaim, shut down and drop off the audio thread.
pub struct WorkerPool<J: Jobs> {
    config: PoolConfig,
    id: u64,
    current: u64,
    shared: Arc<Shared<J>>,
    runner: Option<BlockRunner<J>>,
    admitted: HashMap<Generation, Arc<GenerationState<J>>>,
    threads: Vec<JoinHandle<()>>,
}

/// The single callback handle. Retain it across blocks and drop it on the control thread.
pub struct BlockRunner<J: Jobs> {
    shared: Arc<Shared<J>>,
    assigned: Vec<Option<JobId>>,
    // Fault views must survive a concurrent admit and reclaim. Only the runner owns this copy.
    report: Vec<Progress>,
}

impl<J: Jobs + 'static> WorkerPool<J> {
    pub fn start(config: PoolConfig) -> io::Result<Self> {
        static NEXT_POOL: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_POOL
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| io::Error::other("worker pool IDs exhausted"))?;
        if config.sample_rate == 0
            || u32::try_from(config.max_jobs).is_err()
            || config.workers.checked_add(1).is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid worker pool configuration",
            ));
        }
        // Profiling builds can construct the pool before the processor enables its own
        // timeline. Initialize the process-wide client before the workers open their zones.
        #[cfg(feature = "profiling")]
        if config.workers != 0 {
            let _ = tracy_client::Client::start();
        }
        let shared = Arc::new(Shared {
            active: AtomicPtr::new(ptr::null_mut()),
            callback: AtomicPtr::new(ptr::null_mut()),
            published: AtomicU64::new(0),
            workers: filled(config.workers, Worker::new).map_err(allocation_error)?,
            stop: AtomicBool::new(false),
        });
        let runner = BlockRunner {
            shared: Arc::clone(&shared),
            assigned: filled(config.workers, || None).map_err(allocation_error)?,
            report: filled(config.max_jobs, || Progress::Waiting).map_err(allocation_error)?,
        };
        let mut pool = Self {
            config: config.clone(),
            id,
            current: 0,
            shared,
            runner: Some(runner),
            admitted: HashMap::new(),
            threads: Vec::new(),
        };
        pool.threads
            .try_reserve_exact(config.workers)
            .map_err(|_| allocation_error(CapacityError::Allocation))?;
        for index in 0..config.workers {
            pool.threads.push(worker::spawn(
                Arc::clone(&pool.shared),
                config.clone(),
                index,
            )?);
        }
        Ok(pool)
    }

    pub fn slots(&self) -> usize {
        self.config.workers + 1
    }

    pub fn take_runner(&mut self) -> Option<BlockRunner<J>> {
        self.runner.take()
    }

    /// Publish a prepared schedule and `Jobs` together. Older handles become inactive.
    pub fn admit(&mut self, schedule: Schedule, jobs: J) -> Result<Generation, CapacityError> {
        if schedule.jobs() > self.config.max_jobs {
            return Err(CapacityError::Jobs);
        }
        if schedule.edges() > self.config.max_deps {
            return Err(CapacityError::Dependencies);
        }
        let number = self
            .current
            .checked_add(1)
            .ok_or(CapacityError::Generations)?;
        let id = Generation {
            pool: self.id,
            number,
        };
        let mut late = Vec::new();
        late.try_reserve_exact(self.config.workers.min(schedule.jobs()))
            .map_err(|_| CapacityError::Allocation)?;
        let admitted = Arc::new(GenerationState {
            id,
            done: filled(schedule.jobs(), || AtomicBool::new(false))?,
            outstanding: AtomicUsize::new(0),
            block: UnsafeCell::new(BlockState {
                progress: filled(schedule.jobs(), || Progress::Waiting)?,
                late,
                recovery: 0,
            }),
            schedule,
            jobs: UnsafeCell::new(jobs),
        });
        self.admitted
            .try_reserve(1)
            .map_err(|_| CapacityError::Allocation)?;
        let admitted = self.admitted.entry(id).or_insert(admitted);
        self.current = number;
        self.shared
            .active
            .store(Arc::as_ptr(admitted).cast_mut(), Ordering::Release);
        self.shared.published.store(number, Ordering::Release);
        Ok(id)
    }

    /// A current, unknown or already reclaimed generation is never quiescent.
    pub fn quiescent(&self, id: Generation) -> bool {
        // The retirement publication must precede our hazard read in the SC fence order.
        fence(Ordering::SeqCst);
        self.admitted.get(&id).is_some_and(|generation| {
            id.number < self.current
                && !ptr::eq(
                    self.shared.callback.load(Ordering::Acquire),
                    generation.as_ref(),
                )
                && generation.outstanding.load(Ordering::Acquire) == 0
                && self
                    .shared
                    .workers
                    .iter()
                    .all(|worker| worker.acknowledged() > id.number)
        })
    }

    pub fn reclaim(&mut self, id: Generation) -> Option<(Schedule, J)> {
        if !self.quiescent(id) {
            return None;
        }
        let generation = Arc::into_inner(self.admitted.remove(&id)?)?;
        Some((generation.schedule, generation.jobs.into_inner()))
    }

    pub fn shutdown(self) {
        drop(self);
    }
}

impl<J: Jobs> Drop for WorkerPool<J> {
    fn drop(&mut self) {
        self.shared.active.store(ptr::null_mut(), Ordering::Release);
        fence(Ordering::SeqCst);
        while !self.shared.callback.load(Ordering::Acquire).is_null() {
            thread::yield_now();
        }
        self.shared.stop.store(true, Ordering::Release);
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A callback pin for the selected generation. Keep it only while preparing or rendering
/// a block. Retiring the generation leaves this pin valid until it is dropped.
pub struct Admitted<'a, J: Jobs> {
    pin: CallbackPin<'a, J>,
    assigned: &'a mut [Option<JobId>],
    report: &'a mut [Progress],
}

struct CallbackPin<'a, J> {
    shared: &'a Shared<J>,
    generation: *const GenerationState<J>,
}

impl<'a, J> CallbackPin<'a, J> {
    fn new(shared: &'a Shared<J>, id: Generation) -> Option<Self> {
        // The paired SC fences forbid validation from seeing the old pointer while
        // retirement also misses the pin. Use the validated pointer's provenance.
        let candidate = shared.active.load(Ordering::Acquire);
        shared.callback.store(candidate, Ordering::Release);
        fence(Ordering::SeqCst);
        let current = shared.active.load(Ordering::Acquire);
        let pin = Self {
            shared,
            generation: current,
        };
        if current.is_null() || current != candidate || pin.state().id != id {
            return None;
        }
        Some(pin)
    }

    fn state(&self) -> &GenerationState<J> {
        // SAFETY: Called only after the pointer passed hazard validation.
        unsafe { &*self.generation }
    }
}

impl<J> Drop for CallbackPin<'_, J> {
    fn drop(&mut self) {
        self.shared
            .callback
            .store(ptr::null_mut(), Ordering::Release);
    }
}

impl<J: Jobs> BlockRunner<J> {
    pub fn admitted(&mut self, generation: Generation) -> Option<Admitted<'_, J>> {
        Some(Admitted {
            pin: CallbackPin::new(&self.shared, generation)?,
            assigned: &mut self.assigned,
            report: &mut self.report,
        })
    }

    /// Render without accessing the generation's jobs before or after the block.
    pub fn run_block(
        &mut self,
        generation: Generation,
        mode: Mode,
        wait_budget: Duration,
    ) -> BlockOutcome<'_> {
        let Some(pin) = CallbackPin::new(&self.shared, generation) else {
            return BlockOutcome::Inactive;
        };
        execute_block(
            &pin,
            &mut self.assigned,
            &mut self.report,
            mode,
            wait_budget,
        )
    }
}

impl<J: Jobs> Admitted<'_, J> {
    /// Shared job access is available even during a fault. A late worker can still be running.
    pub fn jobs(&self) -> &J {
        // SAFETY: The pin keeps J alive and borrowing the guard excludes mutable access.
        unsafe { &*self.pin.state().jobs.get() }
    }

    /// Exclusive access for copying in owned block inputs and applying between-block edits.
    pub fn jobs_mut(&mut self) -> Option<&mut J> {
        let admitted = self.pin.state();
        // SAFETY: The unique runner owns BlockState. Workers only publish their completion.
        let block = unsafe { &*admitted.block.get() };
        if has_late_claim(admitted, block) {
            return None;
        }
        // SAFETY: Only this guard can begin another block. Completion releases all worker
        // borrows of J, including on the fault path. No shared borrow of this guard can overlap.
        Some(unsafe { &mut *admitted.jobs.get() })
    }

    /// The budget limits waiting on workers. Callback work and the fallback run to completion.
    pub fn run_block(&mut self, mode: Mode, wait_budget: Duration) -> BlockOutcome<'_> {
        execute_block(&self.pin, self.assigned, self.report, mode, wait_budget)
    }
}

fn has_late_claim<J>(admitted: &GenerationState<J>, block: &BlockState) -> bool {
    block
        .late
        .iter()
        .skip(block.recovery)
        .any(|&job| !admitted.done[job as usize].load(Ordering::Acquire))
}

fn execute_block<'a, J: Jobs>(
    pin: &CallbackPin<'_, J>,
    assigned: &mut [Option<JobId>],
    report: &'a mut [Progress],
    mode: Mode,
    wait_budget: Duration,
) -> BlockOutcome<'a> {
    let shared = pin.shared;
    let admitted = pin.state();
    // SAFETY: This is the only runner. Workers never access block.
    let block = unsafe { &mut *admitted.block.get() };
    if let Some(&job) = block.late.get(block.recovery) {
        if has_late_claim(admitted, block) {
            return fault(report, block, job);
        }
        block.recovery += 1;
        return BlockOutcome::Recovered { job };
    }

    if mode == Mode::Serial
        || wait_budget.is_zero()
        || shared.workers.is_empty()
        || admitted.schedule.max_width() <= 1
    {
        for job in 0..admitted.schedule.jobs() {
            run_job(admitted, job as JobId, 0);
        }
        return BlockOutcome::Complete;
    }

    let start = Instant::now();
    block.progress.fill(Progress::Waiting);
    for level in admitted.schedule.levels() {
        if level.len() == 1 {
            run_inline(admitted, block, level[0]);
        } else if !run_level(shared, assigned, admitted, block, level, start, wait_budget) {
            settle_timeout(shared, assigned, admitted, block);
            if let Some(&job) = block.late.first() {
                return fault(report, block, job);
            }
            break;
        }
    }
    BlockOutcome::Complete
}
fn fault<'a>(report: &'a mut [Progress], block: &BlockState, job: JobId) -> BlockOutcome<'a> {
    let report = &mut report[..block.progress.len()];
    report.copy_from_slice(&block.progress);
    BlockOutcome::Faulted {
        job,
        not_run: NotRun { progress: report },
    }
}

fn run_job<J: Jobs>(admitted: &GenerationState<J>, job: JobId, slot: WorkerSlot) {
    // SAFETY: Only the callback launches jobs. Its guard excludes jobs_mut for this call.
    // A worker with an unfinished claim prevents jobs_mut after the callback returns.
    unsafe { &*admitted.jobs.get() }.run(job, slot);
}

fn run_inline<J: Jobs>(admitted: &GenerationState<J>, block: &mut BlockState, job: JobId) {
    run_job(admitted, job, 0);
    block.progress[job as usize] = Progress::Done;
}

fn run_level<J: Jobs>(
    shared: &Shared<J>,
    assigned: &mut [Option<JobId>],
    admitted: &GenerationState<J>,
    block: &mut BlockState,
    level: &[JobId],
    start: Instant,
    budget: Duration,
) -> bool {
    let mut next = 0;
    loop {
        let mut waiting = false;
        if start.elapsed() >= budget {
            return false;
        }
        let callback = level.get(next).copied();
        next += usize::from(callback.is_some());
        for (worker, assignment) in shared.workers.iter().zip(assigned.iter_mut()) {
            if let Some(job) = *assignment
                && admitted.done[job as usize].load(Ordering::Acquire)
            {
                block.progress[job as usize] = Progress::Done;
                *assignment = None;
            }
            if assignment.is_none()
                && worker.is_idle()
                && let Some(&job) = level.get(next)
                && start.elapsed() < budget
            {
                admitted.done[job as usize].store(false, Ordering::Relaxed);
                admitted.outstanding.fetch_add(1, Ordering::Relaxed);
                worker.offer(Command {
                    generation: ptr::from_ref(admitted),
                    job,
                });
                *assignment = Some(job);
                next += 1;
            }
            waiting |= assignment.is_some();
        }
        if let Some(job) = callback {
            run_inline(admitted, block, job);
        } else if !waiting {
            return true;
        } else {
            std::hint::spin_loop();
        }
    }
}

fn settle_timeout<J: Jobs>(
    shared: &Shared<J>,
    assigned: &mut [Option<JobId>],
    admitted: &GenerationState<J>,
    block: &mut BlockState,
) {
    block.late.clear();
    block.recovery = 0;
    // Freeze eligibility before the canonical scan. A worker that completes after it is
    // declared late cannot make its successors eligible halfway through the fallback.
    for (worker, assignment) in shared.workers.iter().zip(assigned) {
        if let Some(job) = assignment.take() {
            if worker.cancel() {
                admitted.outstanding.fetch_sub(1, Ordering::AcqRel);
            } else if admitted.done[job as usize].load(Ordering::Acquire) {
                block.progress[job as usize] = Progress::Done;
            } else {
                block.progress[job as usize] = Progress::Blocked;
                block.late.push(job);
            }
        }
    }
    block.late.sort_unstable();
    for job in 0..admitted.schedule.jobs() {
        if block.progress[job] != Progress::Waiting {
            continue;
        }
        if admitted
            .schedule
            .dependencies(job)
            .any(|dep| block.progress[dep] == Progress::Blocked)
        {
            block.progress[job] = Progress::Blocked;
        } else {
            run_inline(admitted, block, job as JobId);
        }
    }
}

fn filled<T>(len: usize, make: impl FnMut() -> T) -> Result<Vec<T>, CapacityError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| CapacityError::Allocation)?;
    values.resize_with(len, make);
    Ok(values)
}

fn allocation_error(_: CapacityError) -> io::Error {
    io::Error::other("worker pool capacity allocation failed")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod model;
