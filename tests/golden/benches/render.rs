//! Offline render speed and block times of the reference scenes (`grooviq_golden::perf`).
//!
//! `cargo bench -p grooviq-golden --bench render [-- <scene name filter>]`
//!
//! Environment: `GROOVIQ_PERF_SECONDS` (audio per repetition, default 30), `GROOVIQ_PERF_REPS`
//! (timed repetitions, default 5), `GROOVIQ_PERF_JSON` (also write the results there), and
//! `GITHUB_STEP_SUMMARY` (CI: append a Markdown table). Exits with an error if a scene's bits
//! depend on the block size; the timings themselves never fail it.

use std::fmt::Write as _;
use std::process::ExitCode;

use grooviq_golden::SAMPLE_RATE;
use grooviq_golden::perf::{Summary, measure, perf_scenes};

const BLOCKS: [usize; 3] = [64, 128, 256];

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
        "{:<10} {:>5} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8} {:>7} {:>7}  hash",
        "scene",
        "block",
        "x RT med",
        "x RT min",
        "p50 us",
        "p99 us",
        "p99.9 us",
        "max us",
        "p99 %",
        "max %"
    );
    let mut summaries = Vec::new();
    let mut failed = false;
    for scene in perf_scenes() {
        if filter.as_deref().is_some_and(|f| !scene.name.contains(f)) {
            continue;
        }
        let mut hashes = Vec::new();
        for block in BLOCKS {
            let m = match measure(&scene, block, frames, reps) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("error: {e}");
                    failed = true;
                    continue;
                }
            };
            let s = m.summary();
            println!(
                "{:<10} {:>5} {:>9.1} {:>9.1} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>7.2} {:>7.2}  {:016x}",
                s.scene,
                s.block,
                s.speed_median,
                s.speed_worst,
                us(s.p50_ns),
                us(s.p99_ns),
                us(s.p999_ns),
                us(s.max_ns),
                pct(s.p99_ns, &s),
                pct(s.max_ns, &s),
                s.hash
            );
            hashes.push(s.hash);
            summaries.push(s);
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

fn us(ns: u64) -> f64 {
    ns as f64 / 1e3
}

/// A block time as a percentage of its deadline.
fn pct(ns: u64, s: &Summary) -> f64 {
    ns as f64 / s.deadline_ns * 100.0
}

fn markdown(summaries: &[Summary], frames: usize, reps: usize) -> String {
    let mut out = format!(
        "### Render speed and block times\n\n{:.1} s of audio at {SAMPLE_RATE} Hz, {reps} timed \
         repetitions. Block times are single `process` calls, as a share of the block's \
         real-time deadline.\n\n\
         | scene | block | x real time (median) | x real time (slowest) | p50 µs | p99 µs | \
         p99.9 µs | max µs | p99 % of deadline | max % of deadline | output hash |\n\
         |---|--:|--:|--:|--:|--:|--:|--:|--:|--:|---|\n",
        frames as f64 / f64::from(SAMPLE_RATE)
    );
    for s in summaries {
        let _ = writeln!(
            out,
            "| {} | {} | {:.1} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | `{:016x}` |",
            s.scene,
            s.block,
            s.speed_median,
            s.speed_worst,
            us(s.p50_ns),
            us(s.p99_ns),
            us(s.p999_ns),
            us(s.max_ns),
            pct(s.p99_ns, s),
            pct(s.max_ns, s),
            s.hash
        );
    }
    out.push('\n');
    out
}

fn json(summaries: &[Summary], frames: usize, reps: usize) -> String {
    let rows: Vec<String> = summaries
        .iter()
        .map(|s| {
            format!(
                "    {{\"scene\": \"{}\", \"block\": {}, \"deadline_ns\": {:.0}, \
                 \"x_realtime_median\": {:.3}, \"x_realtime_slowest\": {:.3}, \
                 \"p50_ns\": {}, \"p99_ns\": {}, \"p999_ns\": {}, \"max_ns\": {}, \
                 \"hash\": \"{:016x}\"}}",
                s.scene,
                s.block,
                s.deadline_ns,
                s.speed_median,
                s.speed_worst,
                s.p50_ns,
                s.p99_ns,
                s.p999_ns,
                s.max_ns,
                s.hash
            )
        })
        .collect();
    format!(
        "{{\n  \"sample_rate\": {SAMPLE_RATE},\n  \"frames\": {frames},\n  \"reps\": {reps},\n  \
         \"results\": [\n{}\n  ]\n}}\n",
        rows.join(",\n")
    )
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
