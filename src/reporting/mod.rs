//! Report formatting (HORO-955): pure, dependency-free presentation logic
//! shared by every `glomeris` CLI subcommand's output.
//!
//! Nothing here does I/O or touches evidence/policy/executor logic — it
//! only turns already-computed domain values (`Evidence`,
//! `PolicyDecision`, a byte count) into human-readable strings or
//! `Serialize`-able DTOs. Orchestration (discovering evidence, refreshing
//! correlation, calling `policy::classify`) lives in [`crate::cli`].

pub mod bytes;
pub mod policy_label;

pub use bytes::human_bytes;
pub use policy_label::{label_for, PolicyLabel};
