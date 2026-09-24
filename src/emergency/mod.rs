//! Emergency recovery mode (HORO-953): a DEGRADED product path that must
//! run and produce a useful result even when SQLite/history/log writes
//! fail, there is no network, there is no LLM provider configured, and
//! there is no GUI. This is not `free --target` with a flag — see this
//! ticket's PR description "Known limitations" for what "useful result"
//! honestly means today.
//!
//! Canonical safety invariant (unchanged here): AI can recommend. Policy
//! decides. Executor verifies. Filesystem reality wins. Emergency
//! pressure is NOT permission to weaken that invariant — this module
//! reuses [`crate::policy::classify`]/[`crate::policy::approval::authorize`] and
//! [`crate::executor::execute`] exactly as-is, and never references
//! anything network- or LLM-related. There is nothing in this file's
//! production code (the part above its own test module) for the
//! `module_never_references_network_or_llm_types` test below to find.
//!
//! ## Deliberate MVP scope decisions
//!
//! - No interactive `Ask` handling. A degraded, no-time,
//!   no-mechanism-for-consent path can only auto-execute
//!   [`crate::policy::PolicyClass::AutoSafe`] candidates; everything else
//!   (`Ask`, `Protected`) is correctly refused and counted in
//!   [`EmergencyReport::denied_candidates`], never escalated to
//!   interactive consent. This is a deliberate MVP choice, not an
//!   oversight.
//! - Bounded, cheap discovery only: candidates come from
//!   [`crate::detectors::DetectorRegistry::discover_all`] (already
//!   bounded/shallow), never from the scanner's full, still-potentially-
//!   slow filesystem walk.
//!
//! ## Known limitations
//!
//! - **Preallocated emergency reserve file: deferred, not implemented.**
//!   The originating ticket names a "preallocated emergency reserve
//!   file" as an experimental/optional idea and is explicit that it
//!   should be rejected or deferred without reliable measured evidence —
//!   and that building that evidence is itself out of scope for this
//!   pass. This module does not implement it.
//! - ~~**The candidate loop still frees nothing via a real
//!   detector-produced candidate today.**~~ Resolved upstream by HORO-994,
//!   with no change needed here, exactly as this bullet predicted:
//!   `executor::build_fresh_evidence` now reuses the same bounded size
//!   estimate for `reclaimable_bytes` that it already used for
//!   `logical_bytes`, instead of hardcoding that field back to
//!   `Unavailable(NotAttempted)` and so aborting every real `AutoSafe`
//!   approval inside deletion-time revalidation. The candidate loop
//!   therefore reclaims real bytes today, and the
//!   self-owned-disposable-state step (step 1) is no longer the only path
//!   that does. Proven end to end against a disposable fixture by
//!   `tests/golden_chain_execute.rs`.
//! - **Near-zero-real-disk-space testing was not performed.** Actually
//!   driving a test machine's free space to near zero is destructive to
//!   that machine and was judged not worth the risk for this pass. Fault
//!   injection via fakes (`AlwaysFailingPersistence`, an always-failing
//!   `EvidenceCollector`, a nonexistent self-state path, …) exercises the
//!   equivalent failure modes without that risk.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::actions::ActionRegistry;
use crate::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Evidence, ResourceKind};
use crate::evidence::probe::ProbeOutcome;
use crate::executor::{execute, ExecutionOutcome, ExecutionReport};
use crate::monitor::fs_stat::FsStat;
use crate::monitor::persistence::{
    append_audit_record, unix_now_secs, ActionSource, AuditRecord, PersistenceBackend,
    PressureEvent,
};
use crate::monitor::pressure::PressureState;
use crate::policy::approval::authorize;
use crate::policy::{classify, PolicyClass, PolicyConfig};
use crate::reporting::policy_label::label_for;

/// Per-`collect()` probe timeout used while revalidating one emergency
/// candidate. Kept short — unlike [`crate::executor`]'s own internal 5s
/// revalidation budget (not configurable from here), emergency mode's
/// whole point is bounded, fast operation under pressure. A single
/// `collect()` call may still spend up to roughly `timeout * 4` wall time
/// in the worst case (see [`crate::evidence::correlate`]'s own docs) —
/// accepted as a known bound, not eliminated, given a caller-enforced
/// wall-clock deadline around the whole candidate loop regardless.
const CANDIDATE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound on how many error strings [`EmergencyReport`] retains — mirrors
/// [`crate::evidence::model::Evidence::push_source`]'s bounded-provenance
/// convention: this report must stay printable/bounded even in a
/// pathological run where nearly everything fails.
const MAX_ERRORS: usize = 8;

/// Result of one `glomeris emergency` run. Every field is a plain,
/// dependency-free primitive (`u32`/`u64`/`String`) so this stays
/// printable even if every other subsystem in the process has failed —
/// see the `Display` impl below. Deliberately has no `serde` impl (not
/// needed for this ticket's scope, per its own instructions).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmergencyReport {
    pub actions_attempted: u32,
    pub actions_succeeded: u32,
    pub total_bytes_freed: u64,
    /// Protected/ambiguous candidates correctly refused — i.e. every
    /// candidate whose [`crate::policy::classify`] result was not
    /// `AutoSafe`. Never incremented for an unrelated wiring gap (e.g. no
    /// registered action for an `AutoSafe` kind) — those go to `errors`
    /// instead, so this field stays a clean read of "policy said no".
    pub denied_candidates: u32,
    /// Non-fatal errors encountered along the way (e.g. a persistence
    /// write failure) — reported, never fatal. Bounded internally so an
    /// adversarial run can't grow this field unboundedly.
    pub errors: Vec<String>,
}

impl EmergencyReport {
    /// Push a non-fatal error message, silently dropping it once `errors`
    /// has reached [`MAX_ERRORS`] — errors are advisory context for a
    /// human reading the report, not a field a caller should rely on
    /// being exhaustive.
    fn push_error(&mut self, message: impl Into<String>) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(message.into());
        }
    }
}

impl std::fmt::Display for EmergencyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "glomeris emergency report:")?;
        writeln!(f, "  actions attempted: {}", self.actions_attempted)?;
        writeln!(f, "  actions succeeded: {}", self.actions_succeeded)?;
        writeln!(f, "  bytes freed:       {}", self.total_bytes_freed)?;
        writeln!(f, "  candidates denied: {}", self.denied_candidates)?;
        if self.errors.is_empty() {
            writeln!(f, "  errors:            none")
        } else {
            writeln!(f, "  errors ({}):", self.errors.len())?;
            for err in &self.errors {
                writeln!(f, "    - {err}")?;
            }
            Ok(())
        }
    }
}

/// Frees the tool's own disposable state — today, the best-effort
/// pressure-history file [`crate::monitor::persistence::FilePersistence`]
/// appends to (see `main.rs`'s `daemon_run` for the real, production
/// path). This runs FIRST, unconditionally, and is never gated on
/// [`crate::policy::classify`]: it is not a developer resource under
/// policy's purview, it is this tool's own append-only log, and deleting
/// it is safe by construction — the worst outcome is losing pressure
/// history, never a build artifact or a developer's data.
///
/// A missing file is not an error — this function silently does nothing
/// rather than fabricating work, exactly as this ticket asks: "if no such
/// self-owned disposable state exists/it's already minimal, just move
/// on, don't fabricate work."
fn free_self_owned_disposable_state(path: &Path, report: &mut EmergencyReport) {
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            report.push_error(format!(
                "failed to stat self-owned disposable state {}: {e}",
                path.display()
            ));
            return;
        }
    };

    let size = metadata.len();
    report.actions_attempted += 1;
    match fs::remove_file(path) {
        Ok(()) => {
            report.actions_succeeded += 1;
            report.total_bytes_freed += size;
        }
        Err(e) => report.push_error(format!(
            "failed to remove self-owned disposable state {}: {e}",
            path.display()
        )),
    }
}

/// Runs emergency recovery end to end. See the module docs for the
/// product requirement this exists to satisfy: it must run and produce a
/// useful result even when SQLite/history/log writes fail, there's no
/// network, no LLM provider, and no GUI.
///
/// Signature note: this deviates from a hypothetical minimal signature in
/// two ways, both required to make every failure mode this ticket must
/// fault-inject actually injectable from a test, per this crate's "fully
/// unit-testable with fakes" constraint:
/// - `persistence`/`discovery_ctx`/`self_state_path` are explicit
///   parameters rather than resolved internally from `$HOME` the way
///   `main.rs`'s `daemon_run`/`run_detect_command` do for the real binary
///   — a test must never depend on, or mutate, the developer's real home
///   directory (this repo's `CLAUDE.md`: "never depend on the developer's
///   real home directory contents in a test"). The real CLI wiring in
///   `main.rs` computes these the same way `daemon_run` does and passes
///   them in.
/// - `#[allow(clippy::too_many_arguments)]`: ten parameters, every one
///   an independently fakeable seam — bundling them into a config struct
///   would only rename this list, not shrink it. Precedented in this
///   crate at `detectors::discovery_evidence`. `audit_log_path`
///   (HORO-1057) is one more such seam: a plain `&Path`, matching
///   `self_state_path`'s own shape, rather than an injected backend.
#[allow(clippy::too_many_arguments)]
pub fn run_emergency(
    fs_stat: &dyn FsStat,
    collector: &dyn EvidenceCollector,
    registry: &DetectorRegistry,
    actions: &ActionRegistry,
    persistence: &dyn PersistenceBackend,
    discovery_ctx: &DiscoveryContext,
    self_state_path: &Path,
    max_actions: u32,
    max_duration: Duration,
    audit_log_path: &Path,
) -> EmergencyReport {
    let start = Instant::now();
    let now = SystemTime::now();
    let mut report = EmergencyReport::default();

    // Step 1: free the tool's own disposable state first — the safest
    // possible thing to reclaim, and the only step here that does not
    // depend on detectors/policy/executor at all.
    free_self_owned_disposable_state(self_state_path, &mut report);

    // Step 2: bounded, cheap candidate discovery — detectors only, never
    // the scanner's full filesystem walk (see module docs).
    let mut candidates: Vec<Evidence> = Vec::new();
    for (_detector_id, status) in registry.discover_all(discovery_ctx) {
        match status {
            DetectorStatus::Found(evidences) => candidates.extend(evidences),
            // A detector's tool being absent is normal, expected state,
            // never an error (see `crate::detectors` module docs).
            DetectorStatus::ToolAbsent => {}
            DetectorStatus::Failed(message) => {
                report.push_error(format!("detector failed: {message}"));
            }
        }
    }

    process_candidates(
        candidates,
        collector,
        actions,
        now,
        max_actions,
        start,
        max_duration,
        &mut report,
        audit_log_path,
    );

    // Step 3: best-effort persistence, recorded LAST, deliberately AFTER
    // step 1's deletion. In production `self_state_path` and this
    // record's destination are the same file (see `main.rs`): step 1
    // reclaims whatever bulk history had accumulated, and this step then
    // appends exactly one small fresh line documenting the run — a
    // deliberate net-positive trade (`FilePersistence::record` recreates
    // the file via `create_dir_all` + append either way; ordering it
    // last just means the "before" byte count step 1 reports reflects
    // the accumulated history, not this run's own single-line record).
    // Any failure here is caught and pushed into `report.errors`, never
    // propagated as a panic or early-return — see `record_emergency_run`.
    record_emergency_run(fs_stat, persistence, &mut report);

    report
}

/// Best-effort: records that emergency mode ran, via the exact same
/// [`crate::monitor::persistence::PersistenceBackend`] contract the
/// healthy polling loop uses. ANY failure here — including one
/// manufactured by a fake backend in a test, or a failure to even read
/// filesystem usage via `fs_stat` — is caught and pushed into
/// `report.errors`. It is NEVER allowed to panic or make this function
/// (or its caller) return early: no feature in this crate may depend on
/// persistence succeeding.
fn record_emergency_run(
    fs_stat: &dyn FsStat,
    persistence: &dyn PersistenceBackend,
    report: &mut EmergencyReport,
) {
    let usage = match fs_stat.stat(Path::new("/")) {
        Ok(u) => u,
        Err(e) => {
            report.push_error(format!(
                "failed to read filesystem usage for emergency history record: {e}"
            ));
            return;
        }
    };

    let event = PressureEvent {
        unix_time_secs: unix_now_secs(),
        from: PressureState::Emergency,
        to: PressureState::Emergency,
        used_percent: usage.used_percent(),
        free_bytes: usage.free_bytes,
    };

    if let Err(e) = persistence.record(&event) {
        report.push_error(format!("failed to persist emergency run record: {e}"));
    }
}

/// Builds an [`AuditRecord`] (HORO-1057, `source: "emergency"`) from a
/// real [`ExecutionReport`] and appends it to `audit_log_path`,
/// unconditionally discarding the `Result` — matching
/// [`crate::monitor::persistence::append_audit_record`]'s best-effort
/// contract, same reasoning as [`record_emergency_run`] above: an
/// audit-write failure must never influence this module's own report
/// bookkeeping. Never called for [`ExecutionOutcome::DryRun`]'s tag,
/// since `execute()` never actually produces that variant (see this
/// module's own comment on that arm below).
fn append_emergency_audit_record(
    report: &ExecutionReport,
    policy_label: &'static str,
    audit_log_path: &Path,
    now: SystemTime,
) {
    let (outcome, abort_reason) = match &report.outcome {
        ExecutionOutcome::Succeeded => ("succeeded", None),
        ExecutionOutcome::Failed(_) => ("failed", None),
        ExecutionOutcome::AbortedByRevalidation(reason) => {
            ("aborted_by_revalidation", Some(format!("{reason:?}")))
        }
        ExecutionOutcome::DryRun => ("dry_run", None),
    };
    let record = AuditRecord {
        timestamp: now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        action_id: report.action.0.to_string(),
        resource_id: report.resource.to_string(),
        policy_label: policy_label.to_string(),
        outcome: outcome.to_string(),
        abort_reason,
        actual_reclaimed_bytes: report.actual_reclaimed_bytes.observed().copied(),
        source: ActionSource::Emergency.to_string(),
        // Emergency mode never calls a model (see this module's docs).
        model_rank: None,
    };
    let _ = append_audit_record(audit_log_path, &record);
}

/// Classifies and (only if `AutoSafe`) executes each candidate in
/// `evidences`, bounded by `max_actions` and `max_duration` (measured
/// from `loop_start`). One candidate's failure — at classification,
/// authorization, or execution — never stops the loop from reaching the
/// next candidate.
#[allow(clippy::too_many_arguments)]
fn process_candidates(
    evidences: Vec<Evidence>,
    collector: &dyn EvidenceCollector,
    actions: &ActionRegistry,
    now: SystemTime,
    max_actions: u32,
    loop_start: Instant,
    max_duration: Duration,
    report: &mut EmergencyReport,
    audit_log_path: &Path,
) {
    for evidence in evidences {
        if report.actions_attempted >= max_actions {
            break;
        }
        if loop_start.elapsed() >= max_duration {
            break;
        }
        process_candidate(evidence, collector, actions, now, report, audit_log_path);
    }
}

/// Classifies one detected candidate via the exact same
/// [`crate::policy::classify`] every other path in this crate uses, and
/// executes it via [`crate::executor::execute`] ONLY when that
/// classification is [`PolicyClass::AutoSafe`]. Never handles `Ask` —
/// there is no interactive consent mechanism in a degraded path (see
/// module docs) — and never executes `Protected`, which
/// [`crate::policy::approval::authorize`] refuses unconditionally anyway. This is a
/// deliberate MVP choice, not an oversight: emergency pressure is not
/// permission to weaken policy.
fn process_candidate(
    mut evidence: Evidence,
    collector: &dyn EvidenceCollector,
    actions: &ActionRegistry,
    now: SystemTime,
    report: &mut EmergencyReport,
    audit_log_path: &Path,
) {
    let correlation = collector.collect(
        &evidence.resource,
        ProbeBudget {
            timeout: CANDIDATE_PROBE_TIMEOUT,
        },
    );
    merge_into(&mut evidence, correlation);
    // Stamp with the freshly-correlated timestamp before classifying, same
    // as `executor::build_fresh_evidence` does — this is evidence we just
    // observed, so it is never stale by definition. It does NOT bypass
    // `classify`'s staleness gate for anything else: only this just-probed
    // snapshot gets the new timestamp, and every other check (Protected,
    // completeness, active-use, regenerability) still runs unmodified.
    evidence.collected_at = now;

    let cfg = PolicyConfig::default();
    let decision = classify(&evidence, &cfg, now);

    if decision.class != PolicyClass::AutoSafe {
        report.denied_candidates += 1;
        return;
    }

    let Some(action_id) = action_id_for_kind(evidence.resource.kind) else {
        // Not a policy denial — a wiring gap (an `AutoSafe` kind with no
        // registered cleanup action). Reported, never silently dropped,
        // but kept out of `denied_candidates` so that field stays a clean
        // read of "policy said no".
        report.push_error(format!(
            "no registered action for AutoSafe candidate {}",
            evidence.resource
        ));
        return;
    };
    let Some(action) = actions.get(action_id) else {
        report.push_error(format!(
            "action id {action_id} not found in registry for {}",
            evidence.resource
        ));
        return;
    };

    // An action being mapped to the kind is not the same claim as it being
    // runnable against THIS resource (HORO-1359). `action_id_for_kind` maps
    // `HomebrewCache` to `homebrew.cleanup.cache`, whose step carries no
    // scoped path, so `execute` refused it every time — and this function
    // had already charged `actions_attempted` for it by then. In a degraded
    // low-disk path that is one of very few attempts the run is allowed,
    // spent on an outcome that was certain in advance.
    //
    // Asked here rather than by narrowing the map: the map answers "which
    // action cleans this kind", which is a true and useful statement, and a
    // kind whose only action is unrunnable today may become runnable
    // without the map changing. Per this ticket's AC4 either form is
    // acceptable, and filtering at the caller keeps the two facts separate.
    //
    // Safe to plan at this point, and only at this point: the `AutoSafe`
    // check above has already returned for anything Protected, so this may
    // use the policy-free half of the predicate.
    if let Some(reason) = crate::actionability::plan_refusal(action, &evidence) {
        // Not `denied_candidates` — policy said yes. Reported as an error so
        // it is never silently dropped, and worded as an action that was
        // never eligible rather than one that failed, because the two mean
        // different things to whoever reads this report afterwards.
        report.push_error(format!(
            "action {action_id} is not eligible for {}: {reason}",
            evidence.resource
        ));
        return;
    }

    let fingerprint = evidence.fingerprint.clone();
    // Captured before `decision` is moved into `authorize` below —
    // HORO-1057's audit record needs the report-facing label for the
    // decision this candidate was actually authorized under.
    let policy_label = label_for(&decision).as_str();
    let Some(approval) = authorize(decision, fingerprint, None) else {
        // `authorize` never refuses an `AutoSafe` decision per its own
        // contract — defensive, not currently reachable.
        report.denied_candidates += 1;
        return;
    };

    report.actions_attempted += 1;
    let exec_report = execute(action, &approval, collector, &cfg, now);
    // HORO-1057: best-effort audit-log append, AFTER the real outcome
    // above is already known. `append_emergency_audit_record` never
    // returns a `Result` its caller could (mis)handle — see that
    // function's doc comment for why an audit-write failure must never
    // influence this module's own bookkeeping below.
    append_emergency_audit_record(&exec_report, policy_label, audit_log_path, now);
    match exec_report.outcome {
        ExecutionOutcome::Succeeded => {
            report.actions_succeeded += 1;
            if let ProbeOutcome::Observed(bytes) = exec_report.actual_reclaimed_bytes {
                report.total_bytes_freed += bytes;
            }
        }
        ExecutionOutcome::Failed(message) => {
            report.push_error(format!("action {action_id} failed: {message}"));
        }
        ExecutionOutcome::AbortedByRevalidation(reason) => {
            // See this module's own "Known limitations" (updated by
            // HORO-992): `execute`'s own deletion-time revalidation
            // rebuilds evidence via `executor::build_fresh_evidence`,
            // which does not reuse a detector's `reclaimable_bytes`
            // estimate and always reports it `Unavailable(NotAttempted)`,
            // so even a genuinely `AutoSafe` approval is expected to
            // abort here with `PolicyClassDowngraded` — a pre-existing
            // upstream gap in `executor`, not something this module
            // papers over.
            report.push_error(format!(
                "action {action_id} aborted by revalidation: {reason:?}"
            ));
        }
        ExecutionOutcome::DryRun => {
            // `execute()` never actually produces this variant in
            // today's implementation (only `dry_run()`'s separate return
            // type does) — handled explicitly instead of matched
            // unsafely, per this module's no-panic constraint.
            report.push_error(format!(
                "action {action_id} unexpectedly returned DryRun from execute()"
            ));
        }
    }
}

/// Static, closed mapping from a resource kind to the one pre-registered
/// [`crate::actions::ActionId`] string that cleans it up. Deliberately
/// NOT sourced from `Evidence::native_cleanup` — no detector in this
/// crate populates `NativeCleanup::Available` today (every built-in
/// detector emits `NativeCleanup::Unsupported` unconditionally), so that
/// field cannot be relied on yet. This mapping only reads
/// `ActionRegistry::get`'s existing public API — it does not add or
/// change anything in `crate::actions`.
fn action_id_for_kind(kind: ResourceKind) -> Option<&'static str> {
    match kind {
        ResourceKind::CargoTargetDir => Some("cargo.clean.target_dir"),
        ResourceKind::NodeModules => Some("node.clean.node_modules"),
        ResourceKind::HomebrewCache => Some("homebrew.cleanup.cache"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    use crate::detectors::dev_ino_fingerprint;
    use crate::detectors::probe_mtime;
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        NativeCleanup, Recoverability, ResourceFingerprint, ResourceId, ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use crate::monitor::fs_stat::FsUsage;

    /// A fixed-reading `FsStat` fake — never touches the real filesystem.
    struct FakeFsStat(FsUsage);
    impl FsStat for FakeFsStat {
        fn stat(&self, _path: &Path) -> io::Result<FsUsage> {
            Ok(self.0)
        }
    }

    /// A backend that always fails a plain write — simulates a genuine
    /// persistence I/O failure (e.g. disk full, permission denied).
    struct AlwaysFailingPersistence;
    impl PersistenceBackend for AlwaysFailingPersistence {
        fn record(&self, _event: &PressureEvent) -> io::Result<()> {
            Err(io::Error::other("simulated persistence write failure"))
        }
    }

    /// A backend that always fails specifically at the temp/log
    /// directory-creation step `FilePersistence::record` performs before
    /// writing — a distinct simulated cause from
    /// `AlwaysFailingPersistence`, exercising the same "any persistence
    /// call is wrapped" contract from a different failure origin.
    struct AlwaysFailingDirCreationPersistence;
    impl PersistenceBackend for AlwaysFailingDirCreationPersistence {
        fn record(&self, _event: &PressureEvent) -> io::Result<()> {
            Err(io::Error::other(
                "simulated failure creating history log directory",
            ))
        }
    }

    /// A collector reporting a fully clean, non-active resource on every
    /// call: empty process lists, no git repo, tool not live. Used where
    /// a test wants correlation to fill in cleanly so it can isolate a
    /// different variable (e.g. `reclaimable_bytes`'s missing-ness).
    struct CleanCollector;
    impl EvidenceCollector for CleanCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    /// A collector whose every correlation probe reports `Unavailable` —
    /// simulates a total process/liveness-probe outage (e.g. `lsof`/
    /// `git`/`pgrep` all missing or timing out).
    struct AllProbesUnavailableCollector;
    impl EvidenceCollector for AllProbesUnavailableCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Unavailable(ProbeReason::Failed),
                process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::Failed),
                git_state: ProbeOutcome::Unavailable(ProbeReason::Failed),
                tool_liveness: ProbeOutcome::Unavailable(ProbeReason::Failed),
            }
        }
    }

    /// Builds an [`Evidence`] that is `Completeness::Complete` and clean
    /// (no active-use signals) for `path`/`kind` as of `collected_at` —
    /// i.e. exactly the shape `policy::classify` maps to `AutoSafe`.
    /// `reclaimable_bytes` is `Observed` here as a hand-built stand-in for
    /// whatever a real detector would report — this fixture exists to
    /// prove `process_candidate`'s AutoSafe-only wiring in isolation, not
    /// to exercise a real detector (see `tests/reclaimable_bytes_reaches_auto_safe.rs`
    /// at the crate root for that proof).
    fn autosafe_evidence(path: PathBuf, kind: ResourceKind, collected_at: SystemTime) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(path.clone())),
            fingerprint: ResourceFingerprint {
                dev_ino: dev_ino_fingerprint(&path),
                mtime: probe_mtime(&path).observed().copied(),
                tool_revision: None,
            },
            detector: crate::detectors::DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(4096),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(collected_at),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at,
            sources: Vec::new(),
        }
    }

    /// `process_candidates` delegates each evidence to `process_candidate`
    /// in order — proven here with a single candidate; the "one
    /// candidate's failure doesn't stop the loop" multi-candidate scenario
    /// is its own dedicated test (see `process_candidates_continues_after_one_candidate_aborts`).
    #[test]
    fn process_candidates_delegates_a_single_candidate_to_process_candidate() {
        let dir = make_temp_dir("candidates-single");
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();
        let now = SystemTime::now();

        let mut evidence = autosafe_evidence(target, ResourceKind::CargoTargetDir, now);
        evidence.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);

        let mut report = EmergencyReport::default();
        process_candidates(
            vec![evidence],
            &CleanCollector,
            &ActionRegistry::builtin(),
            now,
            u32::MAX,
            Instant::now(),
            Duration::from_secs(30),
            &mut report,
            &dir.join("actions.jsonl"),
        );

        assert_eq!(report.denied_candidates, 1);
        assert_eq!(report.actions_attempted, 0);

        fs::remove_dir_all(&dir).ok();
    }

    /// Candidate iteration reuses `policy::classify` exactly as normal: an
    /// incomplete-evidence candidate (e.g. `reclaimable_bytes` missing —
    /// the shape a detector still reports whenever its own size probe
    /// fails) is correctly denied, never executed.
    #[test]
    fn process_candidate_denies_incomplete_evidence_without_executing() {
        let dir = make_temp_dir("candidate-denied");
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();
        let now = SystemTime::now();

        let mut evidence = autosafe_evidence(target, ResourceKind::CargoTargetDir, now);
        evidence.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);

        let mut report = EmergencyReport::default();
        process_candidate(
            evidence,
            &CleanCollector,
            &ActionRegistry::builtin(),
            now,
            &mut report,
            &dir.join("actions.jsonl"),
        );

        assert_eq!(report.denied_candidates, 1);
        assert_eq!(report.actions_attempted, 0);
        assert_eq!(report.actions_succeeded, 0);

        fs::remove_dir_all(&dir).ok();
    }

    /// Process/liveness-probe-failure fault injection: even a candidate
    /// that is otherwise `Complete` (would reach `AutoSafe`) must be
    /// correctly denied — never silently treated as `AutoSafe` — once
    /// every correlation probe reports `Unavailable`.
    #[test]
    fn process_candidate_denies_when_all_correlation_probes_are_unavailable() {
        let dir = make_temp_dir("candidate-probes-unavailable");
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();
        let now = SystemTime::now();

        let evidence = autosafe_evidence(target, ResourceKind::CargoTargetDir, now);
        let mut report = EmergencyReport::default();
        process_candidate(
            evidence,
            &AllProbesUnavailableCollector,
            &ActionRegistry::builtin(),
            now,
            &mut report,
            &dir.join("actions.jsonl"),
        );

        assert_eq!(report.denied_candidates, 1);
        assert_eq!(report.actions_attempted, 0);
        assert_eq!(report.actions_succeeded, 0);

        fs::remove_dir_all(&dir).ok();
    }

    /// A genuinely `AutoSafe`-classified candidate IS authorized and
    /// handed to `executor::execute` — reusing policy/executor exactly as
    /// normal, no parallel/looser path. Before HORO-994, `execute()`'s own
    /// deletion-time revalidation rebuilt evidence via
    /// `executor::build_fresh_evidence`, which did not reuse a detector's
    /// `reclaimable_bytes` estimate, so it was lost again at that step and
    /// the fresh classification downgraded to `Ask`, aborting the
    /// execution unconditionally — see this module's own "Known
    /// limitations" (as it read before HORO-994). Post-HORO-994,
    /// `build_fresh_evidence` reproduces the same `reclaimable_bytes`
    /// estimate a detector would, so the fresh classification agrees with
    /// the plan-time one and this genuinely `AutoSafe`, fully-owned,
    /// regenerable `node_modules` fixture is actually deleted for real —
    /// this is the fix working as intended, not a regression.
    #[test]
    fn process_candidate_attempts_autosafe_and_succeeds() {
        let dir = make_temp_dir("candidate-autosafe-succeeds");
        let node_modules = dir.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("pkg.js"), vec![0u8; 64]).unwrap();
        let now = SystemTime::now();

        let evidence = autosafe_evidence(node_modules.clone(), ResourceKind::NodeModules, now);
        let mut report = EmergencyReport::default();
        let audit_log_path = dir.join("actions.jsonl");
        process_candidate(
            evidence,
            &CleanCollector,
            &ActionRegistry::builtin(),
            now,
            &mut report,
            &audit_log_path,
        );

        assert_eq!(report.actions_attempted, 1);
        assert_eq!(report.actions_succeeded, 1);
        assert_eq!(report.denied_candidates, 0);
        assert!(report.errors.is_empty());
        // The real fixture was actually deleted.
        assert!(!node_modules.exists());

        // HORO-1057 AC: `process_candidate` appends an audit record after
        // the real outcome is known.
        let audit_tail = crate::monitor::read_audit_tail(&audit_log_path, 10);
        assert_eq!(audit_tail.len(), 1, "expected exactly one audit record");
        assert_eq!(audit_tail[0].source, "emergency");
        assert_eq!(audit_tail[0].outcome, "succeeded");
        assert_eq!(audit_tail[0].policy_label, "AUTO_SAFE");

        fs::remove_dir_all(&dir).ok();
    }

    /// Matches the executor/policy test suites' own helper shape — a
    /// fresh, per-test temp directory, never the developer's real home
    /// directory (see this repo's `CLAUDE.md` testing tooling policy).
    fn make_temp_dir(prefix: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-emergency-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn free_self_owned_disposable_state_removes_existing_file_and_reports_bytes() {
        let dir = make_temp_dir("self-state-present");
        let history = dir.join("history.tsv");
        fs::write(&history, vec![0u8; 128]).unwrap();

        let mut report = EmergencyReport::default();
        free_self_owned_disposable_state(&history, &mut report);

        assert!(!history.exists());
        assert_eq!(report.actions_attempted, 1);
        assert_eq!(report.actions_succeeded, 1);
        assert_eq!(report.total_bytes_freed, 128);
        assert!(report.errors.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn free_self_owned_disposable_state_missing_file_does_not_fabricate_work() {
        let dir = make_temp_dir("self-state-missing");
        let history = dir.join("does-not-exist.tsv");

        let mut report = EmergencyReport::default();
        free_self_owned_disposable_state(&history, &mut report);

        assert_eq!(report, EmergencyReport::default());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_report_has_no_errors_and_is_all_zero() {
        let report = EmergencyReport::default();
        assert_eq!(report.actions_attempted, 0);
        assert_eq!(report.actions_succeeded, 0);
        assert_eq!(report.total_bytes_freed, 0);
        assert_eq!(report.denied_candidates, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn push_error_is_bounded_at_max_errors() {
        let mut report = EmergencyReport::default();
        for i in 0..20 {
            report.push_error(format!("error-{i}"));
        }
        assert_eq!(report.errors.len(), MAX_ERRORS);
    }

    #[test]
    fn display_renders_without_panicking_for_empty_and_populated_reports() {
        let empty = EmergencyReport::default();
        assert!(empty.to_string().contains("errors:            none"));

        let mut populated = EmergencyReport {
            actions_attempted: 2,
            actions_succeeded: 1,
            total_bytes_freed: 4096,
            denied_candidates: 1,
            ..EmergencyReport::default()
        };
        populated.push_error("something non-fatal happened");
        let rendered = populated.to_string();
        assert!(rendered.contains("actions attempted: 2"));
        assert!(rendered.contains("something non-fatal happened"));
    }

    /// Structural proof that this module never references anything
    /// network- or LLM-capable: scans this file's own production source
    /// (everything before this test module starts) for a closed list of
    /// telltale names. No `actions::llm`/HORO-954 code, no HTTP client, no
    /// raw socket type, no async runtime is ever imported or named here —
    /// emergency mode is defined by NOT depending on any of it, even if
    /// such a module exists elsewhere in the crate by the time this lands.
    #[test]
    fn module_never_references_network_or_llm_types() {
        let source = include_str!("mod.rs");
        let production_code = source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one part");

        let forbidden_needles = [
            "reqwest",
            "hyper::",
            "TcpStream",
            "TcpListener",
            "UdpSocket",
            "tokio::net",
            "::llm",
            "llm::",
        ];
        for needle in forbidden_needles {
            assert!(
                !production_code.contains(needle),
                "emergency module's production code must never reference {needle}"
            );
        }
    }

    /// Partial-cleanup-continues: one candidate's `execute()` outcome
    /// (here, a genuine `ResourceIdentityChanged` revalidation abort —
    /// candidate `b`'s directory vanishes between detection and
    /// `process_candidates` running, a real TOCTOU-shaped scenario, not
    /// the pre-HORO-994 `reclaimable_bytes` gap) never stops the loop
    /// from reaching the next candidate — both are attempted, both are
    /// individually reported, the run as a whole still completes.
    #[test]
    fn process_candidates_continues_after_one_candidate_aborts() {
        let dir = make_temp_dir("candidates-partial-continue");
        let node_modules_a = dir.join("a").join("node_modules");
        let node_modules_b = dir.join("b").join("node_modules");
        fs::create_dir_all(&node_modules_a).unwrap();
        fs::create_dir_all(&node_modules_b).unwrap();
        let now = SystemTime::now();

        let evidence_a = autosafe_evidence(node_modules_a.clone(), ResourceKind::NodeModules, now);
        let evidence_b = autosafe_evidence(node_modules_b.clone(), ResourceKind::NodeModules, now);
        // Candidate b's resource vanishes after detection, before this
        // loop revalidates it — `execute()`'s fingerprint check must
        // catch this and abort, never treat a stale fingerprint as still
        // valid.
        fs::remove_dir_all(&node_modules_b).unwrap();

        let audit_log_path = dir.join("actions.jsonl");
        let mut report = EmergencyReport::default();
        process_candidates(
            vec![evidence_a, evidence_b],
            &CleanCollector,
            &ActionRegistry::builtin(),
            now,
            u32::MAX,
            Instant::now(),
            Duration::from_secs(30),
            &mut report,
            &audit_log_path,
        );

        // Both candidates were reached and individually attempted/
        // reported — b's abort did not short-circuit the loop, and a's
        // genuinely AutoSafe fixture was actually deleted.
        assert_eq!(report.actions_attempted, 2);
        assert_eq!(report.actions_succeeded, 1);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("aborted by revalidation"));
        assert!(!node_modules_a.exists());

        // HORO-1057 AC: the `emergency` real-execution path appends one
        // audit record per attempted candidate, both the succeeded one
        // and the aborted one.
        let audit_tail = crate::monitor::read_audit_tail(&audit_log_path, 10);
        assert_eq!(
            audit_tail.len(),
            2,
            "expected one audit record per attempted candidate"
        );
        assert!(audit_tail.iter().all(|r| r.source == "emergency"));
        assert_eq!(
            audit_tail
                .iter()
                .filter(|r| r.outcome == "succeeded")
                .count(),
            1
        );
        assert_eq!(
            audit_tail
                .iter()
                .filter(|r| r.outcome == "aborted_by_revalidation")
                .count(),
            1
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// HORO-1057 AC, proven literally: an audit-write failure never
    /// changes `process_candidate`'s own outcome bookkeeping. `audit_log_path`
    /// is pointed at a path whose PARENT already exists as a plain file
    /// (not a directory) — `append_audit_record`'s own
    /// `create_dir_all(parent)` step is guaranteed to fail against that,
    /// deterministically simulating an unwritable audit destination.
    #[test]
    fn audit_write_failure_does_not_change_process_candidate_outcome() {
        let dir = make_temp_dir("audit-write-failure");
        let node_modules = dir.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        let now = SystemTime::now();
        let evidence = autosafe_evidence(node_modules.clone(), ResourceKind::NodeModules, now);

        let unwritable_parent = dir.join("not-a-directory");
        fs::write(&unwritable_parent, b"blocking file").unwrap();
        let audit_log_path = unwritable_parent.join("actions.jsonl");

        let mut report = EmergencyReport::default();
        process_candidates(
            vec![evidence],
            &CleanCollector,
            &ActionRegistry::builtin(),
            now,
            u32::MAX,
            Instant::now(),
            Duration::from_secs(30),
            &mut report,
            &audit_log_path,
        );

        assert_eq!(
            report.actions_succeeded, 1,
            "the real execution outcome must be completely unaffected by the audit-write failure"
        );
        assert!(
            !node_modules.exists(),
            "the real fixture must still actually be deleted"
        );
        // Confirm the audit write genuinely did fail, rather than this
        // test accidentally not exercising the failure path at all.
        assert!(!audit_log_path.exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// Persistence-failure fault injection at the whole-`run_emergency`
    /// level: a backend whose `record()` always fails must never stop
    /// the run, and at least one safe fixture (the self-owned disposable
    /// history file) is still freed.
    ///
    /// Uses an empty `DetectorRegistry` — never `builtin()` — deliberately.
    /// `builtin()`'s Homebrew/Docker detectors shell out to whatever
    /// `brew`/`docker` the *real host machine* actually has, ignoring
    /// `DiscoveryContext::home_dir` entirely (see `DetectorRegistry::
    /// from_detectors`'s own doc comment). Post-HORO-994, a genuinely
    /// `AutoSafe` real-machine Homebrew cache candidate is no longer
    /// masked by the revalidation abort this module's tests used to rely
    /// on — `process_candidate` would actually authorize and execute
    /// `brew cleanup -s` against the developer's real cache. This test
    /// must only ever mutate its own tempdir fixtures.
    #[test]
    fn run_emergency_survives_persistence_write_failure_and_frees_self_owned_state() {
        let dir = make_temp_dir("run-persistence-failure");
        let self_state = dir.join("history.tsv");
        fs::write(&self_state, vec![0u8; 256]).unwrap();
        let home_dir = dir.join("home");
        fs::create_dir_all(&home_dir).unwrap();

        let ctx = DiscoveryContext::new(home_dir);
        let registry = DetectorRegistry::from_detectors(vec![]);
        let actions = ActionRegistry::builtin();
        let fs_stat = FakeFsStat(FsUsage::new(100, 3));

        let report = run_emergency(
            &fs_stat,
            &CleanCollector,
            &registry,
            &actions,
            &AlwaysFailingPersistence,
            &ctx,
            &self_state,
            10,
            Duration::from_secs(10),
            &dir.join("actions.jsonl"),
        );

        assert!(!self_state.exists());
        assert_eq!(report.actions_succeeded, 1);
        assert_eq!(report.total_bytes_freed, 256);
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("failed to persist emergency run record")));

        fs::remove_dir_all(&dir).ok();
    }

    /// Same wrap point, a distinct simulated cause: a backend whose
    /// `record()` fails specifically at the temp/log directory-creation
    /// step must be handled identically — never fatal, never a panic.
    ///
    /// Uses an empty `DetectorRegistry` for the same reason as
    /// `run_emergency_survives_persistence_write_failure_and_frees_self_owned_state`
    /// above — see its doc comment.
    #[test]
    fn run_emergency_survives_history_directory_creation_failure_and_frees_self_owned_state() {
        let dir = make_temp_dir("run-dircreate-failure");
        let self_state = dir.join("history.tsv");
        fs::write(&self_state, vec![0u8; 64]).unwrap();
        let home_dir = dir.join("home");
        fs::create_dir_all(&home_dir).unwrap();

        let ctx = DiscoveryContext::new(home_dir);
        let registry = DetectorRegistry::from_detectors(vec![]);
        let actions = ActionRegistry::builtin();
        let fs_stat = FakeFsStat(FsUsage::new(100, 3));

        let report = run_emergency(
            &fs_stat,
            &CleanCollector,
            &registry,
            &actions,
            &AlwaysFailingDirCreationPersistence,
            &ctx,
            &self_state,
            10,
            Duration::from_secs(10),
            &dir.join("actions.jsonl"),
        );

        assert!(!self_state.exists());
        assert_eq!(report.actions_succeeded, 1);
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("failed to persist emergency run record")));

        fs::remove_dir_all(&dir).ok();
    }

    /// End-to-end happy path with a real, working `FilePersistence`:
    /// proves the non-failure path also works, not just the fault
    /// injection above.
    ///
    /// Uses an empty `DetectorRegistry` — never `builtin()` — for the same
    /// reason as `run_emergency_survives_persistence_write_failure_and_frees_self_owned_state`
    /// above: post-HORO-994, `builtin()`'s real Homebrew/Docker detectors
    /// finding genuine machine state is no longer masked by a revalidation
    /// abort, so this run would actually authorize and execute a real
    /// `brew cleanup -s` against the host's real cache. With detection
    /// scoped to zero fake detectors, the only action in this run is
    /// freeing the tempdir `self_state` fixture, so the assertions below
    /// are deterministic rather than "tolerate zero-or-more" hedges.
    #[test]
    fn run_emergency_end_to_end_with_working_persistence_frees_self_state_and_records_history() {
        let dir = make_temp_dir("run-happy-path");
        let self_state = dir.join("history.tsv");
        fs::write(&self_state, vec![0u8; 32]).unwrap();
        let home_dir = dir.join("home");
        fs::create_dir_all(&home_dir).unwrap();
        let history_log = dir.join("persisted-history.tsv");

        let ctx = DiscoveryContext::new(home_dir);
        let registry = DetectorRegistry::from_detectors(vec![]);
        let actions = ActionRegistry::builtin();
        let fs_stat = FakeFsStat(FsUsage::new(100, 3));
        let persistence = crate::monitor::persistence::FilePersistence::new(&history_log);

        let report = run_emergency(
            &fs_stat,
            &CleanCollector,
            &registry,
            &actions,
            &persistence,
            &ctx,
            &self_state,
            10,
            Duration::from_secs(10),
            &dir.join("actions.jsonl"),
        );

        assert!(!self_state.exists());
        assert_eq!(report.actions_succeeded, 1);
        assert_eq!(report.total_bytes_freed, 32);
        assert!(
            report.errors.is_empty(),
            "expected no errors with an empty detector registry, got {:?}",
            report.errors
        );
        assert!(
            history_log.exists(),
            "a working backend must actually persist a record"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
