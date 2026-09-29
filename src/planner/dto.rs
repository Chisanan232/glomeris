//! Everything that leaves this Mac, and nothing else.
//!
//! # The allowlist is the type
//!
//! This module is the complete answer to "what does the provider see". Not
//! a filter applied to a richer structure, not a serializer with skipped
//! fields — a separate set of types whose every field was chosen. Adding a
//! field to [`crate::workspace::WorkspaceEvidenceGraph`] cannot widen
//! egress, because nothing here is derived automatically from it;
//! [`super::project`] has to be edited too, and
//! `model_payload_key_set_is_pinned` fails until the new key is written
//! into the pinned list by hand.
//!
//! # What this module may not name
//!
//! `PathBuf`, `Path`, `std::path`, [`crate::evidence::ResourceId`] and
//! [`crate::evidence::Evidence`]. `scripts/check-workspace-aggregation-has-no-authority.sh`
//! checks for those tokens, which turns the campaign's first rule — no
//! absolute paths reach a provider — into a property of the module rather
//! than a property of one test's fixtures. A module that cannot name a path
//! cannot serialize one, whatever future field someone adds in a hurry.
//!
//! Deliberately absent for the same reason, though no token check can
//! express them: repository directory names, worktree directory names,
//! branch names, the branch a merge was measured against, process command
//! lines, pids, pull-request titles, task keys, task titles, and usernames.
//! Each is identifying, none is needed to rank storage, and several would
//! carry a corporate project name off the machine on their own.
//!
//! # Why aliases are positional
//!
//! `repo_1`, `workspace_1`, `resource_1`. Not hashes: the hash of a path
//! whose only unknown segment is a project name is a guessable keyspace,
//! and a stable hash is a correlation handle across sessions. Positional
//! aliases are request-scoped and carry no information about the thing they
//! name. [`super::project::AliasTable`] keeps the mapping locally, and it
//! is not serializable.
//!
//! An alias doubles as an evidence reference: a planner response citing
//! `workspace_2` is citing this projection's second worktree, and anything
//! it cites that was never issued is rejected rather than guessed at.

use serde::Serialize;

/// The evidence reference for the machine-level facts. Fixed rather than
/// positional — there is one machine.
pub const MACHINE_EVIDENCE_REF: &str = "machine";

/// The evidence reference for the workflow baseline.
pub const WORKFLOW_HISTORY_EVIDENCE_REF: &str = "workflow_history";

/// One value as it is allowed to leave this Mac: what was observed, or why
/// there is no answer.
///
/// Both keys are always emitted, including as `null`. That is deliberate
/// and costs a few bytes: `#[serde(skip_serializing_if)]` would make the
/// serialized key set depend on the input, and a pinned key set that shifts
/// with the data cannot detect an added field. It also means a reader never
/// has to decide whether an absent key meant zero, false or unknown —
/// `status` always says which.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reported<T> {
    /// `"observed"` or `"unavailable"`.
    pub status: &'static str,
    /// The value, present only when `status` is `"observed"`.
    pub value: Option<T>,
    /// Why there is no value, present only when `status` is
    /// `"unavailable"`: a [`crate::evidence::ProbeReason`] tag. Carried
    /// because "the probe failed" and "the answer is no" are different
    /// facts, and a model shown only an absence will read the second.
    pub unavailable_reason: Option<&'static str>,
}

impl<T> Reported<T> {
    pub fn observed(value: T) -> Self {
        Self {
            status: "observed",
            value: Some(value),
            unavailable_reason: None,
        }
    }

    pub fn unavailable(reason: &'static str) -> Self {
        Self {
            status: "unavailable",
            value: None,
            unavailable_reason: Some(reason),
        }
    }
}

/// The complete model-facing projection. Serializing this is the only way
/// local evidence reaches a provider.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelGraphView {
    pub machine: MachineView,
    pub repositories: Vec<RepositoryView>,
    /// Resources with no containing repository — the global caches. Either
    /// the git probe ran and found none, or the resource is addressed by its
    /// owning tool rather than by a path, so there is nothing for a
    /// repository to contain (HORO-1561).
    pub global_resources: Vec<ResourceView>,
    /// Path-located resources whose containing repository could not be
    /// determined. Kept as their own list rather than folded into
    /// `global_resources`, so the model is never told a build directory is a
    /// global cache because a probe timed out.
    pub unplaced_resources: Vec<UnplacedResourceView>,
    pub workflow_history: WorkflowHistoryView,
}

/// Machine-level facts. No volume names, no mount points, no device
/// identifiers — three numbers and whether a goal is set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineView {
    pub evidence_ref: &'static str,
    pub free_bytes: Reported<u64>,
    pub total_bytes: Reported<u64>,
    /// How many bytes the active recovery goal still wants. `null` means no
    /// goal is set, which is a complete answer rather than an unknown one —
    /// hence a bare `Option` here where the two measurements are
    /// [`Reported`].
    pub recovery_goal_bytes: Option<u64>,
}

/// One repository, identified only by its position in this request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepositoryView {
    /// `repo_N`.
    pub evidence_ref: String,
    pub worktree_count: usize,
    pub worktrees: Vec<WorktreeView>,
}

/// One working tree.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorktreeView {
    /// `workspace_N`.
    pub evidence_ref: String,
    /// Whether this is a linked worktree rather than the main checkout. A
    /// shape fact about the repository, not an identity.
    pub linked: bool,
    pub branch: BranchView,
    pub activity: ActivityView,
    pub pull_request: ExternalFactView,
    pub task: ExternalFactView,
    pub resources: Vec<ResourceView>,
}

/// Branch lifecycle, as shapes rather than names.
///
/// No branch name and no merge axis. `merged_state` says whether HEAD is
/// contained somewhere, `merge_comparison_known` says whether there was an
/// axis to compare against at all — which is what distinguishes "not
/// merged" from "this machine has no `origin/HEAD` so nobody knows".
///
/// The integration fields HORO-1545 added keep to the same rule, and it
/// costs them something: no commit ids, no branch name on either side of
/// the comparison, no commit messages and no dates. A model is told that
/// two commits here have equivalents over there and cannot be told which
/// commits, which branch, or when — because every one of those is a
/// description of what this machine's owner is working on, and none of them
/// changes how a directory should be ranked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BranchView {
    pub dirty: bool,
    pub untracked: bool,
    /// `"present"`, `"absent"` or `"unknown"`. The single most important
    /// field in this projection: `"unknown"` is not a softer `"absent"`.
    pub unique_work: &'static str,
    /// `"untracked"`, `"tracking"` or `"unknown"`.
    pub upstream_state: Reported<&'static str>,
    pub commits_ahead_of_upstream: Reported<u32>,
    pub commits_behind_upstream: Reported<u32>,
    /// `"merged"`, `"not_merged"` or `"unknown"`.
    pub merged_state: Reported<&'static str>,
    pub merge_comparison_known: Reported<bool>,
    /// `true` when HEAD is detached. Not an error and not an absence — a
    /// state a worktree is sometimes deliberately left in.
    pub detached_head: Reported<bool>,
    /// `"equivalent"`, `"not_equivalent"` or `"not_applicable"` — whether
    /// the work here has landed on the comparison branch in some form other
    /// than ancestry (HORO-1545). `unavailable` covers both a probe that
    /// could not answer and an answer of "unknown", which are the same fact
    /// in this projection and carry the reason either way.
    ///
    /// `"not_applicable"` is the one worth reading carefully: it means
    /// ancestry already contains HEAD, so there was no question to ask —
    /// not that the answer was no.
    pub patch_equivalence: Reported<&'static str>,
    /// `"per_commit_patch_id"` or `"content_identical"`. Sent because the
    /// two are not equally strong: the first found every commit's patch
    /// already over there, the second found the *files* identical and says
    /// nothing about how they got that way.
    pub equivalence_method: Reported<&'static str>,
    /// Commits here that the comparison branch does not have and has no
    /// equivalent of.
    pub commits_unique_to_head: Reported<u32>,
    /// Commits here that the comparison branch has an equivalent patch for
    /// under a different id — what a cherry-pick or rebase leaves behind.
    pub commits_equivalent_elsewhere: Reported<u32>,
    /// Commits absent from the comparison branch that the per-commit method
    /// declined to classify, which in practice means merge commits. Not a
    /// count of commits known to hold nothing; a count of commits nobody
    /// asked about. Sent so the three above can be seen not to add up to
    /// "nothing unique here".
    pub commits_unclassified: Reported<u32>,
    /// How many days ago the newest commit here was written, and the same
    /// for the branch it was compared against. Durations rather than dates,
    /// and days rather than seconds: "a branch whose tip is older than the
    /// branch it was measured against" is the shape worth reasoning about,
    /// and a timestamp would additionally place this machine's activity on a
    /// calendar.
    pub head_tip_age_days: Reported<u64>,
    pub comparison_tip_age_days: Reported<u64>,
}

/// Whether anything is using this working tree.
///
/// Counts, never identities. A pid is useless to a ranking model and a
/// command line can name a corporate tool, so neither is sent.
/// `unanswered_probe_count` is here so the model can see *why* a state is
/// `"unknown"` and cannot mistake it for `"idle"`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivityView {
    /// `"in_use"`, `"idle"` or `"unknown"`.
    pub state: &'static str,
    pub observed_process_count: usize,
    pub unanswered_probe_count: usize,
}

/// One fact from a service outside this machine.
///
/// `state` distinguishes "the provider answered and there is nothing"
/// (`"none_observed"`) from "the provider could not answer" (a
/// [`Reported`] `unavailable`). Those being one value is the defect §10 of
/// the campaign names, and here they are not the same shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExternalFactView {
    /// Which kind of service this came from — `"github_pull_requests"` or
    /// `"jira_issues"`. The provider's identity, never the account's.
    pub source: &'static str,
    pub state: Reported<&'static str>,
    /// How stale the answer is. A merged pull request read four weeks ago
    /// supports less than one read a minute ago.
    pub observed_age_days: Reported<u64>,
}

/// One discovered resource.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceView {
    /// `resource_N`. The handle a planner response must select from; a
    /// response naming anything else is rejected.
    pub evidence_ref: String,
    /// A [`crate::evidence::ResourceKind`] tag.
    pub kind: &'static str,
    pub owning_tool: String,
    pub reclaimable_bytes: Reported<u64>,
    /// `true` when `reclaimable_bytes` is a measurement a bounded walk
    /// stopped short of finishing, so the real figure is at least that.
    /// Never set beside an unavailable measurement.
    pub reclaimable_bytes_is_lower_bound: bool,
    pub age_days: Reported<u64>,
    pub regenerability: &'static str,
    pub completeness: &'static str,
    pub policy_label: &'static str,
    /// Whether the tool that owns this resource is running. `unavailable`
    /// with `tool_not_running` for the tools that have no daemon to ask
    /// about, which is a real answer and not a gap.
    pub tool_liveness: Reported<bool>,
    /// The action ids on offer for this resource, from
    /// [`crate::actionability::eligible_action_ids`]. The planner may
    /// select from these and may not invent one; an id outside this list is
    /// dropped by validation rather than resolved.
    pub offered_action_ids: Vec<&'static str>,
    /// What Docker said about this object, for the resources Docker owns.
    ///
    /// `null` on every other resource. That is the one place in this module
    /// where an absent value is a complete answer rather than an unknown one,
    /// and it is why this is an `Option` and not a [`Reported`]: a Cargo
    /// target directory has no Docker activity to be unavailable about.
    pub docker_lifecycle: Option<DockerLifecycleView>,
}

/// What Docker can state about one of its own objects.
///
/// # Why the three fields and not the object
///
/// `tool_liveness` above answers "is the daemon up", which is one answer for
/// every Docker object on the machine. It cannot distinguish 11 GB of images
/// no container needs from the named volume a developer's local Postgres
/// keeps its data in, and those are opposite recommendations. So the per-object
/// facts are projected — and only as the three axes
/// [`crate::evidence::DockerLifecycle`] carries, each with its own explicit
/// `"unknown"`, never collapsed into one state string.
///
/// # Counts, never identities
///
/// The local graph links an image to the containers that need it by resource
/// identity. Those identities stay home. A container name is chosen by a human
/// or a compose file and routinely carries a product or customer name, an image
/// reference carries a registry host and a repository path, and neither is
/// needed to rank storage: "two things reference this, one of them is running"
/// is the whole of what a ranking model can act on. Same reasoning as
/// [`ActivityView`], reached independently for a different tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DockerLifecycleView {
    /// `"active"`, `"inactive"` or `"unknown"`. `"unknown"` is not a quieter
    /// `"inactive"`: an object whose activity was never established has not
    /// been reported idle.
    pub activity: &'static str,
    /// `"user_managed"`, `"tool_managed"` or `"unknown"`. Whose data this is,
    /// as far as Docker stated it — not inferred from a name.
    pub persistence: &'static str,
    /// How many other Docker objects Docker reported as referencing this one.
    /// `unavailable` when the reference query did not run, which is a
    /// different fact from an observed zero and must not arrive as one.
    pub referrer_count: Reported<usize>,
    /// How many of those referrers Docker reported as running.
    pub active_referrers: Reported<u32>,
}

/// A resource whose containing repository could not be established.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnplacedResourceView {
    pub resource: ResourceView,
    /// The [`crate::evidence::ProbeReason`] the git probe gave. Present so
    /// the model can weigh a missing `git` differently from a denied
    /// permission instead of treating both as "no repository".
    pub unplaced_reason: &'static str,
}

/// How this developer has been working, over more than one observation.
///
/// `mode` is `"unknown"` and `confidence` `"insufficient"` whenever too few
/// observations exist, so a single snapshot cannot be presented as a habit.
/// The prompt must also state that history never outranks today's evidence,
/// but the value itself is already honest before the prompt gets involved.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowHistoryView {
    pub evidence_ref: &'static str,
    /// `"serial_single_checkout"`, `"serial_multi_branch"`,
    /// `"parallel_multi_worktree"`, `"mixed"` or `"unknown"`.
    pub mode: Reported<&'static str>,
    /// `"observed"` or `"insufficient"`.
    pub confidence: Reported<&'static str>,
    pub observation_count: Reported<u32>,
}
