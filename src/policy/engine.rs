//! Deterministic `AUTO_SAFE`/`ASK`/`PROTECTED` classification (HORO-950).

use std::time::SystemTime;

use crate::evidence::{Completeness, Evidence, ResourceKind};

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

    // Regenerability branch lands in a follow-up commit. Until then,
    // Complete evidence with no active-use signal falls through to a
    // placeholder AutoSafe — transitional within this commit series, not
    // final behavior.
    decision(
        PolicyClass::AutoSafe,
        vec![ReasonCode::EvidenceFreshAndComplete],
    )
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
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: crate::evidence::NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
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
    fn policy_version_is_one() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.policy_version, 1);
    }
}
