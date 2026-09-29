//! Running one allowlisted read-only probe (HORO-1549).
//!
//! # What a model is allowed to cause to happen
//!
//! One of seven questions, about one thing it was already shown. That is the
//! whole surface, and it is a surface of *choices* rather than of inputs:
//! [`super::ProbeId`] is a payload-free enum, [`super::ProbeSubject`] comes out
//! of a table this machine built, and everything a probe actually needs — the
//! program to run, its arguments, the directory to run it in, the timeout — is
//! supplied here. Campaign section 16 lists what provider output may never
//! contain: an executable program, an argv, a shell string, a filesystem path,
//! a URL, a credential. None of those has a place to enter, because no function
//! in this module takes a string from a response.
//!
//! # Why the dispatch is over pairs and not over the probe alone
//!
//! [`super::ProbeId::subject_kinds`] already says which kinds each probe can be
//! asked about, and [`super::validate`] already drops a request that names the
//! wrong kind. This module matches on `(probe, subject)` regardless, and its
//! fallthrough arm is a refusal rather than an `unreachable!`. Two checks of
//! the same rule in two places is the intended arrangement: a future probe
//! added to the enum and forgotten here does not run something plausible
//! against the wrong directory, it returns
//! [`ProbeFindingView::Unavailable`].
//!
//! # Every probe here is read-only, and none of them mutates git
//!
//! No fetch, no pull, no merge, no checkout, no gc. The branch and patch
//! probes read what is already on disk, which is why a `merged` answer can be
//! stale and is reported as a measurement rather than as permission. Campaign
//! section 9 is explicit that answering a question is not a reason to change
//! the repository.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::contract::ProbeId;
use super::dto::{ProbeFindingView, ProbeResultView, Reported};
use super::project::{
    activity_view, branch_view, external_view, patch_equivalence_unavailable,
    patch_equivalence_view, workflow_history_view,
};
use super::ProbeSubject;
use crate::evidence::correlate::{GitProbe, ProcessCwdProbe, ToolLivenessProbe};
use crate::evidence::{ProbeOutcome, ProbeReason, ResourceLocator};
use crate::workspace::external::ExternalContextResolver;
use crate::workspace::history;
use crate::workspace::{ActivityFacts, BranchLifecycle, BranchProbe};

/// Answers one evidence request.
///
/// A trait so the loop can be driven by a runner that answers from a fixture:
/// the bound-enforcement and injection tests have to be able to run hundreds of
/// probes without a `git` process, and a test that needed real ones would be a
/// test nobody runs.
pub trait ProbeRunner {
    /// `round` is echoed into the finding so the model can see a re-read as a
    /// re-read. Never returns an error: a probe that could not answer is a
    /// finding, which is campaign section 16's "probe failure is returned as
    /// failure/unknown evidence and cannot be interpreted as absence".
    fn run(
        &self,
        round: u32,
        probe: ProbeId,
        subject_ref: &str,
        subject: &ProbeSubject,
        now: SystemTime,
    ) -> ProbeResultView;
}

/// The real runner: each probe id wired to the deterministic read-only probe
/// that already answers that question for a snapshot.
///
/// Deliberately the *same* probes and the *same* projection functions the first
/// round used. A second implementation of "is this tree dirty" would be a
/// second chance for the two rounds to disagree about what they measured, and a
/// model shown both would have no way to tell a changed fact from a differently
/// computed one.
pub struct LocalProbeRunner<'a> {
    git: &'a dyn GitProbe,
    branch: &'a dyn BranchProbe,
    processes: &'a dyn ProcessCwdProbe,
    tools: &'a dyn ToolLivenessProbe,
    external: &'a ExternalContextResolver<'a>,
    /// How long any one probe may take. Not a model input.
    timeout: Duration,
}

impl<'a> LocalProbeRunner<'a> {
    pub fn new(
        git: &'a dyn GitProbe,
        branch: &'a dyn BranchProbe,
        processes: &'a dyn ProcessCwdProbe,
        tools: &'a dyn ToolLivenessProbe,
        external: &'a ExternalContextResolver<'a>,
        timeout: Duration,
    ) -> Self {
        Self {
            git,
            branch,
            processes,
            tools,
            external,
            timeout,
        }
    }

    /// Both git probes over one worktree, folded into the lifecycle the
    /// projection already knows how to describe.
    ///
    /// Runs [`GitProbe`] as well as [`BranchProbe`] because
    /// [`super::dto::BranchView`]'s `dirty` and `untracked` come from the
    /// first, and there is no honest value for them if it does not answer —
    /// see [`ProbeFindingView::Unavailable`].
    fn branch_lifecycle(&self, root: &Path) -> Result<BranchLifecycle, ProbeReason> {
        let state = match self.git.state_of(root, self.timeout) {
            ProbeOutcome::Observed(Some(state)) => state,
            // The probe ran and this is not a working tree. Not a failure, and
            // not a branch state either: there is nothing here to describe.
            // `NotAttempted` rather than `Failed`, because nothing failed.
            ProbeOutcome::Observed(None) => return Err(ProbeReason::NotAttempted),
            ProbeOutcome::Unavailable(reason) => return Err(reason),
        };
        Ok(BranchLifecycle {
            dirty: state.dirty,
            untracked: state.untracked,
            branch: self.branch.state_of(root, self.timeout),
        })
    }
}

impl ProbeRunner for LocalProbeRunner<'_> {
    fn run(
        &self,
        round: u32,
        probe: ProbeId,
        subject_ref: &str,
        subject: &ProbeSubject,
        now: SystemTime,
    ) -> ProbeResultView {
        let finding = match (probe, subject) {
            (ProbeId::GitBranchState, ProbeSubject::Worktree { root }) => {
                match self.branch_lifecycle(root) {
                    Ok(lifecycle) => {
                        ProbeFindingView::BranchState(Box::new(branch_view(&lifecycle, now)))
                    }
                    Err(reason) => ProbeFindingView::Unavailable {
                        reason: reason.tag(),
                    },
                }
            }
            (ProbeId::GitPatchEquivalence, ProbeSubject::Worktree { root }) => {
                // The narrower question gets the narrower answer. The branch
                // probe measures integration as part of one run, so this reads
                // the same measurement and sends only the part that was asked
                // about — less egress for a request that wanted less.
                match self.branch.state_of(root, self.timeout) {
                    ProbeOutcome::Observed(state) => ProbeFindingView::PatchEquivalence(Box::new(
                        patch_equivalence_view(&state.integration, now),
                    )),
                    ProbeOutcome::Unavailable(reason) => ProbeFindingView::PatchEquivalence(
                        Box::new(patch_equivalence_unavailable(reason.tag())),
                    ),
                }
            }
            (ProbeId::ProcessActivity, ProbeSubject::Worktree { root }) => {
                ProbeFindingView::Activity(activity_view(&ActivityFacts::from_one_probe(
                    &self.processes.processes_with_cwd_under(root, self.timeout),
                )))
            }
            (ProbeId::ProcessActivity, ProbeSubject::Resource { resource }) => {
                match &resource.locator {
                    ResourceLocator::Path(path) => {
                        ProbeFindingView::Activity(activity_view(&ActivityFacts::from_one_probe(
                            &self.processes.processes_with_cwd_under(path, self.timeout),
                        )))
                    }
                    // A tool-native resource — a Docker volume, say — has no
                    // directory for a process to sit in. Refused rather than
                    // answered `idle`, which would be the false negative
                    // campaign section 13 names.
                    ResourceLocator::Tool { .. } => ProbeFindingView::Unavailable {
                        reason: ProbeReason::NotAttempted.tag(),
                    },
                }
            }
            (ProbeId::ToolLiveness, ProbeSubject::Resource { resource }) => {
                let liveness = self
                    .tools
                    .is_running(resource.kind.owning_tool(), self.timeout);
                ProbeFindingView::ToolLiveness(match liveness {
                    ProbeOutcome::Observed(running) => Reported::observed(running),
                    ProbeOutcome::Unavailable(reason) => Reported::unavailable(reason.tag()),
                })
            }
            (ProbeId::GithubPrState, ProbeSubject::Worktree { root }) => {
                let branch = self.branch_probe_branch(root);
                let resolved = self
                    .external
                    .resolve_branch_outcome(root, branch_ref(&branch), now);
                ProbeFindingView::PullRequest(external_view(
                    &resolved.context.pull_request,
                    |state| state.tag(),
                    now,
                ))
            }
            (ProbeId::JiraTaskState, ProbeSubject::Worktree { root }) => {
                let branch = self.branch_probe_branch(root);
                let resolved = self
                    .external
                    .resolve_branch_outcome(root, branch_ref(&branch), now);
                ProbeFindingView::Task(external_view(
                    &resolved.context.task,
                    |state| state.tag(),
                    now,
                ))
            }
            (ProbeId::WorkspaceHistorySummary, ProbeSubject::WorkflowHistory) => {
                match history::store::default_history_path() {
                    Ok(path) => {
                        let state = history::store::read(&path);
                        ProbeFindingView::WorkflowHistory(workflow_history_view(
                            &history::classify::classify(&state, unix_secs(now)),
                        ))
                    }
                    Err(_) => ProbeFindingView::Unavailable {
                        reason: ProbeReason::Failed.tag(),
                    },
                }
            }
            // Every pair `subject_kinds` does not allow, and every pair a
            // future probe forgets to wire. `validate` should have dropped
            // this request before it reached here; if it did not, nothing runs.
            _ => ProbeFindingView::Unavailable {
                reason: ProbeReason::NotAttempted.tag(),
            },
        };

        ProbeResultView {
            round,
            probe_id: probe.tag(),
            subject_ref: subject_ref.to_string(),
            finding,
        }
    }
}

impl LocalProbeRunner<'_> {
    /// The branch name, as an outcome rather than an option.
    ///
    /// [`ExternalContextResolver::resolve_branch_outcome`] wants exactly this
    /// distinction: a detached HEAD is a deliberate state, an unread branch
    /// probe is a question nobody answered, and correlating a pull request on
    /// the second as though it were the first would report "no pull request"
    /// for a branch that may well have one.
    fn branch_probe_branch(&self, root: &Path) -> ProbeOutcome<Option<String>> {
        match self.branch.state_of(root, self.timeout) {
            ProbeOutcome::Observed(state) => ProbeOutcome::Observed(state.branch),
            ProbeOutcome::Unavailable(reason) => ProbeOutcome::Unavailable(reason),
        }
    }
}

/// Borrows an owned branch outcome without flattening its three states into
/// two.
fn branch_ref(branch: &ProbeOutcome<Option<String>>) -> ProbeOutcome<Option<&str>> {
    match branch {
        ProbeOutcome::Observed(name) => ProbeOutcome::Observed(name.as_deref()),
        ProbeOutcome::Unavailable(reason) => ProbeOutcome::Unavailable(*reason),
    }
}

/// Seconds since the epoch, or `0` for a clock before it.
///
/// `0` rather than a panic: [`history::classify::classify`] reads this as an
/// observation age, and the worst a zero can do is make every stored
/// observation look old — which pushes the baseline towards `unknown`, the
/// direction that claims less.
fn unix_secs(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::correlate::{GitState, ProcessRef};
    use crate::evidence::{ResourceId, ResourceKind};
    use crate::workspace::external::{RepositoryBranchSubject, SubjectResolver, SubjectUnknown};
    use crate::workspace::{IntegrationEvidence, MergedState, UpstreamState, WorktreeBranchState};
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    /// Never touched: every probe below is driven by a double, so no path here
    /// is ever opened.
    fn worktree() -> ProbeSubject {
        ProbeSubject::Worktree {
            root: PathBuf::from("/nonexistent-path-for-a-test"),
        }
    }

    fn resource_subject(kind: ResourceKind) -> ProbeSubject {
        ProbeSubject::Resource {
            resource: ResourceId::new(
                kind,
                ResourceLocator::Path(PathBuf::from("/nonexistent-path-for-a-test")),
            ),
        }
    }

    struct FakeGit {
        answer: ProbeOutcome<Option<GitState>>,
        /// Every path this probe was handed, so a test can assert the runner
        /// did not invent one.
        asked: RefCell<Vec<PathBuf>>,
    }

    impl FakeGit {
        fn observing(dirty: bool, untracked: bool) -> Self {
            Self {
                answer: ProbeOutcome::Observed(Some(GitState {
                    repo_root: PathBuf::from("/nonexistent-path-for-a-test"),
                    common_dir: PathBuf::from("/nonexistent-path-for-a-test/.git"),
                    dirty,
                    untracked,
                    worktree: true,
                })),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn failing(reason: ProbeReason) -> Self {
            Self {
                answer: ProbeOutcome::Unavailable(reason),
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl GitProbe for FakeGit {
        fn state_of(&self, path: &Path, _: Duration) -> ProbeOutcome<Option<GitState>> {
            self.asked.borrow_mut().push(path.to_path_buf());
            self.answer.clone()
        }
    }

    struct FakeBranch {
        answer: ProbeOutcome<WorktreeBranchState>,
        calls: RefCell<u32>,
    }

    impl FakeBranch {
        fn observing(ahead: u32) -> Self {
            Self {
                calls: RefCell::new(0),
                answer: ProbeOutcome::Observed(WorktreeBranchState {
                    branch: Some("a-branch-name-that-must-not-leave".to_string()),
                    upstream: UpstreamState::Tracking { ahead, behind: 0 },
                    merged: MergedState::Unknown,
                    integration: IntegrationEvidence::not_attempted(),
                }),
            }
        }

        fn failing(reason: ProbeReason) -> Self {
            Self {
                answer: ProbeOutcome::Unavailable(reason),
                calls: RefCell::new(0),
            }
        }
    }

    impl BranchProbe for FakeBranch {
        fn state_of(&self, _: &Path, _: Duration) -> ProbeOutcome<WorktreeBranchState> {
            *self.calls.borrow_mut() += 1;
            self.answer.clone()
        }
    }

    struct FakeProcesses {
        answer: ProbeOutcome<Vec<ProcessRef>>,
        calls: RefCell<u32>,
    }

    impl ProcessCwdProbe for FakeProcesses {
        fn processes_with_cwd_under(&self, _: &Path, _: Duration) -> ProbeOutcome<Vec<ProcessRef>> {
            *self.calls.borrow_mut() += 1;
            self.answer.clone()
        }
    }

    struct FakeTools {
        answer: ProbeOutcome<bool>,
        asked: RefCell<Vec<crate::evidence::OwningTool>>,
    }

    impl FakeTools {
        fn answering(answer: ProbeOutcome<bool>) -> Self {
            Self {
                answer,
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl ToolLivenessProbe for FakeTools {
        fn is_running(&self, tool: crate::evidence::OwningTool, _: Duration) -> ProbeOutcome<bool> {
            self.asked.borrow_mut().push(tool);
            self.answer.clone()
        }
    }

    struct NoSubjects;

    impl SubjectResolver for NoSubjects {
        fn repository_branch(
            &self,
            _: &Path,
            _: Option<&str>,
        ) -> Result<RepositoryBranchSubject, SubjectUnknown> {
            Err(SubjectUnknown::NoRemote)
        }
    }

    /// One assembled runner, with every double under the caller's control.
    struct Rig {
        git: FakeGit,
        branch: FakeBranch,
        processes: FakeProcesses,
        tools: FakeTools,
        subjects: NoSubjects,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                git: FakeGit::observing(false, false),
                branch: FakeBranch::observing(0),
                processes: FakeProcesses {
                    answer: ProbeOutcome::Observed(Vec::new()),
                    calls: RefCell::new(0),
                },
                tools: FakeTools::answering(ProbeOutcome::Observed(true)),
                subjects: NoSubjects,
            }
        }

        fn run(
            &self,
            probe: ProbeId,
            subject_ref: &str,
            subject: &ProbeSubject,
        ) -> ProbeResultView {
            let external = ExternalContextResolver::disabled(&self.subjects);
            let runner = LocalProbeRunner::new(
                &self.git,
                &self.branch,
                &self.processes,
                &self.tools,
                &external,
                Duration::from_secs(1),
            );
            runner.run(1, probe, subject_ref, subject, now())
        }
    }

    fn finding_tag(finding: &ProbeFindingView) -> &'static str {
        match finding {
            ProbeFindingView::BranchState(_) => "branch_state",
            ProbeFindingView::PatchEquivalence(_) => "patch_equivalence",
            ProbeFindingView::Activity(_) => "activity",
            ProbeFindingView::ToolLiveness(_) => "tool_liveness",
            ProbeFindingView::PullRequest(_) => "pull_request",
            ProbeFindingView::Task(_) => "task",
            ProbeFindingView::WorkflowHistory(_) => "workflow_history",
            ProbeFindingView::Unavailable { .. } => "unavailable",
        }
    }

    /// AC 6, and the reason `Unavailable` is a finding rather than an absent
    /// entry. A git probe that could not run must not leave `dirty: false`
    /// behind, which would be a failed probe read as "nothing here".
    #[test]
    fn a_failed_git_probe_is_unavailable_and_not_a_clean_tree() {
        let mut rig = Rig::new();
        rig.git = FakeGit::failing(ProbeReason::PermissionDenied);

        let result = rig.run(ProbeId::GitBranchState, "workspace_1", &worktree());

        assert_eq!(
            result.finding,
            ProbeFindingView::Unavailable {
                reason: "permission_denied"
            }
        );

        // Positive control: the same runner with an answering git probe does
        // produce a branch state, so the assertion above is discriminating.
        let answering = Rig::new().run(ProbeId::GitBranchState, "workspace_1", &worktree());
        assert_eq!(finding_tag(&answering.finding), "branch_state");
    }

    /// A directory that is genuinely not a working tree is `not_attempted`,
    /// not `failed`. Nothing broke — there was nothing to read.
    #[test]
    fn a_path_that_is_no_working_tree_is_not_a_failure() {
        let mut rig = Rig::new();
        rig.git = FakeGit {
            answer: ProbeOutcome::Observed(None),
            asked: RefCell::new(Vec::new()),
        };

        let result = rig.run(ProbeId::GitBranchState, "workspace_1", &worktree());

        assert_eq!(
            result.finding,
            ProbeFindingView::Unavailable {
                reason: "not_attempted"
            }
        );
    }

    /// Every probe answers about the subject it was given, and the answer is
    /// the shape that probe promises. Driven over all seven so a probe added
    /// to the enum and left unwired shows up here.
    #[test]
    fn every_probe_answers_in_its_own_shape() {
        let rig = Rig::new();
        let expected = [
            (ProbeId::GitBranchState, worktree(), "branch_state"),
            (
                ProbeId::GitPatchEquivalence,
                worktree(),
                "patch_equivalence",
            ),
            (ProbeId::ProcessActivity, worktree(), "activity"),
            (
                ProbeId::ToolLiveness,
                resource_subject(ResourceKind::CargoTargetDir),
                "tool_liveness",
            ),
            (ProbeId::GithubPrState, worktree(), "pull_request"),
            (ProbeId::JiraTaskState, worktree(), "task"),
            (
                ProbeId::WorkspaceHistorySummary,
                ProbeSubject::WorkflowHistory,
                "workflow_history",
            ),
        ];

        for (probe, subject, tag) in &expected {
            let result = rig.run(*probe, "subject_1", subject);
            assert_eq!(result.probe_id, probe.tag());
            assert_eq!(result.subject_ref, "subject_1");
            assert_eq!(result.round, 1);
            assert_eq!(
                finding_tag(&result.finding),
                *tag,
                "{} answered in the wrong shape",
                probe.tag()
            );
        }

        // Non-vacuity: the table covers the whole registry, so an eighth probe
        // cannot be added without being wired.
        let covered: Vec<&str> = expected.iter().map(|(p, _, _)| p.tag()).collect();
        for probe in ProbeId::ALL {
            assert!(
                covered.contains(&probe.tag()),
                "{} is untested",
                probe.tag()
            );
        }
    }

    /// The wrong kind of subject runs nothing. `validate` drops these before
    /// they get here; this asserts the second check, which is what a future
    /// probe added to the enum and forgotten in the dispatch falls through to.
    #[test]
    fn a_probe_about_the_wrong_kind_of_subject_runs_nothing() {
        let rig = Rig::new();
        let wrong = [
            (ProbeId::GitBranchState, ProbeSubject::Machine),
            (
                ProbeId::GitPatchEquivalence,
                resource_subject(ResourceKind::CargoTargetDir),
            ),
            (ProbeId::ToolLiveness, worktree()),
            (ProbeId::GithubPrState, ProbeSubject::WorkflowHistory),
            (ProbeId::WorkspaceHistorySummary, worktree()),
            (
                ProbeId::GitBranchState,
                ProbeSubject::Repository {
                    common_dir: PathBuf::from("/nonexistent-path-for-a-test/.git"),
                },
            ),
        ];

        for (probe, subject) in &wrong {
            let result = rig.run(*probe, "subject_1", subject);
            assert_eq!(
                result.finding,
                ProbeFindingView::Unavailable {
                    reason: "not_attempted"
                },
                "{} ran against {subject:?}",
                probe.tag()
            );
        }

        // Nothing reached any probe for any of them. All four, because a
        // refusal that only spared `git` would still have shelled out.
        assert!(
            rig.git.asked.borrow().is_empty(),
            "a refused pair still opened a path: {:?}",
            rig.git.asked.borrow()
        );
        assert_eq!(*rig.branch.calls.borrow(), 0, "the branch probe ran");
        assert_eq!(*rig.processes.calls.borrow(), 0, "the process probe ran");
        assert!(rig.tools.asked.borrow().is_empty(), "the tool probe ran");
    }

    /// `tool_liveness` reads the owning tool off the resource rather than off
    /// anything the model said.
    #[test]
    fn tool_liveness_asks_about_the_tool_the_resource_identity_names() {
        let rig = Rig::new();

        let result = rig.run(
            ProbeId::ToolLiveness,
            "resource_1",
            &resource_subject(ResourceKind::DockerVolume),
        );

        assert_eq!(finding_tag(&result.finding), "tool_liveness");
        assert_eq!(
            *rig.tools.asked.borrow(),
            vec![ResourceKind::DockerVolume.owning_tool()]
        );
    }

    /// A resource with no directory cannot be asked whether a process is in
    /// it, and is refused rather than reported idle.
    #[test]
    fn a_tool_located_resource_has_no_directory_to_probe_for_processes() {
        let rig = Rig::new();
        let volume = ProbeSubject::Resource {
            resource: ResourceId::new(
                ResourceKind::DockerVolume,
                ResourceLocator::Tool {
                    tool: ResourceKind::DockerVolume.owning_tool(),
                    id: "a-volume-name".to_string(),
                },
            ),
        };

        let result = rig.run(ProbeId::ProcessActivity, "resource_1", &volume);

        assert_eq!(
            result.finding,
            ProbeFindingView::Unavailable {
                reason: "not_attempted"
            }
        );

        // Positive control: a path-located resource of the same probe DOES get
        // an activity answer, so the refusal above is about the locator.
        let path_located = rig.run(
            ProbeId::ProcessActivity,
            "resource_1",
            &resource_subject(ResourceKind::CargoTargetDir),
        );
        assert_eq!(finding_tag(&path_located.finding), "activity");
    }

    /// An unanswered process probe is one unanswered probe, and the tree is
    /// not idle. The single-probe fold, seen from here.
    #[test]
    fn an_unanswered_process_probe_is_not_an_idle_tree() {
        let mut rig = Rig::new();
        rig.processes = FakeProcesses {
            answer: ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
            calls: RefCell::new(0),
        };

        let result = rig.run(ProbeId::ProcessActivity, "workspace_1", &worktree());

        match &result.finding {
            ProbeFindingView::Activity(activity) => {
                assert_eq!(activity.unanswered_probe_count, 1);
                assert_eq!(activity.state, "unknown");
            }
            other => panic!("expected an activity finding, got {other:?}"),
        }

        // Positive control: an answered empty probe IS idle with nothing
        // unanswered, so the counts above are not constants.
        let answered = Rig::new().run(ProbeId::ProcessActivity, "workspace_1", &worktree());
        match &answered.finding {
            ProbeFindingView::Activity(activity) => {
                assert_eq!(activity.unanswered_probe_count, 0);
                assert_eq!(activity.state, "idle");
            }
            other => panic!("expected an activity finding, got {other:?}"),
        }
    }

    /// A failed branch probe still produces a patch-equivalence finding, with
    /// every field carrying the reason — the narrow question's version of
    /// "failed is not empty".
    #[test]
    fn a_failed_branch_probe_reports_a_reason_on_every_equivalence_field() {
        let mut rig = Rig::new();
        rig.branch = FakeBranch::failing(ProbeReason::TimedOut);

        let result = rig.run(ProbeId::GitPatchEquivalence, "workspace_1", &worktree());

        match &result.finding {
            ProbeFindingView::PatchEquivalence(fields) => {
                assert_eq!(
                    fields.patch_equivalence.unavailable_reason,
                    Some("timed_out")
                );
                assert_eq!(
                    fields.commits_unique_to_head.unavailable_reason,
                    Some("timed_out")
                );
                assert_eq!(fields.commits_unique_to_head.value, None);
                assert_eq!(
                    fields.head_tip_age_days.unavailable_reason,
                    Some("timed_out")
                );
            }
            other => panic!("expected a patch-equivalence finding, got {other:?}"),
        }
    }

    /// AC 9's other half, from the runner's side: nothing a probe returns
    /// carries local identity onto the wire. The branch name the doubles hold
    /// is deliberately distinctive.
    #[test]
    fn no_finding_carries_a_path_or_a_branch_name() {
        let rig = Rig::new();

        for probe in ProbeId::ALL {
            for subject in [
                worktree(),
                resource_subject(ResourceKind::CargoTargetDir),
                ProbeSubject::WorkflowHistory,
                ProbeSubject::Machine,
            ] {
                let result = rig.run(probe, "subject_1", &subject);
                let body = serde_json::to_string(&result).expect("a finding serializes");
                assert!(
                    !body.contains('/'),
                    "{} leaked a path separator: {body}",
                    probe.tag()
                );
                assert!(
                    !body.contains("a-branch-name-that-must-not-leave"),
                    "{} leaked a branch name: {body}",
                    probe.tag()
                );
                assert!(
                    !body.contains("nonexistent-path-for-a-test"),
                    "{} leaked a path segment: {body}",
                    probe.tag()
                );
            }
        }
    }
}
