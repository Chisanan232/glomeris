//! Branch state for one git worktree: which branch it is on, whether it
//! holds work no remote has, and whether it has already landed on the
//! repository's default branch (HORO-1511).
//!
//! Every question here is asked to *explain* a worktree family to a
//! person. None of it is an input to whether anything may be deleted —
//! see the rule at the top of [`crate::workspace`], which is why this
//! probe is not part of [`crate::evidence`] and these facts never reach
//! [`crate::policy`].
//!
//! Up to eleven read-only `git` invocations per worktree root, run once
//! per *root* rather than once per candidate — a repository with forty
//! discovered build directories under one worktree asks these questions
//! once. Every one is local: nothing here fetches, and a branch is
//! compared against the remote-tracking refs this machine already has.
//!
//! "Up to" because the expensive half is skipped when a cheaper answer
//! already settles the question — see [`IntegrationEvidence`], which is
//! also where the bound on the expensive half is written down (HORO-1545).

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use crate::evidence::correlate::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

/// What one worktree's HEAD looks like, as separate answers.
///
/// Separate and not one boolean: "safe to remove" is not a question this
/// type answers, and collapsing these into one would be exactly the delete
/// authority HORO-1511 forbids a group summary from having.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeBranchState {
    /// The checked-out branch, or `None` for a detached HEAD. A detached
    /// HEAD is not an error and not an absence of information — it is a
    /// state some worktrees are deliberately left in.
    pub branch: Option<String>,
    pub upstream: UpstreamState,
    pub merged: MergedState,
    /// Everything about HEAD's relationship to the default branch that
    /// plain ancestry cannot express (HORO-1545).
    pub integration: IntegrationEvidence,
}

/// How this worktree's branch stands against the remote it tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamState {
    /// No upstream is configured for this branch.
    ///
    /// **Not** "nothing is unpushed". It means there is no published
    /// counterpart to compare against, so every commit here may be work
    /// that exists only on this machine. [`Self::may_hold_unpushed_work`]
    /// reads it that way on purpose.
    Untracked,
    /// Counts against the configured upstream, from
    /// `rev-list --left-right --count @{upstream}...HEAD`: `behind` is how
    /// many commits the upstream has that this branch does not, `ahead` is
    /// how many this branch has that the upstream does not.
    Tracking { ahead: u32, behind: u32 },
    /// The comparison could not be made.
    Unknown,
}

impl UpstreamState {
    /// Whether this branch may hold commits that exist nowhere else.
    ///
    /// `true` for [`Self::Untracked`] and [`Self::Unknown`] as well as for
    /// a non-zero `ahead`. The two non-answers are counted as "may" rather
    /// than "does not", because the sentence this feeds is shown to
    /// someone deciding what to let go of.
    pub fn may_hold_unpushed_work(&self) -> bool {
        match self {
            Self::Tracking { ahead, .. } => *ahead > 0,
            Self::Untracked | Self::Unknown => true,
        }
    }

    /// Stable token for JSON and for the app's vocabulary.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Untracked => "untracked",
            Self::Tracking { .. } => "tracking",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether this worktree's HEAD is already contained in the repository's
/// default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergedState {
    /// Every commit on HEAD is an ancestor of `into`.
    Merged { into: String },
    /// HEAD has at least one commit `into` does not.
    NotMerged { into: String },
    /// There is no answer: this machine has no `refs/remotes/origin/HEAD`
    /// to resolve a default branch from, or the comparison failed.
    ///
    /// Deliberately not a guess. Trying `main`, then `master`, then
    /// whatever else looks plausible would produce a confident
    /// "merged: true" against an axis nobody stated — and "already
    /// merged" is the single most persuasive thing this whole module can
    /// say about a worktree.
    Unknown,
}

impl MergedState {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Merged { .. } => "merged",
            Self::NotMerged { .. } => "not_merged",
            Self::Unknown => "unknown",
        }
    }

    /// Which branch the answer is about, for the two states that have one.
    /// A caller rendering "merged" without this is stating half a fact.
    pub fn compared_against(&self) -> Option<&str> {
        match self {
            Self::Merged { into } | Self::NotMerged { into } => Some(into),
            Self::Unknown => None,
        }
    }
}

/// How much work HEAD holds that the comparison branch does not contain,
/// split by whether an equivalent patch is already over there.
///
/// All three counts are about commits the comparison branch does **not**
/// contain, and they sum to how many that is. A merged branch therefore
/// reports zero across the board: nothing is absent, so nothing is
/// absent-but-equivalent or absent-but-unclassifiable either. That is an
/// observed zero from ancestry, not a probe that failed.
///
/// # Why the third count exists
///
/// `git cherry` says nothing at all about merge commits — there is no
/// single patch for one to have an id. Counting only its two kinds of line
/// would report "nothing unique, nothing equivalent" for a branch sitting
/// on a merge the comparison branch has never seen, and a reader would take
/// that for an absence of unique work. It is the opposite: it is a commit
/// the per-commit method declined to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    /// Commits absent from the comparison branch with no equivalent patch
    /// there — `git cherry`'s `+` lines.
    pub unique_commits: u32,
    /// Commits absent from the comparison branch whose patch *is* already
    /// there, under a different commit id — `git cherry`'s `-` lines. This
    /// is what a cherry-pick or a rebase leaves behind.
    pub equivalent_commits: u32,
    /// Commits absent from the comparison branch that `git cherry` declined
    /// to classify either way, which in practice means merge commits. Not a
    /// count of commits known to hold nothing, and not a count of commits
    /// known to hold something — a count of commits nobody asked about.
    pub unclassified_commits: u32,
}

/// Whether the work on this branch has already landed on the comparison
/// branch in some form other than ancestry.
///
/// # Why this is not a boolean
///
/// AC 4 of HORO-1545, and the reason is the same one [`MergedState`] gives:
/// "this has already landed" is the most persuasive sentence this module
/// can produce about a worktree, and a boolean has nowhere to put the two
/// answers that are not "yes" or "no". [`Self::NotApplicable`] and
/// [`Self::Unknown`] are the ones that matter — a question that could not
/// be asked and a question that has no subject are different facts, and
/// neither is a quiet `NotEquivalent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchEquivalence {
    /// The comparison branch already contains this work, by the named
    /// method. Evidence of integration — **not** a claim that ancestry
    /// holds, and not authority to delete anything (AC 3, AC 7).
    Equivalent(EquivalenceMethod),
    /// Both methods ran and neither found this work over there.
    NotEquivalent,
    /// There is nothing for equivalence to be a question about: ancestry
    /// already contains HEAD, so no commit is absent from the comparison
    /// branch.
    ///
    /// Distinct from [`Self::Unknown`] on the HORO-1561 grounds — a
    /// question that does not apply was not a question that failed.
    NotApplicable,
    /// The question applies and has no answer: no recorded default branch
    /// to compare against ([`ProbeReason::NotAttempted`]), more divergence
    /// than the bound below allows (also `NotAttempted`), or git could not
    /// answer ([`ProbeReason::Failed`] and friends).
    Unknown(ProbeReason),
}

/// How an [`PatchEquivalence::Equivalent`] answer was reached.
///
/// Reported rather than hidden because the two methods support different
/// weights of conclusion, and a reader told only "equivalent" cannot tell
/// which one they were given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquivalenceMethod {
    /// `git cherry`: every absent commit has a patch-id match on the
    /// comparison branch. The strong answer — it is per commit, and it
    /// catches cherry-picks, rebases and a single-commit squash.
    PerCommitPatchId,
    /// Every file this branch touched since the merge base has identical
    /// content on the comparison branch. The weaker answer, and the only
    /// one that survives a multi-commit squash: a squashed combination has
    /// no per-commit patch-id match by construction, so `git cherry`
    /// reports every one of its commits as unique.
    ContentIdentical,
}

impl PatchEquivalence {
    /// Stable token for JSON and for the app's vocabulary.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Equivalent(_) => "equivalent",
            Self::NotEquivalent => "not_equivalent",
            Self::NotApplicable => "not_applicable",
            Self::Unknown(_) => "unknown",
        }
    }

    /// The method, for the one state that has one.
    pub fn method(&self) -> Option<EquivalenceMethod> {
        match self {
            Self::Equivalent(method) => Some(*method),
            _ => None,
        }
    }

    /// Why there is no answer, for the one state that has a reason. A
    /// caller rendering `unknown` without this is repeating the mistake
    /// HORO-1542 fixed everywhere else.
    pub fn reason(&self) -> Option<ProbeReason> {
        match self {
            Self::Unknown(reason) => Some(*reason),
            _ => None,
        }
    }
}

impl EquivalenceMethod {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::PerCommitPatchId => "per_commit_patch_id",
            Self::ContentIdentical => "content_identical",
        }
    }
}

/// HEAD's relationship to the default branch beyond plain ancestry, plus
/// the freshness of both sides of that comparison.
///
/// # What this exists to prevent
///
/// Two opposite mistakes, and the ticket names both. Ancestry alone calls
/// squashed, rebased and cherry-picked work "not merged", which understates
/// how much has landed. And a branch that once merged may have commits
/// written since, which the merge answer keeps calling "merged" — hence the
/// two commit timestamps, so "merged" and "the branch tip is newer than the
/// branch it merged into" can be seen at the same time.
///
/// # None of it is authority
///
/// Nothing here reaches [`crate::policy`], and
/// [`crate::workspace::BranchLifecycle::unique_work`] deliberately does not
/// consult it. `Equivalent` is evidence that work landed somewhere else; it
/// is not evidence that this working tree has nothing newer in it (AC 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationEvidence {
    pub divergence: ProbeOutcome<Divergence>,
    pub equivalence: PatchEquivalence,
    /// When HEAD's commit was made.
    pub tip_committed_at: ProbeOutcome<SystemTime>,
    /// When the comparison branch's tip commit was made. `NotAttempted`
    /// when there is no recorded default branch to have a tip.
    pub comparison_tip_committed_at: ProbeOutcome<SystemTime>,
}

/// What "bounded" means for the two expensive methods (AC 6).
///
/// Values rather than constants, threaded through, so a test can shrink
/// them and prove the refusal actually happens. A bound only a
/// two-hundred-commit fixture could reach is a bound nobody checks, and an
/// unchecked bound is indistinguishable from a missing one.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    /// How many commits may diverge before the patch-equivalence work is
    /// skipped rather than attempted.
    ///
    /// `git cherry` computes a patch id for every commit on both sides,
    /// which on a branch that forked a year ago is minutes of work for a
    /// question asked while someone waits. Past this,
    /// [`PatchEquivalence::Unknown`] carries [`ProbeReason::NotAttempted`]
    /// and says so, rather than the timeout silently producing `Failed`
    /// and reading like a broken repository.
    max_divergent_commits: u32,
    /// How many changed paths may be passed to the content comparison,
    /// whose argv grows with the number of files the branch touched.
    max_compared_paths: usize,
}

impl Bounds {
    const DEFAULT: Self = Self {
        max_divergent_commits: 200,
        max_compared_paths: 512,
    };
}

impl IntegrationEvidence {
    /// Every field unanswered, because nothing was asked. The state a
    /// fixture or a caller with no git access starts from — never a state
    /// this module returns to describe a repository it *did* look at.
    pub fn not_attempted() -> Self {
        Self {
            divergence: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            equivalence: PatchEquivalence::Unknown(ProbeReason::NotAttempted),
            tip_committed_at: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            comparison_tip_committed_at: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        }
    }
}

/// Reads [`WorktreeBranchState`] for a worktree root.
pub trait BranchProbe {
    /// `Unavailable(reason)` means the probe could not run — not that the
    /// worktree is uninteresting. A caller must not convert it into a
    /// tidy default.
    fn state_of(
        &self,
        worktree_root: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<WorktreeBranchState>;
}

/// Real [`BranchProbe`] backed by the `git` CLI.
pub struct GitCliBranchProbe;

impl BranchProbe for GitCliBranchProbe {
    fn state_of(
        &self,
        worktree_root: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<WorktreeBranchState> {
        // `--abbrev-ref HEAD` prints the literal `HEAD` for a detached
        // head, which is the one branch name git guarantees cannot be a
        // real branch here.
        let head = match run_git(
            worktree_root,
            &["rev-parse", "--abbrev-ref", "HEAD"],
            timeout,
        ) {
            Ok(output) if output.status.success() => trimmed(&output.stdout),
            // An empty repository has no HEAD to resolve. That is the
            // probe having no subject, not the probe failing.
            Ok(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
            Err(outcome) => return outcome,
        };
        let branch = if head == "HEAD" { None } else { Some(head) };

        // Resolved once and threaded through: `merged` needs the ref to
        // compare against and so does everything in `integration`, and
        // asking `symbolic-ref` twice would make the invocation count in
        // this module's header a lie for no gain.
        let axis = default_ref(worktree_root, timeout);
        let merged = containment(worktree_root, axis.as_deref(), timeout);
        let integration = integration_evidence(
            worktree_root,
            axis.as_deref(),
            &merged,
            Bounds::DEFAULT,
            timeout,
        );

        ProbeOutcome::Observed(WorktreeBranchState {
            branch,
            upstream: upstream_state(worktree_root, timeout),
            merged,
            integration,
        })
    }
}

/// `ahead`/`behind` against the configured upstream.
///
/// A non-zero exit is the ordinary way git says "this branch has no
/// upstream", so it becomes [`UpstreamState::Untracked`] rather than an
/// error. A probe-level failure (git absent, timed out) becomes
/// [`UpstreamState::Unknown`]; both read as "may hold unpushed work", so
/// the distinction costs nothing in safety and keeps the reported reason
/// honest.
fn upstream_state(worktree_root: &Path, timeout: Duration) -> UpstreamState {
    let output = match run_git(
        worktree_root,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        timeout,
    ) {
        Ok(output) if output.status.success() => output,
        Ok(_) => return UpstreamState::Untracked,
        Err(_) => return UpstreamState::Unknown,
    };
    parse_left_right_count(&output.stdout).unwrap_or(UpstreamState::Unknown)
}

/// `git rev-list --left-right --count A...B` prints two tab-separated
/// counts on one line: left first (`A` has, `B` lacks), right second. With
/// `@{upstream}...HEAD` that is `behind`, then `ahead`.
///
/// The order is the whole content of this function, and getting it
/// backwards would report an entirely-pushed branch as holding unique
/// work and an unpushed one as holding none.
fn parse_left_right_count(stdout: &[u8]) -> Option<UpstreamState> {
    let text = String::from_utf8_lossy(stdout);
    let mut fields = text.split_whitespace();
    let behind: u32 = fields.next()?.parse().ok()?;
    let ahead: u32 = fields.next()?.parse().ok()?;
    Some(UpstreamState::Tracking { ahead, behind })
}

/// The full ref this repository records as its default branch, or `None`.
///
/// It comes from `refs/remotes/origin/HEAD`, the pointer `git clone` writes
/// and `git remote set-head` maintains. Nothing is guessed when it is
/// missing; see [`MergedState::Unknown`] for why.
fn default_ref(worktree_root: &Path, timeout: Duration) -> Option<String> {
    let resolved = match run_git(
        worktree_root,
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
        timeout,
    ) {
        Ok(output) if output.status.success() => trimmed(&output.stdout),
        _ => return None,
    };
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

/// Whether HEAD is already contained in the default branch.
fn containment(worktree_root: &Path, default_ref: Option<&str>, timeout: Duration) -> MergedState {
    let Some(default_ref) = default_ref else {
        return MergedState::Unknown;
    };
    let into = short_ref(default_ref);

    match run_git(
        worktree_root,
        &["merge-base", "--is-ancestor", "HEAD", default_ref],
        timeout,
    ) {
        // Exit 0: every commit on HEAD is already in the default branch.
        Ok(output) if output.status.success() => MergedState::Merged { into },
        // Exit 1 is git's answer "no"; anything else is git failing, and
        // an unmerged branch must not be reported as merged either way.
        Ok(output) if output.status.code() == Some(1) => MergedState::NotMerged { into },
        _ => MergedState::Unknown,
    }
}

/// Everything ancestry cannot say, for one worktree.
///
/// # The order of the work, and why the cheap answers come first
///
/// Reading down: HEAD's own commit time needs no comparison branch and is
/// always asked. With no default branch recorded, nothing else can be
/// asked at all, and every field says `not_attempted` rather than
/// `failed` — no probe ran, so no probe failed. When ancestry already
/// contains HEAD there is nothing absent to be equivalent, so the two
/// counts are an observed zero and equivalence is `NotApplicable` — and
/// the expensive half is skipped entirely, which is the common case on a
/// tidy machine. Only a genuinely unmerged branch pays for `git cherry`,
/// and only past a bound.
fn integration_evidence(
    worktree_root: &Path,
    default_ref: Option<&str>,
    merged: &MergedState,
    bounds: Bounds,
    timeout: Duration,
) -> IntegrationEvidence {
    let tip_committed_at = commit_time(worktree_root, "HEAD", timeout);

    let Some(default_ref) = default_ref else {
        return IntegrationEvidence {
            tip_committed_at,
            ..IntegrationEvidence::not_attempted()
        };
    };
    let comparison_tip_committed_at = commit_time(worktree_root, default_ref, timeout);

    let (divergence, equivalence) = match merged {
        // Contained, so nothing is absent from the comparison branch —
        // which makes both counts zero as a fact rather than as a default,
        // and makes equivalence a question with no subject.
        MergedState::Merged { .. } => (
            ProbeOutcome::Observed(Divergence {
                unique_commits: 0,
                equivalent_commits: 0,
                unclassified_commits: 0,
            }),
            PatchEquivalence::NotApplicable,
        ),
        // The ref is recorded but the comparison itself did not answer — a
        // default branch this machine does not have, most often. The
        // question applies; git could not answer it.
        MergedState::Unknown => (
            ProbeOutcome::Unavailable(ProbeReason::Failed),
            PatchEquivalence::Unknown(ProbeReason::Failed),
        ),
        MergedState::NotMerged { .. } => {
            divergence_and_equivalence(worktree_root, default_ref, bounds, timeout)
        }
    };

    IntegrationEvidence {
        divergence,
        equivalence,
        tip_committed_at,
        comparison_tip_committed_at,
    }
}

/// The two methods, for a branch ancestry says is not merged.
fn divergence_and_equivalence(
    worktree_root: &Path,
    default_ref: &str,
    bounds: Bounds,
    timeout: Duration,
) -> (ProbeOutcome<Divergence>, PatchEquivalence) {
    let unanswered = |reason: ProbeReason| {
        (
            ProbeOutcome::Unavailable(reason),
            PatchEquivalence::Unknown(reason),
        )
    };

    // Cheap: how many commits `git cherry` would have to compute patch ids
    // for. Asked first so the bound can refuse *before* the cost, and so
    // the refusal is a stated `not_attempted` rather than a timeout.
    let ahead = match run_git(
        worktree_root,
        &["rev-list", "--count", &format!("{default_ref}..HEAD")],
        timeout,
    ) {
        Ok(output) if output.status.success() => match trimmed(&output.stdout).parse::<u32>() {
            Ok(count) => count,
            Err(_) => return unanswered(ProbeReason::Failed),
        },
        _ => return unanswered(ProbeReason::Failed),
    };
    if ahead == 0 {
        // Ancestry said not merged and yet nothing is ahead. Some ref moved
        // under us between the two invocations. Reporting either answer
        // would be reporting a repository that no longer exists.
        return unanswered(ProbeReason::Failed);
    }
    if ahead > bounds.max_divergent_commits {
        return unanswered(ProbeReason::NotAttempted);
    }

    let (unique_commits, equivalent_commits) =
        match run_git(worktree_root, &["cherry", default_ref, "HEAD"], timeout) {
            Ok(output) if output.status.success() => parse_cherry(&output.stdout),
            _ => return unanswered(ProbeReason::Failed),
        };
    // Whatever `rev-list` counted and `cherry` then declined to speak
    // about. Saturating because the two invocations are not one atomic read
    // of the repository: a ref that moves in between could leave `cherry`
    // describing more commits than `rev-list` counted, and the honest
    // reading of that is "none unaccounted for", not a wrapped u32.
    let divergence = Divergence {
        unique_commits,
        equivalent_commits,
        unclassified_commits: ahead.saturating_sub(unique_commits + equivalent_commits),
    };

    let equivalence = if divergence.unique_commits == 0 && divergence.unclassified_commits == 0 {
        // Every commit ahead has a patch-id twin over there. The strong
        // answer, and the only one reached without further work. Reached
        // only with at least one such commit: `ahead` was non-zero, and
        // nothing is unique or unclassified, so the remainder are twins.
        PatchEquivalence::Equivalent(EquivalenceMethod::PerCommitPatchId)
    } else {
        // Either some commit has no twin over there, or `cherry` declined
        // to describe it. Both leave the per-commit method short of an
        // answer rather than at a negative one, and comparing the trees can
        // still reach one — which is the case a squash lands in, and also
        // the case where the same work arrived through a different merge.
        match content_equivalence(worktree_root, default_ref, bounds, timeout) {
            Some(true) => PatchEquivalence::Equivalent(EquivalenceMethod::ContentIdentical),
            Some(false) => PatchEquivalence::NotEquivalent,
            None => PatchEquivalence::Unknown(ProbeReason::Failed),
        }
    };

    (ProbeOutcome::Observed(divergence), equivalence)
}

/// `git cherry <upstream> <head>` prints one line per commit on `head`
/// that `upstream` does not contain: `+ <sha>` when no equivalent patch
/// exists upstream, `- <sha>` when one does.
///
/// Returns the two counts rather than a [`Divergence`], because the third
/// count in one is not a thing this output can be read for — it is what the
/// output failed to mention, which only the caller's `rev-list` total knows.
fn parse_cherry(stdout: &[u8]) -> (u32, u32) {
    let text = String::from_utf8_lossy(stdout);
    let (mut unique, mut equivalent) = (0, 0);
    for line in text.lines() {
        match line.as_bytes().first() {
            Some(b'+') => unique += 1,
            Some(b'-') => equivalent += 1,
            _ => {}
        }
    }
    (unique, equivalent)
}

/// The squash-merge method: does every file this branch touched since the
/// merge base have identical content on the comparison branch?
///
/// `Some(true)` is integration evidence a multi-commit squash cannot
/// produce any other way — combining commits destroys their individual
/// patch ids, so [`EquivalenceMethod::PerCommitPatchId`] reports every one
/// of them as unique. `Some(false)` is a real negative. `None` is "no
/// answer", and is returned for every condition that would otherwise
/// become an accidental yes.
///
/// # Deliberately conservative in one direction
///
/// A rename, or later unrelated edits to the same files on the comparison
/// branch, both make the content differ and so report `Some(false)` for
/// work that did land. That is the tolerable error; the intolerable one is
/// the reverse. Read-only throughout: `git commit-tree`, the usual trick
/// for this question, writes an object into the repository, which the
/// ticket forbids.
fn content_equivalence(
    worktree_root: &Path,
    default_ref: &str,
    bounds: Bounds,
    timeout: Duration,
) -> Option<bool> {
    let base = match run_git(worktree_root, &["merge-base", "HEAD", default_ref], timeout) {
        Ok(output) if output.status.success() => trimmed(&output.stdout),
        _ => return None,
    };
    if base.is_empty() {
        return None;
    }

    let changed = match run_git(
        worktree_root,
        &["diff", "--name-only", "-z", &base, "HEAD"],
        timeout,
    ) {
        Ok(output) if output.status.success() => output.stdout,
        _ => return None,
    };
    let paths: Vec<String> = String::from_utf8_lossy(&changed)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
    // An empty path list is the trap in this method: `git diff --quiet A B
    // --` with no pathspec compares the two trees entire, which for a
    // branch that changed nothing would be a confident `Some(true)` about
    // a comparison nobody made. There is also nothing to conclude from it.
    if paths.is_empty() || paths.len() > bounds.max_compared_paths {
        return None;
    }

    let mut args: Vec<&str> = vec!["diff", "--quiet", default_ref, "HEAD", "--"];
    args.extend(paths.iter().map(String::as_str));
    // Filenames are data here, not patterns: a path containing `*` or a
    // leading `:` would otherwise be read as a pathspec and silently match
    // the wrong set of files.
    match run_git_literal_pathspecs(worktree_root, &args, timeout) {
        Ok(output) if output.status.success() => Some(true),
        Ok(output) if output.status.code() == Some(1) => Some(false),
        _ => None,
    }
}

/// When the commit at `rev` was made.
///
/// `%ct` is the committer date in seconds since the epoch — the commit's
/// own record, so it survives a `git clone` where a file mtime would not.
fn commit_time(worktree_root: &Path, rev: &str, timeout: Duration) -> ProbeOutcome<SystemTime> {
    let output = match run_git(worktree_root, &["log", "-1", "--format=%ct", rev], timeout) {
        Ok(output) if output.status.success() => output,
        Ok(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
        // A missing or timed-out `git` keeps its own reason: this is the
        // one place where the distinction is still recoverable.
        Err(ProbeOutcome::Unavailable(reason)) => return ProbeOutcome::Unavailable(reason),
        Err(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
    };
    match trimmed(&output.stdout).parse::<u64>() {
        Ok(seconds) => {
            ProbeOutcome::Observed(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        }
        Err(_) => ProbeOutcome::Unavailable(ProbeReason::Failed),
    }
}

/// `refs/remotes/origin/main` reads as `origin/main` — the form a person
/// recognises, and the form `git log` prints.
fn short_ref(full: &str) -> String {
    full.strip_prefix("refs/remotes/")
        .unwrap_or(full)
        .to_string()
}

fn trimmed(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout).trim().to_string()
}

type BranchRunResult = Result<Output, ProbeOutcome<WorktreeBranchState>>;

fn run_git(path: &Path, args: &[&str], timeout: Duration) -> BranchRunResult {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    finish_git(command, timeout)
}

/// As [`run_git`], with pathspec globbing and magic disabled — for the one
/// invocation that passes filenames read out of the repository back into
/// git.
fn run_git_literal_pathspecs(path: &Path, args: &[&str], timeout: Duration) -> BranchRunResult {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_LITERAL_PATHSPECS", "1");
    finish_git(command, timeout)
}

fn finish_git(command: Command, timeout: Duration) -> BranchRunResult {
    match run_with_timeout(command, timeout) {
        CommandOutcome::NotFound => Err(ProbeOutcome::Unavailable(ProbeReason::ToolAbsent)),
        CommandOutcome::TimedOut => Err(ProbeOutcome::Unavailable(ProbeReason::TimedOut)),
        CommandOutcome::SpawnFailed => Err(ProbeOutcome::Unavailable(ProbeReason::Failed)),
        CommandOutcome::Completed(output) => Ok(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn left_is_behind_and_right_is_ahead() {
        assert_eq!(
            parse_left_right_count(b"3\t7\n"),
            Some(UpstreamState::Tracking {
                ahead: 7,
                behind: 3
            })
        );
    }

    #[test]
    fn an_up_to_date_branch_counts_zero_both_ways() {
        assert_eq!(
            parse_left_right_count(b"0\t0\n"),
            Some(UpstreamState::Tracking {
                ahead: 0,
                behind: 0
            })
        );
    }

    #[test]
    fn unparsable_counts_are_not_a_count() {
        assert_eq!(parse_left_right_count(b""), None);
        assert_eq!(parse_left_right_count(b"3\n"), None);
        assert_eq!(parse_left_right_count(b"fatal: no upstream\n"), None);
    }

    /// The two non-answers are counted as "may", which is the direction
    /// this function exists to fix. A branch with no upstream has nothing
    /// published anywhere, and reporting it as holding no unique work is
    /// the reassurance most likely to be acted on.
    #[test]
    fn a_branch_with_no_upstream_may_hold_unique_work() {
        assert!(UpstreamState::Untracked.may_hold_unpushed_work());
        assert!(UpstreamState::Unknown.may_hold_unpushed_work());
        assert!(UpstreamState::Tracking {
            ahead: 1,
            behind: 0
        }
        .may_hold_unpushed_work());
        assert!(!UpstreamState::Tracking {
            ahead: 0,
            behind: 9
        }
        .may_hold_unpushed_work());
    }

    #[test]
    fn a_remote_tracking_ref_is_shortened_to_the_form_people_read() {
        assert_eq!(short_ref("refs/remotes/origin/main"), "origin/main");
        assert_eq!(short_ref("refs/heads/main"), "refs/heads/main");
    }

    /// `merged` is never a bare claim: the two states that answer it name
    /// the branch they answered against, and the one that does not answer
    /// names nothing.
    #[test]
    fn merged_always_states_which_branch_it_is_about() {
        assert_eq!(
            MergedState::Merged {
                into: "origin/main".to_string()
            }
            .compared_against(),
            Some("origin/main")
        );
        assert_eq!(
            MergedState::NotMerged {
                into: "origin/main".to_string()
            }
            .compared_against(),
            Some("origin/main")
        );
        assert_eq!(MergedState::Unknown.compared_against(), None);
    }

    #[test]
    fn every_state_has_a_distinct_tag() {
        let upstream = [
            UpstreamState::Untracked.tag(),
            UpstreamState::Tracking {
                ahead: 0,
                behind: 0,
            }
            .tag(),
            UpstreamState::Unknown.tag(),
        ];
        assert_eq!(
            upstream
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );

        let merged = [
            MergedState::Merged {
                into: String::new(),
            }
            .tag(),
            MergedState::NotMerged {
                into: String::new(),
            }
            .tag(),
            MergedState::Unknown.tag(),
        ];
        assert_eq!(
            merged
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
    }

    /// A throwaway repository with one commit, used by the two
    /// `merged_state` tests below. Real `git`, because what is under test
    /// is how git answers — a fake would only restate the assumption.
    fn scratch_repo(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("glomeris-h1511-{name}-{}", std::process::id()));
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

    /// The merge answer as the real probe produces it.
    ///
    /// Through [`GitCliBranchProbe::state_of`] rather than through
    /// `default_ref` + `containment` directly, so that these tests cannot
    /// keep passing against a composition that exists only here — HORO-1545
    /// split the one `merged_state` function into two, and a test-local
    /// reassembly of them is exactly the thing that would then be free to
    /// drift from what the product runs.
    fn merged_state(root: &Path, timeout: Duration) -> MergedState {
        match GitCliBranchProbe.state_of(root, timeout) {
            ProbeOutcome::Observed(state) => state.merged,
            other => panic!("expected an answer for {root:?}, got {other:?}"),
        }
    }

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

    /// The no-guess rule, against a real repository, arranged so a guess
    /// would actually succeed: there is no `refs/remotes/origin/HEAD`, but
    /// there *is* a `refs/remotes/origin/main` containing HEAD. Falling
    /// back to the popular name would therefore return a confident
    /// "merged into origin/main" about a repository that never said main
    /// was its default branch — the most persuasive thing this module can
    /// say, asserted against an axis nobody stated.
    ///
    /// The repo is deliberately on a branch called `trunk` so the guessed
    /// ref is not the branch under test.
    #[test]
    fn without_a_recorded_default_branch_there_is_no_merge_answer() {
        let root = scratch_repo("no-origin-head");
        run(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);

        let state = merged_state(&root, Duration::from_secs(10));

        assert_eq!(state, MergedState::Unknown);
        assert_eq!(state.compared_against(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `symbolic-ref` succeeds whether or not its target exists, so a
    /// repository can name a default branch this machine does not have —
    /// after a branch rename upstream, for instance. `merge-base` then
    /// fails outright, which is neither "merged" nor "not merged", and
    /// must not be reported as the reassuring one.
    #[test]
    fn a_default_branch_this_machine_does_not_have_is_not_a_merge_answer() {
        let root = scratch_repo("dangling-origin-head");
        run(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/gone",
            ],
        );

        assert_eq!(
            merged_state(&root, Duration::from_secs(10)),
            MergedState::Unknown
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The other half: when the repository does record a default branch,
    /// the answer names it. Here HEAD *is* that ref, so every commit on
    /// HEAD is trivially contained in it.
    #[test]
    fn with_a_recorded_default_branch_the_answer_names_it() {
        let root = scratch_repo("origin-head");
        // A remote-tracking ref and the pointer `git clone` would have
        // written, without a network: point origin/HEAD at a copy of the
        // branch this repo is actually on.
        run(&root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        run(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );

        let state = merged_state(&root, Duration::from_secs(10));

        assert_eq!(
            state,
            MergedState::Merged {
                into: "origin/trunk".to_string()
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A branch with a commit the default branch does not have is
    /// `NotMerged`, and says so about the same named ref.
    #[test]
    fn a_branch_ahead_of_the_default_is_not_merged_into_it() {
        let root = scratch_repo("ahead-of-default");
        run(&root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        run(
            &root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        std::fs::write(root.join("g"), b"y").expect("write file");
        run(&root, &["add", "g"]);
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
                "two",
            ],
        );

        assert_eq!(
            merged_state(&root, Duration::from_secs(10)),
            MergedState::NotMerged {
                into: "origin/trunk".to_string()
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The one real-subprocess test, on the same grounds as the git probe
    /// in `evidence::correlate`: this checkout is itself a git repository,
    /// so `git` is guaranteed present wherever the suite runs.
    ///
    /// Asserts shape, not values. CI may run from a detached HEAD, from a
    /// clone with no `origin/HEAD`, and from a branch with or without an
    /// upstream — all of which this type has a state for.
    #[test]
    fn the_real_probe_answers_for_this_checkout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        match GitCliBranchProbe.state_of(root, Duration::from_secs(10)) {
            ProbeOutcome::Observed(state) => {
                // A detached HEAD is `None`; a named branch is never the
                // literal string git uses to mean detached.
                assert_ne!(state.branch.as_deref(), Some("HEAD"));
                if let MergedState::Merged { into } | MergedState::NotMerged { into } =
                    &state.merged
                {
                    assert!(!into.is_empty());
                }
            }
            other => panic!("expected an answer for this checkout, got {other:?}"),
        }
    }
}
