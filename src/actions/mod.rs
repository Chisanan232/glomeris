//! Typed action registry (HORO-951).
//!
//! An [`Action`] turns [`Evidence`] into an [`ActionPlan`] — a closed,
//! typed description of exactly what would be run/deleted. Nothing in
//! this module executes anything; that is [`crate::executor`]'s job. No
//! LLM-composed string ever reaches [`ActionStep`] construction: an LLM
//! (or any caller) may only select a pre-registered [`ActionId`] via
//! `ActionRegistry::get` (added in a follow-up commit) and hand it a
//! resource's [`Evidence`] — the typed [`Action::plan`] implementation
//! decides the actual steps.

mod cargo;
mod homebrew;
pub mod llm;
mod node;

use std::path::PathBuf;

pub use cargo::CargoCleanTargetDir;
pub use homebrew::HomebrewCleanupCache;
pub use node::NodeCleanNodeModules;

// Reuse the exact `ActionId` type `evidence::model` already defines for
// this purpose — see that module's doc comment: "the future `actions`
// module will reuse this same type rather than defining its own."
pub use crate::evidence::model::ActionId;
use crate::evidence::model::{Evidence, EvidenceField, Recoverability, ResourceId, ResourceKind};
use crate::evidence::probe::ProbeOutcome;

/// One pre-registered, typed cleanup action.
pub trait Action: Send + Sync {
    fn id(&self) -> ActionId;
    fn applies_to(&self) -> &'static [ResourceKind];
    /// Precondition documentation ONLY — this list is never consulted by
    /// [`crate::evidence::Evidence::completeness`]. It exists so a caller
    /// (or a reviewer) can see which evidence fields this action's
    /// [`Action::plan`] actually reads.
    fn required_evidence(&self) -> &'static [EvidenceField];
    fn recoverability(&self) -> Recoverability;
    /// PURE: builds a plan from evidence, does not execute anything.
    /// Dry-run ([`crate::executor::dry_run`]) and real execution
    /// ([`crate::executor::execute`]) both call exactly this method on
    /// the same [`Evidence`] — that identity is what makes "dry-run shows
    /// exactly what will happen" a structural guarantee, not just a docs
    /// promise.
    fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError>;
}

/// Why [`Action::plan`] could not build a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The evidence's resource kind is not in [`Action::applies_to`].
    ResourceMismatch,
    MissingRequiredEvidence(EvidenceField),
    Unsupported(String),
}

/// SAFETY-CRITICAL: never add `#[derive(serde::Deserialize)]` (or any
/// other deserialization impl) to this type, under any circumstance —
/// including "just for testing".
///
/// The actual guarantee this type provides: there is no `from_str`/
/// `from_json`/`Deserialize` path into it, so no EXTERNAL input — a parsed
/// network payload, LLM-composed text, anything from outside this process
/// — can ever materialize an [`ActionPlan`] or [`ActionStep`] directly.
/// The only way external input reaches execution is by selecting a
/// pre-registered [`ActionId`] via `ActionRegistry::get` and handing it
/// [`Evidence`] for a real [`Action::plan`] implementation to interpret.
///
/// This is NOT a guarantee that this type is only constructible inside
/// this crate. `ActionPlan`'s, `ActionStep`'s, and `ToolBinary`'s fields
/// are `pub` — not `pub(crate)` — and re-exported at `glomeris::actions`,
/// so ANY external crate depending on `glomeris` as a library can
/// hand-build a value of this type too, not just intra-crate code.
/// Nothing currently executes an externally-supplied or hand-built plan
/// (every call to [`crate::executor::execute`]/[`crate::executor::dry_run`]
/// in this codebase is fed a plan freshly returned by a real
/// `Action::plan` call), so today's guarantee rests entirely on there
/// being no execution-facing API — anywhere, including for a downstream
/// library consumer — that accepts a caller-supplied plan, not on the
/// fields being inaccessible to anyone. If that ever changes, or if this
/// crate is ever consumed as a library by untrusted code, revisit whether
/// these fields should become private with constructor functions instead.
#[derive(Debug, Clone)]
pub struct ActionPlan {
    pub action: ActionId,
    pub resource: ResourceId,
    pub steps: Vec<ActionStep>,
    pub expected_reclaimed_bytes: ProbeOutcome<u64>,
    /// Human-readable, rendered FROM the typed `steps` above — never
    /// freeform text composed independently of them.
    pub explain: String,
}

/// One concrete, typed step of an [`ActionPlan`].
#[derive(Debug, Clone)]
pub enum ActionStep {
    RunTool {
        tool: ToolBinary,
        args: Vec<String>,
        /// The single path this command's blast radius is scoped to, if
        /// any — the executor re-verifies this path's filesystem identity
        /// immediately before spawning `tool` and refuses to run it if the
        /// identity has changed (e.g. a symlink swap) since the earliest
        /// point `execute()` could observe it. `None` means there is
        /// nothing for that revalidation to check, so the executor
        /// refuses to run the step at all (see e.g. `HomebrewCleanupCache`,
        /// which is unscoped by design and is therefore never actually
        /// executed by `executor::execute` today, fail-closed) — this is
        /// not a lesser-verified variant, it is an unexecutable one.
        scoped_path: Option<PathBuf>,
    },
    /// The executor re-verifies this path's filesystem identity
    /// immediately before use and never trusts it as-is — see
    /// [`crate::executor::execute`].
    DeletePath { path: PathBuf },
}

/// CLOSED set of binaries a [`ActionStep::RunTool`] may invoke. No
/// arbitrary program is representable in this type — even a compromised
/// upstream caller could only ever select one of these five, never an
/// arbitrary command.
///
/// Docker is deliberately excluded here per the accepted HORO-948 design
/// cut: Docker stays detect-only, with no registered cleanup action in
/// this ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolBinary {
    Brew,
    Cargo,
    Npm,
    Pnpm,
    Yarn,
}

impl ToolBinary {
    pub fn program(&self) -> &'static str {
        match self {
            ToolBinary::Brew => "brew",
            ToolBinary::Cargo => "cargo",
            ToolBinary::Npm => "npm",
            ToolBinary::Pnpm => "pnpm",
            ToolBinary::Yarn => "yarn",
        }
    }
}

/// Plain compile-time list of the built-in actions — deliberately NOT a
/// plugin/inventory registration system, matching
/// [`crate::detectors::DetectorRegistry`]'s own reasoning. Built-in
/// actions are registered one at a time in follow-up commits as each is
/// implemented.
pub struct ActionRegistry {
    actions: Vec<Box<dyn Action>>,
}

impl ActionRegistry {
    /// Registers all built-in actions.
    pub fn builtin() -> Self {
        Self {
            actions: vec![
                Box::new(CargoCleanTargetDir),
                Box::new(NodeCleanNodeModules),
                Box::new(HomebrewCleanupCache),
            ],
        }
    }

    /// Look up a registered action by its stable id string. Returns
    /// `None` for an unknown id — callers must hard-error on `None`,
    /// never silently skip the requested action.
    pub fn get(&self, id: &str) -> Option<&dyn Action> {
        self.actions
            .iter()
            .find(|action| action.id().0 == id)
            .map(|action| action.as_ref())
    }

    /// Stable ids of every registered action whose [`Action::applies_to`]
    /// includes `kind` — additive read-only accessor (HORO-954) so
    /// `actions::llm::LlmResourceView` can report which real actions are
    /// actually registered for a resource's kind, without exposing the
    /// private `actions` field or its ordering.
    pub fn ids_for_kind(&self, kind: ResourceKind) -> Vec<&'static str> {
        self.actions
            .iter()
            .filter(|action| action.applies_to().contains(&kind))
            .map(|action| action.id().0)
            .collect()
    }

    /// Finds a registered action whose [`Action::applies_to`] includes
    /// `kind`. Used by HORO-952's recovery loop as a fallback when a
    /// candidate's `Evidence::native_cleanup` is
    /// [`crate::evidence::model::NativeCleanup::Unsupported`] even though
    /// a real action exists for the resource's kind — every built-in
    /// detector (HORO-948) currently always sets `native_cleanup` to
    /// `Unsupported` at discovery time (populating
    /// `NativeCleanup::Available` from a real registry lookup was noted
    /// as future work in those detectors' own doc comments, e.g.
    /// `detectors::homebrew`), so relying on `NativeCleanup::Available`
    /// alone would make every action registered here unreachable from
    /// real discovery output today.
    pub fn find_for_kind(&self, kind: ResourceKind) -> Option<&dyn Action> {
        self.actions
            .iter()
            .find(|action| action.applies_to().contains(&kind))
            .map(|action| action.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_binary_program_names_are_stable() {
        assert_eq!(ToolBinary::Brew.program(), "brew");
        assert_eq!(ToolBinary::Cargo.program(), "cargo");
        assert_eq!(ToolBinary::Npm.program(), "npm");
        assert_eq!(ToolBinary::Pnpm.program(), "pnpm");
        assert_eq!(ToolBinary::Yarn.program(), "yarn");
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let registry = ActionRegistry::builtin();
        assert!(registry.get("docker.clean.everything").is_none());
    }

    #[test]
    fn get_finds_cargo_action_by_id() {
        let registry = ActionRegistry::builtin();
        assert!(registry.get("cargo.clean.target_dir").is_some());
    }

    #[test]
    fn get_finds_node_action_by_id() {
        let registry = ActionRegistry::builtin();
        assert!(registry.get("node.clean.node_modules").is_some());
    }

    #[test]
    fn get_finds_homebrew_action_by_id() {
        let registry = ActionRegistry::builtin();
        assert!(registry.get("homebrew.cleanup.cache").is_some());
    }

    #[test]
    fn find_for_kind_finds_cargo_action_for_cargo_target_dir() {
        let registry = ActionRegistry::builtin();
        let action = registry
            .find_for_kind(ResourceKind::CargoTargetDir)
            .expect("expected a registered action for CargoTargetDir");
        assert_eq!(action.id(), ActionId("cargo.clean.target_dir"));
    }

    #[test]
    fn find_for_kind_returns_none_for_a_kind_with_no_registered_action() {
        let registry = ActionRegistry::builtin();
        assert!(registry
            .find_for_kind(ResourceKind::DockerBuildCache)
            .is_none());
    }

    #[test]
    fn builtin_registry_registers_three_actions() {
        let registry = ActionRegistry::builtin();
        assert_eq!(registry.actions.len(), 3);
    }
}
