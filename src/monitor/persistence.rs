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

/// One recorded real-execution attempt (HORO-1057) — the audit trail
/// `history.tsv` never provided (that file records pressure transitions
/// only, never what was actually executed, by what path, with what
/// outcome). Appended, one JSON object per line, to `actions.jsonl` by
/// every real-execution call site (`glomeris execute`, `glomeris free`,
/// `glomeris emergency`) — never by `glomeris clean --dry-run` or
/// `dry_run()`, which mutate nothing.
///
/// JSON Lines rather than TSV (unlike `history.tsv`): `resource_id` is a
/// filesystem path, which can itself contain a literal tab byte, and
/// `serde_json` is already a dependency — see this ticket's rationale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub timestamp: u64,
    pub action_id: String,
    pub resource_id: String,
    /// `crate::reporting::policy_label::PolicyLabel::as_str()`'s output
    /// (`"AUTO_SAFE"`/`"ASK"`/`"PROTECTED"`/`"UNKNOWN_INCOMPLETE"`) at the
    /// moment this action was authorized — not re-derived from `outcome`.
    pub policy_label: String,
    /// One of `"succeeded"`, `"failed"`, `"aborted_by_revalidation"` —
    /// mirrors `crate::executor::ExecutionOutcome`'s variants without
    /// deriving `Serialize` on that type directly, same reasoning as
    /// `crate::reporting::dto::ExecuteReport::outcome`. Never `"dry_run"`:
    /// no real-execution call site ever records a dry run here.
    pub outcome: String,
    /// Populated only when `outcome == "aborted_by_revalidation"`.
    pub abort_reason: Option<String>,
    pub actual_reclaimed_bytes: Option<u64>,
    /// Which real-execution path produced this record, and — for
    /// `autopilot` — under what authority: `"execute"`, `"free"`,
    /// `"emergency"`, `"autopilot_auto_safe"`, or
    /// `"autopilot_preauthorized_ask"`. The two `autopilot_*` values are
    /// distinct on purpose (HORO-1310): "Autopilot did this" and "Autopilot
    /// did this to an `ASK` resource under a pre-authorization the user
    /// granted in advance" are different facts, and an audit trail that
    /// collapsed them would lose the one a user would actually go looking
    /// for.
    pub source: String,
    /// Where the model's plan ranked this resource, or `None` if no model
    /// named it (HORO-1310). 1-based, so `1` is the model's first choice and
    /// `0` never appears — the number here is the same one
    /// `glomeris autopilot run` printed, and a log offset by one from the
    /// report it came from would be worse than no log at all.
    ///
    /// This is the record of what the model *suggested*, kept deliberately
    /// separate from `policy_label` (what policy *decided*) and `source`
    /// (what *authorized* it) so a reader can see all three independently —
    /// a model suggestion that policy then refused leaves no record here at
    /// all, because nothing was executed.
    ///
    /// `None` conflates "there was no model" with "the model did not name
    /// this resource". Both read correctly as "not model-suggested", which
    /// is the only question this field exists to answer.
    ///
    /// `#[serde(default)]` so the `actions.jsonl` lines written before this
    /// field existed still parse — [`read_audit_tail`] silently drops a line
    /// it cannot deserialize, and a schema addition must not quietly erase a
    /// user's existing audit history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_rank: Option<u32>,
}

/// The single producer of every [`AuditRecord::source`] value (HORO-1312).
///
/// These five strings used to be written as literals at four separate call
/// sites — `src/cli/mod.rs`, `src/executor/recovery_loop.rs`,
/// `src/emergency/mod.rs`, `src/autopilot/run.rs`. That had a specific
/// consequence rather than a stylistic one:
/// `scripts/check-vocabulary-covers-cli-tokens.sh` compares each CLI token set
/// against the menu-bar app's wording for it, and can only do so where one
/// `as_str`-style match is the sole producer. `source` had none, so it was one
/// of three vocabularies the guard had to skip — and HORO-1310 then added
/// `autopilot_auto_safe` and `autopilot_preauthorized_ask` without the GUI
/// learning words for them, exactly the drift the guard exists to catch.
/// Nothing failed; the history panel would simply have called Autopilot's own
/// rows an unrecognised trigger.
///
/// [`AuditRecord::source`] stays a `String` rather than becoming this type.
/// The field is read back by `glomeris actions history` from a file an
/// arbitrarily newer build may have written, and
/// [`read_audit_tail`] drops any line it cannot deserialize — so a typed field
/// would make a future version's new source value silently erase history a
/// user can still read today. Writing is where the closed set belongs;
/// reading has to stay tolerant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionSource {
    /// `glomeris execute` — one resource the user named.
    Execute,
    /// `glomeris free`'s bounded recovery loop.
    Free,
    /// `glomeris emergency`'s degraded path.
    Emergency,
    /// `glomeris autopilot run`, on a resource policy classified `AUTO_SAFE`
    /// on its own.
    AutopilotAutoSafe,
    /// `glomeris autopilot run`, on a resource policy would have asked about,
    /// under a pre-authorization granted in advance for that exact kind and
    /// reason. Distinct from [`ActionSource::AutopilotAutoSafe`] because
    /// "Autopilot did this" and "Autopilot did this under a standing consent"
    /// are the two different facts someone auditing the log is looking for.
    AutopilotPreauthorizedAsk,
}

impl ActionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionSource::Execute => "execute",
            ActionSource::Free => "free",
            ActionSource::Emergency => "emergency",
            ActionSource::AutopilotAutoSafe => "autopilot_auto_safe",
            ActionSource::AutopilotPreauthorizedAsk => "autopilot_preauthorized_ask",
        }
    }
}

impl std::fmt::Display for ActionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Safety-valve size cap for `actions.jsonl` — this is a rotation trigger,
/// not a log-management system (see this ticket's scope notes). 10 MiB is
/// generous for a one-line-per-action JSONL file; ordinary use would take
/// years to reach it.
const MAX_AUDIT_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Appends one [`AuditRecord`] to `path` as a single JSON line, rotating
/// `path` to `path` + `.1` first if it has grown past
/// [`MAX_AUDIT_LOG_BYTES`] (overwriting any previous `.1` — at most one
/// rotated backup is kept; see this ticket's scope notes for why this is
/// intentionally not a full log-management scheme).
///
/// Best-effort by contract, matching [`PersistenceBackend::record`]'s
/// failure philosophy exactly (see this module's doc comment): every
/// caller in this crate is required to ignore this function's `Result`
/// rather than propagate it — an audit-write failure must never change a
/// real execution's own outcome or exit code. This function itself never
/// panics.
pub fn append_audit_record(path: &Path, record: &AuditRecord) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_if_oversized(path)?;

    let mut line = serde_json::to_string(record).map_err(io::Error::from)?;
    line.push('\n');

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
}

/// Renames `path` to `path` + `.1` when it exceeds [`MAX_AUDIT_LOG_BYTES`],
/// replacing any previous `.1` file. A missing `path` is not an error —
/// there is nothing to rotate yet.
fn rotate_if_oversized(path: &Path) -> io::Result<()> {
    let size = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if size <= MAX_AUDIT_LOG_BYTES {
        return Ok(());
    }
    let mut rotated = path.as_os_str().to_os_string();
    rotated.push(".1");
    std::fs::rename(path, PathBuf::from(rotated))
}

/// Reads up to `limit` of the most recent [`AuditRecord`]s from `path` (an
/// `actions.jsonl` written by [`append_audit_record`]), oldest-first —
/// same ordering, missing-file-is-empty, and malformed-line-skip contract
/// as [`read_history_tail`] (see that function's doc comment); this reads
/// only `path` itself, never a rotated `.1` backup, matching this
/// ticket's "simplest reasonable scheme, not a log-management system"
/// scope decision.
pub fn read_audit_tail(path: &Path, limit: usize) -> Vec<AuditRecord> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut tail: std::collections::VecDeque<AuditRecord> =
        std::collections::VecDeque::with_capacity(limit.min(1024));
    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<AuditRecord>(line) else {
            continue;
        };
        if limit == 0 {
            continue;
        }
        if tail.len() == limit {
            tail.pop_front();
        }
        tail.push_back(record);
    }
    tail.into_iter().collect()
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

    /// HORO-1312. The values are a wire vocabulary: `glomeris history`
    /// prints them, the menu-bar app looks them up in
    /// `GlomerisVocabulary.actionSource`, and
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` diffs this match
    /// against that lookup. Renaming one is a breaking change to already
    /// written audit files, so pin the strings here rather than letting a
    /// refactor quietly rewrite them.
    #[test]
    fn action_source_strings_are_the_ones_the_gui_and_history_expect() {
        assert_eq!(ActionSource::Execute.as_str(), "execute");
        assert_eq!(ActionSource::Free.as_str(), "free");
        assert_eq!(ActionSource::Emergency.as_str(), "emergency");
        assert_eq!(
            ActionSource::AutopilotAutoSafe.as_str(),
            "autopilot_auto_safe"
        );
        assert_eq!(
            ActionSource::AutopilotPreauthorizedAsk.as_str(),
            "autopilot_preauthorized_ask"
        );
    }

    /// The two Autopilot sources are deliberately distinct: one records
    /// work policy allowed on its own, the other records work that only
    /// ran because the operator pre-authorized an `ASK` kind by name. An
    /// audit trail that collapsed them would lose the answer to "who
    /// permitted this".
    #[test]
    fn action_source_strings_are_distinct_and_non_empty() {
        let all = [
            ActionSource::Execute,
            ActionSource::Free,
            ActionSource::Emergency,
            ActionSource::AutopilotAutoSafe,
            ActionSource::AutopilotPreauthorizedAsk,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for source in all {
            let token = source.as_str();
            assert!(!token.is_empty(), "{source:?} produced an empty token");
            assert!(
                seen.insert(token),
                "{source:?} duplicates an existing token: {token}"
            );
            assert_eq!(
                source.to_string(),
                token,
                "Display must agree with as_str, or the two writing paths diverge"
            );
        }
        assert_eq!(seen.len(), 5);
    }

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

    fn unique_audit_test_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glomeris-audit-persistence-test-{tag}-{}-{}.jsonl",
            std::process::id(),
            unix_now_secs()
        ))
    }

    fn sample_audit_record(action_id: &str) -> AuditRecord {
        AuditRecord {
            timestamp: 1_700_000_000,
            action_id: action_id.to_string(),
            resource_id: "/tmp/some/target".to_string(),
            policy_label: "AUTO_SAFE".to_string(),
            outcome: "succeeded".to_string(),
            abort_reason: None,
            actual_reclaimed_bytes: Some(1_024),
            source: "execute".to_string(),
            model_rank: None,
        }
    }

    #[test]
    fn append_and_read_audit_records_round_trip_in_order() {
        let path = unique_audit_test_path("round-trip");

        append_audit_record(&path, &sample_audit_record("action.one"))
            .expect("append should succeed");
        append_audit_record(&path, &sample_audit_record("action.two"))
            .expect("append should succeed");

        let tail = read_audit_tail(&path, 10);

        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].action_id, "action.one");
        assert_eq!(tail[1].action_id, "action.two");
        assert_eq!(tail[0], sample_audit_record("action.one"));

        let _ = std::fs::remove_file(&path);
    }

    /// A schema addition must not erase existing audit history:
    /// `read_audit_tail` silently drops any line it cannot deserialize, so a
    /// `model_rank`-less line written by an earlier build has to keep
    /// parsing (HORO-1310).
    #[test]
    fn an_audit_line_written_before_model_rank_existed_still_parses() {
        let path = unique_audit_test_path("pre-model-rank");
        std::fs::write(
            &path,
            "{\"timestamp\":1700000000,\"action_id\":\"cargo.clean.target_dir\",\
             \"resource_id\":\"/tmp/some/target\",\"policy_label\":\"AUTO_SAFE\",\
             \"outcome\":\"succeeded\",\"abort_reason\":null,\
             \"actual_reclaimed_bytes\":1024,\"source\":\"execute\"}\n",
        )
        .expect("write");

        let tail = read_audit_tail(&path, 10);

        assert_eq!(tail.len(), 1, "an older line must not be dropped");
        assert_eq!(tail[0].model_rank, None);
        assert_eq!(tail[0].action_id, "cargo.clean.target_dir");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_model_suggested_action_records_its_rank() {
        let path = unique_audit_test_path("model-rank");
        let mut record = sample_audit_record("cargo.clean.target_dir");
        record.source = "autopilot_auto_safe".to_string();
        record.model_rank = Some(1);

        append_audit_record(&path, &record).expect("append");
        let tail = read_audit_tail(&path, 10);

        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0], record);
        assert_eq!(tail[0].model_rank, Some(1));

        let _ = std::fs::remove_file(&path);
    }

    /// `skip_serializing_if` keeps the common case (nothing model-suggested)
    /// exactly as it was on disk, so a user's audit log does not grow a
    /// `"model_rank":null` on every line.
    #[test]
    fn a_rule_ranked_action_writes_no_model_rank_key_at_all() {
        let path = unique_audit_test_path("no-model-rank-key");
        append_audit_record(&path, &sample_audit_record("cargo.clean.target_dir")).expect("append");

        let written = std::fs::read_to_string(&path).expect("read");

        assert!(
            !written.contains("model_rank"),
            "expected no model_rank key in {written:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_audit_tail_returns_empty_for_missing_file() {
        let path = unique_audit_test_path("missing-file");
        assert!(!path.exists());

        assert!(read_audit_tail(&path, 10).is_empty());
    }

    #[test]
    fn read_audit_tail_skips_malformed_lines() {
        let path = unique_audit_test_path("malformed-line");
        let good = serde_json::to_string(&sample_audit_record("action.good")).unwrap();
        std::fs::write(
            &path,
            format!("{good}\nthis is not valid JSON at all\n{good}\n"),
        )
        .unwrap();

        let tail = read_audit_tail(&path, 10);

        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].action_id, "action.good");
        assert_eq!(tail[1].action_id, "action.good");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_audit_tail_returns_at_most_limit_most_recent_records() {
        let path = unique_audit_test_path("bounded-limit");
        for i in 0..5 {
            append_audit_record(&path, &sample_audit_record(&format!("action.{i}")))
                .expect("append should succeed");
        }

        let tail = read_audit_tail(&path, 2);

        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].action_id, "action.3");
        assert_eq!(tail[1].action_id, "action.4");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_audit_record_rotates_oversized_file_to_dot_one() {
        let path = unique_audit_test_path("rotation");
        // Pre-seed a file already over the size cap so the very next
        // append triggers rotation, without actually writing 10 MiB of
        // real records.
        std::fs::write(&path, vec![b'x'; (MAX_AUDIT_LOG_BYTES + 1) as usize]).unwrap();
        let rotated_path = {
            let mut s = path.as_os_str().to_os_string();
            s.push(".1");
            PathBuf::from(s)
        };
        let _ = std::fs::remove_file(&rotated_path);

        append_audit_record(&path, &sample_audit_record("action.after-rotation"))
            .expect("append should succeed");

        assert!(
            rotated_path.exists(),
            "expected the oversized file to be rotated to .1"
        );
        let tail = read_audit_tail(&path, 10);
        assert_eq!(
            tail,
            vec![sample_audit_record("action.after-rotation")],
            "expected the fresh file to contain only the post-rotation record"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&rotated_path);
    }
}
