use super::realtime;
use grooviq_engine::rt_workers::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

struct Fixture {
    worker_entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    slots: Arc<AtomicU32>,
    force_fault: bool,
}

// SAFETY: accesses only atomics. The bounded delays force specific scheduling paths in tests.
unsafe impl Jobs for Fixture {
    fn run(&self, job: JobId, slot: WorkerSlot) {
        self.slots.fetch_or(1 << slot, Ordering::Relaxed);
        let start = Instant::now();
        if job == 1 && slot != 0 {
            self.worker_entered.store(true, Ordering::Release);
            if self.force_fault {
                while !self.release.load(Ordering::Acquire)
                    && start.elapsed() < Duration::from_secs(2)
                {
                    std::hint::spin_loop();
                }
            }
        }
        if job == 0 {
            while !self.worker_entered.load(Ordering::Acquire)
                && start.elapsed() < Duration::from_millis(500)
            {
                std::hint::spin_loop();
            }
        }
    }
}

#[test]
fn callback_and_worker_pool_are_realtime_on_first_use_fault_and_recovery() {
    #[cfg(feature = "profiling")]
    let _client = tracy_client::Client::start();

    let config = PoolConfig {
        workers: 1,
        test_normal_priority: true,
        test_worker_context: Some(|poll| realtime(poll)),
        ..PoolConfig::default()
    };
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    for force_fault in [false, true] {
        let worker_entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let slots = Arc::new(AtomicU32::new(0));
        let generation = pool
            .admit(
                Schedule::build(4, &[(2, 1), (3, 2)], &config).unwrap(),
                Fixture {
                    worker_entered,
                    release: release.clone(),
                    slots: slots.clone(),
                    force_fault,
                },
            )
            .unwrap();
        let received = realtime(|| {
            let mut admitted = runner.admitted(generation).unwrap();
            if admitted.jobs_mut().is_none() {
                return false;
            }
            let outcome = admitted.run_block(Mode::Pool, Duration::from_millis(75));
            match outcome {
                BlockOutcome::Faulted { job: 1, not_run } => {
                    force_fault
                        && not_run.contains(1)
                        && not_run.contains(2)
                        && not_run.contains(3)
                        && admitted.jobs_mut().is_none()
                }
                BlockOutcome::Complete => !force_fault,
                _ => false,
            }
        });
        release.store(true, Ordering::Release);
        assert!(received);
        assert_eq!(slots.load(Ordering::Relaxed), 0b11);
        if force_fault {
            let until = Instant::now() + Duration::from_secs(2);
            loop {
                match realtime(|| runner.run_block(generation, Mode::Serial, Duration::ZERO)) {
                    BlockOutcome::Recovered { job: 1 } => break,
                    BlockOutcome::Faulted { job: 1, .. } if Instant::now() < until => {
                        std::thread::yield_now();
                    }
                    _ => panic!("expected recovery"),
                }
            }
        }
    }
}
