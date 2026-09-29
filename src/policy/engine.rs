//! Deterministic `AUTO_SAFE`/`ASK`/`PROTECTED` classification (HORO-950).

use std::time::SystemTime;

use crate::evidence::{Completeness, Evidence, Recoverability, Regenerability, ResourceKind};

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
        policy_version: 1,
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
        Some(Some(g)) if g.dirty || g.worktree
    );
    if git_active {
        active_use_reasons.push(ReasonCode::GitWorktreeDirty);
    }

    let tool_live = matches!(ev.tool_liveness.observed(), Some(true));
    if tool_live {
        active_use_reasons.push(ReasonCode::OwningToolLive);
    }

    if !active_use_reasons.is_empty() {
        return decision(PolicyClass::Ask, active_use_reasons);
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
        GitState, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint, ResourceId,
        ResourceLocator,
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
            collected_at,
            sources: Vec::new(),
        }
    }

    fn cfg() -> PolicyConfig {
        PolicyConfig {
            max_evidence_age: Duration::from_secs(300),
        }
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
        ev.open_by_process = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef {
            pid: 1,
            command: "cargo".to_string(),
        }]);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::ResourceInActiveUse));
    }

    #[test]
    fn process_cwd_match_is_ask_in_active_use() {
        let mut ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        ev.process_cwd_match = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef {
            pid: 1,
            command: "cargo".to_string(),
        }]);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.class, PolicyClass::Ask);
        assert!(decision.reasons.contains(&ReasonCode::ResourceInActiveUse));
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

    #[test]
    fn policy_version_is_one() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.policy_version, 1);
    }
}
