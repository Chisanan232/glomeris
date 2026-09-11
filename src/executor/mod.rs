//! Dry-run, real execution, and reclaim accounting (HORO-951) — the final
//! piece wiring evidence, detection, policy, and actions together into
//! real (but carefully bounded) destructive capability.
//!
//! Canonical safety invariant: AI can recommend. Policy decides. Executor
//! verifies. Filesystem reality wins. This module is the "executor
//! verifies" half of that sentence: [`execute`] never trusts a
//! previously-computed [`crate::policy::PolicyDecision`] at face value —
//! it always re-collects evidence and re-runs [`crate::policy::classify`]
//! immediately before mutating anything, and aborts rather than acts if
//! that fresh read disagrees with what was approved.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::actions::{Action, ActionError, ActionId, ActionPlan, ActionStep};
use crate::detectors::{discovery_evidence, probe_mtime, shallow_logical_bytes, DetectorId};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Completeness, Evidence, NativeCleanup, ResourceId, ResourceLocator};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::policy::{classify, Approval, PolicyClass, PolicyConfig, PolicyDecision, ReasonCode};

/// Time budget applied to the deletion-time revalidation's correlation
/// pass. Kept short — this runs synchronously right before a destructive
/// action, so it must not stall indefinitely.
const REVALIDATION_TIMEOUT: Duration = Duration::from_secs(5);

/// The outcome of one execution (or dry-run) attempt.
pub struct ExecutionReport {
    pub action: ActionId,
    pub resource: ResourceId,
    pub outcome: ExecutionOutcome,
    pub expected_reclaimed_bytes: ProbeOutcome<u64>,
    /// Re-measured AFTER execution — never assumed equal to
    /// `expected_reclaimed_bytes`.
    pub actual_reclaimed_bytes: ProbeOutcome<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionOutcome {
    /// No mutation happened — this is just the rendered plan.
    DryRun,
    Succeeded,
    Failed(String),
    /// The deletion-time TOCTOU/policy revalidation tripped. Nothing was
    /// mutated.
    AbortedByRevalidation(AbortReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    /// Fingerprint mismatch between plan-time and execute-time —
    /// resource identity changed (deleted, replaced, symlink-swapped)
    /// between approval and execution.
    ResourceIdentityChanged,
    /// A fresh [`crate::policy::classify`] no longer agrees with the
    /// planned [`crate::policy::PolicyClass`].
    PolicyClassDowngraded,
    /// The fresh decision carries a reason code that was not present in
    /// the approved decision. `Ask` is a heterogeneous bucket: consent
    /// granted for `Ask{RebuildCostHigh}` does not authorize executing
    /// against a freshly observed `Ask{ResourceInActiveUse}` — the user
    /// consented to a different risk.
    PolicyReasonsWidened,
    /// Fresh evidence completeness is worse than it was at plan time,
    /// even though class and reasons alone didn't already catch it.
    EvidenceDegraded,
    RevalidationEvidenceStale,
}

/// Render a plan without executing it. Identical output to what
/// [`execute`] would act on, because both call the same [`Action::plan`]
/// on the same [`Evidence`].
pub fn dry_run(action: &dyn Action, ev: &Evidence) -> Result<ActionPlan, ActionError> {
    action.plan(ev)
}

/// Execute a plan for real.
///
/// Requires an [`Approval`] — there is no execution entry point that
/// skips it. Immediately before mutating anything, this function
/// re-collects evidence and re-classifies it (see module docs), aborting
/// rather than acting if that fresh read disagrees with what was
/// approved. Only once revalidation passes does it actually run the
/// plan's steps.
pub fn execute(
    action: &dyn Action,
    approval: &Approval,
    collector: &dyn EvidenceCollector,
    cfg: &PolicyConfig,
    now: SystemTime,
) -> ExecutionReport {
    let resource = approval.decision().resource.clone();

    // 0. Snapshot the resource's live filesystem identity via
    // `symlink_metadata` (never `metadata` — a symlink must be detected as
    // a symlink, not resolved through) as the VERY FIRST thing this
    // function does, before the slow revalidation correlation pass
    // (`build_fresh_evidence` -> `collector.collect()`) gets any chance to
    // run. This is the anchor a symlink-swap TOCTOU attack cannot
    // backdate: even if the attacker replaces the resource with a symlink
    // during the slow correlation pass below, the final guard right before
    // mutation (see `verify_identity_unchanged`) compares against THIS
    // early snapshot, not against a freshly re-derived one.
    let identity_snapshot = match &resource.locator {
        ResourceLocator::Path(p) => snapshot_identity(p).ok(),
        ResourceLocator::Tool { .. } => None,
    };

    // 1-2: re-canonicalize/re-collect: `fresh_evidence` re-probes the
    // resource's basic fields (size/mtime/fingerprint) directly from the
    // filesystem and re-runs the same `EvidenceCollector` HORO-949
    // defined, rather than trusting anything cached from plan time.
    let fresh_evidence = build_fresh_evidence(&resource, action, collector, now);

    // 3. Fingerprint comparison — resource identity check.
    if fresh_evidence.fingerprint != *approval.fingerprint() {
        return aborted_report(action.id(), resource, AbortReason::ResourceIdentityChanged);
    }

    // 4. Re-run the same deterministic classify() on fresh evidence.
    let fresh_decision = classify(&fresh_evidence, cfg, now);
    let approved_decision = approval.decision();

    // 5. Class must not have changed.
    if fresh_decision.class != approved_decision.class {
        return aborted_report(action.id(), resource, AbortReason::PolicyClassDowngraded);
    }

    // 6. No reason may have widened beyond what was approved.
    if reasons_widened(&approved_decision.reasons, &fresh_decision.reasons) {
        return aborted_report(action.id(), resource, AbortReason::PolicyReasonsWidened);
    }

    // 7. Completeness must not have degraded. `classify()` can only ever
    // reach `AutoSafe` from `Complete` evidence, and its `Partial`/
    // `Failed` completeness branches emit exactly one specific reason
    // code each (`EvidenceIncomplete` / `EvidenceProbeFailed`) — so under
    // today's `classify()` semantics, any real completeness degradation
    // also changes the reason set and is already caught by step 6. This
    // check is a defensive, forward-looking guard against a future
    // `classify()` change that could decouple the two; see PR "Known
    // limitations" for why it is not separately exercised by a dedicated
    // "fires" test.
    let planned_rank = completeness_rank_from_decision(approved_decision);
    let fresh_rank = completeness_rank(&fresh_evidence.completeness());
    if fresh_rank > planned_rank {
        return aborted_report(action.id(), resource, AbortReason::EvidenceDegraded);
    }

    // 8. Build the plan from the SAME fresh evidence just validated —
    // this is what makes dry-run and real execution structurally
    // identical: both call `Action::plan` on the same `Evidence`. Only
    // after all of 1-7 pass do we actually run the steps.
    let plan = match action.plan(&fresh_evidence) {
        Ok(p) => p,
        Err(e) => {
            return failed_report(
                action.id(),
                resource,
                format!("{e:?}"),
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            )
        }
    };

    execute_plan(plan, identity_snapshot)
}

/// `Ask` is a heterogeneous bucket: a reason present in `fresh` but not
/// in `planned` means the user's consent (granted against `planned`)
/// does not cover the freshly-observed risk.
fn reasons_widened(planned: &[ReasonCode], fresh: &[ReasonCode]) -> bool {
    fresh.iter().any(|reason| !planned.contains(reason))
}

fn completeness_rank(completeness: &Completeness) -> u8 {
    match completeness {
        Completeness::Complete => 0,
        Completeness::Partial { .. } => 1,
        Completeness::Failed => 2,
    }
}

/// Derives an approximate completeness rank from a [`PolicyDecision`]
/// alone (an `Approval` does not carry the original `Evidence`).
/// `classify()`'s own structure makes this exact, not merely
/// approximate: `AutoSafe` is only ever reached from `Complete` evidence,
/// and the `Partial`/`Failed` branches emit exactly
/// `[EvidenceIncomplete]` / `[EvidenceProbeFailed]` respectively, with no
/// other code path producing those reasons.
fn completeness_rank_from_decision(decision: &PolicyDecision) -> u8 {
    if decision.class == PolicyClass::AutoSafe {
        return 0;
    }
    if decision.reasons.contains(&ReasonCode::EvidenceProbeFailed) {
        return 2;
    }
    if decision.reasons.contains(&ReasonCode::EvidenceIncomplete) {
        return 1;
    }
    0
}

/// Rebuilds an [`Evidence`] for `resource` the same way a detector would
/// at discovery time (see [`crate::detectors::discovery_evidence`]),
/// then merges in a fresh correlation pass via `collector`. Used only for
/// deletion-time revalidation — never a substitute for a real detector
/// pass.
fn build_fresh_evidence(
    resource: &ResourceId,
    action: &dyn Action,
    collector: &dyn EvidenceCollector,
    now: SystemTime,
) -> Evidence {
    let path = match &resource.locator {
        ResourceLocator::Path(p) => Some(p.as_path()),
        ResourceLocator::Tool { .. } => None,
    };

    let logical_bytes = match path {
        Some(p) => shallow_logical_bytes(p),
        None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    };
    let last_modified = match path {
        Some(p) => probe_mtime(p),
        None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    };
    let fingerprint_path: &Path = path.unwrap_or_else(|| Path::new(""));

    let mut evidence = discovery_evidence(
        resource.clone(),
        DetectorId("executor_revalidation"),
        fingerprint_path,
        logical_bytes,
        last_modified,
        resource.kind.regenerability(),
        action.recoverability(),
        NativeCleanup::Unsupported,
    );

    let correlation = collector.collect(
        resource,
        ProbeBudget {
            timeout: REVALIDATION_TIMEOUT,
        },
    );
    merge_into(&mut evidence, correlation);

    // Stamp `collected_at` with the same injected clock `execute` was
    // given, rather than `discovery_evidence`'s internal
    // `SystemTime::now()` — keeps `classify`'s no-ambient-clock contract
    // intact end to end and keeps this deterministic under test.
    evidence.collected_at = now;

    evidence
}

fn aborted_report(action: ActionId, resource: ResourceId, reason: AbortReason) -> ExecutionReport {
    ExecutionReport {
        action,
        resource,
        outcome: ExecutionOutcome::AbortedByRevalidation(reason),
        expected_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        actual_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    }
}

fn failed_report(
    action: ActionId,
    resource: ResourceId,
    message: String,
    expected_reclaimed_bytes: ProbeOutcome<u64>,
) -> ExecutionReport {
    ExecutionReport {
        action,
        resource,
        outcome: ExecutionOutcome::Failed(message),
        expected_reclaimed_bytes,
        actual_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    }
}

/// Runs every step of `plan` for real. No `unwrap()`/`panic!` — every
/// fallible step produces `ExecutionOutcome::Failed(reason)` instead of
/// crashing.
///
/// `identity_snapshot` is the early, pre-slow-I/O filesystem identity
/// captured by `execute()` before revalidation ran (see its step 0
/// comment). `DeletePath` re-verifies against it immediately before
/// mutating — see [`verify_identity_unchanged`].
fn execute_plan(plan: ActionPlan, identity_snapshot: Option<IdentitySnapshot>) -> ExecutionReport {
    let mut actual_bytes_total: u64 = 0;
    let mut saw_run_tool = false;
    let mut saw_delete = false;

    for step in &plan.steps {
        match step {
            ActionStep::RunTool { tool, args } => {
                saw_run_tool = true;
                match Command::new(tool.program()).args(args).output() {
                    Ok(output) if output.status.success() => {}
                    Ok(output) => {
                        return failed_report(
                            plan.action,
                            plan.resource,
                            format!(
                                "{} exited with {}: {}",
                                tool.program(),
                                output.status,
                                String::from_utf8_lossy(&output.stderr)
                            ),
                            plan.expected_reclaimed_bytes,
                        );
                    }
                    Err(e) => {
                        return failed_report(
                            plan.action,
                            plan.resource,
                            format!("failed to spawn {}: {e}", tool.program()),
                            plan.expected_reclaimed_bytes,
                        );
                    }
                }
            }
            ActionStep::DeletePath { path } => {
                saw_delete = true;
                match delete_path_with_guard(path, &plan.resource, identity_snapshot.as_ref()) {
                    Ok(bytes_removed) => actual_bytes_total += bytes_removed,
                    Err(message) => {
                        return failed_report(
                            plan.action,
                            plan.resource,
                            message,
                            plan.expected_reclaimed_bytes,
                        );
                    }
                }
            }
        }
    }

    // 9. Re-measure actual reclaimed bytes. For a `RunTool` step there is
    // no general way to know what the external tool actually freed, so
    // that is honestly reported as `Unavailable` rather than estimated —
    // only a plan made entirely of `DeletePath` steps gets a real
    // measured total.
    let actual_reclaimed_bytes = if saw_run_tool {
        ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
    } else if saw_delete {
        ProbeOutcome::Observed(actual_bytes_total)
    } else {
        ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
    };

    ExecutionReport {
        action: plan.action,
        resource: plan.resource,
        outcome: ExecutionOutcome::Succeeded,
        expected_reclaimed_bytes: plan.expected_reclaimed_bytes,
        actual_reclaimed_bytes,
    }
}

/// A live filesystem identity — captured via `symlink_metadata` (never
/// `metadata`, so the path itself being a symlink is observed as such,
/// never silently resolved through to whatever it points at) — pinned to
/// the exact path it was taken from.
///
/// This is the anchor for closing the symlink-swap TOCTOU window: taking
/// this snapshot EARLY (before any slow revalidation I/O runs) and
/// re-verifying against it immediately before a destructive mutation means
/// an attacker who swaps the resource for a symlink *during* the slow
/// window cannot make the final check agree with a snapshot it postdates.
#[derive(Debug, Clone, Copy)]
struct IdentitySnapshot {
    dev: u64,
    ino: u64,
    is_symlink: bool,
    is_dir: bool,
}

#[cfg(unix)]
fn snapshot_identity(path: &Path) -> Result<IdentitySnapshot, String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to stat {}: {e}", path.display()))?;
    Ok(IdentitySnapshot {
        dev: metadata.dev(),
        ino: metadata.ino(),
        is_symlink: metadata.file_type().is_symlink(),
        is_dir: metadata.is_dir(),
    })
}

/// Re-stats `path` immediately before a destructive mutation (no I/O may
/// happen between this call and the actual mutation syscall) and refuses
/// unless it is bit-for-bit the same filesystem object `snapshot` was
/// taken from: same (dev, ino), and NOT a symlink — unconditionally,
/// regardless of what the original snapshot looked like, since a
/// destructive action must never operate through a symlink no matter when
/// it appeared. This closes the multi-second correlation-pass TOCTOU
/// window down to the true, irreducible gap between this stat and the
/// mutation syscall — an inherent OS-level limitation not fixable in
/// userspace without holding an open directory file descriptor across the
/// whole operation, and accepted as residual risk.
#[cfg(unix)]
fn verify_identity_unchanged(path: &Path, snapshot: &IdentitySnapshot) -> Result<(), String> {
    let current = snapshot_identity(path)?;

    if current.is_symlink {
        return Err(format!(
            "refusing to act on {}: resource identity changed immediately before mutation \
             (path is now a symlink)",
            path.display()
        ));
    }

    if current.dev != snapshot.dev || current.ino != snapshot.ino {
        return Err(format!(
            "refusing to act on {}: resource identity changed immediately before mutation \
             (expected dev={} ino={}, found dev={} ino={})",
            path.display(),
            snapshot.dev,
            snapshot.ino,
            current.dev,
            current.ino
        ));
    }

    Ok(())
}

/// Deletes `path` after a final, immediately-preceding identity check
/// against the EARLY snapshot `execute()` captured before any slow
/// revalidation I/O ran (see [`verify_identity_unchanged`]). This is what
/// defeats the symlink-swap TOCTOU: unlike re-deriving "the expected root"
/// from the same live path being deleted (which degenerates to comparing a
/// path to itself), this compares against an identity captured strictly
/// earlier in time, before an attacker had the multi-second correlation-pass
/// window to swap anything.
///
/// Returns the pre-deletion byte size on success (best-effort, for
/// `actual_reclaimed_bytes` accounting). The byte-size walk runs BEFORE the
/// final identity check and deletion — not after — so no slow I/O is ever
/// inserted between the check and the mutation syscall.
fn delete_path_with_guard(
    path: &Path,
    resource: &ResourceId,
    snapshot: Option<&IdentitySnapshot>,
) -> Result<u64, String> {
    match &resource.locator {
        ResourceLocator::Path(_) => {}
        ResourceLocator::Tool { .. } => {
            return Err("DeletePath step on a non-path resource".to_string())
        }
    };

    let snapshot = snapshot.ok_or_else(|| {
        format!(
            "refusing to delete {}: no verified resource identity snapshot available",
            path.display()
        )
    })?;

    // Best-effort size accounting first — this may walk a large tree, and
    // must never land between the identity check and the mutation below.
    let bytes_before = total_size_best_effort(path);

    verify_identity_unchanged(path, snapshot)?;

    let remove_result = if snapshot.is_dir {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    remove_result.map_err(|e| format!("failed to delete {}: {e}", path.display()))?;

    Ok(bytes_before)
}

/// Best-effort recursive size sum. Errors reading any entry are silently
/// skipped — this is used only for reclaim accounting, never for a
/// safety decision.
fn total_size_best_effort(path: &Path) -> u64 {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };

    if !metadata.is_dir() {
        return metadata.len();
    }

    let read_dir = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => return 0,
    };

    let mut total = 0u64;
    for entry in read_dir.flatten() {
        total += total_size_best_effort(&entry.path());
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{CargoCleanTargetDir, NodeCleanNodeModules};
    use crate::detectors::dev_ino_fingerprint;
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        GitState, NativeCleanup, ProcessRef, Recoverability, ResourceFingerprint, ResourceKind,
    };
    use crate::policy::approval::authorize;
    use crate::policy::UserConsent;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn make_temp_dir(prefix: &str) -> PathBuf {
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

    fn evidence_for(resource_path: PathBuf, kind: ResourceKind) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(resource_path)),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: crate::detectors::DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn dry_run_renders_a_plan_without_mutating_anything() {
        let root = make_temp_dir("dry-run");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();

        let ev = evidence_for(node_modules.clone(), ResourceKind::NodeModules);
        let plan = dry_run(&NodeCleanNodeModules, &ev).expect("dry_run should succeed");

        assert_eq!(plan.steps.len(), 1);
        assert!(node_modules.exists(), "dry_run must not mutate anything");

        fs::remove_dir_all(&root).ok();
    }

    /// A fully-controllable fake collector — tests must never use
    /// `DefaultEvidenceCollector`, which shells out to lsof/git/pgrep.
    struct FakeCollector;

    impl EvidenceCollector for FakeCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::<ProcessRef>::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    fn fingerprint_of(path: &Path) -> ResourceFingerprint {
        ResourceFingerprint {
            dev_ino: dev_ino_fingerprint(path),
            mtime: probe_mtime(path).observed().copied(),
            tool_revision: None,
        }
    }

    #[test]
    fn reasons_widened_detects_new_reason_not_in_planned_ask_bucket() {
        // The ticket's literal narrative: planned consent covered
        // RebuildCostHigh; the fresh read observes ResourceInActiveUse
        // instead — a different risk the user never consented to.
        let planned = vec![ReasonCode::RebuildCostHigh];
        let fresh = vec![ReasonCode::ResourceInActiveUse];
        assert!(reasons_widened(&planned, &fresh));
    }

    #[test]
    fn reasons_widened_is_false_when_fresh_reasons_are_a_subset_of_planned() {
        let planned = vec![
            ReasonCode::ResourceInActiveUse,
            ReasonCode::GitWorktreeDirty,
        ];
        let fresh = vec![ReasonCode::ResourceInActiveUse];
        assert!(!reasons_widened(&planned, &fresh));
    }

    /// Proves the TOCTOU resource-identity-change abort path: the
    /// resource is deleted out from under the approval between grant and
    /// `execute()`.
    #[test]
    fn execute_aborts_when_resource_identity_changed_between_approval_and_execution() {
        let root = make_temp_dir("execute-toctou-identity");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("pkg.js"), vec![0u8; 128]).unwrap();

        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(node_modules.clone()),
        );
        let fingerprint = fingerprint_of(&node_modules);
        let now = SystemTime::now();
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::EvidenceIncomplete],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("Ask decision with matching consent must authorize");

        // Simulate the TOCTOU race: the resource vanishes between
        // approval and execution.
        fs::remove_dir_all(&node_modules).unwrap();

        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
        );

        assert_eq!(
            report.outcome,
            ExecutionOutcome::AbortedByRevalidation(AbortReason::ResourceIdentityChanged)
        );

        fs::remove_dir_all(&root).ok();
    }

    /// Proves the reason-widening abort path (step 6): the fresh decision
    /// stays the same `PolicyClass` (`Ask`) as the approved decision, but
    /// carries a reason code the approved consent never covered. This
    /// happens naturally here because `reclaimable_bytes` is never
    /// populated by any detector in this codebase (see PR "Known
    /// limitations"), so a freshly rebuilt `Evidence` is always `Partial`
    /// -> `Ask{EvidenceIncomplete}`, regardless of what the original
    /// decision's reason was.
    #[test]
    fn execute_aborts_when_fresh_reasons_widen_beyond_planned_ask_bucket() {
        let root = make_temp_dir("execute-reason-widening");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("pkg.js"), vec![0u8; 128]).unwrap();

        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(node_modules.clone()),
        );
        let fingerprint = fingerprint_of(&node_modules);
        let now = SystemTime::now();
        // Planned decision: Ask{RebuildCostHigh} — a different risk than
        // what the fresh read below will observe.
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::RebuildCostHigh],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("Ask decision with matching consent must authorize");

        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
        );

        assert_eq!(
            report.outcome,
            ExecutionOutcome::AbortedByRevalidation(AbortReason::PolicyReasonsWidened)
        );
        // Nothing was mutated.
        assert!(node_modules.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// Full happy-path real execution: an `Ask{EvidenceIncomplete}`
    /// approval (the only reachable class today, see PR "Known
    /// limitations") whose fresh revalidation reproduces the identical
    /// class/reasons, so `execute()` proceeds to actually delete a real
    /// disposable `node_modules` fixture.
    #[test]
    fn execute_real_deletion_removes_node_modules_and_reports_actual_bytes() {
        let root = make_temp_dir("execute-real-node");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(node_modules.join("pkg")).unwrap();
        fs::write(node_modules.join("pkg/index.js"), vec![0u8; 4096]).unwrap();

        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(node_modules.clone()),
        );
        let fingerprint = fingerprint_of(&node_modules);
        let now = SystemTime::now();
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::EvidenceIncomplete],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("Ask decision with matching consent must authorize");

        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
        );

        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        assert!(!node_modules.exists());
        match report.actual_reclaimed_bytes {
            ProbeOutcome::Observed(bytes) => assert!(bytes > 0),
            other => panic!("expected Observed(_), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// Same happy-path shape as above, but for the cargo action —
    /// exercises the `RunTool` branch (real `cargo clean`), whose
    /// `actual_reclaimed_bytes` is honestly `Unavailable`.
    #[test]
    fn execute_real_run_tool_cleans_cargo_target_dir() {
        let root = make_temp_dir("execute-real-cargo");
        let target_dir = root.join("target");
        fs::create_dir_all(target_dir.join("debug")).unwrap();
        fs::write(
            target_dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(target_dir.join("debug/build_output.bin"), vec![0u8; 4096]).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"glomeris-test-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();

        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(target_dir.clone()),
        );
        let fingerprint = fingerprint_of(&target_dir);
        let now = SystemTime::now();
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::EvidenceIncomplete],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("Ask decision with matching consent must authorize");

        let report = execute(
            &CargoCleanTargetDir,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
        );

        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        assert!(!target_dir.exists());
        assert_eq!(
            report.actual_reclaimed_bytes,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );

        fs::remove_dir_all(&root).ok();
    }

    /// A collector whose `collect()` implementation, as a side effect,
    /// swaps the approved resource out for a symlink pointing at a
    /// separate "victim" directory — DURING `collect()`, i.e. exactly when
    /// the real revalidation correlation pass would be doing its slow I/O.
    /// Proves the defect-1 TOCTOU regression: `execute()` must refuse to
    /// delete rather than follow the swapped symlink into the victim.
    struct SymlinkSwapCollector {
        swap_path: PathBuf,
        victim_target: PathBuf,
    }

    impl EvidenceCollector for SymlinkSwapCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            fs::remove_dir_all(&self.swap_path).expect("remove approved dir to stage the swap");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&self.victim_target, &self.swap_path)
                .expect("create swap symlink");

            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::<ProcessRef>::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    /// Defect 1 regression test (HORO-951 adversarial review): reproduces
    /// the reviewer's exact scenario — the approved `node_modules`
    /// directory is replaced with a symlink to an unrelated victim
    /// directory during the slow revalidation correlation pass inside
    /// `execute()`. Before the fix, `delete_path_with_guard` re-canonicalized
    /// the SAME live (now-swapped) path on both sides of its check, so
    /// `X.starts_with(X)` always passed and the victim got deleted. After
    /// the fix, the early identity snapshot taken before the slow pass
    /// catches the swap at the final pre-mutation check and refuses.
    #[test]
    fn execute_refuses_deletion_when_resource_is_symlink_swapped_during_revalidation() {
        let root = make_temp_dir("execute-toctou-symlink-swap");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("pkg.js"), vec![0u8; 128]).unwrap();

        let victim_root = make_temp_dir("execute-toctou-victim");
        let victim_target = victim_root.join("victim_target");
        fs::create_dir_all(&victim_target).unwrap();
        let marker = victim_target.join("marker.txt");
        fs::write(&marker, b"do not delete me").unwrap();

        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(node_modules.clone()),
        );
        let fingerprint = fingerprint_of(&node_modules);
        let now = SystemTime::now();
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::EvidenceIncomplete],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("Ask decision with matching consent must authorize");

        let collector = SymlinkSwapCollector {
            swap_path: node_modules.clone(),
            victim_target: victim_target.clone(),
        };

        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &collector,
            &PolicyConfig::default(),
            now,
        );

        assert!(
            marker.exists(),
            "victim marker file must survive the attack — exploit succeeded if this fails"
        );
        match &report.outcome {
            ExecutionOutcome::Failed(msg) => assert!(
                msg.contains("identity changed"),
                "expected an identity-changed refusal, got message: {msg}"
            ),
            other => {
                panic!("expected ExecutionOutcome::Failed(identity changed...), got {other:?}")
            }
        }

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&victim_root).ok();
    }
}
