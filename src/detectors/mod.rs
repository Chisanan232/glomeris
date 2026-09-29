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
#[derive(Debug, PartialEq)]
pub enum DetectorStatus {
    Found(Vec<Evidence>),
    ToolAbsent,
    Failed(String),
}

/// An environment variable that relocates one tool's home directory, and so
/// decides where that tool's cache actually lives.
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
pub enum ToolHomeVar {
    CargoHome,
    GradleUserHome,
}

impl ToolHomeVar {
    /// Declaration order is the iteration order of
    /// [`DiscoveryContext::from_process_env`].
    pub const ALL: [ToolHomeVar; 2] = [ToolHomeVar::CargoHome, ToolHomeVar::GradleUserHome];

    pub fn name(self) -> &'static str {
        match self {
            ToolHomeVar::CargoHome => "CARGO_HOME",
            ToolHomeVar::GradleUserHome => "GRADLE_USER_HOME",
        }
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
    /// Tool-home overrides, as captured at the process boundary. Empty
    /// unless a caller supplied them, which is what makes
    /// [`DiscoveryContext::new`] hermetic — see [`ToolHomeVar`].
    tool_homes: Vec<(ToolHomeVar, String)>,
}

impl DiscoveryContext {
    /// A context that knows nothing about the process environment: every
    /// [`ToolHomeVar`] reads as unset, so a detector resolves its tool home
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
            tool_homes: Vec::new(),
        }
    }

    /// `new`, plus every [`ToolHomeVar`] this process actually has set. The
    /// constructor production code uses.
    pub fn from_process_env(home_dir: impl Into<PathBuf>) -> Self {
        let mut ctx = Self::new(home_dir);
        for var in ToolHomeVar::ALL {
            if let Ok(value) = std::env::var(var.name()) {
                ctx.tool_homes.push((var, value));
            }
        }
        ctx
    }

    pub fn with_known_project_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.known_project_roots = roots;
        self
    }

    /// Sets one tool home explicitly, for tests that need to exercise the
    /// override branch without touching the process environment
    /// (`std::env::set_var` is unsound in Rust's threaded test harness).
    pub fn with_tool_home(mut self, var: ToolHomeVar, value: impl Into<String>) -> Self {
        self.tool_homes.retain(|(existing, _)| *existing != var);
        self.tool_homes.push((var, value.into()));
        self
    }

    /// The override for `var`, or `None` when it is unset. `None` means
    /// "nothing said otherwise", never "the tool is absent".
    pub fn tool_home(&self, var: ToolHomeVar) -> Option<&str> {
        self.tool_homes
            .iter()
            .find(|(existing, _)| *existing == var)
            .map(|(_, value)| value.as_str())
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
/// The two are separate because `tool_absent` is documented, all the way
/// out to `GlomerisDtos.swift`'s `DetectorHealthReportDto`, as "the tool
/// that would produce candidates is not installed" — a claim that is
/// evidence-backed in one case and knowably false in the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RootAbsence {
    /// The path came from running the tool, so the tool is installed: it
    /// answered. A missing directory then means only that the tool has not
    /// written its cache yet — `Found(vec![])`, never `ToolAbsent`.
    ToolAnsweredWithAPathItHasNotWritten,
    /// The path was inferred from `$HOME` or an environment variable and no
    /// tool was ever run, so the directory's absence is the only evidence
    /// available about the tool at all. This is the existing
    /// `xcode`/`cargo`/`node` convention and maps to `ToolAbsent`.
    NothingObservedAboutTheTool,
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
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten => DetectorStatus::Found(Vec::new()),
            RootAbsence::NothingObservedAboutTheTool => DetectorStatus::ToolAbsent,
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
        /// `DockerImageCache` is HORO-1544's: images, build cache and volumes
        /// are distinct lifecycle evidence, and collapsing them into the
        /// existing build-cache detector merely to satisfy this guard is
        /// exactly what that ticket forbids.
        const NOT_YET_DETECTABLE: &[ResourceKind] = &[ResourceKind::DockerImageCache];

        // Pinned, so this exemption cannot quietly grow. A kind added here
        // without its ticket, or one left behind after its detector landed,
        // both fail on this line.
        assert_eq!(
            NOT_YET_DETECTABLE,
            &[ResourceKind::DockerImageCache],
            "the not-yet-detectable exemption changed — add the ticket that \
             owns the new kind to this test's doc comment, or remove a kind \
             whose detector now exists"
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
            ("src/detectors/mod.rs", 2),
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
    /// [`ToolHomeVar`] plus [`DiscoveryContext::from_process_env`] is the
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
             DiscoveryContext as a ToolHomeVar instead (HORO-1543)"
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
