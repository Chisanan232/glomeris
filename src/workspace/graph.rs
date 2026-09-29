//! The local evidence graph (HORO-1542): one coherent, local-only picture
//! of what this machine is working on, joined from evidence that until now
//! only existed as separate lists.
//!
//! Read [`crate::workspace`]'s header first. This module is inside the
//! no-authority zone HORO-1511 established, and it belongs there for a
//! stronger reason than its predecessor did. A worktree family said "this
//! project consumes 30 GiB across nine checkouts". A graph says that *and*
//! "this checkout's branch is merged, nothing has it open, its task is
//! closed and the developer usually works serially". Each of those is a
//! reason to look; together they read like a conclusion. So the layers that
//! decide and the layer that explains still do not meet, and the CI guard
//! `scripts/check-workspace-aggregation-has-no-authority.sh` still says so
//! mechanically.
//!
//! # What this module is for
//!
//! Before it, an LLM could be shown seven fields per resource and nothing
//! about how resources relate. Expanding that list would not have helped:
//! `reclaimable_bytes` for a `target/` directory means something different
//! depending on whether the checkout above it is mid-rebase with a dirty
//! tree or was abandoned four months ago, and neither fact was reachable
//! from the projection. The graph exists so those relationships are
//! constructed once, locally, with their uncertainty intact — and so the
//! separate question of what may be *said* about them off this machine is
//! answered in exactly one other place (`crate::planner::dto`).
//!
//! # Three buckets, not two
//!
//! [`group_families`] drops any candidate whose git state is not
//! `Observed(Some(..))`, which merges two completely different answers: "I
//! looked, and this is not in a git working tree" and "I could not look".
//! That is fine for a display that only groups what it can group, and wrong
//! for a structure something reasons over. So a resource lands in exactly
//! one of:
//!
//! - [`WorkspaceEvidenceGraph::repositories`] — git state observed, inside
//!   a working tree.
//! - [`WorkspaceEvidenceGraph::global_resources`] — no containing
//!   repository, and that is a complete answer: either the git probe ran and
//!   observed `None`, or the resource is tool-owned and so has no path for a
//!   containing repository to exist under.
//! - [`WorkspaceEvidenceGraph::unplaced_resources`] — a resource that does
//!   have a path, whose git probe could not answer for it. Carries the
//!   [`ProbeReason`] saying so.
//!
//! The third bucket is the whole point. Without it a Cargo `target/` under
//! a repository `git` timed out on would be reported as a global cache,
//! which is the most flattering possible misreading of a failed probe.
//!
//! Which is also why [`WorkspaceEvidenceGraph::build`] reads the *locator*
//! before the probe outcome (HORO-1561). A tool-owned resource's `git_state`
//! is `Unavailable(NotAttempted)` because there was no path to probe, not
//! because a probe went missing — and the two must not share a bucket any
//! more than "not in a repository" and "could not tell" do.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::branch::WorktreeBranchState;
use super::group::ActivityState;
use crate::detectors::DetectorId;
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::evidence::{
    Completeness, DockerLifecycle, Evidence, OwningTool, ProcessRef, Regenerability, ResourceId,
};
use crate::policy::PolicyDecision;
use crate::reporting::{label_for, PolicyLabel};

/// One local-only picture of this machine's development storage.
///
/// Built by [`WorkspaceEvidenceGraph::build`] from candidates the CLI has
/// already discovered and had classified, plus the branch survey. Pure:
/// building a graph runs no subprocess and reads no clock beyond the
/// `built_at` its caller supplies.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceEvidenceGraph {
    pub machine: MachineContext,
    /// One entry per git directory, sorted by it, so two runs over one disk
    /// state produce the same graph.
    pub repositories: Vec<RepositoryNode>,
    /// Resources with no containing repository: observed to sit outside any
    /// git working tree, or tool-owned and so having no path to sit under.
    pub global_resources: Vec<GlobalResourceNode>,
    /// Path-located resources whose git probe could not answer. See the
    /// module header.
    pub unplaced_resources: Vec<UnplacedResourceNode>,
    /// The local workflow baseline, which is deliberately NOT part of any
    /// node above: see [`WorkflowHistorySummary`] for why the separation is
    /// the point rather than a layout choice. `NotAttempted` until
    /// HORO-1547 collects it.
    pub history: ProbeOutcome<WorkflowHistorySummary>,
    /// When this graph was assembled, so a reader can say how old the
    /// picture is. Supplied by the caller rather than read here, matching
    /// `crate::policy::engine::classify`'s no-ambient-clock discipline.
    pub built_at: SystemTime,
}

/// What this machine's storage situation is, as far as anything measured
/// it.
///
/// `recovery_goal_bytes` is an `Option` and the two byte figures are
/// [`ProbeOutcome`]s, and the asymmetry is deliberate: "no goal is set" is a
/// complete answer a caller genuinely knows, whereas "how much is free" is
/// something a probe can fail to establish. Collapsing either into the
/// other's shape would lose exactly the distinction this campaign is about.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineContext {
    pub free_bytes: ProbeOutcome<u64>,
    pub total_bytes: ProbeOutcome<u64>,
    /// How many bytes the active recovery goal still wants, when one is
    /// set. `None` means no goal, not an unknown goal.
    pub recovery_goal_bytes: Option<u64>,
}

impl MachineContext {
    /// A context nobody measured and no goal was set in. The honest
    /// starting point for a caller that has not asked the platform layer
    /// anything — never a zero-filled one, which would claim a full disk.
    pub fn unmeasured() -> Self {
        Self {
            free_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            total_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            recovery_goal_bytes: None,
        }
    }
}

/// One repository: every working tree sharing a git directory.
#[derive(Debug, Clone, PartialEq)]
pub struct RepositoryNode {
    /// The shared git directory, from [`crate::evidence::GitState::common_dir`].
    /// The repository's identity, and the one git fact that lives on
    /// `Evidence` — because it identifies a resource rather than
    /// recommending anything about it.
    pub common_dir: PathBuf,
    /// Sorted by root path.
    pub worktrees: Vec<WorktreeNode>,
}

impl RepositoryNode {
    pub fn worktree_count(&self) -> usize {
        self.worktrees.len()
    }

    /// Every resource under every working tree of this repository.
    pub fn resources(&self) -> impl Iterator<Item = &ResourceNode> {
        self.worktrees.iter().flat_map(|w| w.resources.iter())
    }
}

/// One git working tree and everything known about it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorktreeNode {
    pub root: PathBuf,
    /// `true` when this is a `git worktree add` sibling rather than the
    /// main checkout.
    pub linked: bool,
    pub lifecycle: BranchLifecycle,
    pub activity: ActivityFacts,
    /// Read-only context from outside this machine. Every field starts
    /// `NotAttempted`; HORO-1546 is what populates it, and it is optional
    /// forever.
    pub external: ExternalContext,
    /// Sorted by resource id, so the graph is stable for one disk state.
    pub resources: Vec<ResourceNode>,
    /// The newest `last_modified` any resource here reported. `None` when
    /// no resource's mtime could be read — never an epoch standing in for
    /// unread.
    pub newest_resource_modified_at: Option<SystemTime>,
}

/// Branch state and the two working-tree facts that go with it.
///
/// Three independent things and not one verdict, for the reason
/// [`WorktreeBranchState`] gives: "safe to remove" is not a question this
/// type answers, and a type that answered it would be the authority this
/// module must not have.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchLifecycle {
    pub dirty: bool,
    pub untracked: bool,
    pub branch: ProbeOutcome<WorktreeBranchState>,
}

/// Whether a working tree might hold work that exists nowhere else.
///
/// Three values, and `Unknown` is not a polite `No`. It is what an unread
/// branch probe supports, and [`BranchLifecycle::unique_work`] returns it
/// rather than the tidier answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueWork {
    /// Something here is not represented anywhere else: uncommitted
    /// changes, untracked files, or commits no upstream has.
    Present,
    /// Every probe answered and none of them found anything unique.
    Absent,
    /// At least one probe could not answer, so `Absent` is unsupported.
    Unknown,
}

impl UniqueWork {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Unknown => "unknown",
        }
    }
}

impl BranchLifecycle {
    /// A working tree with no branch state read at all. `dirty`/`untracked`
    /// are the caller's own observations of the directory and have no
    /// unknown state on `Evidence`, so they are taken as given.
    pub fn unsurveyed(dirty: bool, untracked: bool) -> Self {
        Self {
            dirty,
            untracked,
            branch: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        }
    }

    /// Whether this tree holds work nothing else has.
    ///
    /// `dirty` or `untracked` settle it immediately — those are direct
    /// observations, and no branch fact can argue with a modified file. An
    /// unread branch probe yields [`UniqueWork::Unknown`], and a read one
    /// defers to [`super::branch::UpstreamState::may_hold_unpushed_work`],
    /// which already counts "no upstream" and "unknown upstream" as may.
    ///
    /// Note what this deliberately does NOT consult: [`super::branch::MergedState`].
    /// A branch contained in some other branch is not a branch whose working tree
    /// has nothing newer in it, and treating containment as an answer here
    /// is the exact inference HORO-1545 exists to make honest.
    pub fn unique_work(&self) -> UniqueWork {
        if self.dirty || self.untracked {
            return UniqueWork::Present;
        }
        match &self.branch {
            ProbeOutcome::Observed(state) => {
                if state.upstream.may_hold_unpushed_work() {
                    UniqueWork::Present
                } else {
                    UniqueWork::Absent
                }
            }
            ProbeOutcome::Unavailable(_) => UniqueWork::Unknown,
        }
    }
}

/// One resource's two process probes: what has it open, and what is working
/// inside it. Named because they are always read as a pair — either answer
/// alone is half of "is anything using this".
pub type ProcessProbePair = (ProbeOutcome<Vec<ProcessRef>>, ProbeOutcome<Vec<ProcessRef>>);

/// Whether something is using this working tree, and how sure that is.
///
/// The invariant worth stating: `unanswered_probes > 0` and
/// `state == ActivityState::Idle` cannot both hold. It is enforced in
/// [`ActivityFacts::from_probes`] rather than left to callers, because
/// "idle" is precisely the claim a failed probe cannot support and this is
/// the only place the two are combined.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityFacts {
    pub state: ActivityState,
    /// Processes actually observed holding a resource open here or working
    /// inside this tree, deduplicated by pid. Only ever populated from
    /// probes that answered.
    pub observed_processes: Vec<ProcessRef>,
    /// How many resource-level probes could not answer. Non-zero forces
    /// `state` away from `Idle`.
    pub unanswered_probes: usize,
}

impl ActivityFacts {
    /// Folds the per-resource process probes of one working tree into one
    /// answer.
    ///
    /// Takes the probes rather than a pre-computed state so the fold — the
    /// part that can get "failed is not empty" wrong — happens once, here,
    /// with a test on it.
    pub fn from_probes<'a, I>(probes: I) -> Self
    where
        I: IntoIterator<Item = &'a ProcessProbePair>,
    {
        let mut observed: Vec<ProcessRef> = Vec::new();
        let mut unanswered = 0usize;

        for (open_by, cwd_match) in probes {
            for probe in [open_by, cwd_match] {
                match probe {
                    ProbeOutcome::Observed(processes) => {
                        for process in processes {
                            if !observed.iter().any(|seen| seen.pid == process.pid) {
                                observed.push(process.clone());
                            }
                        }
                    }
                    ProbeOutcome::Unavailable(_) => unanswered += 1,
                }
            }
        }

        observed.sort_by_key(|process| process.pid);

        let state = if !observed.is_empty() {
            ActivityState::InUse
        } else if unanswered > 0 {
            ActivityState::Unknown
        } else {
            ActivityState::Idle
        };

        debug_assert!(
            !(unanswered > 0 && state == ActivityState::Idle),
            "a tree with an unanswered probe can never be reported idle"
        );

        Self {
            state,
            observed_processes: observed,
            unanswered_probes: unanswered,
        }
    }
}

/// Read-only context from services outside this machine.
///
/// Both fields are [`ExternalFact`]s and not bare outcomes, because a
/// remote answer without a source and a time is not evidence anyone can
/// weigh — "the PR is merged" matters differently if it was read four
/// weeks ago.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalContext {
    pub pull_request: ExternalFact<PullRequestState>,
    pub task: ExternalFact<TaskState>,
}

impl ExternalContext {
    /// No provider was configured or asked. The default, and what the
    /// product must stay fully useful under.
    pub fn unqueried() -> Self {
        Self {
            pull_request: ExternalFact::not_attempted(ExternalSource::GitHubPullRequests),
            task: ExternalFact::not_attempted(ExternalSource::JiraIssues),
        }
    }
}

/// One fact from outside this machine, with where it came from and when.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalFact<T> {
    pub outcome: ProbeOutcome<T>,
    pub source: ExternalSource,
    /// Set only when `outcome` is `Observed`. An `Unavailable` fact has no
    /// observation to be fresh or stale, and a timestamp on one would
    /// describe the attempt rather than the answer.
    pub observed_at: Option<SystemTime>,
}

impl<T> ExternalFact<T> {
    pub fn not_attempted(source: ExternalSource) -> Self {
        Self {
            outcome: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            source,
            observed_at: None,
        }
    }

    /// An observation, stamped. The only constructor that can set
    /// `observed_at`, which is what keeps the pairing above true.
    pub fn observed(source: ExternalSource, value: T, at: SystemTime) -> Self {
        Self {
            outcome: ProbeOutcome::Observed(value),
            source,
            observed_at: Some(at),
        }
    }

    /// A provider that was asked and could not answer. Distinct from
    /// `NoneObserved` inside an `Observed` value, which is the provider
    /// answering "there is nothing".
    pub fn unavailable(source: ExternalSource, reason: ProbeReason) -> Self {
        Self {
            outcome: ProbeOutcome::Unavailable(reason),
            source,
            observed_at: None,
        }
    }

    /// How old this fact is relative to `now`, or `None` when there is no
    /// observation to age.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        self.observed_at.and_then(|at| now.duration_since(at).ok())
    }
}

/// Which outside service a fact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSource {
    GitHubPullRequests,
    JiraIssues,
}

impl ExternalSource {
    /// Every source, so a surface enumerating providers cannot list a subset.
    pub const ALL: [Self; 2] = [Self::GitHubPullRequests, Self::JiraIssues];

    pub fn tag(self) -> &'static str {
        match self {
            Self::GitHubPullRequests => "github_pull_requests",
            Self::JiraIssues => "jira_issues",
        }
    }
}

/// A branch's pull-request state as a provider reported it.
///
/// [`Self::NoneObserved`] is inside the observed side on purpose: it is a
/// provider that answered and found nothing, which is a different fact from
/// a provider that could not be reached. Those two being the same value is
/// the defect the campaign names explicitly, so they are not even
/// representable as the same thing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Merged,
    ClosedUnmerged,
    NoneObserved,
}

impl PullRequestState {
    /// Every state a provider can report.
    ///
    /// Exists so the egress preview can print the *complete* set of values
    /// this field may carry off the machine rather than a hand-written list
    /// beside it. A preview that under-states its own vocabulary is worse
    /// than no preview: it is a privacy claim nothing checks.
    pub const ALL: [Self; 4] = [
        Self::Open,
        Self::Merged,
        Self::ClosedUnmerged,
        Self::NoneObserved,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::ClosedUnmerged => "closed_unmerged",
            Self::NoneObserved => "none_observed",
        }
    }
}

/// A work item's state, for a branch carrying an explicit ticket key.
///
/// [`Self::Other`] keeps a tracker's own bounded status name rather than
/// forcing every workflow into three buckets, and [`Self::NoneObserved`]
/// is again the answer "asked, nothing associated".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    ToDo,
    InProgress,
    Done,
    Other(String),
    NoneObserved,
}

impl TaskState {
    /// Every tag [`Self::tag`] can produce, for the same reason as
    /// [`PullRequestState::ALL`].
    ///
    /// Tags rather than values, because [`Self::Other`] carries a `String` and
    /// cannot sit in a `const` — and because the tracker's own status name in
    /// that variant is exactly what does *not* travel. `other` is the whole of
    /// what a model is told about a bespoke workflow state, and this list is
    /// where that is visible.
    pub const ALL_TAGS: [&'static str; 5] =
        ["to_do", "in_progress", "done", "other", "none_observed"];

    pub fn tag(&self) -> &'static str {
        match self {
            Self::ToDo => "to_do",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Other(_) => "other",
            Self::NoneObserved => "none_observed",
        }
    }
}

/// One discovered resource as a node of the graph.
///
/// Carries the policy label the engine already assigned, exactly as
/// [`super::group::WorkspaceMember`] does, and for the same reason: so a
/// reader can see how much of a repository's bulk sits behind a
/// confirmation. It is reported, never re-derived, and nothing here
/// branches on it to permit anything.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceNode {
    /// The real local id. Never what a provider sees — see
    /// [`ResourceId`]'s own doc comment and `crate::planner::dto`.
    pub resource: ResourceId,
    pub detector: DetectorId,
    pub owning_tool: OwningTool,
    pub reclaimable_bytes: ProbeOutcome<u64>,
    /// `true` only when `reclaimable_bytes` is an `Observed` value a
    /// bounded walk stopped short of completing. Display-only, exactly as
    /// on [`Evidence`], and never a policy input.
    pub reclaimable_bytes_is_lower_bound: bool,
    /// Age derived from the resource's own mtime against the moment its
    /// evidence was collected — never an ambient clock.
    pub age: ProbeOutcome<Duration>,
    pub regenerability: Regenerability,
    pub completeness: Completeness,
    pub tool_liveness: ProbeOutcome<bool>,
    /// What Docker said about this particular object, for the resources
    /// Docker owns (HORO-1544). `None` for everything else, and that is a
    /// different fact from `Some(DockerLifecycle::unknown(..))`: a Cargo
    /// target directory has no Docker activity to be unknown about, whereas a
    /// Docker image whose daemon stopped answering does.
    ///
    /// Carried here rather than re-derived because the relationships are the
    /// reason the graph exists. `tool_liveness` above is one answer for every
    /// Docker object on the machine — the daemon is up or it is not — and
    /// says nothing about which of them a running container needs.
    pub docker_lifecycle: Option<DockerLifecycle>,
    pub label: PolicyLabel,
}

impl ResourceNode {
    fn from_evidence(ev: &Evidence, decision: &PolicyDecision) -> Self {
        Self {
            resource: ev.resource.clone(),
            detector: ev.detector,
            owning_tool: ev.resource.kind.owning_tool(),
            reclaimable_bytes: ev.reclaimable_bytes.clone(),
            reclaimable_bytes_is_lower_bound: ev.reclaimable_bytes.is_observed()
                && ev.reclaimable_bytes_is_lower_bound,
            age: age_of(ev),
            regenerability: ev.regenerability,
            completeness: ev.completeness(),
            tool_liveness: ev.tool_liveness.clone(),
            docker_lifecycle: ev.docker_lifecycle.clone(),
            label: label_for(decision),
        }
    }

    pub fn age_days(&self) -> ProbeOutcome<u64> {
        match &self.age {
            ProbeOutcome::Observed(age) => ProbeOutcome::Observed(age.as_secs() / 86_400),
            ProbeOutcome::Unavailable(reason) => ProbeOutcome::Unavailable(*reason),
        }
    }
}

/// How old a resource is, from the two timestamps its own evidence carries.
///
/// An mtime newer than the collection time is not an age of zero. It means
/// something wrote to the resource between the two reads, or that a clock
/// moved, and either way the subtraction has no answer — so this reports
/// `Failed` rather than saturating, which would have presented a
/// just-modified directory as brand new *and* as confidently measured.
fn age_of(ev: &Evidence) -> ProbeOutcome<Duration> {
    match &ev.last_modified {
        ProbeOutcome::Observed(modified) => match ev.collected_at.duration_since(*modified) {
            Ok(age) => ProbeOutcome::Observed(age),
            Err(_) => ProbeOutcome::Unavailable(ProbeReason::Failed),
        },
        ProbeOutcome::Unavailable(reason) => ProbeOutcome::Unavailable(*reason),
    }
}

/// A resource observed to sit outside any git working tree: a tool-owned
/// global cache.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalResourceNode {
    pub owning_tool: OwningTool,
    pub resource: ResourceNode,
}

/// A resource whose git probe could not answer.
///
/// Not a global cache and not a repository member — the third bucket from
/// the module header. `reason` is the probe's own, so a reader learns
/// whether `git` was missing, refused, timed out or simply was not run.
#[derive(Debug, Clone, PartialEq)]
pub struct UnplacedResourceNode {
    pub resource: ResourceNode,
    pub reason: ProbeReason,
}

/// How this developer has been working, over more than one observation.
///
/// Kept out of every node above and off [`WorktreeNode`] in particular,
/// because the moment a habit sits beside a fact about today it starts
/// being read as one. A worktree that is dirty, in use and locally ahead
/// must stay exactly that regardless of what the last month looked like,
/// and the cheapest way to guarantee it is for the habit not to be
/// reachable from the worktree at all.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowHistorySummary {
    pub mode: WorkflowMode,
    pub confidence: HistoryConfidence,
    /// How many distinct observations this summary rests on.
    pub observation_count: u32,
    /// The counts the mode was read off, so a reader can check the reading.
    pub support: WorkflowSupport,
}

/// The aggregate counts a [`WorkflowHistorySummary`] was derived from.
///
/// HORO-1547's fourth acceptance criterion asks for supporting evidence beside
/// the classification, and this is it: a mode alone is an assertion, whereas a
/// mode plus "eleven observations over nine days, never more than one working
/// tree, two branch changes" is a claim somebody can disagree with. It is also
/// what makes [`HistoryConfidence::Insufficient`] legible — the counts are
/// reported even when no mode may be claimed from them.
///
/// Every field is a count. There is deliberately no path, no branch name and no
/// repository name here, because this struct is the one part of the history that
/// the model-facing projection is allowed to summarise, and a field that does
/// not exist cannot be forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowSupport {
    /// Days between the oldest and newest observation that counted.
    pub spanning_days: u32,
    /// Distinct repositories seen across those observations.
    pub repositories_observed: u32,
    /// Observations in which every repository seen had more than one working
    /// tree.
    pub parallel_observations: u32,
    /// Observations in which every repository seen had exactly one.
    pub serial_observations: u32,
    /// Observations that saw some of each at the same moment.
    pub mixed_observations: u32,
    /// Times a repository's single checkout was seen on a different branch
    /// than the previous time it was seen as a single checkout.
    pub single_checkout_branch_changes: u32,
    /// The largest number of working trees any one repository was seen with.
    pub most_worktrees_seen_at_once: u32,
}

impl WorkflowSupport {
    /// The support behind a summary that rests on nothing.
    ///
    /// Named rather than `Default`, because zero here means "no observation
    /// supports this", which is a statement and not an uninitialised value.
    pub fn nothing_observed() -> Self {
        Self {
            spanning_days: 0,
            repositories_observed: 0,
            parallel_observations: 0,
            serial_observations: 0,
            mixed_observations: 0,
            single_checkout_branch_changes: 0,
            most_worktrees_seen_at_once: 0,
        }
    }
}

/// Below this many observations no pattern may be claimed, whatever the
/// observations looked like.
///
/// "Usually" is a claim about repetition, and one snapshot cannot support
/// it — a developer with nine worktrees open once is not a developer who
/// works in parallel. [`WorkflowHistorySummary::from_observations`] is the
/// only constructor, so the rule cannot be sidestepped by building the
/// struct literally from outside this module.
pub const MIN_OBSERVATIONS_FOR_A_PATTERN: u32 = 3;

impl WorkflowHistorySummary {
    /// The only way to build a summary.
    ///
    /// Fewer than [`MIN_OBSERVATIONS_FOR_A_PATTERN`] observations produce
    /// [`WorkflowMode::Unknown`] with [`HistoryConfidence::Insufficient`]
    /// no matter what `observed_mode` says. The caller's mode is not
    /// wrong, it is unsupported, and this is the difference.
    ///
    /// `support` is carried through both branches unchanged. Withholding the
    /// counts when the mode is unsupported would make `insufficient` an opaque
    /// refusal, and the counts are exactly what explains it.
    pub fn from_observations(
        observed_mode: WorkflowMode,
        observation_count: u32,
        support: WorkflowSupport,
    ) -> Self {
        if observation_count < MIN_OBSERVATIONS_FOR_A_PATTERN {
            return Self {
                mode: WorkflowMode::Unknown,
                confidence: HistoryConfidence::Insufficient,
                observation_count,
                support,
            };
        }
        Self {
            mode: observed_mode,
            confidence: HistoryConfidence::Observed,
            observation_count,
            support,
        }
    }
}

/// The shape of a developer's observed workflow.
///
/// Describes the work, never the person. There is deliberately no
/// `Advanced`, `Beginner`, `Good` or `Bad` variant: this vocabulary exists
/// to explain where storage came from, and a product that grades its user
/// has stopped doing that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowMode {
    SerialSingleCheckout,
    SerialMultiBranch,
    ParallelMultiWorktree,
    Mixed,
    Unknown,
}

impl WorkflowMode {
    pub fn tag(self) -> &'static str {
        match self {
            Self::SerialSingleCheckout => "serial_single_checkout",
            Self::SerialMultiBranch => "serial_multi_branch",
            Self::ParallelMultiWorktree => "parallel_multi_worktree",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

/// How much a history summary rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryConfidence {
    /// At least [`MIN_OBSERVATIONS_FOR_A_PATTERN`] observations agreed
    /// enough for the caller to name a mode.
    Observed,
    /// Too few observations to claim any pattern.
    Insufficient,
}

impl HistoryConfidence {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Insufficient => "insufficient",
        }
    }
}

impl WorkspaceEvidenceGraph {
    /// Builds the graph from classified candidates and a branch survey.
    ///
    /// Pure. Every candidate lands in exactly one of the three buckets, so
    /// the graph accounts for the whole input — a property
    /// `every_candidate_lands_in_exactly_one_bucket` asserts, because a
    /// resource silently absent from a picture of the machine is the same
    /// failure as one misfiled into it.
    pub fn build(
        candidates: &[(Evidence, PolicyDecision)],
        survey: &super::group::WorkspaceSurvey,
        machine: MachineContext,
        built_at: SystemTime,
    ) -> Self {
        let mut repositories: BTreeMap<PathBuf, BTreeMap<PathBuf, WorktreeAccumulator>> =
            BTreeMap::new();
        let mut global_resources = Vec::new();
        let mut unplaced_resources = Vec::new();

        for (ev, decision) in candidates {
            let node = ResourceNode::from_evidence(ev, decision);

            // The locator is consulted before the probe outcome (HORO-1561). A
            // tool-owned resource — a Docker volume, the build cache — has no
            // containing repository *by construction*, and
            // `DefaultEvidenceCollector` therefore reports its `git_state` as
            // `Unavailable(NotAttempted)`: there is no path to run the probe
            // against. Reading `git_state` first filed every one of them under
            // "containing repository could not be determined", which is the
            // campaign's own rule run backwards — not-applicable reported as
            // not-attempted, telling the model a probe was missing when nothing
            // ever was.
            //
            // Placement carries no authority in either direction: group
            // membership grants nothing (HORO-1511) and `policy::classify` never
            // reads this graph. What moves is only the honesty of the picture.
            if matches!(
                ev.resource.locator,
                crate::evidence::ResourceLocator::Tool { .. }
            ) {
                global_resources.push(GlobalResourceNode {
                    owning_tool: node.owning_tool,
                    resource: node,
                });
                continue;
            }

            match &ev.git_state {
                ProbeOutcome::Observed(Some(git)) => {
                    let accumulator = repositories
                        .entry(git.common_dir.clone())
                        .or_default()
                        .entry(git.repo_root.clone())
                        .or_insert_with(|| {
                            WorktreeAccumulator::new(git.worktree, git.dirty, git.untracked)
                        });
                    accumulator.absorb(ev, node);
                }
                ProbeOutcome::Observed(None) => global_resources.push(GlobalResourceNode {
                    owning_tool: node.owning_tool,
                    resource: node,
                }),
                ProbeOutcome::Unavailable(reason) => {
                    unplaced_resources.push(UnplacedResourceNode {
                        resource: node,
                        reason: *reason,
                    })
                }
            }
        }

        global_resources.sort_by_key(|node| node.resource.resource.to_string());
        unplaced_resources.sort_by_key(|node| node.resource.resource.to_string());

        Self {
            machine,
            repositories: repositories
                .into_iter()
                .map(|(common_dir, worktrees)| RepositoryNode {
                    common_dir,
                    worktrees: worktrees
                        .into_iter()
                        .map(|(root, accumulator)| accumulator.finish(root, survey))
                        .collect(),
                })
                .collect(),
            global_resources,
            unplaced_resources,
            history: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            built_at,
        }
    }

    /// Attaches the local workflow baseline (HORO-1547).
    ///
    /// Separate from [`Self::build`] so that stays pure: the baseline lives in a
    /// file, and a `build` that read one could not be called from a test without
    /// a `$HOME`. The caller passes the state it already read and the clock it
    /// already has.
    ///
    /// # All three store states go through here
    ///
    /// Never collected, could not be read, and collected-but-thin all arrive as
    /// [`ProbeOutcome::Unavailable`] or an `Observed` summary whose mode is
    /// `Unknown` — never as a default shape. That is
    /// [`super::history::classify`]'s job and it is asserted there; this method
    /// exists so nothing downstream has to re-derive it, because the one
    /// dangerous implementation is the convenient one: `unwrap_or(Serial…)`.
    ///
    /// The result carries no authority. `history` hangs off the machine node and
    /// is unreachable from a [`WorktreeNode`], holds only counts and a mode so
    /// it cannot be matched against a resource, and is never read by
    /// `policy::classify` — see `tests/workflow_history_has_no_authority.rs`.
    pub fn with_history(mut self, state: &super::history::StoreState, now_unix_secs: u64) -> Self {
        self.history = super::history::classify(state, now_unix_secs);
        self
    }

    /// Every resource in the graph, in bucket order. Used by the tests that
    /// prove the buckets partition the input, and by the model projection,
    /// which needs one stable traversal to assign aliases from.
    pub fn resources(&self) -> impl Iterator<Item = &ResourceNode> {
        self.repositories
            .iter()
            .flat_map(|repo| repo.resources())
            .chain(self.global_resources.iter().map(|node| &node.resource))
            .chain(self.unplaced_resources.iter().map(|node| &node.resource))
    }

    pub fn resource_count(&self) -> usize {
        self.resources().count()
    }

    /// Fills in each working tree's [`ExternalContext`] from whichever providers
    /// are configured, and reports what happened for each attempt.
    ///
    /// Separate from [`Self::build`] on purpose. `build` is pure — no I/O, no
    /// clock, no network — and every caller relies on being able to assemble a
    /// picture of the machine offline. This is the one method here that can reach
    /// a service, so it is the one method a reader has to look at to know whether
    /// a command talks to anything, and it does nothing at all when no provider
    /// exists: not a request, not a `git` process, not a traversal.
    ///
    /// What it changes is one field per working tree. It does not touch
    /// [`BranchLifecycle`], [`ActivityFacts`], any [`ResourceNode`]'s policy
    /// decision, or anything a [`crate::actions::Action`] can read — a merged
    /// pull request is context for a person and for a model, never authority.
    pub fn attach_external_context(
        &mut self,
        resolver: &super::external::ExternalContextResolver<'_>,
        now: SystemTime,
    ) -> Vec<ExternalContextAttempt> {
        if !resolver.is_enabled() {
            return Vec::new();
        }

        let mut attempts = Vec::new();
        for repository in &mut self.repositories {
            for worktree in &mut repository.worktrees {
                // The branch name comes from the local probe that already ran,
                // never from a second reading and never from the directory name.
                // Its three states are carried across intact; see
                // `resolve_branch_outcome`.
                let branch = match &worktree.lifecycle.branch {
                    ProbeOutcome::Observed(state) => {
                        ProbeOutcome::Observed(state.branch.as_deref())
                    }
                    ProbeOutcome::Unavailable(reason) => ProbeOutcome::Unavailable(*reason),
                };
                let resolved = resolver.resolve_branch_outcome(&worktree.root, branch, now);
                attempts.push(ExternalContextAttempt {
                    worktree: worktree.root.clone(),
                    pull_request_detail: resolved.pull_request_detail,
                    task_detail: resolved.task_detail,
                });
                worktree.external = resolved.context;
            }
        }
        attempts
    }
}

/// What one working tree's external lookup did, for a local report.
///
/// The [`super::external::ExternalDetail`]s stay here rather than on
/// [`WorktreeNode`] because they are diagnostics for whoever is setting the
/// feature up — "the token was refused", "this remote is on another host" — and
/// the graph is what a model projection is built from. A field on the node would
/// be a field somebody could serialise by accident.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalContextAttempt {
    pub worktree: PathBuf,
    pub pull_request_detail: super::external::ExternalDetail,
    pub task_detail: super::external::ExternalDetail,
}

/// Mutable state while one working tree's resources are collected.
struct WorktreeAccumulator {
    linked: bool,
    dirty: bool,
    untracked: bool,
    newest_resource_modified_at: Option<SystemTime>,
    resources: Vec<ResourceNode>,
    process_probes: Vec<ProcessProbePair>,
}

impl WorktreeAccumulator {
    fn new(linked: bool, dirty: bool, untracked: bool) -> Self {
        Self {
            linked,
            dirty,
            untracked,
            newest_resource_modified_at: None,
            resources: Vec::new(),
            process_probes: Vec::new(),
        }
    }

    fn absorb(&mut self, ev: &Evidence, node: ResourceNode) {
        if let ProbeOutcome::Observed(modified) = &ev.last_modified {
            self.newest_resource_modified_at = match self.newest_resource_modified_at {
                Some(newest) if newest >= *modified => Some(newest),
                _ => Some(*modified),
            };
        }
        self.process_probes
            .push((ev.open_by_process.clone(), ev.process_cwd_match.clone()));
        self.resources.push(node);
    }

    fn finish(mut self, root: PathBuf, survey: &super::group::WorkspaceSurvey) -> WorktreeNode {
        self.resources.sort_by_key(|node| node.resource.to_string());
        WorktreeNode {
            lifecycle: BranchLifecycle {
                dirty: self.dirty,
                untracked: self.untracked,
                branch: survey.state_of(&root),
            },
            activity: ActivityFacts::from_probes(self.process_probes.iter()),
            external: ExternalContext::unqueried(),
            resources: self.resources,
            newest_resource_modified_at: self.newest_resource_modified_at,
            linked: self.linked,
            root,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        GitState, NativeCleanup, Recoverability, ResourceFingerprint, ResourceKind, ResourceLocator,
    };
    use crate::policy::{PolicyClass, ReasonCode};
    use crate::workspace::branch::{
        Divergence, EquivalenceMethod, IntegrationEvidence, PatchEquivalence,
    };
    use crate::workspace::branch::{MergedState, UpstreamState};
    use crate::workspace::group::WorkspaceSurvey;
    use std::time::Duration;

    const T0: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    fn git(root: &str, common_dir: &str, dirty: bool, untracked: bool) -> GitState {
        GitState {
            repo_root: PathBuf::from(root),
            common_dir: PathBuf::from(common_dir),
            dirty,
            untracked,
            worktree: true,
        }
    }

    /// A resource whose every probe answered, sitting in a clean worktree.
    /// Each test mutates only the field it is about, so a failure names one
    /// cause rather than "the fixture".
    fn evidence(path: &str, git_state: ProbeOutcome<Option<GitState>>) -> Evidence {
        Evidence {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from(path)),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(1024),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(T0),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: Regenerability::RegenerableByRebuild,
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state,
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            collected_at: at(86_400 * 30),
            sources: Vec::new(),
        }
    }

    fn decision(path: &str, class: PolicyClass, reasons: Vec<ReasonCode>) -> PolicyDecision {
        PolicyDecision {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from(path)),
            ),
            class,
            reasons,
            evidence_collected_at: T0,
            evaluated_at: T0,
            policy_version: 1,
        }
    }

    fn candidate(
        path: &str,
        git_state: ProbeOutcome<Option<GitState>>,
    ) -> (Evidence, PolicyDecision) {
        (
            evidence(path, git_state),
            decision(
                path,
                PolicyClass::AutoSafe,
                vec![ReasonCode::NoActiveUseObserved],
            ),
        )
    }

    /// A resource Docker addresses by its own id, as a real detector produces
    /// it: no path, and therefore `git_state: Unavailable(NotAttempted)` —
    /// `DefaultEvidenceCollector` has nothing to run the probe against.
    fn tool_candidate(kind: ResourceKind, id: &str) -> (Evidence, PolicyDecision) {
        let resource = ResourceId::new(
            kind,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: id.to_string(),
            },
        );
        let mut ev = evidence(
            "/unused",
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        );
        ev.resource = resource.clone();
        let mut d = decision(
            "/unused",
            PolicyClass::Ask,
            vec![ReasonCode::RebuildCostHigh],
        );
        d.resource = resource;
        (ev, d)
    }

    fn surveyed(root: &str, state: WorktreeBranchState) -> WorkspaceSurvey {
        struct Fixed(WorktreeBranchState);
        impl super::super::branch::BranchProbe for Fixed {
            fn state_of(
                &self,
                _root: &std::path::Path,
                _timeout: Duration,
            ) -> ProbeOutcome<WorktreeBranchState> {
                ProbeOutcome::Observed(self.0.clone())
            }
        }
        WorkspaceSurvey::survey([PathBuf::from(root)], &Fixed(state), Duration::from_secs(1))
    }

    fn build(
        candidates: &[(Evidence, PolicyDecision)],
        survey: &WorkspaceSurvey,
    ) -> WorkspaceEvidenceGraph {
        WorkspaceEvidenceGraph::build(
            candidates,
            survey,
            MachineContext::unmeasured(),
            at(86_400 * 30),
        )
    }

    // ---------------------------------------------------------------
    // AC 9 fixture 1 — known evidence
    // ---------------------------------------------------------------

    /// Everything answered: the resource lands under its repository, under
    /// its worktree, with the branch state the survey read and an age
    /// derived from the resource's own two timestamps.
    #[test]
    fn fully_known_evidence_builds_a_complete_repository_node() {
        let state = WorktreeBranchState {
            branch: Some("feature".to_string()),
            upstream: UpstreamState::Tracking {
                ahead: 0,
                behind: 0,
            },
            merged: MergedState::Merged {
                into: "origin/main".to_string(),
            },
            integration: IntegrationEvidence::not_attempted(),
        };
        let graph = build(
            &[candidate(
                "/w/a/target",
                ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", false, false))),
            )],
            &surveyed("/w/a", state.clone()),
        );

        assert_eq!(graph.repositories.len(), 1);
        assert!(graph.global_resources.is_empty());
        assert!(graph.unplaced_resources.is_empty());

        let repo = &graph.repositories[0];
        assert_eq!(repo.common_dir, PathBuf::from("/w/.git"));
        assert_eq!(repo.worktree_count(), 1);

        let worktree = &repo.worktrees[0];
        assert_eq!(worktree.root, PathBuf::from("/w/a"));
        assert!(worktree.linked);
        assert_eq!(worktree.lifecycle.branch, ProbeOutcome::Observed(state));
        assert_eq!(worktree.activity.state, ActivityState::Idle);
        assert_eq!(worktree.resources.len(), 1);
        assert_eq!(worktree.newest_resource_modified_at, Some(T0));

        assert_eq!(
            worktree.resources[0].age_days(),
            ProbeOutcome::Observed(30),
            "age comes from collected_at minus last_modified, never an \
             ambient clock"
        );
        assert_eq!(worktree.resources[0].label, PolicyLabel::AutoSafe);
        assert_eq!(worktree.resources[0].owning_tool, OwningTool::Cargo);
    }

    // ---------------------------------------------------------------
    // AC 9 fixture 2 — unknown evidence
    // ---------------------------------------------------------------

    /// Nothing answered. Every unknown stays an unknown of its own kind:
    /// the branch probe's `NotAttempted` survives, the process fold reports
    /// `Unknown` rather than idle, and an unreadable mtime produces no age
    /// and no newest-modified timestamp.
    #[test]
    fn wholly_unknown_evidence_never_collapses_into_a_negative_answer() {
        let mut c = candidate(
            "/w/a/target",
            ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", false, false))),
        );
        c.0.last_modified = ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);
        c.0.open_by_process = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);
        c.0.process_cwd_match = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);
        c.0.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);

        let graph = build(&[c], &WorkspaceSurvey::unsurveyed());
        let worktree = &graph.repositories[0].worktrees[0];

        assert_eq!(
            worktree.lifecycle.branch,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(worktree.lifecycle.unique_work(), UniqueWork::Unknown);
        assert_eq!(worktree.activity.state, ActivityState::Unknown);
        assert_eq!(worktree.activity.unanswered_probes, 2);
        assert!(worktree.activity.observed_processes.is_empty());
        assert_eq!(worktree.newest_resource_modified_at, None);

        let resource = &worktree.resources[0];
        assert_eq!(
            resource.age_days(),
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied),
            "the reason the mtime could not be read is carried through, not \
             flattened to a generic failure"
        );
        assert_eq!(
            resource.reclaimable_bytes,
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied)
        );
        assert!(
            !resource.reclaimable_bytes_is_lower_bound,
            "an unmeasured size is not a lower bound of anything"
        );
    }

    // ---------------------------------------------------------------
    // AC 9 fixture 3 — failed evidence
    // ---------------------------------------------------------------

    /// The whole reason this module exists rather than reusing
    /// `group_families`. A failed git probe is a third answer: the resource
    /// is neither filed under a repository nor reported as a global cache,
    /// and the reason the probe failed is preserved.
    #[test]
    fn a_failed_git_probe_is_unplaced_and_never_a_global_cache() {
        let graph = build(
            &[
                candidate(
                    "/w/a/target",
                    ProbeOutcome::Unavailable(ProbeReason::TimedOut),
                ),
                candidate("/cache/registry", ProbeOutcome::Observed(None)),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert!(
            graph.repositories.is_empty(),
            "nothing may be filed under a repository the probe never named"
        );
        assert_eq!(graph.unplaced_resources.len(), 1);
        assert_eq!(graph.unplaced_resources[0].reason, ProbeReason::TimedOut);
        assert!(graph.unplaced_resources[0]
            .resource
            .resource
            .to_string()
            .contains("target"));

        assert_eq!(
            graph.global_resources.len(),
            1,
            "and the resource whose probe DID answer 'not in a git tree' is \
             still reported as the global cache it is — the bucket is not a \
             dumping ground for everything non-repository"
        );
        assert_eq!(graph.global_resources[0].owning_tool, OwningTool::Cargo);
    }

    /// Positive control for the test above: the same two resources differ
    /// only in their git probe outcome, so if `build` ever treated
    /// `Unavailable` and `Observed(None)` alike, both would land in one
    /// bucket and this assertion would catch it from the other side.
    #[test]
    fn observed_none_and_unavailable_are_not_the_same_bucket() {
        let graph = build(
            &[
                candidate("/x/one", ProbeOutcome::Observed(None)),
                candidate(
                    "/x/two",
                    ProbeOutcome::Unavailable(ProbeReason::PermissionDenied),
                ),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );
        assert_eq!(graph.global_resources.len(), 1);
        assert_eq!(graph.unplaced_resources.len(), 1);
    }

    /// The lifecycle facts reach the graph intact, and — the half that
    /// matters — a resource Docker does not own is `None` rather than an
    /// all-unknown lifecycle. Collapsing the two would tell a reader that a
    /// Cargo target directory has Docker activity nobody established, which
    /// is a gap invented out of an inapplicable question.
    #[test]
    fn docker_lifecycle_reaches_the_graph_and_absence_is_not_unknown() {
        let (mut ev, decision) = tool_candidate(ResourceKind::DockerImage, "sha256:abc");
        ev.docker_lifecycle = Some(DockerLifecycle {
            activity: crate::evidence::DockerActivity::Active,
            persistence: crate::evidence::DockerPersistence::ToolManaged,
            references: ProbeOutcome::Observed(crate::evidence::DockerReferences {
                referenced_by: vec![ResourceId::new(
                    ResourceKind::DockerContainer,
                    ResourceLocator::Tool {
                        tool: OwningTool::Docker,
                        id: "c1".to_string(),
                    },
                )],
                active_referrers: 1,
            }),
        });

        let graph = build(
            &[
                (ev, decision),
                candidate("/w/a/target", ProbeOutcome::Observed(None)),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        let by_id = |id: &str| {
            graph
                .global_resources
                .iter()
                .find(|n| n.resource.resource.to_string() == id)
                .map(|n| n.resource.docker_lifecycle.clone())
                .expect("candidate should be a global resource")
        };

        let image = by_id("docker_image:docker:sha256:abc").expect("Docker object carries one");
        assert_eq!(image.activity, crate::evidence::DockerActivity::Active);
        assert_eq!(
            image.persistence,
            crate::evidence::DockerPersistence::ToolManaged
        );
        match &image.references {
            ProbeOutcome::Observed(refs) => {
                assert_eq!(refs.referenced_by.len(), 1);
                assert_eq!(refs.active_referrers, 1);
            }
            other => panic!("references should survive the join: {other:?}"),
        }

        assert!(
            by_id("cargo_target_dir:/w/a/target").is_none(),
            "a non-Docker resource has no Docker lifecycle to be unknown about"
        );
    }

    // ---------------------------------------------------------------
    // HORO-1561 — a tool-owned resource has no repository by construction
    // ---------------------------------------------------------------

    /// AC 1. Every Docker object arrives with `git_state:
    /// Unavailable(NotAttempted)` because there is no path to probe. Reading
    /// that outcome first filed all of them under "containing repository could
    /// not be determined", which reports not-applicable as not-attempted — the
    /// campaign's own rule inverted.
    #[test]
    fn a_tool_owned_resource_is_a_global_cache_on_the_strength_of_its_locator() {
        let graph = build(
            &[
                tool_candidate(ResourceKind::DockerVolume, "pgdata"),
                tool_candidate(ResourceKind::DockerImage, "sha256:abc"),
                tool_candidate(ResourceKind::DockerBuildCache, "build_cache"),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert_eq!(graph.global_resources.len(), 3);
        assert!(
            graph.unplaced_resources.is_empty(),
            "nothing was missing, so nothing is unplaced: {:?}",
            graph
                .unplaced_resources
                .iter()
                .map(|n| n.resource.resource.to_string())
                .collect::<Vec<_>>()
        );
        assert!(graph.repositories.is_empty());
    }

    /// AC 4, the mutation control. These two candidates are identical in the
    /// only field the old rule read — both carry
    /// `Unavailable(NotAttempted)` — and differ only in their locator. A
    /// revert to bucketing on `git_state` first puts both in
    /// `unplaced_resources` and fails here, naming the Docker volume as the
    /// resource that moved.
    #[test]
    fn a_locator_alone_separates_a_tool_owned_cache_from_an_unprobed_path() {
        let graph = build(
            &[
                tool_candidate(ResourceKind::DockerVolume, "pgdata"),
                candidate(
                    "/w/a/target",
                    ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
                ),
            ],
            &WorkspaceSurvey::unsurveyed(),
        );

        assert_eq!(
            graph
                .global_resources
                .iter()
                .map(|n| n.resource.resource.to_string())
                .collect::<Vec<_>>(),
            vec!["docker_volume:docker:pgdata"],
            "the tool-owned resource belongs here; if this list is empty the \
             bucketing is reading git_state before the locator again"
        );
        assert_eq!(
            graph
                .unplaced_resources
                .iter()
                .map(|n| n.resource.resource.to_string())
                .collect::<Vec<_>>(),
            vec!["cargo_target_dir:/w/a/target"],
            "and the path-located resource whose probe did not run stays here"
        );
    }

    /// AC 2, from the other side and with the reasons that matter most. A path
    /// exists for each of these, so "no containing repository" was never
    /// established — and the real reason travels with the resource so a reader
    /// can weigh a denied permission differently from a timeout.
    #[test]
    fn a_path_whose_git_probe_failed_is_still_unplaced_with_its_reason() {
        for reason in [
            ProbeReason::TimedOut,
            ProbeReason::PermissionDenied,
            ProbeReason::Failed,
            ProbeReason::ToolAbsent,
            ProbeReason::NotAttempted,
        ] {
            let graph = build(
                &[candidate("/w/a/target", ProbeOutcome::Unavailable(reason))],
                &WorkspaceSurvey::unsurveyed(),
            );

            assert!(
                graph.global_resources.is_empty(),
                "a path-located resource must not become a global cache on a \
                 {reason:?} probe"
            );
            assert_eq!(graph.unplaced_resources.len(), 1, "{reason:?}");
            assert_eq!(graph.unplaced_resources[0].reason, reason);
        }
    }

    // ---------------------------------------------------------------
    // AC 9 fixture 4 — contradictory evidence
    // ---------------------------------------------------------------

    /// A worktree whose every remote-shaped signal says "finished" and whose
    /// local filesystem says "in progress". The graph must keep both,
    /// unreconciled, and the one local judgement it does make —
    /// `unique_work` — must follow the filesystem.
    #[test]
    fn contradictory_evidence_is_preserved_and_the_local_fact_wins() {
        let mut c = candidate(
            "/w/a/target",
            ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", true, true))),
        );
        c.0.open_by_process = ProbeOutcome::Observed(vec![ProcessRef {
            pid: 4242,
            command: "cargo".to_string(),
        }]);

        let merged_and_pushed = WorktreeBranchState {
            branch: Some("done".to_string()),
            upstream: UpstreamState::Tracking {
                ahead: 0,
                behind: 0,
            },
            merged: MergedState::Merged {
                into: "origin/main".to_string(),
            },
            integration: IntegrationEvidence::not_attempted(),
        };
        let mut graph = build(&[c], &surveyed("/w/a", merged_and_pushed));

        // Every "finished" signal an optional provider could add, at once.
        let worktree = &mut graph.repositories[0].worktrees[0];
        worktree.external = ExternalContext {
            pull_request: ExternalFact::observed(
                ExternalSource::GitHubPullRequests,
                PullRequestState::Merged,
                at(86_400 * 29),
            ),
            task: ExternalFact::observed(
                ExternalSource::JiraIssues,
                TaskState::Done,
                at(86_400 * 29),
            ),
        };
        // And a historical pattern that would make throwing it away routine.
        graph.history = ProbeOutcome::Observed(WorkflowHistorySummary::from_observations(
            WorkflowMode::ParallelMultiWorktree,
            40,
            WorkflowSupport::nothing_observed(),
        ));

        let worktree = &graph.repositories[0].worktrees[0];
        assert_eq!(
            worktree.lifecycle.unique_work(),
            UniqueWork::Present,
            "a dirty, untracked working tree holds unique work no matter \
             what the branch, the PR, the task or the habit say"
        );
        assert_eq!(worktree.activity.state, ActivityState::InUse);
        assert_eq!(worktree.activity.observed_processes[0].pid, 4242);

        // Each contradicting signal is still individually readable — the
        // graph reconciles nothing, it only refuses to lose anything.
        assert!(matches!(
            worktree.external.pull_request.outcome,
            ProbeOutcome::Observed(PullRequestState::Merged)
        ));
        assert!(matches!(
            worktree.external.task.outcome,
            ProbeOutcome::Observed(TaskState::Done)
        ));
        assert!(matches!(
            worktree.lifecycle.branch,
            ProbeOutcome::Observed(WorktreeBranchState {
                merged: MergedState::Merged { .. },
                ..
            })
        ));
        assert!(matches!(
            graph.history,
            ProbeOutcome::Observed(WorkflowHistorySummary {
                mode: WorkflowMode::ParallelMultiWorktree,
                ..
            })
        ));
    }

    // ---------------------------------------------------------------
    // Structural invariants
    // ---------------------------------------------------------------

    /// The graph accounts for its whole input. A resource silently missing
    /// from a picture of the machine misleads exactly as much as one filed
    /// in the wrong place, and only a count over all three buckets catches
    /// the first kind.
    #[test]
    fn every_candidate_lands_in_exactly_one_bucket() {
        let candidates = vec![
            candidate(
                "/w/a/target",
                ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", false, false))),
            ),
            candidate(
                "/w/b/target",
                ProbeOutcome::Observed(Some(git("/w/b", "/w/.git", false, false))),
            ),
            candidate(
                "/other/target",
                ProbeOutcome::Observed(Some(git("/other", "/other/.git", false, false))),
            ),
            candidate("/cache/one", ProbeOutcome::Observed(None)),
            candidate("/cache/two", ProbeOutcome::Observed(None)),
            candidate("/lost/one", ProbeOutcome::Unavailable(ProbeReason::Failed)),
            // The locator-decided path, in the same partition check (HORO-1561
            // AC 3): the early `continue` that places it must not skip the
            // accounting either.
            tool_candidate(ResourceKind::DockerVolume, "pgdata"),
        ];
        let graph = build(&candidates, &WorkspaceSurvey::unsurveyed());

        assert_eq!(graph.resource_count(), candidates.len());
        assert_eq!(graph.repositories.len(), 2, "grouped by common_dir");
        assert_eq!(graph.repositories[0].worktree_count(), 1);
        assert_eq!(graph.repositories[1].worktree_count(), 2);
        assert_eq!(graph.global_resources.len(), 3);
        assert_eq!(graph.unplaced_resources.len(), 1);

        let ids: Vec<String> = graph
            .resources()
            .map(|node| node.resource.to_string())
            .collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            ids.len(),
            "a resource appearing twice would double-count bytes in any \
             aggregate built from this traversal"
        );
    }

    /// Two builds over one input produce one graph. The model projection
    /// assigns aliases positionally from [`WorkspaceEvidenceGraph::resources`],
    /// so an unstable order would silently re-point aliases between runs.
    #[test]
    fn build_is_deterministic_regardless_of_candidate_order() {
        let a = candidate(
            "/w/b/target",
            ProbeOutcome::Observed(Some(git("/w/b", "/w/.git", false, false))),
        );
        let b = candidate(
            "/w/a/target",
            ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", false, false))),
        );
        let c = candidate("/cache/z", ProbeOutcome::Observed(None));
        let d = candidate("/cache/a", ProbeOutcome::Observed(None));

        let forward = build(
            &[a.clone(), b.clone(), c.clone(), d.clone()],
            &WorkspaceSurvey::unsurveyed(),
        );
        let reversed = build(&[d, c, b, a], &WorkspaceSurvey::unsurveyed());
        assert_eq!(forward, reversed);
    }

    /// `unanswered_probes > 0` and `Idle` must never coexist. Driven over
    /// every combination of one answered/unanswered pair rather than the
    /// one interesting case, so the fold cannot regress into "no processes
    /// found means nothing is running".
    #[test]
    fn a_tree_with_an_unanswered_probe_is_never_idle() {
        let empty = ProbeOutcome::Observed(Vec::new());
        let failed: ProbeOutcome<Vec<ProcessRef>> =
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);

        for pair in [
            (empty.clone(), failed.clone()),
            (failed.clone(), empty.clone()),
            (failed.clone(), failed.clone()),
        ] {
            let facts = ActivityFacts::from_probes([&pair]);
            assert_ne!(facts.state, ActivityState::Idle, "{pair:?}");
            assert!(facts.state.may_be_in_use());
        }

        // Positive control: both probes answering nothing IS idle, so the
        // assertion above is discriminating rather than vacuously true.
        let both_answered = ActivityFacts::from_probes([&(empty.clone(), empty.clone())]);
        assert_eq!(both_answered.state, ActivityState::Idle);
        assert_eq!(both_answered.unanswered_probes, 0);
    }

    /// An observed process outranks an unanswered sibling probe: a tree
    /// something is demonstrably working in is `InUse`, not `Unknown`.
    #[test]
    fn an_observed_process_outranks_an_unanswered_probe() {
        let seen = ProbeOutcome::Observed(vec![ProcessRef {
            pid: 7,
            command: "rustc".to_string(),
        }]);
        let failed = ProbeOutcome::Unavailable(ProbeReason::TimedOut);
        let facts = ActivityFacts::from_probes([&(seen, failed)]);
        assert_eq!(facts.state, ActivityState::InUse);
        assert_eq!(facts.unanswered_probes, 1);
        assert_eq!(facts.observed_processes.len(), 1);
    }

    /// One process holding two resources in one tree open is one process.
    /// Without the dedup an "8 processes are using this" line would be
    /// counting directories.
    #[test]
    fn processes_are_deduplicated_by_pid_across_resources() {
        let same = ProbeOutcome::Observed(vec![ProcessRef {
            pid: 99,
            command: "cargo".to_string(),
        }]);
        let empty = ProbeOutcome::Observed(Vec::new());
        let pair = (same, empty);
        let facts = ActivityFacts::from_probes([&pair, &pair]);
        assert_eq!(facts.observed_processes.len(), 1);
        assert_eq!(facts.observed_processes[0].pid, 99);
    }

    /// `merged` is deliberately not an input to `unique_work`. A branch
    /// contained in `origin/main` whose tree is clean and fully pushed has
    /// no unique work *because of the upstream count*, and this asserts
    /// containment alone never supplies that answer.
    #[test]
    fn unique_work_ignores_merged_state_and_reads_the_upstream() {
        let merged_but_ahead = BranchLifecycle {
            dirty: false,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("x".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 3,
                    behind: 0,
                },
                merged: MergedState::Merged {
                    into: "origin/main".to_string(),
                },
                integration: IntegrationEvidence::not_attempted(),
            }),
        };
        assert_eq!(
            merged_but_ahead.unique_work(),
            UniqueWork::Present,
            "three local commits are three local commits, contained or not"
        );

        let no_upstream = BranchLifecycle {
            dirty: false,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("x".to_string()),
                upstream: UpstreamState::Untracked,
                merged: MergedState::Merged {
                    into: "origin/main".to_string(),
                },
                integration: IntegrationEvidence::not_attempted(),
            }),
        };
        assert_eq!(
            no_upstream.unique_work(),
            UniqueWork::Present,
            "no upstream is not 'nothing unpushed'"
        );

        let unknown_upstream = BranchLifecycle {
            dirty: false,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: None,
                upstream: UpstreamState::Unknown,
                merged: MergedState::Unknown,
                integration: IntegrationEvidence::not_attempted(),
            }),
        };
        assert_eq!(unknown_upstream.unique_work(), UniqueWork::Present);

        // Positive control: the only shape that yields `Absent`.
        let settled = BranchLifecycle {
            dirty: false,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("x".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 0,
                    behind: 5,
                },
                merged: MergedState::NotMerged {
                    into: "origin/main".to_string(),
                },
                integration: IntegrationEvidence::not_attempted(),
            }),
        };
        assert_eq!(settled.unique_work(), UniqueWork::Absent);
    }

    /// AC 2 of HORO-1545, as a control on the evidence that ticket added.
    ///
    /// `IntegrationEvidence` exists to say "this work is already over there
    /// in some form", which is a more persuasive sentence than `merged`
    /// manages, and it is *still* not an answer to whether this working
    /// tree holds anything of its own. A squash that landed last week
    /// covers the commits it was made from and nothing written since.
    ///
    /// Each case below pairs the strongest integration answer the probe can
    /// produce with a reason the tree is not settled, and asserts the
    /// integration answer loses. The pairing matters: an assertion that
    /// `Equivalent` yields `Present` proves nothing on a tree that is dirty
    /// for unrelated reasons unless the same shape without the integration
    /// evidence yields `Present` too — which
    /// `unique_work_ignores_merged_state_and_reads_the_upstream` above
    /// establishes.
    #[test]
    fn unique_work_ignores_patch_equivalence() {
        let integrated = |unique: u32| IntegrationEvidence {
            divergence: ProbeOutcome::Observed(Divergence {
                unique_commits: unique,
                equivalent_commits: 0,
                unclassified_commits: 0,
            }),
            equivalence: PatchEquivalence::Equivalent(EquivalenceMethod::ContentIdentical),
            tip_committed_at: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            comparison_tip_committed_at: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
        };

        let equivalent_but_ahead = BranchLifecycle {
            dirty: false,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("x".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 2,
                    behind: 0,
                },
                merged: MergedState::NotMerged {
                    into: "origin/main".to_string(),
                },
                integration: integrated(2),
            }),
        };
        assert_eq!(
            equivalent_but_ahead.unique_work(),
            UniqueWork::Present,
            "a squash covers the commits it was made from, not the ones written after it"
        );

        let equivalent_but_dirty = BranchLifecycle {
            dirty: true,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("x".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 0,
                    behind: 0,
                },
                merged: MergedState::Merged {
                    into: "origin/main".to_string(),
                },
                integration: integrated(0),
            }),
        };
        assert_eq!(
            equivalent_but_dirty.unique_work(),
            UniqueWork::Present,
            "every commit landed and the file on disk still differs from all of them"
        );

        let equivalent_but_unpublished = BranchLifecycle {
            dirty: false,
            untracked: false,
            branch: ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("x".to_string()),
                upstream: UpstreamState::Untracked,
                merged: MergedState::Merged {
                    into: "origin/main".to_string(),
                },
                integration: integrated(0),
            }),
        };
        assert_eq!(
            equivalent_but_unpublished.unique_work(),
            UniqueWork::Present,
            "equivalence is measured against one branch this machine happens to have"
        );
    }

    /// A dirty tree is `Present` even when the branch probe failed, so the
    /// direct observation is not lost behind the unknown.
    #[test]
    fn a_dirty_tree_holds_unique_work_even_with_no_branch_state() {
        let lifecycle = BranchLifecycle::unsurveyed(true, false);
        assert_eq!(lifecycle.unique_work(), UniqueWork::Present);
        let untracked_only = BranchLifecycle::unsurveyed(false, true);
        assert_eq!(untracked_only.unique_work(), UniqueWork::Present);
        let neither = BranchLifecycle::unsurveyed(false, false);
        assert_eq!(neither.unique_work(), UniqueWork::Unknown);
    }

    /// An mtime in the future has no age. Reporting zero would present a
    /// resource written to moments ago as both brand new and confidently
    /// measured — the flattering reading of a subtraction with no answer.
    #[test]
    fn an_mtime_after_collection_yields_no_age_rather_than_zero() {
        let mut c = candidate(
            "/w/a/target",
            ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", false, false))),
        );
        c.0.last_modified = ProbeOutcome::Observed(at(86_400 * 31));
        let graph = build(&[c], &WorkspaceSurvey::unsurveyed());
        assert_eq!(
            graph.repositories[0].worktrees[0].resources[0].age_days(),
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }

    /// One observation is not a habit. Asserted across every mode so a new
    /// variant cannot arrive with a bypass.
    #[test]
    fn too_few_observations_can_never_produce_a_pattern() {
        let modes = [
            WorkflowMode::SerialSingleCheckout,
            WorkflowMode::SerialMultiBranch,
            WorkflowMode::ParallelMultiWorktree,
            WorkflowMode::Mixed,
            WorkflowMode::Unknown,
        ];
        for mode in modes {
            for count in 0..MIN_OBSERVATIONS_FOR_A_PATTERN {
                let summary = WorkflowHistorySummary::from_observations(
                    mode,
                    count,
                    WorkflowSupport::nothing_observed(),
                );
                assert_eq!(summary.mode, WorkflowMode::Unknown, "{mode:?} at {count}");
                assert_eq!(summary.confidence, HistoryConfidence::Insufficient);
                assert_eq!(summary.observation_count, count);
            }
            // Positive control at the threshold.
            let summary = WorkflowHistorySummary::from_observations(
                mode,
                MIN_OBSERVATIONS_FOR_A_PATTERN,
                WorkflowSupport::nothing_observed(),
            );
            assert_eq!(summary.mode, mode);
            assert_eq!(summary.confidence, HistoryConfidence::Observed);
        }
    }

    /// A graph nobody collected history for says so, rather than defaulting
    /// to a mode.
    #[test]
    fn a_fresh_graph_has_not_attempted_history_and_an_unmeasured_machine() {
        let graph = build(&[], &WorkspaceSurvey::unsurveyed());
        assert_eq!(
            graph.history,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            graph.machine.free_bytes,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            graph.machine.total_bytes,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            graph.machine.recovery_goal_bytes, None,
            "no goal set is a complete answer, not an unmeasured one"
        );
    }

    /// Only an observation carries a timestamp. An `Unavailable` fact with
    /// an `observed_at` would describe when the *attempt* happened while
    /// reading as when the answer was true.
    #[test]
    fn only_an_observed_external_fact_has_a_timestamp() {
        let observed = ExternalFact::observed(
            ExternalSource::GitHubPullRequests,
            PullRequestState::Open,
            at(100),
        );
        assert_eq!(observed.observed_at, Some(at(100)));
        assert_eq!(observed.age(at(400)), Some(Duration::from_secs(300)));

        let unavailable: ExternalFact<PullRequestState> =
            ExternalFact::unavailable(ExternalSource::GitHubPullRequests, ProbeReason::ToolAbsent);
        assert_eq!(unavailable.observed_at, None);
        assert_eq!(unavailable.age(at(400)), None);

        let not_attempted: ExternalFact<TaskState> =
            ExternalFact::not_attempted(ExternalSource::JiraIssues);
        assert_eq!(not_attempted.observed_at, None);
        assert_eq!(
            not_attempted.outcome,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    /// "The provider answered and there is no PR" and "the provider could
    /// not be reached" are not the same value, and the type makes them
    /// different shapes rather than different strings.
    #[test]
    fn none_observed_is_not_provider_unavailable() {
        let none = ExternalFact::observed(
            ExternalSource::GitHubPullRequests,
            PullRequestState::NoneObserved,
            at(1),
        );
        let unavailable: ExternalFact<PullRequestState> =
            ExternalFact::unavailable(ExternalSource::GitHubPullRequests, ProbeReason::Failed);
        assert_ne!(none.outcome, unavailable.outcome);
        assert!(none.outcome.is_observed());
        assert!(!unavailable.outcome.is_observed());
        assert_ne!(
            PullRequestState::NoneObserved.tag(),
            "failed",
            "the token for 'asked, nothing there' must not collide with any \
             probe-failure wording a report might print beside it"
        );
    }

    /// A lower-bound flag only means anything beside a measurement. Carrying
    /// it on an unmeasured size would read as "at least an unknown amount".
    #[test]
    fn a_lower_bound_flag_needs_an_observed_measurement() {
        let mut c = candidate(
            "/w/a/target",
            ProbeOutcome::Observed(Some(git("/w/a", "/w/.git", false, false))),
        );
        c.0.reclaimable_bytes_is_lower_bound = true;
        let measured = build(&[c.clone()], &WorkspaceSurvey::unsurveyed());
        assert!(
            measured.repositories[0].worktrees[0].resources[0].reclaimable_bytes_is_lower_bound
        );

        c.0.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::TimedOut);
        let unmeasured = build(&[c], &WorkspaceSurvey::unsurveyed());
        assert!(
            !unmeasured.repositories[0].worktrees[0].resources[0].reclaimable_bytes_is_lower_bound
        );
    }

    /// Every `tag()` in this module is a wire-facing token some report or
    /// projection will print, so they have to be distinct within each enum
    /// — two states sharing a tag would be indistinguishable downstream.
    #[test]
    fn tags_are_distinct_within_each_enum() {
        fn all_distinct(tags: &[&str]) {
            let mut sorted = tags.to_vec();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), tags.len(), "{tags:?}");
        }
        all_distinct(&[
            UniqueWork::Present.tag(),
            UniqueWork::Absent.tag(),
            UniqueWork::Unknown.tag(),
        ]);
        all_distinct(&[
            PullRequestState::Open.tag(),
            PullRequestState::Merged.tag(),
            PullRequestState::ClosedUnmerged.tag(),
            PullRequestState::NoneObserved.tag(),
        ]);
        all_distinct(&[
            TaskState::ToDo.tag(),
            TaskState::InProgress.tag(),
            TaskState::Done.tag(),
            TaskState::Other("x".to_string()).tag(),
            TaskState::NoneObserved.tag(),
        ]);
        all_distinct(&[
            WorkflowMode::SerialSingleCheckout.tag(),
            WorkflowMode::SerialMultiBranch.tag(),
            WorkflowMode::ParallelMultiWorktree.tag(),
            WorkflowMode::Mixed.tag(),
            WorkflowMode::Unknown.tag(),
        ]);
        all_distinct(&[
            ExternalSource::GitHubPullRequests.tag(),
            ExternalSource::JiraIssues.tag(),
        ]);
        all_distinct(&[
            HistoryConfidence::Observed.tag(),
            HistoryConfidence::Insufficient.tag(),
        ]);
    }

    /// The workflow vocabulary describes the work, never the person. A
    /// variant grading the developer would leak into every report built
    /// from this graph, so the prohibition is asserted rather than left in
    /// a doc comment.
    #[test]
    fn no_workflow_mode_grades_the_developer() {
        let forbidden = [
            "advanced", "beginner", "expert", "novice", "good", "bad", "poor", "sloppy", "messy",
        ];
        for mode in [
            WorkflowMode::SerialSingleCheckout,
            WorkflowMode::SerialMultiBranch,
            WorkflowMode::ParallelMultiWorktree,
            WorkflowMode::Mixed,
            WorkflowMode::Unknown,
        ] {
            let tag = mode.tag();
            for word in forbidden {
                assert!(
                    !tag.contains(word),
                    "workflow mode {tag:?} grades the developer"
                );
            }
        }
    }

    /// Every declared vocabulary is complete and collision-free.
    ///
    /// Written as a match over each variant rather than as a length check,
    /// because a length check passes when somebody adds a variant and a
    /// placeholder tag together. The `match` here fails to compile when a
    /// variant is added, which is the point: the compiler asks the question
    /// before a preview surface silently under-reports what may leave.
    #[test]
    fn the_external_vocabularies_are_complete() {
        for source in ExternalSource::ALL {
            let expected = match source {
                ExternalSource::GitHubPullRequests => "github_pull_requests",
                ExternalSource::JiraIssues => "jira_issues",
            };
            assert_eq!(source.tag(), expected);
        }

        let pull_request_tags: Vec<&str> = PullRequestState::ALL.iter().map(|s| s.tag()).collect();
        for state in PullRequestState::ALL {
            let expected = match state {
                PullRequestState::Open => "open",
                PullRequestState::Merged => "merged",
                PullRequestState::ClosedUnmerged => "closed_unmerged",
                PullRequestState::NoneObserved => "none_observed",
            };
            assert_eq!(state.tag(), expected);
        }

        // `Other` stands in for every bespoke tracker status there is, so the
        // representative carries a name that must not appear in the tag.
        let task_states = [
            TaskState::ToDo,
            TaskState::InProgress,
            TaskState::Done,
            TaskState::Other("DEV VERIFY".into()),
            TaskState::NoneObserved,
        ];
        for state in &task_states {
            assert!(
                TaskState::ALL_TAGS.contains(&state.tag()),
                "{state:?} tags as {:?}, which is outside the declared vocabulary",
                state.tag()
            );
        }
        assert_eq!(task_states.len(), TaskState::ALL_TAGS.len());

        for tags in [
            &pull_request_tags[..],
            &TaskState::ALL_TAGS[..],
            &ExternalSource::ALL.map(ExternalSource::tag)[..],
        ] {
            let mut unique: Vec<&str> = tags.to_vec();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), tags.len(), "two values share a tag: {tags:?}");
        }
    }
}
