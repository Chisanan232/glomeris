//! Detector registry (HORO-948).
//!
//! Each detector is a bounded, shallow probe of a known root for one
//! family of tool-owned resources — NOT a full filesystem walk (that's
//! [`crate::scanner`]'s job; detectors deliberately do not reuse
//! `scanner::walker`). A detector's tool being absent from the machine is
//! normal, expected state, never an error.

mod cargo;
mod docker;
mod homebrew;
mod node;
mod xcode;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::evidence::{
    Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, Regenerability,
    ResourceFingerprint, ResourceId,
};
use crate::scanner::{ScanBudget, StopReason};

/// Stable identifier for one detector, e.g. `"cargo_target_dir"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DetectorId(pub &'static str);

/// Outcome of running one detector's discovery pass.
///
/// `ToolAbsent` is normal, expected state — not every detector's tool is
/// installed on every machine. `Failed` must never be silently converted
/// to an empty/safe result by an upstream caller: a failed probe is not
/// evidence of "nothing to clean up", it is evidence of "we don't know."
/// This ticket's code only constructs the variant; enforcing that
/// distinction end-to-end is the future policy layer's job.
#[derive(Debug, PartialEq)]
pub enum DetectorStatus {
    Found(Vec<Evidence>),
    ToolAbsent,
    Failed(String),
}

/// Shared context passed to every detector's `discover` call.
pub struct DiscoveryContext {
    pub home_dir: PathBuf,
    /// Project roots to check for build/dependency directories (e.g.
    /// `<root>/target`, `<root>/node_modules`). Empty by default —
    /// detectors that need this do nothing when it's empty rather than
    /// guessing at project locations.
    pub known_project_roots: Vec<PathBuf>,
}

impl DiscoveryContext {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            known_project_roots: Vec::new(),
        }
    }

    pub fn with_known_project_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.known_project_roots = roots;
        self
    }
}

/// One detector: a bounded, shallow probe for one family of resources.
pub trait Detector: Send + Sync {
    fn id(&self) -> DetectorId;
    fn resource_kinds(&self) -> &'static [crate::evidence::ResourceKind];

    /// Discover candidates. MUST NOT panic on tool-absent or permission
    /// errors — return [`DetectorStatus::ToolAbsent`] /
    /// [`DetectorStatus::Failed`] instead.
    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus;
}

/// Maximum number of filesystem entries [`estimate_logical_bytes`] visits
/// before treating its own budget as exhausted (HORO-1016).
pub(crate) const SIZE_ESTIMATE_MAX_ENTRIES: u64 = 200_000;

/// Wall-clock deadline for one [`estimate_logical_bytes`] call (HORO-1016).
pub(crate) const SIZE_ESTIMATE_DEADLINE: Duration = Duration::from_millis(750);

/// The bounded [`ScanBudget`] every detector's size estimate uses — shared
/// by discovery-time probes and `executor::build_fresh_evidence`'s
/// revalidation, so `detect`'s reported size and `clean`'s revalidated
/// size stay consistent for an unchanged resource.
pub(crate) fn size_estimate_budget() -> ScanBudget {
    ScanBudget::default()
        .with_max_files_visited(SIZE_ESTIMATE_MAX_ENTRIES)
        .with_max_duration(SIZE_ESTIMATE_DEADLINE)
}

/// Outcome of one [`estimate_logical_bytes`] call.
#[derive(Debug)]
pub(crate) struct SizeEstimate {
    pub bytes: ProbeOutcome<u64>,
    /// Meaningful only when `bytes` is `Observed` — the root's own probe
    /// failing (`Unavailable`) never got far enough to consult a budget.
    pub stop_reason: StopReason,
    pub entries_visited: u64,
    pub unreadable_entries: u64,
}

impl SizeEstimate {
    /// A provenance note for [`Evidence::push_source`], present only when
    /// the walk stopped early (`stop_reason != Exhausted`) — i.e. `bytes`
    /// is a truthful lower bound, not the full subtree total. Never a
    /// policy input: this is advisory text only, not compared by
    /// `classify()`/`execute()`'s revalidation.
    pub fn lower_bound_note(&self) -> Option<String> {
        if !self.bytes.is_observed() {
            return None;
        }
        match self.stop_reason {
            StopReason::Exhausted => None,
            StopReason::TimeBudget => Some(format!(
                "size estimate stopped early after {} entries: {:?} time budget exceeded — \
                 reported bytes are a lower bound, not the full subtree total",
                self.entries_visited, SIZE_ESTIMATE_DEADLINE
            )),
            StopReason::FileCountBudget => Some(format!(
                "size estimate stopped early after {} entries: {} entry budget exceeded — \
                 reported bytes are a lower bound, not the full subtree total",
                self.entries_visited, SIZE_ESTIMATE_MAX_ENTRIES
            )),
        }
    }
}

/// Bounded, recursive logical-byte estimate for `path` (HORO-1016):
/// replaces the old non-recursive probe that only summed a directory's
/// immediate entries (miscounting a subdirectory's own inode size instead
/// of its contents). Walks the full subtree under `path` using an
/// explicit stack — never real recursion, so an arbitrarily deep tree
/// cannot risk a stack overflow — bounded by `budget`
/// ([`crate::scanner::ScanBudget`]'s data types only; this module
/// deliberately does not reuse `scanner::walker`'s traversal — see this
/// module's own doc comment).
///
/// Once past `path`'s own `fs::symlink_metadata`/`fs::read_dir` (the only
/// point that can yield `Unavailable`), this function always returns
/// `Observed(_)`: budget exhaustion reports a truthful partial sum as a
/// lower bound, never `Unavailable`, and a per-entry error (one unreadable
/// child) is counted in `unreadable_entries` and skipped rather than
/// failing the whole estimate. This is what keeps a truncated walk from
/// ever looking like a probe failure to `Evidence::completeness()` or to
/// `executor::execute`'s TOCTOU revalidation.
///
/// A symlink is counted by its own `lstat` size and never followed or
/// descended into, matching `executor::total_size_best_effort`'s existing
/// convention. A directory entry contributes 0 bytes itself (only its
/// contents count), also matching that convention.
pub(crate) fn estimate_logical_bytes(path: &Path, budget: ScanBudget) -> SizeEstimate {
    let root_meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return SizeEstimate {
                bytes: ProbeOutcome::Unavailable(ProbeReason::PermissionDenied),
                stop_reason: StopReason::Exhausted,
                entries_visited: 0,
                unreadable_entries: 0,
            };
        }
        Err(_) => {
            return SizeEstimate {
                bytes: ProbeOutcome::Unavailable(ProbeReason::Failed),
                stop_reason: StopReason::Exhausted,
                entries_visited: 0,
                unreadable_entries: 0,
            };
        }
    };

    if !root_meta.is_dir() {
        return SizeEstimate {
            bytes: ProbeOutcome::Observed(root_meta.len()),
            stop_reason: StopReason::Exhausted,
            entries_visited: 0,
            unreadable_entries: 0,
        };
    }

    let mut total: u64 = 0;
    let mut entries_visited: u64 = 0;
    let mut unreadable_entries: u64 = 0;
    let mut stop_reason = StopReason::Exhausted;
    let mut tracker = budget.tracker();
    // Explicit stack, never real recursion — a deep `target/`-style tree
    // must not risk a stack overflow.
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];
    // Only the FIRST `read_dir` (the root's own) yields `Unavailable` on
    // failure; every subsequent directory's `read_dir` failure (a nested
    // subdirectory that turned out to be unreadable mid-walk) instead just
    // increments `unreadable_entries` — see this function's doc comment.
    let mut is_root = true;

    'walk: while let Some(dir) = stack.pop() {
        let read_dir = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if is_root && e.kind() == std::io::ErrorKind::PermissionDenied => {
                return SizeEstimate {
                    bytes: ProbeOutcome::Unavailable(ProbeReason::PermissionDenied),
                    stop_reason: StopReason::Exhausted,
                    entries_visited: 0,
                    unreadable_entries: 0,
                };
            }
            Err(_) if is_root => {
                return SizeEstimate {
                    bytes: ProbeOutcome::Unavailable(ProbeReason::Failed),
                    stop_reason: StopReason::Exhausted,
                    entries_visited: 0,
                    unreadable_entries: 0,
                };
            }
            Err(_) => {
                unreadable_entries += 1;
                is_root = false;
                continue;
            }
        };
        is_root = false;

        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    unreadable_entries += 1;
                    continue;
                }
            };

            entries_visited += 1;
            if let Some(reason) = tracker.record_visit() {
                stop_reason = reason;
                break 'walk;
            }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    unreadable_entries += 1;
                    continue;
                }
            };

            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }

    SizeEstimate {
        bytes: ProbeOutcome::Observed(total),
        stop_reason,
        entries_visited,
        unreadable_entries,
    }
}

/// Best-effort mtime probe for `path` itself.
pub(crate) fn probe_mtime(path: &Path) -> ProbeOutcome<SystemTime> {
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => ProbeOutcome::Observed(mtime),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied)
        }
        Err(_) => ProbeOutcome::Unavailable(ProbeReason::Failed),
    }
}

/// Best-effort dev/inode fingerprint for `path`.
#[cfg(unix)]
pub(crate) fn dev_ino_fingerprint(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Build a discovery-stage [`Evidence`]: the four correlation fields
/// (`open_by_process`, `process_cwd_match`, `git_state`, `tool_liveness`)
/// are ALWAYS `Unavailable(NotAttempted)` here — HORO-948 never attempts
/// process/git/lsof correlation. HORO-949 is responsible for filling
/// those in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn discovery_evidence(
    resource: ResourceId,
    detector: DetectorId,
    path_for_fingerprint: &Path,
    logical_bytes: ProbeOutcome<u64>,
    reclaimable_bytes: ProbeOutcome<u64>,
    last_modified: ProbeOutcome<SystemTime>,
    regenerability: Regenerability,
    recoverability: Recoverability,
    native_cleanup: NativeCleanup,
) -> Evidence {
    Evidence {
        resource,
        fingerprint: ResourceFingerprint {
            dev_ino: dev_ino_fingerprint(path_for_fingerprint),
            mtime: last_modified.observed().copied(),
            tool_revision: None,
        },
        detector,
        logical_bytes,
        physical_bytes: None,
        reclaimable_bytes,
        last_modified,
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability,
        recoverability,
        native_cleanup,
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        collected_at: SystemTime::now(),
        sources: Vec::new(),
    }
}

/// Plain compile-time list of the built-in detectors — deliberately NOT a
/// plugin/inventory registration system. Five detectors don't earn that
/// complexity.
pub struct DetectorRegistry {
    detectors: Vec<Box<dyn Detector>>,
}

impl DetectorRegistry {
    /// Registers all five built-in detectors (xcode, homebrew, cargo,
    /// node, docker).
    pub fn builtin() -> Self {
        Self {
            detectors: vec![
                Box::new(xcode::XcodeDetector),
                Box::new(homebrew::HomebrewDetector),
                Box::new(cargo::CargoDetector),
                Box::new(node::NodeDetector),
                Box::new(docker::DockerDetector),
            ],
        }
    }

    /// Builds a registry from an explicit detector list. Crate-internal:
    /// production code always uses [`DetectorRegistry::builtin`]; this
    /// exists so tests elsewhere in the crate can substitute fake
    /// detectors instead of exercising the real tool-probing ones.
    ///
    /// This matters beyond mere convenience: several built-in detectors
    /// (e.g. [`homebrew::HomebrewDetector`]) shell out to a real,
    /// already-installed system tool regardless of
    /// [`DiscoveryContext::home_dir`]/`known_project_roots`, so
    /// `builtin()` can discover a genuine resource on whatever machine
    /// runs the test — the caller controls what a fake `Detector` reports
    /// instead.
    #[cfg(test)]
    pub(crate) fn from_detectors(detectors: Vec<Box<dyn Detector>>) -> Self {
        Self { detectors }
    }

    pub fn discover_all(&self, ctx: &DiscoveryContext) -> Vec<(DetectorId, DetectorStatus)> {
        self.detectors
            .iter()
            .map(|detector| (detector.id(), detector.discover(ctx)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_registers_all_five_detectors() {
        let registry = DetectorRegistry::builtin();
        assert_eq!(registry.detectors.len(), 5);
    }

    struct StubDetector(DetectorStatus);

    impl Detector for StubDetector {
        fn id(&self) -> DetectorId {
            DetectorId("stub")
        }

        fn resource_kinds(&self) -> &'static [crate::evidence::ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            match &self.0 {
                DetectorStatus::Found(evidence) => DetectorStatus::Found(evidence.clone()),
                DetectorStatus::ToolAbsent => DetectorStatus::ToolAbsent,
                DetectorStatus::Failed(msg) => DetectorStatus::Failed(msg.clone()),
            }
        }
    }

    #[test]
    fn from_detectors_uses_exactly_the_given_detectors() {
        let registry = DetectorRegistry::from_detectors(vec![Box::new(StubDetector(
            DetectorStatus::ToolAbsent,
        ))]);
        let ctx = DiscoveryContext::new("/nonexistent-home-for-test");
        let results = registry.discover_all(&ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, DetectorId("stub"));
    }

    /// `discover_all` returns exactly one status per registered detector
    /// — proven with fake detectors, deliberately never with the real
    /// wiring constructor (see the structural guard test below): running
    /// `builtin()`'s real detectors through `discover_all` would shell
    /// out to whatever `brew`/`docker` the *test machine* actually has
    /// installed, which this invariant doesn't need in order to hold.
    #[test]
    fn discover_all_returns_one_status_per_detector() {
        let registry = DetectorRegistry::from_detectors(vec![
            Box::new(StubDetector(DetectorStatus::ToolAbsent)),
            Box::new(StubDetector(DetectorStatus::Found(Vec::new()))),
            Box::new(StubDetector(DetectorStatus::Failed("boom".to_string()))),
        ]);
        let ctx = DiscoveryContext::new("/nonexistent-home-for-test");
        let results = registry.discover_all(&ctx);
        assert_eq!(results.len(), 3);
    }

    /// Structural regression guard (Jira HORO-952, HORO-994): twice during
    /// this campaign, test code that reached for the registry's real
    /// wiring constructor ended up feeding its evidence into a real
    /// `execute()` call, which actually ran `brew cleanup -s` against the
    /// developer's live Homebrew cache — because several detectors it
    /// wires up (e.g. `HomebrewDetector`) shell out to whatever tool the
    /// *host machine* actually has installed, ignoring `DiscoveryContext`
    /// entirely (see `DetectorRegistry::from_detectors`'s own doc comment
    /// above).
    ///
    /// Like `emergency::tests::module_never_references_network_or_llm_types`,
    /// this is a deliberately lightweight source-text scan, not real AST
    /// analysis: it reads every `.rs` file under `src/` and `tests/` at
    /// test time and looks for the wiring constructor's call text inside
    /// test code only (the whole file for `tests/*.rs` integration tests,
    /// everything from the first `#[cfg(test)]` marker onward for `src/`
    /// files).
    ///
    /// A small, explicit allowlist covers the tests already reviewed and
    /// known safe:
    /// - `builtin_registry_registers_all_five_detectors` directly above,
    ///   which only asserts the registered detector count and never
    ///   calls `.discover()`/`.execute()` on anything; the sibling
    ///   `discover_all_returns_one_status_per_detector` test deliberately
    ///   proves that invariant with fake detectors instead, so it does
    ///   not appear here;
    /// - `tests/golden_chain_execute.rs` and
    ///   `tests/reclaimable_bytes_reaches_auto_safe.rs`, which do use the
    ///   real registry but scope every subsequent policy/execute step to
    ///   a single detector's evidence for a disposable tempdir fixture
    ///   (see those files' own doc comments);
    /// - `tests/cli_project_root_wiring.rs` (HORO-957), which calls
    ///   `discover_all` only and never routes any evidence into
    ///   `authorize`/`execute` — its two real-registry occurrences prove
    ///   `--project-root` reaches the real cargo/node detectors and stop
    ///   there, so no real-host cleanup action can ever fire from it;
    /// - `tests/cargo_symlinked_target_manifest_round_trip.rs`
    ///   (HORO-1017), which mirrors `tests/golden_chain_execute.rs`'s own
    ///   already-reviewed shape: it scopes the real cargo detector's
    ///   evidence to a single disposable tempdir fixture (a symlinked
    ///   `target/` plus a decoy neighbor project, both under
    ///   `std::env::temp_dir()`) and only ever runs `cargo clean` against
    ///   that fixture's own real `target/` dir.
    ///
    /// Any new occurrence must be reviewed and added here explicitly —
    /// an un-reviewed new occurrence is exactly the failure mode this
    /// test exists to catch, so it fails the build instead of passing
    /// silently.
    #[test]
    fn no_unreviewed_test_code_wires_up_the_real_detector_registry() {
        // Built from two literals rather than one contiguous string so
        // this test's own source text never contains the needle it is
        // searching for — otherwise this test would fail against itself
        // the moment it is compiled.
        let needle = format!("{}{}", "DetectorRegistry::builtin", "()");

        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

        // (path relative to the manifest dir, exact expected occurrence
        // count in that file's test code)
        let allowlist: &[(&str, usize)] = &[
            ("src/detectors/mod.rs", 1),
            ("tests/golden_chain_execute.rs", 1),
            ("tests/reclaimable_bytes_reaches_auto_safe.rs", 1),
            ("tests/cli_project_root_wiring.rs", 2),
            ("tests/cargo_symlinked_target_manifest_round_trip.rs", 1),
        ];

        for (rel_path, expected_count) in allowlist {
            let full_path = manifest_dir.join(rel_path);
            let content = fs::read_to_string(&full_path)
                .unwrap_or_else(|e| panic!("failed to read allowlisted file {rel_path}: {e}"));
            let test_code = test_code_of(rel_path, &content);
            let actual_count = test_code.matches(&needle).count();
            assert_eq!(
                actual_count, *expected_count,
                "allowlisted file {rel_path} now has {actual_count} occurrence(s) \
                 of the real DetectorRegistry wiring constructor in test code, \
                 expected exactly {expected_count} — if a new one was added \
                 deliberately, review it for real-host-execution risk (does its \
                 evidence ever reach a real `execute()` call outside a disposable \
                 tempdir fixture?) and only then update this allowlist"
            );
        }

        let mut offenders: Vec<String> = Vec::new();
        for top in ["src", "tests"] {
            scan_rs_files(&manifest_dir.join(top), &mut |path, content| {
                let rel = path
                    .strip_prefix(manifest_dir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if allowlist.iter().any(|(p, _)| *p == rel) {
                    return;
                }
                let test_code = test_code_of(&rel, content);
                if test_code.contains(&needle) {
                    offenders.push(rel);
                }
            });
        }

        assert!(
            offenders.is_empty(),
            "found un-reviewed test code wiring up the real DetectorRegistry in: \
             {offenders:?} — this wires up detectors that shell out to real host \
             tools (brew, cargo, docker, ...); use \
             DetectorRegistry::from_detectors(vec![]) or fake Detector \
             implementations instead (see HORO-952, HORO-994)"
        );
    }

    /// Returns the portion of `content` that is actually test code: for
    /// an integration test file under `tests/`, the whole file (every
    /// integration test file is test-only by construction); for a `src/`
    /// file, everything from the first `#[cfg(test)]` marker onward,
    /// mirroring `emergency::tests::module_never_references_network_or_llm_types`'s
    /// own production/test split convention (that test keeps the part
    /// before the marker; this one wants the part after).
    fn test_code_of(rel_path: &str, content: &str) -> String {
        let raw = if rel_path.starts_with("tests/") {
            content
        } else {
            match content.split_once("#[cfg(test)]") {
                Some((_, test_part)) => test_part,
                None => "",
            }
        };
        // Drop comment lines (`//`, `///`, `//!`) before scanning: this
        // guard cares about an actual call in code, not a doc comment
        // that merely mentions the wiring constructor's name in prose
        // (as several tests in this crate already do, to explain why
        // they deliberately avoid it).
        raw.lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Recursively visits every `.rs` file under `dir`, calling `visit`
    /// with its path and file content. Best-effort: an unreadable
    /// directory or file is silently skipped rather than failing the
    /// test — this guard's job is to catch real occurrences of the
    /// needle, not to assert every file in the tree is readable.
    fn scan_rs_files(dir: &Path, visit: &mut dyn FnMut(&Path, &str)) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_rs_files(&path, visit);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                if let Ok(content) = fs::read_to_string(&path) {
                    visit(&path, &content);
                }
            }
        }
    }
}

/// Regression tests for [`estimate_logical_bytes`] (HORO-1016) — the
/// bounded recursive replacement for the old non-recursive
/// `shallow_logical_bytes`, which only summed a directory's immediate
/// entries and miscounted a subdirectory as its own inode size instead of
/// its contents.
#[cfg(test)]
mod size_estimate_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-size-estimate-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn nested_tree_total_is_exact_sum_of_file_lengths() {
        let root = make_temp_dir("nested-exact");
        fs::write(root.join("top.bin"), vec![0u8; 1000]).unwrap();
        fs::create_dir_all(root.join("mid")).unwrap();
        fs::write(root.join("mid/mid.bin"), vec![0u8; 2000]).unwrap();
        fs::create_dir_all(root.join("mid/leaf")).unwrap();
        fs::write(root.join("mid/leaf/leaf.bin"), vec![0u8; 3000]).unwrap();

        let estimate = estimate_logical_bytes(&root, ScanBudget::unlimited());

        assert_eq!(estimate.bytes, ProbeOutcome::Observed(6000));
        assert_eq!(estimate.stop_reason, StopReason::Exhausted);
        assert!(estimate.lower_bound_note().is_none());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn deep_tree_is_counted_at_every_depth() {
        let root = make_temp_dir("deep-chain");
        let mut cursor = root.clone();
        for i in 0..6 {
            cursor = cursor.join(format!("level{i}"));
            fs::create_dir_all(&cursor).unwrap();
        }
        fs::write(cursor.join("leaf.bin"), vec![0u8; 4242]).unwrap();

        let estimate = estimate_logical_bytes(&root, ScanBudget::unlimited());

        assert_eq!(estimate.bytes, ProbeOutcome::Observed(4242));
        assert_eq!(estimate.stop_reason, StopReason::Exhausted);

        fs::remove_dir_all(&root).ok();
    }

    /// The single most important safety test: budget exhaustion must
    /// always yield a truthful `Observed` lower bound, never
    /// `Unavailable` — that's what keeps a truncated walk from ever
    /// looking like a probe failure to `Evidence::completeness()` or
    /// `executor::execute`'s TOCTOU revalidation.
    #[test]
    fn budget_exhaustion_returns_observed_lower_bound_never_unavailable() {
        let root = make_temp_dir("budget-exhaustion");
        let mut true_total: u64 = 0;
        for i in 0..10 {
            let sub = root.join(format!("dir{i}"));
            fs::create_dir_all(&sub).unwrap();
            for j in 0..5 {
                let bytes = vec![0u8; 100 + i + j];
                fs::write(sub.join(format!("file{j}.bin")), &bytes).unwrap();
                true_total += bytes.len() as u64;
            }
        }

        let file_count_budget = ScanBudget::default().with_max_files_visited(3);
        let estimate = estimate_logical_bytes(&root, file_count_budget);
        match estimate.bytes {
            ProbeOutcome::Observed(bytes) => {
                assert!(
                    bytes < true_total,
                    "expected a truncated lower bound, got the full total {bytes}"
                );
            }
            other => panic!("expected Observed(_), got {other:?}"),
        }
        assert_eq!(estimate.stop_reason, StopReason::FileCountBudget);
        assert!(estimate.lower_bound_note().is_some());

        let time_budget = ScanBudget::default().with_max_duration(Duration::ZERO);
        let estimate = estimate_logical_bytes(&root, time_budget);
        assert!(estimate.bytes.is_observed());
        assert_eq!(estimate.stop_reason, StopReason::TimeBudget);
        assert!(estimate.lower_bound_note().is_some());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unreadable_subdirectory_does_not_change_the_outcome_variant() {
        let root = make_temp_dir("unreadable-subdir");
        let denied = root.join("denied");
        fs::create_dir_all(&denied).unwrap();
        fs::write(denied.join("inside.bin"), vec![0u8; 4096]).unwrap();
        fs::write(root.join("visible.bin"), vec![0u8; 64]).unwrap();

        let mut perms = fs::metadata(&denied).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&denied, perms).unwrap();

        let estimate = estimate_logical_bytes(&root, ScanBudget::unlimited());

        // Restore permissions so the temp dir can be cleaned up.
        let mut restore = fs::metadata(&denied).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&denied, restore).ok();

        match estimate.bytes {
            ProbeOutcome::Observed(bytes) => {
                if estimate.unreadable_entries == 0 {
                    eprintln!(
                        "skipping strict assertion: permission check bypassed (running as root?)"
                    );
                } else {
                    assert!(bytes >= 64, "expected at least the visible file's bytes");
                    assert!(estimate.unreadable_entries > 0);
                }
            }
            other => panic!("expected Observed(_), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn permission_denied_root_is_unavailable_not_zero() {
        let root = make_temp_dir("permission-denied-root");
        fs::write(root.join("inside.bin"), vec![0u8; 4096]).unwrap();

        let mut perms = fs::metadata(&root).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&root, perms).unwrap();

        let estimate = estimate_logical_bytes(&root, ScanBudget::unlimited());

        // Restore permissions so the temp dir can be cleaned up.
        let mut restore = fs::metadata(&root).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&root, restore).ok();

        match estimate.bytes {
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied) => {}
            ProbeOutcome::Observed(_) => {
                eprintln!("skipping assertion: permission check bypassed (running as root?)");
            }
            other => panic!("expected Unavailable(PermissionDenied), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn symlinks_are_counted_by_their_own_size_and_never_followed() {
        let root = make_temp_dir("symlink-not-followed");
        let outside_target = std::env::temp_dir().join(format!(
            "glomeris-size-estimate-symlink-target-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&outside_target, vec![0u8; 1 << 20]).unwrap();
        symlink(&outside_target, root.join("link_to_big_file")).unwrap();

        let estimate = estimate_logical_bytes(&root, ScanBudget::unlimited());

        match estimate.bytes {
            ProbeOutcome::Observed(bytes) => {
                assert!(
                    bytes < 1024,
                    "expected the symlink's own small lstat size, got {bytes} (target followed?)"
                );
            }
            other => panic!("expected Observed(_), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
        fs::remove_file(&outside_target).ok();
    }

    #[test]
    fn sparse_file_is_counted_by_logical_length() {
        let root = make_temp_dir("sparse-file");
        let sparse_path = root.join("sparse.bin");
        let file = fs::File::create(&sparse_path).unwrap();
        file.set_len(1 << 20).unwrap();
        drop(file);

        let estimate = estimate_logical_bytes(&root, ScanBudget::unlimited());

        assert_eq!(estimate.bytes, ProbeOutcome::Observed(1 << 20));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn non_directory_root_is_observed_file_length() {
        let root = make_temp_dir("non-directory-root");
        let file_path = root.join("plain.bin");
        fs::write(&file_path, vec![0u8; 777]).unwrap();

        let estimate = estimate_logical_bytes(&file_path, ScanBudget::unlimited());

        assert_eq!(estimate.bytes, ProbeOutcome::Observed(777));
        assert_eq!(estimate.stop_reason, StopReason::Exhausted);

        fs::remove_dir_all(&root).ok();
    }
}
