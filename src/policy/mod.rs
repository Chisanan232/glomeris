//! Policy layer (HORO-950): the single deterministic module that decides
//! `AUTO_SAFE` / `ASK` / `PROTECTED` for a resource's [`crate::evidence::Evidence`].
//!
//! Canonical safety invariant: AI can recommend. Policy decides. Executor
//! verifies. Filesystem reality wins.

pub mod approval;
pub mod class;
pub mod config;
pub mod decision;
pub mod engine;
mod protected;

pub use approval::{Approval, UserConsent};
pub use class::{PolicyClass, ReasonCode};
pub use config::PolicyConfig;
pub use decision::PolicyDecision;
pub use engine::classify;
