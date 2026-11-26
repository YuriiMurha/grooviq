use std::fmt::Write as _;

use serde_json::json;

use crate::SAMPLE_RATE;

use super::{B2_TARGET_PCT, Summary};

const PROMOTED: &str = "promoted";

pub struct Row {
    pub summary: Summary,
    pub frames: usize,
    pub promotion: String,
}

impl Row {
    pub fn audio_seconds(&self) -> f64 {
        self.frames as f64 / f64::from(SAMPLE_RATE)
    }

    fn under_b2_target(&self) -> Option<bool> {
        if self.promotion == PROMOTED {
            self.summary.under_b2_target()
        } else {
            None
        }
    }
}

/// A row is promoted only if every timed repetition succeeded.
pub fn promotion_label(rejections: impl Iterator<Item = Option<String>>) -> String {
    rejections.flatten().next().map_or_else(
        || PROMOTED.into(),
        |reason| format!("not promoted: {reason}"),
    )
}

pub fn us(ns: u64) -> f64 {
    ns as f64 / 1e3
}

pub fn b2_target(row: &Row) -> &'static str {
    match row.under_b2_target() {
        Some(true) => "under 50%",
        Some(false) => "at or over 50%",
        None if row.summary.block == 128 => "— (not promoted)",
        None => "—",
    }
}

pub fn markdown(rows: &[Row], reps: usize) -> String {
    let mut out = format!(
        "### Render speed and block times\n\nAudio at {SAMPLE_RATE} Hz, {reps} timed \
         repetitions. For promoted 128-frame runs, the target is a max below 50% of the \
         deadline. Preemption counts are wall-time outliers above 10× the row's p50.\n\n\
         | scene | block | audio s | x real time (median) | x real time (slowest) | p50 µs | p99 µs | \
         p99.9 µs | max µs | deadline µs | p99 % of deadline | max % of deadline | preemption count | \
         128-frame target | thread priority | output hash |\n\
         |---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|---|---|---|\n"
    );
    for row in rows {
        let s = &row.summary;
        let _ = writeln!(
            out,
            "| {} | {} | {:.1} | {:.1} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {} | `{:016x}` |",
            s.scene,
            s.block,
            row.audio_seconds(),
            s.speed_median,
            s.speed_worst,
            us(s.p50_ns),
            us(s.p99_ns),
            us(s.p999_ns),
            us(s.max_ns),
            s.deadline_ns / 1e3,
            s.deadline_pct(s.p99_ns),
            s.deadline_pct(s.max_ns),
            s.preemption_count,
            b2_target(row),
            row.promotion.replace('|', "\\|").replace(['\r', '\n'], " "),
            s.hash
        );
    }
    out.push('\n');
    out
}

pub fn json(rows: &[Row], reps: usize) -> String {
    let results: Vec<_> = rows
        .iter()
        .map(|row| {
            let s = &row.summary;
            let under_b2_target = row.under_b2_target();
            json!({
                "scene": s.scene,
                "block": s.block,
                "frames": row.frames,
                "deadline_ns": s.deadline_ns.round() as u64,
                "x_realtime_median": s.speed_median,
                "x_realtime_slowest": s.speed_worst,
                "p50_ns": s.p50_ns,
                "p99_ns": s.p99_ns,
                "p999_ns": s.p999_ns,
                "max_ns": s.max_ns,
                "hash": format!("{:016x}", s.hash),
                "promotion": row.promotion,
                "preemption_count": s.preemption_count,
                "max_deadline_pct": s.deadline_pct(s.max_ns),
                "target_deadline_pct": under_b2_target.map(|_| B2_TARGET_PCT),
                "under_b2_target": under_b2_target,
            })
        })
        .collect();
    let report = json!({
        "sample_rate": SAMPLE_RATE,
        "reps": reps,
        "results": results,
    });
    format!("{report:#}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::Measurement;

    fn row(block: usize, promotion: &str) -> Row {
        let mut times = vec![10_000; 1_000];
        times.extend([20_000; 995]);
        times.extend([100_000, 110_000, 150_000, 190_000, 1_000_000]);
        Row {
            frames: 2_000 * block,
            summary: Measurement {
                scene: "test_scene",
                block,
                frames: 2_000 * block,
                block_ns: times,
                speed: vec![100.0],
                hash: 0x1234,
            }
            .summary(),
            promotion: promotion.to_owned(),
        }
    }

    #[test]
    fn counts_clusters_of_blocks_strictly_over_ten_times_p50() {
        let s = row(128, "promoted").summary;
        assert_eq!((s.p50_ns, s.p99_ns, s.p999_ns), (10_000, 20_000, 150_000));
        assert_eq!(s.preemption_count, 4);
    }

    #[test]
    fn promotion_requires_success_in_every_repetition() {
        assert_eq!(promotion_label([None, None].into_iter()), "promoted");
        assert_eq!(
            promotion_label(
                [
                    None,
                    Some("first refusal".into()),
                    Some("later refusal".into()),
                    None
                ]
                .into_iter()
            ),
            "not promoted: first refusal"
        );
    }

    #[test]
    fn b2_requires_strictly_less_than_half_the_128_frame_deadline() {
        let mut s = row(128, "promoted").summary;
        assert!((s.deadline_pct(s.max_ns) - 37.5).abs() < 1e-9);
        assert_eq!(s.under_b2_target(), Some(true));
        s.deadline_ns = 2_000_000.0;
        assert_eq!(s.under_b2_target(), Some(false));
        assert_eq!(row(64, "promoted").summary.under_b2_target(), None);
        assert_eq!(row(256, "promoted").summary.under_b2_target(), None);
    }

    #[test]
    fn json_preserves_hash_keys_and_reports_each_promotion_result() {
        let statuses = ["promoted", "not promoted: denied | \"policy\""];
        let rows = statuses.map(|status| row(128, status));
        let report: serde_json::Value = serde_json::from_str(&json(&rows, 1)).unwrap();
        for (i, status) in statuses.into_iter().enumerate() {
            let row = &report["results"][i];
            assert_eq!(row["scene"], "test_scene");
            assert_eq!(row["block"], 128);
            assert_eq!(row["frames"], 256_000);
            assert_eq!(row["hash"], "0000000000001234");
            assert_eq!(row["promotion"], status);
            assert_eq!(row["preemption_count"], 4);
            assert!((row["max_deadline_pct"].as_f64().unwrap() - 37.5).abs() < 1e-9);
        }
        let promoted = &report["results"][0];
        assert_eq!(promoted["target_deadline_pct"], 50.0);
        assert_eq!(promoted["under_b2_target"], true);
        let rejected = &report["results"][1];
        assert_eq!(
            rejected.get("target_deadline_pct"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            rejected.get("under_b2_target"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn markdown_reports_priority_and_target_per_row() {
        let mut rows = [
            row(128, "promoted"),
            row(128, "not promoted: denied | policy"),
        ];
        rows[1].summary.max_ns = 2_000_000;
        let report = markdown(&rows, 1);
        assert!(report.contains("| preemption count | 128-frame target | thread priority |"));
        assert!(report.contains("| 37.50 | 4 | under 50% | promoted |"));
        assert!(
            report.contains("| 75.00 | 4 | — (not promoted) | not promoted: denied \\| policy |")
        );
    }
}
