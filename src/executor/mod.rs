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

use crate::actions::ActionId;
use crate::evidence::model::ResourceId;
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

// `dry_run` and `execute` land in follow-up commits.
