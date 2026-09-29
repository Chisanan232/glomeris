//! What, exactly, is being asked about (HORO-1546).
//!
//! # Correlation is exact or it does not happen
//!
//! Two subjects, one per provider, and both are derived rather than guessed:
//!
//! - [`RepositoryBranchSubject`] comes from git's own configuration — the
//!   remote this branch tracks, or the single remote if there is only one — and
//!   from the branch name the local probe already read. AC 5 is "GitHub
//!   correlation identifies the exact repository/branch subject", and that is
//!   satisfiable only by reading the identity out of the repository. Two
//!   remotes and no branch preference is [`SubjectUnknown::AmbiguousRemote`],
//!   not a coin toss: picking the wrong fork would answer a question about
//!   somebody else's pull requests.
//! - [`TaskKey`] comes from an explicit issue key in the branch name, matched
//!   under the documented rule in [`task_key_from_branch`]. AC 4 is "Jira
//!   correlation requires an explicit deterministic key, not fuzzy guessing",
//!   and `fix-storage-stuff` therefore correlates to nothing at all.
//!
//! # No subject is not an absence of a pull request
//!
//! Everything that goes wrong here produces a [`SubjectUnknown`], and a
//! `SubjectUnknown` can never become
//! [`crate::workspace::PullRequestState::NoneObserved`] or
//! [`crate::workspace::TaskState::NoneObserved`] — those are answers, and
//! nothing was asked. A detached HEAD, a missing remote and an unparseable
//! remote URL all mean "Glomeris could not tell what to ask about", which is
//! the campaign's §10 rule applied one step earlier than the provider itself.
//!
//! # The types are the injection guard
//!
//! Both subjects end up interpolated into a URL path or query, so neither may
//! be constructible from arbitrary text. [`TaskKey`] has a private field and
//! one fallible constructor that admits only `[A-Z0-9]` and a single `-`;
//! [`RepositoryBranchSubject`]'s fields come from git and are checked against
//! an allowlist narrow enough that the only character needing escaping is the
//! `/` a branch name may legally contain — see
//! [`RepositoryBranchSubject::encoded_branch`]. A subject that cannot hold a
//! `?`, a `#`, a `%` or a `..` cannot redirect a request somewhere else.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::evidence::correlate::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::ProbeReason;

/// An exact GitHub repository and branch, as this worktree's own git
/// configuration describes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryBranchSubject {
    /// The remote's host, so the caller can refuse a subject that is not on the
    /// host its provider is configured for. Kept here rather than compared here
    /// because this module's job is to read git, not to know which service is
    /// enabled — and because a worktree on a corporate host must produce a
    /// subject that is *recognisably not* the configured one rather than a
    /// parse failure.
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

impl RepositoryBranchSubject {
    /// The branch, safe to place in a URL query value.
    ///
    /// Only `/` needs encoding, and that is provable rather than hopeful:
    /// [`is_safe_branch_name`] is the sole gate on this field and it admits
    /// ASCII alphanumerics, `-`, `_`, `.` and `/`, of which the first four are
    /// already query-safe. So this cannot be an incomplete escaper that somebody
    /// has to remember to extend — extending the allowlist is what would force
    /// extending this, and the two are next to each other.
    pub fn encoded_branch(&self) -> String {
        self.branch.replace('/', "%2F")
    }
}

/// Why no subject could be identified.
///
/// Every variant means "nothing was asked", never "there is nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectUnknown {
    /// git could not be run, or could not answer.
    GitUnavailable(ProbeReason),
    /// HEAD is not on a branch, so there is no branch to correlate. Common
    /// during a rebase or a bisect, and not a fault.
    DetachedHead,
    /// The repository has no remote, so it has no remote identity. A perfectly
    /// ordinary state for local-only work.
    NoRemote,
    /// More than one remote, and the branch does not say which one it belongs
    /// to. Refused rather than guessed: `upstream` and `origin` are usually a
    /// project and a fork of it, and asking the wrong one returns confident
    /// answers about the wrong pull requests.
    AmbiguousRemote,
    /// The remote URL is not a shape this module can read an owner and a
    /// repository out of.
    ///
    /// Carries no detail on purpose. The URL may name a corporate host, and
    /// this reason travels into a local report where it does not need to;
    /// anybody diagnosing it can read their own `git remote -v`, which this
    /// module will not do on their behalf.
    UnreadableRemoteUrl,
    /// The branch name is one git allows and a URL does not survive.
    ///
    /// Also carries no detail: a ref name is the developer's own words about
    /// their own work, and there is nothing to gain by copying it into a
    /// report that may be read aloud in a meeting.
    UnusableBranchName,
}

impl SubjectUnknown {
    /// The bounded reason this travels as, once it reaches a
    /// [`crate::workspace::ExternalFact`].
    ///
    /// Four of the five are [`ProbeReason::NotAttempted`], which is exactly
    /// accurate: no request was made. It is emphatically not
    /// [`ProbeReason::Failed`] — a local-only branch with no remote is not a
    /// malfunction, and reporting it as one would have a user hunting a broken
    /// provider that is working perfectly.
    pub fn reason(&self) -> ProbeReason {
        match self {
            Self::GitUnavailable(reason) => *reason,
            Self::DetachedHead
            | Self::NoRemote
            | Self::AmbiguousRemote
            | Self::UnreadableRemoteUrl
            | Self::UnusableBranchName => ProbeReason::NotAttempted,
        }
    }

    /// A stable snake_case token for the local surfaces, where the whole
    /// distinction is worth keeping. Sole producer of these strings.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::GitUnavailable(_) => "git_unavailable",
            Self::DetachedHead => "detached_head",
            Self::NoRemote => "no_remote",
            Self::AmbiguousRemote => "ambiguous_remote",
            Self::UnreadableRemoteUrl => "unreadable_remote_url",
            Self::UnusableBranchName => "unusable_branch_name",
        }
    }
}

/// How a subject is obtained for a worktree.
///
/// A trait with one real implementation, for one reason: it lets
/// [`crate::workspace::external::ExternalContextResolver`]'s tests reach every
/// branch of provider handling — a host that is not served, a refused
/// credential, an answered absence — without each of them first building a git
/// repository with a remote. The git reading itself is tested against real git,
/// in this module, where a stub would only restate the assumption.
pub trait SubjectResolver {
    fn repository_branch(
        &self,
        worktree: &Path,
        branch: Option<&str>,
    ) -> Result<RepositoryBranchSubject, SubjectUnknown>;
}

/// The real one: reads git.
pub struct GitSubjectResolver {
    /// Bound on each `git` invocation.
    pub timeout: Duration,
}

impl SubjectResolver for GitSubjectResolver {
    fn repository_branch(
        &self,
        worktree: &Path,
        branch: Option<&str>,
    ) -> Result<RepositoryBranchSubject, SubjectUnknown> {
        resolve_repository_branch(worktree, branch, self.timeout)
    }
}

/// Reads this worktree's remote identity for `branch`.
///
/// `branch` is passed in rather than probed because
/// [`crate::workspace::WorktreeBranchState`] has already read it, honestly,
/// including the detached case — and a second `symbolic-ref` here would be a
/// second chance to disagree with it. `None` is [`SubjectUnknown::DetachedHead`].
pub fn resolve_repository_branch(
    worktree: &Path,
    branch: Option<&str>,
    timeout: Duration,
) -> Result<RepositoryBranchSubject, SubjectUnknown> {
    let branch = branch.ok_or(SubjectUnknown::DetachedHead)?;
    if !is_safe_branch_name(branch) {
        // git permits a great deal in a ref name that a URL does not survive.
        // Refused here, at the one place a branch becomes a subject, rather
        // than escaped at each interpolation site.
        return Err(SubjectUnknown::UnusableBranchName);
    }
    let remote = resolve_remote_name(worktree, branch, timeout)?;
    let url = read_config(worktree, &format!("remote.{remote}.url"), timeout)?
        .ok_or(SubjectUnknown::UnreadableRemoteUrl)?;
    let identity = parse_remote_url(&url).ok_or(SubjectUnknown::UnreadableRemoteUrl)?;
    Ok(RepositoryBranchSubject {
        host: identity.host,
        owner: identity.owner,
        repo: identity.repo,
        branch: branch.to_string(),
    })
}

/// Which remote this branch belongs to.
///
/// In order: what the branch itself says, then the only remote if there is
/// exactly one. Nothing else — there is no fallback to a remote named
/// `origin`, because "probably origin" is a guess, and this module does not
/// guess. Mirrors the existing discipline in
/// [`crate::workspace::branch`], which refuses to assume a default branch is
/// called `main`.
fn resolve_remote_name(
    worktree: &Path,
    branch: &str,
    timeout: Duration,
) -> Result<String, SubjectUnknown> {
    if let Some(configured) = read_config(worktree, &format!("branch.{branch}.remote"), timeout)? {
        return Ok(configured);
    }
    let output = run_git(worktree, &["remote"], timeout)?;
    let listed = String::from_utf8_lossy(&output.stdout);
    let mut names = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = names.next().ok_or(SubjectUnknown::NoRemote)?;
    if names.next().is_some() {
        return Err(SubjectUnknown::AmbiguousRemote);
    }
    Ok(first.to_string())
}

/// One `git config --get` value, or `None` when the key is unset.
///
/// An unset key exits 1 with no output, which is an answer rather than a
/// failure — the distinction matters because "this branch tracks no remote" is
/// how the sole-remote fallback is reached, and treating it as a git failure
/// would stop the resolution dead.
fn read_config(
    worktree: &Path,
    key: &str,
    timeout: Duration,
) -> Result<Option<String>, SubjectUnknown> {
    let output = run_git(worktree, &["config", "--get", key], timeout)?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

fn run_git(
    worktree: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, SubjectUnknown> {
    let mut command = Command::new("git");
    command.arg("-C").arg(worktree).args(args);
    match run_with_timeout(command, timeout) {
        CommandOutcome::Completed(output) => Ok(output),
        CommandOutcome::NotFound => Err(SubjectUnknown::GitUnavailable(ProbeReason::ToolAbsent)),
        CommandOutcome::TimedOut => Err(SubjectUnknown::GitUnavailable(ProbeReason::TimedOut)),
        CommandOutcome::SpawnFailed => Err(SubjectUnknown::GitUnavailable(ProbeReason::Failed)),
    }
}

/// A host, owner and repository read out of a remote URL.
struct RemoteIdentity {
    host: String,
    owner: String,
    repo: String,
}

/// Reads the three parts out of the URL forms git actually writes.
///
/// Handles `https://host/owner/repo.git`, `ssh://git@host/owner/repo.git` and
/// the scp-like `git@host:owner/repo.git`. Deliberately narrow: a URL form this
/// does not recognise becomes [`SubjectUnknown::UnreadableRemoteUrl`], and an
/// unrecognised URL is a missing answer rather than a wrong one. A permissive
/// parser here would produce a confident owner/repo pair for a host nobody
/// intended to query.
fn parse_remote_url(url: &str) -> Option<RemoteIdentity> {
    let url = url.trim();
    let (host, path) = if let Some(rest) = strip_scheme(url) {
        // `user@host/owner/repo` or `host/owner/repo`.
        let rest = rest.split_once('@').map_or(rest, |(_, after)| after);
        let (authority, path) = rest.split_once('/')?;
        (strip_port(authority), path)
    } else if let Some((authority, path)) = url.split_once(':') {
        // scp-like. Rejected if the part after the colon starts with a digit,
        // which is a port and means this was a scheme-less URL rather than an
        // scp target — misreading one would invent an owner out of a port.
        if path.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let authority = authority
            .split_once('@')
            .map_or(authority, |(_, after)| after);
        (authority, path)
    } else {
        return None;
    };

    let path = path.trim_start_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    let (owner, repo) = path.split_once('/')?;
    // Exactly two segments. `owner/repo/extra` is not a repository path, and
    // guessing which two of three were meant is the sort of helpfulness that
    // produces a request about the wrong thing.
    if repo.contains('/') {
        return None;
    }
    if host.is_empty() || !is_safe_path_segment(owner) || !is_safe_path_segment(repo) {
        return None;
    }
    Some(RemoteIdentity {
        host: host.to_ascii_lowercase(),
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

fn strip_scheme(url: &str) -> Option<&str> {
    ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
}

fn strip_port(authority: &str) -> &str {
    authority
        .split_once(':')
        .map_or(authority, |(host, _)| host)
}

/// Whether a value can be interpolated into a URL path or query unescaped.
///
/// The allowlist is the guarantee: a subject built only from these characters
/// cannot introduce a path segment, a query parameter, a fragment or a `..`,
/// so no remote URL and no branch name in a repository can redirect a request
/// this module makes. Checked rather than escaped because a check that fails
/// closed is easier to be sure of than an escape that has to be remembered at
/// every interpolation site.
fn is_safe_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether a ref name can become a subject.
///
/// [`is_safe_path_segment`] plus `/`, because a branch name legally contains
/// them — this repository's own convention is
/// `v0.0.1/HORO-1546/feat/external_context` — and refusing slashes outright
/// would mean refusing to correlate the very branches this campaign is written
/// on. The extra character is why [`RepositoryBranchSubject::encoded_branch`]
/// exists, and it is the only one that needs encoding.
///
/// Every component is checked separately, so `a/../b` is refused: git would not
/// create that ref, but this function's job is to be sure rather than to trust
/// that nothing upstream ever writes a surprising value into a config file.
fn is_safe_branch_name(value: &str) -> bool {
    !value.starts_with('/') && !value.ends_with('/') && value.split('/').all(is_safe_path_segment)
}

/// An explicit issue key, such as `HORO-1546`.
///
/// The field is private and [`TaskKey::parse`] is the only way in, so a
/// `TaskKey` is by construction `[A-Z][A-Z0-9]+-[0-9]+` — nothing that could
/// escape a URL path segment, and nothing a provider response or a language
/// model could fabricate into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskKey(String);

impl RepositoryBranchSubject {
    /// The same host and branch, naming a repository a *provider* pointed at.
    ///
    /// Exists for one case: a branch pushed to a fork has its pull request in
    /// the upstream repository, and the only place the upstream's name can come
    /// from is GitHub's own answer. That makes `owner` and `repo` here
    /// provider-controlled strings on their way into a URL path — which is the
    /// shape campaign section 16 refuses categorically for a language model, and
    /// the reasoning does not weaken for a service. So they go through the same
    /// allowlist a git remote does, and a name that would not survive that check
    /// yields `None` rather than a request.
    ///
    /// The host and the branch are deliberately *not* taken from the provider.
    /// A provider cannot redirect a lookup to another forge, and cannot change
    /// which branch is being asked about.
    pub(super) fn redirected_to(&self, owner: &str, repo: &str) -> Option<Self> {
        if !is_safe_path_segment(owner) || !is_safe_path_segment(repo) {
            return None;
        }
        Some(Self {
            host: self.host.clone(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: self.branch.clone(),
        })
    }
}

/// The longest project prefix accepted, and the most digits. Bounds exist so a
/// pathological branch name cannot produce an unbounded URL; both are far above
/// any real key.
const MAX_PREFIX_LEN: usize = 10;
const MAX_NUMBER_LEN: usize = 9;

impl TaskKey {
    /// Parses a whole string as an issue key, or refuses it.
    ///
    /// Uppercase only. Case-insensitive matching was considered and rejected:
    /// it turns ordinary branch words into keys — `utf-8`, `sha-256`, `p-2` —
    /// and every one of those would be a request to Jira about an issue nobody
    /// referenced. Jira's own keys are uppercase, so requiring it costs
    /// nothing real and removes a whole class of accidental correlation.
    pub fn parse(value: &str) -> Option<Self> {
        let (prefix, number) = value.split_once('-')?;
        if prefix.len() < 2 || prefix.len() > MAX_PREFIX_LEN {
            return None;
        }
        let mut prefix_chars = prefix.chars();
        if !prefix_chars.next()?.is_ascii_uppercase() {
            return None;
        }
        if !prefix_chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
            return None;
        }
        if number.is_empty() || number.len() > MAX_NUMBER_LEN {
            return None;
        }
        if !number.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // `HORO-0` and `HORO-007` are not keys Jira issues; accepting them
        // would send requests that can only ever 404.
        if number.starts_with('0') {
            return None;
        }
        Some(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The one documented rule for finding a task key in a branch name.
///
/// A candidate is an issue key that starts at a boundary — the start of the
/// name, or just after `/`, `-`, `_` or `.` — and ends at one. So all of
/// `HORO-1546`, `v0.0.1/HORO-1546/feat/thing` and `feature/HORO-1546-add-thing`
/// correlate, and `fix-storage-stuff` does not.
///
/// Two *different* keys in one branch name yield `None`. That is the fail-closed
/// half of AC 4: a name mentioning both `HORO-1546` and `HORO-1200` does not
/// say which one the work is for, and choosing the leftmost would be a guess
/// dressed as a rule. The same key repeated is still one key.
pub fn task_key_from_branch(branch: &str) -> Option<TaskKey> {
    let bytes = branch.as_bytes();
    let mut found: Option<TaskKey> = None;
    for start in 0..bytes.len() {
        if !starts_at_boundary(bytes, start) {
            continue;
        }
        let Some(end) = key_end(bytes, start) else {
            continue;
        };
        let Some(key) = TaskKey::parse(&branch[start..end]) else {
            continue;
        };
        match &found {
            // Ambiguity is refused, and refusing is final: a third mention of
            // the first key must not undo it.
            Some(existing) if existing != &key => return None,
            Some(_) => {}
            None => found = Some(key),
        }
    }
    found
}

fn starts_at_boundary(bytes: &[u8], index: usize) -> bool {
    if !bytes[index].is_ascii_uppercase() {
        return false;
    }
    match index.checked_sub(1) {
        None => true,
        Some(before) => is_separator(bytes[before]),
    }
}

/// Where the key starting at `start` ends, if one does.
///
/// Scans the prefix, the `-`, then the digits, and requires a boundary after
/// the digits so `HORO-15X` is not read as `HORO-15`.
fn key_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    while index < bytes.len()
        && (bytes[index].is_ascii_uppercase() || bytes[index].is_ascii_digit())
    {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b'-' {
        return None;
    }
    index += 1;
    let digits_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == digits_start {
        return None;
    }
    if index < bytes.len() && !is_separator(bytes[index]) {
        return None;
    }
    Some(index)
}

fn is_separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repo_convention_branch_yields_its_key() {
        assert_eq!(
            task_key_from_branch("v0.0.1/HORO-1546/feat/external_context")
                .map(|k| k.as_str().to_string()),
            Some("HORO-1546".to_string())
        );
    }

    /// The other common shape, where the key and the summary share a segment.
    #[test]
    fn a_key_followed_by_a_dashed_summary_yields_its_key() {
        assert_eq!(
            task_key_from_branch("feature/HORO-1546-add-thing").map(|k| k.as_str().to_string()),
            Some("HORO-1546".to_string())
        );
    }

    /// AC 4, the whole point of it.
    #[test]
    fn a_descriptive_branch_name_correlates_to_nothing() {
        for branch in [
            "fix-storage-stuff",
            "main",
            "release/v1.2.3",
            "wip",
            "chisanan/try-the-thing",
        ] {
            assert_eq!(task_key_from_branch(branch), None, "{branch} matched");
        }
    }

    /// The false positive worth naming: an encoding, a hash size and a version
    /// look exactly like keys until the uppercase rule is applied.
    #[test]
    fn lowercase_words_are_not_keys() {
        for branch in ["fix/utf-8-encoding", "feat/sha-256-digests", "bug/p-2"] {
            assert_eq!(task_key_from_branch(branch), None, "{branch} matched");
        }
    }

    /// Two different keys is not a choice this module gets to make.
    #[test]
    fn two_different_keys_in_one_branch_refuse_to_correlate() {
        assert_eq!(task_key_from_branch("HORO-1546/and/HORO-1200/feat/x"), None);
    }

    /// …but the same key twice is still one key, which is a shape the repo's
    /// own worktree naming can produce.
    #[test]
    fn the_same_key_repeated_is_one_key() {
        assert_eq!(
            task_key_from_branch("HORO-1546/feat/HORO-1546_thing").map(|k| k.as_str().to_string()),
            Some("HORO-1546".to_string())
        );
    }

    /// A key must be delimited on both sides, or a fragment of a longer token
    /// would produce confident nonsense.
    #[test]
    fn a_key_must_start_and_end_at_a_boundary() {
        // Trailing rubbish: the digits do not end at a boundary.
        assert_eq!(task_key_from_branch("HORO-15X"), None);
        // A key cannot begin mid-token. `xHORO-15` is one word, not a
        // reference, and reading the tail of it as a key is exactly the fuzzy
        // behaviour AC 4 rules out.
        assert_eq!(task_key_from_branch("xHORO-15"), None);
        // `XHORO-15`, by contrast, *is* a well-formed key for a project called
        // XHORO. There is no way to tell it from a typo, and inventing one
        // would mean keeping a list of real project prefixes.
        assert_eq!(
            task_key_from_branch("XHORO-15").map(|k| k.as_str().to_string()),
            Some("XHORO-15".to_string())
        );
        // `.` is a boundary, so `HORO-1546.patch` correlates. The cost is that
        // `HORO-15.2` reads as `HORO-15`; the benefit is that a dotted suffix
        // does not destroy a real reference, and a key nobody issued can only
        // ever come back as an answered absence.
        assert_eq!(
            task_key_from_branch("HORO-1546.patch").map(|k| k.as_str().to_string()),
            Some("HORO-1546".to_string())
        );
    }

    /// The type is the URL guard, so it has to refuse everything that could
    /// leave a path segment.
    #[test]
    fn a_task_key_cannot_hold_anything_that_escapes_a_url() {
        for value in [
            "HORO-1546/../admin",
            "HORO 1546",
            "HORO-1546?x=1",
            "horo-1546",
            "H-1",
            "HORO-",
            "-1546",
            "HORO-0",
            "HORO-007",
            "VERYLONGPREFIX-1",
            "HORO-1234567890",
            "",
        ] {
            assert_eq!(TaskKey::parse(value), None, "{value:?} parsed");
        }
    }

    #[test]
    fn https_and_ssh_and_scp_urls_all_name_the_same_repository() {
        for url in [
            "https://github.com/Chisanan232/glomeris.git",
            "https://github.com/Chisanan232/glomeris",
            "http://github.com/Chisanan232/glomeris.git",
            "ssh://git@github.com/Chisanan232/glomeris.git",
            "ssh://git@github.com:22/Chisanan232/glomeris.git",
            "git@github.com:Chisanan232/glomeris.git",
            "git://github.com/Chisanan232/glomeris.git",
            "https://github.com/Chisanan232/glomeris/",
        ] {
            let identity = parse_remote_url(url).unwrap_or_else(|| panic!("{url} did not parse"));
            assert_eq!(identity.host, "github.com", "{url}");
            assert_eq!(identity.owner, "Chisanan232", "{url}");
            assert_eq!(identity.repo, "glomeris", "{url}");
        }
    }

    /// A host that is not GitHub still parses. The decision about whether to
    /// ask it belongs to the caller holding the configuration, and a parse
    /// failure here would make "this worktree is on another host" look like a
    /// malformed URL.
    #[test]
    fn a_non_github_host_parses_and_reports_its_host() {
        let identity = parse_remote_url("https://git.example.test/team/service.git").unwrap();
        assert_eq!(identity.host, "git.example.test");
        assert_eq!(identity.owner, "team");
        assert_eq!(identity.repo, "service");
    }

    /// The host is compared by the caller, so it is lowercased here — `GitHub.com`
    /// and `github.com` are the same host and must not be two subjects.
    #[test]
    fn the_host_is_lowercased() {
        let identity = parse_remote_url("https://GitHub.COM/Owner/Repo.git").unwrap();
        assert_eq!(identity.host, "github.com");
        // The owner and repository keep their case: GitHub's paths are
        // case-insensitive but its API echoes what it was given, and rewriting
        // a user's repository name is not this module's business.
        assert_eq!(identity.owner, "Owner");
        assert_eq!(identity.repo, "Repo");
    }

    #[test]
    fn an_unreadable_remote_url_parses_to_nothing() {
        for url in [
            "",
            "github.com",
            "/srv/git/local.git",
            "https://github.com/",
            "https://github.com/only-one-segment",
            "https://github.com/owner/repo/extra",
            "file:///srv/git/local.git",
        ] {
            assert!(parse_remote_url(url).is_none(), "{url:?} parsed");
        }
    }

    /// A local path with a colon must not be read as an scp target, or
    /// `/srv:git/repo` would invent an owner. The digit rule covers the
    /// scheme-less `host:port/path` case for the same reason.
    #[test]
    fn a_port_after_a_colon_is_not_an_owner() {
        assert!(parse_remote_url("github.com:22/owner/repo.git").is_none());
    }

    /// A remote URL is attacker-adjacent data: it comes from a repository's
    /// config, which arrives with any clone. None of it may reach a URL with a
    /// path separator or a query still in it.
    #[test]
    fn a_remote_url_cannot_smuggle_a_path_or_a_query_into_a_subject() {
        for url in [
            "https://github.com/owner/repo?x=1",
            "https://github.com/owner/..",
            "https://github.com/../repo",
            "https://github.com/owner/re%2Fpo",
        ] {
            assert!(parse_remote_url(url).is_none(), "{url:?} parsed");
        }
    }

    /// Every reason must say "nothing was asked" or carry git's own reason.
    /// None of them may become `Failed` for an ordinary local-only branch.
    #[test]
    fn no_ordinary_missing_subject_is_reported_as_a_failure() {
        for unknown in [
            SubjectUnknown::DetachedHead,
            SubjectUnknown::NoRemote,
            SubjectUnknown::AmbiguousRemote,
            SubjectUnknown::UnreadableRemoteUrl,
            SubjectUnknown::UnusableBranchName,
        ] {
            assert_eq!(
                unknown.reason(),
                ProbeReason::NotAttempted,
                "{unknown:?} reported {:?}",
                unknown.reason()
            );
        }
        assert_eq!(
            SubjectUnknown::GitUnavailable(ProbeReason::ToolAbsent).reason(),
            ProbeReason::ToolAbsent
        );
    }

    #[test]
    fn every_subject_unknown_has_a_distinct_tag() {
        let all = [
            SubjectUnknown::GitUnavailable(ProbeReason::Failed),
            SubjectUnknown::DetachedHead,
            SubjectUnknown::NoRemote,
            SubjectUnknown::AmbiguousRemote,
            SubjectUnknown::UnreadableRemoteUrl,
            SubjectUnknown::UnusableBranchName,
        ];
        let mut tags: Vec<&str> = all.iter().map(SubjectUnknown::tag).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), all.len(), "two variants share a tag");
    }

    /// A detached HEAD short-circuits before git is ever run, so this needs no
    /// repository — and proving it needs none is the point: the caller's `None`
    /// is authoritative and is not second-guessed with another probe.
    #[test]
    fn a_detached_head_resolves_to_no_subject_without_running_git() {
        let error = resolve_repository_branch(
            Path::new("/nonexistent-path-for-a-test"),
            None,
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert_eq!(error, SubjectUnknown::DetachedHead);
    }

    /// A ref name git allows but a URL does not survive is refused before any
    /// request, and refused at the boundary rather than escaped later.
    #[test]
    fn a_branch_name_a_url_cannot_carry_is_refused_before_git_runs() {
        for branch in [
            "feature/../admin",
            "has space",
            "has?query",
            "has#fragment",
            "has%25percent",
            "/leading",
            "trailing/",
            "double//slash",
            "",
        ] {
            let error = resolve_repository_branch(
                Path::new("/nonexistent-path-for-a-test"),
                Some(branch),
                Duration::from_millis(1),
            )
            .unwrap_err();
            assert_eq!(
                error,
                SubjectUnknown::UnusableBranchName,
                "{branch:?} was accepted"
            );
        }
    }

    /// The repository's own branch convention has three slashes in it, so the
    /// query encoding is not an edge case — it is the normal path.
    #[test]
    fn a_slashed_branch_is_carried_as_one_query_value() {
        let subject = RepositoryBranchSubject {
            host: "github.com".into(),
            owner: "Chisanan232".into(),
            repo: "glomeris".into(),
            branch: "v0.0.1/HORO-1546/feat/external_context".into(),
        };
        assert_eq!(
            subject.encoded_branch(),
            "v0.0.1%2FHORO-1546%2Ffeat%2Fexternal_context"
        );
        // Nothing else in the allowlist needs encoding, so an unslashed branch
        // passes through unchanged — an encoder that altered it would be
        // rewriting a ref name.
        let plain = RepositoryBranchSubject {
            branch: "main".into(),
            ..subject
        };
        assert_eq!(plain.encoded_branch(), "main");
    }

    // ---- Against real git -------------------------------------------------
    //
    // What is under test below is how git answers, so these use real git in a
    // throwaway repository. A stub would only restate the assumption, which is
    // the same reasoning `crate::workspace::branch`'s tests already follow.

    fn run(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed in {root:?}");
    }

    /// A throwaway repository on branch `trunk` with one commit and no remote.
    fn scratch_repo(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("glomeris-h1546-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch repo");
        run(&root, &["init", "--initial-branch=trunk"]);
        std::fs::write(root.join("f"), b"x").expect("write file");
        run(&root, &["add", "f"]);
        run(
            &root,
            &[
                "-c",
                "user.name=glomeris test",
                "-c",
                "user.email=test@invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "one",
            ],
        );
        root
    }

    fn resolved(root: &Path, branch: &str) -> Result<RepositoryBranchSubject, SubjectUnknown> {
        resolve_repository_branch(root, Some(branch), Duration::from_secs(10))
    }

    /// Local-only work. Ordinary, and not a fault.
    #[test]
    fn a_repository_with_no_remote_has_no_subject() {
        let root = scratch_repo("no-remote");
        assert_eq!(resolved(&root, "trunk"), Err(SubjectUnknown::NoRemote));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One remote and no branch preference: the sole remote is unambiguous, so
    /// it is used. AC 5 — the owner, repository and branch all come from the
    /// repository itself.
    #[test]
    fn a_sole_remote_identifies_the_subject() {
        let root = scratch_repo("sole-remote");
        run(
            &root,
            &[
                "remote",
                "add",
                "upstream",
                "git@github.com:Chisanan232/glomeris.git",
            ],
        );

        let subject = resolved(&root, "trunk").expect("a subject");

        assert_eq!(subject.host, "github.com");
        assert_eq!(subject.owner, "Chisanan232");
        assert_eq!(subject.repo, "glomeris");
        assert_eq!(subject.branch, "trunk");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two remotes and nothing saying which: refused. Arranged so a guess would
    /// succeed — one of them is called `origin`, the name a guesser would reach
    /// for — because the point is that Glomeris does not reach for it. Asking
    /// the fork instead of the parent returns confident answers about the wrong
    /// pull requests.
    #[test]
    fn two_remotes_without_a_branch_preference_are_ambiguous() {
        let root = scratch_repo("two-remotes");
        run(
            &root,
            &["remote", "add", "origin", "git@github.com:me/glomeris.git"],
        );
        run(
            &root,
            &[
                "remote",
                "add",
                "upstream",
                "git@github.com:Chisanan232/glomeris.git",
            ],
        );

        assert_eq!(
            resolved(&root, "trunk"),
            Err(SubjectUnknown::AmbiguousRemote)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// …and with two remotes, what the branch itself records wins. This is the
    /// only thing that makes the ambiguous case above safe to refuse: a branch
    /// that tracks something has already answered the question.
    #[test]
    fn a_branch_that_records_its_remote_resolves_against_that_one() {
        let root = scratch_repo("branch-preference");
        run(
            &root,
            &["remote", "add", "origin", "git@github.com:me/glomeris.git"],
        );
        run(
            &root,
            &[
                "remote",
                "add",
                "upstream",
                "git@github.com:Chisanan232/glomeris.git",
            ],
        );
        run(&root, &["config", "branch.trunk.remote", "upstream"]);

        let subject = resolved(&root, "trunk").expect("a subject");

        assert_eq!(subject.owner, "Chisanan232");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A remote git accepts and this module cannot read an owner out of.
    #[test]
    fn a_local_path_remote_has_no_remote_identity() {
        let root = scratch_repo("path-remote");
        run(&root, &["remote", "add", "origin", "/srv/git/mirror.git"]);

        assert_eq!(
            resolved(&root, "trunk"),
            Err(SubjectUnknown::UnreadableRemoteUrl)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A corporate remote resolves to a subject naming its own host, so the
    /// caller can decline to ask github.com about it. A parse failure here
    /// would make "this is somewhere else" look like a broken URL.
    #[test]
    fn a_non_github_remote_still_resolves_and_names_its_host() {
        let root = scratch_repo("other-host");
        run(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://git.example.test/t/s.git",
            ],
        );

        let subject = resolved(&root, "trunk").expect("a subject");

        assert_eq!(subject.host, "git.example.test");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// git itself missing is git's reason, not a missing subject.
    #[test]
    fn a_directory_that_is_not_a_repository_fails_rather_than_inventing_a_subject() {
        let root =
            std::env::temp_dir().join(format!("glomeris-h1546-not-a-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create dir");

        // `git remote` outside a repository exits non-zero with no output, which
        // reads as "no remotes" — the honest answer for a directory that has
        // none, and one that cannot be mistaken for an observation.
        assert_eq!(resolved(&root, "trunk"), Err(SubjectUnknown::NoRemote));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The encoder is only complete because the allowlist is narrow. If the
    /// allowlist ever grows, this fails and sends somebody to read
    /// `encoded_branch`.
    #[test]
    fn the_branch_allowlist_admits_nothing_else_needing_encoding() {
        for c in 0u8..=127 {
            let ch = c as char;
            let allowed = is_safe_branch_name(&format!("a{ch}b"));
            let expected = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/');
            assert_eq!(allowed, expected, "{ch:?} (0x{c:02x})");
        }
    }
}
