//! Turning the local graph into the bounded projection, and keeping the
//! alias table on this side of the wire.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::dto::{
    ActivityView, BranchView, ExternalFactView, MachineView, ModelGraphView, Reported,
    RepositoryView, ResourceView, UnplacedResourceView, WorkflowHistoryView, WorktreeView,
    MACHINE_EVIDENCE_REF, WORKFLOW_HISTORY_EVIDENCE_REF,
};
use crate::actions::llm::{completeness_tag, regenerability_tag};
use crate::actions::ActionRegistry;
use crate::evidence::{Evidence, ProbeOutcome, ResourceId};
use crate::policy::PolicyDecision;
use crate::workspace::{
    ActivityFacts, BranchLifecycle, ExternalFact, ResourceNode, UpstreamState,
    WorkspaceEvidenceGraph,
};

/// A projection plus the local table needed to read its answers back.
///
/// The two halves exist separately on purpose: `view` is serializable and
/// `aliases` is not, so there is no code path that sends both.
#[derive(Debug, Clone)]
pub struct GraphProjection {
    pub view: ModelGraphView,
    pub aliases: AliasTable,
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

/// Hands out `resource_N` / `workspace_N` / `repo_N` in one traversal, so
/// two callers cannot disagree about which alias belongs to what.
struct AliasIssuer {
    next_resource: usize,
    next_worktree: usize,
    next_repository: usize,
    table: AliasTable,
}

impl AliasIssuer {
    fn new() -> Self {
        Self {
            next_resource: 0,
            next_worktree: 0,
            next_repository: 0,
            table: AliasTable::default(),
        }
    }

    fn repository(&mut self) -> String {
        self.next_repository += 1;
        format!("repo_{}", self.next_repository)
    }

    fn worktree(&mut self) -> String {
        self.next_worktree += 1;
        format!("workspace_{}", self.next_worktree)
    }

    fn resource(&mut self, resource: &ResourceId) -> String {
        self.next_resource += 1;
        let alias = format!("resource_{}", self.next_resource);
        self.table.resources.push((alias.clone(), resource.clone()));
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
                let evidence_ref = issuer.repository();
                RepositoryView {
                    evidence_ref,
                    worktree_count: repo.worktree_count(),
                    worktrees: repo
                        .worktrees
                        .iter()
                        .map(|worktree| {
                            let evidence_ref = issuer.worktree();
                            WorktreeView {
                                evidence_ref,
                                linked: worktree.linked,
                                branch: branch_view(&worktree.lifecycle),
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
                workflow_history: workflow_history_view(graph),
            },
            aliases: issuer.table,
        }
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

fn workflow_history_view(graph: &WorkspaceEvidenceGraph) -> WorkflowHistoryView {
    match &graph.history {
        ProbeOutcome::Observed(summary) => WorkflowHistoryView {
            evidence_ref: WORKFLOW_HISTORY_EVIDENCE_REF,
            mode: Reported::observed(summary.mode.tag()),
            confidence: Reported::observed(summary.confidence.tag()),
            observation_count: Reported::observed(summary.observation_count),
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
        },
    }
}

fn branch_view(lifecycle: &BranchLifecycle) -> BranchView {
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
    }
}

fn activity_view(activity: &ActivityFacts) -> ActivityView {
    ActivityView {
        state: activity.state.tag(),
        observed_process_count: activity.observed_processes.len(),
        unanswered_probe_count: activity.unanswered_probes,
    }
}

fn external_view<T, F>(fact: &ExternalFact<T>, tag: F, now: SystemTime) -> ExternalFactView
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
        ExternalSource, MachineContext, MergedState, PullRequestState, TaskState,
        WorkflowHistorySummary, WorkflowMode, WorkspaceSurvey, WorktreeBranchState,
    };
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
