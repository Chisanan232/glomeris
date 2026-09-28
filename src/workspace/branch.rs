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
//! Three read-only `git` invocations per worktree root, run once per
//! *root* rather than once per candidate — a repository with forty
//! discovered build directories under one worktree asks these three
//! questions once. All three are local: nothing here fetches, and a
//! branch is compared against the remote-tracking refs this machine
//! already has.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use crate::evidence::correlate::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

/// What one worktree's HEAD looks like, as three separate answers.
///
/// Three and not one boolean: "safe to remove" is not a question this type
/// answers, and collapsing these into one would be exactly the delete
/// authority HORO-1511 forbids a group summary from having.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeBranchState {
    /// The checked-out branch, or `None` for a detached HEAD. A detached
    /// HEAD is not an error and not an absence of information — it is a
    /// state some worktrees are deliberately left in.
    pub branch: Option<String>,
    pub upstream: UpstreamState,
    pub merged: MergedState,
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

        ProbeOutcome::Observed(WorktreeBranchState {
            branch,
            upstream: upstream_state(worktree_root, timeout),
            merged: merged_state(worktree_root, timeout),
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

/// Whether HEAD is already contained in the default branch.
///
/// The default branch comes from `refs/remotes/origin/HEAD`, the pointer
/// `git clone` writes and `git remote set-head` maintains. A repository
/// without it reports [`MergedState::Unknown`]; see that variant's doc for
/// why no fallback name is tried.
fn merged_state(worktree_root: &Path, timeout: Duration) -> MergedState {
    let default_ref = match run_git(
        worktree_root,
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
        timeout,
    ) {
        Ok(output) if output.status.success() => trimmed(&output.stdout),
        _ => return MergedState::Unknown,
    };
    if default_ref.is_empty() {
        return MergedState::Unknown;
    }
    let into = short_ref(&default_ref);

    match run_git(
        worktree_root,
        &["merge-base", "--is-ancestor", "HEAD", &default_ref],
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
