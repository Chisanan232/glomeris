//! Evidence domain model (HORO-948).
//!
//! Detectors (see [`crate::detectors`]) discover candidate resources and
//! describe what they observed about each one as [`Evidence`]. This module
//! defines that shape and its derived [`Completeness`]/[`Confidence`]
//! judgments — it does not decide what to do about a resource; that is the
//! policy layer's job (future ticket).

pub mod correlate;
pub mod docker;
pub mod model;
pub mod probe;

pub use correlate::derive_claim;
pub use docker::{
    daemon_unreachable, DockerActivity, DockerLifecycle, DockerPersistence, DockerReferences,
};
pub use model::{
    decode_fingerprint_token, encode_fingerprint_token, ActionId, Completeness, Confidence,
    DependencyRef, Evidence, EvidenceField, ExeIdentity, ExecutableDependencyReport,
    FingerprintTokenError, GitState, NativeCleanup, OwningTool, ProcessClaim, ProcessIdentity,
    ProcessRef, Provenance, Recoverability, Regenerability, ResourceFingerprint, ResourceId,
    ResourceKind, ResourceLocator, SourceTag, Supervisor, UnresolvedRef,
};
pub use probe::{ProbeOutcome, ProbeReason};
