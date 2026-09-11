//! Best-effort persistence seam.
//!
//! Full structured local state is a later ticket's concern (HORO-943 notes
//! SQLite for bounded local state, architecture-wide). For this ticket,
//! persistence exists only to record pressure transitions for later
//! inspection, and it is explicitly **failure-tolerant**: any error here
//! must never stop the monitor loop from continuing to observe and notify.
//! No feature in this crate may depend on persistence succeeding.

use crate::monitor::pressure::PressureState;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One recorded pressure transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PressureEvent {
    pub unix_time_secs: u64,
    pub from: PressureState,
    pub to: PressureState,
    pub used_percent: f64,
    pub free_bytes: u64,
}

/// Records pressure history. Implementations should treat every error as
/// non-fatal to the caller — callers are expected to log and continue, not
/// propagate persistence failure as monitor failure.
pub trait PersistenceBackend: Send + Sync {
    fn record(&self, event: &PressureEvent) -> io::Result<()>;
}

/// Appends one line per event to a plain text file. Minimal, dependency-free
/// stub — sufficient for this ticket's "best-effort history" scope; a
/// structured/queryable store is out of scope here (see HORO-943).
pub struct FilePersistence {
    path: PathBuf,
}

impl FilePersistence {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl PersistenceBackend for FilePersistence {
    fn record(&self, event: &PressureEvent) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(
            file,
            "{}\t{}\t{}\t{:.2}\t{}",
            event.unix_time_secs, event.from, event.to, event.used_percent, event.free_bytes
        )
    }
}

/// Current unix time in seconds, saturating to 0 if the clock is somehow
/// before the epoch (never expected, but never worth panicking over).
pub fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A backend that always fails, used by the polling-loop tests to prove
/// persistence failure never stops monitoring.
#[cfg(test)]
pub struct AlwaysFailingPersistence;

#[cfg(test)]
impl PersistenceBackend for AlwaysFailingPersistence {
    fn record(&self, _event: &PressureEvent) -> io::Result<()> {
        Err(io::Error::other("simulated persistence failure"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_persistence_appends_a_line_per_event() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-persistence-test-{}-{}",
            std::process::id(),
            unix_now_secs()
        ));
        let path = dir.join("history.tsv");
        let backend = FilePersistence::new(&path);

        let event = PressureEvent {
            unix_time_secs: 1_700_000_000,
            from: PressureState::Healthy,
            to: PressureState::Warn,
            used_percent: 76.5,
            free_bytes: 40_000_000_000,
        };
        backend.record(&event).expect("record should succeed");
        backend.record(&event).expect("record should succeed");

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.contains("HEALTHY"));
        assert!(contents.contains("WARN"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn always_failing_backend_returns_error_without_panicking() {
        let backend = AlwaysFailingPersistence;
        let event = PressureEvent {
            unix_time_secs: 0,
            from: PressureState::Healthy,
            to: PressureState::Warn,
            used_percent: 0.0,
            free_bytes: 0,
        };
        assert!(backend.record(&event).is_err());
    }
}
