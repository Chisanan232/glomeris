//! The read-only GitHub pull-request adapter (HORO-1546).
//!
//! # What it answers, and what it refuses to guess
//!
//! One question: does *this exact* repository and branch have a pull request,
//! and what became of it. The repository and branch come from git's own
//! configuration ([`super::subject`]), never from a search, a similarity score
//! or a title match. A branch this adapter cannot name exactly is a branch it
//! does not ask about.
//!
//! # Every non-answer keeps its own reason
//!
//! GitHub says "no" in several ways that mean different things, and AC 3 is
//! that they stay apart:
//!
//! - `200 []` is GitHub answering that there is no pull request. That is an
//!   observation, and the only path in this file that produces
//!   [`PullRequestState::NoneObserved`].
//! - `401` is a credential GitHub rejected.
//! - `403` is *either* a token without the scope *or* an exhausted quota, and
//!   GitHub uses the same status for both. `x-ratelimit-remaining` is what tells
//!   them apart; without it, a user gets sent to rotate a credential that was
//!   never the problem.
//! - `404` is a repository this credential cannot see — which is emphatically
//!   not "this branch has no pull request". It stays
//!   [`ExternalProviderError::UnexpectedStatus`].
//!
//! # Read-only, structurally
//!
//! The adapter holds a [`ReadOnlyHttp`] and nothing else that touches a network.
//! That trait has one method and it is a GET, so there is no argument this file
//! could pass to reach another verb (AC 2).

use serde_json::Value;

use super::error::ExternalProviderError;
use super::http::{HeaderPair, HttpJson, ReadOnlyHttp};
use super::provider::PullRequestProvider;
use super::subject::RepositoryBranchSubject;
use crate::workspace::PullRequestState;

/// The host a `github.com` remote reports, and the API that serves it.
///
/// Separate constants because GitHub Enterprise Server breaks the relationship:
/// its remotes name the installation's own host and its API lives under
/// `https://<host>/api/v3`. The pairing is configuration, not an assumption this
/// file may bake in.
pub const GITHUB_HOST: &str = "github.com";
pub const GITHUB_API_BASE: &str = "https://api.github.com";

/// Pinned so a future GitHub default cannot silently change what the responses
/// look like under a running Glomeris.
const API_VERSION: &str = "2022-11-28";

/// GitHub rejects a request with no user agent. It identifies the program and
/// carries nothing about the machine, the user or the repository.
const USER_AGENT: &str = "glomeris";

/// Enough to see whether any pull request for this branch is open, and to tell
/// a merged one from a closed one. Well above the number of pull requests one
/// branch has; the bound exists so a surprising answer cannot become an
/// unbounded loop of requests, and this adapter reads the first page only.
const PER_PAGE: u32 = 100;

/// A read-only GitHub pull-request lookup.
pub struct GitHubPullRequests {
    http: Box<dyn ReadOnlyHttp>,
    /// The host a matching git remote reports, compared by
    /// [`super::ExternalContextResolver`] against the subject's own host so a
    /// repository on another forge is never asked about here.
    host: String,
    api_base: String,
    /// A read-only credential. Held to build one header per request and never
    /// formatted into an error, a report or a `Debug` — which is why this struct
    /// does not derive one.
    token: String,
}

impl GitHubPullRequests {
    /// Builds an adapter, or refuses.
    ///
    /// Validation happens here rather than per request so a configuration that
    /// cannot work never reaches a request builder, and so the failure names the
    /// configuration rather than arriving later disguised as a network fault.
    pub fn new(
        http: Box<dyn ReadOnlyHttp>,
        host: String,
        api_base: String,
        token: String,
    ) -> Result<Self, ExternalProviderError> {
        if token.is_empty() || host.is_empty() || api_base.is_empty() {
            return Err(ExternalProviderError::NotConfigured);
        }
        // `https` only. A credential must not be sent over a transport that
        // cannot protect it, and a configuration file is an easy place for an
        // `http://` to end up unnoticed.
        if !api_base.starts_with("https://") {
            return Err(ExternalProviderError::InvalidConfiguration(
                "the GitHub API base must start with https://".into(),
            ));
        }
        if api_base.contains('?') || api_base.contains('#') || api_base.ends_with('/') {
            return Err(ExternalProviderError::InvalidConfiguration(
                "the GitHub API base must have no query, no fragment and no trailing slash".into(),
            ));
        }
        Ok(Self {
            http,
            host,
            api_base,
            token,
        })
    }

    /// `github.com` through the public API.
    pub fn dot_com(
        http: Box<dyn ReadOnlyHttp>,
        token: String,
    ) -> Result<Self, ExternalProviderError> {
        Self::new(http, GITHUB_HOST.into(), GITHUB_API_BASE.into(), token)
    }

    fn headers(&self) -> Vec<HeaderPair> {
        vec![
            HeaderPair {
                name: "Authorization",
                value: format!("Bearer {}", self.token),
            },
            HeaderPair {
                name: "Accept",
                value: "application/vnd.github+json".into(),
            },
            HeaderPair {
                name: "X-GitHub-Api-Version",
                value: API_VERSION.into(),
            },
            HeaderPair {
                name: "User-Agent",
                value: USER_AGENT.into(),
            },
        ]
    }

    /// GETs `path` and returns the body only if GitHub actually answered.
    fn read(&self, url: &str) -> Result<String, ExternalProviderError> {
        let response = self.http.get_json(url, &self.headers())?;
        match response.status {
            200 => Ok(response.body),
            401 => Err(ExternalProviderError::AuthRejected { status: 401 }),
            403 => Err(forbidden_reason(&response)),
            429 => Err(ExternalProviderError::RateLimited),
            status => Err(ExternalProviderError::UnexpectedStatus { status }),
        }
    }

    /// The pull requests whose head is `subject`'s branch in `repository`.
    ///
    /// `repository` is separate from `subject` because a branch pushed to a fork
    /// has its pull request in the *upstream* repository while still being
    /// `fork_owner:branch` as far as the query is concerned.
    fn pulls_for(
        &self,
        repository: &RepositoryBranchSubject,
        subject: &RepositoryBranchSubject,
    ) -> Result<Vec<PullSummary>, ExternalProviderError> {
        let url = format!(
            "{}/repos/{}/{}/pulls?state=all&per_page={}&head={}:{}",
            self.api_base,
            repository.owner,
            repository.repo,
            PER_PAGE,
            subject.owner,
            subject.encoded_branch(),
        );
        let body = self.read(&url)?;
        let listed: Value = serde_json::from_str(&body).map_err(|e| {
            ExternalProviderError::UnusableResponse(format!("pull request list was not JSON: {e}"))
        })?;
        let entries = listed.as_array().ok_or_else(|| {
            ExternalProviderError::UnusableResponse("pull request list was not an array".into())
        })?;

        let mut summaries = Vec::new();
        for entry in entries {
            let summary = PullSummary::read(entry)?;
            // GitHub already filtered by head, and this checks it again. A
            // provider's answer is not a promise, and the whole value of this
            // adapter is that the pull request it reports belongs to the branch
            // it was asked about.
            if summary.head_ref.as_deref() == Some(subject.branch.as_str()) {
                summaries.push(summary);
            }
        }
        Ok(summaries)
    }

    /// The repository this one was forked from, if GitHub says it is a fork.
    ///
    /// One hop only: `parent` is the immediate upstream, and a fork of a fork
    /// stops here rather than walking a chain of requests whose length a
    /// provider would get to choose.
    fn upstream_of(
        &self,
        subject: &RepositoryBranchSubject,
    ) -> Result<Option<RepositoryBranchSubject>, ExternalProviderError> {
        let url = format!("{}/repos/{}/{}", self.api_base, subject.owner, subject.repo);
        let body = self.read(&url)?;
        let repository: Value = serde_json::from_str(&body).map_err(|e| {
            ExternalProviderError::UnusableResponse(format!("repository was not JSON: {e}"))
        })?;
        if repository.get("fork").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let full_name = match repository
            .get("parent")
            .and_then(|parent| parent.get("full_name"))
            .and_then(Value::as_str)
        {
            Some(name) => name,
            // GitHub says fork and names no parent. Nothing to ask, and
            // inventing an upstream from the fork's own name would be a guess.
            None => return Ok(None),
        };
        let Some((owner, repo)) = full_name.split_once('/') else {
            return Ok(None);
        };
        Ok(subject.redirected_to(owner, repo))
    }
}

impl PullRequestProvider for GitHubPullRequests {
    fn host(&self) -> &str {
        &self.host
    }

    fn pull_request_state(
        &self,
        subject: &RepositoryBranchSubject,
    ) -> Result<PullRequestState, ExternalProviderError> {
        let direct = self.pulls_for(subject, subject)?;
        if let Some(state) = most_live_state(&direct) {
            return Ok(state);
        }

        // Nothing in the repository the remote names. Before reporting an
        // absence, check the one place a pull request for this branch honestly
        // lives elsewhere: a fork's branch has its pull request upstream, and
        // reporting `NoneObserved` there would be a wrong answer rather than a
        // missing one — exactly what campaign section 13 is about.
        match self.upstream_of(subject)? {
            Some(upstream) => {
                let from_fork = self.pulls_for(&upstream, subject)?;
                Ok(most_live_state(&from_fork).unwrap_or(PullRequestState::NoneObserved))
            }
            None => Ok(PullRequestState::NoneObserved),
        }
    }
}

/// What a `403` meant.
///
/// `x-ratelimit-remaining: 0` is a quota that will return on its own; anything
/// else is a credential that will not. A missing header is *not* read as
/// "there is quota left": it means GitHub said nothing about quota, so the
/// honest reading is the credential, which is the one a user can act on.
fn forbidden_reason(response: &HttpJson) -> ExternalProviderError {
    if response.rate_limit_remaining == Some(0) {
        ExternalProviderError::RateLimited
    } else {
        ExternalProviderError::AuthRejected { status: 403 }
    }
}

/// The state to report when a branch has more than one pull request.
///
/// Open beats merged beats closed-unmerged, and the ordering is a safety choice
/// rather than a recency one: "merged" is the single most persuasive thing this
/// program can say about a directory, and a branch that also has an open pull
/// request is a branch somebody is still working on. Where the two disagree, the
/// one that does not read as permission wins.
fn most_live_state(summaries: &[PullSummary]) -> Option<PullRequestState> {
    if summaries.is_empty() {
        return None;
    }
    if summaries.iter().any(|s| s.open) {
        return Some(PullRequestState::Open);
    }
    if summaries.iter().any(|s| s.merged) {
        return Some(PullRequestState::Merged);
    }
    Some(PullRequestState::ClosedUnmerged)
}

/// The three fields this adapter reads from a pull request.
///
/// Deliberately not the title, the body, the author, the reviewers or the
/// number. A title is somebody's sentence about unreleased work, and the
/// privacy contract is easiest to keep when the type cannot hold one.
struct PullSummary {
    open: bool,
    /// `merged_at` present and non-null. GitHub reports a merged pull request as
    /// `state: "closed"`, so closed-unmerged and merged are the same `state`
    /// and only this field separates them.
    merged: bool,
    head_ref: Option<String>,
}

impl PullSummary {
    fn read(entry: &Value) -> Result<Self, ExternalProviderError> {
        let state = entry
            .get("state")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ExternalProviderError::UnusableResponse(
                    "a pull request had no state field".to_string(),
                )
            })?
            .to_string();
        Ok(Self {
            open: state == "open",
            merged: entry
                .get("merged_at")
                .is_some_and(|merged_at| !merged_at.is_null()),
            head_ref: entry
                .get("head")
                .and_then(|head| head.get("ref"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::http::fake::{json, FakeHttp};
    use super::*;
    use std::time::Duration;

    fn subject(owner: &str, repo: &str, branch: &str) -> RepositoryBranchSubject {
        RepositoryBranchSubject {
            host: GITHUB_HOST.into(),
            owner: owner.into(),
            repo: repo.into(),
            branch: branch.into(),
        }
    }

    fn adapter(answers: Vec<Result<HttpJson, ExternalProviderError>>) -> GitHubPullRequests {
        GitHubPullRequests::dot_com(Box::new(FakeHttp::new(answers)), "read-only-token".into())
            .expect("the fixture configuration is valid")
    }

    /// Building the adapter and reading back what it asked for, which needs the
    /// fake kept alive beside it.
    fn adapter_watching(
        answers: Vec<Result<HttpJson, ExternalProviderError>>,
    ) -> (GitHubPullRequests, std::rc::Rc<FakeHttp>) {
        let http = std::rc::Rc::new(FakeHttp::new(answers));
        let adapter = GitHubPullRequests::dot_com(
            Box::new(std::rc::Rc::clone(&http)),
            "read-only-token".into(),
        )
        .expect("the fixture configuration is valid");
        (adapter, http)
    }

    fn pull(state: &str, merged_at: &str, head_ref: &str) -> String {
        format!(r#"{{"state":"{state}","merged_at":{merged_at},"head":{{"ref":"{head_ref}"}}}}"#)
    }

    /// AC 5. The exact owner, repository and branch appear in the request, and
    /// a slashed branch survives as one query value rather than becoming a path.
    #[test]
    fn the_request_names_the_exact_repository_and_branch() {
        let (adapter, http) = adapter_watching(vec![
            Ok(json(200, "[]")),
            Ok(json(200, r#"{"fork":false}"#)),
        ]);

        adapter
            .pull_request_state(&subject(
                "Chisanan232",
                "glomeris",
                "v0.0.1/HORO-1546/feat/external_context",
            ))
            .unwrap();

        let urls = http.urls();
        assert_eq!(
            urls[0],
            "https://api.github.com/repos/Chisanan232/glomeris/pulls\
             ?state=all&per_page=100&head=Chisanan232:v0.0.1%2FHORO-1546%2Ffeat%2Fexternal_context"
        );
    }

    /// The credential travels as a header, and a test can prove that without
    /// ever holding its value.
    #[test]
    fn the_credential_is_sent_as_a_header() {
        let (adapter, http) = adapter_watching(vec![
            Ok(json(200, "[]")),
            Ok(json(200, r#"{"fork":false}"#)),
        ]);

        adapter
            .pull_request_state(&subject("o", "r", "trunk"))
            .unwrap();

        let names = http.header_names.borrow().clone();
        assert!(names.contains(&"Authorization"), "{names:?}");
        assert!(names.contains(&"Accept"), "{names:?}");
        assert!(names.contains(&"X-GitHub-Api-Version"), "{names:?}");
        assert!(names.contains(&"User-Agent"), "{names:?}");
    }

    /// GitHub answering "there are none" is the one path that produces an
    /// observed absence — and it costs one extra request, because a fork's pull
    /// request lives upstream.
    #[test]
    fn an_empty_list_from_a_plain_repository_is_an_observed_absence() {
        let (adapter, http) = adapter_watching(vec![
            Ok(json(200, "[]")),
            Ok(json(200, r#"{"fork":false}"#)),
        ]);

        let state = adapter
            .pull_request_state(&subject("o", "r", "trunk"))
            .unwrap();

        assert_eq!(state, PullRequestState::NoneObserved);
        assert_eq!(http.urls().len(), 2);
        assert_eq!(http.urls()[1], "https://api.github.com/repos/o/r");
    }

    /// A branch pushed to a fork has its pull request upstream. Reporting
    /// `NoneObserved` for it would be a wrong answer rather than a missing one.
    #[test]
    fn a_fork_branch_finds_its_pull_request_upstream() {
        let (adapter, http) = adapter_watching(vec![
            Ok(json(200, "[]")),
            Ok(json(
                200,
                r#"{"fork":true,"parent":{"full_name":"upstream-org/glomeris"}}"#,
            )),
            Ok(json(
                200,
                &format!("[{}]", pull("closed", "\"2026-09-01T00:00:00Z\"", "trunk")),
            )),
        ]);

        let state = adapter
            .pull_request_state(&subject("forker", "glomeris", "trunk"))
            .unwrap();

        assert_eq!(state, PullRequestState::Merged);
        let urls = http.urls();
        assert_eq!(urls.len(), 3);
        assert_eq!(
            urls[2],
            "https://api.github.com/repos/upstream-org/glomeris/pulls\
             ?state=all&per_page=100&head=forker:trunk"
        );
    }

    /// A parent name that could change the shape of a URL is refused, and the
    /// answer becomes an honest absence rather than a request built from it.
    #[test]
    fn a_provider_supplied_parent_name_cannot_redirect_a_request() {
        for parent in [
            "../../secret/repo",
            "upstream/repo?x=1",
            "upstream/repo#f",
            "upstream/re po",
            "upstream",
        ] {
            let (adapter, http) = adapter_watching(vec![
                Ok(json(200, "[]")),
                Ok(json(
                    200,
                    &format!(r#"{{"fork":true,"parent":{{"full_name":"{parent}"}}}}"#),
                )),
            ]);

            let state = adapter
                .pull_request_state(&subject("forker", "repo", "trunk"))
                .unwrap();

            assert_eq!(state, PullRequestState::NoneObserved, "{parent}");
            assert_eq!(http.urls().len(), 2, "{parent} produced {:?}", http.urls());
        }
    }

    /// A fork of a fork stops after one hop, so a provider does not get to
    /// choose how many requests a lookup makes.
    #[test]
    fn the_upstream_search_takes_one_hop_only() {
        let (adapter, http) = adapter_watching(vec![
            Ok(json(200, "[]")),
            Ok(json(
                200,
                r#"{"fork":true,"parent":{"full_name":"middle/repo"}}"#,
            )),
            Ok(json(200, "[]")),
        ]);

        let state = adapter
            .pull_request_state(&subject("forker", "repo", "trunk"))
            .unwrap();

        assert_eq!(state, PullRequestState::NoneObserved);
        assert_eq!(http.urls().len(), 3);
    }

    /// GitHub reports a merged pull request as `state: "closed"`, so `state`
    /// alone cannot tell merged from abandoned and `merged_at` is what does.
    #[test]
    fn merged_and_abandoned_are_both_closed_and_stay_distinct() {
        let merged = adapter(vec![Ok(json(
            200,
            &format!("[{}]", pull("closed", "\"2026-09-01T00:00:00Z\"", "trunk")),
        ))]);
        assert_eq!(
            merged
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap(),
            PullRequestState::Merged
        );

        let abandoned = adapter(vec![Ok(json(
            200,
            &format!("[{}]", pull("closed", "null", "trunk")),
        ))]);
        assert_eq!(
            abandoned
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap(),
            PullRequestState::ClosedUnmerged
        );
    }

    /// An open pull request is reported as open, and one extra request is not
    /// made once an answer exists.
    #[test]
    fn an_open_pull_request_answers_in_one_request() {
        let (adapter, http) = adapter_watching(vec![Ok(json(
            200,
            &format!("[{}]", pull("open", "null", "trunk")),
        ))]);

        let state = adapter
            .pull_request_state(&subject("o", "r", "trunk"))
            .unwrap();

        assert_eq!(state, PullRequestState::Open);
        assert_eq!(http.urls().len(), 1);
    }

    /// Where an open and a merged pull request disagree, the one that does not
    /// read as permission wins.
    #[test]
    fn an_open_pull_request_outranks_a_merged_one() {
        let body = format!(
            "[{},{}]",
            pull("closed", "\"2026-09-01T00:00:00Z\"", "trunk"),
            pull("open", "null", "trunk")
        );
        let adapter = adapter(vec![Ok(json(200, &body))]);

        assert_eq!(
            adapter
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap(),
            PullRequestState::Open
        );
    }

    /// A pull request GitHub returned for a different branch is not reported for
    /// this one, even though the query already filtered by head.
    #[test]
    fn a_pull_request_for_another_branch_is_not_this_branchs_answer() {
        let adapter = adapter(vec![
            Ok(json(
                200,
                &format!("[{}]", pull("open", "null", "some-other-branch")),
            )),
            Ok(json(200, r#"{"fork":false}"#)),
        ]);

        assert_eq!(
            adapter
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap(),
            PullRequestState::NoneObserved
        );
    }

    /// AC 3, on the status that matters most: a repository this credential
    /// cannot see is not a branch without a pull request.
    #[test]
    fn a_404_is_not_an_absence() {
        let adapter = adapter(vec![Ok(json(404, r#"{"message":"Not Found"}"#))]);

        let error = adapter
            .pull_request_state(&subject("o", "r", "trunk"))
            .unwrap_err();

        assert_eq!(error.tag(), "unexpected_status");
        assert_eq!(error.reason().tag(), "failed");
    }

    /// GitHub uses `403` for a token without the scope and for an exhausted
    /// quota, and sending a user to rotate a working credential is the failure
    /// this distinction prevents.
    #[test]
    fn a_403_is_a_quota_only_when_github_says_the_quota_is_gone() {
        let exhausted = adapter(vec![Ok(HttpJson {
            status: 403,
            body: "{}".into(),
            rate_limit_remaining: Some(0),
        })]);
        assert_eq!(
            exhausted
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap_err()
                .tag(),
            "rate_limited"
        );

        let unscoped = adapter(vec![Ok(HttpJson {
            status: 403,
            body: "{}".into(),
            rate_limit_remaining: Some(4_987),
        })]);
        assert_eq!(
            unscoped
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap_err()
                .tag(),
            "auth_rejected"
        );

        // No header at all is read as the credential, because that is the one a
        // user can act on, and claiming quota remains would be inventing a fact
        // GitHub did not state.
        let silent = adapter(vec![Ok(json(403, "{}"))]);
        assert_eq!(
            silent
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap_err()
                .tag(),
            "auth_rejected"
        );
    }

    /// Each remaining status keeps its own reason rather than collapsing into
    /// one opaque failure.
    #[test]
    fn every_refusal_keeps_its_own_reason() {
        for (status, tag, wire) in [
            (401, "auth_rejected", "permission_denied"),
            (429, "rate_limited", "rate_limited"),
            (500, "unexpected_status", "failed"),
            (301, "unexpected_status", "failed"),
        ] {
            let adapter = adapter(vec![Ok(json(status, "{}"))]);
            let error = adapter
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap_err();
            assert_eq!(error.tag(), tag, "{status}");
            assert_eq!(error.reason().tag(), wire, "{status}");
        }
    }

    /// A transport failure arrives as itself. The adapter adds no
    /// interpretation to "no answer came back".
    #[test]
    fn a_transport_failure_is_passed_through() {
        let adapter = adapter(vec![Err(ExternalProviderError::TimedOut)]);

        assert_eq!(
            adapter
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap_err()
                .tag(),
            "timed_out"
        );
    }

    /// A `200` whose body is not the shape this adapter reads is a failure, not
    /// an absence. Silently reading an unparseable answer as "no pull request"
    /// is how a provider change becomes a deletion recommendation.
    #[test]
    fn an_unreadable_success_body_is_a_failure_and_not_an_absence() {
        for body in [
            "not json at all",
            r#"{"message":"an object where an array belongs"}"#,
            r#"[{"head":{"ref":"trunk"}}]"#,
        ] {
            let adapter = adapter(vec![Ok(json(200, body))]);
            let error = adapter
                .pull_request_state(&subject("o", "r", "trunk"))
                .unwrap_err();
            assert_eq!(error.tag(), "unusable_response", "{body}");
        }
    }

    /// A configuration that cannot work is refused where it is built, not later
    /// disguised as a network fault.
    #[test]
    fn an_unusable_configuration_is_refused_at_construction() {
        let refused = |host: &str, base: &str, token: &str| {
            GitHubPullRequests::new(
                Box::new(FakeHttp::refusing()),
                host.into(),
                base.into(),
                token.into(),
            )
            .err()
            .map(|e| e.tag())
        };

        assert_eq!(
            refused(GITHUB_HOST, GITHUB_API_BASE, ""),
            Some("not_configured")
        );
        assert_eq!(refused("", GITHUB_API_BASE, "t"), Some("not_configured"));
        assert_eq!(refused(GITHUB_HOST, "", "t"), Some("not_configured"));
        // A credential must not be sent over a transport that cannot protect it.
        assert_eq!(
            refused(GITHUB_HOST, "http://api.github.com", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(
            refused(GITHUB_HOST, "https://api.github.com/", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(
            refused(GITHUB_HOST, "https://api.github.com?x=1", "t"),
            Some("invalid_configuration")
        );
        assert_eq!(refused(GITHUB_HOST, GITHUB_API_BASE, "t"), None);
    }

    /// An Enterprise Server installation names its own host and its own API
    /// base, so neither may be hard-wired to the public pairing.
    #[test]
    fn an_enterprise_installation_keeps_its_own_host_and_api_base() {
        let http = std::rc::Rc::new(FakeHttp::new(vec![Ok(json(
            200,
            &format!("[{}]", pull("open", "null", "trunk")),
        ))]));
        let adapter = GitHubPullRequests::new(
            Box::new(std::rc::Rc::clone(&http)),
            "ghe.example.test".into(),
            "https://ghe.example.test/api/v3".into(),
            "read-only-token".into(),
        )
        .unwrap();

        assert_eq!(adapter.host(), "ghe.example.test");
        let mut ghe_subject = subject("team", "service", "trunk");
        ghe_subject.host = "ghe.example.test".into();
        assert_eq!(
            adapter.pull_request_state(&ghe_subject).unwrap(),
            PullRequestState::Open
        );
        assert!(
            http.urls()[0].starts_with("https://ghe.example.test/api/v3/repos/team/service/pulls"),
            "{:?}",
            http.urls()
        );
    }

    /// The bound on one request exists; this pins that the adapter passes a
    /// deadline through rather than relying on a default.
    #[test]
    fn the_real_transport_is_constructible_with_a_deadline() {
        let http = super::super::http::UreqReadOnlyHttp::new(Duration::from_secs(5));
        assert!(GitHubPullRequests::dot_com(Box::new(http), "t".into()).is_ok());
    }
}
