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
        let worker = thread::spawn(move || {
            shared.workers[0].poll(
                1,
                #[cfg(feature = "profiling")]
                None,
            )
        });

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
