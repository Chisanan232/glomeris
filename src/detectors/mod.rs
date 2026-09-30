//! Detector registry (HORO-948).
//!
//! Each detector is a bounded probe of a known root for one family of
//! tool-owned resources — NOT a full filesystem walk (that's
//! [`crate::scanner`]'s job; detectors deliberately do not reuse
//! `scanner::walker`). A detector's per-resource size estimate does recurse
//! within that known root, bounded by a fixed entry/time budget
//! (HORO-1016; see `estimate_logical_bytes`). A detector's tool being
//! absent from the machine is normal, expected state, never an error.

mod cargo;
mod docker;
mod docker_objects;
mod go;
mod gradle;
mod homebrew;
mod maven;
mod node;
mod python;
mod swiftpm;
mod xcode;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::evidence::{
    Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, Regenerability,
    ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
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
///
/// `ToolNotRunning` is HORO-1544's addition, for the client/daemon tools
/// where "installed" and "answering" are different questions. Docker is the
/// case that forced it: `docker` on `PATH` with no daemon listening is
/// neither of the other two — reporting `ToolAbsent` tells a developer their
/// 21 GB of images are not there, and reporting `Failed` tells them
/// something broke when nothing did. It is a third fact and it needs a third
/// word.
#[derive(Debug, PartialEq)]
pub enum DetectorStatus {
    Found(Vec<Evidence>),
    ToolAbsent,
    /// The tool is installed but not answering: a daemon that is not
    /// running, or a client that cannot reach one. Says nothing about how
    /// much this detector would have found.
    ToolNotRunning,
    Failed(String),
}

/// An environment variable through which a tool says where it keeps its
/// files, and so decides where that tool's cache actually lives.
///
/// Some of these name a tool's home directory (`CARGO_HOME`) and some name a
/// cache directory outright (`HOMEBREW_CACHE`); what they have in common is
/// that the tool documents them, so reading one is an answer about this
/// machine and not a guess.
///
/// Carried on [`DiscoveryContext`] rather than read with `std::env::var`
/// inside a `discover()` call. An env-var back-channel makes a detector
/// answer from the *host* machine even when its caller supplied a fixture
/// `home_dir` — the same hazard [`DetectorRegistry::from_detectors`]'s doc
/// comment describes for detectors that shell out to host tools, and one
/// that really did reach a developer's live `~/.cargo/registry` from a test
/// whose `$HOME` was an empty temporary directory (HORO-1543). Cargo sets
/// `CARGO_HOME` in every process it spawns, so `cargo test` guarantees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEnvVar {
    CargoHome,
    GradleUserHome,
    NpmCache,
    GoCache,
    GoModCache,
    GoPath,
    PipCacheDir,
    UvCacheDir,
    /// Not one tool's variable: `uv` documents XDG cache semantics on every
    /// platform including macOS, so `XDG_CACHE_HOME` relocates `uv`'s cache
    /// to `$XDG_CACHE_HOME/uv` — verified against the installed `uv` rather
    /// than assumed, because the neighbouring tools here do *not* honour it.
    XdgCacheHome,
    HomebrewCache,
}

impl ToolEnvVar {
    /// Declaration order is the iteration order of
    /// [`DiscoveryContext::from_process_env`].
    pub const ALL: [ToolEnvVar; 10] = [
        ToolEnvVar::CargoHome,
        ToolEnvVar::GradleUserHome,
        ToolEnvVar::NpmCache,
        ToolEnvVar::GoCache,
        ToolEnvVar::GoModCache,
        ToolEnvVar::GoPath,
        ToolEnvVar::PipCacheDir,
        ToolEnvVar::UvCacheDir,
        ToolEnvVar::XdgCacheHome,
        ToolEnvVar::HomebrewCache,
    ];

    /// Every spelling the tool documents for this variable, canonical first.
    ///
    /// Usually one. `npm` is the exception that makes this a slice: its
    /// configuration variables are documented in the lowercase
    /// `npm_config_*` form, and npm also reads the uppercased form, so
    /// `npm_config_cache` and `NPM_CONFIG_CACHE` both relocate the cache —
    /// both were confirmed against the installed `npm`. Consulting only one
    /// of them would leave the other invisible, and reporting the documented
    /// default path while npm is using a directory the other spelling named
    /// is exactly the kind of confident wrong answer this route exists to
    /// avoid.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            ToolEnvVar::CargoHome => &["CARGO_HOME"],
            ToolEnvVar::GradleUserHome => &["GRADLE_USER_HOME"],
            ToolEnvVar::NpmCache => &["NPM_CONFIG_CACHE", "npm_config_cache"],
            ToolEnvVar::GoCache => &["GOCACHE"],
            ToolEnvVar::GoModCache => &["GOMODCACHE"],
            ToolEnvVar::GoPath => &["GOPATH"],
            ToolEnvVar::PipCacheDir => &["PIP_CACHE_DIR"],
            ToolEnvVar::UvCacheDir => &["UV_CACHE_DIR"],
            ToolEnvVar::XdgCacheHome => &["XDG_CACHE_HOME"],
            ToolEnvVar::HomebrewCache => &["HOMEBREW_CACHE"],
        }
    }

    /// The canonical spelling, for messages. Never used to *read* the
    /// environment — [`DiscoveryContext::from_process_env`] reads every
    /// spelling in [`ToolEnvVar::names`].
    pub fn name(self) -> &'static str {
        self.names()[0]
    }
}

/// Shared context passed to every detector's `discover` call.
pub struct DiscoveryContext {
    pub home_dir: PathBuf,
    /// Project roots to check for build/dependency directories (e.g.
    /// `<root>/target`, `<root>/node_modules`). Empty by default —
    /// detectors that need this do nothing when it's empty rather than
    /// guessing at project locations.
    pub known_project_roots: Vec<PathBuf>,
    /// What the tools' own variables say, as captured at the process
    /// boundary: the variable, the spelling that was set, and its value.
    /// Empty unless a caller supplied them, which is what makes
    /// [`DiscoveryContext::new`] hermetic — see [`ToolEnvVar`].
    ///
    /// Keyed by (variable, spelling) rather than by variable, because two
    /// documented spellings of the same variable can both be set and
    /// disagree, and collapsing them here would hide that from
    /// [`DiscoveryContext::tool_env`].
    tool_env_vars: Vec<(ToolEnvVar, &'static str, String)>,
}

impl DiscoveryContext {
    /// A context that knows nothing about the process environment: every
    /// [`ToolEnvVar`] reads as unset, so a detector resolves its tool home
    /// from `home_dir` alone.
    ///
    /// This is the hermetic constructor, and the default on purpose. A
    /// production caller wants [`DiscoveryContext::from_process_env`]; a test
    /// that reached for that one by accident would silently measure the
    /// developer's real caches.
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            known_project_roots: Vec::new(),
            tool_env_vars: Vec::new(),
        }
    }

    /// `new`, plus every [`ToolEnvVar`] this process actually has set. The
    /// constructor production code uses.
    pub fn from_process_env(home_dir: impl Into<PathBuf>) -> Self {
        let mut ctx = Self::new(home_dir);
        for var in ToolEnvVar::ALL {
            for name in var.names() {
                if let Ok(value) = std::env::var(name) {
                    ctx.tool_env_vars.push((var, name, value));
                }
            }
        }
        ctx
    }

    pub fn with_known_project_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.known_project_roots = roots;
        self
    }

    /// Sets one tool variable explicitly, under its canonical spelling, for
    /// tests that need to exercise the override branch without touching the
    /// process environment (`std::env::set_var` is unsound in Rust's threaded
    /// test harness).
    pub fn with_tool_env(self, var: ToolEnvVar, value: impl Into<String>) -> Self {
        let name = var.name();
        self.with_tool_env_spelling(var, name, value)
    }

    /// [`DiscoveryContext::with_tool_env`] for one named spelling of `var`,
    /// so a test can set two spellings of the same variable and exercise what
    /// happens when they disagree.
    ///
    /// # Panics
    ///
    /// If `name` is not one of `var`'s documented spellings. A test that sets
    /// a spelling the product never reads would pass while proving nothing.
    pub fn with_tool_env_spelling(
        mut self,
        var: ToolEnvVar,
        name: &'static str,
        value: impl Into<String>,
    ) -> Self {
        assert!(
            var.names().contains(&name),
            "{name} is not a documented spelling of {var:?} ({:?})",
            var.names()
        );
        self.tool_env_vars
            .retain(|(existing, spelling, _)| !(*existing == var && *spelling == name));
        self.tool_env_vars.push((var, name, value.into()));
        self
    }

    /// The spelling of `var` that answered and what it said, or `None`.
    ///
    /// `None` means "nothing said otherwise", never "the tool is absent".
    /// It also covers the one genuinely ambiguous case: two documented
    /// spellings set to different values. Which one the tool would honour is
    /// its own precedence rule, not something to be guessed at here, so
    /// nothing is reported and the caller falls back to asking the tool,
    /// which is authoritative about its own configuration.
    pub fn tool_env_spelling(&self, var: ToolEnvVar) -> Option<(&'static str, &str)> {
        let mut set = self
            .tool_env_vars
            .iter()
            .filter(|(existing, _, _)| *existing == var)
            .map(|(_, name, value)| (*name, value.as_str()));
        let first = set.next()?;
        if set.any(|(_, value)| value != first.1) {
            return None;
        }
        Some(first)
    }

    /// [`DiscoveryContext::tool_env_spelling`] without the spelling, for
    /// callers that only need the value.
    pub fn tool_env(&self, var: ToolEnvVar) -> Option<&str> {
        self.tool_env_spelling(var).map(|(_, value)| value)
    }

    /// Whether any documented spelling of `var` is set, whatever they say.
    ///
    /// The difference between this and `tool_env(var).is_some()` is the
    /// ambiguous case, and it matters: a variable that is set but unreadable
    /// still establishes that the user has configured this tool's location,
    /// so falling back to the tool's *default* location would report a
    /// directory the tool is demonstrably not using.
    pub fn tool_env_is_set(&self, var: ToolEnvVar) -> bool {
        self.tool_env_vars
            .iter()
            .any(|(existing, _, _)| *existing == var)
    }
}

/// One detector: a bounded probe of a known root for one family of resources.
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
    /// Typed truncation signal for [`Evidence::reclaimable_bytes_is_lower_bound`]:
    /// `true` only when `bytes` is `Observed` AND the walk stopped early on
    /// its own budget rather than exhausting the subtree. This is the
    /// single source of truth both this typed flag and
    /// [`SizeEstimate::lower_bound_note`]'s advisory text are derived
    /// from — callers must never derive one from the other.
    pub fn is_lower_bound(&self) -> bool {
        self.bytes.is_observed() && self.stop_reason != StopReason::Exhausted
    }

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
                "size estimate stopped early after {} entries ({} unreadable): {:?} time \
                 budget exceeded — reported bytes are a lower bound, not the full subtree total",
                self.entries_visited, self.unreadable_entries, SIZE_ESTIMATE_DEADLINE
            )),
            StopReason::FileCountBudget => Some(format!(
                "size estimate stopped early after {} entries ({} unreadable): {} entry \
                 budget exceeded — reported bytes are a lower bound, not the full subtree total",
                self.entries_visited, self.unreadable_entries, SIZE_ESTIMATE_MAX_ENTRIES
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
    reclaimable_bytes_is_lower_bound: bool,
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
        reclaimable_bytes_is_lower_bound,
        last_modified,
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability,
        recoverability,
        native_cleanup,
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        docker_lifecycle: None,
        collected_at: SystemTime::now(),
        sources: Vec::new(),
    }
}

/// Outcome of asking an installed tool where it keeps something.
///
/// Deliberately not a `Result` (same reasoning as [`ProbeOutcome`]): "the
/// tool is not installed on this machine" is normal, expected state, and
/// must never travel the same channel as "the tool is installed and the
/// probe went wrong" — HORO-1543 AC 2. A caller that collapsed the two
/// would report a broken probe as an absent ecosystem.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ToolQuery {
    /// stdout's non-empty, trimmed lines in the order the tool printed
    /// them. Never empty — no-output is [`ToolQuery::Failed`], because a
    /// tool that answered nothing has not told us where anything is.
    Lines(Vec<String>),
    ToolAbsent,
    Failed(String),
    /// The tool was spawned and did not finish within [`PROBE_DEADLINE`].
    ///
    /// Separate from [`ToolQuery::Failed`] because the two are not the same
    /// claim: `Failed` means the tool answered and the answer was unusable,
    /// which is a fact about the tool. This means we stopped waiting, which
    /// is a fact about *us*. Nothing downstream may read either one as "the
    /// cache is absent" or as "the tool is not installed" (HORO-1559).
    TimedOut(String),
}

/// Wall-clock a single spawned probe may take before it is abandoned
/// (HORO-1559).
///
/// Healthy cache-location queries answer in well under a second — the
/// observed `npm`, `pip`, `uv`, `go env` and `brew --cache` calls on this
/// workstation are all a few hundred milliseconds. Three seconds leaves an
/// order of magnitude of headroom for a loaded machine while keeping a
/// discovery pass responsive.
///
/// The bound exists because an unbounded one was not theoretical: a `proto`
/// shim fronting `npm`, invoked with a cold store, spent over eleven minutes
/// provisioning a toolchain and would have blocked `detect` for as long as it
/// took. A GUI waiting on that has nothing to show and nothing to cancel.
pub(crate) const PROBE_DEADLINE: Duration = Duration::from_secs(3);

/// How often [`query_tool_lines_with_deadline`] re-checks a running child.
///
/// Small enough that the deadline is honoured closely, large enough that a
/// probe which answers immediately is not delayed by polling and does not
/// spin a core while it waits.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Detectors in [`DetectorRegistry::builtin`] that spawn a tool to locate a
/// cache, and so can each spend up to one [`PROBE_DEADLINE`].
///
/// Counted per *detector*, not per call site: `go_build_cache` and
/// `go_module_cache` share one call site and spawn `go env` once each, so
/// call sites would undercount the pass.
///
/// Test-only, like [`WORST_CASE_SPAWN_WAIT`]: nothing in the product reads
/// either one. They exist so the bound is asserted rather than merely
/// described in prose, and a value the product consumed would be a second
/// source of truth for a number already decided by [`PROBE_DEADLINE`].
#[cfg(test)]
pub(crate) const SPAWNING_DETECTORS: &[&str] = &[
    "homebrew_cache",
    "npm_cache",
    "pip_cache",
    "uv_cache",
    "go_build_cache",
    "go_module_cache",
];

/// Worst case wall-clock one discovery pass can spend waiting on spawned
/// tools, if every one of them stalls.
///
/// Detectors run one after another in [`DetectorRegistry::discover_all`], and
/// each spawning site can burn at most one [`PROBE_DEADLINE`] — including
/// `pip`'s two-name loop, which stops at the first timeout rather than paying
/// the deadline again for a second name that routes through the same stalled
/// shim.
///
/// This is a stated serial bound, not a shared pool: a pass-level budget
/// would have to be threaded through [`DiscoveryContext`] and mutated from
/// `&self` detectors, and that contract is not worth reshaping for a case
/// where every tool on the machine stalls at once.
///
/// [`SPAWNING_DETECTORS`] is maintained by hand. The guard test
/// `every_spawning_detector_is_registered_and_accounted_for` keeps it honest
/// for a new module that starts spawning, which is how a regression would
/// realistically arrive.
#[cfg(test)]
pub(crate) const WORST_CASE_SPAWN_WAIT: Duration =
    Duration::from_secs(PROBE_DEADLINE.as_secs() * SPAWNING_DETECTORS.len() as u64);

/// Every program a discovery pass may run on this machine, in the order a
/// reader would look them up (HORO-1560).
///
/// Not test-only, unlike [`SPAWNING_DETECTORS`]: the honesty of the
/// `detect`/`explain` safety label rests on *which* foreign programs a
/// read-only-looking command can start, and the guard test that grounds that
/// label observes these names on a `PATH` of recording stubs rather than
/// trusting a sentence. A list a reader cannot reach from outside the crate
/// could not be checked that way.
///
/// Built from the detectors' own declarations rather than retyped, so the
/// names cannot drift from the call sites. The guard test
/// `every_spawned_program_is_declared_once_and_listed` closes the other
/// direction: a new spawn site must name its program through such a constant,
/// and that constant's value must appear here.
pub const SPAWNED_PROGRAMS: &[&str] = &[
    homebrew::BREW_PROGRAM,
    docker::DOCKER_PROGRAM,
    go::GO_PROGRAM,
    node::NPM_PROGRAM,
    python::PIP_PROGRAM,
    python::PIP3_PROGRAM,
    python::UV_PROGRAM,
];

/// Runs `program args...` and returns stdout's non-empty lines.
///
/// An argument array, never a shell string (HORO-1543 AC 7): there is no
/// interpolation point here for a path, a detector input or anything a
/// model could ever reach, and nothing in `args` is word-split by a shell
/// because no shell is involved.
///
/// stdin is `/dev/null`. A discovery probe must never end up waiting on
/// the user's terminal, so a tool that decides to prompt gets EOF and
/// exits instead of hanging the scan.
///
/// The wait is bounded by [`PROBE_DEADLINE`]. Closing stdin stops a tool
/// that wants *input*; it does nothing for a tool that is simply slow, and
/// one of those blocked `detect` indefinitely (HORO-1559).
pub(crate) fn query_tool_lines(program: &str, args: &[&str]) -> ToolQuery {
    query_tool_lines_with_deadline(program, args, PROBE_DEADLINE)
}

/// [`query_tool_lines`] with the deadline supplied, so tests can assert the
/// timeout path without waiting a real [`PROBE_DEADLINE`] for each one.
pub(crate) fn query_tool_lines_with_deadline(
    program: &str,
    args: &[&str],
    deadline: Duration,
) -> ToolQuery {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ToolQuery::ToolAbsent,
        Err(e) => return ToolQuery::Failed(format!("failed to spawn {program}: {e}")),
    };

    // Drain both pipes on their own threads rather than reading after the
    // wait. A pipe buffer is finite, so a tool that prints more than it holds
    // blocks writing until someone reads — and a deadline that expired on a
    // tool which was only waiting for *us* to read would be a self-inflicted
    // timeout, reported as if the tool were at fault.
    let stdout = child.stdout.take().map(drain_on_thread);
    let stderr = child.stderr.take().map(drain_on_thread);

    let status = match wait_bounded(&mut child, deadline) {
        Ok(Some(status)) => status,
        Ok(None) => {
            return ToolQuery::TimedOut(format!(
                "{program} {} did not answer within {deadline:?}",
                args.join(" ")
            ));
        }
        Err(e) => return ToolQuery::Failed(format!("failed to wait for {program}: {e}")),
    };

    // Joining only on the success path: after a timeout the reader threads
    // end when the killed child's pipes close, and nothing needs their bytes.
    drop(stderr);

    if !status.success() {
        return ToolQuery::Failed(format!(
            "{program} {} exited with status {status}",
            args.join(" ")
        ));
    }

    let bytes = match stdout.map(|handle| handle.join()) {
        Some(Ok(bytes)) => bytes,
        Some(Err(_)) => {
            return ToolQuery::Failed(format!("failed to read {program}'s output"));
        }
        None => Vec::new(),
    };

    let lines: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    if lines.is_empty() {
        return ToolQuery::Failed(format!(
            "{program} {} succeeded but printed nothing",
            args.join(" ")
        ));
    }

    ToolQuery::Lines(lines)
}

/// Waits for `child` for at most `deadline`, returning `Ok(None)` if it is
/// still running when that expires.
///
/// On expiry the child is killed *and reaped*. Killing without reaping leaves
/// a zombie for the lifetime of the CLI; abandoning it without killing leaves
/// it running — the stalled `proto` in HORO-1559 kept consuming CPU until it
/// was signalled by hand, and it would have outlived the process that spawned
/// it.
///
/// This signals the child only, not its descendants. Containing a whole
/// process group would mean spawning into one and signalling the group, which
/// needs a facility beyond `std::process`; the observed case is a direct child
/// and terminating it did release the probe.
fn wait_bounded(
    child: &mut Child,
    deadline: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let expires_at = Instant::now() + deadline;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= expires_at {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        thread::sleep(PROBE_POLL_INTERVAL.min(expires_at - now));
    }
}

/// Reads `reader` to end on a new thread, so a child writing more than a pipe
/// buffer holds is never blocked on us.
fn drain_on_thread<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        // A read error is not this function's to report: the exit status and
        // the resulting empty output already say the probe produced nothing
        // usable, and `Failed("printed nothing")` is the honest outcome.
        let _ = reader.read_to_end(&mut buf);
        buf
    })
}

/// [`query_tool_lines`] for a tool asked for exactly one path.
///
/// The answer must be the *only* absolute path the tool printed. Anything
/// else on stdout is discarded by its shape rather than by its position:
/// picking the first or the last line would turn a banner, a warning or a
/// deprecation notice into a confidently wrong filesystem location, and which
/// end of the output it lands on is not a property anything guarantees.
///
/// Requiring one line and nothing else was the earlier rule, and it reported
/// a false [`ToolQuery::Failed`] — "the probe did not answer" for a tool that
/// answered perfectly well. Observed while writing HORO-1557: with
/// `CLAUDECODE=1` in the environment, the `proto` shims that front `npm`, `pip`
/// and `uv` on one workstation prepend
/// `{"type":"message","message":"Detected an AI agent environment, ..."}` to
/// stdout, so three of the five cache probes failed on a machine where all
/// three tools were installed and working.
///
/// Zero absolute paths, or two of them, is still [`ToolQuery::Failed`]. Two
/// candidates means the output is genuinely ambiguous — a banner that itself
/// mentions a path — and there is no honest way to choose.
pub(crate) fn query_tool_single_path(program: &str, args: &[&str]) -> ToolQuery {
    match query_tool_lines(program, args) {
        ToolQuery::Lines(lines) => single_absolute_path(program, args, lines),
        other => other,
    }
}

/// [`query_tool_single_path`]'s selection rule, without the subprocess, so
/// that the shapes it has to survive can be asserted directly.
fn single_absolute_path(program: &str, args: &[&str], lines: Vec<String>) -> ToolQuery {
    let printed = lines.len();
    let mut paths: Vec<String> = lines
        .into_iter()
        .filter(|line| Path::new(line).is_absolute())
        .collect();

    match paths.len() {
        1 => ToolQuery::Lines(vec![paths.remove(0)]),
        0 => ToolQuery::Failed(format!(
            "{program} {} printed {printed} line(s), none of them an absolute path",
            args.join(" ")
        )),
        n => ToolQuery::Failed(format!(
            "{program} {} printed {n} absolute paths where one was expected",
            args.join(" ")
        )),
    }
}

/// Outcome of building discovery evidence for one directory-shaped,
/// tool-owned cache root.
pub(crate) enum CacheDirProbe {
    /// Boxed only to keep the enum small: an `Evidence` is ~390 bytes and
    /// the other two variants are a pointer's worth, so an unboxed payload
    /// would make every `Absent`/`Failed` return move that much stack for
    /// nothing.
    Found(Box<Evidence>),
    /// The directory is not there. For a tool-owned cache root that means
    /// the tool has not populated it — which is emphatically NOT a
    /// zero-byte resource (HORO-1543 AC 3), so this variant carries no
    /// [`Evidence`] at all rather than evidence claiming 0 bytes.
    Absent,
    Failed(String),
}

/// Builds discovery evidence for `path` as one resource of `kind`.
///
/// Shared by every cache-root detector so the AC-3 rule — a probe failure
/// can never surface as zero bytes or as "nothing found" — is implemented
/// once instead of being re-derived per ecosystem. The three outcomes are
/// kept structurally distinct: a missing directory is `Absent`, an
/// unreadable one is `Failed`, and a readable one whose *size* could not
/// be established is `Found` with `logical_bytes`/`reclaimable_bytes` set
/// to `Unavailable(_)` (never `Observed(0)`) by
/// [`estimate_logical_bytes`].
///
/// `reclaimable_bytes` equals `logical_bytes` here, matching the existing
/// cargo/node/homebrew detectors: a tool-owned cache root has no
/// partial-retention concept to model, so the two are an honest
/// equivalence of meaning rather than a convenient reuse.
pub(crate) fn cache_dir_evidence(
    kind: ResourceKind,
    detector: DetectorId,
    path: &Path,
    regenerability: Regenerability,
    recoverability: Recoverability,
) -> CacheDirProbe {
    if !path.is_absolute() {
        return CacheDirProbe::Failed(format!(
            "{} is not an absolute path",
            path.to_string_lossy()
        ));
    }

    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CacheDirProbe::Absent,
        Err(e) => {
            return CacheDirProbe::Failed(format!(
                "failed to canonicalize {}: {e}",
                path.to_string_lossy()
            ));
        }
    };

    let resource = ResourceId::new(kind, ResourceLocator::Path(canonical.clone()));
    let estimate = estimate_logical_bytes(&canonical, size_estimate_budget());
    let logical_bytes = estimate.bytes.clone();
    let mut evidence = discovery_evidence(
        resource,
        detector,
        &canonical,
        logical_bytes.clone(),
        logical_bytes,
        estimate.is_lower_bound(),
        probe_mtime(&canonical),
        regenerability,
        recoverability,
        // Every kind added by HORO-1543 is detect-and-explain only. None
        // of them has a structurally scoped cleanup contract yet, and the
        // ticket is explicit that a resource may stay detect-only rather
        // than have an action invented to satisfy completion.
        NativeCleanup::Unsupported,
    );
    if let Some(note) = estimate.lower_bound_note() {
        evidence.push_source(note);
    }

    CacheDirProbe::Found(Box::new(evidence))
}

/// What a missing cache root means, which depends entirely on how the
/// detector learned the path.
///
/// The variants are separate because `tool_absent` is documented, all the
/// way out to `GlomerisDtos.swift`'s `DetectorHealthReportDto`, as "the tool
/// that would produce candidates is not installed". That is a claim about
/// the *tool*, and a probe of one cache directory is only entitled to make
/// it when something observable actually supports it (HORO-1575).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RootAbsence {
    /// The path came from running the tool, so the tool is installed: it
    /// answered. A missing directory then means only that the tool has not
    /// written its cache yet — `Found(vec![])`, never `ToolAbsent`.
    ToolAnsweredWithAPathItHasNotWritten,
    /// The path was inferred, and the given parent is a directory that only
    /// this tool creates — a Gradle user home, a Cargo home, `~/.m2`,
    /// `~/Library/Developer/Xcode`. Nothing here ran the tool, so the
    /// parent's own presence is the only evidence available about it, and it
    /// is real evidence: the tool has been here at least once.
    ///
    /// Judged by [`absence_under_tool_owned_parent`]: a present parent means
    /// the tool exists but has not written *this* subdirectory, an absent one
    /// is the `ToolAbsent` claim's only honest basis, and a parent that
    /// cannot be read is `Failed` rather than either.
    InferredUnderToolOwnedParent(PathBuf),
    /// The path was inferred and sits under a directory shared with
    /// unrelated software — `~/Library/Caches` holds entries for most of the
    /// software on a Mac. Neither the root's absence nor its parent's
    /// presence says anything at all about this tool, so the honest answer is
    /// that nothing was found here: `Found(vec![])`, never `ToolAbsent`.
    InferredUnderSharedParent,
}

/// Decides what a missing inferred cache root means from the one thing that
/// can still be observed about its tool: whether the tool's own home
/// directory is there (HORO-1575).
///
/// Kept separate from [`cache_root_status`] so the detector that builds its
/// evidence by hand — `xcode_derived_data` — applies the identical rule
/// instead of re-deriving it.
///
/// The `Err` arm matters as much as the other two. A parent that exists but
/// cannot be read (a sandbox denial on `~/Library/Developer`, say) is not
/// permission to claim either that the tool is missing or that it found
/// nothing: it is `Failed`, which the CLI and the GUI both surface as "this
/// detector did not answer".
pub(crate) fn absence_under_tool_owned_parent(parent: &Path) -> DetectorStatus {
    match parent.try_exists() {
        // The tool's home is there, so the tool has been here; only this
        // subdirectory has not been written yet.
        Ok(true) => DetectorStatus::Found(Vec::new()),
        // Nothing of the tool's exists under this home at all. This is the
        // one case where `ToolAbsent` is an evidence-backed claim.
        Ok(false) => DetectorStatus::ToolAbsent,
        Err(e) => DetectorStatus::Failed(format!(
            "failed to determine whether {} exists: {e}",
            parent.to_string_lossy()
        )),
    }
}

/// How a cache root's location was established, before any tool was run.
///
/// Recorded on the evidence's own provenance list, so `detect --json` always
/// carries which route answered. A byte count reached through a documented
/// default path and one reached by asking the tool are equally real
/// measurements of the same directory, but they are not equally strong claims
/// about *whose* directory it is, and a reader is entitled to know which of
/// the two they are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheRoute {
    /// A documented environment variable, under the spelling carried here,
    /// named the directory.
    EnvVar(&'static str),
    /// No documented variable said otherwise, and the tool's documented
    /// default directory is there.
    DocumentedDefault,
}

impl CacheRoute {
    /// The provenance sentence for [`cache_root_status`]'s `provenance`
    /// argument. `subject` describes the resource in the detector's own
    /// words, the same way the spawning detectors' literals do.
    pub(crate) fn provenance(self, subject: &str) -> String {
        match self {
            CacheRoute::EnvVar(name) => {
                format!("{subject}, at the directory {name} names — the tool was not run")
            }
            CacheRoute::DocumentedDefault => format!(
                "{subject}, at the tool's documented default location — the tool was not run"
            ),
        }
    }
}

/// `~/Library/Caches`, which is where several of these tools document their
/// default cache location on macOS.
///
/// `None` on other platforms rather than a guess at their layout: the whole
/// point of the documented-default route is that the path is something the
/// tool states, and a path this code invented would be neither documented nor
/// a default. Callers then fall through to asking the tool, which is where
/// every one of these detectors was before.
///
/// Takes `home` rather than reading `$HOME` so a fixture context resolves
/// against its own directory (HORO-1543).
pub(crate) fn user_caches_dir(home: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Caches"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        None
    }
}

/// What a cache root reached through [`documented_cache_root`] means when it
/// turns out not to be there after all.
///
/// That function only answers for a directory that exists, so this is reached
/// only if the directory disappears between its check and the probe — a real
/// race on a cache its tool is free to clear at any moment. Nothing on this
/// route ran the tool, so nothing on it is entitled to say whether the tool is
/// installed: [`RootAbsence::InferredUnderSharedParent`] is the variant that
/// makes no claim about it (HORO-1575).
pub(crate) const DOCUMENTED_ROUTE_ABSENCE: RootAbsence = RootAbsence::InferredUnderSharedParent;

/// Where a tool's own documented configuration says its cache is, without
/// running the tool (HORO-1560 AC 2).
///
/// Order: a documented environment variable beats the documented default
/// path, and either beats spawning the tool. `None` means the documented
/// routes did not answer and the caller should ask the tool, which remains
/// the fallback.
///
/// ## Why only a directory that already exists counts as an answer
///
/// A documented location holding nothing says nothing about the resource, and
/// the *tool* can still distinguish the two states that would otherwise be
/// folded together: "not installed" and "installed, has written no cache yet".
/// Reporting the first as the second is the exact defect HORO-1558 and
/// HORO-1575 were filed for. So an absent directory falls through to the
/// spawn, which keeps every status this module reports at least as strong as
/// it was — this route removes spawns, never information.
///
/// A value that is empty or relative also falls through. What a tool resolves
/// a relative cache path against is its own business, and a path this code
/// assembled from a guess about that would be a fabrication dressed as an
/// observation.
///
/// ## The limitation this route knowingly has
///
/// A tool relocated through its own *config file* — `.npmrc`, go's `env`
/// file — is invisible here; only the tool knows about those. The
/// existing-directory rule is what keeps that from producing a wrong answer:
/// such a tool usually has no directory at the default location, so the tool
/// is asked. Where a stale default directory does exist, the bytes reported
/// are that directory's real bytes, attributed to the tool that created it —
/// a resource that is genuinely on this machine either way.
pub(crate) fn documented_cache_root(
    ctx: &DiscoveryContext,
    var: ToolEnvVar,
    documented_default: Option<PathBuf>,
) -> Option<(PathBuf, CacheRoute)> {
    let (path, route) = match ctx.tool_env_spelling(var) {
        Some((name, value)) => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return None;
            }
            (path, CacheRoute::EnvVar(name))
        }
        // Set, and the documented spellings disagree. The default location is
        // not the fallback here: the user has configured this tool's cache
        // somewhere, so its default directory is one the tool is
        // demonstrably not using, and only the tool knows which of the two it
        // honours.
        None if ctx.tool_env_is_set(var) => return None,
        None => (documented_default?, CacheRoute::DocumentedDefault),
    };

    if path.is_dir() {
        Some((path, route))
    } else {
        None
    }
}

/// [`cache_dir_evidence`] for a detector whose whole answer is one cache
/// root, recording where the path came from on the evidence itself.
pub(crate) fn cache_root_status(
    detector: DetectorId,
    kind: ResourceKind,
    path: &Path,
    regenerability: Regenerability,
    recoverability: Recoverability,
    provenance: &str,
    absence: RootAbsence,
) -> DetectorStatus {
    match cache_dir_evidence(kind, detector, path, regenerability, recoverability) {
        CacheDirProbe::Found(mut evidence) => {
            evidence.push_source(provenance.to_string());
            DetectorStatus::Found(vec![*evidence])
        }
        CacheDirProbe::Absent => match absence {
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten
            | RootAbsence::InferredUnderSharedParent => DetectorStatus::Found(Vec::new()),
            RootAbsence::InferredUnderToolOwnedParent(parent) => {
                absence_under_tool_owned_parent(&parent)
            }
        },
        CacheDirProbe::Failed(msg) => DetectorStatus::Failed(msg),
    }
}

/// Plain compile-time list of the built-in detectors — deliberately NOT a
/// plugin/inventory registration system. A hand-written list of this size
/// doesn't earn that complexity.
pub struct DetectorRegistry {
    detectors: Vec<Box<dyn Detector>>,
}

impl DetectorRegistry {
    /// Registers every built-in detector, in the order discovery runs
    /// them. `builtin_registry_registers_every_builtin_detector` pins the
    /// list by identity, so this doc comment deliberately does not repeat
    /// it in prose where it could rot.
    pub fn builtin() -> Self {
        Self {
            detectors: vec![
                Box::new(xcode::XcodeDetector),
                Box::new(homebrew::HomebrewDetector),
                Box::new(cargo::CargoDetector),
                Box::new(cargo::CargoRegistryCacheDetector),
                Box::new(node::NodeDetector),
                Box::new(node::NodePackageManagerCacheDetector),
                Box::new(docker::DockerDetector),
                Box::new(docker_objects::DockerObjectDetector),
                Box::new(python::PipCacheDetector),
                Box::new(python::UvCacheDetector),
                Box::new(go::GoBuildCacheDetector),
                Box::new(go::GoModuleCacheDetector),
                Box::new(gradle::GradleCacheDetector),
                Box::new(maven::MavenLocalRepositoryDetector),
                Box::new(swiftpm::SwiftPmCacheDetector),
                Box::new(swiftpm::SwiftPmBuildDirDetector),
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
        self.discover_all_with_progress(ctx, |_, _| {})
    }

    /// Same discovery loop as [`DetectorRegistry::discover_all`] — one
    /// detector at a time, in registration order, identical
    /// `DetectorStatus` results — but additionally invokes `on_progress`
    /// immediately before and after each detector's `discover` call
    /// (HORO-1052). This is the one place per-detector discovery
    /// granularity is observable from outside this module; `detect`/
    /// `explain`/`llm-plan`'s shared `cli::discover_and_classify_with_progress`
    /// is the only caller that passes a non-no-op callback (to emit
    /// `--progress-json` NDJSON lines on stderr) — `discover_all` itself
    /// passes a no-op closure, so this refactor changes no observable
    /// behavior for any existing caller.
    pub fn discover_all_with_progress(
        &self,
        ctx: &DiscoveryContext,
        mut on_progress: impl FnMut(DetectorId, DetectorProgress<'_>),
    ) -> Vec<(DetectorId, DetectorStatus)> {
        self.detectors
            .iter()
            .map(|detector| {
                let id = detector.id();
                on_progress(id, DetectorProgress::Started);
                let status = detector.discover(ctx);
                on_progress(id, DetectorProgress::Finished(&status));
                (id, status)
            })
            .collect()
    }
}

/// Per-detector lifecycle event reported by
/// [`DetectorRegistry::discover_all_with_progress`] (HORO-1052). Kept
/// crate-internal-shaped (borrows `&DetectorStatus` rather than owning a
/// presentation DTO) so this module has no dependency on
/// `crate::reporting` — the CLI layer, which already depends on both, is
/// responsible for projecting this into `reporting::dto::ProgressEvent`.
#[derive(Debug)]
pub enum DetectorProgress<'a> {
    Started,
    Finished(&'a DetectorStatus),
}

/// Disposable-fixture helpers shared by the detector modules' tests.
///
/// Every detector added by HORO-1543 needs the same thing: a unique
/// throwaway directory to stand in for a cache root, so its evidence path
/// can be exercised on a machine where the real tool is absent. Kept in one
/// place rather than copied per module — the two pre-HORO-1543 detectors
/// that predate it keep their own local copies, which this deliberately
/// does not go and rewrite.
#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    /// A fresh, empty directory under the system temp dir. Unique per
    /// process, per call and per nanosecond, because the test harness runs
    /// these concurrently.
    pub(crate) fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `ToolAbsent` claim's only honest basis: the tool's own home is
    /// not there either (HORO-1575).
    #[test]
    fn a_missing_tool_home_is_the_one_basis_for_claiming_the_tool_is_absent() {
        let parent = test_support::make_temp_dir("absence-parent-gone");
        let gone = parent.join("never-created");
        assert_eq!(
            absence_under_tool_owned_parent(&gone),
            DetectorStatus::ToolAbsent
        );
        std::fs::remove_dir_all(&parent).ok();
    }

    /// The defect this rule exists to stop: the tool's home is right there,
    /// so the tool has been here, and the only thing its missing cache
    /// subdirectory establishes is that the cache is empty.
    #[test]
    fn a_present_tool_home_means_an_empty_cache_not_a_missing_tool() {
        let parent = test_support::make_temp_dir("absence-parent-present");
        match absence_under_tool_owned_parent(&parent) {
            DetectorStatus::Found(evidence) => assert!(
                evidence.is_empty(),
                "a missing cache root must not become a zero-byte resource"
            ),
            other => panic!(
                "a present tool home must not be reported as {other:?}: the tool \
                 demonstrably ran here"
            ),
        }
        std::fs::remove_dir_all(&parent).ok();
    }

    /// A parent that cannot be read is neither answer. `try_exists` is what
    /// makes this reachable at all — `Path::exists` collapses every error
    /// into `false`, which would have reported the tool as absent on a
    /// sandbox denial.
    #[test]
    fn an_unreadable_tool_home_is_failed_rather_than_absent_or_empty() {
        let parent = test_support::make_temp_dir("absence-parent-unreadable");
        let barrier = parent.join("barrier");
        std::fs::create_dir(&barrier).unwrap();
        let target = barrier.join("tool-home");
        std::fs::create_dir(&target).unwrap();
        // Remove search permission on the intermediate directory, so
        // resolving `target` fails with EACCES rather than ENOENT.
        let mut perms = std::fs::metadata(&barrier).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        std::fs::set_permissions(&barrier, perms).unwrap();

        let status = absence_under_tool_owned_parent(&target);

        // Restore before asserting, so a failure still leaves a removable
        // fixture behind.
        let mut perms = std::fs::metadata(&barrier).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(&barrier, perms).unwrap();
        std::fs::remove_dir_all(&parent).ok();

        match status {
            DetectorStatus::Failed(_) => {}
            other => panic!(
                "an unreadable tool home must be Failed, got {other:?}: not knowing \
                 whether the tool is there is not the same as knowing it is not"
            ),
        }
    }

    /// Pins the production wiring by identity and order rather than by a
    /// bare count: a detector silently dropped from `builtin()`, or renamed
    /// without its callers noticing, both fail here and both name the
    /// detector involved. Only reads `id()` — never `discover()` — which is
    /// why this test is allowlisted in
    /// `no_unreviewed_test_code_wires_up_the_real_detector_registry` below.
    #[test]
    fn builtin_registry_registers_every_builtin_detector() {
        let registry = DetectorRegistry::builtin();
        let ids: Vec<&str> = registry.detectors.iter().map(|d| d.id().0).collect();
        assert_eq!(
            ids,
            vec![
                "xcode_derived_data",
                "homebrew_cache",
                "cargo_target_dir",
                "cargo_registry_cache",
                "node_modules",
                "npm_cache",
                "docker_build_cache",
                "docker_objects",
                "pip_cache",
                "uv_cache",
                "go_build_cache",
                "go_module_cache",
                "gradle_cache",
                "maven_local_repository",
                "swiftpm_cache",
                "swiftpm_build_dir",
            ]
        );
    }

    /// HORO-1552 AC 4: every `ResourceKind` a detector could ever declare
    /// must actually be declared by one, in the registry that production
    /// wires up.
    ///
    /// A kind that nothing can discover still has an `owning_tool()`, a
    /// `regenerability()`, a policy class, a GUI word and — the reason this
    /// guard exists — acceptance by `AutopilotEnvelope::allow_kind`. A user
    /// can put such a kind into an Autopilot allowlist, see no error, have it
    /// persisted, and have authorized nothing at all, because no code path
    /// can construct a resource of that kind for the permission to apply to.
    /// That is a control reporting "configured" over zero real surface.
    ///
    /// `ResourceKind::Unknown` is exempt by construction: it is the
    /// fail-closed sink for a kind this build does not recognise, and a
    /// detector that *could* emit it would itself be the bug.
    ///
    /// `NOT_YET_DETECTABLE` is a self-retiring exemption, not a permanent
    /// one — its exact contents are asserted below, so the ticket that adds
    /// the missing detector is forced to empty it in the same change.
    ///
    /// Only reads `resource_kinds()` and `id()`, never `discover()`, which is
    /// why this is safe to allowlist in
    /// `no_unreviewed_test_code_wires_up_the_real_detector_registry` below.
    #[test]
    fn every_resource_kind_except_unknown_has_a_detector() {
        use crate::evidence::ResourceKind;

        /// Kinds with no detector yet, each with the ticket that owns it.
        ///
        /// Empty as of HORO-1544: `docker_objects` declares the last three —
        /// `DockerContainer`, `DockerImage` and `DockerVolume` — as the
        /// distinct lifecycle evidence they are, rather than collapsing them
        /// into the existing build-cache detector to satisfy this guard, which
        /// is what that ticket forbids.
        const NOT_YET_DETECTABLE: &[ResourceKind] = &[];

        // Pinned empty, so the exemption cannot quietly come back. A kind added
        // to `ResourceKind` without a detector must either get one or be listed
        // here with the ticket that owns it — and the list is asserted so the
        // second choice cannot be made silently.
        assert!(
            NOT_YET_DETECTABLE.is_empty(),
            "a kind was exempted from needing a detector — name the ticket that \
             owns it in this test's doc comment, and delete this assertion's \
             expectation of emptiness deliberately rather than as a side effect"
        );

        let registry = DetectorRegistry::builtin();
        let declared: Vec<ResourceKind> = registry
            .detectors
            .iter()
            .flat_map(|d| d.resource_kinds().iter().copied())
            .collect();

        let undeclared: Vec<&str> = ResourceKind::ALL
            .iter()
            .filter(|kind| **kind != ResourceKind::Unknown)
            .filter(|kind| !NOT_YET_DETECTABLE.contains(kind))
            .filter(|kind| !declared.contains(kind))
            .map(|kind| kind.tag())
            .collect();

        assert!(
            undeclared.is_empty(),
            "these ResourceKind variants are policy-classified and \
             Autopilot-allowlistable but no detector in the production \
             registry declares them, so authorizing them authorizes \
             nothing: {undeclared:?}"
        );

        // A kind exempted *and* declared means the exemption is stale.
        for kind in NOT_YET_DETECTABLE {
            assert!(
                !declared.contains(kind),
                "{} is exempted as not-yet-detectable but a detector now \
                 declares it — remove it from NOT_YET_DETECTABLE",
                kind.tag()
            );
        }
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
                DetectorStatus::ToolNotRunning => DetectorStatus::ToolNotRunning,
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
    /// - the two tests directly above —
    ///   `builtin_registry_registers_every_builtin_detector`, which reads
    ///   only `id()`, and `every_resource_kind_except_unknown_has_a_detector`
    ///   (HORO-1552 AC 4), which reads only `resource_kinds()` and `id()`.
    ///   Neither calls `.discover()` or `.execute()` on anything, so
    ///   neither can reach a real host tool: they inspect the registry's
    ///   *shape*, which is the whole point of asserting it against the
    ///   production wiring rather than a stub. The sibling
    ///   `discover_all_returns_one_status_per_detector` test deliberately
    ///   proves its invariant with fake detectors instead, so it does
    ///   not appear here. `probe_deadline_tests::`
    ///   `every_spawning_detector_is_registered_and_accounted_for`
    ///   (HORO-1559) is the same reviewed shape: it reads `id()` on each
    ///   registered detector to check [`SPAWNING_DETECTORS`] still names
    ///   real ones, and must use the production wiring because a stub
    ///   registry could not go stale — it never calls `discover()`, so it
    ///   spawns no tool and produces no evidence to execute;
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
            ("src/detectors/mod.rs", 3),
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

    /// Companion structural guard (HORO-1543): a detector must not read the
    /// process environment, and production must not build a
    /// [`DiscoveryContext`] that cannot see it.
    ///
    /// Both halves of that come from one incident. The Cargo registry and
    /// Gradle detectors originally called `std::env::var` inside
    /// `discover()`, which made them answer from `CARGO_HOME` /
    /// `GRADLE_USER_HOME` no matter what `DiscoveryContext::home_dir` said.
    /// Because `cargo test` exports `CARGO_HOME` into every process it
    /// spawns, an integration test that carefully pointed `$HOME` at an
    /// empty temporary directory still measured the developer's real
    /// `~/.cargo/registry`. The env-var back-channel is the defect;
    /// [`ToolEnvVar`] plus [`DiscoveryContext::from_process_env`] is the
    /// fix, and this test is what stops the back-channel coming back.
    ///
    /// Same lightweight source-text technique as the guard above, and the
    /// same reason: an AST pass would be a lot of machinery to answer a
    /// question about two identifiers.
    #[test]
    fn no_detector_reads_the_process_environment() {
        // Split so this test's own source never contains either needle.
        let env_read = format!("{}{}", "env::", "var");
        let hermetic_ctor = format!("{}{}", "DiscoveryContext::", "new(");

        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

        // `mod.rs` holds `DiscoveryContext::from_process_env`, which is the
        // one place in this module tree that legitimately reads the
        // environment: it is not a detector, it is the boundary that keeps
        // detectors from having to be one.
        let env_read_allowlist: &[(&str, usize)] = &[("src/detectors/mod.rs", 1)];

        let mut offenders: Vec<String> = Vec::new();
        scan_rs_files(&manifest_dir.join("src/detectors"), &mut |path, content| {
            let rel = path
                .strip_prefix(manifest_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            // Production code only: a detector *test* may read the
            // environment (several use `std::env::temp_dir`), and a test
            // cannot smuggle host state into a production probe.
            let production = match content.split_once("#[cfg(test)]") {
                Some((before, _)) => before,
                None => content,
            };
            let count = production
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| line.contains(&env_read))
                .count();
            let expected = env_read_allowlist
                .iter()
                .find(|(p, _)| *p == rel)
                .map(|(_, n)| *n)
                .unwrap_or(0);
            if count != expected {
                offenders.push(format!(
                    "{rel} ({count} occurrence(s), expected {expected})"
                ));
            }
        });

        assert!(
            offenders.is_empty(),
            "detector production code must not read the process environment: \
             {offenders:?} — a detector that does answers from the host machine \
             even under a fixture DiscoveryContext. Carry the variable on \
             DiscoveryContext as a ToolEnvVar instead (HORO-1543)"
        );

        // The other half: every production DiscoveryContext must be built
        // with `from_process_env`, or a relocated tool home is invisible to
        // the product while remaining perfectly visible to its tests.
        let main_rs = fs::read_to_string(manifest_dir.join("src/main.rs"))
            .expect("failed to read src/main.rs");
        let hermetic_uses = main_rs
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains(&hermetic_ctor))
            .count();
        assert_eq!(
            hermetic_uses, 0,
            "src/main.rs builds a DiscoveryContext with the hermetic \
             constructor, so a user who has relocated CARGO_HOME or \
             GRADLE_USER_HOME gets tool_absent for a cache that exists; use \
             DiscoveryContext::from_process_env (HORO-1543)"
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
    pub(super) fn scan_rs_files(dir: &Path, visit: &mut dyn FnMut(&Path, &str)) {
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

/// Tests for the bound on a spawned probe (HORO-1559).
///
/// These do spawn processes, unlike the selection-rule tests below, because
/// the property under test *is* what happens to a real child that will not
/// finish. `sleep` and `seq` stand in for a stalled and a chatty tool: both
/// are POSIX utilities, and neither depends on which language toolchains the
/// machine running the tests happens to have.
#[cfg(test)]
mod probe_deadline_tests {
    use super::*;

    /// Short enough to keep the suite fast, long enough that a loaded machine
    /// still starts the child before it expires.
    const TEST_DEADLINE: Duration = Duration::from_millis(250);

    /// The duration argument that identifies the one child
    /// [`an_abandoned_probe_leaves_no_running_child`] is allowed to see.
    ///
    /// Counting children named `sleep` is not good enough. `cargo test` runs
    /// this binary's tests as threads of a single process, so every `sleep` any
    /// other test spawns — `evidence::correlate::timeout`'s `sleep 5`, and this
    /// module's own `sleep 30` — is also a child of `std::process::id()`, and a
    /// before/after count of them races those tests starting and finishing.
    /// This value appears at exactly one call site, so a matching child can
    /// only be the one under test.
    const STALL_MARKER_SECONDS: &str = "31.4159";

    /// The duration argument of the child
    /// [`an_abandoned_probe_leaves_no_running_child`] spawns and deliberately
    /// leaves running, to prove [`own_children_matching`] can see a child at
    /// all. Distinct from [`STALL_MARKER_SECONDS`] so the two never count each
    /// other.
    const CONTROL_MARKER_SECONDS: &str = "27.1828";

    /// The duration argument of the child the reaping test kills without
    /// waiting for, to prove [`own_zombie_children`] can see an unreaped child.
    const ZOMBIE_MARKER_SECONDS: &str = "23.6067";

    /// Counts our own children that have exited and not been reaped.
    ///
    /// A zombie has no argv left — `ps` renders it as `<defunct>` — so unlike
    /// [`own_children_matching`] this cannot be narrowed to one test's child.
    /// [`settles_to_no_zombie_children`] is what makes it usable anyway.
    fn own_zombie_children() -> usize {
        let me = std::process::id().to_string();
        let out = Command::new("ps")
            .args(["-A", "-o", "ppid=,stat="])
            .output()
            .expect("failed to run ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let ppid = parts.next()?;
                let stat = parts.next()?;
                (ppid == me && stat.starts_with('Z')).then_some(())
            })
            .count()
    }

    /// Waits for our zombie-child count to reach zero, returning whether it did.
    ///
    /// The settling is the whole point, not slack. Tests run as threads of one
    /// process, so another test's own kill-then-reap is briefly visible as a
    /// zombie of ours — but it clears within its own `wait`. A child that
    /// nothing ever waits on stays a zombie for the life of the process, so
    /// "clears" and "does not clear" separate exactly the two cases.
    fn settles_to_no_zombie_children() -> bool {
        let expires_at = Instant::now() + Duration::from_secs(2);
        loop {
            if own_zombie_children() == 0 {
                return true;
            }
            if Instant::now() >= expires_at {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Counts our own still-running child processes whose command line contains
    /// `marker`.
    ///
    /// `ps` is read-only and its own invocation is reaped by `output()` before
    /// the count is taken. Matched against the full argv rather than `comm`
    /// because the marker is an argument.
    ///
    /// `-A` is load-bearing, not tidiness. Without it `ps` lists only processes
    /// attached to the current terminal, and a test binary has none — so the
    /// count was silently always zero, and the assertion it backs held for a
    /// probe that leaked its child just as happily as for one that killed it.
    /// The positive control in that test is what keeps this honest.
    fn own_children_matching(marker: &str) -> usize {
        let me = std::process::id().to_string();
        let out = Command::new("ps")
            .args(["-A", "-o", "ppid=,args="])
            .output()
            .expect("failed to run ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let (ppid, argv) = line.trim_start().split_once(char::is_whitespace)?;
                (ppid == me && argv.contains(marker)).then_some(())
            })
            .count()
    }

    /// The defect itself: before the deadline existed this call did not
    /// return, and neither did `glomeris detect`.
    #[test]
    fn a_probe_that_never_answers_is_abandoned_at_the_deadline() {
        let started = Instant::now();
        let query = query_tool_lines_with_deadline("sleep", &["30"], TEST_DEADLINE);
        let elapsed = started.elapsed();

        assert!(
            matches!(query, ToolQuery::TimedOut(_)),
            "a tool that never answers must be reported as abandoned, got {query:?}"
        );
        assert!(
            elapsed >= TEST_DEADLINE,
            "returned in {elapsed:?}, before the {TEST_DEADLINE:?} deadline — \
             the wait was not actually bounded by the deadline"
        );
        assert!(
            elapsed < TEST_DEADLINE * 8,
            "took {elapsed:?} for a {TEST_DEADLINE:?} deadline; the 30s child \
             was waited on rather than abandoned"
        );
    }

    /// A timeout must not be readable as any of the three outcomes that would
    /// let a caller conclude something about the cache. `Lines` says where it
    /// is, `ToolAbsent` says the ecosystem is not installed, and an empty
    /// `Lines` cannot occur at all — only `TimedOut` says "we do not know".
    #[test]
    fn an_abandoned_probe_is_none_of_the_answering_outcomes() {
        let query = query_tool_lines_with_deadline("sleep", &["30"], TEST_DEADLINE);

        assert!(!matches!(query, ToolQuery::Lines(_)));
        assert!(!matches!(query, ToolQuery::ToolAbsent));
        assert!(!matches!(query, ToolQuery::Failed(_)));
        assert!(matches!(query, ToolQuery::TimedOut(_)));
    }

    /// Abandoning the wait must not abandon the process. An unreaped child is
    /// a zombie for the life of the CLI; an unkilled one keeps running — the
    /// `proto` in HORO-1559 was still burning CPU minutes later.
    #[test]
    fn an_abandoned_probe_leaves_no_running_child() {
        // A child deliberately left running, so "nothing matched" cannot pass
        // by the observation being blind. Every observation is taken before
        // anything is asserted, so a failing assertion cannot leak it.
        let mut control = Command::new("sleep")
            .arg(CONTROL_MARKER_SECONDS)
            .stdin(Stdio::null())
            .spawn()
            .expect("failed to spawn the control child");

        let control_seen = own_children_matching(CONTROL_MARKER_SECONDS);
        let before = own_children_matching(STALL_MARKER_SECONDS);
        let query = query_tool_lines_with_deadline("sleep", &[STALL_MARKER_SECONDS], TEST_DEADLINE);
        // The kill is synchronous with the reap, so no settling loop is
        // needed: if the child were still listed here it would still be ours.
        let after = own_children_matching(STALL_MARKER_SECONDS);

        control.kill().ok();
        control.wait().ok();

        assert_eq!(
            control_seen, 1,
            "a child this test is still holding open was not observed, so the \
             assertion below would pass for a probe that leaked its child"
        );
        assert_eq!(
            before, 0,
            "{STALL_MARKER_SECONDS} is supposed to appear at one call site only"
        );
        assert!(matches!(query, ToolQuery::TimedOut(_)));
        assert_eq!(
            after, 0,
            "a timed-out probe left its child running or unreaped"
        );
    }

    /// The other half of leaving no process behind: killing a child does not
    /// dispose of it. An unwaited-for child stays a zombie in this process's
    /// table for as long as the process lives, and `glomeris detect` is invoked
    /// by a menu-bar app that lives a long time.
    #[test]
    fn an_abandoned_probe_leaves_no_unreaped_child() {
        // Plain `sleep 30`, not [`STALL_MARKER_SECONDS`]: this test counts
        // zombies rather than matching argv, and taking that marker would cost
        // `an_abandoned_probe_leaves_no_running_child` the uniqueness its own
        // assertion depends on.
        let query = query_tool_lines_with_deadline("sleep", &["30"], TEST_DEADLINE);
        assert!(matches!(query, ToolQuery::TimedOut(_)));

        // Positive control, run after the probe so its own zombie cannot be
        // mistaken for the probe's: a child killed and deliberately not reaped
        // must be observable, or the assertion below would hold for a probe
        // that never reaped either. Reaped here, before that assertion.
        let mut leaked = Command::new("sleep")
            .arg(ZOMBIE_MARKER_SECONDS)
            .stdin(Stdio::null())
            .spawn()
            .expect("failed to spawn the control child");
        leaked.kill().expect("failed to kill the control child");
        let mut saw_zombie = false;
        let expires_at = Instant::now() + Duration::from_secs(2);
        while Instant::now() < expires_at {
            if own_zombie_children() >= 1 {
                saw_zombie = true;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        leaked.wait().expect("failed to reap the control child");

        assert!(
            saw_zombie,
            "a child killed and deliberately left unreaped was not observed, so \
             the assertion below cannot distinguish a probe that reaps from one \
             that does not"
        );
        assert!(
            settles_to_no_zombie_children(),
            "a timed-out probe left an unreaped child"
        );
    }

    /// The other half of the bound: a tool that answers must not be slowed or
    /// truncated by the machinery that bounds one that does not.
    #[test]
    fn a_probe_that_answers_promptly_still_answers() {
        match query_tool_lines_with_deadline("echo", &["/tmp/somewhere"], TEST_DEADLINE) {
            ToolQuery::Lines(lines) => assert_eq!(lines, vec!["/tmp/somewhere".to_string()]),
            other => panic!("expected the echoed line, got {other:?}"),
        }
    }

    /// A tool printing more than one pipe buffer holds would block writing if
    /// nothing drained it, and the deadline would then expire on a tool that
    /// was only waiting for us — a self-inflicted timeout blamed on the tool.
    /// 200k lines is far more than any pipe buffer.
    #[test]
    fn a_chatty_probe_is_not_a_self_inflicted_timeout() {
        match query_tool_lines_with_deadline("seq", &["200000"], Duration::from_secs(30)) {
            ToolQuery::Lines(lines) => assert_eq!(lines.len(), 200_000),
            other => panic!("expected 200000 drained lines, got {other:?}"),
        }
    }

    /// A tool that exits non-zero is still a `Failed` probe and not a
    /// timeout: it answered, and the answer was that it could not help.
    #[test]
    fn a_tool_that_exits_non_zero_is_failed_not_timed_out() {
        match query_tool_lines_with_deadline("false", &[], TEST_DEADLINE) {
            ToolQuery::Failed(msg) => assert!(msg.contains("exited with status")),
            other => panic!("expected a failed probe, got {other:?}"),
        }
    }

    /// A program that is not installed must still be `ToolAbsent`. The
    /// deadline changed how the child is waited on, and conflating "no such
    /// program" with "did not answer in time" would report every missing
    /// ecosystem as a broken probe.
    #[test]
    fn a_program_that_does_not_exist_is_still_tool_absent() {
        assert_eq!(
            query_tool_lines_with_deadline("glomeris-no-such-program-exists", &[], TEST_DEADLINE),
            ToolQuery::ToolAbsent
        );
    }

    /// The shipped deadline, not just the injected one, is what `detect` uses.
    #[test]
    fn the_shipped_deadline_is_the_one_wired_into_the_probe() {
        assert_eq!(PROBE_DEADLINE, Duration::from_secs(3));
        assert!(
            PROBE_DEADLINE > TEST_DEADLINE,
            "the tests must exercise a shorter deadline than production, or \
             they prove nothing about production being bounded"
        );
    }

    /// AC 7: the worst case for a whole pass is stated, and it is the serial
    /// sum it claims to be. Every detector named must actually exist, so the
    /// count cannot drift by a detector being renamed away.
    #[test]
    fn every_spawning_detector_is_registered_and_accounted_for() {
        let registry = DetectorRegistry::builtin();
        let registered: Vec<&str> = registry.detectors.iter().map(|d| d.id().0).collect();

        for id in SPAWNING_DETECTORS {
            assert!(
                registered.contains(id),
                "SPAWNING_DETECTORS names {id}, which is not registered — the \
                 worst-case pass bound is computed from a detector that no \
                 longer exists"
            );
        }

        assert_eq!(
            WORST_CASE_SPAWN_WAIT,
            PROBE_DEADLINE * SPAWNING_DETECTORS.len() as u32,
            "the documented worst case is no longer the serial sum it claims"
        );

        // The realistic regression is a *new* module that starts spawning
        // without the bound being revisited, so pin which modules may.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let spawning_modules: &[&str] = &[
            "src/detectors/go.rs",
            "src/detectors/homebrew.rs",
            "src/detectors/node.rs",
            "src/detectors/python.rs",
        ];
        // Split so this test's own source does not contain the needle.
        let needle = format!("{}{}", "query_tool_", "single_path(");

        let mut unexpected: Vec<String> = Vec::new();
        super::tests::scan_rs_files(&manifest_dir.join("src/detectors"), &mut |path, content| {
            let rel = path
                .strip_prefix(manifest_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "src/detectors/mod.rs" || spawning_modules.contains(&rel.as_str()) {
                return;
            }
            let production = match content.split_once("#[cfg(test)]") {
                Some((before, _)) => before,
                None => content,
            };
            if production
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .any(|line| line.contains(&needle))
            {
                unexpected.push(rel);
            }
        });

        assert!(
            unexpected.is_empty(),
            "{unexpected:?} spawn a tool but are not accounted for in \
             SPAWNING_DETECTORS, so the worst-case pass wait is understated \
             (HORO-1559)"
        );
    }
}

/// Tests for [`single_absolute_path`] — the rule that decides which of a
/// tool's stdout lines is the path it was asked for (HORO-1557).
///
/// Pure, without spawning anything: the shapes worth defending are properties
/// of the selection rule, and a test that ran the real `npm` would assert a
/// property of whichever npm the machine happens to have.
#[cfg(test)]
mod single_absolute_path_tests {
    use super::*;

    fn select(lines: &[&str]) -> ToolQuery {
        single_absolute_path(
            "npm",
            &["config", "get", "cache"],
            lines.iter().map(|l| l.to_string()).collect(),
        )
    }

    /// The shape that motivated the rule, quoted from the workstation where it
    /// was observed: with `CLAUDECODE=1` set, the `proto` shim fronting `npm`
    /// prepends an NDJSON notice to stdout before npm's own answer. The earlier
    /// one-line-and-nothing-else rule reported `Failed` here — "the probe did
    /// not answer" for a tool that answered perfectly well.
    #[test]
    fn a_banner_line_before_the_answer_is_discarded() {
        assert_eq!(
            select(&[
                r#"{"type":"message","message":"Detected an AI agent environment, printing as NDJSON. Trace logs are written to stderr, while user-facing logs are written to stdout."}"#,
                "/tmp/probeshape/.npm",
            ]),
            ToolQuery::Lines(vec!["/tmp/probeshape/.npm".to_string()])
        );
    }

    /// A banner *after* the answer is discarded by the same rule. Which end of
    /// stdout the noise lands on is not a property anything guarantees, which
    /// is why the rule is about shape rather than position.
    #[test]
    fn a_trailing_notice_after_the_answer_is_discarded() {
        assert_eq!(
            select(&["/Users/dev/.npm", "npm notice New version available"]),
            ToolQuery::Lines(vec!["/Users/dev/.npm".to_string()])
        );
    }

    /// The ordinary case, unchanged.
    #[test]
    fn a_lone_absolute_path_is_the_answer() {
        assert_eq!(
            select(&["/Users/dev/.npm"]),
            ToolQuery::Lines(vec!["/Users/dev/.npm".to_string()])
        );
    }

    /// Output that names no absolute path at all is a failed probe, not an
    /// absent tool: the tool ran and said something, so `ToolAbsent` would be
    /// a false statement about the machine. A relative path is included here
    /// deliberately — accepting one would produce a location resolved against
    /// whatever directory Glomeris happened to be started from.
    #[test]
    fn output_with_no_absolute_path_fails() {
        for lines in [
            vec!["undefined"],
            vec!["relative/cache/dir"],
            vec!["npm notice something", "npm notice something else"],
        ] {
            match select(&lines) {
                ToolQuery::Failed(msg) => assert!(
                    msg.contains("none of them an absolute path"),
                    "unexpected message for {lines:?}: {msg}"
                ),
                other => panic!("expected Failed for {lines:?}, got {other:?}"),
            }
        }
    }

    /// Two candidates means the output is genuinely ambiguous, and there is no
    /// honest way to choose. Failing closed here is what keeps the rule from
    /// degrading into "take the first one", which would confidently name the
    /// wrong directory.
    #[test]
    fn two_absolute_paths_fail_rather_than_picking_one() {
        match select(&["/opt/homebrew/lib/node_modules", "/Users/dev/.npm"]) {
            ToolQuery::Failed(msg) => assert!(
                msg.contains("printed 2 absolute paths"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The boundary of "absolute path" the rule relies on: a prose line that
    /// *mentions* a path does not itself start at the root, so it does not
    /// count as a second candidate and does not trigger the ambiguity refusal
    /// above. Without this, the common "warning, see /usr/local/share/doc"
    /// shape would turn a perfectly clear answer into a failed probe.
    #[test]
    fn a_line_merely_mentioning_a_path_is_not_a_candidate() {
        assert_eq!(
            select(&["warning: see /usr/local/share/doc", "/Users/dev/.npm"]),
            ToolQuery::Lines(vec!["/Users/dev/.npm".to_string()])
        );
    }
}
/// Tests for [`SPAWNED_PROGRAMS`] — which foreign programs a discovery pass
/// can start on this machine (HORO-1560).
///
/// Separate from the probe-deadline guards next door: those bound how long a
/// spawn may take, and this one bounds *what* may be spawned. The two answer
/// to different claims — one to responsiveness, one to the honesty of the
/// safety label on `detect`.
#[cfg(test)]
mod spawned_program_tests {
    use super::*;
    use std::path::Path;

    /// HORO-1560: every program a discovery pass can start is named at its
    /// call site through a constant, and every one of those constants is
    /// listed in [`SPAWNED_PROGRAMS`].
    ///
    /// This is what lets the read-only guard test ground the `detect` safety
    /// label by *observation* — it puts a recording stub on `PATH` under each
    /// of these names — instead of by a prose claim about which tools run. A
    /// spawn site that hardcoded its program name would be invisible to that
    /// stub `PATH`, and the label would go back to being unverified.
    #[test]
    fn every_spawned_program_is_declared_once_and_listed() {
        for program in SPAWNED_PROGRAMS {
            assert!(
                !program.is_empty()
                    && program
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{program:?} is not a bare program name, so the read-only \
                 guard cannot create a stub file named after it"
            );
        }
        let mut unique: Vec<&str> = SPAWNED_PROGRAMS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            SPAWNED_PROGRAMS.len(),
            "SPAWNED_PROGRAMS names the same program twice"
        );

        // Split so this test's own source contains none of the needles it
        // searches for.
        let spawn_needles = [
            format!("{}{}", "query_tool_", "lines("),
            format!("{}{}", "query_tool_", "lines_with_deadline("),
            format!("{}{}", "query_tool_", "single_path("),
            format!("{}{}", "Command", "::new("),
        ];
        let declaration = format!("{}{}", "_PROGRAM: ", "&str = \"");

        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut declared: Vec<String> = Vec::new();
        let mut hardcoded: Vec<String> = Vec::new();
        let mut sites = 0usize;
        super::tests::scan_rs_files(&manifest_dir.join("src/detectors"), &mut |path, content| {
            let rel = path
                .strip_prefix(manifest_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let production = match content.split_once("#[cfg(test)]") {
                Some((before, _)) => before,
                None => content,
            };
            for line in production
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
            {
                if let Some(value) = line
                    .split_once(&declaration)
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(value, _)| value.to_string())
                {
                    declared.push(value);
                }
                for needle in &spawn_needles {
                    for (at, _) in line.match_indices(needle.as_str()) {
                        sites += 1;
                        if line[at + needle.len()..].starts_with('"') {
                            hardcoded.push(format!("{rel}: {}", line.trim()));
                        }
                    }
                }
            }
        });

        assert!(
            sites >= SPAWNED_PROGRAMS.len(),
            "found only {sites} spawn sites, fewer than the {} programs \
             SPAWNED_PROGRAMS claims are reachable — the scan is not reading \
             the source it thinks it is",
            SPAWNED_PROGRAMS.len()
        );
        assert!(
            hardcoded.is_empty(),
            "{hardcoded:?} name a spawned program inline. Declare it as a \
             `*_PROGRAM` constant and list it in SPAWNED_PROGRAMS, or the \
             read-only guard cannot observe that program running (HORO-1560)"
        );

        declared.sort();
        declared.dedup();
        assert_eq!(
            declared,
            unique.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            "the programs declared at detector call sites and the programs \
             SPAWNED_PROGRAMS lists have diverged"
        );
    }

    /// HORO-1560 AC 3: every probe that still has to spawn a tool says so at
    /// its call site — why the tool has to be asked, and what the tool does
    /// when asked.
    ///
    /// The point is not tidiness. Once a cache location can be read from a
    /// tool's documented configuration (AC 2), a spawn that remains is a claim
    /// that reading was not enough, and that claim is the thing a reviewer has
    /// to be able to check. Left undocumented, a probe kept out of habit looks
    /// exactly like a probe kept out of necessity.
    ///
    /// Same source-text technique as the guards above, and the same reason: an
    /// AST pass would be a lot of machinery to answer a question about where a
    /// comment sits relative to a call.
    ///
    /// `mod.rs` is excluded because it is where the spawn *helpers* live — the
    /// one `Command::new` in this file is the implementation every call site
    /// goes through, not a call site with a tool of its own to justify.
    #[test]
    fn every_spawn_site_says_why_the_tool_has_to_be_asked() {
        // Split so this test's own source contains neither the needles it
        // searches for nor the marker it looks for.
        let spawn_needles = [
            format!("{}{}", "query_tool_", "lines("),
            format!("{}{}", "query_tool_", "lines_with_deadline("),
            format!("{}{}", "query_tool_", "single_path("),
            format!("{}{}", "Command", "::new("),
        ];
        let marker = format!("{}{}", "has to be ", "asked");

        /// How far above a spawn the explanation may sit. Wide enough for a
        /// helper's doc comment to cover the spawn inside that helper's body,
        /// narrow enough that an unrelated comment elsewhere in the function
        /// cannot vouch for it.
        const WINDOW: usize = 24;

        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut undocumented: Vec<String> = Vec::new();
        let mut files_with_sites: Vec<String> = Vec::new();

        super::tests::scan_rs_files(&manifest_dir.join("src/detectors"), &mut |path, content| {
            let rel = path
                .strip_prefix(manifest_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "src/detectors/mod.rs" {
                return;
            }
            let production = match content.split_once("#[cfg(test)]") {
                Some((before, _)) => before,
                None => content,
            };
            let lines: Vec<&str> = production.lines().collect();
            for (n, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if !spawn_needles
                    .iter()
                    .any(|needle| line.contains(needle.as_str()))
                {
                    continue;
                }
                if !files_with_sites.contains(&rel) {
                    files_with_sites.push(rel.clone());
                }
                let above = &lines[n.saturating_sub(WINDOW)..n];
                if !above.iter().any(|l| l.contains(&marker)) {
                    undocumented.push(format!("{rel}:{}", n + 1));
                }
            }
        });

        assert!(
            undocumented.is_empty(),
            "these probes spawn a tool without saying why it {marker} at the \
             call site: {undocumented:?} — say what the documented route \
             cannot see and what the tool does when asked, or read the \
             location instead of spawning (HORO-1560 AC 2/AC 3)"
        );

        // Non-vacuity: the scan must be reading the source it thinks it is. A
        // detector module that starts spawning arrives here rather than
        // silently outside the guard's attention.
        files_with_sites.sort();
        assert_eq!(
            files_with_sites,
            [
                "src/detectors/docker.rs",
                "src/detectors/docker_objects.rs",
                "src/detectors/go.rs",
                "src/detectors/homebrew.rs",
                "src/detectors/node.rs",
                "src/detectors/python.rs",
            ],
            "the set of detector modules that spawn a tool has changed"
        );
    }
}

/// The route order [`documented_cache_root`] implements, and the cases where
/// it deliberately declines to answer (HORO-1560 AC 2).
#[cfg(test)]
mod documented_route_tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        test_support::make_temp_dir(name)
    }

    /// A documented variable is the strongest statement available about where
    /// a cache is, so it wins over a default directory that also exists.
    #[test]
    fn a_documented_variable_beats_a_documented_default_that_also_exists() {
        let named = temp("route-named");
        let default = temp("route-default");
        let ctx = DiscoveryContext::new("/nonexistent-home")
            .with_tool_env(ToolEnvVar::PipCacheDir, named.to_str().unwrap());

        let (path, route) = documented_cache_root(&ctx, ToolEnvVar::PipCacheDir, Some(default))
            .expect("a variable naming an existing directory answers");

        assert_eq!(path, named);
        assert_eq!(route, CacheRoute::EnvVar("PIP_CACHE_DIR"));
    }

    #[test]
    fn the_documented_default_answers_when_no_variable_is_set() {
        let default = temp("route-default-only");
        let ctx = DiscoveryContext::new("/nonexistent-home");

        let (path, route) =
            documented_cache_root(&ctx, ToolEnvVar::PipCacheDir, Some(default.clone()))
                .expect("an existing default directory answers");

        assert_eq!(path, default);
        assert_eq!(route, CacheRoute::DocumentedDefault);
    }

    /// The rule that keeps this route from costing information. A directory
    /// that is not there says nothing about the resource *or* the tool, and
    /// the tool can still tell those two apart (HORO-1558, HORO-1575). Remove
    /// the existence check and both halves of this test fail.
    #[test]
    fn a_documented_location_that_is_not_there_is_not_an_answer() {
        let parent = temp("route-absent");
        let missing = parent.join("never-written");

        let unset = DiscoveryContext::new("/nonexistent-home");
        assert!(
            documented_cache_root(&unset, ToolEnvVar::PipCacheDir, Some(missing.clone())).is_none(),
            "an absent default directory must fall through to asking the tool"
        );

        let named = unset.with_tool_env(ToolEnvVar::PipCacheDir, missing.to_str().unwrap());
        assert!(
            documented_cache_root(&named, ToolEnvVar::PipCacheDir, None).is_none(),
            "a variable naming an absent directory must fall through too"
        );
    }

    /// A file where a cache directory was promised is not a cache directory,
    /// and reporting its bytes as the cache's would be a wrong answer rather
    /// than a missing one.
    #[test]
    fn a_documented_location_that_is_a_file_is_not_an_answer() {
        let dir = temp("route-file");
        let file = dir.join("not-a-directory");
        fs::write(&file, b"x").expect("write decoy file");
        let ctx = DiscoveryContext::new("/nonexistent-home");

        assert!(documented_cache_root(&ctx, ToolEnvVar::PipCacheDir, Some(file)).is_none());
    }

    /// What a tool resolves a relative cache path against is the tool's own
    /// business, so this code does not get to decide it.
    #[test]
    fn a_relative_or_empty_variable_value_is_not_an_answer() {
        for value in ["", ".cache/pip", "~/Library/Caches/pip"] {
            let ctx = DiscoveryContext::new("/nonexistent-home")
                .with_tool_env(ToolEnvVar::PipCacheDir, value);
            assert!(
                documented_cache_root(&ctx, ToolEnvVar::PipCacheDir, None).is_none(),
                "{value:?} is not an absolute path and must not be treated as one"
            );
        }
    }

    /// Both spellings npm honours, agreeing. One answer, and the spelling
    /// that was actually set is the one recorded.
    #[test]
    fn two_documented_spellings_that_agree_answer_once() {
        let cache = temp("route-npm-agree");
        let value = cache.to_str().unwrap();
        let ctx = DiscoveryContext::new("/nonexistent-home")
            .with_tool_env_spelling(ToolEnvVar::NpmCache, "npm_config_cache", value)
            .with_tool_env_spelling(ToolEnvVar::NpmCache, "NPM_CONFIG_CACHE", value);

        let (path, route) = documented_cache_root(&ctx, ToolEnvVar::NpmCache, None)
            .expect("agreeing spellings answer");

        assert_eq!(path, cache);
        assert_eq!(route, CacheRoute::EnvVar("npm_config_cache"));
    }

    /// Disagreeing spellings are the one genuinely ambiguous case. npm's
    /// precedence between them is npm's rule, not something to guess at, so
    /// nothing is reported — and, in particular, the documented *default* is
    /// not reported either, because npm is demonstrably using neither.
    #[test]
    fn two_documented_spellings_that_disagree_answer_nothing() {
        let a = temp("route-npm-a");
        let b = temp("route-npm-b");
        let default = temp("route-npm-default");
        let ctx = DiscoveryContext::new("/nonexistent-home")
            .with_tool_env_spelling(
                ToolEnvVar::NpmCache,
                "npm_config_cache",
                a.to_str().unwrap(),
            )
            .with_tool_env_spelling(
                ToolEnvVar::NpmCache,
                "NPM_CONFIG_CACHE",
                b.to_str().unwrap(),
            );

        assert!(ctx.tool_env(ToolEnvVar::NpmCache).is_none());
        assert!(
            documented_cache_root(&ctx, ToolEnvVar::NpmCache, Some(default)).is_none(),
            "an ambiguous configuration must reach npm itself, not the default path"
        );
    }

    /// Two variables reading the same environment name would make one tool's
    /// configuration answer for another's.
    #[test]
    fn every_documented_spelling_belongs_to_exactly_one_variable() {
        let mut seen: Vec<&str> = Vec::new();
        for var in ToolEnvVar::ALL {
            assert!(
                !var.names().is_empty(),
                "{var:?} documents no spelling, so nothing can ever read it"
            );
            assert_eq!(var.name(), var.names()[0], "{var:?}");
            for name in var.names() {
                assert!(
                    !seen.contains(name),
                    "{name} is claimed by more than one ToolEnvVar"
                );
                seen.push(name);
            }
        }
        assert!(
            seen.len() > ToolEnvVar::ALL.len(),
            "no variable has a second spelling, so the slice-shaped names() is untested"
        );
    }

    /// The provenance sentence says the tool was not run. That sentence is
    /// carried out to `detect --json`, so a reader can tell a measurement
    /// reached without the tool from one the tool pointed at.
    #[test]
    fn the_provenance_records_which_route_answered() {
        assert_eq!(
            CacheRoute::EnvVar("PIP_CACHE_DIR").provenance("pip's cache"),
            "pip's cache, at the directory PIP_CACHE_DIR names — the tool was not run"
        );
        assert_eq!(
            CacheRoute::DocumentedDefault.provenance("pip's cache"),
            "pip's cache, at the tool's documented default location — the tool was not run"
        );
    }
}
