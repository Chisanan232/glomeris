//! Developer-workspace aggregation (HORO-1511): grouping discovered
//! resources by the git worktree family they belong to, so a person can be
//! told "this project consumes 30 GiB across nine worktrees" instead of
//! reading thirty unrelated-looking lines.
//!
//! # This module has no authority
//!
//! Everything here is explanatory and prioritisation metadata. Nothing in
//! it may make anything executable, and nothing that consults it may
//! become a step in deciding whether a resource can be deleted:
//!
//! - Execution stays per actual typed resource, through the action a
//!   detector registered and the policy class the engine assigned to it.
//!   A group has no action, no action id and no `executable` field —
//!   structurally, not by convention. A caller that wants to know what may
//!   run has to go back to the member's own candidate report, which is why
//!   [`WorkspaceMember`] carries the resource id and nothing that could be
//!   mistaken for permission.
//! - A dirty, active or unpushed worktree stays protected by its own
//!   evidence whatever the family around it looks like. "The other eight
//!   worktrees are merged" is not a fact about the ninth.
//! - "This family consumes 30 GiB" is a reason to *look*, never a reason
//!   to delete. The aggregate is an attention figure: it is assembled from
//!   candidate estimates, and campaign section 9 reserves completion and
//!   progress for re-measured filesystem state.
//!
//! The corresponding structural guarantee is checked by
//! `scripts/check-workspace-aggregation-has-no-authority.sh`: no module
//! under `src/policy`, `src/executor`, `src/autopilot` or `src/actions`
//! may reference this one.
//!
//! # Why the branch facts are not in `Evidence`
//!
//! `merged_into_default`, `ahead`/`behind` and the branch name are the
//! most persuasive things this repository can compute about a directory —
//! "that branch is already merged" reads as permission. Put on
//! [`crate::evidence::Evidence`] they would be one field access away from
//! the policy engine, and the temptation to add "…or the branch is merged"
//! to a classification rule would be permanent. They live here instead,
//! reachable from reporting and from nothing that decides.
//!
//! [`crate::evidence::GitState::common_dir`] *is* on `Evidence`, because
//! it is an identity rather than a safety signal: it says which family a
//! resource belongs to and nothing about whether the resource may go.

pub mod branch;
mod graph;
mod group;

pub use branch::{
    BranchProbe, Divergence, EquivalenceMethod, GitCliBranchProbe, IntegrationEvidence,
    MergedState, PatchEquivalence, UpstreamState, WorktreeBranchState,
};
pub use graph::{
    ActivityFacts, BranchLifecycle, ExternalContext, ExternalFact, ExternalSource,
    GlobalResourceNode, HistoryConfidence, MachineContext, PullRequestState, RepositoryNode,
    ResourceNode, TaskState, UniqueWork, UnplacedResourceNode, WorkflowHistorySummary,
    WorkflowMode, WorkspaceEvidenceGraph, WorktreeNode, MIN_OBSERVATIONS_FOR_A_PATTERN,
};
pub use group::{
    group_families, ActivityState, WorkspaceFamily, WorkspaceMember, WorkspaceSurvey,
    WorkspaceWorktree,
};
