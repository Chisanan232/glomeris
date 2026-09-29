//! The one place a resource alias and an action id meet (HORO-1542).
//!
//! # Why this is a module and not a function in `crate::actions::llm`
//!
//! Two constraints of this campaign pull against each other. The model has
//! to be told which actions are on offer for a resource, or it cannot rank
//! anything. And [`crate::workspace`] — where the evidence graph lives —
//! may not name an action id at all, which
//! `scripts/check-workspace-aggregation-has-no-authority.sh` enforces
//! literally, because HORO-1511 established that a group must never gain
//! authority from what its members can do.
//!
//! So the join happens here. [`crate::workspace::WorkspaceEvidenceGraph`]
//! knows the relationships and nothing about actions;
//! [`crate::actionability`] knows what is on offer and nothing about
//! workspaces; this module imports both, produces one bounded projection,
//! and imports nothing that decides. The dependency direction is fixed and
//! one-way: planner → {workspace, actionability, actions}. Nothing under
//! `src/policy`, `src/executor`, `src/autopilot` or `src/actions` may
//! import this module, for the same reason none of them may import
//! `crate::workspace`.
//!
//! # This module has no authority either
//!
//! Its output is a ranking suggestion. The contract
//! [`crate::actions::llm::plan_with_llm`] already states — the caller MUST
//! re-run [`crate::policy::classify`] against freshly collected evidence
//! before anything executes — is unchanged and unweakened by anything here.
//! An alias table maps opaque wire ids back to real [`crate::evidence::ResourceId`]s,
//! and resolving one yields a resource to *re-evaluate*, never a resource
//! to delete.
//!
//! # The split between [`dto`] and [`project`]
//!
//! [`dto`] holds the serializable view types and only those. It may not
//! name `PathBuf`, `Path`, `std::path`, [`crate::evidence::ResourceId`] or
//! [`crate::evidence::Evidence`] — the guard script checks for those tokens
//! — which makes the campaign's first egress rule a property of the module
//! rather than a test over its output: a module that cannot name a path
//! cannot serialize one.
//!
//! [`project`] does the joining. It reads the graph, the candidates and the
//! action registry, hands out aliases, and keeps the alias table on the
//! non-serializable side.

//! # The planner contract is versioned (HORO-1548)
//!
//! [`contract`] holds the closed vocabularies a planner response may use and
//! the version number that says which set applies. It is deliberately the
//! smallest module here and depends on nothing: the words a model is allowed
//! to say should be readable in one sitting, without reading the parser.
//!
//! Version 1 — the flat ranking contract in [`crate::actions::llm`] — keeps
//! working unchanged. It was never versioned on the wire, so a response that
//! declares no version is read as v1 rather than guessed at.

pub mod contract;
pub mod dto;
pub mod plan;
pub mod probe;
pub mod project;
pub mod prompt;
pub mod response;
pub mod validate;

pub use contract::{
    ClaimConfidence, Disposition, ObservationKind, ProbeId, ProbeSubjectKind,
    PLANNER_CONTRACT_VERSION,
};
pub use dto::{
    ActivityView, BranchView, DockerLifecycleView, ExternalFactView, MachineView, ModelGraphView,
    PatchEquivalenceView, PlannerRequestView, ProbeFindingView, ProbeResultView, Reported,
    RepositoryView, ResourceView, UnplacedResourceView, WorkflowHistoryView, WorkflowSupportView,
    WorktreeView, MACHINE_EVIDENCE_REF, WORKFLOW_HISTORY_EVIDENCE_REF,
};
pub use plan::{build_workspace_request, plan_workspace, WorkspacePlanResult, WorkspaceRequest};
pub use probe::{LocalProbeRunner, ProbeRunner};
pub use project::{AliasTable, GraphProjection, ProbeSubject, SubjectTable};
pub use prompt::system_prompt;
pub use response::{
    read_planner_response, EvidenceRequestClaim, ObservationClaim, PlanItemClaim, PlannerResponse,
    PlannerResponseClaim, ResponseError, WorkspaceProfileClaim,
};
pub use validate::{
    validate_response, PlanValidationCounters, ValidatedEvidenceRequest, ValidatedObservation,
    ValidatedProfile, ValidatedWorkspaceItem, ValidatedWorkspacePlan,
};
