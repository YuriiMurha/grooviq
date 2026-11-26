use super::*;
use std::sync::atomic::{AtomicU32, AtomicUsize};

#[test]
fn schedule_rejects_forward_edges_and_overflow() {
    let config = PoolConfig::default();
    assert!(matches!(
        Schedule::build(3, &[(0, 2)], &config),
        Err(ScheduleError::Order {
            job: 0,
            dependency: 2
        })
    ));
    assert!(matches!(
        Schedule::build(3, &[(1, 1)], &config),
        Err(ScheduleError::Order { .. })
    ));
    assert!(matches!(
        Schedule::build(16_385, &[], &config),
        Err(ScheduleError::Capacity(CapacityError::Jobs))
    ));
    let edges = vec![(1, 0); 65_536];
    assert!(Schedule::build(16_384, &edges, &config).is_ok());
    assert!(matches!(
        Schedule::build(2, &[edges.as_slice(), &[(1, 0)]].concat(), &config),
        Err(ScheduleError::Capacity(CapacityError::Dependencies))
    ));
    let schedule = Schedule::build(6, &[(3, 2), (2, 0), (5, 3), (5, 4)], &config).unwrap();
    assert_eq!(schedule.level_widths(), &[3, 1, 1, 1]);
    assert_eq!(schedule.max_width(), 3);
}

struct Gate {
    entered: AtomicBool,
    released: AtomicBool,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: AtomicBool::new(false),
            released: AtomicBool::new(false),
        }
    }

    fn wait(&self) {
        let until = Instant::now() + Duration::from_secs(3);
        while !self.entered.load(Ordering::Acquire) {
            assert!(Instant::now() < until, "job never arrived at gate");
            thread::yield_now();
        }
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
    }
}

struct GatedJobs {
    calls: Arc<[AtomicUsize; 6]>,
    gate: Option<Arc<Gate>>,
}

// SAFETY: each counter is atomic and the gate is only read by the test job.
unsafe impl Jobs for GatedJobs {
    fn run(&self, job: JobId, slot: WorkerSlot) {
        self.calls[job as usize].fetch_add(1, Ordering::Relaxed);
        if let Some(gate) = &self.gate {
            // Job 0 keeps the callback busy until the worker claims job 1.
            let until = Instant::now() + Duration::from_secs(2);
            if job == 0 {
                while !gate.entered.load(Ordering::Acquire) && Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
            if job == 1 && slot != 0 {
                gate.entered.store(true, Ordering::Release);
                while !gate.released.load(Ordering::Acquire) && Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
        }
    }
}

fn wait_for(f: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(3);
    while !f() {
        assert!(Instant::now() < until, "timed out");
        thread::yield_now();
    }
}

fn config(workers: usize) -> PoolConfig {
    PoolConfig {
        workers,
        // Priority is orthogonal to these synthetic scheduling tests.
        #[cfg(any(test, feature = "test-hooks"))]
        test_normal_priority: true,
        ..PoolConfig::default()
    }
}

#[test]
fn claimed_job_faults_with_transitive_successors_then_recovers() {
    let config = config(1);
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let gate = Arc::new(Gate::new());
    let calls = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
    let generation = pool
        .admit(
            Schedule::build(6, &[(2, 1), (3, 2), (5, 4)], &config).unwrap(),
            GatedJobs {
                calls: calls.clone(),
                gate: Some(gate.clone()),
            },
        )
        .unwrap();
    let budget = Duration::from_millis(40);
    let start = Instant::now();
    let result = runner.run_block(generation, Mode::Pool, budget);
    let elapsed = start.elapsed();
    let fault = match result {
        BlockOutcome::Faulted { job, not_run } => (job, not_run.iter().collect::<Vec<_>>()),
        _ => panic!("expected a claimed worker job to fault"),
    };
    gate.wait();
    assert_eq!(fault, (1, vec![1, 2, 3]));
    assert!(elapsed < budget + Duration::from_millis(150), "{elapsed:?}");
    assert_eq!(
        calls.each_ref().map(|n| n.load(Ordering::Relaxed)),
        [1, 1, 0, 0, 1, 1]
    );
    for mode in [Mode::Serial, Mode::Pool] {
        let BlockOutcome::Faulted { job: 1, not_run } = runner.run_block(generation, mode, budget)
        else {
            panic!("the late claim must refuse the entire block");
        };
        assert_eq!(not_run.iter().collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5]);
        assert_eq!(
            calls.each_ref().map(|n| n.load(Ordering::Relaxed)),
            [1, 1, 0, 0, 1, 1]
        );
    }
    gate.release();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match runner.run_block(generation, Mode::Serial, budget) {
            BlockOutcome::Recovered { job: 1 } => break,
            BlockOutcome::Faulted { job: 1, .. } if Instant::now() < until => thread::yield_now(),
            other => panic!("expected recovery, got {other:?}"),
        }
    }
    assert_eq!(calls[1].load(Ordering::Relaxed), 1);
    assert!(matches!(
        runner.run_block(generation, Mode::Serial, budget),
        BlockOutcome::Complete
    ));
    assert_eq!(calls[1].load(Ordering::Relaxed), 2);
}

#[test]
fn retired_generation_keeps_a_late_claim_and_all_of_its_data_alive() {
    let config = config(1);
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let gate = Arc::new(Gate::new());
    let calls = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
    let old = pool
        .admit(
            Schedule::build(2, &[], &config).unwrap(),
            GatedJobs {
                calls: calls.clone(),
                gate: Some(gate.clone()),
            },
        )
        .unwrap();
    let budget = Duration::from_millis(35);
    assert!(matches!(
        runner.run_block(old, Mode::Pool, budget),
        BlockOutcome::Faulted { job: 1, .. }
    ));
    let next = pool
        .admit(
            Schedule::build(1, &[], &config).unwrap(),
            GatedJobs {
                calls: Arc::new(std::array::from_fn(|_| AtomicUsize::new(0))),
                gate: None,
            },
        )
        .unwrap();
    assert!(!pool.quiescent(old));
    assert!(pool.reclaim(old).is_none());
    assert!(matches!(
        runner.run_block(old, Mode::Serial, budget),
        BlockOutcome::Inactive
    ));
    assert!(matches!(
        runner.run_block(next, Mode::Serial, budget),
        BlockOutcome::Complete
    ));
    gate.release();
    wait_for(|| pool.quiescent(old));
    let (_, returned) = pool.reclaim(old).unwrap();
    assert!(Arc::ptr_eq(&calls, &returned.calls));
    assert_eq!(calls[1].load(Ordering::Relaxed), 1);
    assert!(pool.reclaim(old).is_none());
    let newest = pool
        .admit(
            Schedule::build(0, &[], &config).unwrap(),
            GatedJobs {
                calls: returned.calls,
                gate: None,
            },
        )
        .unwrap();
    wait_for(|| pool.quiescent(next));
    assert!(pool.reclaim(next).is_some());
    assert!(!pool.quiescent(newest));
}

struct Node {
    calls: AtomicU32,
    epoch: AtomicU32,
    state: AtomicU32,
    output: Box<[AtomicU32]>,
}

struct Dag {
    nodes: Arc<[Node]>,
    deps: Vec<Vec<usize>>,
    scratch_busy: Box<[AtomicBool]>,
    scratch: Box<[AtomicU32]>,
    source: Arc<AtomicU32>,
    slot_mask: Arc<AtomicUsize>,
    errors: Arc<AtomicUsize>,
    jitter: bool,
}

// SAFETY: atomics make injected scheduler faults observable without adding test data races.
unsafe impl Jobs for Dag {
    fn run(&self, job: JobId, slot: WorkerSlot) {
        let node = &self.nodes[job as usize];
        let epoch = self.source.load(Ordering::Relaxed);
        self.slot_mask.fetch_or(1 << slot, Ordering::Relaxed);
        if epoch == 1 && job == 7 && slot == 0 {
            // First callback job in the wide level. Let every worker take an offer
            // even when the small-frame workload would otherwise finish while cold.
            let expected = (1 << self.scratch_busy.len()) - 1;
            let start = Instant::now();
            while self.slot_mask.load(Ordering::Relaxed) != expected
                && start.elapsed() < Duration::from_millis(500)
            {
                std::hint::spin_loop();
            }
        }
        if self.scratch_busy[slot].swap(true, Ordering::AcqRel) {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        if node.calls.fetch_add(1, Ordering::Relaxed) + 1 != epoch
            || self.deps[job as usize]
                .iter()
                .any(|&d| self.nodes[d].epoch.load(Ordering::Relaxed) != epoch)
        {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        let mut state = f32::from_bits(node.state.load(Ordering::Relaxed));
        self.scratch[slot].store(job, Ordering::Relaxed);
        if self.jitter {
            for _ in 0..(epoch.wrapping_mul(37).wrapping_add(job * 17) % 1_003) {
                std::hint::spin_loop();
            }
        }
        if self.scratch[slot].load(Ordering::Relaxed) != job {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        for (i, out) in node.output.iter().enumerate() {
            let mut input = (epoch + job + i as u32) as f32 * 0.001;
            for &d in &self.deps[job as usize] {
                input += f32::from_bits(self.nodes[d].output[i].load(Ordering::Relaxed));
            }
            state = state * 0.75 + input;
            out.store(state.to_bits(), Ordering::Relaxed);
        }
        node.state.store(state.to_bits(), Ordering::Relaxed);
        node.epoch.store(epoch, Ordering::Relaxed);
        self.scratch_busy[slot].store(false, Ordering::Release);
    }
}

fn render(frames: usize, workers: usize, jitter: bool) -> Vec<u32> {
    // Diamond, independent chain and a wide fan-out/fan-in.
    let mut edges = vec![(1, 0), (2, 0), (3, 1), (3, 2)];
    edges.extend((5..20).map(|job| (job, job - 1)));
    edges.extend((20..84).map(|job| (job, 3)));
    edges.extend((20..84).map(|dependency| (84, dependency)));
    let config = config(workers);
    let schedule = Schedule::build(85, &edges, &config).unwrap();
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let source = Arc::new(AtomicU32::new(0));
    let mask = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let mut deps = vec![vec![]; 85];
    for (job, dependency) in edges {
        deps[job as usize].push(dependency as usize);
    }
    let nodes: Arc<[Node]> = (0..85)
        .map(|_| Node {
            calls: AtomicU32::new(0),
            epoch: AtomicU32::new(0),
            state: AtomicU32::new(0),
            output: (0..frames).map(|_| AtomicU32::new(0)).collect(),
        })
        .collect();
    let dag = Dag {
        nodes: nodes.clone(),
        deps,
        scratch_busy: (0..pool.slots()).map(|_| AtomicBool::new(false)).collect(),
        scratch: (0..pool.slots()).map(|_| AtomicU32::new(0)).collect(),
        source: source.clone(),
        slot_mask: mask.clone(),
        errors: errors.clone(),
        jitter,
    };
    let generation = pool.admit(schedule, dag).unwrap();
    const BLOCKS: u32 = 32;
    let mut bits = Vec::with_capacity(BLOCKS as usize * nodes.len() * (frames + 1));
    let mode = if workers == 0 {
        Mode::Serial
    } else {
        Mode::Pool
    };
    for epoch in 1..=BLOCKS {
        source.store(epoch, Ordering::Relaxed);
        assert!(matches!(
            runner.run_block(generation, mode, Duration::from_secs(1)),
            BlockOutcome::Complete
        ));
        for node in &*nodes {
            assert_eq!(node.calls.load(Ordering::Relaxed), epoch);
            bits.push(node.state.load(Ordering::Relaxed));
            bits.extend(node.output.iter().map(|out| out.load(Ordering::Relaxed)));
        }
    }
    assert_eq!(errors.load(Ordering::Relaxed), 0);
    assert_eq!(mask.load(Ordering::Relaxed), (1 << (workers + 1)) - 1);
    bits
}

#[test]
fn canonical_bytes_across_slots_frames_and_completion_orders() {
    for frames in [1, 37, 64, 127, 128] {
        let serial = render(frames, 0, false);
        for workers in [1, 2, 4] {
            assert_eq!(serial, render(frames, workers, false));
            assert_eq!(serial, render(frames, workers, true));
        }
    }
}

#[test]
fn a_late_worker_within_budget_completes_exactly_once() {
    let config = config(1);
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let gate = Arc::new(Gate::new());
    let calls = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
    let generation = pool
        .admit(
            Schedule::build(6, &[(2, 1), (3, 2), (5, 4)], &config).unwrap(),
            GatedJobs {
                calls: calls.clone(),
                gate: Some(gate.clone()),
            },
        )
        .unwrap();
    thread::scope(|scope| {
        scope.spawn(|| {
            gate.wait();
            gate.release();
        });
        assert!(matches!(
            runner.run_block(generation, Mode::Pool, Duration::from_secs(1)),
            BlockOutcome::Complete
        ));
    });
    assert_eq!(calls.each_ref().map(|n| n.load(Ordering::Relaxed)), [1; 6]);
}

#[test]
fn a_worker_can_acknowledge_the_new_generation_before_claiming_the_old() {
    struct SwapJobs {
        callback: Option<Arc<Gate>>,
        worker: Option<Arc<Gate>>,
    }
    // SAFETY: only atomic gates are shared. The delays are bounded and test-only.
    unsafe impl Jobs for SwapJobs {
        fn run(&self, job: JobId, slot: WorkerSlot) {
            if (job, slot) == (1, 0)
                && let Some(worker) = &self.worker
            {
                // Job 2 has been offered. Keep callback work running until the worker
                // claims it so the empty-tail fast path cannot take back the offer.
                let start = Instant::now();
                while !worker.entered.load(Ordering::Acquire)
                    && start.elapsed() < Duration::from_secs(2)
                {
                    std::hint::spin_loop();
                }
                return;
            }
            let gate = match (job, slot) {
                (0, 0) => self.callback.as_ref(),
                (2, 1) => self.worker.as_ref(),
                _ => None,
            };
            if let Some(gate) = gate {
                gate.entered.store(true, Ordering::Release);
                let until = Instant::now() + Duration::from_secs(2);
                while !gate.released.load(Ordering::Acquire) && Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
        }
    }

    let config = config(1);
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let before_dispatch = Arc::new(Gate::new());
    let while_claimed = Arc::new(Gate::new());
    let old = pool
        .admit(
            Schedule::build(3, &[(1, 0), (2, 0)], &config).unwrap(),
            SwapJobs {
                callback: Some(before_dispatch.clone()),
                worker: Some(while_claimed.clone()),
            },
        )
        .unwrap();
    thread::scope(|scope| {
        let callback = scope.spawn(|| {
            matches!(
                runner.run_block(old, Mode::Pool, Duration::from_millis(350)),
                BlockOutcome::Faulted { job: 2, .. }
            )
        });
        before_dispatch.wait();
        let new = pool
            .admit(
                Schedule::build(0, &[], &config).unwrap(),
                SwapJobs {
                    callback: None,
                    worker: None,
                },
            )
            .unwrap();
        wait_for(|| pool.shared.workers[0].acknowledged() == new.number);
        assert!(!pool.quiescent(old), "callback still has a pin");
        before_dispatch.release();
        assert!(
            callback.join().unwrap(),
            "the acknowledged worker must hold job 2 past the deadline"
        );
        assert!(while_claimed.entered.load(Ordering::Acquire));
        // The pin has been cleared and the worker already acknowledged the new generation.
        assert!(!pool.quiescent(old), "worker still has an old claim");
        assert!(pool.reclaim(old).is_none());
        while_claimed.release();
        wait_for(|| pool.quiescent(old));
        assert!(pool.reclaim(old).is_some());
    });
}

#[test]
fn an_offer_that_never_gets_claimed_falls_back_in_canonical_order() {
    static POLLING: AtomicBool = AtomicBool::new(false);
    struct Calls(AtomicU32, AtomicUsize);
    // SAFETY: the order recorder and worker count are atomic.
    unsafe impl Jobs for Calls {
        fn run(&self, job: JobId, slot: WorkerSlot) {
            if slot == 0 {
                let _ = self
                    .0
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |seen| {
                        Some(seen.wrapping_mul(10).wrapping_add(job + 1))
                    });
            } else {
                self.1.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let config = PoolConfig {
        test_worker_context: Some(|poll| POLLING.load(Ordering::Acquire) && poll()),
        ..config(1)
    };
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let first = pool
        .admit(
            Schedule::build(4, &[(2, 1), (3, 2)], &config).unwrap(),
            Calls(AtomicU32::new(0), AtomicUsize::new(0)),
        )
        .unwrap();
    let start = Instant::now();
    assert!(matches!(
        runner.run_block(first, Mode::Pool, Duration::from_secs(2)),
        BlockOutcome::Complete
    ));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "waited for the worker deadline"
    );
    POLLING.store(true, Ordering::Release);
    pool.admit(
        Schedule::build(0, &[], &config).unwrap(),
        Calls(AtomicU32::new(0), AtomicUsize::new(0)),
    )
    .unwrap();
    wait_for(|| pool.quiescent(first));
    let (_, jobs) = pool.reclaim(first).unwrap();
    assert_eq!(jobs.0.into_inner(), 1234);
    assert_eq!(jobs.1.into_inner(), 0);
}

#[test]
fn startup_joins_each_stable_slot_before_accepting_blocks() {
    let slots = Arc::new(AtomicUsize::new(0));
    let joined = Arc::clone(&slots);
    let config = PoolConfig {
        workgroup_join: Some(Arc::new(move |slot| {
            joined.fetch_or(1 << slot, Ordering::Relaxed);
            Ok(())
        })),
        ..config(3)
    };
    let pool = WorkerPool::<GatedJobs>::start(config).unwrap();
    assert_eq!(slots.load(Ordering::Relaxed), 0b1110);
    assert_eq!(pool.slots(), 4);
    pool.shutdown();
}

#[test]
fn multiple_late_workers_report_the_union_and_recover_each_job() {
    struct Multi {
        entered: Arc<AtomicUsize>,
        released: Arc<AtomicUsize>,
    }
    // SAFETY: atomic rendezvous, with bounded delays to force the fault path.
    unsafe impl Jobs for Multi {
        fn run(&self, job: JobId, slot: WorkerSlot) {
            let start = Instant::now();
            if job == 0 {
                while self.entered.load(Ordering::Acquire) != 0b110
                    && start.elapsed() < Duration::from_secs(2)
                {
                    std::hint::spin_loop();
                }
            } else if slot != 0 {
                let bit = 1 << job;
                self.entered.fetch_or(bit, Ordering::Release);
                while self.released.load(Ordering::Acquire) & bit == 0
                    && start.elapsed() < Duration::from_secs(2)
                {
                    std::hint::spin_loop();
                }
            }
        }
    }

    let config = config(2);
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let entered = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicUsize::new(0));
    let id = pool
        .admit(
            Schedule::build(5, &[(3, 1), (4, 2)], &config).unwrap(),
            Multi {
                entered,
                released: released.clone(),
            },
        )
        .unwrap();
    let outcome = runner.run_block(id, Mode::Pool, Duration::from_millis(75));
    let BlockOutcome::Faulted { job: 1, not_run } = outcome else {
        panic!("expected both worker claims to outlive the deadline");
    };
    assert_eq!(not_run.iter().collect::<Vec<_>>(), [1, 2, 3, 4]);
    released.store(0b110, Ordering::Release);
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match runner.run_block(id, Mode::Serial, Duration::ZERO) {
            BlockOutcome::Recovered { job: 1 } => break,
            BlockOutcome::Faulted { .. } if Instant::now() < until => thread::yield_now(),
            other => panic!("expected first recovery, got {other:?}"),
        }
    }
    assert!(matches!(
        runner.run_block(id, Mode::Pool, Duration::ZERO),
        BlockOutcome::Recovered { job: 2 }
    ));
    assert!(matches!(
        runner.run_block(id, Mode::Serial, Duration::ZERO),
        BlockOutcome::Complete
    ));
}

#[test]
fn jobs_mut_is_available_exactly_when_worker_borrows_have_ended() {
    struct Inputs {
        input: usize,
        seen: Arc<AtomicUsize>,
        block_worker: bool,
        gate: Arc<Gate>,
    }
    // SAFETY: the caller writes the plain input through the guard only. The jobs
    // share atomics and hold the worker's borrow long enough to exercise refusal.
    unsafe impl Jobs for Inputs {
        fn run(&self, job: JobId, slot: WorkerSlot) {
            let until = Instant::now() + Duration::from_secs(2);
            if job == 0 && self.block_worker {
                while !self.gate.entered.load(Ordering::Acquire) && Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
            if job == 1 {
                self.seen.store(self.input, Ordering::Relaxed);
                if slot != 0 && self.block_worker {
                    self.gate.entered.store(true, Ordering::Release);
                    while !self.gate.released.load(Ordering::Acquire) && Instant::now() < until {
                        std::hint::spin_loop();
                    }
                }
            }
        }
    }
    let config = config(1);
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Gate::new());
    let generation = pool
        .admit(
            Schedule::build(2, &[], &config).unwrap(),
            Inputs {
                input: 0,
                seen: seen.clone(),
                block_worker: false,
                gate: gate.clone(),
            },
        )
        .unwrap();
    let mut admitted = runner.admitted(generation).unwrap();
    admitted.jobs_mut().unwrap().input = 1;
    assert!(matches!(
        admitted.run_block(Mode::Serial, Duration::ZERO),
        BlockOutcome::Complete
    ));
    assert_eq!(seen.load(Ordering::Relaxed), 1);
    let inputs = admitted.jobs_mut().unwrap();
    inputs.input = 2;
    inputs.block_worker = true;

    let budget = Duration::from_millis(75);
    assert!(matches!(
        admitted.run_block(Mode::Pool, budget),
        BlockOutcome::Faulted { job: 1, .. }
    ));
    assert_eq!(admitted.jobs().input, 2);
    assert_eq!(seen.load(Ordering::Relaxed), 2);
    let denied = admitted.jobs_mut().is_none();
    gate.release();
    assert!(denied, "a late worker still had a shared borrow");

    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match admitted.run_block(Mode::Serial, Duration::ZERO) {
            BlockOutcome::Recovered { job: 1 } => break,
            BlockOutcome::Faulted { .. } if Instant::now() < until => thread::yield_now(),
            other => panic!("expected recovery, got {other:?}"),
        }
    }
    let inputs = admitted.jobs_mut().unwrap();
    inputs.input = 3;
    inputs.block_worker = false;
    assert!(matches!(
        admitted.run_block(Mode::Pool, budget),
        BlockOutcome::Complete
    ));
    assert!(admitted.jobs_mut().is_some());
    assert_eq!(seen.load(Ordering::Relaxed), 3);
}

#[test]
fn deadline_settlement_never_reruns_an_already_completed_worker_job() {
    static COMPLETED_POLL: AtomicBool = AtomicBool::new(false);

    struct Calls(Arc<[AtomicUsize; 2]>);
    // SAFETY: all shared test state is atomic and the artificial spin is bounded.
    unsafe impl Jobs for Calls {
        fn run(&self, job: JobId, _: WorkerSlot) {
            self.0[job as usize].fetch_add(1, Ordering::Relaxed);
            if job == 0 {
                let start = Instant::now();
                // Wait for the real poll to publish completion, then cross the deadline
                // while still in callback-owned work so run_level cannot collect it.
                while !COMPLETED_POLL.load(Ordering::Acquire)
                    && start.elapsed() < Duration::from_secs(2)
                {
                    std::hint::spin_loop();
                }
                let delay = Instant::now();
                while delay.elapsed() < Duration::from_millis(30) {
                    std::hint::spin_loop();
                }
            }
        }
    }

    let config = PoolConfig {
        test_worker_context: Some(|poll| {
            let completed = poll();
            if completed {
                COMPLETED_POLL.store(true, Ordering::Release);
            }
            completed
        }),
        ..config(1)
    };
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let calls = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
    let id = pool
        .admit(
            Schedule::build(2, &[], &config).unwrap(),
            Calls(calls.clone()),
        )
        .unwrap();
    let mut runner = pool.take_runner().unwrap();
    assert!(matches!(
        runner.run_block(id, Mode::Pool, Duration::from_millis(10)),
        BlockOutcome::Complete
    ));
    assert!(
        COMPLETED_POLL.load(Ordering::Acquire),
        "worker did not claim job 1"
    );
    assert_eq!(calls.each_ref().map(|n| n.load(Ordering::Relaxed)), [1, 1]);
}

#[test]
fn later_level_fault_never_reruns_a_collected_worker_job() {
    static FIRST_POLL_COMPLETED: AtomicBool = AtomicBool::new(false);

    struct Calls {
        counts: Arc<[AtomicUsize; 4]>,
        later: Arc<Gate>,
    }

    // SAFETY: all shared state is atomic, and each test-only spin has an upper bound.
    unsafe impl Jobs for Calls {
        fn run(&self, job: JobId, slot: WorkerSlot) {
            self.counts[job as usize].fetch_add(1, Ordering::Relaxed);
            let flag = match (job, slot) {
                (0, 0) => &FIRST_POLL_COMPLETED,
                (2, 0) => &self.later.entered,
                (3, 1) => {
                    self.later.entered.store(true, Ordering::Release);
                    &self.later.released
                }
                _ => return,
            };
            let start = Instant::now();
            while !flag.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(2) {
                std::hint::spin_loop();
            }
        }
    }

    let config = PoolConfig {
        test_worker_context: Some(|poll| {
            let completed = poll();
            if completed {
                FIRST_POLL_COMPLETED.store(true, Ordering::Release);
            }
            completed
        }),
        ..config(1)
    };
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let counts = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
    let later = Arc::new(Gate::new());
    let id = pool
        .admit(
            Schedule::build(4, &[(2, 0), (2, 1), (3, 0), (3, 1)], &config).unwrap(),
            Calls {
                counts: counts.clone(),
                later: later.clone(),
            },
        )
        .unwrap();

    wait_for(|| pool.shared.workers[0].acknowledged() == id.number);
    let outcome = runner.run_block(id, Mode::Pool, Duration::from_millis(40));
    later.release();
    assert!(
        matches!(outcome, BlockOutcome::Faulted { job: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        counts.each_ref().map(|n| n.load(Ordering::Relaxed)),
        [1, 1, 1, 1]
    );
}
