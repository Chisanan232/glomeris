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

pub mod goal;
pub mod lock;
pub mod recovery_loop;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::actions::{Action, ActionError, ActionId, ActionPlan, ActionStep};
use crate::detectors::{
    discovery_evidence, estimate_logical_bytes, probe_mtime, size_estimate_budget, DetectorId,
};
use crate::evidence::correlate::host_integrity;
use crate::evidence::correlate::{
    compare_host_integrity, merge_into, EvidenceCollector, ProbeBudget,
};
use crate::evidence::model::{
    Completeness, Evidence, ExecutableDependencyReport, HostIntegrityReport, NativeCleanup,
    ResourceId, ResourceLocator,
};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::policy::{classify, Approval, PolicyClass, PolicyConfig, PolicyDecision, ReasonCode};

/// Time budget applied to the deletion-time revalidation's correlation
/// pass. Kept short — this runs synchronously right before a destructive
/// action, so it must not stall indefinitely.
const REVALIDATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Capability seam for HORO-1826's per-action free-byte delta (ADR-0001
/// §4.4). Deliberately its own trait rather than a new method on
/// [`crate::monitor::fs_stat::FsStat`] — that trait's own doc comment
/// explicitly rejects widening it, since every existing test double
/// (`FixedFsStat`, `ScriptedFsStat`, `FailingFsStat`) would have to grow
/// an unrelated method just to keep compiling for a capability only this
/// module needs.
pub trait VolumeDelta: Send + Sync {
    /// `(volume_dev, free_bytes)` for the filesystem containing `path` —
    /// one `statvfs`/`stat` pair, O(1), no directory traversal.
    fn sample(&self, path: &Path) -> std::io::Result<(u64, u64)>;
}

/// Production [`VolumeDelta`]: wraps [`crate::platform::macos::statfs::MacosFsStat`]'s
/// existing `stat`/`volume_dev` calls — no new syscall, no new dependency.
#[cfg(unix)]
pub struct MacosVolumeDelta;

#[cfg(unix)]
impl VolumeDelta for MacosVolumeDelta {
    fn sample(&self, path: &Path) -> std::io::Result<(u64, u64)> {
        use crate::monitor::fs_stat::FsStat;
        use crate::platform::macos::statfs::MacosFsStat;
        let stat = MacosFsStat;
        let usage = stat.stat(path)?;
        let dev = stat.volume_dev(path)?;
        Ok((dev, usage.free_bytes))
    }
}

/// Computes [`ExecutionReport::volume_free_delta`] from a `before`/`after`
/// sample pair, fail-closed on every ambiguous case (ADR-0001 §4.4 /
/// §8 "measured volume != resource volume => delta UNVERIFIED for that
/// action").
fn compute_volume_delta(
    resource_dev: Option<u64>,
    before: Option<(u64, u64)>,
    after: Option<(u64, u64)>,
) -> ProbeOutcome<i64> {
    let (Some(resource_dev), Some((before_dev, before_free)), Some((after_dev, after_free))) =
        (resource_dev, before, after)
    else {
        return ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
    };
    // "Different filesystem" (ADR-0001 §4.4): the resource's own `st_dev`
    // (captured pre-mutation, see `IdentitySnapshot`) must match the
    // measured volume's `st_dev` on BOTH samples, and the two samples
    // must agree with each other -- a volume swapped mid-action is not a
    // byte count that means anything.
    if before_dev != resource_dev || after_dev != resource_dev || before_dev != after_dev {
        return ProbeOutcome::Unavailable(ProbeReason::Failed);
    }
    // Signed on purpose: a concurrent writer can outpace this action's
    // own freed bytes, producing a negative delta. That is a true
    // observation, not a failure -- see the field's own doc comment.
    ProbeOutcome::Observed(after_free as i64 - before_free as i64)
}

/// The outcome of one execution (or dry-run) attempt.
pub struct ExecutionReport {
    pub action: ActionId,
    pub resource: ResourceId,
    pub outcome: ExecutionOutcome,
    pub expected_reclaimed_bytes: ProbeOutcome<u64>,
    /// Re-measured AFTER execution — never assumed equal to
    /// `expected_reclaimed_bytes`.
    pub actual_reclaimed_bytes: ProbeOutcome<u64>,
    /// Pre/post host-dependency integrity (ADR-0001 §4.4, HORO-1826).
    /// `None` when nothing was mutated (`DryRun`/`AbortedByRevalidation`),
    /// or when the pre-action probe cleanly found nothing to verify — see
    /// [`compute_host_integrity`]. `Some` is never a placeholder "all
    /// OK": its own [`HostIntegrityReport::status`] fails closed to
    /// `Degraded`/`Unverified` on any probe failure.
    pub host_integrity: Option<HostIntegrityReport>,
    /// Signed free-byte delta measured immediately before and after the
    /// mutating step (ADR-0001 §4.4, HORO-1826). Includes concurrent
    /// writers — a negative value means something else grew the volume's
    /// usage faster than this action freed it, not that the action
    /// failed. `Unavailable` when no [`VolumeDelta`] was supplied, the
    /// resource has no single path (`ResourceLocator::Tool`), either
    /// sample failed, or the resource's own filesystem differs from the
    /// measured volume's (ADR-0001 §4.4 "different filesystem").
    pub volume_free_delta: ProbeOutcome<i64>,
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
    volume: Option<&dyn VolumeDelta>,
) -> ExecutionReport {
    execute_inner(action, approval, collector, cfg, now, volume, None)
}
pub(crate) fn execute_owned(
    action: &dyn Action,
    approval: &Approval,
    collector: &dyn EvidenceCollector,
    cfg: &PolicyConfig,
    now: SystemTime,
    volume: Option<&dyn VolumeDelta>,
    _authority: &crate::executor::lock::ExecutionLockGuard,
) -> ExecutionReport {
    execute_inner(action, approval, collector, cfg, now, volume, None)
}
pub(crate) fn execute_managed_owned(
    action: &dyn Action,
    approval: &Approval,
    collector: &dyn EvidenceCollector,
    cfg: &PolicyConfig,
    now: SystemTime,
    volume: Option<&dyn VolumeDelta>,
    permit: crate::ownership::ManagedRetirementPermit<'_>,
) -> ExecutionReport {
    let guard = match permit.into_guard(action, approval) {
        Ok(g) => g,
        Err(_) => {
            return aborted_report(
                action.id(),
                approval.decision().resource.clone(),
                AbortReason::EvidenceDegraded,
            )
        }
    };
    execute_inner(action, approval, collector, cfg, now, volume, Some(guard))
}
fn execute_inner(
    action: &dyn Action,
    approval: &Approval,
    collector: &dyn EvidenceCollector,
    cfg: &PolicyConfig,
    now: SystemTime,
    volume: Option<&dyn VolumeDelta>,
    admitted_guard: Option<crate::ownership::RetirementGuard>,
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
        ResourceLocator::Path(p) => match snapshot_identity(p) {
            Ok(snapshot) => Some(snapshot),
            Err(_) => {
                return aborted_report(action.id(), resource, AbortReason::ResourceIdentityChanged)
            }
        },
        ResourceLocator::Tool { .. } => None,
    };

    let _native_guard = match crate::ownership::native::execution_guard(&resource) {
        Ok(guard) => guard,
        Err(_) => return aborted_report(action.id(), resource, AbortReason::EvidenceDegraded),
    };
    let mut managed_guard = if resource.kind.is_managed_resource() {
        let crate::evidence::ResourceLocator::Path(path) = &resource.locator else {
            return aborted_report(action.id(), resource, AbortReason::ResourceIdentityChanged);
        };
        let _ = path;
        let Some(guard) = admitted_guard else {
            return aborted_report(action.id(), resource, AbortReason::EvidenceDegraded);
        };
        Some(guard)
    } else {
        if let crate::evidence::ResourceLocator::Path(path) = &resource.locator {
            if crate::ownership::protects_other_kind(path) {
                return aborted_report(action.id(), resource, AbortReason::PolicyReasonsWidened);
            }
        }
        None
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

    let pre_dependency = fresh_evidence.executable_dependency.clone();

    // HORO-1826 §4.4: the "before" volume sample, taken as close to the
    // actual mutation as this function gets -- right before
    // `execute_plan`, not back at step 0's identity snapshot (which the
    // revalidation's own, possibly slow, correlation pass sits between).
    // `mount_path` is the resource's PARENT directory: it is on the same
    // filesystem as the resource by definition, and -- unlike the
    // resource itself -- still exists after a successful deletion, so
    // the "after" sample below is taken against the same path.
    let mount_path: Option<PathBuf> = match &resource.locator {
        ResourceLocator::Path(p) => p.parent().map(Path::to_path_buf),
        ResourceLocator::Tool { .. } => None,
    };
    let sample = |p: &Path| -> Option<(u64, u64)> { volume?.sample(p).ok() };
    let before = mount_path.as_deref().and_then(sample);

    if let Some(guard) = &mut managed_guard {
        if guard.intent().is_err() {
            return aborted_report(action.id(), resource, AbortReason::EvidenceDegraded);
        }
        #[cfg(test)]
        crate::ownership::crash_tests::checkpoint("intent");
    }
    let mut report = execute_plan(plan, identity_snapshot);
    #[cfg(test)]
    if managed_guard.is_some() {
        crate::ownership::crash_tests::checkpoint("deleted");
    }

    // HORO-1826 §4.4: a pre/post host-dependency integrity check, taken
    // AFTER whatever `execute_plan` just did (successfully or not — a
    // step that fails partway through may still have mutated something,
    // and that is exactly when this check matters most). Overwrites
    // whichever `host_integrity: None` default `execute_plan`'s own
    // constructors set, rather than threading the pre-snapshot and
    // collector through every one of its internal return sites.
    report.host_integrity = compute_host_integrity(&pre_dependency, &report.outcome);

    // Same reasoning for the "after" sample: taken unconditionally once
    // `execute_plan` returns, regardless of its outcome, because a
    // `RunTool` step can exit non-zero (`Failed`) after partially
    // mutating the filesystem -- the honest byte delta still matters
    // then, not only on a clean `Succeeded`.
    let after = mount_path.as_deref().and_then(sample);
    report.volume_free_delta =
        compute_volume_delta(identity_snapshot.map(|s| s.dev), before, after);
    if let Some(guard) = &mut managed_guard {
        let delta = report.volume_free_delta.observed().copied();
        if let Err(e) = guard.finish(delta) {
            report.outcome =
                ExecutionOutcome::Failed(format!("cleanup outcome persistence failed: {e}"));
        }
    }

    report
}

/// Decides whether a post-action host-dependency check is worth running
/// at all, and if so, runs it and compares against `pre` (HORO-1826 §4.4).
///
/// Only runs the post-check when the pre-action snapshot was NOT a clean,
/// fully-resolved negative — i.e. when it found at least one reference to
/// verify, or could not already confirm there wasn't one
/// (`unresolved` non-empty, or the probe itself failed). A resource the
/// pre-action probe cleanly found nothing on has nothing this check can
/// usefully verify, so probing it again would be pure overhead, not a
/// safety gap — see the ticket's own framing. `None` is returned for that
/// case and for any outcome that never attempted a mutation.
///
/// **Why this does NOT call `collector.collect()` again for the post
/// side** (the design this replaced): [`HostDependencyProbe::probe`]'s
/// first step is `fs::canonicalize(resource_path)`, which fails the
/// instant a successful cleanup has deleted the resource — exactly the
/// case this check exists to verify. Re-running the full probe
/// post-action would therefore report `Unavailable` on every real
/// successful cleanup regardless of whether the tracked dependency
/// itself survived, making `Ok`/`Degraded` unreachable in production and
/// the "valid unchanged hook -> OK" / "disposable hook -> DEGRADED"
/// acceptance criteria undemonstrable outside a fake collector. Instead,
/// [`restat_post_action`] re-stats each already-known dependency's own
/// path directly — no symlink-chain walk, no host-config re-read, no
/// subprocess — which stays well-defined whether or not the resource
/// itself still exists.
fn compute_host_integrity(
    pre: &ProbeOutcome<ExecutableDependencyReport>,
    outcome: &ExecutionOutcome,
) -> Option<HostIntegrityReport> {
    // Nothing was mutated (or attempted) — there is no run to verify.
    if matches!(
        outcome,
        ExecutionOutcome::DryRun | ExecutionOutcome::AbortedByRevalidation(_)
    ) {
        return None;
    }

    let pre_report = match pre.observed() {
        Some(report) => report,
        // The pre-action probe itself failed: we cannot confirm there was
        // nothing to protect, so fail closed and verify anyway rather
        // than silently skipping — `compare_host_integrity` reads a
        // failed `pre` as `Unverified` regardless of `post`.
        None => {
            return Some(compare_host_integrity(
                pre,
                &ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            ))
        }
    };

    let needs_verification =
        !pre_report.references_inside.is_empty() || !pre_report.unresolved.is_empty();
    if !needs_verification {
        return None;
    }

    let post = host_integrity::restat_post_action(pre_report, &host_integrity::FsDependencyRestat);
    Some(compare_host_integrity(pre, &post))
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
    // HORO-1825 §14 (same shape as the §14-flagged `TargetOwnershipUnknown`
    // addendum for HORO-1824): `ExecutableDependencyUnknown` is a
    // degraded-completeness reason, not a plain active-use veto. Without
    // this, a plan consented to under `ExecutableDependencyUnknown` would
    // compute `planned_rank = 0` while fresh revalidation correctly
    // computes a higher rank, tripping a spurious `EvidenceDegraded` abort
    // on every execution of an executable-bearing kind whose dependency
    // probe is unresolved. Fails closed either way; this just keeps that
    // class executable once it legitimately clears.
    // HORO-1824 §14 (the explicit addendum the ADR carries forward from
    // HORO-1823's own reviewer finding on HORO-1825): `TargetOwnershipUnknown`
    // is a degraded-completeness reason too, same shape as
    // `ExecutableDependencyUnknown` above. Without this, a plan consented
    // to under `TargetOwnershipUnknown` would compute `planned_rank = 0`
    // while fresh revalidation correctly computes a higher rank, tripping
    // a spurious `EvidenceDegraded` abort on every execution of a Cargo
    // target dir whose ownership probe is unresolved. Fails closed either
    // way; this just keeps that class executable once it legitimately
    // clears.
    if decision.reasons.contains(&ReasonCode::EvidenceIncomplete)
        || decision
            .reasons
            .contains(&ReasonCode::ExecutableDependencyUnknown)
        || decision
            .reasons
            .contains(&ReasonCode::TargetOwnershipUnknown)
    {
        return 1;
    }
    0
}

/// Rebuilds an [`Evidence`] for `resource` the same way a detector would
/// at discovery time (see [`crate::detectors::discovery_evidence`]),
/// then merges in a fresh correlation pass via `collector`. Used only for
/// deletion-time revalidation — never a substitute for a real detector
/// pass.
pub(crate) fn build_fresh_evidence(
    resource: &ResourceId,
    action: &dyn Action,
    collector: &dyn EvidenceCollector,
    now: SystemTime,
) -> Evidence {
    let path = match &resource.locator {
        ResourceLocator::Path(p) => Some(p.as_path()),
        ResourceLocator::Tool { .. } => None,
    };

    let size_estimate = path.map(|p| estimate_logical_bytes(p, size_estimate_budget()));
    let logical_bytes = match &size_estimate {
        Some(estimate) => estimate.bytes.clone(),
        None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    };
    let is_lower_bound = size_estimate.as_ref().is_some_and(|e| e.is_lower_bound());
    let last_modified = match path {
        Some(p) => probe_mtime(p),
        None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    };
    let fingerprint_path: &Path = path.unwrap_or_else(|| Path::new(""));

    // HORO-994 fix: reuse the SAME `reclaimable_bytes == logical_bytes`
    // equivalence HORO-992 established for detectors, on this revalidation
    // rebuild too. Before this fix, this always reported `NotAttempted`
    // here regardless of what the originating detector observed at
    // discovery time — that degraded `Completeness`/reason set relative
    // to plan time on every single real execution, tripping
    // `PolicyClassDowngraded`/`PolicyReasonsWidened`/`EvidenceDegraded`
    // and aborting every real cleanup unconditionally. `ResourceLocator::Tool`
    // resources (Docker) have no equivalent path-based estimate and stay
    // `Unavailable` — Docker has no registered cleanup action anyway.
    // HORO-1016: `logical_bytes` above is now the same bounded *recursive*
    // estimate a detector uses at discovery time (same shared budget, see
    // `crate::detectors::size_estimate_budget`), so `detect`'s reported
    // size and `clean`'s revalidated size agree for an unchanged resource.
    let reclaimable_bytes = logical_bytes.clone();

    let mut evidence = discovery_evidence(
        resource.clone(),
        DetectorId("executor_revalidation"),
        fingerprint_path,
        logical_bytes,
        reclaimable_bytes,
        is_lower_bound,
        last_modified,
        resource.kind.regenerability(),
        action.recoverability(),
        NativeCleanup::Unsupported,
    );

    // HORO-1824 §13 item 4 (the HORO-994 trap, for this ticket's half):
    // `cargo_target_scope` must be re-resolved HERE, at revalidation time,
    // not merely carried over from discovery — dropping this population
    // would make `TargetOwnership` permanently `Unavailable` on every
    // revalidation, which `Evidence::completeness()` reads as missing,
    // which aborts EVERY real cleanup of a Cargo target dir unconditionally
    // (the exact trap HORO-994 fixed for `reclaimable_bytes` above).
    if resource.kind == crate::evidence::ResourceKind::CargoTargetDir {
        if let Some(target_path) = path {
            // Same fallback the action itself uses (see
            // `crate::actions::cargo::manifest_path_for`): the discovered
            // project root when known, else the target dir's own parent.
            let project_root = resource
                .source_project_root
                .clone()
                .or_else(|| target_path.parent().map(Path::to_path_buf));
            if let Some(project_root) = project_root {
                let cargo_home_override = std::env::var("CARGO_HOME").ok();
                // `executor` is outside `src/detectors`, so reading the
                // real ambient `$HOME` here (unlike inside
                // `detectors::cargo`) is the intended production
                // behavior: revalidation wants Cargo's REAL current
                // answer, exactly as if the user ran `cargo clean`
                // themselves right now.
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/"));
                evidence.cargo_target_scope =
                    Some(crate::detectors::cargo::resolve_cargo_target_scope(
                        &project_root,
                        target_path,
                        cargo_home_override.as_deref(),
                        &home,
                    ));
            } else {
                evidence.cargo_target_scope = Some(ProbeOutcome::Unavailable(ProbeReason::Failed));
            }
        } else {
            evidence.cargo_target_scope =
                Some(ProbeOutcome::Unavailable(ProbeReason::NotAttempted));
        }
    }

    let correlation = collector.collect(
        resource,
        ProbeBudget {
            timeout: REVALIDATION_TIMEOUT,
        },
    );
    merge_into(&mut evidence, correlation);

    if resource.kind.is_managed_resource() {
        evidence.managed_ownership = Some(match path {
            Some(path) => crate::ownership::collect_ownership(path, now),
            None => ProbeOutcome::Unavailable(ProbeReason::Failed),
        });
    }

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
        // Nothing was mutated — there is no run to verify integrity for.
        host_integrity: None,
        // Nothing was mutated — no delta to measure. `execute()` itself
        // fills this in for its own real before/after sample (this
        // constructor is only reached for an abort BEFORE `execute_plan`
        // ever runs).
        volume_free_delta: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
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
        // These structural refusals fire before any step runs (see
        // `structural_refusal`/`execute_plan`'s early return sites),
        // before a mutation that would make host integrity meaningful to
        // check. `execute()` itself fills this in for failures that DO
        // reach a mutating step (see its own post-`execute_plan` step).
        host_integrity: None,
        // Same reasoning: `execute()` overwrites this unconditionally
        // after `execute_plan` returns for every call reached through
        // that path; this default only matters for the `action.plan()`
        // error path in `execute()` itself, which never reaches
        // `execute_plan` at all.
        volume_free_delta: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
    }
}

/// The refusal message for a mutating `RunTool` step carrying no
/// `scoped_path`. A function rather than a literal at each site so
/// [`structural_refusal`] and [`execute_plan`] cannot drift apart in what
/// they say about the same refusal.
fn unscoped_run_tool_refusal(program: &str) -> String {
    format!(
        "refusing to run {program}: this step has no scoped_path, so its identity cannot be \
         revalidated before mutation — an unscoped mutating action is never executed regardless \
         of policy class"
    )
}

/// The refusal message for a `scoped_path` that is not one of the values the
/// same step passes to the tool (HORO-1005). See
/// [`unscoped_run_tool_refusal`] for why this is a function.
fn decoy_scoped_path_refusal(program: &str, scoped: &Path) -> String {
    format!(
        "refusing to run {program}: scoped_path {} does not appear in this step's own args — the \
         executor cannot confirm what this command will actually mutate",
        scoped.display()
    )
}

/// Every reason [`execute_plan`] refuses `plan` outright, *before* mutating
/// anything, or `None` if the plan clears all of them.
///
/// This is the single source of truth for "would execution refuse this plan
/// on sight?", and it exists so that the actionability a candidate is
/// *offered* with can be derived from the same rule execution enforces
/// rather than restated independently. `execute_plan` returns these
/// verbatim, and [`crate::reporting::dto`] consults it to decide
/// `executable`/`offered_actions`/`refusal_reason` — so the product can no
/// longer present a Clean action whose execution is already known to be
/// impossible (HORO-1358).
///
/// Deliberately **structural only**: it reads the shape of the plan's steps
/// and nothing else — never a policy class, never a policy label, never
/// live filesystem state. That matters in both directions:
///
/// - It is not a policy check, so consulting it from reporting does not
///   move any policy decision out of [`crate::policy`].
/// - Refusals that depend on runtime state (a missing identity snapshot, a
///   fingerprint that changed under us, a tool that fails to spawn) are
///   deliberately **not** here. Reporting cannot honestly predict those, and
///   a candidate that clears this predicate is therefore "not already
///   refused", never "guaranteed to succeed".
pub fn structural_refusal(plan: &ActionPlan) -> Option<String> {
    if plan.steps.is_empty() {
        return Some(
            "refusing to execute an empty plan: a plan with zero steps would otherwise fall \
             through to a false ExecutionOutcome::Succeeded report despite mutating nothing"
                .to_string(),
        );
    }
    if plan.steps.len() > 1 {
        return Some(format!(
            "refusing to execute a {}-step plan: multi-step plans are not yet supported \
             (partial-deletion byte accounting would be discarded on a later-step failure)",
            plan.steps.len()
        ));
    }
    // Checked for every step, not just the first, so this stays correct if
    // multi-step plans are ever supported: nothing may mutate until every
    // step in the plan has cleared its structural gate.
    for step in &plan.steps {
        match step {
            ActionStep::RunTool {
                tool,
                args,
                scoped_path,
            } => match scoped_path {
                None => return Some(unscoped_run_tool_refusal(tool.program())),
                Some(scoped) => {
                    let scoped_str = scoped.to_string_lossy();
                    if !args.iter().any(|a| a.as_str() == scoped_str) {
                        return Some(decoy_scoped_path_refusal(tool.program(), scoped));
                    }
                }
            },
            // A `DeletePath` step's guards (`delete_path_with_guard`) are
            // all runtime-state checks, so by the rule above none of them
            // belongs here.
            ActionStep::DeletePath { .. } => {}
        }
    }
    None
}

/// Runs every step of `plan` for real. No `unwrap()`/`panic!` — every
/// fallible step produces `ExecutionOutcome::Failed(reason)` instead of
/// crashing.
///
/// `identity_snapshot` is the early, pre-slow-I/O filesystem identity
/// captured by `execute()` before revalidation ran (see its step 0
/// comment). Every mutating step re-verifies against it immediately before
/// mutating — see [`verify_identity_unchanged`].
///
/// Structural constraint (see PR "Known limitations"): a plan with more
/// than one step is rejected outright rather than executed, because
/// `actual_bytes_total` accounting below has no way to report a partial
/// total if a later step fails after an earlier `DeletePath` step already
/// succeeded. No registered `Action::plan` implementation emits more than
/// one step today; this is a guard against that invariant silently
/// breaking in the future, not a currently-reachable path.
fn execute_plan(plan: ActionPlan, identity_snapshot: Option<IdentitySnapshot>) -> ExecutionReport {
    // Every structural refusal, evaluated over the whole plan before any
    // step runs — see [`structural_refusal`], which reporting consults too
    // so a candidate is never offered as executable when this would fire.
    if let Some(message) = structural_refusal(&plan) {
        return failed_report(
            plan.action,
            plan.resource,
            message,
            plan.expected_reclaimed_bytes,
        );
    }

    let mut actual_bytes_total: u64 = 0;
    let mut saw_run_tool = false;
    let mut saw_delete = false;

    for step in &plan.steps {
        match step {
            ActionStep::RunTool {
                tool,
                args,
                scoped_path,
            } => {
                saw_run_tool = true;
                let scoped = match scoped_path {
                    Some(scoped) => scoped,
                    None => {
                        // Fail-closed, not an accepted trade-off: a
                        // mutating RunTool step with no scoped path has
                        // nothing for verify_identity_unchanged to check,
                        // which means it would run with zero TOCTOU/
                        // path-safety guard — the exact hole the HORO-951
                        // adversarial review closed for every other
                        // mutating step. `brew cleanup -s` (the one
                        // built-in action that hits this) genuinely can't
                        // be scoped to a path, so it is refused here
                        // unconditionally rather than executed unguarded.
                        //
                        // `structural_refusal` above already rejected this
                        // plan, so this arm is unreachable today. It stays
                        // as the last line of defence rather than becoming
                        // an `unreachable!()`: the invariant this protects
                        // is "no unguarded mutation, ever", and a panic —
                        // or a fallthrough — would be a worse answer than a
                        // refusal if the two ever diverge.
                        return failed_report(
                            plan.action,
                            plan.resource,
                            unscoped_run_tool_refusal(tool.program()),
                            plan.expected_reclaimed_bytes,
                        );
                    }
                };
                // HORO-1005: a `scoped_path` is only meaningful if it is
                // actually one of the values this same step is about to
                // pass to `tool` — otherwise the identity guard below would
                // faithfully verify a path that has nothing to do with what
                // the command will really mutate. This is a structural
                // check enforced by the executor itself, not trust in
                // whatever a given `Action::plan()` implementation chose to
                // put in `scoped_path`. Match the exact string
                // representation a real `Action::plan()` uses when building
                // `args` (see `CargoCleanTargetDir::plan`, which pushes
                // `target_dir.to_string_lossy().into_owned()`) rather than
                // some other `Path`/`PathBuf` comparison that could differ
                // in representation despite denoting the same path.
                // As with the `None` arm above, `structural_refusal` has
                // already rejected this case; kept here as defence in depth.
                let scoped_str = scoped.to_string_lossy();
                if !args.iter().any(|a| a.as_str() == scoped_str) {
                    return failed_report(
                        plan.action,
                        plan.resource,
                        decoy_scoped_path_refusal(tool.program(), scoped),
                        plan.expected_reclaimed_bytes,
                    );
                }
                let snapshot = match &identity_snapshot {
                    Some(s) => s,
                    None => {
                        return failed_report(
                            plan.action,
                            plan.resource,
                            format!(
                                "refusing to run {}: no verified resource identity snapshot \
                                 available for scoped path {}",
                                tool.program(),
                                scoped.display()
                            ),
                            plan.expected_reclaimed_bytes,
                        );
                    }
                };
                if let Err(message) = verify_identity_unchanged(scoped, snapshot) {
                    return failed_report(
                        plan.action,
                        plan.resource,
                        message,
                        plan.expected_reclaimed_bytes,
                    );
                }
                let mut command = Command::new(tool.program());
                command.args(args);
                // Security (flagged by independent review, same finding as
                // HORO-1824's resolver): when the real tool is Cargo via
                // rustup's proxy, the command inherits GLOMERIS'S OWN
                // process cwd, which is not necessarily the fixed,
                // untrusted `project_root` the resolver worries about, but
                // is still whatever directory the user happened to launch
                // this CLI from — pin the same way for defense in depth,
                // so a `rust-toolchain.toml` anywhere above THAT cwd
                // cannot steer which toolchain actually runs `cargo
                // clean`. A pin that cannot be established is not applied
                // (the command still runs under the ambient default
                // rather than being blocked here — this spawn, unlike the
                // read-only resolver, has independent authorization via
                // `authorize()`/`verify_identity_unchanged` already
                // passed above, so failing the real cleanup action closed
                // on a transient `rustup` hiccup would be its own
                // availability regression).
                if matches!(tool, crate::actions::ToolBinary::Cargo) {
                    command.env("RUSTUP_AUTO_INSTALL", "0");
                    if let Some(home) = std::env::var_os("HOME") {
                        // Unlike the read-only resolver (which fails
                        // closed on `Unresolved`), this spawn proceeds
                        // without a pin either way: the action already
                        // passed `authorize()`/`verify_identity_unchanged`
                        // above and runs with EXACT `--manifest-path`/
                        // `--target-dir` args rather than relying on cwd
                        // for identity, so blocking an already-authorized
                        // cleanup over a transient `rustup` resolution
                        // hiccup would be its own availability
                        // regression for no corresponding safety gain.
                        if let crate::detectors::cargo::ToolchainPin::Pinned(toolchain) =
                            crate::detectors::cargo::resolve_trusted_toolchain(Path::new(&home))
                        {
                            command.env("RUSTUP_TOOLCHAIN", toolchain);
                        }
                    }
                }
                match command.output() {
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
        // Filled in by `execute()` immediately after this call returns —
        // see `compute_host_integrity`. `execute_plan` itself has no
        // pre-action snapshot or collector to compare against.
        host_integrity: None,
        // Filled in by `execute()` immediately after this call returns —
        // see its own before/after sampling around `execute_plan`.
        volume_free_delta: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
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

/// Re-stats `path` immediately before a destructive mutation and refuses
/// unless it is bit-for-bit the same filesystem object `snapshot` was
/// taken from: same (dev, ino), and NOT a symlink — unconditionally,
/// regardless of what the original snapshot looked like, since a
/// destructive action must never operate through a symlink no matter when
/// it appeared. This closes the multi-second correlation-pass TOCTOU
/// window down to a residual gap whose size depends on the caller:
///
/// - For `DeletePath`, no further I/O happens between this call and the
///   `fs::remove_dir_all`/`remove_file` syscall — the residual window is
///   the true, irreducible gap between one stat and one syscall, an
///   inherent OS-level limitation not fixable in userspace without holding
///   an open directory file descriptor across the whole operation.
/// - For a `RunTool` step whose `scoped_path` this guards, the residual
///   window is materially wider: stat → `Command::spawn` (fork/exec) →
///   the invoked tool's OWN re-resolution of the same path (e.g. `cargo`
///   re-opening `--target-dir` itself). This function cannot close that
///   second window — it only guarantees the path's identity was correct
///   at the moment the tool was about to be spawned, not that the tool's
///   own subsequent access re-validates it. Both windows are accepted
///   residual risk; only the multi-second correlation-pass window (the
///   actual TOCTOU defect this function fixes) is closed by design.
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
    use crate::actions::{CargoCleanTargetDir, NodeCleanNodeModules, ToolBinary};
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

    // --- HORO-1826 §4.4: `compute_volume_delta` unit tests ---------------

    #[test]
    fn volume_delta_is_unavailable_when_no_sampler_was_supplied() {
        assert_eq!(
            compute_volume_delta(Some(1), None, None),
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    #[test]
    fn volume_delta_reports_signed_bytes_freed() {
        let delta = compute_volume_delta(Some(7), Some((7, 1_000)), Some((7, 1_500)));
        assert_eq!(delta, ProbeOutcome::Observed(500));
    }

    /// Concurrent target regrowth (ticket AC): free bytes can go DOWN
    /// between the before/after samples even though the action itself
    /// only deleted things — a concurrent writer outpaced it. This must
    /// be reported as a genuine negative `Observed` delta, never clamped
    /// to zero and never read as a probe failure.
    #[test]
    fn volume_delta_reports_negative_bytes_on_concurrent_regrowth() {
        let delta = compute_volume_delta(Some(7), Some((7, 1_000)), Some((7, 400)));
        assert_eq!(delta, ProbeOutcome::Observed(-600));
    }

    /// Different filesystem (ADR-0001 §4.4 / §8): the resource's own
    /// `st_dev` disagreeing with the measured volume's `st_dev` must
    /// produce `Unavailable`, never a byte count that compares two
    /// unrelated filesystems.
    #[test]
    fn volume_delta_is_unavailable_when_resource_is_on_a_different_filesystem() {
        let delta = compute_volume_delta(Some(7), Some((9, 1_000)), Some((9, 1_500)));
        assert_eq!(delta, ProbeOutcome::Unavailable(ProbeReason::Failed));
    }

    /// The mount itself changing between the two samples (an external
    /// disk swapped mid-action) must also fail closed.
    #[test]
    fn volume_delta_is_unavailable_when_the_mount_changes_between_samples() {
        let delta = compute_volume_delta(Some(7), Some((7, 1_000)), Some((9, 1_500)));
        assert_eq!(delta, ProbeOutcome::Unavailable(ProbeReason::Failed));
    }

    #[test]
    fn volume_delta_is_unavailable_when_either_sample_is_missing() {
        assert_eq!(
            compute_volume_delta(Some(7), None, Some((7, 1_500))),
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            compute_volume_delta(Some(7), Some((7, 1_000)), None),
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    /// `host_integrity` and `volume_free_delta` are independent
    /// dimensions: a negative (regrowth) delta must never influence the
    /// host-dependency verdict, and vice versa. Exercised end to end
    /// through `execute()` with a fake `VolumeDelta` that reports
    /// regrowth alongside a clean, empty dependency probe.
    struct FixedVolumeDelta {
        before: (u64, u64),
        after: (u64, u64),
        calls: AtomicU64,
    }

    impl VolumeDelta for FixedVolumeDelta {
        fn sample(&self, _path: &Path) -> std::io::Result<(u64, u64)> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(if call == 0 { self.before } else { self.after })
        }
    }

    #[test]
    fn execute_reports_regrowth_delta_independent_of_clean_host_integrity() {
        let root = make_temp_dir("execute-volume-delta-regrowth");
        pin_cargo_target_dir_locally(&root);
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
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

        // The mount's `st_dev` must agree with the resource's own
        // identity snapshot dev for the delta to be `Observed` at all.
        let resource_dev = {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(&target_dir).unwrap().dev()
        };
        let volume = FixedVolumeDelta {
            before: (resource_dev, 10_000),
            // Free bytes went DOWN despite a successful delete: a
            // concurrent writer outpaced it.
            after: (resource_dev, 3_000),
            calls: AtomicU64::new(0),
        };

        let report = execute(
            &CargoCleanTargetDir,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            Some(&volume),
        );

        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        assert_eq!(report.volume_free_delta, ProbeOutcome::Observed(-7_000));
        // A clean resource with nothing to protect skips host-integrity
        // entirely -- the regrowth above must not have changed that.
        assert!(report.host_integrity.is_none());

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1824: pins `cargo metadata`'s target-dir resolution to the
    /// fixture's own local `target/`, overriding any machine-wide
    /// `~/.cargo/config.toml` the developer running this test happens to
    /// have (e.g. a shared `target-dir` redirect — see
    /// `~/.claude/CLAUDE.md`'s own documented incident on this very
    /// machine). Without this, a real-fixture test's `resolve_cargo_target_scope`
    /// call at revalidation time would resolve against the DEVELOPER'S
    /// real global config rather than the fixture's own intent, and could
    /// legitimately observe `Redirected` purely as an artifact of the
    /// machine it runs on — not a reflection of anything this test means
    /// to exercise. A project-local config always wins over the user-level
    /// one, which is exactly the override a real project wanting a local
    /// target dir would also write.
    fn pin_cargo_target_dir_locally(root: &Path) {
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"target\"\n",
        )
        .unwrap();
    }

    fn evidence_for(resource_path: PathBuf, kind: ResourceKind) -> Evidence {
        let cargo_target_scope = (kind == ResourceKind::CargoTargetDir).then(|| {
            ProbeOutcome::Observed(crate::evidence::CargoTargetScope {
                resolved_target: resource_path.clone(),
                resolved_build_dir: None,
                workspace_root: resource_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| resource_path.clone()),
                scope: crate::evidence::TargetScope::Local,
                is_direct_locator: true,
            })
        });
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
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            docker_lifecycle: None,
            executable_dependency: ProbeOutcome::Observed(
                crate::evidence::ExecutableDependencyReport::empty(),
            ),
            managed_ownership: None,
            cargo_target_scope,
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    /// HORO-1824 §14 addendum: `TargetOwnershipUnknown` must rank as
    /// degraded completeness (1), same as `EvidenceIncomplete` and
    /// `ExecutableDependencyUnknown` — not 0. Without this, a plan
    /// consented to under `TargetOwnershipUnknown` computes
    /// `planned_rank = 0` while fresh revalidation (which re-resolves
    /// `cargo_target_scope` and may legitimately still be `Unavailable`)
    /// computes `fresh_rank = 1`, tripping a spurious
    /// `AbortReason::EvidenceDegraded` on every execution of that class —
    /// the exact bug class HORO-1823's reviewer found in HORO-1825's own
    /// `ExecutableDependencyUnknown` handling, reproduced proactively here
    /// rather than waiting for a reviewer to find it again.
    #[test]
    fn target_ownership_unknown_ranks_as_degraded_completeness_not_zero() {
        let now = SystemTime::now();
        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/tmp/x/target")),
        );
        let decision = PolicyDecision {
            resource,
            class: PolicyClass::Ask,
            reasons: vec![crate::policy::ReasonCode::TargetOwnershipUnknown],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 3,
        };
        assert_eq!(completeness_rank_from_decision(&decision), 1);
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
                executable_dependency: ProbeOutcome::Observed(
                    crate::evidence::ExecutableDependencyReport::empty(),
                ),
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
            None,
        );

        assert_eq!(
            report.outcome,
            ExecutionOutcome::AbortedByRevalidation(AbortReason::ResourceIdentityChanged)
        );

        fs::remove_dir_all(&root).ok();
    }

    /// A collector that reports the resource as actively in use by a
    /// live process — used to force a fresh `classify()` read to stay in
    /// the same `PolicyClass::Ask` bucket as the planned decision while
    /// still carrying a genuinely different reason code, so the
    /// reason-widening check (step 6) is exercised in isolation from the
    /// class check (step 5).
    struct ActiveUseCollector;
    impl EvidenceCollector for ActiveUseCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(vec![ProcessRef::new(
                    1,
                    "node".to_string(),
                )]),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
                executable_dependency: ProbeOutcome::Observed(
                    crate::evidence::ExecutableDependencyReport::empty(),
                ),
            }
        }
    }

    /// Proves the reason-widening abort path (step 6): the fresh decision
    /// stays the same `PolicyClass` (`Ask`) as the approved decision —
    /// evidence is complete on both sides (post-HORO-994, `reclaimable_bytes`
    /// is populated on revalidation the same way a detector reports it at
    /// plan time), but a live process is observed using the resource at
    /// revalidation time, which `classify()` surfaces as
    /// `Ask{ResourceInActiveUse}` — a reason the plan-time consent (granted
    /// against `Ask{RebuildCostHigh}`) never covered.
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
            &ActiveUseCollector,
            &PolicyConfig::default(),
            now,
            None,
        );

        assert_eq!(
            report.outcome,
            ExecutionOutcome::AbortedByRevalidation(AbortReason::PolicyReasonsWidened)
        );
        // Nothing was mutated.
        assert!(node_modules.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// Full happy-path real execution: an `AutoSafe` approval — matching
    /// what a real detector plus `classify()` would produce for a clean,
    /// complete, regenerable-by-rebuild `node_modules` (post-HORO-994,
    /// `build_fresh_evidence` populates `reclaimable_bytes` the same way
    /// a detector does, so the fresh revalidation reproduces the
    /// identical class/reasons) — so `execute()` proceeds to actually
    /// delete a real disposable `node_modules` fixture.
    #[test]
    fn untracked_data_created_after_approval_aborts_without_artifact_change() {
        use crate::evidence::correlate::{GitCliProbe, GitProbe};

        struct FreshGitCollector(PathBuf);
        impl EvidenceCollector for FreshGitCollector {
            fn collect(&self, id: &ResourceId, budget: ProbeBudget) -> CorrelationResult {
                let timeout = budget.timeout;
                let mut result = FakeCollector.collect(id, budget);
                result.git_state = GitCliProbe.state_of(&self.0, timeout);
                result
            }
        }
        let root = make_temp_dir("execute-untracked-race");
        let run = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        };
        run(&["init"]);
        run(&["config", "user.name", "Fixture"]);
        run(&["config", "user.email", "fixture@example.invalid"]);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        run(&["add", ".gitignore"]);
        run(&["commit", "-m", "Establish disposable fixture"]);
        let cache = root.join("node_modules");
        fs::create_dir_all(&cache).unwrap();
        let payload = cache.join("owned-fixture.bin");
        fs::write(&payload, vec![0u8; 4096]).unwrap();
        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(cache.clone()),
        );
        let fingerprint = fingerprint_of(&cache);
        let now = SystemTime::now();
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 5,
        };
        let approval = authorize(decision, fingerprint.clone(), None).unwrap();
        let unique = root.join("unique-owner.txt");
        fs::write(&unique, "preserve").unwrap();
        assert_eq!(fingerprint_of(&cache), fingerprint);
        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &FreshGitCollector(root.clone()),
            &PolicyConfig::default(),
            now,
            None,
        );
        assert_eq!(
            report.outcome,
            ExecutionOutcome::AbortedByRevalidation(AbortReason::PolicyClassDowngraded)
        );
        assert_eq!(fs::read_to_string(&unique).unwrap(), "preserve");
        assert!(payload.exists());
        fs::remove_dir_all(root).unwrap();
    }

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
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            None,
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
        pin_cargo_target_dir_locally(&root);
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
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let report = execute(
            &CargoCleanTargetDir,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            None,
        );

        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        assert!(!target_dir.exists());
        assert_eq!(
            report.actual_reclaimed_bytes,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1824 ticket fixture: a target dir missing `CACHEDIR.TAG`.
    /// Real `cargo clean --target-dir <path>` refuses to touch a
    /// directory that does not carry this marker (it errors rather than
    /// deleting), because without it cargo cannot tell the directory is
    /// actually a build output directory and not, say, a user's real
    /// `target/` source folder that happens to share the name. Glomeris
    /// must surface this as a real execution failure — never silently
    /// succeed, never fall back to a broader delete — and the directory
    /// must survive untouched.
    #[test]
    fn execute_real_run_tool_fails_closed_when_cachedir_tag_is_missing() {
        let root = make_temp_dir("execute-real-cargo-no-cachedir-tag");
        pin_cargo_target_dir_locally(&root);
        let target_dir = root.join("target");
        fs::create_dir_all(target_dir.join("debug")).unwrap();
        // Deliberately no CACHEDIR.TAG written here.
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
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let report = execute(
            &CargoCleanTargetDir,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            None,
        );

        match report.outcome {
            ExecutionOutcome::Failed(_) => {}
            other => panic!("expected Failed(_) when CACHEDIR.TAG is missing, got {other:?}"),
        }
        assert!(
            target_dir.exists(),
            "a refused cargo clean must leave the directory untouched"
        );
        assert!(target_dir.join("debug/build_output.bin").exists());

        fs::remove_dir_all(&root).ok();
    }

    /// A collector whose `executable_dependency` answer changes between
    /// calls — the first call stands in for `execute()`'s pre-action
    /// revalidation (inside `build_fresh_evidence`), the second for
    /// HORO-1826's post-action probe (`compute_host_integrity`). Every
    /// OTHER correlation field is held fixed and clean, so this isolates
    /// the host-integrity wiring from everything else `execute()` checks.
    struct StatefulDependencyCollector {
        calls: std::cell::Cell<u32>,
        pre: crate::evidence::ExecutableDependencyReport,
        post: crate::evidence::ExecutableDependencyReport,
    }

    impl EvidenceCollector for StatefulDependencyCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            let call = self.calls.get();
            self.calls.set(call + 1);
            let report = if call == 0 {
                self.pre.clone()
            } else {
                self.post.clone()
            };
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::<ProcessRef>::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
                executable_dependency: ProbeOutcome::Observed(report),
            }
        }
    }

    /// AC: a pre-action probe that could not fully resolve every
    /// recognized source (`unresolved` non-empty, `ExecutableDependencyUnknown`
    /// — real enough to reach `execute()` under explicit `Ask` consent,
    /// unlike `references_inside`, which is unconditionally `PROTECTED`
    /// and therefore refused before `execute()` ever mutates anything)
    /// must surface a real `Some(HostIntegrityReport)` on the returned
    /// `ExecutionReport`, and that report's status must be `Unverified` —
    /// never silently `None`, and never a bare `Ok`.
    #[test]
    fn execute_reports_unverified_host_integrity_when_pre_action_probe_is_incomplete() {
        let root = make_temp_dir("execute-host-integrity-unverified");
        pin_cargo_target_dir_locally(&root);
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
        // `Ask`, not `AutoSafe`: `ExecutableDependencyUnknown` is a real
        // veto (ADR-0001 §5 step 6.2), so the approved decision must
        // already carry it for the fresh revalidation's reasons to not
        // "widen" beyond what was consented to.
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![crate::policy::ReasonCode::ExecutableDependencyUnknown],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("Ask decision with matching consent must authorize");

        let collector = StatefulDependencyCollector {
            calls: std::cell::Cell::new(0),
            pre: crate::evidence::ExecutableDependencyReport {
                references_inside: Vec::new(),
                running_inside: Vec::new(),
                unresolved: vec![crate::evidence::model::UnresolvedRef::SourceUnreadable {
                    source: crate::evidence::model::SourceTag::CodexHooks,
                }],
                sources_examined: vec![crate::evidence::model::SourceTag::CodexHooks],
            },
            post: crate::evidence::ExecutableDependencyReport::empty(),
        };

        let report = execute(
            &CargoCleanTargetDir,
            &approval,
            &collector,
            &PolicyConfig::default(),
            now,
            None,
        );

        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        let host_integrity = report
            .host_integrity
            .as_ref()
            .expect("an incomplete pre-action probe must produce Some(_), never None");
        assert_eq!(
            host_integrity.status,
            crate::evidence::model::IntegrityStatus::Unverified,
            "an incomplete pre-action sweep must never read as all-clear"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// AC: a resource whose pre-action probe cleanly found nothing to
    /// protect (empty `references_inside`, empty `unresolved`) must skip
    /// the post-action probe entirely — `host_integrity` is `None`, not a
    /// computed-but-trivial `Ok`. This is the "don't probe for an action
    /// with no dependencies at all" cost rule, confirmed by the collector
    /// never being called a second time.
    #[test]
    fn execute_skips_host_integrity_when_pre_action_probe_is_clean_and_empty() {
        let root = make_temp_dir("execute-host-integrity-skipped");
        pin_cargo_target_dir_locally(&root);
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
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let collector = StatefulDependencyCollector {
            calls: std::cell::Cell::new(0),
            pre: crate::evidence::ExecutableDependencyReport::empty(),
            // If this were ever consulted it would prove a wasted
            // post-probe ran; the assertion on `calls` below is the real
            // guard that it never is.
            post: crate::evidence::ExecutableDependencyReport {
                references_inside: Vec::new(),
                running_inside: Vec::new(),
                unresolved: vec![crate::evidence::model::UnresolvedRef::SourceUnreadable {
                    source: crate::evidence::model::SourceTag::CodexHooks,
                }],
                sources_examined: Vec::new(),
            },
        };

        let report = execute(
            &CargoCleanTargetDir,
            &approval,
            &collector,
            &PolicyConfig::default(),
            now,
            None,
        );

        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        assert!(
            report.host_integrity.is_none(),
            "nothing to protect pre-action must skip the check entirely, not report a trivial Ok"
        );
        assert_eq!(
            collector.calls.get(),
            1,
            "the post-action probe must never run when pre-action found nothing to verify"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// Regression for a real defect found by HORO-957's independent
    /// golden-scenario evaluation: `HomebrewCleanupCache`'s plan carries
    /// `scoped_path: None` (real `brew cleanup -s` has no path argument
    /// to scope to), and prior to this fix that meant `execute()` ran it
    /// with NO identity/TOCTOU guard at all — a real, unscoped destructive
    /// command reaching the developer's actual Homebrew Cellar the moment
    /// Homebrew cache classified `AutoSafe` (which the evaluator confirmed
    /// happens on a real machine). `execute()` must now refuse any
    /// `RunTool` step whose `scoped_path` is `None`, unconditionally,
    /// before ever spawning the tool. This test never spawns real `brew`
    /// — the refusal must happen before `Command::new` is reached.
    #[test]
    fn execute_refuses_run_tool_step_with_no_scoped_path() {
        use crate::actions::HomebrewCleanupCache;

        let root = make_temp_dir("execute-refuses-unscoped-run-tool");
        let cache_dir = root.join("homebrew-cache");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::write(cache_dir.join("download.tar.gz"), vec![0u8; 4096]).unwrap();

        let resource = ResourceId::new(
            ResourceKind::HomebrewCache,
            ResourceLocator::Path(cache_dir.clone()),
        );
        let now = SystemTime::now();
        // Build the decision from the SAME evidence-rebuild path `execute`
        // itself uses (rather than hand-writing reason codes and risking
        // drift from `classify`'s real output) so this test's plan-time
        // decision and execute()'s freshly re-derived decision agree —
        // exactly as they would for a real, unchanged resource.
        let evidence = build_fresh_evidence(&resource, &HomebrewCleanupCache, &FakeCollector, now);
        let decision = classify(&evidence, &PolicyConfig::default(), now);
        assert_eq!(
            decision.class,
            PolicyClass::AutoSafe,
            "test fixture must reach AutoSafe for this regression to exercise the real path"
        );
        let consent = UserConsent::new(resource.clone(), evidence.fingerprint.clone(), now);
        let approval = authorize(decision, evidence.fingerprint.clone(), Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let report = execute(
            &HomebrewCleanupCache,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            None,
        );

        match &report.outcome {
            ExecutionOutcome::Failed(message) => assert!(
                message.contains("no scoped_path"),
                "expected the unscoped-refusal message, got: {message}"
            ),
            other => panic!("expected Failed(_), got {other:?}"),
        }
        // The real point of this test: the cache dir must still exist —
        // `brew` was never actually spawned.
        assert!(cache_dir.exists());

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
                executable_dependency: ProbeOutcome::Observed(
                    crate::evidence::ExecutableDependencyReport::empty(),
                ),
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
        // AutoSafe, matching what a real detector + classify() compute
        // for this clean, complete, regenerable-by-rebuild `node_modules`
        // fixture — post-HORO-994, the fresh revalidation below also
        // reproduces AutoSafe (its `estimate_logical_bytes`/`probe_mtime`
        // reads happen against the REAL pre-swap directory, before
        // `SymlinkSwapCollector::collect()` performs the swap), so
        // classify/reasons-widening pass and control reaches the actual
        // pre-mutation identity guard (`verify_identity_unchanged`) this
        // test exists to exercise.
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::AutoSafe,
            reasons: vec![
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::NoActiveUseObserved,
            ],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        };
        let consent = UserConsent::new(resource.clone(), fingerprint.clone(), now);
        let approval = authorize(decision, fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

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
            None,
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

    /// Defect 2 regression test (HORO-951 adversarial review), using a REAL
    /// `cargo clean` invocation against two real disposable temp Cargo
    /// projects — a "victim" with a real built `target/`, and an "approver"
    /// whose own `target/` is a symlink to the victim's.
    ///
    /// Note on scope: the reviewer's original hypothesis was that `cargo
    /// clean --target-dir <symlink>` follows the link and wholesale-deletes
    /// the linked directory's *contents*. Hand-verified against the
    /// installed `cargo` (1.98.0) before writing this test: that specific
    /// data-loss outcome does NOT reproduce — this cargo version removes
    /// only the symlink directory entry itself (a real destructive action
    /// against something outside the approved resource, just not the one
    /// originally hypothesized) and leaves the victim's real files
    /// untouched. The underlying defect is still real and still exactly
    /// what the fix targets — spawning a path-scoped tool command against
    /// an unverified path is unsafe regardless of which particular cargo
    /// version's behavior makes it observable, and other tool
    /// versions/invocations are not guaranteed to be as forgiving. This
    /// test therefore proves the guard's actual, verifiable contract: given
    /// a symlinked scoped path, `execute()` must refuse BEFORE spawning
    /// `cargo` at all. That is directly observable here because real
    /// `cargo clean` provably does mutate the symlink entry itself when
    /// allowed to run (confirmed above) — so the symlink surviving
    /// untouched is proof the tool was never spawned.
    #[test]
    fn execute_refuses_cargo_clean_when_scoped_target_dir_is_a_symlink_to_another_project() {
        let victim_root = make_temp_dir("cargoguard-victim");
        fs::write(
            victim_root.join("Cargo.toml"),
            "[package]\nname = \"victim-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(victim_root.join("src")).unwrap();
        fs::write(victim_root.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        let victim_target = victim_root.join("target");

        let build_status = Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(victim_root.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&victim_target)
            .status()
            .expect("failed to spawn cargo build for victim fixture");
        assert!(build_status.success(), "victim fixture build must succeed");
        assert!(
            victim_target.join("debug").exists(),
            "victim fixture must have real build output before the test runs"
        );

        let approver_root = make_temp_dir("cargoguard-approver");
        pin_cargo_target_dir_locally(&approver_root);
        fs::write(
            approver_root.join("Cargo.toml"),
            "[package]\nname = \"approver-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(approver_root.join("src")).unwrap();
        fs::write(approver_root.join("src/lib.rs"), "pub fn y() {}\n").unwrap();
        let approver_target = approver_root.join("target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim_target, &approver_target)
            .expect("create approver target symlink pointing at victim");

        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(approver_target.clone()),
        );
        let fingerprint = fingerprint_of(&approver_target);
        let now = SystemTime::now();
        // HORO-1824: a `target/` that is itself a symlink to a
        // DIFFERENT project's target dir resolves `cargo_target_scope`
        // to `Redirected` (`canonical(resolved_target)` lands outside
        // `approver_root`, exactly ADR-0001 §4.2's "symlinked-out
        // target/" case) — `Ask{SharedBuildCacheBlastRadius}`, matching
        // what a real detector + classify() compute for this fixture,
        // not `AutoSafe`. The fresh revalidation below reproduces the
        // SAME decision (nothing about the symlink changes mid-flight in
        // this test), so reasons-widening still passes and control still
        // reaches the plan + pre-spawn identity guard this test exists to
        // exercise — just via `Ask`+consent rather than `AutoSafe`.
        let decision = PolicyDecision {
            resource: resource.clone(),
            class: PolicyClass::Ask,
            reasons: vec![ReasonCode::SharedBuildCacheBlastRadius],
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 3,
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
            None,
        );

        assert!(
            victim_target.join("debug").exists(),
            "victim's real build output must survive regardless"
        );
        assert!(
            fs::symlink_metadata(&approver_target).is_ok(),
            "the approver's target symlink must still exist — cargo must never have been \
             spawned; a real `cargo clean --target-dir <symlink>` provably removes this \
             symlink entry when allowed to run, so its survival proves the guard refused \
             before spawning the tool"
        );
        match &report.outcome {
            ExecutionOutcome::Failed(msg) => assert!(
                msg.contains("symlink") || msg.contains("identity changed"),
                "expected a symlink/identity refusal, got message: {msg}"
            ),
            other => panic!("expected ExecutionOutcome::Failed(...), got {other:?}"),
        }

        fs::remove_dir_all(&victim_root).ok();
        fs::remove_dir_all(&approver_root).ok();
    }

    /// Defect 3 regression test: a plan with more than one step is
    /// rejected outright by `execute_plan` rather than run — today no
    /// registered `Action::plan` emits more than one step, but this guards
    /// the structural invariant `actual_bytes_total` accounting relies on
    /// (a later step's failure must never silently discard an earlier
    /// `DeletePath` step's already-accumulated real byte total).
    #[test]
    fn execute_plan_rejects_multi_step_plans_without_touching_either_path() {
        let root = make_temp_dir("execute-plan-multistep");
        let a = root.join("a");
        fs::create_dir_all(&a).unwrap();
        fs::write(a.join("f"), b"x").unwrap();
        let b = root.join("b");
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("f"), b"y").unwrap();

        let resource = ResourceId::new(ResourceKind::NodeModules, ResourceLocator::Path(a.clone()));
        let plan = ActionPlan {
            action: ActionId("test.multi.step"),
            resource,
            steps: vec![
                ActionStep::DeletePath { path: a.clone() },
                ActionStep::DeletePath { path: b.clone() },
            ],
            expected_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            explain: "test-only multi-step plan".to_string(),
        };

        let report = execute_plan(plan, None);

        match &report.outcome {
            ExecutionOutcome::Failed(msg) => assert!(
                msg.contains("multi-step") || msg.contains("more than one"),
                "unexpected failure message: {msg}"
            ),
            other => panic!("expected ExecutionOutcome::Failed(...), got {other:?}"),
        }
        assert!(a.exists(), "first step must not have run");
        assert!(b.exists(), "second step must not have run");

        fs::remove_dir_all(&root).ok();
    }

    /// A zero-step plan must never fall through to a false `Succeeded`
    /// report — no registered `Action::plan` produces one today, but this
    /// guards defense-in-depth against a future buggy `Action` impl doing
    /// so silently.
    #[test]
    fn execute_plan_rejects_empty_plans_instead_of_reporting_false_success() {
        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(PathBuf::from("/tmp/glomeris-empty-plan-test")),
        );
        let plan = ActionPlan {
            action: ActionId("test.empty.plan"),
            resource,
            steps: vec![],
            expected_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            explain: "test-only empty plan".to_string(),
        };

        let report = execute_plan(plan, None);

        match &report.outcome {
            ExecutionOutcome::Failed(msg) => {
                assert!(msg.contains("empty"), "unexpected failure message: {msg}")
            }
            other => panic!("expected ExecutionOutcome::Failed(...), got {other:?}"),
        }
    }

    /// Locks `estimate_logical_bytes` and `total_size_best_effort`'s byte
    /// semantics together permanently (HORO-1016): both now count files
    /// and symlinks by their own size and skip a directory's own inode
    /// size, so they must agree exactly on the same real tree.
    #[test]
    fn estimate_matches_total_size_best_effort_on_the_same_tree() {
        let root = make_temp_dir("estimate-parity");
        fs::create_dir_all(root.join("nested/deeper")).unwrap();
        fs::write(root.join("top.bin"), vec![0u8; 1000]).unwrap();
        fs::write(root.join("nested/mid.bin"), vec![0u8; 2000]).unwrap();
        fs::write(root.join("nested/deeper/leaf.bin"), vec![0u8; 3000]).unwrap();

        let outside_target = std::env::temp_dir().join(format!(
            "glomeris-estimate-parity-symlink-target-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&outside_target, vec![0u8; 4096]).unwrap();
        std::os::unix::fs::symlink(&outside_target, root.join("link")).unwrap();

        let sparse = fs::File::create(root.join("sparse.bin")).unwrap();
        sparse.set_len(1 << 20).unwrap();
        drop(sparse);

        let estimate = estimate_logical_bytes(&root, crate::scanner::ScanBudget::unlimited());
        let best_effort = total_size_best_effort(&root);

        match estimate.bytes {
            ProbeOutcome::Observed(bytes) => assert_eq!(bytes, best_effort),
            other => panic!("expected Observed(_), got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
        fs::remove_file(&outside_target).ok();
    }

    /// Proves `build_fresh_evidence` now reports the full recursive total
    /// for a nested tree, not just what the old non-recursive probe would
    /// have summed at the top level (HORO-1016).
    #[test]
    fn build_fresh_evidence_reports_recursive_bytes_for_a_nested_tree() {
        let root = make_temp_dir("fresh-evidence-nested");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(node_modules.join("pkg/nested")).unwrap();
        // Nothing directly at the top level of `node_modules/` itself —
        // the old non-recursive probe would have reported ~0 bytes here.
        fs::write(node_modules.join("pkg/index.js"), vec![0u8; 5000]).unwrap();
        fs::write(node_modules.join("pkg/nested/deep.js"), vec![0u8; 7000]).unwrap();

        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(node_modules.clone()),
        );
        let now = SystemTime::now();

        let evidence = build_fresh_evidence(&resource, &NodeCleanNodeModules, &FakeCollector, now);

        match evidence.logical_bytes {
            ProbeOutcome::Observed(bytes) => assert_eq!(bytes, 12000),
            other => panic!("expected Observed(12000), got {other:?}"),
        }
        assert_eq!(evidence.reclaimable_bytes, evidence.logical_bytes);

        fs::remove_dir_all(&root).ok();
    }

    /// The key safety-invariant test (HORO-1016): a byte-magnitude
    /// mismatch between plan-time evidence and execute-time revalidation
    /// must never trip `AbortReason::ResourceIdentityChanged` /
    /// `PolicyClassDowngraded` / `PolicyReasonsWidened` / `EvidenceDegraded`
    /// — those are keyed off fingerprint/class/reason/completeness, never
    /// off the byte value itself.
    #[test]
    fn byte_estimate_magnitude_never_participates_in_revalidation() {
        let root = make_temp_dir("byte-magnitude-invariant");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(node_modules.join("pkg")).unwrap();
        fs::write(node_modules.join("pkg/index.js"), vec![0u8; 4096]).unwrap();

        let resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(node_modules.clone()),
        );
        let real_fingerprint = fingerprint_of(&node_modules);
        let now = SystemTime::now();

        // Deliberately wrong/fabricated byte value, simulating a stale
        // plan-time estimate that no longer matches the real fixture's
        // actual ~4096 bytes.
        let planned_evidence = Evidence {
            resource: resource.clone(),
            fingerprint: real_fingerprint.clone(),
            detector: crate::detectors::DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(999_999_999),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(999_999_999),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(now),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: resource.kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            docker_lifecycle: None,
            // HORO-1825: NodeModules now requires this field for
            // Completeness::Complete — a clean negative here, matching
            // every other correlation field's clean-happy-path value.
            executable_dependency: ProbeOutcome::Observed(
                crate::evidence::ExecutableDependencyReport::empty(),
            ),
            managed_ownership: None,
            cargo_target_scope: None,
            collected_at: now,
            sources: Vec::new(),
        };

        let decision = classify(&planned_evidence, &PolicyConfig::default(), now);
        assert_eq!(
            decision.class,
            PolicyClass::AutoSafe,
            "expected AutoSafe from clean fabricated-bytes evidence, got {decision:?}"
        );

        let consent = UserConsent::new(resource.clone(), real_fingerprint.clone(), now);
        let approval = authorize(decision, real_fingerprint, Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let report = execute(
            &NodeCleanNodeModules,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            None,
        );

        assert_eq!(
            report.outcome,
            ExecutionOutcome::Succeeded,
            "a byte-magnitude mismatch between plan-time (fabricated 999999999) and \
             execute-time (real ~4096) must never abort revalidation"
        );
        assert!(!node_modules.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// A deliberately-broken `Action` whose `plan()` supplies a
    /// `scoped_path` that does NOT appear anywhere in the step's own
    /// `args` — the exact structural gap HORO-1005 closes. Modeled on
    /// `CargoCleanTargetDir`, which is the one real action carrying a
    /// `Some(_)` `scoped_path`.
    struct DecoyScopedPathAction {
        decoy_path: PathBuf,
    }

    impl Action for DecoyScopedPathAction {
        fn id(&self) -> ActionId {
            ActionId("test.decoy.scoped_path_mismatch")
        }

        fn applies_to(&self) -> &'static [ResourceKind] {
            &[ResourceKind::CargoTargetDir]
        }

        fn required_evidence(&self) -> &'static [crate::evidence::model::EvidenceField] {
            &[]
        }

        fn recoverability(&self) -> Recoverability {
            Recoverability::RegenerableByRebuild
        }

        fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError> {
            Ok(ActionPlan {
                action: self.id(),
                resource: ev.resource.clone(),
                steps: vec![ActionStep::RunTool {
                    tool: ToolBinary::Cargo,
                    // Deliberately a COMPLETELY DIFFERENT path than
                    // `scoped_path` below — this is the mismatch the new
                    // guard must catch. The path does not exist, so even
                    // if the guard failed to catch this and `cargo` were
                    // spawned, this would only fail loudly (no manifest
                    // found), never touch anything real.
                    args: vec![
                        "clean".to_string(),
                        "--target-dir".to_string(),
                        "/some/other/real/path".to_string(),
                    ],
                    scoped_path: Some(self.decoy_path.clone()),
                }],
                expected_reclaimed_bytes: ev.reclaimable_bytes.clone(),
                explain: "test-only decoy plan".to_string(),
            })
        }
    }

    /// HORO-1005: proves the new structural guard fires when `scoped_path`
    /// does not appear in the step's own `args` — a fake `Action` supplies
    /// a `scoped_path` pointing at a real, controlled tempdir while `args`
    /// names a totally different, nonexistent path. `execute()` must
    /// refuse before ever spawning `cargo`, so neither path is ever
    /// touched or created.
    #[test]
    fn execute_refuses_run_tool_step_when_scoped_path_is_absent_from_args() {
        let root = make_temp_dir("execute-scoped-path-args-mismatch");
        pin_cargo_target_dir_locally(&root);
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"glomeris-test-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        let decoy_path = root.join("target");
        fs::create_dir_all(&decoy_path).unwrap();
        fs::write(decoy_path.join("marker.txt"), b"do not touch").unwrap();

        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(decoy_path.clone()),
        );
        let now = SystemTime::now();
        let action = DecoyScopedPathAction {
            decoy_path: decoy_path.clone(),
        };

        // Build the decision from the same evidence-rebuild path `execute`
        // itself uses, so plan-time and execute-time agree (same pattern
        // as `execute_refuses_run_tool_step_with_no_scoped_path` above).
        let evidence = build_fresh_evidence(&resource, &action, &FakeCollector, now);
        let decision = classify(&evidence, &PolicyConfig::default(), now);
        assert_eq!(
            decision.class,
            PolicyClass::AutoSafe,
            "test fixture must reach AutoSafe for this regression to exercise the real path"
        );
        let consent = UserConsent::new(resource.clone(), evidence.fingerprint.clone(), now);
        let approval = authorize(decision, evidence.fingerprint.clone(), Some(&consent))
            .expect("AutoSafe decision must always authorize");

        let report = execute(
            &action,
            &approval,
            &FakeCollector,
            &PolicyConfig::default(),
            now,
            None,
        );

        match &report.outcome {
            ExecutionOutcome::Failed(message) => {
                assert!(
                    message.contains("does not appear in this step's own args"),
                    "expected the scoped_path/args mismatch refusal, got: {message}"
                );
            }
            other => panic!("expected Failed(_), got {other:?}"),
        }
        // Nothing spawned: the decoy dir survives untouched, and the
        // bogus decoy path named in `args` was never created either.
        assert!(decoy_path.join("marker.txt").exists());
        assert!(!Path::new("/some/other/real/path").exists());

        fs::remove_dir_all(&root).ok();
    }
}
