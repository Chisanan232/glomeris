//! Deterministic `AUTO_SAFE`/`ASK`/`PROTECTED` classification (HORO-950).

use std::time::SystemTime;

use crate::evidence::{
    Completeness, DockerActivity, Evidence, ProbeOutcome, Recoverability, Regenerability,
    ResourceKind, TargetScope, UnresolvedRef,
};

use super::class::{PolicyClass, ReasonCode};
use super::config::PolicyConfig;
use super::decision::PolicyDecision;
use super::protected::protected_reason;

/// Classify one [`Evidence`] against `cfg`, as of `now`.
///
/// PURE: no I/O, no ambient clock — `now` is a parameter, never
/// `SystemTime::now()` called internally. This is what makes fail-closed
/// behavior testable, and what lets a future executor call this exact
/// function again on freshly re-collected evidence at deletion time
/// without any special-casing (the TOCTOU boundary between this
/// classification and actual deletion is closed by re-running `classify`
/// on fresh evidence, not by trusting a cached decision).
pub fn classify(ev: &Evidence, cfg: &PolicyConfig, now: SystemTime) -> PolicyDecision {
    let decision = |class: PolicyClass, reasons: Vec<ReasonCode>| PolicyDecision {
        resource: ev.resource.clone(),
        class,
        reasons,
        evidence_collected_at: ev.collected_at,
        evaluated_at: now,
        // HORO-1873: untracked work now vetoes unattended reclamation.
        // Fresh class/reason revalidation, rather than this version alone,
        // invalidates an approval collected before the work appeared.
        policy_version: 5,
    };

    // 1. Unknown resource kind is an unconditional, fail-closed Protected
    // sink — never falls through to any evidence-based judgment.
    if ev.resource.kind == ResourceKind::Unknown {
        return decision(
            PolicyClass::Protected,
            vec![ReasonCode::ProtectedUnknownResourceKind],
        );
    }

    // 2. Path/pattern-based protected check runs before any
    // freshness/completeness logic: a Protected classification never
    // depends on evidence freshness, so a stale-but-protected resource is
    // still Protected, never "upgraded" by fresher evidence.
    if let Some(reason) = protected_reason(&ev.resource) {
        return decision(PolicyClass::Protected, vec![reason]);
    }

    // 2b (HORO-1825 §5). A recognized hook/daemon/LaunchAgent config or a
    // PATH entry references a file inside this resource: it is an
    // installed executable dependency. Unconditionally Protected, same
    // rationale as step 2 — a stale positive is still Protected, never
    // "upgraded" by fresher evidence that simply hasn't run yet.
    if let Some(report) = ev.executable_dependency.observed() {
        if !report.references_inside.is_empty() {
            return decision(
                PolicyClass::Protected,
                vec![ReasonCode::ProtectedExecutableDependency],
            );
        }
    }

    // 2c (HORO-1868). A reference from a recognized source, or the
    // running-process probe, was EXAMINED but could not be confirmed
    // either way (`UnresolvedRef::is_examined_reference`) — unconditionally
    // Protected, same rationale as step 2b: a bare hook command that is
    // not currently on PATH may still run on the next invocation, so
    // "could not confirm it's absent" carries the same risk as "confirmed
    // present". Placed BEFORE the staleness/completeness checks (steps
    // 3-5) deliberately: this ordering is load-bearing. Placing it after
    // completeness would make it bypassable — a resource with
    // `Completeness::Partial` on an unrelated field, or stale evidence,
    // would return `Ask` from an earlier step before step 2c's veto is
    // ever reached, silently widening an examined-but-unresolved
    // executable dependency into a consentable ASK through a
    // Partial/stale shortcut.
    if ev.resource.kind.is_executable_bearing() {
        if let Some(report) = ev.executable_dependency.observed() {
            if report
                .unresolved
                .iter()
                .any(UnresolvedRef::is_examined_reference)
            {
                return decision(
                    PolicyClass::Protected,
                    vec![ReasonCode::ProtectedUnresolvedExecutableDependency],
                );
            }
        }
    }

    // 3. Staleness: evidence older than cfg.max_evidence_age (or whose
    // collected_at is somehow in the future, which duration_since reports
    // as an error) is untrustworthy -> Ask, never silently treated as
    // fresh.
    match now.duration_since(ev.collected_at) {
        Ok(age) if age <= cfg.max_evidence_age => {}
        _ => return decision(PolicyClass::Ask, vec![ReasonCode::EvidenceStale]),
    }

    // 4-5. Completeness gates.
    match ev.completeness() {
        Completeness::Failed => {
            return decision(PolicyClass::Ask, vec![ReasonCode::EvidenceProbeFailed]);
        }
        // HORO-1824 §5: a Partial whose ONLY gap is `TargetOwnership`
        // (`cargo metadata` absent/timed out/failed, §8) gets its own
        // specific reason rather than the generic `EvidenceIncomplete`,
        // so `UNKNOWN_INCOMPLETE` reporting and
        // `executor::completeness_rank_from_decision` can name the actual
        // gap. A Partial with other missing fields too (mixed) still maps
        // to the generic code — both are `Ask`, both are
        // evidence-quality, both rank the same in
        // `completeness_rank_from_decision`, so this is purely a
        // reporting-precision distinction, never a safety one.
        Completeness::Partial { missing }
            if missing == [crate::evidence::EvidenceField::TargetOwnership] =>
        {
            return decision(PolicyClass::Ask, vec![ReasonCode::TargetOwnershipUnknown]);
        }
        Completeness::Partial { .. } => {
            return decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        }
        Completeness::Complete => {}
    }

    // 6. Active-use signals, most-significant first.
    let mut active_use_reasons = Vec::new();

    let process_active = matches!(ev.open_by_process.observed(), Some(v) if !v.is_empty())
        || matches!(ev.process_cwd_match.observed(), Some(v) if !v.is_empty());
    if process_active {
        active_use_reasons.push(ReasonCode::ResourceInActiveUse);
    }

    let git_active = matches!(
        ev.git_state.observed(),
        Some(Some(g)) if g.dirty || g.untracked || g.worktree
    );
    if git_active {
        active_use_reasons.push(ReasonCode::GitWorktreeDirty);
    }

    let tool_live = matches!(ev.tool_liveness.observed(), Some(true));
    if tool_live {
        active_use_reasons.push(ReasonCode::OwningToolLive);
    }

    // Docker lifecycle (HORO-1544). `tool_liveness` above answers "is the
    // daemon up", which is one answer for every Docker object on the
    // machine and says nothing about which of them is in use. These two
    // read the per-object fact instead, and the `Unknown` arm is the point:
    // a Docker object whose activity was never established must not travel
    // the same path as one Docker positively reported as idle.
    if let Some(lifecycle) = &ev.docker_lifecycle {
        match lifecycle.activity {
            DockerActivity::Active => active_use_reasons.push(ReasonCode::DockerObjectInUse),
            DockerActivity::Unknown => active_use_reasons.push(ReasonCode::DockerActivityUnknown),
            DockerActivity::Inactive => {}
        }
        // An unanswered reference query is the same class of gap: "nothing
        // was found to reference this" and "the reference query did not
        // run" would otherwise both arrive as an empty referrer set.
        if !lifecycle.references.is_observed() {
            active_use_reasons.push(ReasonCode::DockerActivityUnknown);
        }
    }

    // HORO-1825 §5 step 6, in the specified order: a running dependency
    // vetoes before an unresolved one, matching "most-significant first".
    if let Some(report) = ev.executable_dependency.observed() {
        if !report.running_inside.is_empty() {
            active_use_reasons.push(ReasonCode::ExecutableRunningFromResource);
        }
        if !report.unresolved.is_empty() && ev.resource.kind.is_executable_bearing() {
            active_use_reasons.push(ReasonCode::ExecutableDependencyUnknown);
        }
    }

    // HORO-1824 §5 step 6.3: a Redirected Cargo target is shared by an
    // unbounded, unenumerable set of other workspaces — see
    // `TargetScope::Redirected`'s own doc comment. For the ORDINARY case
    // (`is_direct_locator`, i.e. this evidence is about a project's own
    // plain `<root>/target`), `Local`/`Orphan` add no reason: `Orphan`
    // means the configured root does NOT share this target (it resolves
    // elsewhere), so the directory this evidence is about is not itself
    // shared by anything this probe can see.
    //
    // Security (flagged by independent review): a resource reached via
    // discovery widening (`!is_direct_locator`) ALWAYS vetoes here,
    // regardless of its own recomputed `scope` — see
    // `CargoTargetScope::is_direct_locator`'s own doc comment for why
    // `scope` alone is not a safe signal for such a resource (it can
    // recompute `Local` at revalidation purely because its locator
    // trivially equals the resolved target it was discovered at). A
    // widened resource is, structurally, always a shared-cache candidate;
    // it never gets to present as AUTO_SAFE-eligible on the strength of
    // its own internal structure alone.
    if let Some(ProbeOutcome::Observed(scope)) = &ev.cargo_target_scope {
        if scope.scope == TargetScope::Redirected || !scope.is_direct_locator {
            active_use_reasons.push(ReasonCode::SharedBuildCacheBlastRadius);
        }
    }

    active_use_reasons.dedup();

    if !active_use_reasons.is_empty() {
        return decision(PolicyClass::Ask, active_use_reasons);
    }

    if ev.resource.kind.is_managed_resource() {
        match ev.managed_ownership.as_ref().and_then(|p| p.observed()) {
            Some(evidence) if evidence.reclaimable => {}
            _ => return decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]),
        }
    }

    // 7. Recoverability: a detector that has positively established that
    // deleting this resource is permanent — no tool can refetch it, no
    // rebuild can recreate it — has said the one thing that must reach a
    // human every time (HORO-1553). `Ask` rather than `Protected`: consent
    // is possible, it just cannot be given in advance (see
    // `crate::autopilot::is_preauthorizable`).
    match ev.recoverability {
        Recoverability::Irreversible => {
            return decision(
                PolicyClass::Ask,
                vec![ReasonCode::RecoverabilityIrreversible],
            );
        }
        Recoverability::RegenerableByTool | Recoverability::RegenerableByRebuild => {}
    }

    // 8. Complete evidence, no active use: the per-instance regenerability
    // judgment on the Evidence itself decides (not the kind's static
    // default — a detector may have determined something more specific
    // about this instance). NotRegenerable is never auto-deleted,
    // regardless of how clean the rest of the evidence looks.
    //
    // Exhaustive `match` rather than the equality comparisons this used to
    // be (HORO-1553): `if ev.regenerability == NotRegenerable { ... }`
    // diverted exactly one variant and let every other one — including
    // `Unknown` — reach AUTO_SAFE through the implicit else. Unknown is not
    // false: a detector reporting that it could not establish
    // reproducibility must not be read as reporting that reproducibility is
    // fine. Matching exhaustively also means a future fifth variant stops
    // the compiler here instead of silently inheriting AUTO_SAFE.
    let regenerable_by_tool = match ev.regenerability {
        Regenerability::NotRegenerable => {
            return decision(PolicyClass::Ask, vec![ReasonCode::RebuildCostHigh]);
        }
        Regenerability::Unknown => {
            return decision(PolicyClass::Ask, vec![ReasonCode::RegenerabilityUnknown]);
        }
        Regenerability::RegenerableByTool => true,
        Regenerability::RegenerableByRebuild => false,
    };

    let mut auto_safe_reasons = vec![ReasonCode::EvidenceFreshAndComplete];
    if regenerable_by_tool {
        auto_safe_reasons.push(ReasonCode::RegenerableByTool);
    }
    auto_safe_reasons.push(ReasonCode::NoActiveUseObserved);

    decision(PolicyClass::AutoSafe, auto_safe_reasons)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::detectors::DetectorId;
    use crate::evidence::{
        DockerLifecycle, GitState, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint,
        ResourceId, ResourceLocator,
    };

    use super::*;

    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    fn complete_evidence(kind: ResourceKind, collected_at: SystemTime) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from("/tmp/x"))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(1024),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: crate::evidence::NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            // HORO-1825: a clean-negative report by default, so every
            // existing AutoSafe/Ask fixture built on an executable-bearing
            // kind (CargoTargetDir etc.) stays Complete unless a test
            // explicitly overrides this field.
            executable_dependency: ProbeOutcome::Observed(
                crate::evidence::ExecutableDependencyReport::empty(),
            ),
            // HORO-1824: same "clean default" rationale as
            // `executable_dependency` above — a Local-scope positive
            // result for CargoTargetDir, so every existing fixture built
            // on it stays Complete unless a test explicitly overrides
            // this field to exercise Redirected/Orphan/Unavailable.
            managed_ownership: None,
            cargo_target_scope: if kind == ResourceKind::CargoTargetDir {
                Some(ProbeOutcome::Observed(crate::evidence::CargoTargetScope {
                    resolved_target: PathBuf::from("/tmp/x"),
                    resolved_build_dir: None,
                    workspace_root: PathBuf::from("/tmp"),
                    scope: crate::evidence::TargetScope::Local,
                    is_direct_locator: true,
                }))
            } else {
                None
            },
            collected_at,
            sources: Vec::new(),
        }
    }

    fn cfg() -> PolicyConfig {
        PolicyConfig {
            max_evidence_age: Duration::from_secs(300),
        }
    }

    #[test]
    fn managed_cargo_library_requires_positive_enrollment_ownership_evidence() {
        let cfg = cfg();
        let mut ev = complete_evidence(ResourceKind::ManagedCargoLibrary, NOW);
        assert_eq!(
            classify(&ev, &cfg, NOW).reasons,
            vec![ReasonCode::EvidenceIncomplete]
        );

        ev.managed_ownership = Some(ProbeOutcome::Unavailable(ProbeReason::Failed));
        assert_eq!(
            classify(&ev, &cfg, NOW).reasons,
            vec![ReasonCode::EvidenceIncomplete]
        );

        ev.managed_ownership = Some(ProbeOutcome::Observed(
            crate::ownership::OwnershipEvidence {
                reclaimable: true,
                blockers: Vec::new(),
            },
        ));
        assert_eq!(classify(&ev, &cfg, NOW).class, PolicyClass::AutoSafe);

        ev.managed_ownership = Some(ProbeOutcome::Observed(
            crate::ownership::OwnershipEvidence {
                reclaimable: false,
                blockers: vec!["consumer is active".into()],
            },
        ));
        assert_eq!(
            classify(&ev, &cfg, NOW).reasons,
            vec![ReasonCode::EvidenceIncomplete]
        );
    }

    /// HORO-1049's critical AC: `reclaimable_bytes_is_lower_bound` is
    /// purely a display/reporting concern and must NEVER become a policy
    /// input. Two otherwise-identical `Evidence` values, differing only in
    /// this field, must classify to an identical `PolicyDecision` — same
    /// class, same reasons — in every branch `classify` can reach
    /// (Protected via unknown kind, Ask via active-use, and the AutoSafe
    /// happy path), not just the default AutoSafe case.
    #[test]
    fn reclaimable_bytes_is_lower_bound_never_affects_classify_output() {
        let cfg = cfg();

        // AutoSafe path.
        let mut ev_false = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let mut ev_true = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev_false.reclaimable_bytes_is_lower_bound = false;
        ev_true.reclaimable_bytes_is_lower_bound = true;
        assert_eq!(
            classify(&ev_false, &cfg, NOW),
            classify(&ev_true, &cfg, NOW)
        );

        // Ask path (active use).
        let mut ev_false = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let mut ev_true = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev_false.tool_liveness = ProbeOutcome::Observed(true);
        ev_true.tool_liveness = ProbeOutcome::Observed(true);
        ev_false.reclaimable_bytes_is_lower_bound = false;
        ev_true.reclaimable_bytes_is_lower_bound = true;
        assert_eq!(
            classify(&ev_false, &cfg, NOW),
            classify(&ev_true, &cfg, NOW)
        );

        // Protected path (unconditional, unknown kind).
        let mut ev_false = complete_evidence(ResourceKind::Unknown, NOW);
        let mut ev_true = complete_evidence(ResourceKind::Unknown, NOW);
        ev_false.reclaimable_bytes_is_lower_bound = false;
        ev_true.reclaimable_bytes_is_lower_bound = true;
        assert_eq!(
            classify(&ev_false, &cfg, NOW),
            classify(&ev_true, &cfg, NOW)
        );
    }

    #[test]
    fn unknown_resource_kind_is_unconditionally_protected() {
        let ev = complete_evidence(ResourceKind::Unknown, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedUnknownResourceKind]
        );
    }

    #[test]
    fn protected_path_wins_even_with_stale_evidence() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, SystemTime::UNIX_EPOCH);
        ev.resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/.ssh/id_ed25519")),
        );
        let far_future = NOW + Duration::from_secs(10_000);
        let decision = classify(&ev, &cfg(), far_future);
        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedCredentialMaterial]
        );
    }

    #[test]
    fn stale_evidence_is_ask() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, SystemTime::UNIX_EPOCH);
        let later = NOW + Duration::from_secs(301);
        let decision = classify(&ev, &cfg(), later);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::EvidenceStale]);
    }

    #[test]
    fn evidence_collected_after_now_is_ask_stale() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW + Duration::from_secs(10));
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::EvidenceStale]);
    }

    #[test]
    fn failed_completeness_is_ask_probe_failed_never_protected() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.logical_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.last_modified = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.open_by_process = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.process_cwd_match = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.git_state = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        // HORO-1825: CargoTargetDir now has a 7th required field. Leaving
        // it Observed here would drop `missing.len()` from 6-of-6 to
        // 6-of-7, turning `Failed` into `Partial` — this must stay
        // Unavailable too for the test to keep asserting what its name
        // says.
        ev.executable_dependency = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        // HORO-1824: CargoTargetDir now has an 8th required field
        // (`TargetOwnership`) — same reasoning as the comment above.
        ev.cargo_target_scope = Some(ProbeOutcome::Unavailable(ProbeReason::NotAttempted));
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::EvidenceProbeFailed]);
    }

    #[test]
    fn partial_completeness_is_ask_incomplete() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.git_state = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::EvidenceIncomplete]);
    }

    #[test]
    fn open_by_process_is_ask_in_active_use() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.open_by_process = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef::new(
            1,
            "cargo".to_string(),
        )]);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::ResourceInActiveUse));
    }

    #[test]
    fn process_cwd_match_is_ask_in_active_use() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.process_cwd_match = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef::new(
            1,
            "cargo".to_string(),
        )]);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::ResourceInActiveUse));
    }

    /// HORO-1823 ADR-0001 §13 item 2 ("detached still-active build"): a
    /// cargo build detached from its launching terminal — PPID == 1,
    /// supervisor `Unknown` (not a launchd job, not reparented to
    /// anything `launchctl` recognizes), zombie `false` — must still
    /// veto. The holder's mere presence is what protects it; nothing
    /// about its identity (orphaned-looking PPID, unknown supervision)
    /// is read as "therefore safe". Manually verified during review: with
    /// the `process_active` check (the two lines above step 6's first
    /// `if`) deleted, this test fails RED — see the PR body for the
    /// captured failing output.
    #[test]
    fn detached_still_active_build_is_protected_regardless_of_identity() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let detached = crate::evidence::ProcessRef::new(4242, "cargo".to_string()).with_identity(
            ProbeOutcome::Observed(crate::evidence::ProcessIdentity {
                start_time: NOW,
                uid: 501,
                ppid: 1,
                pgid: 4242,
                state_zombie: false,
                exe: ProbeOutcome::Unavailable(ProbeReason::Failed),
                supervisor: crate::evidence::Supervisor::Unknown,
            }),
        );
        ev.open_by_process = ProbeOutcome::Observed(vec![detached]);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::ResourceInActiveUse));
    }

    /// Identity-invariance: with the holder set fixed, varying only the
    /// `identity` field across Observed / Unavailable / zombie / a
    /// different (mismatched) identity tuple must never change
    /// `classify`'s decision. This is the structural proof that identity
    /// and (by extension) any derived claim never act as authority —
    /// `classify` doesn't read `identity` at all, and this test would
    /// catch a regression that made it start to.
    #[test]
    fn classify_decision_is_invariant_to_process_identity_variations() {
        fn decision_with(
            identity: ProbeOutcome<crate::evidence::ProcessIdentity>,
        ) -> (PolicyClass, Vec<ReasonCode>) {
            let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
            ev.open_by_process = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef::new(
                7,
                "cargo".to_string(),
            )
            .with_identity(identity)]);
            let decision = classify(&ev, &cfg(), NOW);
            (decision.class, decision.reasons)
        }

        let healthy = crate::evidence::ProcessIdentity {
            start_time: NOW,
            uid: 501,
            ppid: 1,
            pgid: 7,
            state_zombie: false,
            exe: ProbeOutcome::Observed(crate::evidence::ExeIdentity {
                path: PathBuf::from("/usr/bin/cargo"),
                dev: 1,
                ino: 1,
            }),
            supervisor: crate::evidence::Supervisor::LaunchdJob,
        };
        let zombie = crate::evidence::ProcessIdentity {
            state_zombie: true,
            ..healthy.clone()
        };
        let mismatched_tuple = crate::evidence::ProcessIdentity {
            uid: 999,
            ..healthy.clone()
        };

        let baseline = decision_with(ProbeOutcome::Observed(healthy));
        assert_eq!(baseline.0, PolicyClass::Ask);
        assert!(baseline.1.contains(&ReasonCode::ResourceInActiveUse));
        // Not just the class — the exact reason set too. A regression
        // that added or removed an identity-derived reason under `Ask`
        // (e.g. an `UNKNOWN_INCOMPLETE`-shaped addition) would pass a
        // class-only comparison but must fail this one, since the
        // executor aborts on `PolicyReasonsWidened` whenever the reason
        // set itself changes between a consented plan and revalidation.
        assert_eq!(
            decision_with(ProbeOutcome::Unavailable(ProbeReason::Failed)),
            baseline
        );
        assert_eq!(decision_with(ProbeOutcome::Observed(zombie)), baseline);
        assert_eq!(
            decision_with(ProbeOutcome::Observed(mismatched_tuple)),
            baseline
        );
    }

    /// HORO-1823 AC: "a verified inactive disposable target can lose one
    /// active-use veto only after a fresh evidence pass" — never from
    /// identity or a claim alone. Starts with a holder (ResourceInActiveUse)
    /// plus an unrelated, independent veto (a dirty git worktree), then
    /// simulates a fresh `merge_into` that re-probes and finds the holder
    /// genuinely gone. Only `ResourceInActiveUse` drops; the unrelated
    /// veto survives untouched, proving the loss came from fresh evidence
    /// re-observing an empty holder set, not from any identity/claim
    /// mechanism silently clearing reasons.
    #[test]
    fn a_holder_veto_is_lost_only_via_a_fresh_empty_observation_not_via_identity() {
        use crate::evidence::correlate::{merge_into, CorrelationResult};
        use crate::evidence::{ExecutableDependencyReport, GitState};

        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.open_by_process = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef::new(
            7,
            "cargo".to_string(),
        )]);
        ev.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from("/tmp/x"),
            common_dir: PathBuf::from("/tmp/x/.git"),
            dirty: true,
            untracked: false,
            worktree: false,
        }));

        let before = classify(&ev, &cfg(), NOW);
        assert_eq!(before.class, PolicyClass::Ask);
        assert!(before.reasons.contains(&ReasonCode::ResourceInActiveUse));
        assert!(before.reasons.contains(&ReasonCode::GitWorktreeDirty));

        // A fresh revalidation pass genuinely re-observes an empty holder
        // set (the only way this veto may ever drop) while the git state
        // is re-observed unchanged.
        merge_into(
            &mut ev,
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(Some(GitState {
                    repo_root: PathBuf::from("/tmp/x"),
                    common_dir: PathBuf::from("/tmp/x/.git"),
                    dirty: true,
                    untracked: false,
                    worktree: false,
                })),
                tool_liveness: ProbeOutcome::Observed(false),
                executable_dependency: ProbeOutcome::Observed(ExecutableDependencyReport::empty()),
            },
        );

        let after = classify(&ev, &cfg(), NOW);
        assert_eq!(after.class, PolicyClass::Ask);
        assert!(!after.reasons.contains(&ReasonCode::ResourceInActiveUse));
        assert!(after.reasons.contains(&ReasonCode::GitWorktreeDirty));
    }

    #[test]
    fn untracked_only_git_data_vetoes_auto_safe() {
        let mut ev = complete_evidence(ResourceKind::NodeModules, NOW);
        ev.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from("/tmp"),
            common_dir: PathBuf::from("/tmp/.git"),
            dirty: false,
            untracked: true,
            worktree: false,
        }));
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::GitWorktreeDirty]);
        assert_eq!(decision.policy_version, 5);
        assert!(crate::reporting::dto::active_use_signals(&ev)
            .contains(&"git: untracked files present".to_string()));
    }

    #[test]
    fn dirty_git_worktree_is_ask() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from("/tmp"),
            common_dir: PathBuf::from("/tmp/.git"),
            dirty: true,
            untracked: false,
            worktree: false,
        }));
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::GitWorktreeDirty));
    }

    #[test]
    fn is_worktree_true_is_ask_even_when_not_dirty() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from("/tmp"),
            common_dir: PathBuf::from("/tmp/.git"),
            dirty: false,
            untracked: false,
            worktree: true,
        }));
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::GitWorktreeDirty));
    }

    #[test]
    fn tool_live_is_ask() {
        let mut ev = complete_evidence(ResourceKind::XcodeDerivedData, NOW);
        ev.tool_liveness = ProbeOutcome::Observed(true);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::OwningToolLive));
    }

    #[test]
    fn clean_git_state_and_no_process_activity_is_not_ask_for_active_use() {
        // Baseline sanity: complete_evidence()'s defaults (empty process
        // lists, git_state Observed(None), tool_liveness Observed(false))
        // must not themselves trigger any active-use reason.
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert!(!decision.reasons.contains(&ReasonCode::ResourceInActiveUse));
        assert!(!decision.reasons.contains(&ReasonCode::GitWorktreeDirty));
        assert!(!decision.reasons.contains(&ReasonCode::OwningToolLive));
    }

    #[test]
    fn clean_complete_evidence_regenerable_by_rebuild_is_auto_safe() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::AutoSafe);
        assert!(decision
            .reasons
            .contains(&ReasonCode::EvidenceFreshAndComplete));
        assert!(decision.reasons.contains(&ReasonCode::NoActiveUseObserved));
        assert!(!decision.reasons.contains(&ReasonCode::RegenerableByTool));
    }

    #[test]
    fn clean_complete_evidence_regenerable_by_tool_is_auto_safe_with_reason() {
        let ev = complete_evidence(ResourceKind::CargoRegistryCache, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::AutoSafe);
        assert!(decision.reasons.contains(&ReasonCode::RegenerableByTool));
    }

    #[test]
    fn not_regenerable_is_ask_rebuild_cost_high_even_when_clean() {
        // Per-instance regenerability on Evidence (not the kind's static
        // default) is what classify consults, so a detector that
        // determines a specific instance is NotRegenerable is respected
        // even though clean/complete evidence would otherwise be
        // AutoSafe.
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.regenerability = Regenerability::NotRegenerable;
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::RebuildCostHigh]);
    }

    /// HORO-1553. The defect this pins: `Regenerability::Unknown` used to
    /// reach `AUTO_SAFE` through the implicit else of
    /// `if ev.regenerability == NotRegenerable`, carrying the reason set
    /// `evidence_fresh_and_complete` + `no_active_use_observed` — a set that
    /// positively asserts the classification is well-founded.
    ///
    /// Mutation control: reverting the `Regenerability::Unknown` arm in
    /// `classify` to fall through fails this test on the first assertion,
    /// naming `AutoSafe` where `Ask` was required. It is not an
    /// off-by-one-in-a-reason-count assertion that would also fail for
    /// unrelated edits.
    #[test]
    fn unknown_regenerability_is_ask_not_auto_safe_however_clean_the_rest_is() {
        let mut ev = complete_evidence(ResourceKind::CargoRegistryCache, NOW);
        // Everything else about this evidence is the AutoSafe happy path:
        // fresh, complete, nothing open, no dirty git state, tool idle.
        // `CargoRegistryCache`'s own static regenerability is
        // RegenerableByTool, so the only thing standing between this and
        // AUTO_SAFE is the per-instance judgment below.
        ev.regenerability = Regenerability::Unknown;

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(
            decision.class,
            PolicyClass::Ask,
            "unknown reproducibility must not be read as safe"
        );
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::RegenerabilityUnknown],
            "an absence of knowledge must not be reported as a rebuild-cost judgment"
        );
        assert!(
            !decision
                .reasons
                .contains(&ReasonCode::EvidenceFreshAndComplete),
            "fresh-and-complete evidence is not a reason to act on a resource \
             whose reproducibility was never established"
        );
    }

    /// HORO-1553. `Evidence::recoverability` was read by no decision
    /// anywhere before this ticket, so a detector that had positively
    /// established permanent loss had no way to say so.
    ///
    /// Mutation control: deleting the `Recoverability::Irreversible` arm in
    /// `classify` makes this fail with `AutoSafe` (the evidence is otherwise
    /// clean), naming the class rather than a count.
    #[test]
    fn irreversible_recoverability_is_ask_even_when_regenerability_says_otherwise() {
        let mut ev = complete_evidence(ResourceKind::CargoRegistryCache, NOW);
        // Deliberately contradictory: the static per-kind property says a
        // tool can refetch this, the per-instance judgment says removal is
        // permanent. The more severe axis has to win, or the field is
        // decorative again.
        assert_eq!(ev.regenerability, Regenerability::RegenerableByTool);
        ev.recoverability = Recoverability::Irreversible;

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::RecoverabilityIrreversible]
        );
    }

    /// The two new `Ask` reasons must not be consentable in advance, or the
    /// `Ask` classification above becomes a formality Autopilot walks past.
    #[test]
    fn neither_new_ask_reason_can_be_preauthorized() {
        for reason in [
            ReasonCode::RegenerabilityUnknown,
            ReasonCode::RecoverabilityIrreversible,
        ] {
            assert!(
                !crate::autopilot::envelope::is_preauthorizable(reason),
                "{} must never be pre-authorizable",
                reason.as_str()
            );
        }
    }

    /// A Docker object as a detector will produce it: identified by Docker's
    /// own id rather than a path, with the three path probes reported
    /// `NotAttempted` because there is no path to point them at. `Complete`
    /// regardless, since `required_evidence()` does not ask a tool-owned kind
    /// for path facts (HORO-1544).
    fn docker_evidence(kind: ResourceKind, lifecycle: DockerLifecycle) -> Evidence {
        let mut ev = complete_evidence(kind, NOW);
        ev.resource = ResourceId::new(
            kind,
            ResourceLocator::Tool {
                tool: crate::evidence::OwningTool::Docker,
                id: "deadbeef".to_string(),
            },
        );
        ev.open_by_process = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.process_cwd_match = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.git_state = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.docker_lifecycle = Some(lifecycle);
        ev
    }

    fn observed_idle() -> DockerLifecycle {
        DockerLifecycle {
            activity: DockerActivity::Inactive,
            persistence: crate::evidence::DockerPersistence::ToolManaged,
            references: ProbeOutcome::Observed(crate::evidence::DockerReferences::none()),
        }
    }

    /// A running container is active-use evidence (HORO-1544 AC4), and it must
    /// arrive as its own reason rather than as `owning_tool_live` — the daemon
    /// being up is one fact about every Docker object at once and says nothing
    /// about which of them anything is using.
    #[test]
    fn a_running_docker_container_is_ask_for_being_in_use() {
        let mut lifecycle = observed_idle();
        lifecycle.activity = DockerActivity::Active;
        let ev = docker_evidence(ResourceKind::DockerContainer, lifecycle);

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::DockerObjectInUse]);
    }

    /// An image a running container needs is in use as surely as the container
    /// is, and `docker system df` calls that same image "Reclaimable (100%)".
    /// Docker's offer is not authority (HORO-1544 safety requirement).
    #[test]
    fn an_image_a_running_container_needs_is_ask_for_being_in_use() {
        let lifecycle = DockerLifecycle {
            activity: DockerActivity::Active,
            persistence: crate::evidence::DockerPersistence::ToolManaged,
            references: ProbeOutcome::Observed(crate::evidence::DockerReferences {
                referenced_by: vec![ResourceId::new(
                    ResourceKind::DockerContainer,
                    ResourceLocator::Tool {
                        tool: crate::evidence::OwningTool::Docker,
                        id: "c0ffee".to_string(),
                    },
                )],
                active_referrers: 1,
            }),
        };
        let ev = docker_evidence(ResourceKind::DockerImage, lifecycle);

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::DockerObjectInUse]);
    }

    /// Unknown is not a quieter `Inactive`. An object whose activity was never
    /// established must not travel the same path as one Docker positively
    /// reported as idle, and the distinguishing evidence is the reason code:
    /// `docker_activity_unknown`, not `docker_object_in_use` and not silence.
    #[test]
    fn unknown_docker_activity_is_ask_and_not_spelled_like_in_use() {
        let ev = docker_evidence(
            ResourceKind::DockerImage,
            DockerLifecycle::unknown(ProbeReason::ToolNotRunning),
        );

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::DockerActivityUnknown]);
    }

    /// The reference query is its own axis: "Docker was asked and nothing
    /// references this" and "the reference query did not run" would otherwise
    /// both arrive as an empty referrer set. The activity axis is `Inactive`
    /// here, so the reason can only have come from the unanswered query.
    #[test]
    fn an_unanswered_reference_query_is_ask_even_when_activity_is_known() {
        let mut lifecycle = observed_idle();
        lifecycle.references = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        let ev = docker_evidence(ResourceKind::DockerImage, lifecycle);

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::DockerActivityUnknown]);
    }

    /// The negative control for all four tests above: a fully observed idle
    /// object adds neither Docker reason, so any of them appearing elsewhere
    /// came from the lifecycle facts rather than from merely being Docker.
    ///
    /// It still does not reach AUTO_SAFE — a stopped container holds its
    /// writable layer, which `regenerability()` reports `NotRegenerable` — and
    /// that is the point: the Ask arrives for the honest reason.
    #[test]
    fn an_observed_idle_docker_object_adds_no_docker_reason() {
        let ev = docker_evidence(ResourceKind::DockerContainer, observed_idle());

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::RebuildCostHigh]);
    }

    /// AC 5, through the whole classifier rather than through
    /// `protected_reason` alone, and with the evidence stacked as favourably as
    /// any real snapshot could ever make it: Docker was asked and answered on
    /// every axis, nothing references the volume, nothing is running, and the
    /// persistence axis has been set to the most permissive value in the
    /// vocabulary. This is the exact shape a "surely this one is fine" argument
    /// would arrive in, and it must still not be executable without asking.
    ///
    /// The refusal comes from the kind, not from the evidence — which is why it
    /// cannot be argued out of by a better-looking snapshot.
    #[test]
    fn a_docker_volume_is_never_auto_safe_however_idle_the_evidence_says_it_is() {
        let mut lifecycle = observed_idle();
        lifecycle.persistence = crate::evidence::DockerPersistence::ToolManaged;
        let ev = docker_evidence(ResourceKind::DockerVolume, lifecycle);

        let decision = classify(&ev, &cfg(), NOW);

        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "a volume may hold the only copy of the developer's data"
        );
        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedPersistentVolume]
        );
    }

    /// A non-Docker resource carries `docker_lifecycle: None`, which must mean
    /// "the concept does not apply" and not "the facts are unknown". If the
    /// `None` arm ever started defaulting to `Unknown`, every Cargo target dir
    /// on the machine would become an Ask.
    #[test]
    fn absent_docker_lifecycle_does_not_read_as_unknown_activity() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        assert!(ev.docker_lifecycle.is_none());

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::AutoSafe);
        assert!(!decision
            .reasons
            .contains(&ReasonCode::DockerActivityUnknown));
    }

    /// Both new reasons are refusals, so neither may be consented to in
    /// advance — an Autopilot run must not silently pre-authorize a running
    /// container or an unanswered question.
    #[test]
    fn neither_docker_reason_can_be_preauthorized() {
        for reason in [
            ReasonCode::DockerObjectInUse,
            ReasonCode::DockerActivityUnknown,
        ] {
            assert!(
                !crate::autopilot::envelope::is_preauthorizable(reason),
                "{} must never be pre-authorizable",
                reason.as_str()
            );
        }
    }

    #[test]
    fn policy_version_is_five() {
        // HORO-1868: bumped 3 -> 4. See the `classify()` comment at
        // the `policy_version` literal for what changed.
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.policy_version, 5);
    }

    // ---- HORO-1825 §13 anti-vacuity + AC tests ----

    use crate::evidence::{
        DependencyRef, ExeIdentity, ExecutableDependencyReport, Provenance, SourceTag,
        UnresolvedRef,
    };

    fn exe(path: &str) -> ExeIdentity {
        ExeIdentity {
            path: PathBuf::from(path),
            dev: 1,
            ino: 1,
        }
    }

    fn report_with_reference(path: &str) -> ExecutableDependencyReport {
        ExecutableDependencyReport {
            references_inside: vec![DependencyRef {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
                resolved: PathBuf::from(path),
                exe: exe(path),
                provenance: Provenance::Observed,
                next_invocation: true,
            }],
            running_inside: Vec::new(),
            unresolved: Vec::new(),
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        }
    }

    /// AC: "Test-owned fake hook config pointing into target prevents
    /// AUTO_SAFE". Anti-vacuity #1 (§13 item 1): deleting step 2b must
    /// turn this RED.
    #[test]
    fn fake_hook_config_pointing_into_target_is_protected_not_auto_safe() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.executable_dependency =
            ProbeOutcome::Observed(report_with_reference("/tmp/x/debug/hook"));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedExecutableDependency]
        );
    }

    /// AC: "unreferenced target can remain eligible" — the negative
    /// control for the test above: a clean report must not, on its own,
    /// prevent AUTO_SAFE.
    #[test]
    fn unreferenced_target_remains_auto_safe_eligible() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        assert_eq!(
            ev.executable_dependency.observed(),
            Some(&ExecutableDependencyReport::empty())
        );

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::AutoSafe);
    }

    /// §13 item 3: a probe that collapses an unparseable config into
    /// `Observed(empty)` on parse error (rather than an `unresolved`
    /// finding) must turn RED here — this test pins the probe-level
    /// contract via the `classify`-facing symptom: an `unresolved` entry
    /// present on an executable-bearing kind must reach `Ask`, never slip
    /// through as if it had been `Observed(empty)`.
    ///
    /// HORO-1868: this fixture now uses `SourceUnreadable` — the real
    /// "unparseable config" shape (a whole source failed to read/parse,
    /// never examined at the reference level) — rather than
    /// `NonInspectable`, which as of HORO-1868 is an EXAMINED reference
    /// and now classifies `Protected`. The `NonInspectable` -> `Protected`
    /// case is covered by its own dedicated table test,
    /// `each_examined_unresolved_variant_on_executable_bearing_kind_is_protected`.
    #[test]
    fn unresolved_reference_on_executable_bearing_kind_is_ask_not_auto_safe() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.executable_dependency = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: Vec::new(),
            running_inside: Vec::new(),
            unresolved: vec![UnresolvedRef::SourceUnreadable {
                source: SourceTag::ClaudeUserSettings,
            }],
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision
            .reasons
            .contains(&ReasonCode::ExecutableDependencyUnknown));
    }

    /// `unresolved` on a non-executable-bearing kind (a download cache)
    /// must NOT veto — per §5: "Its failure does not block those kinds."
    #[test]
    fn unresolved_reference_on_non_executable_bearing_kind_does_not_veto() {
        let mut ev = complete_evidence(ResourceKind::CargoRegistryCache, NOW);
        ev.executable_dependency = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: Vec::new(),
            running_inside: Vec::new(),
            unresolved: vec![UnresolvedRef::NotOnPath {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            }],
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::AutoSafe);
    }

    /// AC: "a currently-running process whose executable mapping resolves
    /// inside the resource" => Ask, not a silent pass.
    #[test]
    fn running_executable_inside_resource_is_ask() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.executable_dependency = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: Vec::new(),
            running_inside: vec![crate::evidence::ProcessRef::new(
                999,
                "libra-governor".to_string(),
            )],
            unresolved: Vec::new(),
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        });

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision
            .reasons
            .contains(&ReasonCode::ExecutableRunningFromResource));
    }

    /// §13 item 5: `is_preauthorizable` must refuse both new `Ask` codes.
    /// Mirrors `neither_new_ask_reason_can_be_preauthorized` above.
    #[test]
    fn neither_new_executable_dependency_ask_reason_can_be_preauthorized() {
        for reason in [
            ReasonCode::ExecutableRunningFromResource,
            ReasonCode::ExecutableDependencyUnknown,
        ] {
            assert!(
                !crate::autopilot::envelope::is_preauthorizable(reason),
                "{} must never be pre-authorizable",
                reason.as_str()
            );
        }
    }

    /// §13 item 4 (the HORO-994 trap, for this ticket's half): a stale
    /// positive stays Protected even when evidence is otherwise
    /// old/complete — step 2b runs before the staleness check, same as
    /// step 2.
    #[test]
    fn stale_but_positive_executable_dependency_is_still_protected() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, SystemTime::UNIX_EPOCH);
        ev.executable_dependency =
            ProbeOutcome::Observed(report_with_reference("/tmp/x/debug/hook"));
        let far_future = NOW + Duration::from_secs(10_000);

        let decision = classify(&ev, &cfg(), far_future);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedExecutableDependency]
        );
    }

    // ---- HORO-1868: step 2c, ProtectedUnresolvedExecutableDependency ----

    fn unresolved_report(unresolved: UnresolvedRef) -> ExecutableDependencyReport {
        ExecutableDependencyReport {
            references_inside: Vec::new(),
            running_inside: Vec::new(),
            unresolved: vec![unresolved],
            sources_examined: vec![SourceTag::ClaudeUserSettings],
        }
    }

    /// Table test: every `UnresolvedRef` variant for which
    /// `is_examined_reference()` is `true` must classify `Protected`
    /// `[ProtectedUnresolvedExecutableDependency]` on an executable-bearing
    /// kind, and nothing else.
    #[test]
    fn each_examined_unresolved_variant_on_executable_bearing_kind_is_protected() {
        let pointer = "hooks.PostToolUse[0].hooks[0]".to_string();
        let variants = [
            UnresolvedRef::NotOnPath {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
            },
            UnresolvedRef::PathResolutionDivergent {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
            },
            UnresolvedRef::NonInspectable {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
            },
            UnresolvedRef::SymlinkLoopOrTooDeep {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
            },
            UnresolvedRef::PathResolutionIndeterminate {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
            },
            UnresolvedRef::MalformedSchema {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
            },
            UnresolvedRef::RestatFailed {
                source: SourceTag::ClaudeUserSettings,
                pointer: pointer.clone(),
                resolved: PathBuf::from("/tmp/x/debug/hook"),
            },
        ];

        for variant in variants {
            let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
            ev.executable_dependency = ProbeOutcome::Observed(unresolved_report(variant.clone()));

            let decision = classify(&ev, &cfg(), NOW);

            assert_eq!(
                decision.class,
                PolicyClass::Protected,
                "{variant:?} must classify Protected"
            );
            assert_eq!(
                decision.reasons,
                vec![ReasonCode::ProtectedUnresolvedExecutableDependency],
                "{variant:?} must carry exactly one reason"
            );
        }
    }

    /// `SourceUnreadable` (a whole source failed to read/parse, never
    /// examined at the reference level) stays the consentable `Ask
    /// ExecutableDependencyUnknown` — on every kind of source-read
    /// failure, including the N7 lsof-failure shape
    /// (`SourceTag::RunningProcesses`).
    #[test]
    fn source_unreadable_variants_stay_ask_executable_dependency_unknown() {
        for tag in [
            SourceTag::ClaudeUserSettings,
            SourceTag::RunningProcesses,
            SourceTag::CodexHooks,
        ] {
            let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
            ev.executable_dependency =
                ProbeOutcome::Observed(unresolved_report(UnresolvedRef::SourceUnreadable {
                    source: tag.clone(),
                }));

            let decision = classify(&ev, &cfg(), NOW);

            assert_eq!(
                decision.class,
                PolicyClass::Ask,
                "SourceUnreadable({tag:?}) must stay Ask"
            );
            assert_eq!(
                decision.reasons,
                vec![ReasonCode::ExecutableDependencyUnknown]
            );
        }
    }

    /// Precedence: step 2c wins over a `Partial` completeness gap on an
    /// unrelated field (`git_state` unavailable) — never falls through to
    /// the generic `EvidenceIncomplete` ASK.
    #[test]
    fn examined_unresolved_wins_over_partial_git_state_unavailable() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.git_state = ProbeOutcome::Unavailable(crate::evidence::ProbeReason::ToolAbsent);
        ev.executable_dependency =
            ProbeOutcome::Observed(unresolved_report(UnresolvedRef::NotOnPath {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            }));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedUnresolvedExecutableDependency]
        );
    }

    /// Precedence: step 2c wins over staleness — never falls through to
    /// `EvidenceStale`.
    #[test]
    fn examined_unresolved_wins_over_stale_evidence() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, SystemTime::UNIX_EPOCH);
        ev.executable_dependency =
            ProbeOutcome::Observed(unresolved_report(UnresolvedRef::NotOnPath {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            }));
        let far_future = NOW + Duration::from_secs(10_000);

        let decision = classify(&ev, &cfg(), far_future);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedUnresolvedExecutableDependency]
        );
    }

    /// Precedence: step 2c wins over the `cargo_target_scope`-unavailable
    /// Partial (`TargetOwnershipUnknown`'s own specific gap) — never falls
    /// through to that reason either.
    #[test]
    fn examined_unresolved_wins_over_cargo_target_scope_unavailable() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.cargo_target_scope = Some(ProbeOutcome::Unavailable(
            crate::evidence::ProbeReason::ToolAbsent,
        ));
        ev.executable_dependency =
            ProbeOutcome::Observed(unresolved_report(UnresolvedRef::NotOnPath {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            }));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedUnresolvedExecutableDependency]
        );
    }

    /// Precedence: a confirmed `references_inside` (step 2b) alongside an
    /// examined-unresolved entry must still carry exactly one reason — step
    /// 2b wins outright, no double-reason.
    #[test]
    fn confirmed_reference_plus_examined_unresolved_is_step_2b_reason_only() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let mut report = report_with_reference("/tmp/x/debug/hook");
        report.unresolved.push(UnresolvedRef::NotOnPath {
            source: SourceTag::ClaudeUserSettings,
            pointer: "hooks.Stop[0].hooks[0]".to_string(),
        });
        ev.executable_dependency = ProbeOutcome::Observed(report);

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedExecutableDependency]
        );
    }

    /// A mix of an examined-unresolved entry AND a `SourceUnreadable` entry
    /// still classifies `Protected` (step 2c's `.any()` fires on the first
    /// examined entry, regardless of what else is present).
    #[test]
    fn examined_unresolved_mixed_with_source_unreadable_is_still_protected() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.executable_dependency = ProbeOutcome::Observed(ExecutableDependencyReport {
            references_inside: Vec::new(),
            running_inside: Vec::new(),
            unresolved: vec![
                UnresolvedRef::NotOnPath {
                    source: SourceTag::ClaudeUserSettings,
                    pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
                },
                UnresolvedRef::SourceUnreadable {
                    source: SourceTag::CodexHooks,
                },
            ],
            sources_examined: vec![SourceTag::ClaudeUserSettings, SourceTag::CodexHooks],
        });

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ProtectedUnresolvedExecutableDependency]
        );
    }

    /// Unbypassable: `authorize` must refuse a `Protected
    /// ProtectedUnresolvedExecutableDependency` decision even with a
    /// matching consent — there is no override path for `Protected`.
    #[test]
    fn examined_unresolved_decision_is_unbypassable_even_with_matching_consent() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.executable_dependency =
            ProbeOutcome::Observed(unresolved_report(UnresolvedRef::NotOnPath {
                source: SourceTag::ClaudeUserSettings,
                pointer: "hooks.PostToolUse[0].hooks[0]".to_string(),
            }));
        let decision = classify(&ev, &cfg(), NOW);
        let fingerprint = ev.fingerprint.clone();
        let consent = crate::policy::approval::UserConsent::new(
            ev.resource.clone(),
            fingerprint.clone(),
            NOW,
        );

        let approval = crate::policy::approval::authorize(decision, fingerprint, Some(&consent));

        assert!(approval.is_none());
    }

    /// Unaffected (critical — proves no over-scoping): the
    /// `cargo_target_scope`-unavailable-alone `TargetOwnershipUnknown` path
    /// stays `Ask` and consentable when no executable-dependency unresolved
    /// entry is present at all.
    #[test]
    fn cargo_target_scope_unavailable_alone_stays_ask_and_consentable() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.cargo_target_scope = Some(ProbeOutcome::Unavailable(
            crate::evidence::ProbeReason::ToolAbsent,
        ));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::TargetOwnershipUnknown]);

        let fingerprint = ev.fingerprint.clone();
        let consent = crate::policy::approval::UserConsent::new(
            ev.resource.clone(),
            fingerprint.clone(),
            NOW,
        );
        let approval = crate::policy::approval::authorize(decision, fingerprint, Some(&consent));
        assert!(approval.is_some());
    }

    /// Unaffected: a lone `SourceUnreadable(RunningProcesses)` (the N7
    /// lsof-failure shape) stays `Ask` and consentable.
    #[test]
    fn source_unreadable_running_processes_alone_stays_ask_and_consentable() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.executable_dependency =
            ProbeOutcome::Observed(unresolved_report(UnresolvedRef::SourceUnreadable {
                source: SourceTag::RunningProcesses,
            }));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::ExecutableDependencyUnknown]
        );

        let fingerprint = ev.fingerprint.clone();
        let consent = crate::policy::approval::UserConsent::new(
            ev.resource.clone(),
            fingerprint.clone(),
            NOW,
        );
        let approval = crate::policy::approval::authorize(decision, fingerprint, Some(&consent));
        assert!(approval.is_some());
    }

    /// Unaffected: `open_by_process` unavailable alone (an unrelated
    /// completeness gap) stays the generic `Ask EvidenceIncomplete` and
    /// consentable — step 2c's gate never fires with a clean
    /// `executable_dependency` report.
    #[test]
    fn open_by_process_unavailable_alone_stays_ask_evidence_incomplete_and_consentable() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.open_by_process = ProbeOutcome::Unavailable(crate::evidence::ProbeReason::ToolAbsent);

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::EvidenceIncomplete]);

        let fingerprint = ev.fingerprint.clone();
        let consent = crate::policy::approval::UserConsent::new(
            ev.resource.clone(),
            fingerprint.clone(),
            NOW,
        );
        let approval = crate::policy::approval::authorize(decision, fingerprint, Some(&consent));
        assert!(approval.is_some());
    }

    /// `is_preauthorizable(ProtectedUnresolvedExecutableDependency) ==
    /// false`.
    #[test]
    fn protected_unresolved_executable_dependency_cannot_be_preauthorized() {
        assert!(!crate::autopilot::envelope::is_preauthorizable(
            ReasonCode::ProtectedUnresolvedExecutableDependency
        ));
    }

    // ---- HORO-1824 §13 anti-vacuity + AC tests ----

    /// §13 item 6 ("Map Redirected to no veto -> the shared-target
    /// test"): a `Redirected` Cargo target must be ASK, never AUTO_SAFE —
    /// the exact assertion a mutation deleting the `Redirected` arm in
    /// `classify`'s step 6 extension would falsify.
    #[test]
    fn redirected_cargo_target_scope_is_ask_shared_build_cache_not_auto_safe() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.cargo_target_scope = Some(ProbeOutcome::Observed(crate::evidence::CargoTargetScope {
            resolved_target: PathBuf::from("/Users/x/.cargo/shared-target"),
            resolved_build_dir: None,
            workspace_root: PathBuf::from("/tmp/x"),
            scope: crate::evidence::TargetScope::Redirected,
            is_direct_locator: true,
        }));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::SharedBuildCacheBlastRadius]
        );
    }

    /// The negative control for the test above: `Local` scope on the
    /// project's own direct locator adds no reason and reaches
    /// `AutoSafe` — proving the veto above comes from `Redirected`
    /// specifically, not from the mere presence of `cargo_target_scope`.
    #[test]
    fn local_direct_cargo_target_scope_is_auto_safe_eligible() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::AutoSafe);
        assert!(!decision
            .reasons
            .contains(&ReasonCode::SharedBuildCacheBlastRadius));
    }

    /// Security regression (independent review): a resource reached via
    /// discovery widening (`is_direct_locator: false`) must ALWAYS veto,
    /// even when its own recomputed `scope` is `Local` — this is the
    /// exact gap a widened resource's own re-resolution at revalidation
    /// time could otherwise fall into (see `CargoTargetScope::
    /// is_direct_locator`'s doc comment).
    #[test]
    fn a_widened_resource_vetoes_even_when_its_own_scope_recomputes_local() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.cargo_target_scope = Some(ProbeOutcome::Observed(crate::evidence::CargoTargetScope {
            resolved_target: PathBuf::from("/tmp/shared/target"),
            resolved_build_dir: None,
            workspace_root: PathBuf::from("/tmp/shared"),
            scope: crate::evidence::TargetScope::Local,
            is_direct_locator: false,
        }));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(
            decision.reasons,
            vec![ReasonCode::SharedBuildCacheBlastRadius]
        );
    }

    /// §13 item 5, for this ticket's two new reason codes: `is_preauthorizable`
    /// must refuse both.
    #[test]
    fn neither_new_cargo_ownership_reason_can_be_preauthorized() {
        for reason in [
            ReasonCode::SharedBuildCacheBlastRadius,
            ReasonCode::TargetOwnershipUnknown,
        ] {
            assert!(
                !crate::autopilot::envelope::is_preauthorizable(reason),
                "{} must never be pre-authorizable",
                reason.as_str()
            );
        }
    }

    /// §8 / §5: `cargo metadata` absent/timeout/failure (`Unavailable`)
    /// maps to the SPECIFIC `TargetOwnershipUnknown` reason, not the
    /// generic `EvidenceIncomplete` — the ticket's own completeness
    /// addendum (ADR-0001 §5 step 6.4).
    #[test]
    fn unavailable_cargo_target_scope_is_ask_target_ownership_unknown() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.cargo_target_scope = Some(ProbeOutcome::Unavailable(ProbeReason::Failed));

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::TargetOwnershipUnknown]);
    }

    /// A missing `cargo_target_scope` mixed with another missing field
    /// must still fall back to the generic `EvidenceIncomplete` — the
    /// specific code is reserved for the isolated, single-field gap.
    #[test]
    fn target_ownership_unknown_mixed_with_another_missing_field_is_generic_incomplete() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.cargo_target_scope = Some(ProbeOutcome::Unavailable(ProbeReason::Failed));
        ev.git_state = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);

        let decision = classify(&ev, &cfg(), NOW);

        assert_eq!(decision.class, PolicyClass::Ask);
        assert_eq!(decision.reasons, vec![ReasonCode::EvidenceIncomplete]);
    }

    /// HORO-1912's safety net: a candidate a correlation-pass budget ran
    /// out before reaching is merged with
    /// `CorrelationResult::unavailable(TimedOut)` rather than left at
    /// whatever the detector set. This proves that merge, for every
    /// `ResourceKind`, can never classify to `AutoSafe` — a future kind
    /// whose `required_evidence()` somehow omits every correlation field
    /// would otherwise let a skipped-but-unprobed candidate through as
    /// clean. Starting from `complete_evidence` (every field `Observed`,
    /// the most AutoSafe-favorable starting point this file has) and then
    /// overwriting only the five correlation fields is what makes a pass
    /// here mean the merge itself is sufficient, not that the rest of the
    /// fixture happened to already be incomplete.
    #[test]
    fn correlation_unavailable_merge_never_classifies_any_kind_as_auto_safe() {
        use crate::evidence::correlate::{merge_into, CorrelationResult};

        for &kind in ResourceKind::ALL {
            let mut ev = complete_evidence(kind, NOW);
            merge_into(
                &mut ev,
                CorrelationResult::unavailable(ProbeReason::TimedOut),
            );

            let decision = classify(&ev, &cfg(), NOW);

            assert_ne!(
                decision.class,
                PolicyClass::AutoSafe,
                "{kind:?} classified AutoSafe despite every correlation field being \
                 Unavailable(TimedOut) — a pass-budget skip must never read as clean"
            );
        }
    }
}
