//! Policy classification outcomes and reason codes (HORO-950).

/// The three deterministic outcomes of policy classification.
///
/// There is no separate "Unknown" 4th outcome. Missing/stale/failed
/// evidence maps *inside* [`crate::policy::engine::classify`] to `Ask` or
/// `Protected` with a specific [`ReasonCode`] — it never becomes a 4th
/// policy outcome a consumer might read as "not Protected, therefore
/// fine."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyClass {
    AutoSafe,
    Ask,
    Protected,
}

/// Why a [`PolicyClass`] was reached. A
/// [`crate::policy::decision::PolicyDecision`] carries these ordered
/// most-significant-first; a consumer should read all of them rather than
/// assuming a fixed count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonCode {
    // -> Protected
    ProtectedCredentialMaterial,
    ProtectedGitInternals,
    ProtectedInfraState,
    ProtectedPersistentVolume,
    ProtectedUserDocuments,
    ProtectedSystemPath,
    ProtectedUnsafeMountOrSymlink,
    ProtectedUnknownResourceKind,
    // -> Ask
    EvidenceIncomplete,
    EvidenceStale,
    EvidenceProbeFailed,
    ResourceInActiveUse,
    GitWorktreeDirty,
    RebuildCostHigh,
    OwningToolLive,
    // -> AutoSafe
    EvidenceFreshAndComplete,
    RegenerableByTool,
    NoActiveUseObserved,
}

impl ReasonCode {
    /// Stable, snake_case tag for this reason code — used by CLI report
    /// output (HORO-955) and safe to serialize.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReasonCode::ProtectedCredentialMaterial => "protected_credential_material",
            ReasonCode::ProtectedGitInternals => "protected_git_internals",
            ReasonCode::ProtectedInfraState => "protected_infra_state",
            ReasonCode::ProtectedPersistentVolume => "protected_persistent_volume",
            ReasonCode::ProtectedUserDocuments => "protected_user_documents",
            ReasonCode::ProtectedSystemPath => "protected_system_path",
            ReasonCode::ProtectedUnsafeMountOrSymlink => "protected_unsafe_mount_or_symlink",
            ReasonCode::ProtectedUnknownResourceKind => "protected_unknown_resource_kind",
            ReasonCode::EvidenceIncomplete => "evidence_incomplete",
            ReasonCode::EvidenceStale => "evidence_stale",
            ReasonCode::EvidenceProbeFailed => "evidence_probe_failed",
            ReasonCode::ResourceInActiveUse => "resource_in_active_use",
            ReasonCode::GitWorktreeDirty => "git_worktree_dirty",
            ReasonCode::RebuildCostHigh => "rebuild_cost_high",
            ReasonCode::OwningToolLive => "owning_tool_live",
            ReasonCode::EvidenceFreshAndComplete => "evidence_fresh_and_complete",
            ReasonCode::RegenerableByTool => "regenerable_by_tool",
            ReasonCode::NoActiveUseObserved => "no_active_use_observed",
        }
    }

    /// Every reason code, in declaration order (Protected, then Ask, then
    /// AutoSafe). See the `all_lists_every_variant_exactly_once` test below
    /// for the compile-time guard that keeps this exhaustive.
    pub const ALL: &'static [ReasonCode] = &[
        ReasonCode::ProtectedCredentialMaterial,
        ReasonCode::ProtectedGitInternals,
        ReasonCode::ProtectedInfraState,
        ReasonCode::ProtectedPersistentVolume,
        ReasonCode::ProtectedUserDocuments,
        ReasonCode::ProtectedSystemPath,
        ReasonCode::ProtectedUnsafeMountOrSymlink,
        ReasonCode::ProtectedUnknownResourceKind,
        ReasonCode::EvidenceIncomplete,
        ReasonCode::EvidenceStale,
        ReasonCode::EvidenceProbeFailed,
        ReasonCode::ResourceInActiveUse,
        ReasonCode::GitWorktreeDirty,
        ReasonCode::RebuildCostHigh,
        ReasonCode::OwningToolLive,
        ReasonCode::EvidenceFreshAndComplete,
        ReasonCode::RegenerableByTool,
        ReasonCode::NoActiveUseObserved,
    ];

    /// Parses an [`as_str`](Self::as_str) tag back into its reason code, or
    /// `None` for anything that is not exactly one of them (HORO-1310).
    ///
    /// Strict for the same reason [`crate::evidence::ResourceKind::from_tag`]
    /// is: this is how an Autopilot `ASK` pre-authorization names the one
    /// risk it covers, and `rebuild_cost` quietly meaning
    /// [`ReasonCode::RebuildCostHigh`] would be a way to widen authority by
    /// typo. Parsing a tag is not the same as accepting it —
    /// `crate::autopilot::AutopilotEnvelope` separately refuses to
    /// pre-authorize most of what parses here.
    pub fn from_tag(tag: &str) -> Option<ReasonCode> {
        ReasonCode::ALL.iter().copied().find(|r| r.as_str() == tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must map to a distinct, non-empty tag — a CLI report
    /// (HORO-955) reads this as the stable machine-readable reason string.
    ///
    /// Reads [`ReasonCode::ALL`] rather than the hand-written list it used to
    /// carry (HORO-1310): that list was a mirror of the enum, and a mirror is
    /// a thing that drifts. `all_lists_every_variant_exactly_once` below is
    /// what keeps `ALL` honest, so everything reading it inherits that.
    #[test]
    fn as_str_is_distinct_and_non_empty_for_every_variant() {
        let mut tags: Vec<&'static str> = ReasonCode::ALL.iter().map(|r| r.as_str()).collect();
        let original_len = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), original_len, "as_str tags must be distinct");
        assert!(tags.iter().all(|t| !t.is_empty()));
    }

    /// Compile-time guard: the `match` is exhaustive, so a new variant that
    /// is not added to [`ReasonCode::ALL`] fails to build here. The length
    /// assertion catches the reverse (a stale or duplicated entry).
    #[test]
    fn all_lists_every_variant_exactly_once() {
        for (index, reason) in ReasonCode::ALL.iter().enumerate() {
            let expected_index = match reason {
                ReasonCode::ProtectedCredentialMaterial => 0,
                ReasonCode::ProtectedGitInternals => 1,
                ReasonCode::ProtectedInfraState => 2,
                ReasonCode::ProtectedPersistentVolume => 3,
                ReasonCode::ProtectedUserDocuments => 4,
                ReasonCode::ProtectedSystemPath => 5,
                ReasonCode::ProtectedUnsafeMountOrSymlink => 6,
                ReasonCode::ProtectedUnknownResourceKind => 7,
                ReasonCode::EvidenceIncomplete => 8,
                ReasonCode::EvidenceStale => 9,
                ReasonCode::EvidenceProbeFailed => 10,
                ReasonCode::ResourceInActiveUse => 11,
                ReasonCode::GitWorktreeDirty => 12,
                ReasonCode::RebuildCostHigh => 13,
                ReasonCode::OwningToolLive => 14,
                ReasonCode::EvidenceFreshAndComplete => 15,
                ReasonCode::RegenerableByTool => 16,
                ReasonCode::NoActiveUseObserved => 17,
            };
            assert_eq!(
                index,
                expected_index,
                "{} is at index {index} of ReasonCode::ALL, expected {expected_index}",
                reason.as_str()
            );
        }
        assert_eq!(
            ReasonCode::ALL.len(),
            18,
            "ReasonCode::ALL has gained, lost, or duplicated an entry"
        );
    }

    #[test]
    fn every_variant_round_trips_through_its_tag() {
        for reason in ReasonCode::ALL {
            assert_eq!(
                ReasonCode::from_tag(reason.as_str()),
                Some(*reason),
                "tag {:?} did not round-trip",
                reason.as_str()
            );
        }
    }

    #[test]
    fn from_tag_rejects_near_misses_rather_than_guessing() {
        for near_miss in [
            "",
            "rebuild_cost",
            "rebuild-cost-high",
            "REBUILD_COST_HIGH",
            " rebuild_cost_high",
            "rebuild_cost_high ",
            "protected",
            "not_a_reason",
        ] {
            assert_eq!(
                ReasonCode::from_tag(near_miss),
                None,
                "{near_miss:?} must not parse as a reason code"
            );
        }
    }
}
