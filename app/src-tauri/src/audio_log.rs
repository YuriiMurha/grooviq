//! The audio log: what happened to the output and the engine, newest last, for the popover.

use serde::Serialize;
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

/// Entries kept in memory; older ones fall off.
pub const LOG_CAPACITY: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// Milliseconds since the Unix epoch.
    pub at_ms: u64,
    pub level: Level,
    pub message: String,
}

/// The last [`LOG_CAPACITY`] entries.
#[derive(Debug, Default)]
pub struct AudioLog {
    entries: VecDeque<LogEntry>,
}

impl AudioLog {
    /// Adds an entry stamped now.
    pub fn push(&mut self, level: Level, message: impl Into<String>) {
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        self.push_at(at_ms, level, message.into());
    }

    fn push_at(&mut self, at_ms: u64, level: Level, message: String) {
        if self.entries.len() == LOG_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(LogEntry {
            at_ms,
            level,
            message,
        });
    }

    /// Every entry kept, oldest first.
    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_200_entries_oldest_first() {
        let mut log = AudioLog::default();
        for i in 0..250u64 {
            log.push_at(i, Level::Info, format!("entry {i}"));
        }
        let entries = log.entries();
        assert_eq!(entries.len(), LOG_CAPACITY);
        assert_eq!(entries[0].message, "entry 50");
        assert_eq!(entries[0].at_ms, 50);
        assert_eq!(entries[199].message, "entry 249");
    }

    #[test]
    fn serialises_as_the_ui_expects() {
        let mut log = AudioLog::default();
        log.push_at(7, Level::Warn, "careful".into());
        let json = serde_json::to_value(log.entries()).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{ "atMs": 7, "level": "warn", "message": "careful" }])
        );
    }

    #[test]
    fn push_stamps_the_current_time() {
        let mut log = AudioLog::default();
        log.push(Level::Error, "now");
        let at = log.entries()[0].at_ms;
        // After 2020-01-01: the clock was read, not left at zero.
        assert!(at > 1_577_836_800_000, "{at}");
    }
}
