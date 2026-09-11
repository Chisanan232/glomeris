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

use crate::actions::{Action, ActionError, ActionId, ActionPlan};
use crate::evidence::model::{Evidence, ResourceId};
use crate::evidence::probe::ProbeOutcome;

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

// `execute` lands in a follow-up commit.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::NodeCleanNodeModules;
    use crate::evidence::model::{
        NativeCleanup, Recoverability, ResourceFingerprint, ResourceKind, ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

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
}
