//! Optional read-only context from services outside this machine
//! (HORO-1546).
//!
//! # Supporting evidence, never authority
//!
//! This module lives under [`crate::workspace`] and inherits that module's
//! whole contract: nothing here may make anything executable, and
//! `scripts/check-workspace-aggregation-has-no-authority.sh` keeps
//! `src/policy`, `src/executor`, `src/autopilot` and `src/actions` unable to
//! reference it. That placement is the design, not a filing decision — "the
//! pull request was merged" is the single most persuasive sentence this
//! repository can produce about a directory, and it reads as permission. Put
//! anywhere the policy engine could reach, the temptation to add "…or the PR
//! is merged" to a classification rule would be permanent.
//!
//! The local facts keep their own authority regardless:
//! [`crate::workspace::BranchLifecycle::unique_work`] never consults external
//! context, and neither does [`crate::workspace::ActivityFacts`]. A merged
//! pull request beside a dirty tree with three unpushed commits leaves that
//! tree exactly as protected as it was.
//!
//! # Optional means optional
//!
//! Every field starts [`crate::evidence::ProbeReason::NotAttempted`] and stays
//! there unless a provider was configured, enabled, and answered. There is no
//! default provider, no baked-in host and no implicit credential. Glomeris
//! with both providers off must be exactly as useful as it is today, which is
//! why the seam is a pair of `Option<&dyn …>` rather than a trait object with
//! a null implementation: an absent provider is a shape the type system can
//! see.
//!
//! # Read-only by construction
//!
//! The adapters cannot mutate anything because the only transport they can
//! name is [`http::ReadOnlyHttp`], which has one method and it is a GET.
//! There is no code path from this module to a POST, and
//! `scripts/check-external-context-is-read-only.sh` proves the module names no
//! mutating verb. Prompt text and code review are not security boundaries;
//! a transport with no write method is.
//!
//! # No fuzzy matching
//!
//! Correlation is exact or it does not happen. A pull request is looked up by
//! a repository and branch derived from git's own configuration
//! ([`subject`]), and a task by an explicit issue key found in the branch name
//! under a documented rule. `fix-storage-stuff` correlates to nothing, and
//! that is the correct behaviour rather than a gap — a guessed association
//! becomes a guessed lifecycle, and a guessed lifecycle becomes a guessed
//! recommendation about somebody's unpushed work.

mod error;
mod github;
mod http;
mod provider;
mod subject;

pub use error::ExternalProviderError;
pub use github::{GitHubPullRequests, GITHUB_API_BASE, GITHUB_HOST};
pub use http::{HeaderPair, HttpJson, ReadOnlyHttp, UreqReadOnlyHttp, MAX_BODY_BYTES};
pub use provider::{
    ExternalContextResolver, ExternalDetail, PullRequestProvider, ResolvedExternalContext,
    TaskProvider,
};
pub use subject::{
    resolve_repository_branch, task_key_from_branch, GitSubjectResolver, RepositoryBranchSubject,
    SubjectResolver, SubjectUnknown, TaskKey,
};
