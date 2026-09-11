//! Presentation-layer label distinguishing `AUTO_SAFE` / `ASK` /
//! `PROTECTED` / `UNKNOWN_INCOMPLETE` (HORO-955 acceptance criteria:
//! "Clearly distinguish AUTO_SAFE, ASK, PROTECTED, UNKNOWN/INCOMPLETE").
//!
//! [`crate::policy::PolicyClass`] itself only has three variants —
//! `UNKNOWN_INCOMPLETE` is derived here, presentation-side only, from the
//! specific [`crate::policy::ReasonCode`]s that mean "we don't have
//! enough/trustworthy evidence to judge at all" as opposed to a real
//! Ask-because-of-observed-risk judgment. This module does not change,
//! and is never consulted by, [`crate::policy::classify`] itself — it is
//! a read-only projection for report text.

use crate::policy::{PolicyClass, PolicyDecision, ReasonCode};

/// Report-facing classification label. See module docs for how
/// [`PolicyLabel::UnknownIncomplete`] relates to [`PolicyClass::Ask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyLabel {
    AutoSafe,
    Ask,
    Protected,
    UnknownIncomplete,
}

impl PolicyLabel {
    pub fn as_str(&self) -> &'static str {
        match self {
            PolicyLabel::AutoSafe => "AUTO_SAFE",
            PolicyLabel::Ask => "ASK",
            PolicyLabel::Protected => "PROTECTED",
            PolicyLabel::UnknownIncomplete => "UNKNOWN_INCOMPLETE",
        }
    }
}

impl std::fmt::Display for PolicyLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Derives the report-facing label for one [`PolicyDecision`]. A decision
/// classified [`PolicyClass::Ask`] purely because the evidence itself was
/// incomplete, stale, or a probe failed is surfaced as
/// [`PolicyLabel::UnknownIncomplete`] rather than a plain `ASK` — every
/// other `Ask` reason (active use, high rebuild cost, a live owning tool)
/// is a real judgment about the resource, not a gap in evidence, so it
/// stays `ASK`.
pub fn label_for(decision: &PolicyDecision) -> PolicyLabel {
    let is_evidence_quality_reason = decision.reasons.iter().any(|r| {
        matches!(
            r,
            ReasonCode::EvidenceIncomplete
                | ReasonCode::EvidenceProbeFailed
                | ReasonCode::EvidenceStale
        )
    });

    match decision.class {
        PolicyClass::AutoSafe => PolicyLabel::AutoSafe,
        PolicyClass::Protected => PolicyLabel::Protected,
        PolicyClass::Ask if is_evidence_quality_reason => PolicyLabel::UnknownIncomplete,
        PolicyClass::Ask => PolicyLabel::Ask,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;
    use crate::evidence::{ResourceId, ResourceKind, ResourceLocator};

    fn decision(class: PolicyClass, reasons: Vec<ReasonCode>) -> PolicyDecision {
        PolicyDecision {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from("/tmp/x")),
            ),
            class,
            reasons,
            evidence_collected_at: SystemTime::UNIX_EPOCH,
            evaluated_at: SystemTime::UNIX_EPOCH,
            policy_version: 1,
        }
    }

    #[test]
    fn auto_safe_maps_to_auto_safe_label() {
        let d = decision(
            PolicyClass::AutoSafe,
            vec![ReasonCode::EvidenceFreshAndComplete],
        );
        assert_eq!(label_for(&d), PolicyLabel::AutoSafe);
        assert_eq!(label_for(&d).as_str(), "AUTO_SAFE");
    }

    #[test]
    fn protected_maps_to_protected_label_regardless_of_reasons() {
        let d = decision(
            PolicyClass::Protected,
            vec![ReasonCode::ProtectedCredentialMaterial],
        );
        assert_eq!(label_for(&d), PolicyLabel::Protected);
    }

    #[test]
    fn ask_with_active_use_reason_stays_ask() {
        let d = decision(PolicyClass::Ask, vec![ReasonCode::ResourceInActiveUse]);
        assert_eq!(label_for(&d), PolicyLabel::Ask);
    }

    #[test]
    fn ask_with_rebuild_cost_high_stays_ask() {
        let d = decision(PolicyClass::Ask, vec![ReasonCode::RebuildCostHigh]);
        assert_eq!(label_for(&d), PolicyLabel::Ask);
    }

    #[test]
    fn ask_with_evidence_incomplete_is_unknown_incomplete() {
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
        assert_eq!(label_for(&d).as_str(), "UNKNOWN_INCOMPLETE");
    }

    #[test]
    fn ask_with_evidence_probe_failed_is_unknown_incomplete() {
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceProbeFailed]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
    }

    #[test]
    fn ask_with_evidence_stale_is_unknown_incomplete() {
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceStale]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
    }

    #[test]
    fn display_matches_as_str() {
        let d = decision(PolicyClass::AutoSafe, vec![]);
        assert_eq!(label_for(&d).to_string(), "AUTO_SAFE");
    }
}
