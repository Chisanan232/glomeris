//! Turning the local graph into the bounded projection, and keeping the
//! alias table on this side of the wire.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::contract::ProbeSubjectKind;
use super::dto::{
    ActivityView, BranchView, DockerLifecycleView, ExternalFactView, MachineView, ModelGraphView,
    PatchEquivalenceView, Reported, RepositoryView, ResourceView, UnplacedResourceView,
    WorkflowHistoryView, WorkflowSupportView, WorktreeView, MACHINE_EVIDENCE_REF,
    WORKFLOW_HISTORY_EVIDENCE_REF,
};
use crate::actions::llm::{completeness_tag, regenerability_tag};
use crate::actions::ActionRegistry;
use crate::evidence::{DockerLifecycle, Evidence, ProbeOutcome, ResourceId};
use crate::policy::PolicyDecision;
use crate::workspace::{
    ActivityFacts, BranchLifecycle, ExternalFact, IntegrationEvidence, PatchEquivalence,
    ResourceNode, UpstreamState, WorkflowHistorySummary, WorkspaceEvidenceGraph,
};

/// A projection plus the local tables needed to read its answers back.
///
/// The halves exist separately on purpose: `view` is serializable and the two
/// tables are not, so there is no code path that sends both.
#[derive(Debug, Clone)]
pub struct GraphProjection {
    pub view: ModelGraphView,
    pub aliases: AliasTable,
    /// What each issued reference names locally (HORO-1549). A superset of
    /// [`Self::aliases`] in coverage and a different question in kind: that one
    /// answers "which resource", this one answers "which *thing*, of the five
    /// kinds there are" — including the machine and a repository, which are
    /// citable and are not resources.
    pub subjects: SubjectTable,
}

/// Maps the opaque wire ids handed out by one projection back to the real
/// local resources.
///
/// Request-scoped: `resource_1` in one projection and `resource_1` in the
/// next are unrelated, which is the property that makes the alias useless as
/// a correlation handle. Not [`serde::Serialize`], and it must stay that
/// way — this is the half of the payload that never leaves.
#[derive(Debug, Clone, Default)]
pub struct AliasTable {
    resources: Vec<(String, ResourceId)>,
}

impl AliasTable {
    /// The real resource an alias refers to, or `None` for an alias this
    /// projection never issued.
    ///
    /// Returning `None` rather than a best guess is the whole contract: a
    /// model that names `resource_9` in a four-resource request has either
    /// hallucinated or is being replayed against the wrong projection, and
    /// both must fail closed. What a resolved alias yields is a resource to
    /// *re-evaluate* — [`crate::policy::classify`] still has to run against
    /// freshly collected evidence before anything executes.
    pub fn resolve(&self, alias: &str) -> Option<&ResourceId> {
        self.resources
            .iter()
            .find(|(issued, _)| issued == alias)
            .map(|(_, resource)| resource)
    }

    /// Every alias this projection issued, in the order they were handed
    /// out.
    pub fn issued(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(|(alias, _)| alias.as_str())
    }

    pub fn len(&self) -> usize {
        self.resources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

/// The local thing one issued evidence reference names (HORO-1549).
///
/// # Why this is not just [`AliasTable`] with more variants
///
/// An alias resolves to a resource so that policy can be re-run against it.
/// This resolves to whatever a reference names — including two things that are
/// citable and are not resources at all — so that a probe request can be
/// *refused for being about the wrong shape of thing* before any probe runs.
/// [`super::ProbeId::subject_kinds`] states which kinds each probe can answer
/// for; [`Self::kind`] is the other half of that check.
///
/// # Each variant carries what a probe would need and nothing more
///
/// A working tree carries its root, because every probe that accepts one
/// (`git_branch_state`, `git_patch_equivalence`, `process_activity`,
/// `github_pr_state`, `jira_task_state`) starts from a directory. It
/// deliberately does not carry the branch name or the branch state already
/// collected for the projection: this table answers *which thing*, and a probe
/// exists to collect evidence about it. Caching last round's answer here would
/// make a re-plan able to return stale facts while claiming to have probed —
/// and campaign section 16 is explicit that verifying means new evidence.
///
/// Not [`serde::Serialize`], and it must stay that way: these are the real
/// local paths, which is exactly what the opaque aliases exist to keep off the
/// wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeSubject {
    /// The machine-level facts. No probe accepts it.
    Machine,
    /// The local workflow baseline.
    WorkflowHistory,
    /// One repository, identified by the git directory its working trees
    /// share. No probe accepts it — see [`super::ProbeId::subject_kinds`].
    Repository { common_dir: PathBuf },
    /// One git working tree, identified by its root.
    Worktree { root: PathBuf },
    /// One storage resource.
    Resource { resource: ResourceId },
}

impl ProbeSubject {
    /// Which of the five kinds this is.
    pub fn kind(&self) -> ProbeSubjectKind {
        match self {
            Self::Machine => ProbeSubjectKind::Machine,
            Self::WorkflowHistory => ProbeSubjectKind::WorkflowHistory,
            Self::Repository { .. } => ProbeSubjectKind::Repository,
            Self::Worktree { .. } => ProbeSubjectKind::Worktree,
            Self::Resource { .. } => ProbeSubjectKind::Resource,
        }
    }
}

/// What every reference one projection issued names locally.
///
/// Request-scoped for the same reason [`AliasTable`] is, and built in the same
/// single traversal so the two cannot disagree. Keyed by the issued reference
/// string, which is the only handle a model ever holds.
#[derive(Debug, Clone, Default)]
pub struct SubjectTable {
    subjects: Vec<(String, ProbeSubject)>,
}

impl SubjectTable {
    /// What a reference names, or `None` for a reference this projection never
    /// issued.
    ///
    /// `None` is a refusal, not a fallback. A model naming `workspace_9` in a
    /// two-worktree request has hallucinated or is being replayed against
    /// another projection, and there is no working tree to substitute.
    pub fn resolve(&self, reference: &str) -> Option<&ProbeSubject> {
        self.subjects
            .iter()
            .find(|(issued, _)| issued == reference)
            .map(|(_, subject)| subject)
    }

    /// The kind of thing a reference names, or `None` if it was never issued.
    pub fn kind_of(&self, reference: &str) -> Option<ProbeSubjectKind> {
        self.resolve(reference).map(ProbeSubject::kind)
    }

    /// Every reference this table holds, in the order they were handed out.
    pub fn issued(&self) -> impl Iterator<Item = &str> {
        self.subjects
            .iter()
            .map(|(reference, _)| reference.as_str())
    }

    pub fn len(&self) -> usize {
        self.subjects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty()
    }
}

/// Hands out `resource_N` / `workspace_N` / `repo_N` in one traversal, so
/// two callers cannot disagree about which alias belongs to what.
struct AliasIssuer {
    next_resource: usize,
    next_worktree: usize,
    next_repository: usize,
    table: AliasTable,
    subjects: SubjectTable,
}

impl AliasIssuer {
    fn new() -> Self {
        // The machine and the workflow baseline are not issued during the
        // traversal — their references are fixed constants and
        // `machine_view`/`workflow_history_view` always emit them — so they are
        // seeded here. Omitting them would leave two references the payload
        // carries with no subject, and a probe asked about either would be
        // refused as *unknown* rather than as *the wrong kind*, which is a
        // different and less honest answer.
        let subjects = SubjectTable {
            subjects: vec![
                (MACHINE_EVIDENCE_REF.to_string(), ProbeSubject::Machine),
                (
                    WORKFLOW_HISTORY_EVIDENCE_REF.to_string(),
                    ProbeSubject::WorkflowHistory,
                ),
            ],
        };
        Self {
            next_resource: 0,
            next_worktree: 0,
            next_repository: 0,
            table: AliasTable::default(),
            subjects,
        }
    }

    fn repository(&mut self, common_dir: &Path) -> String {
        self.next_repository += 1;
        let alias = format!("repo_{}", self.next_repository);
        self.subjects.subjects.push((
            alias.clone(),
            ProbeSubject::Repository {
                common_dir: common_dir.to_path_buf(),
            },
        ));
        alias
    }

    fn worktree(&mut self, root: &Path) -> String {
        self.next_worktree += 1;
        let alias = format!("workspace_{}", self.next_worktree);
        self.subjects.subjects.push((
            alias.clone(),
            ProbeSubject::Worktree {
                root: root.to_path_buf(),
            },
        ));
        alias
    }

    fn resource(&mut self, resource: &ResourceId) -> String {
        self.next_resource += 1;
        let alias = format!("resource_{}", self.next_resource);
        self.table.resources.push((alias.clone(), resource.clone()));
        self.subjects.subjects.push((
            alias.clone(),
            ProbeSubject::Resource {
                resource: resource.clone(),
            },
        ));
        alias
    }
}

impl GraphProjection {
    /// Projects one graph for one request.
    ///
    /// `candidates` supplies what the graph deliberately does not carry: the
    /// [`Evidence`] and [`PolicyDecision`] each resource was judged from,
    /// which is what [`crate::actionability::eligible_action_ids`] needs.
    /// The graph cannot hold that itself — it lives in
    /// [`crate::workspace`], which may not name an action id — so the join
    /// happens here, keyed on resource identity.
    ///
    /// A resource in the graph with no matching candidate gets an empty
    /// offered-action list. That is the honest projection of "nothing is on
    /// offer", and it is also the safe direction: a missing join can only
    /// ever remove options from a ranking, never add one.
    pub fn build(
        graph: &WorkspaceEvidenceGraph,
        candidates: &[(Evidence, PolicyDecision)],
        actions: &ActionRegistry,
        now: SystemTime,
    ) -> Self {
        let offers = offered_actions_by_resource(candidates, actions);
        let mut issuer = AliasIssuer::new();

        let repositories = graph
            .repositories
            .iter()
            .map(|repo| {
                let evidence_ref = issuer.repository(&repo.common_dir);
                RepositoryView {
                    evidence_ref,
                    worktree_count: repo.worktree_count(),
                    worktrees: repo
                        .worktrees
                        .iter()
                        .map(|worktree| {
                            let evidence_ref = issuer.worktree(&worktree.root);
                            WorktreeView {
                                evidence_ref,
                                linked: worktree.linked,
                                branch: branch_view(&worktree.lifecycle, now),
                                activity: activity_view(&worktree.activity),
                                pull_request: external_view(
                                    &worktree.external.pull_request,
                                    |state| state.tag(),
                                    now,
                                ),
                                task: external_view(
                                    &worktree.external.task,
                                    |state| state.tag(),
                                    now,
                                ),
                                resources: worktree
                                    .resources
                                    .iter()
                                    .map(|node| resource_view(node, &mut issuer, &offers))
                                    .collect(),
                            }
                        })
                        .collect(),
                }
            })
            .collect();

        let global_resources = graph
            .global_resources
            .iter()
            .map(|node| resource_view(&node.resource, &mut issuer, &offers))
            .collect();

        let unplaced_resources = graph
            .unplaced_resources
            .iter()
            .map(|node| UnplacedResourceView {
                resource: resource_view(&node.resource, &mut issuer, &offers),
                unplaced_reason: node.reason.tag(),
            })
            .collect();

        Self {
            view: ModelGraphView {
                machine: machine_view(graph),
                repositories,
                global_resources,
                unplaced_resources,
                workflow_history: workflow_history_view(&graph.history),
            },
            aliases: issuer.table,
            subjects: issuer.subjects,
        }
    }

    /// Every evidence reference this projection actually sent.
    ///
    /// # Read off the view, never off the issuer
    ///
    /// The obvious implementation is to record each alias as [`AliasIssuer`]
    /// hands it out. This walks the finished [`ModelGraphView`] instead, and the
    /// difference is the point: a reference is "issued" only if it reached the
    /// serialized payload. Were a future edit to project a repository without
    /// its worktrees, or to drop the unplaced list, the issuer's record would
    /// still claim those aliases were sent — and a model would be marked as
    /// having cited something it was never shown. Walking the view cannot say
    /// that.
    ///
    /// # What it is for
    ///
    /// [`super::validate`] accepts an `evidence_refs` entry only if it is in
    /// here (HORO-1548). A response citing `workspace_9` in a request that
    /// carried two worktrees is citing nothing, and a report that rendered the
    /// citation anyway would be showing a reader a provenance trail that does
    /// not exist. Distinct from [`AliasTable::resolve`], which answers a
    /// narrower question — which local resource an alias means — and only for
    /// resources: a repository, a worktree, the machine and the workflow
    /// baseline are citable but are not resources, so they have no entry there
    /// and must have one here.
    pub fn issued_evidence_refs(&self) -> std::collections::BTreeSet<&str> {
        let mut refs = std::collections::BTreeSet::new();
        refs.insert(self.view.machine.evidence_ref);
        refs.insert(self.view.workflow_history.evidence_ref);

        for repository in &self.view.repositories {
            refs.insert(repository.evidence_ref.as_str());
            for worktree in &repository.worktrees {
                refs.insert(worktree.evidence_ref.as_str());
                for resource in &worktree.resources {
                    refs.insert(resource.evidence_ref.as_str());
                }
            }
        }
        for resource in &self.view.global_resources {
            refs.insert(resource.evidence_ref.as_str());
        }
        for unplaced in &self.view.unplaced_resources {
            refs.insert(unplaced.resource.evidence_ref.as_str());
        }

        refs
    }

    /// The projected resource an alias refers to, or `None` for an alias this
    /// projection never issued.
    ///
    /// The companion to [`AliasTable::resolve`], which answers *which local
    /// resource* an alias means. This answers *what the model was told about
    /// it* — and the field [`super::validate`] needs is
    /// [`ResourceView::offered_action_ids`]. Checking a claimed action id
    /// against that list, rather than against the whole
    /// [`ActionRegistry`], is the tightening HORO-1548 makes over version 1:
    /// `CargoCleanTargetDir` is a registered action, so v1 accepted it on a
    /// `node_modules` directory and left the mismatch for policy to refuse
    /// later. The offered list is per resource, so the mismatch is refused
    /// here, at the parse boundary, and the report never carries an item that
    /// was never on offer.
    ///
    /// Walks the view for the same reason [`Self::issued_evidence_refs`] does:
    /// a resource the payload did not carry cannot have been offered anything.
    pub fn resource_view(&self, alias: &str) -> Option<&ResourceView> {
        self.view
            .repositories
            .iter()
            .flat_map(|repository| repository.worktrees.iter())
            .flat_map(|worktree| worktree.resources.iter())
            .chain(self.view.global_resources.iter())
            .chain(
                self.view
                    .unplaced_resources
                    .iter()
                    .map(|unplaced| &unplaced.resource),
            )
            .find(|resource| resource.evidence_ref == alias)
    }

    /// What kind of thing an issued reference names, or `None` if this
    /// projection never issued it.
    ///
    /// The lookup [`super::validate`] needs to refuse a probe request whose
    /// subject is the wrong *shape* — `git_branch_state` about `repo_1`, say.
    /// It reads the kind off [`Self::subjects`] rather than off the spelling of
    /// the reference, because a check on a `"workspace_"` prefix would make the
    /// naming scheme load-bearing: renaming an alias would silently widen what
    /// probes accept.
    pub fn probe_subject_kind(&self, reference: &str) -> Option<ProbeSubjectKind> {
        self.subjects.kind_of(reference)
    }
}

/// Which action ids are on offer per resource, keyed by the resource's own
/// string form. Keyed on the string rather than the [`ResourceId`] because
/// that is what both sides already produce for display, and because a key
/// type with a path in it has no business in a lookup this module owns.
fn offered_actions_by_resource(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
) -> BTreeMap<String, Vec<&'static str>> {
    candidates
        .iter()
        .map(|(ev, decision)| {
            (
                ev.resource.to_string(),
                crate::actionability::eligible_action_ids(ev, decision, actions),
            )
        })
        .collect()
}

fn machine_view(graph: &WorkspaceEvidenceGraph) -> MachineView {
    MachineView {
        evidence_ref: MACHINE_EVIDENCE_REF,
        free_bytes: reported_copy(&graph.machine.free_bytes),
        total_bytes: reported_copy(&graph.machine.total_bytes),
        recovery_goal_bytes: graph.machine.recovery_goal_bytes,
    }
}

/// One workflow baseline as the model sees it.
///
/// Takes the outcome rather than the graph so the same projection serves a
/// freshly run `workspace_history_summary` probe (HORO-1549), which has an
/// outcome and no graph. Keeping one function means a probe result and a
/// snapshot cannot describe the same baseline differently.
pub(super) fn workflow_history_view(
    history: &ProbeOutcome<WorkflowHistorySummary>,
) -> WorkflowHistoryView {
    match history {
        ProbeOutcome::Observed(summary) => WorkflowHistoryView {
            evidence_ref: WORKFLOW_HISTORY_EVIDENCE_REF,
            mode: Reported::observed(summary.mode.tag()),
            confidence: Reported::observed(summary.confidence.tag()),
            observation_count: Reported::observed(summary.observation_count),
            support: WorkflowSupportView::of(&summary.support),
        },
        // One reason, repeated across all three fields: no baseline was
        // collected, so there is no mode, no confidence in a mode, and no
        // count — as opposed to a mode of "unknown", which is what a
        // *collected* baseline over too few observations reports.
        ProbeOutcome::Unavailable(reason) => WorkflowHistoryView {
            evidence_ref: WORKFLOW_HISTORY_EVIDENCE_REF,
            mode: Reported::unavailable(reason.tag()),
            confidence: Reported::unavailable(reason.tag()),
            observation_count: Reported::unavailable(reason.tag()),
            support: WorkflowSupportView::nothing_observed(),
        },
    }
}

/// Every field unavailable for one reason — what the branch probe not
/// answering leaves behind.
///
/// `pub(super)` because a `git_patch_equivalence` evidence request that the
/// probe could not answer has to produce the same shape, and a second
/// hand-written set of seven unavailable fields would be a second chance to
/// get one of them wrong.
pub(super) fn patch_equivalence_unavailable(reason: &'static str) -> PatchEquivalenceView {
    PatchEquivalenceView {
        patch_equivalence: Reported::unavailable(reason),
        equivalence_method: Reported::unavailable(reason),
        commits_unique_to_head: Reported::unavailable(reason),
        commits_equivalent_elsewhere: Reported::unavailable(reason),
        commits_unclassified: Reported::unavailable(reason),
        head_tip_age_days: Reported::unavailable(reason),
        comparison_tip_age_days: Reported::unavailable(reason),
    }
}

/// What a measured [`IntegrationEvidence`] says, as the model sees it.
pub(super) fn patch_equivalence_view(
    integration: &IntegrationEvidence,
    now: SystemTime,
) -> PatchEquivalenceView {
    // `Unknown` collapses into `unavailable` carrying its reason rather
    // than becoming an observed `"unknown"` string. Two layers of
    // not-knowing on one field is one layer too many for a reader to
    // keep straight, and this way the reason survives — which is the
    // only part of an unknown that is worth anything.
    let (equivalence, method) = match integration.equivalence {
        PatchEquivalence::Equivalent(method) => (
            Reported::observed("equivalent"),
            Reported::observed(method.tag()),
        ),
        PatchEquivalence::NotEquivalent => (
            Reported::observed("not_equivalent"),
            // No method, because nothing was established. Not
            // `failed`: both methods ran and agreed.
            Reported::unavailable("not_attempted"),
        ),
        PatchEquivalence::NotApplicable => (
            Reported::observed("not_applicable"),
            Reported::unavailable("not_attempted"),
        ),
        PatchEquivalence::Unknown(reason) => (
            Reported::unavailable(reason.tag()),
            Reported::unavailable(reason.tag()),
        ),
    };
    let (unique, equivalent, unclassified) = match &integration.divergence {
        ProbeOutcome::Observed(divergence) => (
            Reported::observed(divergence.unique_commits),
            Reported::observed(divergence.equivalent_commits),
            Reported::observed(divergence.unclassified_commits),
        ),
        ProbeOutcome::Unavailable(reason) => (
            Reported::unavailable(reason.tag()),
            Reported::unavailable(reason.tag()),
            Reported::unavailable(reason.tag()),
        ),
    };
    PatchEquivalenceView {
        patch_equivalence: equivalence,
        equivalence_method: method,
        commits_unique_to_head: unique,
        commits_equivalent_elsewhere: equivalent,
        commits_unclassified: unclassified,
        head_tip_age_days: tip_age_days(&integration.tip_committed_at, now),
        comparison_tip_age_days: tip_age_days(&integration.comparison_tip_committed_at, now),
    }
}

/// A commit timestamp as an age in whole days, or the reason there is none.
fn tip_age_days(tip: &ProbeOutcome<SystemTime>, now: SystemTime) -> Reported<u64> {
    match tip {
        ProbeOutcome::Observed(at) => match now.duration_since(*at) {
            Ok(age) => Reported::observed(age.as_secs() / 86_400),
            // A commit dated after this machine's clock — a clock that moved
            // or a commit written with a date of its own. No age rather than
            // a zero, which would read as "written moments ago" and is the
            // flattering half of a subtraction with no answer.
            Err(_) => Reported::unavailable("failed"),
        },
        ProbeOutcome::Unavailable(reason) => Reported::unavailable(reason.tag()),
    }
}

pub(super) fn branch_view(lifecycle: &BranchLifecycle, now: SystemTime) -> BranchView {
    let (upstream_state, ahead, behind, merged_state, comparison_known, detached) =
        match &lifecycle.branch {
            ProbeOutcome::Observed(state) => {
                let (upstream_tag, ahead, behind) = match &state.upstream {
                    UpstreamState::Untracked => {
                        // No upstream is not "nothing unpushed": there is no
                        // published counterpart to count against, so the
                        // counts are genuinely unavailable rather than zero.
                        (
                            Reported::observed("untracked"),
                            Reported::unavailable("not_attempted"),
                            Reported::unavailable("not_attempted"),
                        )
                    }
                    UpstreamState::Tracking { ahead, behind } => (
                        Reported::observed("tracking"),
                        Reported::observed(*ahead),
                        Reported::observed(*behind),
                    ),
                    UpstreamState::Unknown => (
                        Reported::observed("unknown"),
                        Reported::unavailable("failed"),
                        Reported::unavailable("failed"),
                    ),
                };
                let merged = &state.merged;
                (
                    upstream_tag,
                    ahead,
                    behind,
                    Reported::observed(merged.tag()),
                    Reported::observed(merged.compared_against().is_some()),
                    Reported::observed(state.branch.is_none()),
                )
            }
            // The branch probe did not answer, so every branch-derived field
            // is unavailable for that reason — and `unique_work` below
            // reports `"unknown"` rather than `"absent"`.
            ProbeOutcome::Unavailable(reason) => (
                Reported::unavailable(reason.tag()),
                Reported::unavailable(reason.tag()),
                Reported::unavailable(reason.tag()),
                Reported::unavailable(reason.tag()),
                Reported::unavailable(reason.tag()),
                Reported::unavailable(reason.tag()),
            ),
        };
    let integration = match &lifecycle.branch {
        ProbeOutcome::Observed(state) => patch_equivalence_view(&state.integration, now),
        ProbeOutcome::Unavailable(reason) => patch_equivalence_unavailable(reason.tag()),
    };

    BranchView {
        dirty: lifecycle.dirty,
        untracked: lifecycle.untracked,
        unique_work: lifecycle.unique_work().tag(),
        upstream_state,
        commits_ahead_of_upstream: ahead,
        commits_behind_upstream: behind,
        merged_state,
        merge_comparison_known: comparison_known,
        detached_head: detached,
        integration,
    }
}

pub(super) fn activity_view(activity: &ActivityFacts) -> ActivityView {
    ActivityView {
        state: activity.state.tag(),
        observed_process_count: activity.observed_processes.len(),
        unanswered_probe_count: activity.unanswered_probes,
    }
}

pub(super) fn external_view<T, F>(
    fact: &ExternalFact<T>,
    tag: F,
    now: SystemTime,
) -> ExternalFactView
where
    F: Fn(&T) -> &'static str,
{
    let state = match &fact.outcome {
        ProbeOutcome::Observed(value) => Reported::observed(tag(value)),
        ProbeOutcome::Unavailable(reason) => Reported::unavailable(reason.tag()),
    };
    let observed_age_days = match fact.age(now) {
        Some(age) => Reported::observed(age.as_secs() / 86_400),
        // An unavailable fact has no observation to age, and an observation
        // this machine's clock cannot subtract has no age either. Both are
        // `not_attempted` rather than `0`, which would read as "read just
        // now".
        None => Reported::unavailable("not_attempted"),
    };
    ExternalFactView {
        source: fact.source.tag(),
        state,
        observed_age_days,
    }
}

fn resource_view(
    node: &ResourceNode,
    issuer: &mut AliasIssuer,
    offers: &BTreeMap<String, Vec<&'static str>>,
) -> ResourceView {
    ResourceView {
        evidence_ref: issuer.resource(&node.resource),
        kind: node.resource.kind.tag(),
        owning_tool: node.owning_tool.to_string(),
        reclaimable_bytes: reported_copy(&node.reclaimable_bytes),
        reclaimable_bytes_is_lower_bound: node.reclaimable_bytes_is_lower_bound,
        age_days: reported_copy(&node.age_days()),
        regenerability: regenerability_tag(node.regenerability),
        completeness: completeness_tag(&node.completeness),
        policy_label: node.label.as_str(),
        tool_liveness: reported_copy(&node.tool_liveness),
        offered_action_ids: offers
            .get(&node.resource.to_string())
            .cloned()
            .unwrap_or_default(),
        docker_lifecycle: node.docker_lifecycle.as_ref().map(docker_lifecycle_view),
    }
}

/// Projects the three axes and the *size* of the reference set, never its
/// members — see [`DockerLifecycleView`] for why the identities stay local.
fn docker_lifecycle_view(lifecycle: &DockerLifecycle) -> DockerLifecycleView {
    let (referrer_count, active_referrers) = match &lifecycle.references {
        ProbeOutcome::Observed(references) => (
            Reported::observed(references.referenced_by.len()),
            Reported::observed(references.active_referrers),
        ),
        // Docker was not asked, or did not answer. Zero referrers would be a
        // positive statement that nothing needs this object, which is the one
        // thing an unanswered query has not established.
        ProbeOutcome::Unavailable(reason) => (
            Reported::unavailable(reason.tag()),
            Reported::unavailable(reason.tag()),
        ),
    };
    DockerLifecycleView {
        activity: lifecycle.activity.tag(),
        persistence: lifecycle.persistence.tag(),
        referrer_count,
        active_referrers,
    }
}

fn reported_copy<T: Copy>(outcome: &ProbeOutcome<T>) -> Reported<T> {
    match outcome {
        ProbeOutcome::Observed(value) => Reported::observed(*value),
        ProbeOutcome::Unavailable(reason) => Reported::unavailable(reason.tag()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        GitState, NativeCleanup, ProbeReason, Recoverability, ResourceFingerprint, ResourceKind,
        ResourceLocator,
    };
    use crate::policy::{PolicyClass, ReasonCode};
    use crate::workspace::{
        Divergence, EquivalenceMethod, ExternalSource, IntegrationEvidence, MachineContext,
        MergedState, PullRequestState, TaskState, WorkflowHistorySummary, WorkflowMode,
        WorkflowSupport, WorkspaceSurvey, WorktreeBranchState,
    };
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use std::{fs, time::SystemTime};

    const T0: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    /// A marker that could only reach a payload by way of a real local path,
    /// so a leak test over it cannot pass by accident.
    const ACCOUNT_MARKER: &str = "glomeris-planner-account-marker";

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
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(4096),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(T0),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::CargoTargetDir.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state,
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: at(86_400 * 7),
            sources: Vec::new(),
        }
    }

    fn decision(path: &str, class: PolicyClass) -> PolicyDecision {
        PolicyDecision {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from(path)),
            ),
            class,
            reasons: vec![ReasonCode::NoActiveUseObserved],
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
            decision(path, PolicyClass::AutoSafe),
        )
    }

    /// A resource Docker addresses by its own id, as a real detector produces
    /// it: no path, so `git_state` is `Unavailable(NotAttempted)` because there
    /// was nothing to run the probe against.
    fn tool_candidate(kind: ResourceKind, id: &str) -> (Evidence, PolicyDecision) {
        let resource = ResourceId::new(
            kind,
            ResourceLocator::Tool {
                tool: crate::evidence::OwningTool::Docker,
                id: id.to_string(),
            },
        );
        let mut ev = evidence(
            "/unused",
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        );
        ev.resource = resource.clone();
        ev.regenerability = kind.regenerability();
        let mut d = decision("/unused", PolicyClass::Ask);
        d.resource = resource;
        (ev, d)
    }

    fn in_repo(root: &str, common_dir: &str) -> ProbeOutcome<Option<GitState>> {
        ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from(root),
            common_dir: PathBuf::from(common_dir),
            dirty: false,
            untracked: false,
            worktree: true,
        }))
    }

    /// A real cargo project on disk. `offered_action_ids` depends on whether
    /// `cargo.clean.target_dir` can actually plan for the resource, and
    /// planning stats `Cargo.toml` — so a fabricated path yields an empty
    /// list and would make every offer assertion vacuous.
    fn temp_cargo_project(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glomeris-{prefix}-{ACCOUNT_MARKER}-{}-{nanos}-{n}",
            std::process::id()
        ));
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).expect("create temp target dir");
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write manifest");
        target_dir
    }

    fn graph_of(candidates: &[(Evidence, PolicyDecision)]) -> WorkspaceEvidenceGraph {
        WorkspaceEvidenceGraph::build(
            candidates,
            &WorkspaceSurvey::unsurveyed(),
            MachineContext::unmeasured(),
            at(86_400 * 7),
        )
    }

    fn project(candidates: &[(Evidence, PolicyDecision)]) -> GraphProjection {
        GraphProjection::build(
            &graph_of(candidates),
            candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        )
    }

    fn json(projection: &GraphProjection) -> String {
        serde_json::to_string(&projection.view).expect("the projection serializes")
    }

    // ---------------------------------------------------------------
    // Egress
    // ---------------------------------------------------------------

    /// The load-bearing test of this module. A projection built from real
    /// absolute paths, a real repository layout and a real account-shaped
    /// marker must serialize to something containing none of them.
    #[test]
    fn a_serialized_projection_contains_no_path_and_no_local_identity() {
        let target = temp_cargo_project("egress");
        let target_str = target.to_str().expect("utf-8 temp path").to_string();
        let repo_root = target
            .parent()
            .expect("target has a parent")
            .to_str()
            .expect("utf-8")
            .to_string();

        let candidates = vec![
            candidate(
                &target_str,
                in_repo(&repo_root, &format!("{repo_root}/.git")),
            ),
            candidate(
                "/Users/someone/Library/Caches/acme-internal/registry",
                ProbeOutcome::Observed(None),
            ),
        ];
        let projection = project(&candidates);
        let body = json(&projection);

        assert!(
            !body.contains(ACCOUNT_MARKER),
            "the marker reached the payload: {body}"
        );
        assert!(
            !body.contains('/'),
            "a path separator reached the payload: {body}"
        );
        assert!(!body.contains("acme-internal"));
        assert!(!body.contains("Library"));
        assert!(!body.contains("Caches"));
        assert!(!body.contains(".git"));

        // Non-vacuity: the payload is a real projection of those resources,
        // not an empty object that trivially contains no paths.
        assert!(body.contains("cargo_target_dir"));
        assert!(body.contains("resource_1"));
        assert!(body.contains("repo_1"));
        assert!(body.contains("workspace_1"));
        assert_eq!(projection.aliases.len(), 2);

        // And the marker IS in the local half, so the assertions above are
        // discriminating rather than testing a fixture that never held it.
        assert!(projection
            .aliases
            .resolve("resource_1")
            .expect("resource_1 was issued")
            .to_string()
            .contains(ACCOUNT_MARKER));
    }

    /// The alias table is the only way back, and an alias nobody issued
    /// resolves to nothing rather than to the nearest plausible resource.
    #[test]
    fn an_alias_this_projection_never_issued_resolves_to_nothing() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let projection = project(&candidates);

        assert!(projection.aliases.resolve("resource_1").is_some());
        for bogus in [
            "resource_2",
            "resource_0",
            "resource_99",
            "workspace_1",
            "repo_1",
            "/w/a/target",
            "",
            "RESOURCE_1",
        ] {
            assert!(
                projection.aliases.resolve(bogus).is_none(),
                "{bogus:?} resolved"
            );
        }
        assert_eq!(
            projection.aliases.issued().collect::<Vec<_>>(),
            vec!["resource_1"]
        );
    }

    /// Aliases are positional and request-scoped: the same resource in a
    /// different request gets whatever position it occupies there. That is
    /// what stops an alias being a stable handle something could correlate
    /// across sessions.
    #[test]
    fn aliases_are_positional_and_not_derived_from_the_resource() {
        let a = candidate("/w/a/target", in_repo("/w/a", "/w/.git"));
        let b = candidate("/w/b/target", in_repo("/w/b", "/w/.git"));

        let first = project(&[a.clone(), b.clone()]);
        let second = project(&[b, a.clone()]);

        // Both requests issue the same alias set...
        assert_eq!(
            first.aliases.issued().collect::<Vec<_>>(),
            second.aliases.issued().collect::<Vec<_>>()
        );
        // ...and in both it points at whatever sits in that position, which
        // here is the same resource only because the graph sorts. The point
        // is that the alias carries no information about the resource: two
        // different resources both occupy `resource_1` across these calls.
        let solo = project(&[a]);
        assert_eq!(solo.aliases.len(), 1);
        assert_eq!(
            solo.aliases.resolve("resource_1"),
            first.aliases.resolve("resource_1")
        );
    }

    // ---------------------------------------------------------------
    // Subject table (HORO-1549)
    // ---------------------------------------------------------------

    /// The cross-check that keeps the two halves from drifting.
    ///
    /// [`GraphProjection::issued_evidence_refs`] deliberately walks the
    /// finished view, because a reference counts as issued only once it reached
    /// the payload. [`SubjectTable`] cannot be built that way — only the
    /// traversal sees the local nodes and their paths — so it is built from the
    /// issuer, and the two sources have to be held equal by a test rather than
    /// by construction. A reference in the view with no subject would be
    /// refused as *unknown*; a subject for a reference the view never carried
    /// would let a probe run against something the model was never shown.
    #[test]
    fn every_issued_reference_has_exactly_one_subject_and_vice_versa() {
        let target = temp_cargo_project("subjects");
        let target_str = target.to_str().expect("utf-8 temp path").to_string();
        let repo_root = target
            .parent()
            .expect("target has a parent")
            .to_str()
            .expect("utf-8")
            .to_string();
        let candidates = vec![
            candidate(
                &target_str,
                in_repo(&repo_root, &format!("{repo_root}/.git")),
            ),
            candidate("/w/b/target", in_repo("/w/b", "/w/.git")),
            candidate("/opt/homebrew/cache", ProbeOutcome::Observed(None)),
        ];
        let projection = project(&candidates);

        let issued: BTreeSet<&str> = projection.issued_evidence_refs();
        let subjects: BTreeSet<&str> = projection.subjects.issued().collect();
        assert_eq!(
            issued, subjects,
            "the view and the subject table disagree about what was issued"
        );

        // Non-vacuity: this fixture really does carry all five kinds, so the
        // equality above is over a populated set and not over two empties.
        let kinds: BTreeSet<&str> = issued
            .iter()
            .map(|reference| {
                projection
                    .probe_subject_kind(reference)
                    .unwrap_or_else(|| panic!("{reference} has no subject"))
                    .tag()
            })
            .collect();
        assert_eq!(
            kinds,
            BTreeSet::from([
                "machine",
                "workflow_history",
                "repository",
                "worktree",
                "resource",
            ])
        );
    }

    /// A subject resolves to the real local thing — which is why the table may
    /// never be serialized, and why the check is worth making: if a working
    /// tree's subject held no path, every kind check would still pass and the
    /// probe runner would have nothing to run against.
    #[test]
    fn a_subject_carries_the_real_local_identity_the_alias_hides() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let projection = project(&candidates);

        assert_eq!(
            projection.subjects.resolve("repo_1"),
            Some(&ProbeSubject::Repository {
                common_dir: PathBuf::from("/w/.git")
            })
        );
        assert_eq!(
            projection.subjects.resolve("workspace_1"),
            Some(&ProbeSubject::Worktree {
                root: PathBuf::from("/w/a")
            })
        );
        match projection
            .subjects
            .resolve("resource_1")
            .expect("resource_1 was issued")
        {
            ProbeSubject::Resource { resource } => assert_eq!(
                Some(resource),
                projection.aliases.resolve("resource_1"),
                "the two tables name different resources for one alias"
            ),
            other => panic!("resource_1 resolved to {other:?}"),
        }
        assert_eq!(
            projection.subjects.resolve(MACHINE_EVIDENCE_REF),
            Some(&ProbeSubject::Machine)
        );
        assert_eq!(
            projection.subjects.resolve(WORKFLOW_HISTORY_EVIDENCE_REF),
            Some(&ProbeSubject::WorkflowHistory)
        );
    }

    /// A reference this projection never issued has no kind, so a probe about
    /// it is refused for being unknown rather than resolved to the nearest
    /// plausible subject.
    #[test]
    fn a_reference_this_projection_never_issued_has_no_subject() {
        let projection = project(&[candidate("/w/a/target", in_repo("/w/a", "/w/.git"))]);

        for bogus in [
            "workspace_2",
            "repo_2",
            "resource_2",
            "workspace_0",
            "WORKSPACE_1",
            "workspace_history",
            "/w/a",
            "",
        ] {
            assert!(
                projection.probe_subject_kind(bogus).is_none(),
                "{bogus:?} resolved to a subject"
            );
        }
        // Discriminating: the three this projection did issue all resolve.
        for real in ["workspace_1", "repo_1", "resource_1"] {
            assert!(projection.probe_subject_kind(real).is_some(), "{real}");
        }
    }

    /// No reference names two subjects.
    ///
    /// Worth its own test because the previous one cannot catch this:
    /// [`GraphProjection::issued_evidence_refs`] returns a set, so a duplicate
    /// reference would be deduplicated there and the two sides would still
    /// compare equal. [`SubjectTable`] is a list and [`SubjectTable::resolve`]
    /// takes the first match, so a second `workspace_1` would be silently
    /// unreachable — which is exactly what per-repository alias counters would
    /// produce. This fixture spans two repositories with two working trees
    /// each, so a reset counter fails it.
    #[test]
    fn no_issued_reference_names_two_subjects() {
        let candidates = vec![
            candidate("/a/w1/target", in_repo("/a/w1", "/a/.git")),
            candidate("/a/w2/target", in_repo("/a/w2", "/a/.git")),
            candidate("/b/w1/target", in_repo("/b/w1", "/b/.git")),
            candidate("/b/w2/target", in_repo("/b/w2", "/b/.git")),
        ];
        let projection = project(&candidates);

        let issued: Vec<&str> = projection.subjects.issued().collect();
        let distinct: BTreeSet<&str> = issued.iter().copied().collect();
        assert_eq!(
            issued.len(),
            distinct.len(),
            "a reference is recorded twice: {issued:?}"
        );

        // Non-vacuity: the fixture really is two repositories of two working
        // trees, which is the shape a per-repository counter would collide on.
        assert_eq!(projection.view.repositories.len(), 2);
        for repository in &projection.view.repositories {
            assert_eq!(repository.worktrees.len(), 2);
        }
        // 2 repositories + 4 working trees + 4 resources + machine + history.
        assert_eq!(projection.subjects.len(), 12);
        assert!(!projection.subjects.is_empty());

        // And every working tree resolves to its own root, so the four
        // references are four different places rather than one repeated.
        let roots: BTreeSet<PathBuf> = issued
            .iter()
            .filter_map(|reference| match projection.subjects.resolve(reference) {
                Some(ProbeSubject::Worktree { root }) => Some(root.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(roots.len(), 4, "{roots:?}");
    }

    // ---------------------------------------------------------------
    // Unknown is not false
    // ---------------------------------------------------------------

    /// Every unavailable probe arrives as a `status`/`unavailable_reason`
    /// pair and never as a zero, a false or an absent key. This is what
    /// stops "the probe failed" being read as "the answer is no".
    #[test]
    fn an_unavailable_probe_projects_its_reason_and_never_a_value() {
        let mut c = candidate("/w/a/target", in_repo("/w/a", "/w/.git"));
        c.0.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);
        c.0.last_modified = ProbeOutcome::Unavailable(ProbeReason::TimedOut);
        c.0.tool_liveness = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);

        let projection = project(&[c]);
        let resource = &projection.view.repositories[0].worktrees[0].resources[0];

        assert_eq!(resource.reclaimable_bytes.status, "unavailable");
        assert_eq!(resource.reclaimable_bytes.value, None);
        assert_eq!(
            resource.reclaimable_bytes.unavailable_reason,
            Some("permission_denied")
        );
        assert_eq!(resource.age_days.unavailable_reason, Some("timed_out"));
        assert_eq!(
            resource.tool_liveness.unavailable_reason,
            Some("tool_absent")
        );

        // The keys are present in the wire form too — an absent key would
        // leave a reader guessing between zero, false and unknown.
        let body = serde_json::to_value(&projection.view).expect("serializes");
        let wire = &body["repositories"][0]["worktrees"][0]["resources"][0];
        assert_eq!(wire["reclaimable_bytes"]["status"], "unavailable");
        assert!(wire["reclaimable_bytes"]["value"].is_null());
        assert_eq!(
            wire["reclaimable_bytes"]["unavailable_reason"],
            "permission_denied"
        );

        // Positive control: an observed value projects the other way round.
        let observed = project(&[candidate("/w/a/target", in_repo("/w/a", "/w/.git"))]);
        let ok = &observed.view.repositories[0].worktrees[0].resources[0];
        assert_eq!(ok.reclaimable_bytes.status, "observed");
        assert_eq!(ok.reclaimable_bytes.value, Some(4096));
        assert_eq!(ok.reclaimable_bytes.unavailable_reason, None);
    }

    /// An unread branch probe must not project as a branch with nothing in
    /// it: `unique_work` is `"unknown"`, and every branch-derived number
    /// says why it is missing.
    #[test]
    fn an_unread_branch_probe_projects_unknown_not_absent() {
        let projection = project(&[candidate("/w/a/target", in_repo("/w/a", "/w/.git"))]);
        let branch = &projection.view.repositories[0].worktrees[0].branch;

        assert_eq!(branch.unique_work, "unknown");
        assert_eq!(
            branch.upstream_state.unavailable_reason,
            Some("not_attempted")
        );
        assert_eq!(
            branch.merged_state.unavailable_reason,
            Some("not_attempted")
        );
        assert_eq!(
            branch.detached_head.unavailable_reason,
            Some("not_attempted")
        );
        assert_eq!(branch.commits_ahead_of_upstream.value, None);
    }

    /// A branch with no upstream has no ahead/behind counts — the counts are
    /// unavailable rather than zero, because zero would say "nothing
    /// unpushed" about a branch with no published counterpart at all.
    #[test]
    fn an_untracked_branch_has_no_counts_rather_than_zero_counts() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].lifecycle.branch =
            ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("wip".to_string()),
                upstream: UpstreamState::Untracked,
                merged: MergedState::Unknown,
                integration: IntegrationEvidence::not_attempted(),
            });
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let branch = &projection.view.repositories[0].worktrees[0].branch;

        assert_eq!(branch.upstream_state.value, Some("untracked"));
        assert_eq!(branch.commits_ahead_of_upstream.status, "unavailable");
        assert_eq!(branch.commits_behind_upstream.status, "unavailable");
        assert_eq!(
            branch.unique_work, "present",
            "no upstream means the tree may hold work nothing else has"
        );
        assert_eq!(
            branch.merge_comparison_known.value,
            Some(false),
            "there was no axis to compare against, which is not the same as \
             'not merged'"
        );
    }

    /// The branch NAME and the merge axis never leave, but the shape does.
    #[test]
    fn branch_identity_stays_local_while_its_shape_is_projected() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].lifecycle.branch =
            ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("v0.0.1/ACME-4242/feat/secret_product_name".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 2,
                    behind: 1,
                },
                merged: MergedState::NotMerged {
                    into: "origin/acme-release-train".to_string(),
                },
                integration: IntegrationEvidence::not_attempted(),
            });
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let body = json(&projection);

        assert!(!body.contains("ACME-4242"));
        assert!(!body.contains("secret_product_name"));
        assert!(!body.contains("acme-release-train"));

        let branch = &projection.view.repositories[0].worktrees[0].branch;
        assert_eq!(branch.upstream_state.value, Some("tracking"));
        assert_eq!(branch.commits_ahead_of_upstream.value, Some(2));
        assert_eq!(branch.commits_behind_upstream.value, Some(1));
        assert_eq!(branch.merged_state.value, Some("not_merged"));
        assert_eq!(branch.merge_comparison_known.value, Some(true));
        assert_eq!(branch.detached_head.value, Some(false));
        assert_eq!(branch.unique_work, "present");
    }

    /// The integration evidence projects as counts, a vocabulary token and
    /// two ages — and the commit ids, branch names and dates it was computed
    /// from stay here (HORO-1545).
    #[test]
    fn integration_evidence_projects_shapes_and_keeps_its_identities() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].lifecycle.branch =
            ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("v0.0.1/ACME-9142/feat/unreleased".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 3,
                    behind: 0,
                },
                merged: MergedState::NotMerged {
                    into: "origin/acme-release-train".to_string(),
                },
                integration: IntegrationEvidence {
                    divergence: ProbeOutcome::Observed(Divergence {
                        unique_commits: 2,
                        equivalent_commits: 1,
                        unclassified_commits: 0,
                    }),
                    equivalence: PatchEquivalence::Equivalent(EquivalenceMethod::ContentIdentical),
                    tip_committed_at: ProbeOutcome::Observed(at(86_400 * 5)),
                    comparison_tip_committed_at: ProbeOutcome::Observed(at(86_400 * 2)),
                },
            });
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 9),
        );
        let body = json(&projection);

        assert!(!body.contains("ACME-9142"));
        assert!(!body.contains("acme-release-train"));
        assert!(
            !body.contains("unreleased"),
            "the branch name is not a shape"
        );

        let branch = &projection.view.repositories[0].worktrees[0].branch;
        assert_eq!(
            branch.integration.patch_equivalence.value,
            Some("equivalent")
        );
        assert_eq!(
            branch.integration.equivalence_method.value,
            Some("content_identical"),
            "the two methods are not equally strong, so which one answered is sent"
        );
        assert_eq!(branch.integration.commits_unique_to_head.value, Some(2));
        assert_eq!(
            branch.integration.commits_equivalent_elsewhere.value,
            Some(1)
        );
        assert_eq!(branch.integration.commits_unclassified.value, Some(0));
        assert_eq!(branch.integration.head_tip_age_days.value, Some(4));
        assert_eq!(branch.integration.comparison_tip_age_days.value, Some(7));
        assert_eq!(
            branch.unique_work, "present",
            "equivalence is not an answer to whether this tree holds unique work"
        );
    }

    /// An unknown equivalence carries the reason it is unknown rather than
    /// projecting as an observed `"unknown"` token.
    ///
    /// The distinction is the whole campaign: a model shown
    /// `patch_equivalence: "unknown"` alongside `status: "observed"` has been
    /// told a probe answered, and `"unknown"` then reads as a weak
    /// `"not_equivalent"`. Reported as unavailable, the reason is there to be
    /// read and the absence cannot be mistaken for a finding.
    #[test]
    fn an_unknown_equivalence_reports_why_rather_than_a_token() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].lifecycle.branch =
            ProbeOutcome::Observed(WorktreeBranchState {
                branch: Some("wip".to_string()),
                upstream: UpstreamState::Tracking {
                    ahead: 1,
                    behind: 0,
                },
                merged: MergedState::NotMerged {
                    into: "origin/main".to_string(),
                },
                integration: IntegrationEvidence {
                    // The shape a branch sitting on an unseen merge commit
                    // produces: one commit ahead, and the per-commit method
                    // declined to describe it.
                    divergence: ProbeOutcome::Observed(Divergence {
                        unique_commits: 0,
                        equivalent_commits: 0,
                        unclassified_commits: 1,
                    }),
                    equivalence: PatchEquivalence::Unknown(ProbeReason::NotAttempted),
                    tip_committed_at: ProbeOutcome::Unavailable(ProbeReason::TimedOut),
                    comparison_tip_committed_at: ProbeOutcome::Observed(at(0)),
                },
            });
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 3),
        );
        let branch = &projection.view.repositories[0].worktrees[0].branch;

        assert_eq!(branch.integration.patch_equivalence.status, "unavailable");
        assert_eq!(branch.integration.patch_equivalence.value, None);
        assert_eq!(
            branch.integration.patch_equivalence.unavailable_reason,
            Some("not_attempted")
        );
        assert_eq!(branch.integration.equivalence_method.status, "unavailable");
        assert_eq!(
            branch.integration.commits_unclassified.value,
            Some(1),
            "the count the per-commit method declined to explain is still sent, \
             so nothing reads as an absence of unique work"
        );
        assert_eq!(
            branch.integration.head_tip_age_days.unavailable_reason,
            Some("timed_out")
        );
        assert_eq!(branch.integration.comparison_tip_age_days.value, Some(3));
    }

    /// A commit dated after this machine's clock has no age. Zero would read
    /// as "written moments ago", which is the flattering half of a
    /// subtraction that has no answer.
    #[test]
    fn a_commit_newer_than_the_clock_has_no_age_rather_than_zero() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].lifecycle.branch =
            ProbeOutcome::Observed(WorktreeBranchState {
                branch: None,
                upstream: UpstreamState::Unknown,
                merged: MergedState::Unknown,
                integration: IntegrationEvidence {
                    divergence: ProbeOutcome::Unavailable(ProbeReason::Failed),
                    equivalence: PatchEquivalence::NotApplicable,
                    tip_committed_at: ProbeOutcome::Observed(at(86_400 * 30)),
                    comparison_tip_committed_at: ProbeOutcome::Observed(at(0)),
                },
            });
        let projection =
            GraphProjection::build(&graph, &candidates, &ActionRegistry::builtin(), at(86_400));
        let branch = &projection.view.repositories[0].worktrees[0].branch;

        assert_eq!(branch.integration.head_tip_age_days.status, "unavailable");
        assert_eq!(
            branch.integration.head_tip_age_days.unavailable_reason,
            Some("failed")
        );
        // Positive control: the same projection, one day of it subtractable.
        assert_eq!(branch.integration.comparison_tip_age_days.value, Some(1));
        assert_eq!(
            branch.integration.patch_equivalence.value,
            Some("not_applicable"),
            "ancestry left no question to ask, which is not an answer of no"
        );
        assert_eq!(
            branch.integration.commits_unique_to_head.unavailable_reason,
            Some("failed")
        );
    }

    /// Process identities stay local; only counts are projected. A pid does
    /// not help a ranking model and a command line can name an internal
    /// tool.
    #[test]
    fn process_identities_stay_local_and_only_counts_are_projected() {
        let mut c = candidate("/w/a/target", in_repo("/w/a", "/w/.git"));
        c.0.open_by_process = ProbeOutcome::Observed(vec![crate::evidence::ProcessRef {
            pid: 31337,
            command: "/opt/acme/bin/acme-internal-builder".to_string(),
        }]);
        let projection = project(&[c]);
        let body = json(&projection);

        assert!(!body.contains("31337"));
        assert!(!body.contains("acme-internal-builder"));

        let activity = &projection.view.repositories[0].worktrees[0].activity;
        assert_eq!(activity.state, "in_use");
        assert_eq!(activity.observed_process_count, 1);
        assert_eq!(activity.unanswered_probe_count, 0);
    }

    /// "The provider answered and there is no pull request" and "the
    /// provider could not be reached" project to different shapes, so no
    /// prompt wording is needed to keep them apart.
    #[test]
    fn none_observed_and_provider_unavailable_project_differently() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].external.pull_request = ExternalFact::observed(
            ExternalSource::GitHubPullRequests,
            PullRequestState::NoneObserved,
            at(86_400 * 5),
        );
        graph.repositories[0].worktrees[0].external.task =
            ExternalFact::unavailable(ExternalSource::JiraIssues, ProbeReason::TimedOut);
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let worktree = &projection.view.repositories[0].worktrees[0];

        assert_eq!(worktree.pull_request.state.status, "observed");
        assert_eq!(worktree.pull_request.state.value, Some("none_observed"));
        assert_eq!(worktree.pull_request.observed_age_days.value, Some(2));

        assert_eq!(worktree.task.state.status, "unavailable");
        assert_eq!(worktree.task.state.unavailable_reason, Some("timed_out"));
        assert_eq!(
            worktree.task.observed_age_days.status, "unavailable",
            "a fact nobody observed has no age to be fresh or stale"
        );
        assert_eq!(worktree.task.source, "jira_issues");
    }

    /// A pull request state and a task state are shown, but never their
    /// titles, keys or bodies.
    #[test]
    fn external_context_projects_state_without_identity() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        graph.repositories[0].worktrees[0].external.task = ExternalFact::observed(
            ExternalSource::JiraIssues,
            TaskState::Other("Acme Internal Review".to_string()),
            at(86_400 * 6),
        );
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let body = json(&projection);

        assert!(!body.contains("Acme Internal Review"));
        assert_eq!(
            projection.view.repositories[0].worktrees[0]
                .task
                .state
                .value,
            Some("other"),
            "a tracker's own status name is reduced to the fact that it is \
             not one of the three states this product models"
        );
    }

    // ---------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------

    /// The offered ids come from `actionability` and from nowhere else, and
    /// the assertion is non-vacuous because the fixture is a real cargo
    /// project the action can actually plan for.
    #[test]
    fn offered_action_ids_are_exactly_what_actionability_reports() {
        let target = temp_cargo_project("offers");
        let target_str = target.to_str().expect("utf-8").to_string();
        let candidates = vec![candidate(&target_str, ProbeOutcome::Observed(None))];
        let actions = ActionRegistry::builtin();

        let expected =
            crate::actionability::eligible_action_ids(&candidates[0].0, &candidates[0].1, &actions);
        assert!(
            !expected.is_empty(),
            "the fixture must actually offer something or this test proves \
             nothing"
        );

        let projection = project(&candidates);
        assert_eq!(
            projection.view.global_resources[0].offered_action_ids,
            expected
        );
    }

    /// A resource the graph knows about but no candidate covers offers
    /// nothing. The safe direction: a missing join can only remove options.
    #[test]
    fn a_resource_with_no_candidate_offers_nothing() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let graph = graph_of(&candidates);
        let projection =
            GraphProjection::build(&graph, &[], &ActionRegistry::builtin(), at(86_400 * 7));
        assert!(projection.view.repositories[0].worktrees[0].resources[0]
            .offered_action_ids
            .is_empty());
    }

    // ---------------------------------------------------------------
    // Structure
    // ---------------------------------------------------------------

    /// Every resource in the graph gets exactly one alias, across all three
    /// buckets. A resource the projection silently dropped could never be
    /// recommended, and one aliased twice could be recommended twice.
    #[test]
    fn every_graph_resource_gets_exactly_one_alias() {
        let candidates = vec![
            candidate("/w/a/target", in_repo("/w/a", "/w/.git")),
            candidate("/w/b/target", in_repo("/w/b", "/w/.git")),
            candidate("/other/target", in_repo("/other", "/other/.git")),
            candidate("/cache/registry", ProbeOutcome::Observed(None)),
            candidate(
                "/lost/target",
                ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            ),
        ];
        let graph = graph_of(&candidates);
        let projection = project(&candidates);

        assert_eq!(projection.aliases.len(), graph.resource_count());
        let issued: Vec<&str> = projection.aliases.issued().collect();
        assert_eq!(
            issued,
            vec![
                "resource_1",
                "resource_2",
                "resource_3",
                "resource_4",
                "resource_5"
            ]
        );
        for alias in &issued {
            assert!(projection.aliases.resolve(alias).is_some());
        }

        assert_eq!(projection.view.repositories.len(), 2);
        assert_eq!(projection.view.global_resources.len(), 1);
        assert_eq!(projection.view.unplaced_resources.len(), 1);
        assert_eq!(
            projection.view.unplaced_resources[0].unplaced_reason, "timed_out",
            "the model is told the repository is unknown because a probe \
             timed out, not that there is no repository"
        );
    }

    /// Every reference in the payload is citable, and nothing else is.
    ///
    /// HORO-1548. The two halves matter for different reasons. Completeness:
    /// a resource in the global or unplaced list is as citable as one inside a
    /// worktree, and an implementation that walked only `repositories` would
    /// discard a correct citation from a model — which then reads as the model
    /// having made the reference up. Closure: `repo_9` and `resource_9` are not
    /// in a payload with two repositories and five resources, so a response
    /// naming them has cited nothing and validation must be able to say so.
    #[test]
    fn issued_evidence_refs_are_exactly_what_the_payload_carries() {
        let candidates = vec![
            candidate("/w/a/target", in_repo("/w/a", "/w/.git")),
            candidate("/w/b/target", in_repo("/w/b", "/w/.git")),
            candidate("/other/target", in_repo("/other", "/other/.git")),
            candidate("/cache/registry", ProbeOutcome::Observed(None)),
            candidate(
                "/lost/target",
                ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            ),
        ];
        let projection = project(&candidates);
        let refs = projection.issued_evidence_refs();

        for expected in [
            MACHINE_EVIDENCE_REF,
            WORKFLOW_HISTORY_EVIDENCE_REF,
            "repo_1",
            "repo_2",
            "workspace_1",
            "workspace_2",
            "workspace_3",
            "resource_1",
            "resource_2",
            "resource_3",
            // The global cache and the unplaced resource. Neither sits under a
            // repository, and both were sent.
            "resource_4",
            "resource_5",
        ] {
            assert!(refs.contains(expected), "{expected} is not citable");
        }

        for never_issued in [
            "repo_3",
            "workspace_4",
            "resource_6",
            "resource_9",
            "machine_1",
            "workflow_history_1",
            "",
        ] {
            assert!(
                !refs.contains(never_issued),
                "{never_issued} is citable but was never sent"
            );
        }

        // Nothing beyond the machine, the baseline, 2 repositories,
        // 3 worktrees and 5 resources.
        assert_eq!(refs.len(), 2 + 2 + 3 + 5);
    }

    /// HORO-1548. Every resource alias the payload carried can be looked back
    /// up — including the two that sit outside any repository, which is where
    /// a traversal that only walked the repository tree would quietly return
    /// `None` and make a legitimate item look invented.
    #[test]
    fn every_issued_resource_alias_resolves_to_the_view_that_was_sent() {
        let candidates = vec![
            candidate("/w/a/target", in_repo("/w/a", "/w/.git")),
            candidate("/cache/registry", ProbeOutcome::Observed(None)),
            candidate(
                "/lost/target",
                ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            ),
        ];
        let projection = project(&candidates);

        for alias in ["resource_1", "resource_2", "resource_3"] {
            let view = projection
                .resource_view(alias)
                .unwrap_or_else(|| panic!("{alias} was sent but does not resolve"));
            assert_eq!(view.evidence_ref, alias);
        }

        // A resource alias is the only kind this answers for. The other
        // citable references name things that are not resources, and an
        // action id is never on offer for one.
        for not_a_resource in [
            "resource_4",
            "resource_0",
            "workspace_1",
            "repo_1",
            MACHINE_EVIDENCE_REF,
            WORKFLOW_HISTORY_EVIDENCE_REF,
            "",
        ] {
            assert!(
                projection.resource_view(not_a_resource).is_none(),
                "{not_a_resource} resolved to a resource view"
            );
        }
    }

    /// HORO-1561 AC 5. A tool-owned resource is projected as the global cache
    /// it is, so the model is never handed an `unplaced_reason` of
    /// `"not_attempted"` about a probe that had nothing to run against — and
    /// the path-located resource beside it, which really was left unprobed,
    /// still carries its reason.
    #[test]
    fn a_tool_owned_resource_is_projected_with_no_unplaced_reason() {
        let candidates = vec![
            tool_candidate(ResourceKind::DockerVolume, "pgdata"),
            candidate(
                "/lost/target",
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            ),
        ];
        let projection = project(&candidates);

        assert_eq!(projection.view.global_resources.len(), 1);
        let reasons: Vec<&str> = projection
            .view
            .unplaced_resources
            .iter()
            .map(|view| view.unplaced_reason)
            .collect();
        assert_eq!(
            reasons,
            vec!["not_attempted"],
            "exactly one resource here has an unresolved placement, and it is \
             the one with a path"
        );

        // And from the serialized payload's side, since that is what the model
        // actually reads: one unplaced entry, not two.
        let payload: serde_json::Value =
            serde_json::from_str(&json(&projection)).expect("the projection serializes");
        assert_eq!(
            payload["unplaced_resources"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            payload["global_resources"].as_array().map(Vec::len),
            Some(1)
        );
    }

    /// HORO-1544. The three axes reach the model, the referrer *count* reaches
    /// it, and the referring container's identity does not.
    ///
    /// `not_docker` is the half that would otherwise go unnoticed: a Cargo
    /// target directory must project `null` rather than an all-unknown
    /// lifecycle, because "nobody established this image's activity" and "this
    /// is not a Docker object" are different statements and only one of them is
    /// a gap.
    #[test]
    fn docker_lifecycle_projects_its_axes_and_counts_but_no_identity() {
        let mut docker = tool_candidate(ResourceKind::DockerImage, "sha256:abc");
        docker.0.docker_lifecycle = Some(DockerLifecycle {
            activity: crate::evidence::DockerActivity::Active,
            persistence: crate::evidence::DockerPersistence::UserManaged,
            references: ProbeOutcome::Observed(crate::evidence::DockerReferences {
                referenced_by: vec![
                    ResourceId::new(
                        ResourceKind::DockerContainer,
                        ResourceLocator::Tool {
                            tool: crate::evidence::OwningTool::Docker,
                            id: "acme-billing-replica".to_string(),
                        },
                    ),
                    ResourceId::new(
                        ResourceKind::DockerContainer,
                        ResourceLocator::Tool {
                            tool: crate::evidence::OwningTool::Docker,
                            id: "acme-billing-worker".to_string(),
                        },
                    ),
                ],
                active_referrers: 1,
            }),
        });
        let candidates = vec![
            docker,
            candidate("/w/a/target", ProbeOutcome::Observed(None)),
        ];
        let projection = project(&candidates);
        let serialized = json(&projection);

        let lifecycle = projection
            .view
            .global_resources
            .iter()
            .find_map(|view| view.docker_lifecycle.clone())
            .expect("the Docker object projects a lifecycle");
        assert_eq!(lifecycle.activity, "active");
        assert_eq!(lifecycle.persistence, "user_managed");
        assert_eq!(lifecycle.referrer_count.value, Some(2));
        assert_eq!(lifecycle.active_referrers.value, Some(1));

        assert!(
            !serialized.contains("acme-billing"),
            "a referring container's identity reached the payload: {serialized}"
        );

        let not_docker = projection
            .view
            .global_resources
            .iter()
            .find(|view| view.kind == "cargo_target_dir")
            .expect("the cargo resource is global");
        assert!(
            not_docker.docker_lifecycle.is_none(),
            "a non-Docker resource has no Docker lifecycle to be unknown about"
        );
    }

    /// The anti-vacuity half. An unanswered reference query must not arrive as
    /// an observed zero — "nothing references this image" is deletion-shaped
    /// evidence, and it is exactly what nobody established here.
    #[test]
    fn an_unanswered_reference_query_is_not_a_referrer_count_of_zero() {
        let mut docker = tool_candidate(ResourceKind::DockerImage, "sha256:abc");
        docker.0.docker_lifecycle = Some(DockerLifecycle::unknown(ProbeReason::ToolNotRunning));
        let projection = project(&[docker]);

        let lifecycle = projection
            .view
            .global_resources
            .iter()
            .find_map(|view| view.docker_lifecycle.clone())
            .expect("the Docker object projects a lifecycle");

        assert_eq!(lifecycle.referrer_count.status, "unavailable");
        assert_eq!(lifecycle.referrer_count.value, None);
        assert_eq!(
            lifecycle.referrer_count.unavailable_reason,
            Some("tool_not_running")
        );
        assert_eq!(lifecycle.active_referrers.status, "unavailable");
        assert_eq!(lifecycle.activity, "unknown");
        assert_ne!(
            lifecycle.activity,
            crate::evidence::DockerActivity::Inactive.tag(),
            "unknown activity must not be spelled like an observed idle"
        );
    }

    /// The projection is deterministic for one graph, because the alias a
    /// response cites has to mean the same thing the request meant.
    #[test]
    fn projecting_one_graph_twice_produces_one_payload() {
        let candidates = vec![
            candidate("/w/b/target", in_repo("/w/b", "/w/.git")),
            candidate("/w/a/target", in_repo("/w/a", "/w/.git")),
            candidate("/cache/registry", ProbeOutcome::Observed(None)),
        ];
        let first = project(&candidates);
        let second = project(&candidates);
        assert_eq!(first.view, second.view);
        assert_eq!(json(&first), json(&second));
    }

    /// A collected baseline over too few observations reports `"unknown"`
    /// with `"insufficient"` confidence. A baseline that was never collected
    /// reports `unavailable`. Those are different facts and the projection
    /// keeps them apart.
    #[test]
    fn an_uncollected_baseline_is_not_an_unknown_workflow_mode() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];

        let never_collected = project(&candidates);
        let history = &never_collected.view.workflow_history;
        assert_eq!(history.mode.status, "unavailable");
        assert_eq!(history.mode.unavailable_reason, Some("not_attempted"));
        assert_eq!(history.observation_count.value, None);

        let mut graph = graph_of(&candidates);
        graph.history = ProbeOutcome::Observed(WorkflowHistorySummary::from_observations(
            WorkflowMode::ParallelMultiWorktree,
            1,
            WorkflowSupport::nothing_observed(),
        ));
        let too_few = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let history = &too_few.view.workflow_history;
        assert_eq!(history.mode.status, "observed");
        assert_eq!(history.mode.value, Some("unknown"));
        assert_eq!(history.confidence.value, Some("insufficient"));
        assert_eq!(history.observation_count.value, Some(1));

        graph.history = ProbeOutcome::Observed(WorkflowHistorySummary::from_observations(
            WorkflowMode::ParallelMultiWorktree,
            12,
            WorkflowSupport::nothing_observed(),
        ));
        let settled = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        assert_eq!(
            settled.view.workflow_history.mode.value,
            Some("parallel_multi_worktree")
        );
        assert_eq!(
            settled.view.workflow_history.confidence.value,
            Some("observed")
        );
    }

    /// No goal set is a complete answer and projects as `null`; the two disk
    /// measurements project as `Reported` because a probe can fail to make
    /// them.
    #[test]
    fn machine_context_separates_no_goal_from_no_measurement() {
        let candidates = vec![candidate("/w/a/target", in_repo("/w/a", "/w/.git"))];
        let mut graph = graph_of(&candidates);
        let projection = project(&candidates);
        assert_eq!(projection.view.machine.recovery_goal_bytes, None);
        assert_eq!(projection.view.machine.free_bytes.status, "unavailable");
        assert_eq!(projection.view.machine.evidence_ref, "machine");

        graph.machine = MachineContext {
            free_bytes: ProbeOutcome::Observed(12_000),
            total_bytes: ProbeOutcome::Unavailable(ProbeReason::Failed),
            recovery_goal_bytes: Some(50_000),
        };
        let measured = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        assert_eq!(measured.view.machine.free_bytes.value, Some(12_000));
        assert_eq!(
            measured.view.machine.total_bytes.unavailable_reason,
            Some("failed")
        );
        assert_eq!(measured.view.machine.recovery_goal_bytes, Some(50_000));
    }
}
