//! Evidence domain model (HORO-948).
//!
//! Detectors (see [`crate::detectors`]) discover candidate resources and
//! describe what they observed about each one as [`Evidence`]. This module
//! defines that shape and its derived [`Completeness`]/[`Confidence`]
//! judgments — it does not decide what to do about a resource; that is the
//! policy layer's job (future ticket).

pub mod model;
pub mod probe;

pub use model::{
    ActionId, Completeness, Confidence, Evidence, EvidenceField, GitState, NativeCleanup,
    OwningTool, ProcessRef, Recoverability, Regenerability, ResourceFingerprint, ResourceId,
    ResourceKind, ResourceLocator,
};
pub use probe::{ProbeOutcome, ProbeReason};
