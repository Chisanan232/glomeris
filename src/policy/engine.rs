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

    // Remaining steps (active-use, regenerability) land in follow-up
    // commits. Until then, Complete/clean evidence with no active-use
    // check yet falls through to AutoSafe with no reasons attached — this
    // is a transitional state within this commit series, not final
    // behavior; see the following commits for the real steps 6-8.
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
        ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint, ResourceId, ResourceLocator,
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
    fn policy_version_is_one() {
        let ev = complete_evidence(ResourceKind::CargoTargetDir, NOW);
        let decision = classify(&ev, &cfg(), NOW);
        assert_eq!(decision.policy_version, 1);
    }
}
