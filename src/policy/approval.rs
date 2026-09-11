//! Approval gate: the bypass invariant enforced as a type, not a
//! convention (HORO-950).
//!
//! [`Approval`] can only be constructed inside this module (see
//! [`Seal`]) — the construction path itself ([`authorize`]) lands in a
//! follow-up commit; this commit only defines the shapes it will build.

use std::time::SystemTime;

use crate::evidence::{ResourceFingerprint, ResourceId};

use super::class::PolicyClass;
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

/// The ONLY way to construct an [`Approval`].
///
/// - `AutoSafe` -> always `Some(Approval)`.
/// - `Ask` -> `Some` only when `consent` is `Some`, `consent.resource ==
///   decision.resource`, AND `consent.fingerprint == fingerprint` (consent
///   was granted for this exact resource *identity*, not merely a
///   same-kind resource at the same path).
/// - `Protected` -> always `None`, unconditionally. There is no override
///   parameter that can change this.
pub fn authorize(
    decision: PolicyDecision,
    fingerprint: ResourceFingerprint,
    consent: Option<&UserConsent>,
) -> Option<Approval> {
    match decision.class {
        PolicyClass::Protected => None,
        PolicyClass::AutoSafe => Some(Approval {
            decision,
            fingerprint,
            _seal: Seal,
        }),
        PolicyClass::Ask => {
            let consent = consent?;
            if consent.resource != decision.resource {
                return None;
            }
            if consent.fingerprint != fingerprint {
                return None;
            }
            Some(Approval {
                decision,
                fingerprint,
                _seal: Seal,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use crate::evidence::ResourceLocator;

    use super::*;
    use crate::policy::class::ReasonCode;

    fn resource(path: &str) -> ResourceId {
        ResourceId::new(
            crate::evidence::ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from(path)),
        )
    }

    fn fingerprint(mtime_secs: u64) -> ResourceFingerprint {
        ResourceFingerprint {
            dev_ino: None,
            mtime: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(mtime_secs)),
            tool_revision: None,
        }
    }

    fn decision(resource: ResourceId, class: PolicyClass) -> PolicyDecision {
        PolicyDecision {
            resource,
            class,
            reasons: vec![ReasonCode::EvidenceFreshAndComplete],
            evidence_collected_at: SystemTime::UNIX_EPOCH,
            evaluated_at: SystemTime::UNIX_EPOCH,
            policy_version: 1,
        }
    }

    #[test]
    fn auto_safe_always_authorizes_without_consent() {
        let r = resource("/tmp/target");
        let d = decision(r.clone(), PolicyClass::AutoSafe);
        let approval = authorize(d, fingerprint(1), None);
        assert!(approval.is_some());
    }

    #[test]
    fn protected_never_authorizes_even_with_matching_consent() {
        let r = resource("/tmp/target");
        let fp = fingerprint(1);
        let consent = UserConsent::new(r.clone(), fp.clone(), SystemTime::UNIX_EPOCH);
        let d = decision(r, PolicyClass::Protected);
        let approval = authorize(d, fp, Some(&consent));
        assert!(approval.is_none());
    }

    #[test]
    fn ask_without_consent_does_not_authorize() {
        let r = resource("/tmp/target");
        let d = decision(r, PolicyClass::Ask);
        let approval = authorize(d, fingerprint(1), None);
        assert!(approval.is_none());
    }

    #[test]
    fn ask_with_matching_consent_authorizes() {
        let r = resource("/tmp/target");
        let fp = fingerprint(1);
        let consent = UserConsent::new(r.clone(), fp.clone(), SystemTime::UNIX_EPOCH);
        let d = decision(r, PolicyClass::Ask);
        let approval = authorize(d, fp, Some(&consent));
        assert!(approval.is_some());
    }

    /// Race simulation: consent was granted for resource A's fingerprint,
    /// but the resource changed (new fingerprint) between consent and
    /// authorization — must NOT authorize the now-different instance.
    #[test]
    fn ask_consent_does_not_authorize_when_fingerprint_changed_since_grant() {
        let r = resource("/tmp/target");
        let consent = UserConsent::new(r.clone(), fingerprint(1), SystemTime::UNIX_EPOCH);
        let d = decision(r, PolicyClass::Ask);
        // fingerprint at authorization time differs from the one consent
        // was granted for.
        let approval = authorize(d, fingerprint(2), Some(&consent));
        assert!(approval.is_none());
    }

    /// Consent granted for resource A must not authorize a different
    /// resource B, even at the same PolicyClass and even with the same
    /// fingerprint value.
    #[test]
    fn ask_consent_for_one_resource_does_not_authorize_a_different_resource() {
        let resource_a = resource("/tmp/a/target");
        let resource_b = resource("/tmp/b/target");
        let fp = fingerprint(1);
        let consent = UserConsent::new(resource_a, fp.clone(), SystemTime::UNIX_EPOCH);
        let d = decision(resource_b, PolicyClass::Ask);
        let approval = authorize(d, fp, Some(&consent));
        assert!(approval.is_none());
    }

    #[test]
    fn approval_exposes_decision_and_fingerprint() {
        let r = resource("/tmp/target");
        let fp = fingerprint(1);
        let d = decision(r.clone(), PolicyClass::AutoSafe);
        let approval = authorize(d.clone(), fp.clone(), None).unwrap();
        assert_eq!(approval.decision(), &d);
        assert_eq!(approval.fingerprint(), &fp);
    }
}
