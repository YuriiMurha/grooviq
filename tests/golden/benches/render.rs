//! Offline render speed and block times of the reference scenes (`grooviq_golden::perf`).
//!
//! `cargo bench -p grooviq-golden --bench render [-- <scene name filter>]`
//!
//! Environment: `GROOVIQ_PERF_SECONDS` (audio per repetition, default 30), `GROOVIQ_PERF_REPS`
//! (timed repetitions, default 5), `GROOVIQ_PERF_JSON` (also write the results there), and
//! `GITHUB_STEP_SUMMARY` (CI: append a Markdown table). Exits with an error if a scene's bits
//! depend on the block size; the timings themselves never fail it.

use std::process::ExitCode;

use audio_thread_priority::{
    RtPriorityHandle, demote_current_thread_from_real_time, promote_current_thread_to_real_time,
};
use grooviq_golden::SAMPLE_RATE;
use grooviq_golden::perf::report::{Row, b2_target, json, markdown, us};
use grooviq_golden::perf::{PerfScene, measure_with_timed_loop, perf_scenes};

const BLOCKS: [usize; 3] = [64, 128, 256];

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct RealtimeScope(Option<RtPriorityHandle>);

impl RealtimeScope {
    fn enter(block: usize) -> Result<Self, String> {
        // Hosted CI timings are advisory even when the guest accepts a priority request.
        if std::env::var_os("CI").is_some() {
            return Err("CI runner".into());
        }
        // The crate's fallback backend returns success without changing priority.
        if !cfg!(any(target_os = "macos", target_os = "windows")) {
            return Err("unsupported platform".into());
        }
        let block = u32::try_from(block).expect("the benchmark block size fits in u32");
        promote_current_thread_to_real_time(block, SAMPLE_RATE)
            .map(|handle| Self(Some(handle)))
            .map_err(|e| e.to_string())
    }
}

impl Drop for RealtimeScope {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take()
            && let Err(e) = demote_current_thread_from_real_time(handle)
        {
            eprintln!("warning: cannot restore render worker priority: {e}");
        }
    }
}

fn main() -> ExitCode {
    let seconds: f64 = env_or("GROOVIQ_PERF_SECONDS", 30.0);
    let reps: usize = env_or("GROOVIQ_PERF_REPS", 5).max(1);
    // Whole 256-frame blocks, so every block size renders the same frames.
    let frames = ((seconds * f64::from(SAMPLE_RATE) / 256.0).round() as usize).max(1) * 256;
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));

    println!(
        "{:.1} s of audio at {SAMPLE_RATE} Hz per repetition, {reps} timed repetitions\n",
        frames as f64 / f64::from(SAMPLE_RATE)
    );
    println!(
        "{:<10} {:>5} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8} {:>7} {:>7} {:>11} {:>16}  {:16}  thread priority",
        "scene",
        "block",
        "x RT med",
        "x RT min",
        "p50 us",
        "p99 us",
        "p99.9 us",
        "max us",
        "p99 %",
        "max %",
        "preemptions",
        "128-frame target",
        "hash"
    );
    let mut summaries = Vec::new();
    let mut failed = false;
    for scene in perf_scenes() {
        if filter.as_deref().is_some_and(|f| !scene.name.contains(f)) {
            continue;
        }
        let mut hashes = Vec::new();
        for block in BLOCKS {
            let row = match measure_row(&scene, block, frames, reps) {
                Ok(row) => row,
                Err(e) => {
                    eprintln!("error: {e}");
                    failed = true;
                    continue;
                }
            };
            let s = &row.summary;
            println!(
                "{:<10} {:>5} {:>9.1} {:>9.1} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>7.2} {:>7.2} {:>11} {:>16}  {:016x}  {}",
                s.scene,
                s.block,
                s.speed_median,
                s.speed_worst,
                us(s.p50_ns),
                us(s.p99_ns),
                us(s.p999_ns),
                us(s.max_ns),
                s.deadline_pct(s.p99_ns),
                s.deadline_pct(s.max_ns),
                s.preemption_count,
                b2_target(s),
                s.hash,
                row.promotion
            );
            hashes.push(s.hash);
            summaries.push(row);
        }
        if hashes.windows(2).any(|w| w[0] != w[1]) {
            eprintln!(
                "error: {} renders different bits at block sizes {BLOCKS:?}",
                scene.name
            );
            failed = true;
        }
    }

    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") {
        append(&path, &markdown(&summaries, frames, reps));
    }
    if let Ok(path) = std::env::var("GROOVIQ_PERF_JSON")
        && let Err(e) = std::fs::write(&path, json(&summaries, frames, reps))
    {
        eprintln!("error: cannot write {path}: {e}");
        failed = true;
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn measure_row(scene: &PerfScene, block: usize, frames: usize, reps: usize) -> Result<Row, String> {
    let mut refusal = None;
    let measurement = measure_with_timed_loop(scene, block, frames, reps, |render| {
        // Mach may retain a time-constraint policy after demotion, so end the worker here.
        let rejection = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let priority = RealtimeScope::enter(block);
                    render();
                    priority.err()
                })
                .join()
                .expect("render worker panicked")
        });
        if let Some(reason) = rejection {
            refusal.get_or_insert(reason);
        }
    })?;
    Ok(Row {
        summary: measurement.summary(),
        promotion: match refusal {
            Some(reason) => format!("not promoted: {reason}"),
            None => "promoted".into(),
        },
    })
}

fn append(path: &str, text: &str) {
    use std::io::Write;
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path);
    if let Err(e) = file.and_then(|mut f| f.write_all(text.as_bytes())) {
        eprintln!("warning: cannot write the job summary {path}: {e}");
    }
}
