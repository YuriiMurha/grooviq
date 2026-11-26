//! Warmed production exports of the reopened M3 golden project. Timings are advisory.

use std::{process::Command, thread::available_parallelism, time::Instant};

use grooviq_golden::{perf::median, song};

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

fn main() {
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

    let mut elapsed = [0.0; REPS];
    for (i, time) in elapsed.iter_mut().enumerate() {
        let start = Instant::now();
        let report = song::export(&session, &path);
        *time = start.elapsed().as_secs_f64();
        assert_eq!(report.frames, song::FRAMES as u64);
        println!(
            "- Run {}: {:.3} s ({:.1}× real time)",
            i + 1,
            time,
            seconds / *time
        );
    }
    elapsed.sort_by(f64::total_cmp);
    let median = median(&elapsed);
    println!();
    println!("Median: {median:.3} s ({:.1}× real time)", seconds / median);
}
