//! Evidence domain model (HORO-948).
//!
//! Detectors (see [`crate::detectors`]) discover candidate resources and
//! describe what they observed about each one as `Evidence`. This module
//! defines that shape and its derived completeness/confidence judgments —
//! it does not decide what to do about a resource; that is the policy
//! layer's job (future ticket).

pub mod probe;

pub use probe::{ProbeOutcome, ProbeReason};
