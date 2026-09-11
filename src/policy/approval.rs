//! Approval gate: the bypass invariant enforced as a type, not a
//! convention (HORO-950).
//!
//! [`Approval`] can only be constructed inside this module (see
//! [`Seal`]) — the construction path itself ([`authorize`]) lands in a
//! follow-up commit; this commit only defines the shapes it will build.

use std::time::SystemTime;

use crate::evidence::{ResourceFingerprint, ResourceId};

use super::decision::PolicyDecision;

/// Explicit human consent to act on one specific resource *instance* — not
/// just a resource kind. `fingerprint` pins consent to the exact instance
/// identity observed at grant time, so a same-kind different instance (or
/// the same path after its underlying resource changed) cannot reuse it.
#[derive(Debug, Clone, PartialEq)]
pub struct UserConsent {
    resource: ResourceId,
    fingerprint: ResourceFingerprint,
    granted_at: SystemTime,
}

impl UserConsent {
    pub fn new(
        resource: ResourceId,
        fingerprint: ResourceFingerprint,
        granted_at: SystemTime,
    ) -> Self {
        Self {
            resource,
            fingerprint,
            granted_at,
        }
    }
}

/// A private, unconstructible-outside-this-module marker. Its only purpose
/// is to make [`Approval`] impossible to build anywhere except inside this
/// module's own code — there is no public constructor, no `Default`, no
/// way to name a value of this type from outside `approval.rs`.
struct Seal;

/// Proof that a [`PolicyDecision`] has been authorized for execution.
/// Holding an `Approval` is the only thing an executor (HORO-951) should
/// ever check before acting — never re-deriving "is this okay" from the
/// `PolicyDecision` alone.
pub struct Approval {
    decision: PolicyDecision,
    fingerprint: ResourceFingerprint,
    _seal: Seal,
}

impl Approval {
    pub fn decision(&self) -> &PolicyDecision {
        &self.decision
    }

    pub fn fingerprint(&self) -> &ResourceFingerprint {
        &self.fingerprint
    }
}
