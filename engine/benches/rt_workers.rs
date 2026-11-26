//! Offline block timings and idle callback-entry-to-worker-entry latency on a shared host.
use divan::{Bencher, black_box};
use grooviq_engine::rt_workers::*;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const JOBS: u32 = 24;
const WAIT: Duration = Duration::from_millis(100);
const WAKE_WAIT: Duration = Duration::from_millis(4);

fn main() {
    if std::env::args().any(|arg| arg == "--wake-latency") {
        wake_latency();
    } else {
        divan::main();
    }
}

struct Node {
    input: Option<JobId>,
    samples: Box<[UnsafeCell<f32>]>,
    state: UnsafeCell<f32>,
}

struct Render(Vec<Node>);

// SAFETY: a job is the sole writer of its node. Each input is its registered predecessor;
// the worker pool establishes ordering between its writes and job reads.
unsafe impl Sync for Render {}
unsafe impl Jobs for Render {
    fn run(&self, job: JobId, _: WorkerSlot) {
        let node = &self.0[job as usize];
        let mut state = unsafe { *node.state.get() };
        for (frame, sample) in node.samples.iter().enumerate() {
            let input = match node.input {
                Some(input) => unsafe { *self.0[input as usize].samples[frame].get() },
                None => (1 + frame) as f32 * 0.001,
            };
            for _ in 0..16 {
                state = black_box(state * 0.75 + input * 0.01);
            }
            unsafe { *sample.get() = state };
        }
        unsafe { *node.state.get() = state };
    }
}

fn render(bencher: Bencher, config: (usize, usize), chain: bool) {
    let (frames, workers) = config;
    let config = PoolConfig {
        workers,
        max_frames: frames as u32,
        ..PoolConfig::default()
    };
    let edges: Vec<_> = (1..JOBS)
        .map(|job| (job, if chain { job - 1 } else { 0 }))
        .collect();
    let schedule = Schedule::build(JOBS as usize, &edges, &config).unwrap();
    let jobs = Render(
        (0..JOBS)
            .map(|job| Node {
                input: job
                    .checked_sub(1)
                    .map(|previous| if chain { previous } else { 0 }),
                samples: (0..frames).map(|_| UnsafeCell::new(0.0)).collect(),
                state: UnsafeCell::new(0.0),
            })
            .collect(),
    );
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let generation = pool.admit(schedule, jobs).unwrap();
    let mut runner = pool.take_runner().unwrap();
    let mode = if workers == 0 {
        Mode::Serial
    } else {
        Mode::Pool
    };
    let mut block = || {
        let outcome = runner.run_block(generation, mode, WAIT);
        assert!(matches!(outcome, BlockOutcome::Complete), "{outcome:?}");
    };
    for _ in 0..32 {
        block();
    }
    bencher.bench_local(block);
}

#[divan::bench(args = [(64, 0), (64, 1), (64, 2), (64, 4), (128, 0), (128, 1), (128, 2), (128, 4)])]
fn chain(bencher: Bencher, config: (usize, usize)) {
    render(bencher, config, true);
}

#[divan::bench(args = [(64, 0), (64, 1), (64, 2), (64, 4), (128, 0), (128, 1), (128, 2), (128, 4)])]
fn wide(bencher: Bencher, config: (usize, usize)) {
    render(bencher, config, false);
}

struct WakeProbe {
    clock: Instant,
    entry: Arc<AtomicU64>,
}

// SAFETY: the sole sampled worker writes only an atomic scalar.
unsafe impl Jobs for WakeProbe {
    fn run(&self, _: JobId, slot: WorkerSlot) {
        if slot == 1 {
            self.entry
                .store(self.clock.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}

fn wake_latency() {
    let config = PoolConfig {
        workers: 1,
        ..PoolConfig::default()
    };
    let mut pool = WorkerPool::start(config.clone()).unwrap();
    let entry = Arc::new(AtomicU64::new(0));
    let clock = Instant::now();
    let generation = pool
        .admit(
            Schedule::build(2, &[], &config).unwrap(),
            WakeProbe {
                clock,
                entry: entry.clone(),
            },
        )
        .unwrap();
    let mut runner = pool.take_runner().unwrap();
    let mut samples = Vec::new();
    const SAMPLES: usize = 1_000;
    for sample in 0..SAMPLES {
        std::thread::sleep(Duration::from_micros(500 + sample as u64 % 29));
        entry.store(0, Ordering::Relaxed);
        let start = clock.elapsed().as_nanos() as u64;
        assert!(matches!(
            runner.run_block(generation, Mode::Pool, WAKE_WAIT),
            BlockOutcome::Complete
        ));
        if let Some(elapsed) = entry.load(Ordering::Relaxed).checked_sub(start) {
            samples.push(elapsed);
        }
    }
    samples.sort_unstable();
    assert!(!samples.is_empty(), "worker never claimed a job");
    for (label, percentile) in [("p50", 50), ("p95", 95), ("p99", 99), ("max", 100)] {
        println!(
            "{label} {:.1} us",
            samples[(samples.len() - 1) * percentile / 100] as f64 / 1000.0
        );
    }
    println!("{}/{} claimed by a worker", samples.len(), SAMPLES);
}
