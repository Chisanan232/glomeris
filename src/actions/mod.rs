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
/// This is deliberately a construct-only type: an [`ActionPlan`] can only
/// come from a trusted [`Action::plan`] implementation running in this
/// process, never from parsed external/model/network input. That is the
/// structural guard against an LLM (or any other untrusted input source)
/// composing a plan directly and handing it to
/// [`crate::executor::execute`] — there is no `from_str`/`from_json`/
/// `Deserialize` path into this type, so the only way to get one is to
/// call a real, reviewed `Action::plan` implementation. Adding
/// `Deserialize` here would silently remove that guarantee.
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
        /// point `execute()` could observe it. `None` means the tool is
        /// genuinely unscoped by design (see e.g.
        /// `HomebrewCleanupCache`) — not that no check was thought about.
        scoped_path: Option<PathBuf>,
    },
    /// The executor re-canonicalizes this path immediately before use and
    /// never trusts it as-is — see [`crate::executor::execute`].
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
    fn builtin_registry_registers_three_actions() {
        let registry = ActionRegistry::builtin();
        assert_eq!(registry.actions.len(), 3);
    }
}
