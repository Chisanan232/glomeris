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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must map to a distinct, non-empty tag — a CLI report
    /// (HORO-955) reads this as the stable machine-readable reason string.
    #[test]
    fn as_str_is_distinct_and_non_empty_for_every_variant() {
        let all = [
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
        let mut tags: Vec<&'static str> = all.iter().map(|r| r.as_str()).collect();
        let original_len = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), original_len, "as_str tags must be distinct");
        assert!(tags.iter().all(|t| !t.is_empty()));
    }
}
