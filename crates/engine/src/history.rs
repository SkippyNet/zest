//! A log of what Zest has cleaned, plus lifetime totals.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zest_core::{IoContext, Result, paths};

/// Maximum number of runs kept in the log (totals keep counting).
pub const MAX_ENTRIES: usize = 500;

/// What started a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RunKind {
    /// Custom Clean started by the user.
    Manual,
    /// Health Check.
    HealthCheck,
    /// Scheduled task.
    Scheduled,
    /// Smart cleaning by the background agent.
    Smart,
    /// Registry cleaner.
    Registry,
    /// One of the tools (duplicates, uninstall leftovers, shredder, ...).
    Tools,
}

impl RunKind {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            RunKind::Manual => "Custom Clean",
            RunKind::HealthCheck => "Health Check",
            RunKind::Scheduled => "Scheduled clean",
            RunKind::Smart => "Smart clean",
            RunKind::Registry => "Registry clean",
            RunKind::Tools => "Tools",
        }
    }
}

/// One finished run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// When the run finished (seconds since the Unix epoch).
    pub time: i64,
    /// What started it.
    pub kind: RunKind,
    /// Bytes freed.
    pub bytes: u64,
    /// Items removed.
    pub items: u64,
    /// How long it took, in milliseconds.
    pub duration_ms: u64,
    /// Bytes freed per area, largest first: `(name, bytes)`.
    #[serde(default)]
    pub areas: Vec<(String, u64)>,
}

impl HistoryEntry {
    /// Creates an entry stamped with the current time.
    pub fn now(kind: RunKind, bytes: u64, items: u64, duration_ms: u64) -> Self {
        HistoryEntry {
            time: chrono::Utc::now().timestamp(),
            kind,
            bytes,
            items,
            duration_ms,
            areas: Vec::new(),
        }
    }

    /// The finish time as a `SystemTime`.
    pub fn system_time(&self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(self.time.max(0) as u64)
    }
}

/// The cleaning log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    /// Runs, oldest first.
    pub entries: Vec<HistoryEntry>,
    /// Bytes freed since installation.
    pub total_bytes: u64,
    /// Items removed since installation.
    pub total_items: u64,
    /// Number of runs since installation.
    pub runs: u64,
}

impl History {
    /// Default location of the history file.
    pub fn path() -> PathBuf {
        paths::data_dir().join("history.json")
    }

    /// Loads the history; a missing or damaged file yields an empty history.
    pub fn load() -> History {
        Self::load_from(&Self::path())
    }

    /// Loads from an explicit file.
    pub fn load_from(file: &Path) -> History {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Saves to the default location.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    /// Saves to an explicit file (atomically).
    pub fn save_to(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            paths::ensure_dir(dir)?;
        }
        let mut tmp = file.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .ctx(format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, file).ctx(format!("replacing {}", file.display()))
    }

    /// Adds a run and updates the totals.
    pub fn record(&mut self, entry: HistoryEntry) {
        self.total_bytes = self.total_bytes.saturating_add(entry.bytes);
        self.total_items = self.total_items.saturating_add(entry.items);
        self.runs += 1;
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..excess);
        }
    }

    /// The most recent run, if any.
    pub fn last(&self) -> Option<&HistoryEntry> {
        self.entries.last()
    }

    /// Bytes freed in the last `days` days.
    pub fn bytes_since_days(&self, days: i64) -> u64 {
        let cutoff = chrono::Utc::now().timestamp() - days * 86_400;
        self.entries
            .iter()
            .filter(|e| e.time >= cutoff)
            .map(|e| e.bytes)
            .sum()
    }

    /// Loads, records one run and saves again. Errors are logged, not returned:
    /// failing to write the log must never fail a clean.
    pub fn append(entry: HistoryEntry) {
        let mut history = History::load();
        history.record(entry);
        if let Err(e) = history.save() {
            log::warn!("could not save history: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_and_cap() {
        let mut history = History::default();
        for i in 0..(MAX_ENTRIES as u64 + 10) {
            history.record(HistoryEntry::now(RunKind::Manual, 100, 2, i));
        }
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        assert_eq!(history.runs, MAX_ENTRIES as u64 + 10);
        assert_eq!(history.total_bytes, 100 * (MAX_ENTRIES as u64 + 10));
        assert_eq!(history.total_items, 2 * (MAX_ENTRIES as u64 + 10));
        // The oldest entries were dropped.
        assert_eq!(history.entries[0].duration_ms, 10);
        assert_eq!(history.last().unwrap().duration_ms, MAX_ENTRIES as u64 + 9);
    }

    #[test]
    fn round_trip_and_recent_window() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("history.json");
        let mut history = History::default();
        let mut old = HistoryEntry::now(RunKind::Scheduled, 5000, 10, 1);
        old.time -= 40 * 86_400;
        history.record(old);
        let mut recent = HistoryEntry::now(RunKind::HealthCheck, 700, 3, 2);
        recent.areas = vec![("Temporary files".into(), 600), ("Chrome".into(), 100)];
        history.record(recent);
        history.save_to(&file).unwrap();

        let loaded = History::load_from(&file);
        assert_eq!(loaded, history);
        assert_eq!(loaded.bytes_since_days(30), 700);
        assert_eq!(loaded.bytes_since_days(60), 5700);
        assert_eq!(
            History::load_from(&tmp.path().join("missing.json")),
            History::default()
        );
        std::fs::write(&file, "garbage").unwrap();
        assert_eq!(History::load_from(&file), History::default());
    }
}
