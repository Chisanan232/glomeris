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
