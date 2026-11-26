//! Warmed production exports of the reopened M3 golden project.
//! Set `GROOVIQ_SONG_EXPORT_BUDGET=1` to apply the advisory CI budget.

use std::{
    ffi::OsStr,
    process::{Command, ExitCode},
    thread::available_parallelism,
    time::{Duration, Instant},
};

use grooviq_golden::perf::export_budget::{ExportBudget, FAIL_AT, WARN_AT, classify};
use grooviq_golden::song;

#[path = "support/song_workload.rs"]
mod song_workload;

const REPS: usize = 5;

fn cpu_model() -> String {
    if cfg!(target_os = "linux") {
        return std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|info| {
                info.lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(key, _)| key.trim() == "model name")
                    .map(|(_, value)| value.trim().to_owned())
            })
            .unwrap_or_else(|| "unknown".into());
    }
    let (program, args): (_, &[_]) = match std::env::consts::OS {
        "macos" => ("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "windows" => (
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name",
            ],
        ),
        _ => return "unknown".into(),
    };
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

fn main() -> ExitCode {
    let cores = available_parallelism().map_or_else(|_| "unknown".into(), |n| n.to_string());
    let seconds = song::FRAMES as f64 / f64::from(song::options().sample_rate);
    println!("### M3 production export (advisory)");
    println!();
    println!("Runner: {} ({cores} available logical cores)", cpu_model());
    println!("{seconds:.0} s of audio, one warm-up and {REPS} timed exports");
    println!();

    let dir = tempfile::tempdir().unwrap();
    let session = song_workload::reopen(dir.path());
    let path = dir.path().join("garden.wav");
    assert_eq!(song::export(&session, &path).frames, song::FRAMES as u64);

    let mut elapsed = [Duration::ZERO; REPS];
    for (i, time) in elapsed.iter_mut().enumerate() {
        let start = Instant::now();
        let report = song::export(&session, &path);
        *time = start.elapsed();
        assert_eq!(report.frames, song::FRAMES as u64);
        let elapsed_seconds = time.as_secs_f64();
        println!(
            "- Run {}: {elapsed_seconds:.3} s ({:.1}× real time)",
            i + 1,
            seconds / elapsed_seconds
        );
    }
    elapsed.sort_unstable();
    let median = elapsed[REPS / 2];
    let median_seconds = median.as_secs_f64();
    println!();
    println!(
        "Median: {median_seconds:.3} s ({:.1}× real time)",
        seconds / median_seconds
    );
    if std::env::var_os("GROOVIQ_SONG_EXPORT_BUDGET").as_deref() == Some(OsStr::new("1")) {
        let budget = classify(median);
        if budget != ExportBudget::Pass {
            println!(
                "::warning::M3 export median {median_seconds:.6} s exceeded {} s",
                WARN_AT.as_secs()
            );
        }
        if budget == ExportBudget::Fail {
            println!(
                "::error::M3 export median {median_seconds:.6} s exceeded the {} s budget",
                FAIL_AT.as_secs()
            );
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
