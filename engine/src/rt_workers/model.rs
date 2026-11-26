//! Run the production atomics and mailbox cell with Loom's checked primitives.

use super::*;
use ::loom::cell::UnsafeCell;
use ::loom::{model, thread};

struct Payload {
    input: UnsafeCell<u32>,
    result: UnsafeCell<u32>,
}

// SAFETY: The callback writes input before the offer. Its sole claimant writes result;
// the callback reads that result only after collecting completion with acquire ordering.
unsafe impl Sync for Payload {}
unsafe impl Jobs for Payload {
    fn run(&self, _: JobId, _: WorkerSlot) {
        let input = self.input.with(|value| unsafe { *value });
        self.result.with_mut(|value| unsafe { *value = input });
    }
}

impl Payload {
    fn new() -> Self {
        Self {
            input: UnsafeCell::new(0),
            result: UnsafeCell::new(0),
        }
    }
}

#[test]
fn production_mailbox_publication_claim_and_settlement() {
    model(|| {
        let config = PoolConfig {
            workers: 1,
            max_jobs: 1,
            ..PoolConfig::default()
        };
        let mut pool = WorkerPool::prepare(config.clone()).unwrap();
        let id = pool
            .admit(Schedule::build(1, &[], &config).unwrap(), Payload::new())
            .unwrap();
        let shared = Arc::clone(&pool.shared);
        let worker = thread::spawn(move || poll_worker(&shared.workers[0]));

        let pin = CallbackPin::new(&pool.shared, id).unwrap();
        let admitted = pin.state();
        // SAFETY: one model callback, and no job has been offered.
        let payload = unsafe { &*admitted.jobs.get() };
        payload.input.with_mut(|value| unsafe { *value = 42 });
        let block = unsafe { &mut *admitted.block.get() };
        offer_job(&pool.shared.workers[0], admitted, 0);
        settle_timeout(&pool.shared, &mut [Some(0)], admitted, block);

        if block.late.is_empty() {
            // A canceled job ran inline, or settlement acquired the worker's completion.
            // Read before joining, which would mask a weakened completion store.
            assert_eq!(payload.result.with(|value| unsafe { *value }), 42);
            payload.input.with_mut(|value| unsafe { *value = 43 });
        }
        worker.join().unwrap();
        assert_eq!(admitted.outstanding.load(Ordering::Acquire), 0);
    });
}

#[test]
fn production_callback_pin_excludes_reclamation() {
    model(|| {
        let config = PoolConfig {
            workers: 0,
            max_jobs: 1,
            ..PoolConfig::default()
        };
        let mut pool = WorkerPool::prepare(config.clone()).unwrap();
        let id = pool
            .admit(Schedule::build(1, &[], &config).unwrap(), Payload::new())
            .unwrap();
        // Keep the storage mapped so a mutant reports a Loom race instead of actually
        // freeing bytes under a raw pointer. Only quiescence permits the control write below.
        let retired = Arc::clone(&pool.admitted[&id]);
        let mut callback = pool.take_runner().unwrap();
        let callback = thread::spawn(move || {
            if let Some(mut admitted) = callback.admitted(id) {
                assert!(matches!(
                    admitted.run_block(Mode::Serial, Duration::ZERO),
                    BlockOutcome::Complete
                ));
            }
        });

        pool.admit(Schedule::build(0, &[], &config).unwrap(), Payload::new())
            .unwrap();
        if pool.quiescent(id) {
            let payload = unsafe { &*retired.jobs.get() };
            payload.result.with_mut(|value| unsafe { *value = 99 });
        }
        callback.join().unwrap();
    });
}

#[test]
fn production_level_barrier_acquires_worker_results() {
    model(|| {
        struct Level {
            written: AtomicBool,
            result: UnsafeCell<u32>,
        }

        // SAFETY: only job 1 writes result. The callback reads it after run_level
        // collects job 1's completion. The relaxed flag only arranges the claim.
        unsafe impl Sync for Level {}
        unsafe impl Jobs for Level {
            fn run(&self, job: JobId, _: WorkerSlot) {
                if job == 0 {
                    while !self.written.load(Ordering::Relaxed) {
                        thread::yield_now();
                    }
                } else {
                    self.result.with_mut(|value| unsafe { *value = 42 });
                    self.written.store(true, Ordering::Relaxed);
                }
            }
        }

        let config = PoolConfig {
            workers: 1,
            max_jobs: 2,
            ..PoolConfig::default()
        };
        let mut pool = WorkerPool::prepare(config.clone()).unwrap();
        let id = pool
            .admit(
                Schedule::build(2, &[], &config).unwrap(),
                Level {
                    written: AtomicBool::new(false),
                    result: UnsafeCell::new(0),
                },
            )
            .unwrap();
        let shared = Arc::clone(&pool.shared);
        let worker = thread::spawn(move || {
            while !poll_worker(&shared.workers[0]) {
                thread::yield_now();
            }
        });

        let pin = CallbackPin::new(&pool.shared, id).unwrap();
        let admitted = pin.state();
        // SAFETY: this thread owns the callback pin and is the only block-state accessor.
        let block = unsafe { &mut *admitted.block.get() };
        assert!(run_level(
            &pool.shared,
            &mut [None],
            admitted,
            block,
            &[0, 1],
            Instant::now(),
            Duration::MAX,
        ));
        // Read before joining, which would hide a weakened acquire load.
        let payload = unsafe { &*admitted.jobs.get() };
        assert_eq!(payload.result.with(|value| unsafe { *value }), 42);
        worker.join().unwrap();
    });
}

#[test]
fn production_idle_acquire_protects_mailbox_reuse_after_swap() {
    model(|| {
        let config = PoolConfig {
            workers: 1,
            max_jobs: 1,
            ..PoolConfig::default()
        };
        let mut pool = WorkerPool::prepare(config.clone()).unwrap();
        let old = pool
            .admit(Schedule::build(1, &[], &config).unwrap(), Payload::new())
            .unwrap();
        {
            let pin = CallbackPin::new(&pool.shared, old).unwrap();
            offer_job(&pool.shared.workers[0], pin.state(), 0);
        }
        let shared = Arc::clone(&pool.shared);
        let worker = thread::spawn(move || {
            assert!(poll_worker(&shared.workers[0]));
            while !poll_worker(&shared.workers[0]) {
                thread::yield_now();
            }
        });

        let new = pool
            .admit(Schedule::build(1, &[], &config).unwrap(), Payload::new())
            .unwrap();
        let pin = CallbackPin::new(&pool.shared, new).unwrap();
        let mailbox = &pool.shared.workers[0];
        // The old generation may still own the slot. There is no read of its done
        // flag: the mailbox's idle handoff alone must order its read before reuse.
        while !mailbox.is_idle() {
            thread::yield_now();
        }
        offer_job(mailbox, pin.state(), 0);
        worker.join().unwrap();
        assert!(pin.state().done[0].load(Ordering::Acquire));
    });
}

fn poll_worker<J: Jobs>(worker: &Worker<J>) -> bool {
    worker.poll(
        1,
        #[cfg(feature = "profiling")]
        None,
    )
}
