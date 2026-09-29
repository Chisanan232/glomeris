//! External context is evidence for a reader, never permission (HORO-1546 AC 6,
//! AC 8).
//!
//! The sentence this file exists to make impossible is "the pull request is
//! merged and the ticket is closed, so the working tree can go". It is the most
//! persuasive thing a remote provider can say, it is the thing a provider says
//! most often, and it is wrong whenever the developer has anything on disk that
//! nowhere else has — which, on a machine somebody is working on, is most of the
//! time.
//!
//! So each test below sets up the maximally reassuring remote answer — merged
//! pull request, `Done` ticket, branch contained in the default branch — against
//! a local reality that contradicts it, and asserts the local reality wins.
//!
//! # Why the strongest assertion here is an equality
//!
//! `attach_external_context_changes_nothing_but_the_external_field` builds the
//! same graph twice, attaches a merged/`Done` answer to one, then blanks that one
//! field back out and requires the two graphs to be equal. That is stronger than
//! checking a list of things that did not change, because it covers the fields
//! nobody thought to list — including ones added later. A future edit that let a
//! provider's answer nudge an activity state, a policy decision, a byte figure or
//! a resource's offered actions fails here without anybody having predicted which
//! it would be.
//!
//! # What each test would catch
//!
//! Every one of these is written against a specific mutation, named in its own
//! comment, so that it fails for the intended reason rather than incidentally
//! (campaign §19).

use glomeris::evidence::{
    Evidence, GitState, NativeCleanup, ProbeOutcome, ProbeReason, ProcessRef, Recoverability,
    ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision, ReasonCode};
use glomeris::workspace::external::{
    ExternalContextResolver, ExternalProviderError, PullRequestProvider, RepositoryBranchSubject,
    SubjectResolver, SubjectUnknown, TaskKey, TaskProvider,
};
use glomeris::workspace::{
    ActivityState, ExternalContext, IntegrationEvidence, MachineContext, MergedState,
    PatchEquivalence, PullRequestState, TaskState, UniqueWork, UpstreamState,
    WorkspaceEvidenceGraph, WorkspaceSurvey, WorktreeBranchState,
};

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// A branch shaped like this campaign's own, so the task key really is
/// extractable and the Jira lookup really is reached.
const BRANCH: &str = "v0.0.1/HORO-1546/feat/external_context";

const HOST: &str = "github.com";

fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

fn collected_at() -> SystemTime {
    at(86_400 * 30)
}

/// Inside `PolicyConfig::default().max_evidence_age`, so nothing below is
/// classified `Ask` merely for being stale — which would make every
/// not-auto-safe assertion pass for the wrong reason.
fn now() -> SystemTime {
    collected_at() + Duration::from_secs(60)
}

// ---------------------------------------------------------------------------
// The maximally reassuring providers
// ---------------------------------------------------------------------------

/// A subject resolver that answers without git, so these tests exercise the
/// *combination* rather than re-testing the git reading that
/// `workspace::external::subject` already covers with real repositories.
struct FixedSubject;

impl SubjectResolver for FixedSubject {
    fn repository_branch(
        &self,
        _worktree: &Path,
        branch: Option<&str>,
    ) -> Result<RepositoryBranchSubject, SubjectUnknown> {
        let branch = branch.ok_or(SubjectUnknown::DetachedHead)?;
        Ok(RepositoryBranchSubject {
            host: HOST.to_string(),
            owner: "Chisanan232".to_string(),
            repo: "glomeris".to_string(),
            branch: branch.to_string(),
        })
    }
}

struct SayingPullRequest(Result<PullRequestState, ExternalProviderError>);

impl PullRequestProvider for SayingPullRequest {
    fn host(&self) -> &str {
        HOST
    }

    fn pull_request_state(
        &self,
        _subject: &RepositoryBranchSubject,
    ) -> Result<PullRequestState, ExternalProviderError> {
        self.0.clone()
    }
}

struct SayingTask {
    answer: Result<TaskState, ExternalProviderError>,
    asked: RefCell<Vec<String>>,
}

impl SayingTask {
    fn new(answer: Result<TaskState, ExternalProviderError>) -> Self {
        Self {
            answer,
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl TaskProvider for SayingTask {
    fn task_state(&self, key: &TaskKey) -> Result<TaskState, ExternalProviderError> {
        self.asked.borrow_mut().push(key.as_str().to_string());
        self.answer.clone()
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A real cargo project on disk, because `GraphProjection` and the actionability
/// layer stat the manifest and a fabricated path yields an empty action list.
fn temp_cargo_project(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "glomeris-h1546-authority-{label}-{}-{n}",
        std::process::id()
    ));
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create the temp target directory");
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fixture\"\n")
        .expect("write the temp manifest");
    target
}

/// What the local machine observed about one `target/` directory.
struct Local {
    dirty: bool,
    untracked: bool,
    processes: Vec<ProcessRef>,
}

impl Local {
    /// The reassuring shape: nothing modified, nothing untracked, nothing using
    /// it. Every test that wants a local contradiction adds exactly one.
    fn quiet() -> Self {
        Self {
            dirty: false,
            untracked: false,
            processes: Vec::new(),
        }
    }
}

fn candidate(target: &Path, repo_root: &Path, local: &Local) -> (Evidence, PolicyDecision) {
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
        logical_bytes: ProbeOutcome::Observed(4_096),
        physical_bytes: Some(4_096),
        reclaimable_bytes: ProbeOutcome::Observed(4_096),
        reclaimable_bytes_is_lower_bound: true,
        last_modified: ProbeOutcome::Observed(at(86_400 * 20)),
        last_accessed: ProbeOutcome::Observed(at(86_400 * 21)),
        regenerability: ResourceKind::CargoTargetDir.regenerability(),
        recoverability: Recoverability::RegenerableByRebuild,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Observed(local.processes.clone()),
        process_cwd_match: ProbeOutcome::Observed(Vec::new()),
        git_state: ProbeOutcome::Observed(Some(GitState {
            repo_root: repo_root.to_path_buf(),
            common_dir: repo_root.join(".git"),
            dirty: local.dirty,
            untracked: local.untracked,
            worktree: true,
        })),
        tool_liveness: ProbeOutcome::Observed(false),
        docker_lifecycle: None,
        collected_at: collected_at(),
        sources: Vec::new(),
    };
    // Deliberately the *most* permissive decision the engine can produce, so
    // any test asserting "not auto-safe" is asserting about a fresh `classify`
    // call rather than about this seed value.
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

/// A graph over one working tree, with `branch` as its already-read branch fact.
fn graph_for(
    label: &str,
    local: &Local,
    branch: ProbeOutcome<WorktreeBranchState>,
) -> (WorkspaceEvidenceGraph, Vec<(Evidence, PolicyDecision)>) {
    let target = temp_cargo_project(label);
    let repo_root = target
        .parent()
        .expect("the target has a parent")
        .to_path_buf();
    let candidates = vec![candidate(&target, &repo_root, local)];

    let mut graph = WorkspaceEvidenceGraph::build(
        &candidates,
        &WorkspaceSurvey::unsurveyed(),
        MachineContext::unmeasured(),
        collected_at(),
    );
    graph.repositories[0].worktrees[0].lifecycle.branch = branch;
    (graph, candidates)
}

/// The branch fact a provider's merged answer would most plausibly accompany:
/// on a branch, tracking, nothing ahead, and contained in the default branch.
fn settled_branch() -> ProbeOutcome<WorktreeBranchState> {
    ProbeOutcome::Observed(WorktreeBranchState {
        branch: Some(BRANCH.to_string()),
        upstream: UpstreamState::Tracking {
            ahead: 0,
            behind: 0,
        },
        merged: MergedState::Merged {
            into: "origin/main".to_string(),
        },
        integration: IntegrationEvidence {
            divergence: ProbeOutcome::Observed(glomeris::workspace::Divergence {
                unique_commits: 0,
                equivalent_commits: 0,
                unclassified_commits: 0,
            }),
            equivalence: PatchEquivalence::NotApplicable,
            tip_committed_at: ProbeOutcome::Observed(at(86_400 * 18)),
            comparison_tip_committed_at: ProbeOutcome::Observed(at(86_400 * 19)),
        },
    })
}

/// The same branch with three commits nobody else has.
fn branch_with_local_commits() -> ProbeOutcome<WorktreeBranchState> {
    let ProbeOutcome::Observed(mut state) = settled_branch() else {
        unreachable!("settled_branch is observed")
    };
    state.upstream = UpstreamState::Tracking {
        ahead: 3,
        behind: 0,
    };
    ProbeOutcome::Observed(state)
}

/// Attaches the most reassuring remote answer there is.
fn attach_merged_and_done(graph: &mut WorkspaceEvidenceGraph) {
    let subjects = FixedSubject;
    let pulls = SayingPullRequest(Ok(PullRequestState::Merged));
    let tasks = SayingTask::new(Ok(TaskState::Done));
    let resolver = ExternalContextResolver {
        subjects: &subjects,
        pull_requests: Some(&pulls),
        tasks: Some(&tasks),
    };
    let attempts = graph.attach_external_context(&resolver, now());
    assert_eq!(attempts.len(), 1, "one working tree, one attempt");
    assert_eq!(attempts[0].pull_request_detail.tag(), "answered");
    assert_eq!(attempts[0].task_detail.tag(), "answered");
    // The Jira lookup really happened, and on the exact key. Without this the
    // tests below could pass over a `Done` nobody ever asked for.
    assert_eq!(*tasks.asked.borrow(), vec!["HORO-1546".to_string()]);
}

fn worktree(graph: &WorkspaceEvidenceGraph) -> &glomeris::workspace::WorktreeNode {
    &graph.repositories[0].worktrees[0]
}

// ---------------------------------------------------------------------------
// AC 8 — merged and closed never outrank what is on disk
// ---------------------------------------------------------------------------

/// A modified file outranks every remote answer there is.
///
/// Mutation this catches: `BranchLifecycle::unique_work` consulting
/// `WorktreeNode::external` — the shortest path from this feature to a wrong
/// deletion.
#[test]
fn a_merged_pull_request_and_a_done_task_leave_a_dirty_tree_holding_unique_work() {
    let local = Local {
        dirty: true,
        ..Local::quiet()
    };
    let (mut graph, candidates) = graph_for("dirty", &local, settled_branch());

    attach_merged_and_done(&mut graph);

    let worktree = worktree(&graph);
    assert_eq!(
        worktree.external.pull_request.outcome,
        ProbeOutcome::Observed(PullRequestState::Merged),
        "the fixture must really be reporting the reassuring answer"
    );
    assert_eq!(
        worktree.external.task.outcome,
        ProbeOutcome::Observed(TaskState::Done)
    );
    assert_eq!(worktree.lifecycle.unique_work(), UniqueWork::Present);

    // And the decision layer agrees, on freshly classified evidence.
    let decision = classify(&candidates[0].0, &PolicyConfig::default(), now());
    assert_ne!(decision.class, PolicyClass::AutoSafe, "{decision:?}");
}

/// An untracked file is work that exists nowhere at all — not on a remote, not
/// in a pull request, not in a ticket.
#[test]
fn an_untracked_file_survives_a_merged_pull_request() {
    let local = Local {
        untracked: true,
        ..Local::quiet()
    };
    let (mut graph, _candidates) = graph_for("untracked", &local, settled_branch());

    attach_merged_and_done(&mut graph);

    assert_eq!(
        worktree(&graph).lifecycle.unique_work(),
        UniqueWork::Present
    );
}

/// A merged pull request says nothing about commits made after it merged, which
/// is exactly the inference §13 names: *PR merged ≠ current worktree contains
/// nothing newer*.
#[test]
fn local_commits_made_after_a_merge_are_still_unique_work() {
    let (mut graph, _candidates) = graph_for("ahead", &Local::quiet(), branch_with_local_commits());

    attach_merged_and_done(&mut graph);

    let worktree = worktree(&graph);
    assert!(!worktree.lifecycle.dirty, "the tree itself is clean");
    assert_eq!(worktree.lifecycle.unique_work(), UniqueWork::Present);
}

/// A process holding the directory open is active use. A closed ticket is not a
/// reason to stop believing the process table.
///
/// Mutation this catches: folding `external` into `ActivityFacts`, or any
/// reading that lets a `Done` task downgrade `InUse` to `Idle`.
#[test]
fn a_done_task_does_not_make_an_in_use_worktree_idle() {
    let local = Local {
        processes: vec![ProcessRef {
            pid: 4242,
            command: "cargo build".to_string(),
        }],
        ..Local::quiet()
    };
    let (mut graph, candidates) = graph_for("in-use", &local, settled_branch());

    attach_merged_and_done(&mut graph);

    let worktree = worktree(&graph);
    assert_eq!(worktree.activity.state, ActivityState::InUse);
    assert_eq!(worktree.activity.observed_processes.len(), 1);

    let decision = classify(&candidates[0].0, &PolicyConfig::default(), now());
    assert!(
        decision.reasons.contains(&ReasonCode::ResourceInActiveUse),
        "{decision:?}"
    );
    assert_ne!(decision.class, PolicyClass::AutoSafe);
}

/// The whole of AC 6 as one equality: attaching an answer changes the external
/// field and nothing else in the graph.
///
/// Mutation this catches: any of them. A provider's answer that reached an
/// activity state, a policy decision, a byte figure, a placement bucket or a
/// field added next year fails here without this test having to name it.
#[test]
fn attaching_external_context_changes_nothing_but_the_external_field() {
    let local = Local {
        dirty: true,
        processes: vec![ProcessRef {
            pid: 909,
            command: "rust-analyzer".to_string(),
        }],
        ..Local::quiet()
    };
    let (mut answered, _candidates) = graph_for("answered", &local, settled_branch());
    // A clone rather than a second fixture: each fixture gets its own temp
    // directory, and the paths are part of the value being compared, so two
    // fixtures could never be equal no matter how well the code behaved.
    let untouched = answered.clone();

    attach_merged_and_done(&mut answered);
    assert_ne!(
        answered, untouched,
        "attaching must actually have changed something, or this test is vacuous"
    );

    answered.repositories[0].worktrees[0].external = ExternalContext::unqueried();
    assert_eq!(answered, untouched);
}

/// A resolver with no providers is not just quiet — it leaves the graph byte
/// for byte as it was, which is what "optional forever" has to mean.
#[test]
fn a_disabled_resolver_reports_nothing_and_changes_nothing() {
    let (mut graph, _candidates) = graph_for("disabled", &Local::quiet(), settled_branch());
    let before = graph.clone();

    let subjects = FixedSubject;
    let attempts =
        graph.attach_external_context(&ExternalContextResolver::disabled(&subjects), now());

    assert!(attempts.is_empty());
    assert_eq!(graph, before);
}

// ---------------------------------------------------------------------------
// AC 3 — a provider that could not answer is not a provider that said "no"
// ---------------------------------------------------------------------------

/// Each refusal reaches the graph as its own reason, and none of them becomes
/// `NoneObserved`.
///
/// Mutation this catches: an adapter or resolver mapping a failure to
/// `Ok(NoneObserved)` for tidiness — at which point "there is no pull request"
/// and "GitHub refused the token" become the same sentence.
#[test]
fn a_provider_that_could_not_answer_never_reports_an_absence() {
    for (error, expected) in [
        (
            ExternalProviderError::AuthRejected { status: 401 },
            ProbeReason::PermissionDenied,
        ),
        (ExternalProviderError::RateLimited, ProbeReason::RateLimited),
        (ExternalProviderError::TimedOut, ProbeReason::TimedOut),
        (
            ExternalProviderError::Unreachable("connection refused".into()),
            ProbeReason::Failed,
        ),
    ] {
        let (mut graph, _candidates) = graph_for("refused", &Local::quiet(), settled_branch());

        let subjects = FixedSubject;
        let pulls = SayingPullRequest(Err(error.clone()));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        };
        let attempts = graph.attach_external_context(&resolver, now());

        let fact = &worktree(&graph).external.pull_request;
        assert_eq!(
            fact.outcome,
            ProbeOutcome::Unavailable(expected),
            "{error:?}"
        );
        assert_eq!(fact.observed_at, None, "{error:?}");
        assert!(
            attempts[0].pull_request_detail.reached_provider(),
            "{error:?}: the provider was asked, and the report must say so"
        );
        assert_ne!(attempts[0].pull_request_detail.tag(), "answered");
    }
}

/// An answered absence and an unavailable provider are different values, and the
/// difference survives the trip onto the graph. §10, stated as a test.
#[test]
fn an_answered_absence_is_not_an_unavailable_provider() {
    let (mut answered, _a) = graph_for("none-observed", &Local::quiet(), settled_branch());
    let subjects = FixedSubject;
    let says_none = SayingPullRequest(Ok(PullRequestState::NoneObserved));
    let answered_attempts = answered.attach_external_context(
        &ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&says_none),
            tasks: None,
        },
        now(),
    );

    let (mut refused, _b) = graph_for("unavailable", &Local::quiet(), settled_branch());
    let cannot_say = SayingPullRequest(Err(ExternalProviderError::TimedOut));
    let refused_attempts = refused.attach_external_context(
        &ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&cannot_say),
            tasks: None,
        },
        now(),
    );

    assert_eq!(
        worktree(&answered).external.pull_request.outcome,
        ProbeOutcome::Observed(PullRequestState::NoneObserved)
    );
    assert_eq!(
        worktree(&refused).external.pull_request.outcome,
        ProbeOutcome::Unavailable(ProbeReason::TimedOut)
    );
    assert_ne!(
        answered_attempts[0].pull_request_detail.tag(),
        refused_attempts[0].pull_request_detail.tag()
    );
    // The answered one is stamped; the unavailable one has no observation to be
    // fresh or stale.
    assert_eq!(
        worktree(&answered).external.pull_request.observed_at,
        Some(now())
    );
    assert_eq!(worktree(&refused).external.pull_request.observed_at, None);
}

/// A branch probe that could not answer does not become a detached HEAD or a
/// missing key, and the graph shows the branch probe's own reason.
#[test]
fn an_unread_branch_reaches_the_graph_as_unread() {
    let (mut graph, _candidates) = graph_for(
        "unread-branch",
        &Local::quiet(),
        ProbeOutcome::Unavailable(ProbeReason::TimedOut),
    );

    let subjects = FixedSubject;
    let pulls = SayingPullRequest(Ok(PullRequestState::Merged));
    let tasks = SayingTask::new(Ok(TaskState::Done));
    let attempts = graph.attach_external_context(
        &ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: Some(&tasks),
        },
        now(),
    );

    assert!(
        tasks.asked.borrow().is_empty(),
        "nothing can be asked about a branch nobody read"
    );
    let external = &worktree(&graph).external;
    assert_eq!(
        external.pull_request.outcome,
        ProbeOutcome::Unavailable(ProbeReason::TimedOut)
    );
    assert_eq!(
        external.task.outcome,
        ProbeOutcome::Unavailable(ProbeReason::TimedOut)
    );
    assert_eq!(attempts[0].task_detail.qualifier(), Some("git_unavailable"));
    // And an unread branch leaves unique work unknown, never absent.
    assert_eq!(
        worktree(&graph).lifecycle.unique_work(),
        UniqueWork::Unknown
    );
}

/// A worktree whose remote is on another forge is not asked about, and that is
/// not a failure. Prevents a corporate repository's owner and branch being sent
/// to github.com to earn a confident `404`.
#[test]
fn a_repository_on_another_host_is_not_asked_about() {
    struct ElsewhereSubject;
    impl SubjectResolver for ElsewhereSubject {
        fn repository_branch(
            &self,
            _worktree: &Path,
            _branch: Option<&str>,
        ) -> Result<RepositoryBranchSubject, SubjectUnknown> {
            Ok(RepositoryBranchSubject {
                host: "git.example.internal".to_string(),
                owner: "platform".to_string(),
                repo: "billing".to_string(),
                branch: BRANCH.to_string(),
            })
        }
    }

    let (mut graph, _candidates) = graph_for("other-host", &Local::quiet(), settled_branch());
    let subjects = ElsewhereSubject;
    let pulls = SayingPullRequest(Ok(PullRequestState::Merged));
    let attempts = graph.attach_external_context(
        &ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        },
        now(),
    );

    assert_eq!(attempts[0].pull_request_detail.tag(), "host_not_served");
    assert!(!attempts[0].pull_request_detail.reached_provider());
    assert_eq!(
        worktree(&graph).external.pull_request.outcome,
        ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
    );
}

/// AC 4, end to end: a descriptive branch name reaches no work tracker.
#[test]
fn a_descriptive_branch_name_correlates_to_no_task() {
    let branch = ProbeOutcome::Observed(WorktreeBranchState {
        branch: Some("fix-storage-stuff".to_string()),
        upstream: UpstreamState::Untracked,
        merged: MergedState::Unknown,
        integration: IntegrationEvidence::not_attempted(),
    });
    let (mut graph, _candidates) = graph_for("fuzzy", &Local::quiet(), branch);

    let subjects = FixedSubject;
    let tasks = SayingTask::new(Ok(TaskState::Done));
    let attempts = graph.attach_external_context(
        &ExternalContextResolver {
            subjects: &subjects,
            pull_requests: None,
            tasks: Some(&tasks),
        },
        now(),
    );

    assert!(
        tasks.asked.borrow().is_empty(),
        "a branch with no key must reach no work tracker at all"
    );
    assert_eq!(attempts[0].task_detail.tag(), "no_task_key");
    assert_eq!(
        worktree(&graph).external.task.outcome,
        ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
    );
    // No upstream, so the local work may exist only here — and a `Done` ticket
    // could not have argued with that even if one had been found.
    assert_eq!(
        worktree(&graph).lifecycle.unique_work(),
        UniqueWork::Present
    );
}
