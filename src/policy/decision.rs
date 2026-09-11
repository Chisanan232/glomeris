//! [`PolicyDecision`]: the output of one [`crate::policy::engine::classify`]
//! call (HORO-950).

use std::time::SystemTime;

use crate::evidence::ResourceId;

use super::class::{PolicyClass, ReasonCode};

/// One deterministic classification of a resource, produced by
/// [`crate::policy::engine::classify`].
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyDecision {
    pub resource: ResourceId,
    pub class: PolicyClass,
    /// Non-empty, ordered most-significant reason first.
    pub reasons: Vec<ReasonCode>,
    /// When the [`crate::evidence::Evidence`] this decision was computed
    /// from was collected — carried through so a caller can independently
    /// judge staleness without re-deriving it.
    pub evidence_collected_at: SystemTime,
    /// When `classify` was called (the `now` parameter it was given).
    pub evaluated_at: SystemTime,
    /// Bumped manually on any rule-semantics change to `classify`, so a
    /// consumer can tell whether a cached decision was produced under the
    /// current rules.
    pub policy_version: u32,
}
