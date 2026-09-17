//! Best-effort persistence seam.
//!
//! Full structured local state is a later ticket's concern (HORO-943 notes
//! SQLite for bounded local state, architecture-wide). For this ticket,
//! persistence exists only to record pressure transitions for later
//! inspection, and it is explicitly **failure-tolerant**: any error here
//! must never stop the monitor loop from continuing to observe and notify.
//! No feature in this crate may depend on persistence succeeding.

use crate::monitor::pressure::PressureState;
use serde::{Deserialize, Serialize};
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

/// A snapshot of the most recently completed poll cycle, written best-effort
/// so a future `daemon status --json` (HORO-1045) can tell "installed and
/// loaded" apart from "actually polling". `state` is the pressure state's
/// stable string tag (`PressureState::as_str()`), not a derived
/// serialization of the enum itself — same reasoning as
/// `crate::reporting::dto`: never `#[derive(Serialize)]` a domain type
/// directly, so adding a variant upstream can't silently change this file's
/// shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub last_poll_unix_secs: u64,
    pub state: String,
    pub used_percent: f64,
    pub free_bytes: u64,
}

impl Heartbeat {
    pub fn new(
        last_poll_unix_secs: u64,
        state: PressureState,
        used_percent: f64,
        free_bytes: u64,
    ) -> Self {
        Self {
            last_poll_unix_secs,
            state: state.as_str().to_string(),
            used_percent,
            free_bytes,
        }
    }
}

/// Writes `heartbeat` to `path` as JSON, creating parent directories as
/// needed. Best-effort by contract: callers must treat any error here as
/// non-fatal to the poll loop, matching `PersistenceBackend::record`'s
/// failure philosophy — see this module's doc comment.
///
/// Writes to a sibling temp file, then `fs::rename`s it over `path` — same
/// atomic-write shape as `platform::macos::launchd::write_atomic` — so a
/// concurrent `read_heartbeat` (a separate process, e.g. `daemon status
/// --json`) can never observe a truncated or partially-written file. A
/// plain truncate-then-write would let a reader briefly see an empty file,
/// which `read_heartbeat` degrades to `None` — the exact signal that means
/// "not polling", which would be a false report for an otherwise-healthy
/// daemon.
pub fn write_heartbeat(path: &Path, heartbeat: &Heartbeat) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let write_result = (|| -> io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        serde_json::to_writer(file, heartbeat).map_err(io::Error::from)
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Reads back the heartbeat previously written by `write_heartbeat`.
/// Returns `None` on any error (missing file, unreadable, malformed JSON)
/// rather than propagating — a later ticket (HORO-1045) reads this
/// advisory file, and a bad/missing heartbeat must degrade to "no
/// heartbeat available", never an error the CLI has to handle specially.
pub fn read_heartbeat(path: &Path) -> Option<Heartbeat> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// One parsed line of `history.tsv`, as returned to a reader (e.g. `glomeris
/// history --json`, HORO-1046). `from`/`to` are kept as the raw string tags
/// written by [`FilePersistence::record`] (`PressureState::as_str()`'s
/// output), not re-parsed back into [`PressureState`] — same reasoning as
/// [`Heartbeat::state`]: a reader has no need to reconstruct the enum, and
/// keeping it a string means an unrecognized/future tag still round-trips
/// instead of forcing the line to be treated as malformed.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub unix_time_secs: u64,
    pub from: String,
    pub to: String,
    pub used_percent: f64,
    pub free_bytes: u64,
}

/// Reads up to `limit` of the most recent entries from `path` (a
/// `history.tsv` written by [`FilePersistence::record`]), oldest-first —
/// same ordering `record` appends in. A missing file is not an error: it
/// means "no history recorded yet", so this returns an empty `Vec` rather
/// than propagating `io::Error::NotFound`. A malformed line (wrong column
/// count, or a column that fails to parse as its expected type) is skipped
/// rather than treated as fatal — one corrupted line must never hide every
/// other valid entry.
///
/// Bounded by keeping only the last `limit` valid entries seen while
/// scanning the file forward, rather than collecting every entry first —
/// `history.tsv` grows unboundedly over the life of the daemon (see this
/// module's doc comment: SQLite/rotation is a later ticket's concern), so a
/// `--limit` reader must not hold the whole file in memory to answer "give
/// me the last 20".
pub fn read_history_tail(path: &Path, limit: usize) -> Vec<HistoryEntry> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut tail: std::collections::VecDeque<HistoryEntry> =
        std::collections::VecDeque::with_capacity(limit.min(1024));
    for line in contents.lines() {
        let Some(entry) = parse_history_line(line) else {
            continue;
        };
        if limit == 0 {
            continue;
        }
        if tail.len() == limit {
            tail.pop_front();
        }
        tail.push_back(entry);
    }
    tail.into_iter().collect()
}

/// Parses one tab-separated `history.tsv` line into a [`HistoryEntry`].
/// Returns `None` for anything that doesn't match the exact shape
/// [`FilePersistence::record`] writes — wrong field count, or any field
/// that fails to parse as its expected numeric type — so a caller can skip
/// it rather than fail the whole read.
fn parse_history_line(line: &str) -> Option<HistoryEntry> {
    let mut fields = line.split('\t');
    let unix_time_secs = fields.next()?.parse().ok()?;
    let from = fields.next()?.to_string();
    let to = fields.next()?.to_string();
    let used_percent = fields.next()?.parse().ok()?;
    let free_bytes = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(HistoryEntry {
        unix_time_secs,
        from,
        to,
        used_percent,
        free_bytes,
    })
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

    fn unique_heartbeat_test_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-heartbeat-persistence-test-{tag}-{}-{}",
            std::process::id(),
            unix_now_secs()
        ))
    }

    #[test]
    fn heartbeat_round_trips_through_write_and_read() {
        let path = unique_heartbeat_test_path("round-trip");
        let heartbeat = Heartbeat::new(1_700_000_123, PressureState::Warn, 76.5, 40_000_000_000);

        write_heartbeat(&path, &heartbeat).expect("write_heartbeat should succeed");
        let read_back = read_heartbeat(&path).expect("heartbeat should be readable after write");

        assert_eq!(read_back, heartbeat);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_heartbeat_returns_none_for_malformed_contents() {
        let path = unique_heartbeat_test_path("malformed");
        std::fs::write(&path, b"not valid json").unwrap();

        assert!(read_heartbeat(&path).is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_heartbeat_returns_none_for_missing_file() {
        let path = unique_heartbeat_test_path("missing");
        assert!(!path.exists());

        assert!(read_heartbeat(&path).is_none());
    }

    fn unique_history_test_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-history-persistence-test-{tag}-{}-{}",
            std::process::id(),
            unix_now_secs()
        ))
    }

    #[test]
    fn read_history_tail_returns_at_most_limit_most_recent_entries() {
        let path = unique_history_test_path("bounded-limit");
        let backend = FilePersistence::new(&path);
        for i in 0..5u64 {
            let event = PressureEvent {
                unix_time_secs: 1_700_000_000 + i,
                from: PressureState::Healthy,
                to: PressureState::Warn,
                used_percent: 50.0 + i as f64,
                free_bytes: 1_000 - i,
            };
            backend.record(&event).expect("record should succeed");
        }

        let tail = read_history_tail(&path, 2);

        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].unix_time_secs, 1_700_000_003);
        assert_eq!(tail[1].unix_time_secs, 1_700_000_004);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_history_tail_skips_malformed_lines() {
        let path = unique_history_test_path("malformed-line");
        std::fs::write(
            &path,
            "1700000000\tHEALTHY\tWARN\t50.00\t1000\n\
             this is not a valid line at all\n\
             1700000001\tWARN\tPRESSURED\tnot-a-number\t900\n\
             1700000002\tWARN\tPRESSURED\t60.00\t900\n",
        )
        .unwrap();

        let tail = read_history_tail(&path, 10);

        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].unix_time_secs, 1_700_000_000);
        assert_eq!(tail[1].unix_time_secs, 1_700_000_002);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_history_tail_returns_empty_for_missing_file() {
        let path = unique_history_test_path("missing-file");
        assert!(!path.exists());

        let tail = read_history_tail(&path, 10);

        assert!(tail.is_empty());
    }

    #[test]
    fn write_heartbeat_leaves_no_temp_file_on_success() {
        let path = unique_heartbeat_test_path("no-tmp-leftover");
        let heartbeat = Heartbeat::new(1_700_000_000, PressureState::Healthy, 10.0, 1_000_000);

        write_heartbeat(&path, &heartbeat).expect("write_heartbeat should succeed");

        assert!(!path.with_extension("json.tmp").exists());

        let _ = std::fs::remove_file(&path);
    }
}
