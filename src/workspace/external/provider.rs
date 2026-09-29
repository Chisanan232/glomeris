//! The optional seam, and the resolution that fills
//! [`crate::workspace::ExternalContext`] (HORO-1546).
//!
//! # `Option<&dyn …>`, not a null provider
//!
//! AC 1 is that external context is behind a provider interface and is
//! optional. Both providers are `Option<&dyn …>` on [`ExternalContextResolver`]
//! rather than trait objects with do-nothing implementations, so "no provider
//! is configured" is a shape the compiler can see and every caller has to pass.
//! A null implementation would have made an absent provider indistinguishable
//! from one that answered nothing, and that distinction is the whole of §10.
//!
//! # Two levels of answer, again
//!
//! [`ExternalContextResolver::resolve`] returns both the
//! [`crate::workspace::ExternalContext`] that may travel — bounded state tokens
//! and coarse [`crate::evidence::ProbeReason`]s — and an
//! [`ExternalDetail`] per fact that stays local. The detail is what a person
//! reading `glomeris` output needs in order to act: "no provider is
//! configured", "this worktree's remote is on another host" and "the token was
//! refused" send them to three different places, and none of those three
//! belongs in a model prompt describing this machine's setup.
//!
//! # Nothing here decides anything
//!
//! A provider answering [`crate::workspace::PullRequestState::Merged`] changes
//! one field on a worktree node and nothing else. It does not touch
//! [`crate::workspace::BranchLifecycle::unique_work`], it does not reach
//! [`crate::policy`], and `scripts/check-workspace-aggregation-has-no-authority.sh`
//! keeps it that way.

use std::path::Path;
use std::time::SystemTime;

use super::error::ExternalProviderError;
use super::subject::{
    task_key_from_branch, RepositoryBranchSubject, SubjectResolver, SubjectUnknown, TaskKey,
};
use crate::evidence::ProbeReason;
use crate::workspace::{
    ExternalContext, ExternalFact, ExternalSource, PullRequestState, TaskState,
};

/// A read-only source of pull-request state for an exact repository and branch.
pub trait PullRequestProvider {
    /// The host this provider answers for, lowercase.
    ///
    /// Exists so a worktree whose remote is somewhere else is not asked about.
    /// Without it, a machine with one GitHub provider configured would send a
    /// corporate repository's owner and branch to github.com and get a
    /// confident `404` about a repository that was never there — a request that
    /// leaks an internal project name to a service that had no business
    /// hearing it.
    fn host(&self) -> &str;

    /// The state of a pull request for this exact subject.
    ///
    /// `Ok(NoneObserved)` means the provider answered and there is none.
    /// Anything the provider could not answer is an `Err`.
    fn pull_request_state(
        &self,
        subject: &RepositoryBranchSubject,
    ) -> Result<PullRequestState, ExternalProviderError>;
}

/// A read-only source of work-item state for an explicit key.
pub trait TaskProvider {
    /// The state of the work item with this exact key.
    fn task_state(&self, key: &TaskKey) -> Result<TaskState, ExternalProviderError>;
}

/// Why one external fact came out the way it did — for local surfaces only.
///
/// Never serialized into a model payload, and nothing in
/// [`crate::planner::dto`] can name it. The coarse
/// [`crate::evidence::ProbeReason`] on the fact itself is what travels.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalDetail {
    /// No provider was supplied. The default state of the product.
    ProviderDisabled,
    /// A provider is configured, and this worktree has no subject to ask about.
    NoSubject(SubjectUnknown),
    /// The worktree's remote is on a host this provider does not serve. Not a
    /// fault and not a misconfiguration — one machine may hold repositories
    /// from several forges.
    HostNotServed,
    /// A provider is configured and the branch carries no explicit issue key.
    /// The expected outcome for most branches, and the correct one: AC 4 forbids
    /// inventing an association.
    NoTaskKey,
    /// The provider answered. What it said is on the fact.
    Answered,
    /// The provider was asked and could not answer.
    Failed(ExternalProviderError),
}

impl ExternalDetail {
    /// A stable snake_case token for local surfaces. Sole producer.
    ///
    /// [`Self::Failed`] delegates to the error's own tag rather than adding a
    /// `failed` bucket, because the four distinctions AC 3 is about live there
    /// and flattening them here would undo the taxonomy one layer up.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::ProviderDisabled => "provider_disabled",
            Self::NoSubject(_) => "no_subject",
            Self::HostNotServed => "host_not_served",
            Self::NoTaskKey => "no_task_key",
            Self::Answered => "answered",
            Self::Failed(error) => error.tag(),
        }
    }

    /// The more specific local reason, where there is one.
    ///
    /// Separate from [`Self::tag`] so a report can print "no_subject
    /// (no_remote)" without this enum having to enumerate the cross product.
    pub fn qualifier(&self) -> Option<&'static str> {
        match self {
            Self::NoSubject(unknown) => Some(unknown.tag()),
            _ => None,
        }
    }

    /// Whether anything was actually asked of a service.
    ///
    /// Reported honestly rather than inferred from the fact, because
    /// "unavailable" covers both "we never asked" and "we asked and it broke",
    /// and a person deciding whether their credential works needs to know
    /// which.
    pub fn reached_provider(&self) -> bool {
        matches!(self, Self::Answered | Self::Failed(_))
    }
}

/// Both facts, plus the local detail behind each.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedExternalContext {
    /// The part that may travel to a model, unchanged in shape from
    /// [`crate::workspace::ExternalContext`].
    pub context: ExternalContext,
    pub pull_request_detail: ExternalDetail,
    pub task_detail: ExternalDetail,
}

impl ResolvedExternalContext {
    /// Nothing was configured, so nothing was asked.
    pub fn unqueried() -> Self {
        Self {
            context: ExternalContext::unqueried(),
            pull_request_detail: ExternalDetail::ProviderDisabled,
            task_detail: ExternalDetail::ProviderDisabled,
        }
    }
}

/// Fills one worktree's external context from whichever providers exist.
///
/// Borrows its providers, so a caller holding one concrete adapter does not
/// have to give up ownership to ask about twenty worktrees, and no provider is
/// constructed per worktree — which matters because constructing one reads the
/// environment for a credential.
pub struct ExternalContextResolver<'a> {
    /// How a worktree becomes a [`RepositoryBranchSubject`]. Injected rather
    /// than hard-wired to [`super::subject::GitSubjectResolver`] so the tests
    /// below can reach the host check, the answered-absence path and the
    /// refused-credential path without each first building a git repository
    /// with a remote — the git reading has its own real-git tests in
    /// [`super::subject`].
    pub subjects: &'a dyn SubjectResolver,
    pub pull_requests: Option<&'a dyn PullRequestProvider>,
    pub tasks: Option<&'a dyn TaskProvider>,
}

impl<'a> ExternalContextResolver<'a> {
    /// A resolver with no providers. Resolves everything to "not attempted"
    /// without running git, which is the path the product takes today.
    pub fn disabled(subjects: &'a dyn SubjectResolver) -> Self {
        Self {
            subjects,
            pull_requests: None,
            tasks: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.pull_requests.is_some() || self.tasks.is_some()
    }

    /// Asks each configured provider about one worktree.
    ///
    /// `branch` is the name the local branch probe already read, `None` for a
    /// detached HEAD. `now` stamps an observation; it is a parameter so a test
    /// can age a fact without waiting.
    pub fn resolve(
        &self,
        worktree: &Path,
        branch: Option<&str>,
        now: SystemTime,
    ) -> ResolvedExternalContext {
        if !self.is_enabled() {
            // Short-circuited before git runs. A disabled provider must cost
            // nothing at all, not even a process.
            return ResolvedExternalContext::unqueried();
        }
        let (pull_request, pull_request_detail) = self.resolve_pull_request(worktree, branch, now);
        let (task, task_detail) = self.resolve_task(branch, now);
        ResolvedExternalContext {
            context: ExternalContext { pull_request, task },
            pull_request_detail,
            task_detail,
        }
    }

    fn resolve_pull_request(
        &self,
        worktree: &Path,
        branch: Option<&str>,
        now: SystemTime,
    ) -> (ExternalFact<PullRequestState>, ExternalDetail) {
        const SOURCE: ExternalSource = ExternalSource::GitHubPullRequests;
        let Some(provider) = self.pull_requests else {
            return (
                ExternalFact::not_attempted(SOURCE),
                ExternalDetail::ProviderDisabled,
            );
        };
        let subject = match self.subjects.repository_branch(worktree, branch) {
            Ok(subject) => subject,
            Err(unknown) => {
                return (
                    ExternalFact::unavailable(SOURCE, unknown.reason()),
                    ExternalDetail::NoSubject(unknown),
                );
            }
        };
        if subject.host != provider.host() {
            return (
                // Nothing was asked, and nothing was wrong. `NotAttempted` is
                // the honest reason; `Failed` would have a user debugging a
                // provider that behaved correctly by staying silent.
                ExternalFact::unavailable(SOURCE, ProbeReason::NotAttempted),
                ExternalDetail::HostNotServed,
            );
        }
        match provider.pull_request_state(&subject) {
            Ok(state) => (
                ExternalFact::observed(SOURCE, state, now),
                ExternalDetail::Answered,
            ),
            Err(error) => (
                ExternalFact::unavailable(SOURCE, error.reason()),
                ExternalDetail::Failed(error),
            ),
        }
    }

    fn resolve_task(
        &self,
        branch: Option<&str>,
        now: SystemTime,
    ) -> (ExternalFact<TaskState>, ExternalDetail) {
        const SOURCE: ExternalSource = ExternalSource::JiraIssues;
        let Some(provider) = self.tasks else {
            return (
                ExternalFact::not_attempted(SOURCE),
                ExternalDetail::ProviderDisabled,
            );
        };
        let Some(key) = branch.and_then(task_key_from_branch) else {
            return (
                ExternalFact::unavailable(SOURCE, ProbeReason::NotAttempted),
                ExternalDetail::NoTaskKey,
            );
        };
        match provider.task_state(&key) {
            Ok(state) => (
                ExternalFact::observed(SOURCE, state, now),
                ExternalDetail::Answered,
            ),
            Err(error) => (
                ExternalFact::unavailable(SOURCE, error.reason()),
                ExternalDetail::Failed(error),
            ),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::evidence::ProbeOutcome;
    use std::cell::RefCell;
    use std::time::Duration;

    /// A subject resolver that answers from a value, so the provider paths are
    /// reachable without a repository.
    pub(crate) struct StubSubjects(Result<RepositoryBranchSubject, SubjectUnknown>);

    impl StubSubjects {
        pub(crate) fn on(host: &str, owner: &str, repo: &str, branch: &str) -> Self {
            Self(Ok(RepositoryBranchSubject {
                host: host.into(),
                owner: owner.into(),
                repo: repo.into(),
                branch: branch.into(),
            }))
        }

        pub(crate) fn unknown(unknown: SubjectUnknown) -> Self {
            Self(Err(unknown))
        }
    }

    impl SubjectResolver for StubSubjects {
        fn repository_branch(
            &self,
            _worktree: &Path,
            branch: Option<&str>,
        ) -> Result<RepositoryBranchSubject, SubjectUnknown> {
            // Still honours the detached case, because that decision belongs to
            // the real resolver too and a stub that ignored it would let the
            // resolver's own handling go untested.
            branch.ok_or(SubjectUnknown::DetachedHead)?;
            self.0.clone()
        }
    }

    struct StubPullRequests {
        host: String,
        answer: Result<PullRequestState, ExternalProviderError>,
        asked: RefCell<Vec<RepositoryBranchSubject>>,
    }

    impl StubPullRequests {
        fn on(host: &str, answer: Result<PullRequestState, ExternalProviderError>) -> Self {
            Self {
                host: host.into(),
                answer,
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl PullRequestProvider for StubPullRequests {
        fn host(&self) -> &str {
            &self.host
        }

        fn pull_request_state(
            &self,
            subject: &RepositoryBranchSubject,
        ) -> Result<PullRequestState, ExternalProviderError> {
            self.asked.borrow_mut().push(subject.clone());
            self.answer.clone()
        }
    }

    struct StubTasks {
        answer: Result<TaskState, ExternalProviderError>,
        asked: RefCell<Vec<String>>,
    }

    impl StubTasks {
        fn answering(answer: Result<TaskState, ExternalProviderError>) -> Self {
            Self {
                answer,
                asked: RefCell::new(Vec::new()),
            }
        }
    }

    impl TaskProvider for StubTasks {
        fn task_state(&self, key: &TaskKey) -> Result<TaskState, ExternalProviderError> {
            self.asked.borrow_mut().push(key.as_str().to_string());
            self.answer.clone()
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn worktree() -> &'static Path {
        Path::new("/nonexistent-path-for-a-test")
    }

    /// AC 1, and the state the product ships in: nothing configured, nothing
    /// asked, and no subject resolution attempted either.
    #[test]
    fn a_resolver_with_no_providers_asks_nothing() {
        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        let resolver = ExternalContextResolver::disabled(&subjects);

        let resolved = resolver.resolve(worktree(), Some("HORO-1546/feat/x"), now());

        assert!(!resolver.is_enabled());
        assert_eq!(resolved, ResolvedExternalContext::unqueried());
        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(resolved.pull_request_detail.tag(), "provider_disabled");
        assert_eq!(resolved.task_detail.tag(), "provider_disabled");
        assert!(!resolved.pull_request_detail.reached_provider());
    }

    /// One provider on and the other off. The off one must not borrow the on
    /// one's outcome — a common way for an optional feature to start reporting
    /// things it never looked at.
    #[test]
    fn one_provider_being_configured_says_nothing_about_the_other() {
        let subjects = StubSubjects::on("github.com", "Chisanan232", "glomeris", "trunk");
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Open));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        };

        let resolved = resolver.resolve(worktree(), Some("HORO-1546/feat/x"), now());

        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Observed(PullRequestState::Open)
        );
        assert_eq!(resolved.task_detail.tag(), "provider_disabled");
        assert_eq!(
            resolved.context.task.outcome,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    /// AC 5. The provider is asked about the exact subject git described, not a
    /// reconstruction of it.
    #[test]
    fn the_provider_is_asked_about_the_exact_repository_and_branch() {
        let subjects = StubSubjects::on("github.com", "Chisanan232", "glomeris", "trunk");
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Open));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        };

        resolver.resolve(worktree(), Some("trunk"), now());

        let asked = pulls.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].owner, "Chisanan232");
        assert_eq!(asked[0].repo, "glomeris");
        assert_eq!(asked[0].branch, "trunk");
    }

    /// §10, on the value rather than on a tag: a provider that answered
    /// "there is none" is an observation with a timestamp, and is not the same
    /// value as a provider that could not answer.
    #[test]
    fn an_answered_absence_is_observed_and_stamped() {
        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::NoneObserved));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        };

        let resolved = resolver.resolve(worktree(), Some("trunk"), now());

        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Observed(PullRequestState::NoneObserved)
        );
        assert_eq!(resolved.context.pull_request.observed_at, Some(now()));
        assert_eq!(resolved.pull_request_detail.tag(), "answered");
        assert!(resolved.pull_request_detail.reached_provider());
    }

    /// AC 3, and the other half of §10. Every way a provider can fail stays
    /// `Unavailable` with its own reason, and none of them becomes the answer
    /// `NoneObserved`.
    #[test]
    fn no_provider_failure_becomes_an_answered_absence() {
        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        let expected = [
            (
                ExternalProviderError::AuthRejected { status: 401 },
                "permission_denied",
                "auth_rejected",
            ),
            (
                ExternalProviderError::RateLimited,
                "rate_limited",
                "rate_limited",
            ),
            (ExternalProviderError::TimedOut, "timed_out", "timed_out"),
            (
                ExternalProviderError::Unreachable("refused".into()),
                "failed",
                "unreachable",
            ),
            (
                ExternalProviderError::UnexpectedStatus { status: 404 },
                "failed",
                "unexpected_status",
            ),
            (
                ExternalProviderError::UnusableResponse("not json".into()),
                "failed",
                "unusable_response",
            ),
        ];
        for (error, wire_reason, local_tag) in expected {
            let pulls = StubPullRequests::on("github.com", Err(error.clone()));
            let resolver = ExternalContextResolver {
                subjects: &subjects,
                pull_requests: Some(&pulls),
                tasks: None,
            };

            let resolved = resolver.resolve(worktree(), Some("trunk"), now());

            match resolved.context.pull_request.outcome {
                ProbeOutcome::Unavailable(reason) => {
                    assert_eq!(reason.tag(), wire_reason, "{error:?}");
                }
                ref other => panic!("{error:?} produced {other:?}"),
            }
            assert_eq!(resolved.context.pull_request.observed_at, None, "{error:?}");
            assert_eq!(resolved.pull_request_detail.tag(), local_tag);
            assert!(resolved.pull_request_detail.reached_provider(), "{error:?}");
        }
    }

    /// A repository on another forge is not sent to the configured one. The
    /// point is not tidiness: an internal project's owner and branch name
    /// arriving at github.com is an egress nobody asked for, and it happens
    /// without any credential being misused.
    #[test]
    fn a_subject_on_another_host_is_never_asked_about() {
        let subjects = StubSubjects::on("git.example.test", "team", "service", "trunk");
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Merged));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        };

        let resolved = resolver.resolve(worktree(), Some("trunk"), now());

        assert!(
            pulls.asked.borrow().is_empty(),
            "github.com was asked about {:?}",
            pulls.asked.borrow()
        );
        assert_eq!(resolved.pull_request_detail, ExternalDetail::HostNotServed);
        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    /// Every reason a subject could not be identified travels as "nothing was
    /// asked", keeps its local qualifier, and never reaches a provider.
    #[test]
    fn an_unidentifiable_subject_is_not_asked_about_and_is_not_a_failure() {
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Merged));
        for unknown in [
            SubjectUnknown::NoRemote,
            SubjectUnknown::AmbiguousRemote,
            SubjectUnknown::UnreadableRemoteUrl,
            SubjectUnknown::UnusableBranchName,
        ] {
            let subjects = StubSubjects::unknown(unknown.clone());
            let resolver = ExternalContextResolver {
                subjects: &subjects,
                pull_requests: Some(&pulls),
                tasks: None,
            };

            let resolved = resolver.resolve(worktree(), Some("trunk"), now());

            assert_eq!(
                resolved.context.pull_request.outcome,
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
                "{unknown:?}"
            );
            assert_eq!(resolved.pull_request_detail.tag(), "no_subject");
            assert_eq!(
                resolved.pull_request_detail.qualifier(),
                Some(unknown.tag())
            );
            assert!(!resolved.pull_request_detail.reached_provider());
        }
        assert!(pulls.asked.borrow().is_empty());
    }

    /// git missing is git's reason, not "nothing was asked" — a broken tool must
    /// not read as an ordinary local-only branch.
    #[test]
    fn git_being_unavailable_keeps_gits_own_reason() {
        let subjects =
            StubSubjects::unknown(SubjectUnknown::GitUnavailable(ProbeReason::ToolAbsent));
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Merged));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: None,
        };

        let resolved = resolver.resolve(worktree(), Some("trunk"), now());

        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Unavailable(ProbeReason::ToolAbsent)
        );
    }

    /// A detached HEAD is not asked about by either provider, and is not a
    /// failure.
    #[test]
    fn a_detached_head_is_not_attempted_rather_than_failed() {
        let subjects = StubSubjects::on("github.com", "o", "r", "trunk");
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Merged));
        let tasks = StubTasks::answering(Ok(TaskState::Done));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: Some(&tasks),
        };

        let resolved = resolver.resolve(worktree(), None, now());

        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            resolved.pull_request_detail.qualifier(),
            Some("detached_head")
        );
        assert_eq!(resolved.task_detail.tag(), "no_task_key");
        assert!(pulls.asked.borrow().is_empty());
        assert!(tasks.asked.borrow().is_empty());
    }

    /// AC 4. A descriptive branch name means the task provider is never asked,
    /// and the reason is "nothing was asked" rather than "no task exists".
    #[test]
    fn a_branch_with_no_key_never_reaches_the_task_provider() {
        let subjects = StubSubjects::on("github.com", "o", "r", "fix-storage-stuff");
        let tasks = StubTasks::answering(Ok(TaskState::Done));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: None,
            tasks: Some(&tasks),
        };

        let resolved = resolver.resolve(worktree(), Some("fix-storage-stuff"), now());

        assert_eq!(
            resolved.context.task.outcome,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(resolved.task_detail.tag(), "no_task_key");
        assert!(!resolved.task_detail.reached_provider());
        assert!(
            tasks.asked.borrow().is_empty(),
            "the provider was asked about {:?}",
            tasks.asked.borrow()
        );
    }

    /// The key that is found is the exact key from the branch, passed through
    /// unaltered.
    #[test]
    fn an_explicit_key_is_passed_through_verbatim() {
        let subjects = StubSubjects::on("github.com", "o", "r", "b");
        let tasks = StubTasks::answering(Ok(TaskState::InProgress));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: None,
            tasks: Some(&tasks),
        };

        let resolved = resolver.resolve(
            worktree(),
            Some("v0.0.1/HORO-1546/feat/external_context"),
            now(),
        );

        assert_eq!(*tasks.asked.borrow(), vec!["HORO-1546".to_string()]);
        assert_eq!(
            resolved.context.task.outcome,
            ProbeOutcome::Observed(TaskState::InProgress)
        );
        assert_eq!(resolved.task_detail.tag(), "answered");
    }

    /// A tracker's own status name is kept locally and is `other` on the wire,
    /// so a bespoke workflow is not forced into three buckets and its wording
    /// is not shipped off the machine either.
    #[test]
    fn an_unmapped_task_status_keeps_its_name_locally_and_is_other_on_the_wire() {
        let subjects = StubSubjects::on("github.com", "o", "r", "b");
        let tasks = StubTasks::answering(Ok(TaskState::Other("DEV VERIFY".into())));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: None,
            tasks: Some(&tasks),
        };

        let resolved = resolver.resolve(worktree(), Some("HORO-1546/feat/x"), now());

        match &resolved.context.task.outcome {
            ProbeOutcome::Observed(state) => {
                assert_eq!(state, &TaskState::Other("DEV VERIFY".into()));
                assert_eq!(state.tag(), "other");
            }
            other => panic!("expected an observation, got {other:?}"),
        }
    }

    /// A task provider failing must not silently become "no task", and must not
    /// affect the pull-request fact beside it.
    #[test]
    fn a_task_provider_failure_does_not_touch_the_pull_request_fact() {
        let subjects = StubSubjects::on("github.com", "o", "r", "b");
        let pulls = StubPullRequests::on("github.com", Ok(PullRequestState::Open));
        let tasks = StubTasks::answering(Err(ExternalProviderError::RateLimited));
        let resolver = ExternalContextResolver {
            subjects: &subjects,
            pull_requests: Some(&pulls),
            tasks: Some(&tasks),
        };

        let resolved = resolver.resolve(worktree(), Some("HORO-1546/feat/x"), now());

        assert_eq!(
            resolved.context.task.outcome,
            ProbeOutcome::Unavailable(ProbeReason::RateLimited)
        );
        assert_eq!(resolved.task_detail.tag(), "rate_limited");
        assert_eq!(
            resolved.context.pull_request.outcome,
            ProbeOutcome::Observed(PullRequestState::Open)
        );
    }

    /// Every local detail keeps its own token, and a failure's token is the
    /// error's own — so a quota refusal and a refused credential do not become
    /// one word at the surface a user reads.
    #[test]
    fn a_failure_keeps_the_errors_own_tag() {
        assert_eq!(
            ExternalDetail::Failed(ExternalProviderError::RateLimited).tag(),
            "rate_limited"
        );
        assert_eq!(
            ExternalDetail::Failed(ExternalProviderError::AuthRejected { status: 403 }).tag(),
            "auth_rejected"
        );
    }

    /// Only the two that spoke to a service report having done so. A report
    /// saying "provider reached" for a disabled provider would make a user
    /// think their credential had been tested when nothing had.
    #[test]
    fn only_an_answer_or_a_failure_counts_as_reaching_a_provider() {
        for detail in [
            ExternalDetail::Answered,
            ExternalDetail::Failed(ExternalProviderError::TimedOut),
        ] {
            assert!(detail.reached_provider(), "{detail:?}");
        }
        for detail in [
            ExternalDetail::ProviderDisabled,
            ExternalDetail::NoSubject(SubjectUnknown::NoRemote),
            ExternalDetail::HostNotServed,
            ExternalDetail::NoTaskKey,
        ] {
            assert!(!detail.reached_provider(), "{detail:?}");
        }
    }

    /// The local tokens are what a surface branches on, so no two may collide.
    #[test]
    fn every_local_detail_has_a_distinct_tag() {
        let all = [
            ExternalDetail::ProviderDisabled,
            ExternalDetail::NoSubject(SubjectUnknown::NoRemote),
            ExternalDetail::HostNotServed,
            ExternalDetail::NoTaskKey,
            ExternalDetail::Answered,
            ExternalDetail::Failed(ExternalProviderError::TimedOut),
        ];
        let mut tags: Vec<&str> = all.iter().map(ExternalDetail::tag).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), all.len(), "two details share a tag");
    }
}
