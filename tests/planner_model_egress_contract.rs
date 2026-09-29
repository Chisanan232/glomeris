//! The pinned egress contract for the workspace evidence graph (HORO-1542
//! AC6, AC9).
//!
//! `src/planner/dto.rs` is written so it cannot name a path type, and
//! `scripts/check-workspace-aggregation-has-no-authority.sh` enforces that
//! literally. This test covers the other half: the field *set*. A future
//! field of an already-allowed type — a `String` holding a branch name, a
//! `u32` holding a pid — would compile, pass the token guard, and quietly
//! widen what leaves this Mac. So every key path the projection can
//! serialize is pinned below by hand, and adding one anywhere in the DTO
//! fails this test until somebody writes it into the list and, in doing so,
//! decides that it may leave.
//!
//! Two fixtures are pinned against the same list, which is what makes the
//! pin complete rather than convenient. Both fill all three placement buckets
//! (in a repository, global, unplaced) and leave every collection non-empty,
//! so a field that only appears inside `unplaced_resources` is still covered.
//! One answers every probe and the other answers none — because a pin taken
//! only over answered data cannot see a field that vanishes when there is no
//! answer, and `#[serde(skip_serializing_if)]` on `Reported::value` is exactly
//! that: harmless-looking, byte-saving on the payloads where it matters, and
//! invisible to an all-observed fixture. Requiring both to produce the same
//! key set also pins the stronger property — the shape of a payload does not
//! depend on what was observed, so it cannot tell a provider anything by its
//! silences. `the_model_payload_key_set_is_pinned` asserts the converse too,
//! that the pin holds no path the projection cannot produce, so the list
//! cannot rot into a superset that would quietly accept a removed field's
//! replacement.
//!
//! The leak assertions run over the serialized bytes rather than the typed
//! view, because a field's *type* being `&'static str` says nothing about
//! whether the string it holds came from a path. The fixture's paths carry
//! [`ACCOUNT_MARKER`], its branch names carry a project name, and its
//! process command line names an internal tool; none of those may appear,
//! while `the_payload_projects_the_resources_whose_identity_it_withheld`
//! proves the payload was a projection of exactly those resources — and that
//! the alias table still holds the real identity locally — rather than an
//! empty object that trivially leaks nothing.

use glomeris::actions::ActionRegistry;
use glomeris::evidence::{
    DockerActivity, DockerLifecycle, DockerPersistence, DockerReferences, Evidence, GitState,
    NativeCleanup, OwningTool, ProbeOutcome, ProbeReason, ProcessRef, Recoverability,
    ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::planner::GraphProjection;
use glomeris::policy::{PolicyClass, PolicyDecision, ReasonCode};
use glomeris::workspace::{
    ExternalFact, ExternalSource, MachineContext, MergedState, PullRequestState, TaskState,
    UpstreamState, WorkflowHistorySummary, WorkflowMode, WorkspaceEvidenceGraph, WorkspaceSurvey,
    WorktreeBranchState,
};

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Stands in for the OS account name in `/Users/<account>/…`. Present in
/// every path this fixture builds, so an assertion that it never reaches the
/// payload is an assertion about real local identity rather than about a
/// string chosen to be absent.
const ACCOUNT_MARKER: &str = "glomeris-planner-egress-account";

/// A branch name shaped like the ones this campaign is developed on: a
/// ticket key and a product name, both of which identify work.
const BRANCH_NAME: &str = "v0.0.1/ACME-9142/feat/unreleased_product_codename";

/// The axis a merge was measured against. Identifying on its own — a release
/// train name says what a company ships.
const MERGE_AXIS: &str = "origin/acme-release-train-q3";

/// A command line that names an internal tool.
const PROCESS_COMMAND: &str = "/opt/acme/libexec/acme-internal-builder --watch";

const PROCESS_PID: u32 = 31337;

/// The Docker id of the object the fixture's image is referenced by. A
/// container name is chosen by a human or a compose file and routinely carries
/// a product or a customer name, so this one is shaped like the ones that do.
/// The local graph holds it; the payload may report that *something*
/// references the image and may not say what (HORO-1544).
const CONTAINER_MARKER: &str = "acme-billing-prod-replica";

/// The Docker id of the fixture's image. An image reference carries a registry
/// host and a repository path, which is the same class of identity as a branch
/// name, and it leaves this Mac for the same reason: it does not.
const IMAGE_MARKER: &str = "registry.acme.example/acme-billing/api:v9142";

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

/// Collection time, and the `now` the projection ages facts against.
fn collected_at() -> SystemTime {
    at(86_400 * 30)
}

/// A real cargo project on disk under a marked path.
///
/// Real because `offered_action_ids` comes from
/// `actionability::eligible_action_ids`, which plans the action, and planning
/// `cargo.clean.target_dir` stats `Cargo.toml` (HORO-1360). A fabricated path
/// yields an empty list, and an empty list would leave
/// `…offered_action_ids[]` out of the serialized key set — pinning a key set
/// that the fixture never exercises is how a guard silently stops guarding.
fn temp_cargo_project(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock at or after UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "{ACCOUNT_MARKER}-{label}-{}-{nanos}-{n}",
        std::process::id()
    ));
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create the temp target directory");
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fixture\"\n")
        .expect("write the temp manifest");
    target
}

/// One fully observed candidate. Every probe answered, so no field is
/// skipped for want of data.
fn candidate(
    target: &Path,
    git_state: ProbeOutcome<Option<GitState>>,
    processes: Vec<ProcessRef>,
) -> (Evidence, PolicyDecision) {
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
        logical_bytes: ProbeOutcome::Observed(8_192),
        physical_bytes: Some(8_192),
        reclaimable_bytes: ProbeOutcome::Observed(8_192),
        reclaimable_bytes_is_lower_bound: true,
        last_modified: ProbeOutcome::Observed(at(86_400 * 20)),
        last_accessed: ProbeOutcome::Observed(at(86_400 * 21)),
        regenerability: ResourceKind::CargoTargetDir.regenerability(),
        recoverability: Recoverability::RegenerableByRebuild,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Observed(processes),
        process_cwd_match: ProbeOutcome::Observed(Vec::new()),
        git_state,
        tool_liveness: ProbeOutcome::Observed(false),
        docker_lifecycle: None,
        collected_at: collected_at(),
        sources: Vec::new(),
    };
    let decision = PolicyDecision {
        resource,
        class: PolicyClass::AutoSafe,
        reasons: vec![ReasonCode::NoActiveUseObserved],
        evidence_collected_at: collected_at(),
        evaluated_at: collected_at(),
        policy_version: 1,
    };
    (evidence, decision)
}

/// One Docker object, as a real detector produces it: addressed by its own
/// id rather than by a path, so it is a global resource by construction
/// (HORO-1561) and carries lifecycle facts nothing else does.
///
/// `lifecycle` is the caller's so both fixtures can share this: one passes an
/// answered lifecycle, the other an unknown one, which is what pins the
/// `Reported` keys under `docker_lifecycle` in both directions.
fn docker_candidate(lifecycle: DockerLifecycle) -> (Evidence, PolicyDecision) {
    let resource = ResourceId::new(
        ResourceKind::DockerImage,
        ResourceLocator::Tool {
            tool: OwningTool::Docker,
            id: IMAGE_MARKER.to_string(),
        },
    );
    let evidence = Evidence {
        resource: resource.clone(),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: glomeris::detectors::DetectorId("docker.images"),
        logical_bytes: ProbeOutcome::Observed(1_200_000),
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Observed(1_200_000),
        reclaimable_bytes_is_lower_bound: false,
        last_modified: ProbeOutcome::Observed(at(86_400 * 12)),
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability: ResourceKind::DockerImage.regenerability(),
        recoverability: Recoverability::RegenerableByTool,
        native_cleanup: NativeCleanup::Unsupported,
        // No path, so the three path-based probes structurally did not run.
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Observed(true),
        docker_lifecycle: Some(lifecycle),
        collected_at: collected_at(),
        sources: Vec::new(),
    };
    let decision = PolicyDecision {
        resource,
        class: PolicyClass::Ask,
        reasons: vec![ReasonCode::DockerObjectInUse],
        evidence_collected_at: collected_at(),
        evaluated_at: collected_at(),
        policy_version: 1,
    };
    (evidence, decision)
}

/// The image's referrer, as the local graph holds it: a real Docker identity,
/// so the assertion that only its *count* reaches the payload is an assertion
/// about withheld identity rather than about a string chosen to be absent.
fn container_reference() -> ResourceId {
    ResourceId::new(
        ResourceKind::DockerContainer,
        ResourceLocator::Tool {
            tool: OwningTool::Docker,
            id: CONTAINER_MARKER.to_string(),
        },
    )
}

/// The three-bucket fixture: one resource inside a repository, one global,
/// one whose repository could not be determined — plus the Docker object that
/// is the only source of the `docker_lifecycle` keys.
fn projection() -> GraphProjection {
    let in_repo_target = temp_cargo_project("worktree");
    let repo_root = in_repo_target
        .parent()
        .expect("the target has a parent")
        .to_path_buf();
    let global_target = temp_cargo_project("global");
    let unplaced_target = temp_cargo_project("unplaced");

    let candidates = vec![
        candidate(
            &in_repo_target,
            ProbeOutcome::Observed(Some(GitState {
                repo_root: repo_root.clone(),
                common_dir: repo_root.join(".git"),
                dirty: true,
                untracked: true,
                worktree: true,
            })),
            vec![ProcessRef {
                pid: PROCESS_PID,
                command: PROCESS_COMMAND.to_string(),
            }],
        ),
        candidate(&global_target, ProbeOutcome::Observed(None), Vec::new()),
        candidate(
            &unplaced_target,
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied),
            Vec::new(),
        ),
        docker_candidate(DockerLifecycle {
            activity: DockerActivity::Active,
            persistence: DockerPersistence::ToolManaged,
            references: ProbeOutcome::Observed(DockerReferences {
                referenced_by: vec![container_reference()],
                active_referrers: 1,
            }),
        }),
    ];

    let mut graph = WorkspaceEvidenceGraph::build(
        &candidates,
        &WorkspaceSurvey::unsurveyed(),
        MachineContext {
            free_bytes: ProbeOutcome::Observed(11_000_000),
            total_bytes: ProbeOutcome::Observed(500_000_000),
            recovery_goal_bytes: Some(40_000_000),
        },
        collected_at(),
    );

    // Observed branch, external and history facts, so the fields that only
    // carry a value when something was observed are exercised too.
    let worktree = &mut graph.repositories[0].worktrees[0];
    worktree.lifecycle.branch = ProbeOutcome::Observed(WorktreeBranchState {
        branch: Some(BRANCH_NAME.to_string()),
        upstream: UpstreamState::Tracking {
            ahead: 3,
            behind: 1,
        },
        merged: MergedState::NotMerged {
            into: MERGE_AXIS.to_string(),
        },
    });
    worktree.external.pull_request = ExternalFact::observed(
        ExternalSource::GitHubPullRequests,
        PullRequestState::Open,
        at(86_400 * 28),
    );
    worktree.external.task = ExternalFact::observed(
        ExternalSource::JiraIssues,
        TaskState::InProgress,
        at(86_400 * 29),
    );
    graph.history = ProbeOutcome::Observed(WorkflowHistorySummary::from_observations(
        WorkflowMode::ParallelMultiWorktree,
        14,
    ));

    GraphProjection::build(
        &graph,
        &candidates,
        &ActionRegistry::builtin(),
        collected_at(),
    )
}

/// The same three buckets with every probe unanswered.
///
/// This variant exists because the fixture above answers everything, and a
/// pin taken only over answered data cannot see a field that vanishes when
/// there is no answer. `#[serde(skip_serializing_if = "Option::is_none")]` on
/// `Reported::value` is the concrete regression: harmless-looking, byte-saving
/// on exactly the payloads where it matters, and invisible to an all-observed
/// fixture. `Reported` deliberately emits both keys always, and the assertion
/// that both projections yield the same key set is what holds it to that.
fn projection_with_nothing_observed() -> GraphProjection {
    let in_repo_target = temp_cargo_project("unanswered-worktree");
    let repo_root = in_repo_target
        .parent()
        .expect("the target has a parent")
        .to_path_buf();
    let global_target = temp_cargo_project("unanswered-global");
    let unplaced_target = temp_cargo_project("unanswered-unplaced");

    // The git state stays observed for the first two: it is what places a
    // resource in a bucket, and an unplaced-only graph would have no
    // repository and no worktree, so most of the key set would simply be
    // absent rather than unavailable. Everything a *probe* could have failed
    // to answer is unavailable.
    let mut candidates = vec![
        candidate(
            &in_repo_target,
            ProbeOutcome::Observed(Some(GitState {
                repo_root: repo_root.clone(),
                common_dir: repo_root.join(".git"),
                dirty: false,
                untracked: false,
                worktree: true,
            })),
            Vec::new(),
        ),
        candidate(&global_target, ProbeOutcome::Observed(None), Vec::new()),
        candidate(
            &unplaced_target,
            ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            Vec::new(),
        ),
        // Every axis unknown and the reference query not attempted — what a
        // Docker object looks like when the daemon stopped answering. The
        // `Reported` keys under `docker_lifecycle` are `unavailable` here and
        // observed in the fixture above, so neither direction is unpinned.
        docker_candidate(DockerLifecycle::unknown(ProbeReason::ToolNotRunning)),
    ];
    for (evidence, _) in &mut candidates {
        evidence.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);
        evidence.reclaimable_bytes_is_lower_bound = false;
        evidence.last_modified = ProbeOutcome::Unavailable(ProbeReason::Failed);
        evidence.tool_liveness = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);
        evidence.open_by_process = ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);
        evidence.process_cwd_match = ProbeOutcome::Unavailable(ProbeReason::PermissionDenied);
    }

    // A fresh graph already has an unsurveyed branch probe, unqueried
    // external facts and an unattempted history, so nothing needs
    // overriding — that is the honest default rather than a contrivance.
    let graph = WorkspaceEvidenceGraph::build(
        &candidates,
        &WorkspaceSurvey::unsurveyed(),
        MachineContext::unmeasured(),
        collected_at(),
    );

    GraphProjection::build(
        &graph,
        &candidates,
        &ActionRegistry::builtin(),
        collected_at(),
    )
}

// ------------------------------------------------------------------
// The pin
// ------------------------------------------------------------------

/// The three keys of one `Reported<T>`, plus the container.
///
/// Spelled out as a helper rather than thirty literal lines because
/// `Reported` is one reviewed shape used many times. Every *field name* below
/// is still written by hand exactly once, which is the property this pin
/// exists for: a new DTO field cannot reach a provider until someone adds it
/// here.
fn reported(prefix: &str) -> Vec<String> {
    vec![
        prefix.to_string(),
        format!("{prefix}.status"),
        format!("{prefix}.value"),
        format!("{prefix}.unavailable_reason"),
    ]
}

/// Every key of one `ResourceView`, which appears in all three buckets.
fn resource_view(prefix: &str) -> Vec<String> {
    let mut paths = vec![
        prefix.to_string(),
        format!("{prefix}.evidence_ref"),
        format!("{prefix}.kind"),
        format!("{prefix}.owning_tool"),
        format!("{prefix}.reclaimable_bytes_is_lower_bound"),
        format!("{prefix}.regenerability"),
        format!("{prefix}.completeness"),
        format!("{prefix}.policy_label"),
        format!("{prefix}.offered_action_ids"),
        format!("{prefix}.offered_action_ids[]"),
        // Always emitted, `null` on everything Docker does not own. The keys
        // *inside* it appear only where a Docker resource does — see
        // `docker_lifecycle_view`.
        format!("{prefix}.docker_lifecycle"),
    ];
    paths.extend(reported(&format!("{prefix}.reclaimable_bytes")));
    paths.extend(reported(&format!("{prefix}.age_days")));
    paths.extend(reported(&format!("{prefix}.tool_liveness")));
    paths
}

/// The keys inside one `DockerLifecycleView`.
///
/// Pinned under `global_resources[]` alone, because that is the only bucket a
/// Docker object can reach: Docker addresses its objects by id rather than by
/// path, and a resource with a `ResourceLocator::Tool` has no containing
/// repository by construction (HORO-1561). Should that ever stop being true,
/// these keys appear under a prefix the pin does not list and
/// `the_model_payload_key_set_is_pinned` fails on the added paths — which is
/// the loud failure, not a missed one.
fn docker_lifecycle_view(prefix: &str) -> Vec<String> {
    let mut paths = vec![
        format!("{prefix}.activity"),
        format!("{prefix}.persistence"),
    ];
    paths.extend(reported(&format!("{prefix}.referrer_count")));
    paths.extend(reported(&format!("{prefix}.active_referrers")));
    paths
}

/// Every key path `ModelGraphView` may serialize. Hand-written.
fn pinned_paths() -> BTreeSet<String> {
    let mut paths: Vec<String> = vec![
        // Machine
        "machine".into(),
        "machine.evidence_ref".into(),
        "machine.recovery_goal_bytes".into(),
        // Repositories
        "repositories".into(),
        "repositories[]".into(),
        "repositories[].evidence_ref".into(),
        "repositories[].worktree_count".into(),
        "repositories[].worktrees".into(),
        "repositories[].worktrees[]".into(),
        "repositories[].worktrees[].evidence_ref".into(),
        "repositories[].worktrees[].linked".into(),
        // Branch lifecycle
        "repositories[].worktrees[].branch".into(),
        "repositories[].worktrees[].branch.dirty".into(),
        "repositories[].worktrees[].branch.untracked".into(),
        "repositories[].worktrees[].branch.unique_work".into(),
        // Activity
        "repositories[].worktrees[].activity".into(),
        "repositories[].worktrees[].activity.state".into(),
        "repositories[].worktrees[].activity.observed_process_count".into(),
        "repositories[].worktrees[].activity.unanswered_probe_count".into(),
        // External context
        "repositories[].worktrees[].pull_request".into(),
        "repositories[].worktrees[].pull_request.source".into(),
        "repositories[].worktrees[].task".into(),
        "repositories[].worktrees[].task.source".into(),
        // Collections
        "repositories[].worktrees[].resources".into(),
        "global_resources".into(),
        "unplaced_resources".into(),
        "unplaced_resources[]".into(),
        "unplaced_resources[].unplaced_reason".into(),
        // Workflow history
        "workflow_history".into(),
        "workflow_history.evidence_ref".into(),
    ];

    paths.extend(reported("machine.free_bytes"));
    paths.extend(reported("machine.total_bytes"));

    let branch = "repositories[].worktrees[].branch";
    paths.extend(reported(&format!("{branch}.upstream_state")));
    paths.extend(reported(&format!("{branch}.commits_ahead_of_upstream")));
    paths.extend(reported(&format!("{branch}.commits_behind_upstream")));
    paths.extend(reported(&format!("{branch}.merged_state")));
    paths.extend(reported(&format!("{branch}.merge_comparison_known")));
    paths.extend(reported(&format!("{branch}.detached_head")));

    for external in [
        "repositories[].worktrees[].pull_request",
        "repositories[].worktrees[].task",
    ] {
        paths.extend(reported(&format!("{external}.state")));
        paths.extend(reported(&format!("{external}.observed_age_days")));
    }

    paths.extend(resource_view("repositories[].worktrees[].resources[]"));
    paths.extend(resource_view("global_resources[]"));
    paths.extend(resource_view("unplaced_resources[].resource"));
    paths.extend(docker_lifecycle_view("global_resources[].docker_lifecycle"));

    paths.extend(reported("workflow_history.mode"));
    paths.extend(reported("workflow_history.confidence"));
    paths.extend(reported("workflow_history.observation_count"));

    let set: BTreeSet<String> = paths.iter().cloned().collect();
    assert_eq!(
        set.len(),
        paths.len(),
        "the pin lists a path twice, which would hide a missing one"
    );
    set
}

/// Every key path in a serialized value.
///
/// Records object keys whatever their value type, so a new field of any
/// shape — including an empty collection — appears. Array elements extend the
/// path with `[]`, which collapses repeats: two worktrees contribute one set
/// of paths, and one worktree is enough to cover the shape.
fn key_paths(value: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk(value, "", &mut out);
    out
}

fn walk(value: &Value, path: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                out.insert(child_path.clone());
                walk(child, &child_path, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                let child_path = format!("{path}[]");
                out.insert(child_path.clone());
                walk(item, &child_path, out);
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

/// AC6. The serialized key set is exactly the pinned one — and is the same
/// whether the probes answered or not.
///
/// Both variants are checked against the same pin, which is the stronger
/// claim: the key set does not depend on the data. A payload whose shape
/// changes with what was observed would tell a provider something by its
/// silences, and would leave the pin unable to see a field that only appears
/// on one kind of input.
#[test]
fn the_model_payload_key_set_is_pinned() {
    let pinned = pinned_paths();

    for (label, projection) in [
        ("everything observed", projection()),
        ("nothing observed", projection_with_nothing_observed()),
    ] {
        let body = serde_json::to_value(&projection.view).expect("the projection serializes");
        let actual = key_paths(&body);

        let added: Vec<&String> = actual.difference(&pinned).collect();
        assert!(
            added.is_empty(),
            "[{label}] these keys reach a provider and are not pinned. Each \
             one is a deliberate decision to send something off this machine \
             — add it to `pinned_paths` only after deciding it may leave: \
             {added:#?}"
        );

        let removed: Vec<&String> = pinned.difference(&actual).collect();
        assert!(
            removed.is_empty(),
            "[{label}] these keys are pinned but the projection no longer \
             produces them. A pin that is a superset stops catching \
             replacements, and a key that disappears when a probe fails makes \
             the payload's shape carry information of its own: {removed:#?}"
        );
    }
}

/// The unanswered variant really is unanswered. Without this, the equality
/// above could be satisfied by a fixture that quietly observed everything
/// twice, and the property it exists to pin — that the key set is
/// input-independent — would be tested against one input.
#[test]
fn the_unanswered_variant_actually_reports_unavailable_values() {
    let projection = projection_with_nothing_observed();
    let body = serde_json::to_value(&projection.view).expect("the projection serializes");

    let worktree = &body["repositories"][0]["worktrees"][0];
    assert_eq!(worktree["branch"]["unique_work"], "unknown");
    assert_eq!(
        worktree["branch"]["upstream_state"]["status"],
        "unavailable"
    );
    assert_eq!(worktree["activity"]["state"], "unknown");
    assert_eq!(worktree["pull_request"]["state"]["status"], "unavailable");
    assert_eq!(
        worktree["task"]["observed_age_days"]["status"],
        "unavailable"
    );
    assert_eq!(
        worktree["resources"][0]["reclaimable_bytes"]["unavailable_reason"],
        "permission_denied"
    );
    assert_eq!(
        worktree["resources"][0]["age_days"]["status"],
        "unavailable"
    );
    assert_eq!(body["machine"]["free_bytes"]["status"], "unavailable");
    assert_eq!(body["workflow_history"]["mode"]["status"], "unavailable");
    assert_eq!(
        body["unplaced_resources"][0]["unplaced_reason"],
        "timed_out"
    );

    // The Docker object whose daemon stopped answering. `"unknown"` on both
    // axes, and a referrer count that is unavailable rather than zero — an
    // observed zero would state that nothing needs this image, which is the
    // one thing an unanswered reference query has not established.
    let docker = body["global_resources"]
        .as_array()
        .expect("global resources are an array")
        .iter()
        .find(|resource| !resource["docker_lifecycle"].is_null())
        .expect("the Docker object is a global resource");
    assert_eq!(docker["docker_lifecycle"]["activity"], "unknown");
    assert_eq!(docker["docker_lifecycle"]["persistence"], "unknown");
    assert_eq!(
        docker["docker_lifecycle"]["referrer_count"]["status"],
        "unavailable"
    );
    assert_eq!(
        docker["docker_lifecycle"]["referrer_count"]["unavailable_reason"],
        "tool_not_running"
    );
    assert!(docker["docker_lifecycle"]["active_referrers"]["value"].is_null());

    // And the `value` keys are all present and null, which is exactly what a
    // `skip_serializing_if` would remove.
    assert!(worktree["branch"]["upstream_state"]["value"].is_null());
    assert!(body["machine"]["free_bytes"]["value"].is_null());
}

/// The pin covers a real projection and not a sketch of one. A fixture that
/// exercised two of the DTO's twelve types would pass the equality above
/// while leaving most of the egress surface unpinned.
#[test]
fn the_pinned_set_covers_every_view_type() {
    let pinned = pinned_paths();
    assert!(
        pinned.len() > 120,
        "the egress surface is {} paths, which is too few to be the whole \
         DTO — the fixture has probably stopped exercising a bucket",
        pinned.len()
    );
    for required in [
        "machine.free_bytes.status",
        "repositories[].worktrees[].branch.unique_work",
        "repositories[].worktrees[].activity.unanswered_probe_count",
        "repositories[].worktrees[].pull_request.state.unavailable_reason",
        "repositories[].worktrees[].task.observed_age_days.value",
        "repositories[].worktrees[].resources[].offered_action_ids[]",
        "global_resources[].policy_label",
        "global_resources[].docker_lifecycle.activity",
        "global_resources[].docker_lifecycle.referrer_count.unavailable_reason",
        "unplaced_resources[].unplaced_reason",
        "workflow_history.confidence.value",
    ] {
        assert!(pinned.contains(required), "{required} is not pinned");
    }
}

/// AC9, and the reason this ticket exists. Local identity of every kind the
/// fixture carries — account name, absolute path, branch name, merge axis,
/// process command, pid — stays on this Mac.
#[test]
fn no_local_identity_reaches_the_payload() {
    let projection = projection();
    let body = serde_json::to_string(&projection.view).expect("the projection serializes");

    assert!(
        !body.contains(ACCOUNT_MARKER),
        "the account marker reached the payload: {body}"
    );
    assert!(
        !body.contains('/'),
        "a path separator reached the payload: {body}"
    );
    assert!(
        !body.contains("ACME-9142"),
        "a ticket key reached the payload"
    );
    assert!(
        !body.contains("unreleased_product_codename"),
        "a branch name reached the payload"
    );
    assert!(
        !body.contains("acme-release-train-q3"),
        "the merge axis reached the payload"
    );
    assert!(
        !body.contains("acme-internal-builder"),
        "a process command reached the payload"
    );
    assert!(
        !body.contains(&PROCESS_PID.to_string()),
        "a pid reached the payload"
    );
    assert!(
        !body.contains("Cargo.toml"),
        "a filename below a resource root reached the payload"
    );
    assert!(
        !body.contains(".git"),
        "a git directory reached the payload"
    );
    assert!(
        !body.contains(&std::env::temp_dir().to_string_lossy().to_string()),
        "the temp directory reached the payload"
    );

    // HORO-1544. The Docker object's own identity, and the identity of what
    // references it, are the same class of fact as a branch name: chosen by a
    // human, and routinely naming a product, a customer or a registry host.
    // Only the *count* of referrers may leave.
    assert!(
        !body.contains(IMAGE_MARKER),
        "a Docker image reference reached the payload"
    );
    assert!(
        !body.contains(CONTAINER_MARKER),
        "the name of a referencing container reached the payload"
    );
    assert!(
        !body.contains("acme-billing"),
        "a Docker identity fragment reached the payload"
    );
    assert!(
        !body.contains("registry.acme.example"),
        "a registry host reached the payload"
    );
}

/// The control for the test above: the payload really is a projection of
/// those three resources, and the identity it withheld really was there to
/// withhold. Without this, an empty object would pass every leak assertion.
#[test]
fn the_payload_projects_the_resources_whose_identity_it_withheld() {
    let projection = projection();
    let body = serde_json::to_string(&projection.view).expect("the projection serializes");

    // The shape facts derived from the identity did come through.
    assert!(body.contains("cargo_target_dir"));
    assert!(body.contains("resource_1"));
    assert!(body.contains("repo_1"));
    assert!(body.contains("workspace_1"));
    assert!(body.contains("parallel_multi_worktree"));
    assert!(body.contains("permission_denied"));
    assert!(body.contains("in_use"), "the observed process was counted");
    assert!(body.contains("not_merged"));
    assert!(body.contains("present"), "unique work was reported");

    // And the Docker facts the identity was withheld in favour of.
    assert!(body.contains("docker_image"));
    assert!(
        body.contains("\"activity\":\"active\""),
        "the per-object activity was projected"
    );
    assert!(
        body.contains("tool_managed"),
        "the persistence axis was projected"
    );

    assert_eq!(projection.aliases.len(), 4);
    let resolved: Vec<String> = projection
        .aliases
        .issued()
        .map(|alias| {
            projection
                .aliases
                .resolve(alias)
                .expect("an issued alias resolves")
                .to_string()
        })
        .collect();
    assert_eq!(resolved.len(), 4);

    // Three path-located resources under a marked path, and the Docker object,
    // whose real identity is a registry reference rather than a path. Both
    // halves are asserted because the point is that the alias table holds
    // whatever the real identity *is* — if it held neither, the leak
    // assertions above would have been testing a fixture with nothing to leak.
    let marked = resolved
        .iter()
        .filter(|id| id.contains(ACCOUNT_MARKER))
        .count();
    assert_eq!(
        marked, 3,
        "the local side of the alias table must hold the real paths: {resolved:?}"
    );
    assert!(
        resolved.iter().any(|id| id.contains(IMAGE_MARKER)),
        "and the real Docker identity: {resolved:?}"
    );
}

/// An alias is the only handle a planner response may cite, and one this
/// projection never issued resolves to nothing rather than to the nearest
/// plausible resource. Fail closed, from outside the crate.
#[test]
fn an_alias_this_projection_never_issued_resolves_to_nothing() {
    let projection = projection();
    for bogus in [
        "resource_5",
        "resource_0",
        "resource_1 ",
        "RESOURCE_1",
        "repo_1",
        "workspace_1",
        "machine",
        "workflow_history",
        "",
    ] {
        assert!(
            projection.aliases.resolve(bogus).is_none(),
            "{bogus:?} resolved to a resource"
        );
    }
    for real in ["resource_1", "resource_2", "resource_3", "resource_4"] {
        assert!(projection.aliases.resolve(real).is_some());
    }
}

/// The wire form of an unknown is a reason, not an absence. Asserted on the
/// serialized bytes because that is what a provider reads: a missing key
/// would let a reader supply its own default, and `null` alone would not say
/// whether the answer was no or nobody looked.
#[test]
fn an_unavailable_value_serializes_its_reason_rather_than_vanishing() {
    let projection = projection();
    let body = serde_json::to_value(&projection.view).expect("the projection serializes");

    // The unplaced resource's repository is unknown because a probe was
    // denied, which the payload states.
    assert_eq!(
        body["unplaced_resources"][0]["unplaced_reason"],
        "permission_denied"
    );

    // An observed value keeps both keys present, so the key set does not
    // depend on the data.
    let free = &body["machine"]["free_bytes"];
    assert_eq!(free["status"], "observed");
    assert_eq!(free["value"], 11_000_000);
    assert!(
        free.get("unavailable_reason").is_some(),
        "the key must exist even when there is no reason, or a pinned key \
         set cannot detect an added field"
    );
    assert!(free["unavailable_reason"].is_null());
}
