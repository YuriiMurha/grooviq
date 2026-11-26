use std::time::Duration;

pub const WARN_AT: Duration = Duration::from_secs(4);
pub const FAIL_AT: Duration = Duration::from_secs(9);

#[derive(Debug, PartialEq, Eq)]
pub enum ExportBudget {
    Pass,
    Warn,
    Fail,
}

pub fn classify(median: Duration) -> ExportBudget {
    if median > FAIL_AT {
        ExportBudget::Fail
    } else if median > WARN_AT {
        ExportBudget::Warn
    } else {
        ExportBudget::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_medians_strictly_above_the_thresholds_trigger() {
        for (millis, expected) in [
            (4_000, ExportBudget::Pass),
            (4_001, ExportBudget::Warn),
            (9_000, ExportBudget::Warn),
            (9_001, ExportBudget::Fail),
        ] {
            assert_eq!(classify(Duration::from_millis(millis)), expected);
        }
    }
}
