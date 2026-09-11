//! Dry-run, real execution, and reclaim accounting (HORO-951) — the final
//! piece wiring evidence, detection, policy, and actions together into
//! real (but carefully bounded) destructive capability.
//!
//! Canonical safety invariant: AI can recommend. Policy decides. Executor
//! verifies. Filesystem reality wins. This module is the "executor
//! verifies" half of that sentence: [`execute`] (added in a follow-up
//! commit) never trusts a previously-computed
//! [`crate::policy::PolicyDecision`] at face value — it always
//! re-collects evidence and re-runs [`crate::policy::classify`]
//! immediately before mutating anything, and aborts rather than acts if
//! that fresh read disagrees with what was approved.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::actions::{Action, ActionError, ActionId, ActionPlan};
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
/// `execute` (added in a follow-up commit) would act on, because both
/// call the same [`Action::plan`] on the same [`Evidence`].
pub fn dry_run(action: &dyn Action, ev: &Evidence) -> Result<ActionPlan, ActionError> {
    action.plan(ev)
}

/// Execute a plan for real.
///
/// Requires an [`Approval`] — there is no execution entry point that
/// skips it. Immediately before mutating anything, this function
/// re-collects evidence and re-classifies it (see module docs), aborting
/// rather than acting if that fresh read disagrees with what was
/// approved. Actually running the validated plan's steps (and
/// re-measuring reclaimed bytes) lands in a follow-up commit — for now,
/// once revalidation passes, this returns a `DryRun` outcome carrying the
/// freshly-rendered plan's expected bytes.
pub fn execute(
    action: &dyn Action,
    approval: &Approval,
    collector: &dyn EvidenceCollector,
    cfg: &PolicyConfig,
    now: SystemTime,
) -> ExecutionReport {
    let resource = approval.decision().resource.clone();

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

    // 8-9 (actually running the plan's steps and re-measuring reclaimed
    // bytes) land in a follow-up commit.
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

    ExecutionReport {
        action: plan.action,
        resource: plan.resource,
        outcome: ExecutionOutcome::DryRun,
        expected_reclaimed_bytes: plan.expected_reclaimed_bytes,
        actual_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::NodeCleanNodeModules;
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
}
