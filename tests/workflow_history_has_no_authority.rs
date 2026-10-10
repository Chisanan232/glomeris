//! The workflow baseline changes nothing about what may be done (HORO-1547
//! AC6).
//!
//! `scripts/check-workspace-aggregation-has-no-authority.sh` already forbids
//! `src/policy`, `src/executor`, `src/autopilot` and `src/actions` from naming
//! `crate::workspace` at all, which is the structural half: nothing in the
//! deciding or mutating layers can read a baseline because it cannot reach the
//! module that holds one. This is the observable half, one layer further out —
//! the payload a planner is shown.
//!
//! # The defect this is shaped against
//!
//! The baseline exists to make a recommendation better ordered and better
//! explained. The tempting next step is the one that makes it authority: a
//! machine whose baseline says `parallel_multi_worktree` has, by that reading,
//! a developer who keeps spare working trees around, so an old one is
//! presumably finished with. Written as code that is a nudge to
//! `policy_label`, or a widened `offered_action_ids`, or a `completeness` that
//! reads as settled — each one a plausible small improvement, and each one
//! converts "how this person works" into permission over a specific directory
//! on today's disk.
//!
//! So the assertion is byte-level and total: the same candidates, projected
//! with a settled parallel baseline and with none at all, must produce payloads
//! that differ *only* inside `workflow_history`. Not "differ acceptably" —
//! differ nowhere else. Anything the baseline is allowed to change is something
//! somebody has to come here and permit.
//!
//! # Why the fixture is the hard case
//!
//! One candidate is dirty, has untracked files, is open by a process, and sits
//! in a worktree whose branch is ahead of its upstream and not merged: every
//! axis that makes a directory unsafe to touch, present at once. The other has
//! none of that, so it is offered an action — without it the `offered_action_ids`
//! comparison would be comparing two empty lists, because a protected resource
//! is offered nothing.
//!
//! The baseline they are paired with is the most suggestive one available:
//! fourteen observations spanning nineteen days, every one of them parallel,
//! nine working trees of each of three repositories seen at once, so
//! `HistoryConfidence` is `observed`. If a baseline could ever move a verdict,
//! this is the pairing where it would.
//!
//! Checked by mutation while it was written: making `resource_view` report
//! `ASK_USER` instead of `PROTECTED` when the baseline says
//! `parallel_multi_worktree` — the plausible version of "this person keeps
//! spare working trees, so ask rather than refuse" — fails both tests here,
//! naming the moved label.
//!
//! # The anti-vacuity half
//!
//! A test asserting two payloads are equal outside one key passes trivially if
//! the two payloads are equal *everywhere* — which is what a `with_history`
//! that silently dropped its argument would produce. So it first asserts the
//! two payloads do differ inside `workflow_history`, and that the one built
//! with a baseline reports the parallel mode and the counts behind it. The
//! baseline has to have arrived for its absence from everywhere else to mean
//! anything.

use glomeris::actions::ActionRegistry;
use glomeris::evidence::{
    Evidence, GitState, NativeCleanup, ProbeOutcome, ProbeReason, ProcessRef, Recoverability,
    ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::planner::GraphProjection;
use glomeris::policy::{PolicyClass, PolicyDecision, ReasonCode};
use glomeris::workspace::history::{
    LocalAlias, RepositoryObservation, StoreState, WorkspaceObservation,
};
use glomeris::workspace::{
    IntegrationEvidence, MachineContext, MergedState, UpstreamState, WorkspaceEvidenceGraph,
    WorkspaceSurvey, WorktreeBranchState,
};

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const NOW_SECS: u64 = 86_400 * 30;
const BRANCH_NAME: &str = "feature/acme-invoice-export";

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn collected_at() -> SystemTime {
    at(NOW_SECS)
}

/// A real cargo project on disk, because `offered_action_ids` comes from
/// `actionability::eligible_action_ids`, which plans the action, and planning
/// `cargo.clean.target_dir` stats `Cargo.toml`. A fabricated path yields an
/// empty list on both sides of the comparison, and two empty lists are equal
/// for a reason that has nothing to do with the baseline.
fn temp_cargo_project() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock at or after UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "horo1547-authority-{}-{nanos}-{n}",
        std::process::id()
    ));
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create the temp target directory");
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fixture\"\n")
        .expect("write the temp manifest");
    target
}

/// A resource with every reason to be left alone: dirty tree, untracked files,
/// a process holding it open.
fn protected_candidate(target: &Path) -> (Evidence, PolicyDecision) {
    candidate_for(target, true)
}

/// A resource with nothing against it: clean tree, nothing holding it open.
///
/// Present so `offered_action_ids` is non-empty on both sides of the
/// comparison. A protected resource is offered nothing, and two empty lists are
/// equal for a reason that has nothing to do with the baseline — so without
/// this candidate the offers half of the comparison would be watching nothing.
fn reclaimable_candidate(target: &Path) -> (Evidence, PolicyDecision) {
    candidate_for(target, false)
}

fn candidate_for(target: &Path, in_use: bool) -> (Evidence, PolicyDecision) {
    let repo_root = target
        .parent()
        .expect("the target has a parent")
        .to_path_buf();
    let resource = ResourceId::new(
        ResourceKind::CargoTargetDir,
        ResourceLocator::Path(target.to_path_buf()),
    );
    let evidence = Evidence {
        resource: resource.clone(),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: glomeris::detectors::DetectorId("cargo.target_dir"),
        logical_bytes: ProbeOutcome::Observed(9_000_000),
        physical_bytes: Some(9_000_000),
        reclaimable_bytes: ProbeOutcome::Observed(9_000_000),
        reclaimable_bytes_is_lower_bound: false,
        last_modified: ProbeOutcome::Observed(at(86_400 * 29)),
        last_accessed: ProbeOutcome::Observed(at(86_400 * 29)),
        regenerability: ResourceKind::CargoTargetDir.regenerability(),
        recoverability: Recoverability::RegenerableByRebuild,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Observed(if in_use {
            vec![ProcessRef {
                pid: 4242,
                command: "cargo build".to_string(),
            }]
        } else {
            Vec::new()
        }),
        process_cwd_match: ProbeOutcome::Observed(Vec::new()),
        git_state: ProbeOutcome::Observed(Some(GitState {
            repo_root: repo_root.clone(),
            common_dir: repo_root.join(".git"),
            dirty: in_use,
            untracked: in_use,
            worktree: true,
        })),
        tool_liveness: ProbeOutcome::Observed(false),
        docker_lifecycle: None,
        executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        collected_at: collected_at(),
        sources: Vec::new(),
    };
    // Whatever policy decided, it decided without a baseline: `policy::classify`
    // cannot reach `crate::workspace`. Recorded here as Protected because that
    // is the verdict this fixture's evidence earns, and the point is that it
    // survives.
    let decision = PolicyDecision {
        resource,
        class: if in_use {
            PolicyClass::Protected
        } else {
            PolicyClass::AutoSafe
        },
        reasons: if in_use {
            vec![
                ReasonCode::ResourceInActiveUse,
                ReasonCode::GitWorktreeDirty,
            ]
        } else {
            vec![ReasonCode::NoActiveUseObserved]
        },
        evidence_collected_at: collected_at(),
        evaluated_at: collected_at(),
        policy_version: 1,
    };
    (evidence, decision)
}

/// Fourteen parallel observations spanning nineteen days, nine working trees
/// of each of three repositories every time — the most confident, most
/// suggestive baseline the classifier can produce.
fn a_settled_parallel_baseline() -> StoreState {
    let day = 86_400;
    let observations = (0..14)
        .map(|i| WorkspaceObservation {
            at_unix_secs: NOW_SECS - (21 * day) + (i * 3 * day / 2),
            repositories: (0..3)
                .map(|r| RepositoryObservation {
                    repository: LocalAlias::of_name(&format!("repo-{r}")),
                    worktree_count: 9,
                    linked_worktree_count: 8,
                    detached_worktree_count: 0,
                    single_checkout_branch: None,
                })
                .collect(),
        })
        .collect();
    StoreState::Collected(observations)
}

/// The payload for these candidates, with the given baseline attached.
fn payload(candidates: &[(Evidence, PolicyDecision)], history: &StoreState) -> Value {
    let graph = WorkspaceEvidenceGraph::build(
        candidates,
        &WorkspaceSurvey::unsurveyed(),
        MachineContext {
            free_bytes: ProbeOutcome::Observed(11_000_000),
            total_bytes: ProbeOutcome::Observed(500_000_000),
            recovery_goal_bytes: Some(40_000_000),
        },
        collected_at(),
    );
    let mut graph = graph.with_history(history, NOW_SECS);

    // An unmerged branch ahead of its upstream, so the worktree carries local
    // work as well as local mess. Set on both sides identically.
    if let Some(worktree) = graph
        .repositories
        .first_mut()
        .and_then(|r| r.worktrees.first_mut())
    {
        worktree.lifecycle.branch = ProbeOutcome::Observed(WorktreeBranchState {
            branch: Some(BRANCH_NAME.to_string()),
            upstream: UpstreamState::Tracking {
                ahead: 4,
                behind: 0,
            },
            merged: MergedState::NotMerged {
                into: "origin/main".to_string(),
            },
            integration: IntegrationEvidence::not_attempted(),
        });
    }

    let projection = GraphProjection::build(
        &graph,
        candidates,
        &ActionRegistry::builtin(),
        collected_at(),
    );
    serde_json::to_value(&projection.view).expect("serialize the model view")
}

#[test]
fn workflow_history_cannot_make_a_resource_executable() {
    let target = temp_cargo_project();
    let reclaimable = temp_cargo_project();
    let candidates = vec![
        protected_candidate(&target),
        reclaimable_candidate(&reclaimable),
    ];

    let with = payload(&candidates, &a_settled_parallel_baseline());
    let without = payload(&candidates, &StoreState::NeverCollected);

    // Anti-vacuity first. If `with_history` dropped its argument, everything
    // below would hold over two identical payloads.
    assert_ne!(
        with.get("workflow_history"),
        without.get("workflow_history"),
        "the two payloads carry the same baseline, so nothing below is being \
         tested"
    );
    let history = with
        .get("workflow_history")
        .expect("the payload carries a workflow_history object");
    assert_eq!(
        history
            .get("mode")
            .and_then(|m| m.get("value"))
            .and_then(Value::as_str),
        Some("parallel_multi_worktree"),
        "the settled parallel baseline did not reach the payload: {history}"
    );
    assert_eq!(
        history
            .get("support")
            .and_then(|s| s.get("most_worktrees_seen_at_once"))
            .and_then(Value::as_u64),
        Some(9),
        "the counts behind the mode did not reach the payload: {history}"
    );
    assert_eq!(
        history
            .get("support")
            .and_then(|s| s.get("spanning_days"))
            .and_then(Value::as_u64),
        Some(19),
        "the baseline's span did not reach the payload: {history}"
    );

    // And the other side really is the absent baseline rather than a default
    // shape, which is the AC5 property this test depends on.
    let absent = without
        .get("workflow_history")
        .expect("the payload carries a workflow_history object");
    let absent_mode = absent.get("mode").expect("the view carries a mode");
    assert_eq!(
        absent_mode.get("value"),
        Some(&Value::Null),
        "a never-collected baseline produced a shape: {absent}"
    );
    assert_eq!(
        absent_mode
            .get("unavailable_reason")
            .and_then(Value::as_str),
        Some("not_attempted"),
        "a never-collected baseline did not say why it has no shape, so the \
         model could read its silence as a serial machine: {absent}"
    );

    // The assertion itself: everything outside `workflow_history` is identical.
    let mut stripped_with = with.clone();
    let mut stripped_without = without.clone();
    for value in [&mut stripped_with, &mut stripped_without] {
        value
            .as_object_mut()
            .expect("the model view is a JSON object")
            .remove("workflow_history");
    }
    assert_eq!(
        stripped_with, stripped_without,
        "the workflow baseline changed something outside workflow_history.\n\
         with a baseline:\n{stripped_with:#}\n\nwith none:\n{stripped_without:#}"
    );
}

#[test]
fn a_baseline_changes_no_resources_verdict_or_offered_actions() {
    let target = temp_cargo_project();
    let reclaimable = temp_cargo_project();
    let candidates = vec![
        protected_candidate(&target),
        reclaimable_candidate(&reclaimable),
    ];

    let with = payload(&candidates, &a_settled_parallel_baseline());
    let without = payload(&candidates, &StoreState::NeverCollected);

    // Named separately from the whole-payload comparison because these two
    // fields are the ones that decide what a user can be shown as safe, and a
    // future field this test has not thought about would be excluded from the
    // comparison above by somebody adding it to an ignore list. These are not
    // ignorable.
    let verdicts = |payload: &Value| -> Vec<(String, String, Vec<String>)> {
        let mut out = Vec::new();
        let mut collect = |resources: Option<&Value>| {
            for resource in resources.and_then(Value::as_array).unwrap_or(&Vec::new()) {
                out.push((
                    resource
                        .get("evidence_ref")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    resource
                        .get("policy_label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    resource
                        .get("offered_action_ids")
                        .and_then(Value::as_array)
                        .map(|ids| {
                            ids.iter()
                                .map(|id| id.as_str().unwrap_or_default().to_string())
                                .collect()
                        })
                        .unwrap_or_default(),
                ));
            }
        };
        for repository in payload
            .get("repositories")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            for worktree in repository
                .get("worktrees")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                collect(worktree.get("resources"));
            }
        }
        collect(payload.get("global_resources"));
        out
    };

    let with_verdicts = verdicts(&with);
    assert!(
        !with_verdicts.is_empty(),
        "no resource was found in the payload, so no verdict is being compared"
    );
    assert!(
        with_verdicts
            .iter()
            .any(|(_, label, _)| label == "PROTECTED"),
        "the fixture's protected resource is not reported protected, so this \
         test is not watching the verdict it claims to: {with_verdicts:?}"
    );
    assert!(
        with_verdicts
            .iter()
            .any(|(_, _, offers)| !offers.is_empty()),
        "no resource was offered an action, so comparing the offers is \
         comparing empty lists: {with_verdicts:?}"
    );
    assert_eq!(
        with_verdicts,
        verdicts(&without),
        "a workflow baseline changed a resource's verdict or the actions on \
         offer for it"
    );
}
