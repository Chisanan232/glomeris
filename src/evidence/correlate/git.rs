//! `git`-backed probe for whether a resource's containing directory is
//! inside a git working tree, and if so, its dirty/untracked/worktree
//! state.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use super::timeout::{run_with_timeout, CommandOutcome};
use super::GitState;
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

pub trait GitProbe {
    /// `Observed(None)` means the probe ran successfully and determined
    /// `path` is genuinely not inside a git working tree — a complete,
    /// legitimate answer, not a missing one. `Unavailable(reason)` means
    /// the probe itself could not run to completion.
    fn state_of(&self, path: &Path, timeout: Duration) -> ProbeOutcome<Option<GitState>>;
}

/// Real [`GitProbe`] backed by the `git` CLI.
pub struct GitCliProbe;

impl GitProbe for GitCliProbe {
    fn state_of(&self, path: &Path, timeout: Duration) -> ProbeOutcome<Option<GitState>> {
        let toplevel = match run_git(path, &["rev-parse", "--show-toplevel"], timeout) {
            Ok(output) => output,
            Err(outcome) => return outcome,
        };

        if !toplevel.status.success() {
            // `git rev-parse --show-toplevel` fails with a non-zero exit
            // whenever `path` is not inside any git working tree at all.
            // That's a legitimate, fully-determined answer — never
            // Unavailable, which is reserved for the probe itself
            // failing to run.
            return ProbeOutcome::Observed(None);
        }
        let Ok(repo_root_str) = String::from_utf8(toplevel.stdout) else {
            return ProbeOutcome::Unavailable(ProbeReason::Failed);
        };
        let repo_root = PathBuf::from(repo_root_str.trim());

        let status = match run_git(&repo_root, &["status", "--porcelain"], timeout) {
            Ok(output) if output.status.success() => output,
            Ok(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
            Err(outcome) => return outcome,
        };
        let (dirty, untracked) = parse_status_porcelain(&status.stdout);

        let dirs = match run_git(
            &repo_root,
            &["rev-parse", "--git-dir", "--git-common-dir"],
            timeout,
        ) {
            Ok(output) if output.status.success() => output,
            Ok(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
            Err(outcome) => return outcome,
        };
        let Some(dirs) = parse_git_dirs(&dirs.stdout, &repo_root) else {
            return ProbeOutcome::Unavailable(ProbeReason::Failed);
        };

        ProbeOutcome::Observed(Some(GitState {
            repo_root,
            common_dir: dirs.common_dir,
            dirty,
            untracked,
            worktree: dirs.linked_worktree,
        }))
    }
}

/// Runs `git -C <path> <args>` under `timeout`. `Ok` carries the raw
/// [`Output`] (caller still checks `status.success()` — git's own
/// nonzero-exit-is-a-real-error-vs-legitimate-answer distinction varies
/// by subcommand, so that judgment stays with the caller). `Err` carries
/// an already-final [`ProbeOutcome`] for a probe-level failure (tool
/// absent, timeout, spawn failure).
type GitRunResult = Result<Output, ProbeOutcome<Option<GitState>>>;

fn run_git(path: &Path, args: &[&str], timeout: Duration) -> GitRunResult {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    match run_with_timeout(command, timeout) {
        CommandOutcome::NotFound => Err(ProbeOutcome::Unavailable(ProbeReason::ToolAbsent)),
        CommandOutcome::TimedOut => Err(ProbeOutcome::Unavailable(ProbeReason::TimedOut)),
        CommandOutcome::SpawnFailed => Err(ProbeOutcome::Unavailable(ProbeReason::Failed)),
        CommandOutcome::Completed(output) => Ok(output),
    }
}

/// `dirty` is true if any tracked change (staged or unstaged) is
/// present; `untracked` is true if any `??` (untracked file) line is
/// present. A repo can be untracked-only (dirty=false, untracked=true).
fn parse_status_porcelain(stdout: &[u8]) -> (bool, bool) {
    let text = String::from_utf8_lossy(stdout);
    let mut dirty = false;
    let mut untracked = false;
    for line in text.lines() {
        if line.starts_with("??") {
            untracked = true;
        } else if !line.is_empty() {
            dirty = true;
        }
    }
    (dirty, untracked)
}

/// The two facts `git rev-parse --git-dir --git-common-dir` carries.
struct GitDirs {
    /// Absolute path to the git directory shared by the whole worktree
    /// family. See [`GitState::common_dir`].
    common_dir: PathBuf,
    /// Whether `asked_from` is a `git worktree add` sibling rather than
    /// the main checkout.
    linked_worktree: bool,
}

/// `git rev-parse --git-dir --git-common-dir` prints one path per line.
/// In the main working copy of a repo they are identical; in a linked
/// worktree (`git worktree add`) `--git-dir` points at
/// `<main-repo>/.git/worktrees/<name>` while `--git-common-dir` points
/// at the shared `<main-repo>/.git` — so the two lines differing is the
/// signal that `asked_from` is a linked worktree, not the main checkout.
///
/// Both lines may be relative, and in practice the main-checkout case is:
/// git answers a bare `.git` there and absolute paths in a linked
/// worktree. They are relative to the directory git was *asked from*,
/// which is `asked_from` — so `common_dir` is resolved against it. A
/// relative `common_dir` kept as-is would be the string `.git` for every
/// repository on the machine, and HORO-1511 groups worktrees by this
/// value.
///
/// `None` when the output is not two non-empty lines. That is a probe
/// that did not answer, reported as `Unavailable` rather than as a main
/// checkout — "not a linked worktree" is a claim, and an unparsed answer
/// does not support it.
fn parse_git_dirs(stdout: &[u8], asked_from: &Path) -> Option<GitDirs> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    let git_dir = lines.next().unwrap_or("").trim();
    let common_dir = lines.next().unwrap_or("").trim();
    if git_dir.is_empty() || common_dir.is_empty() {
        return None;
    }
    Some(GitDirs {
        // `join` returns the argument unchanged when it is already
        // absolute, so the linked-worktree case passes through.
        common_dir: asked_from.join(common_dir),
        linked_worktree: git_dir != common_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_porcelain_detects_dirty_and_untracked_independently() {
        assert_eq!(parse_status_porcelain(b""), (false, false));
        assert_eq!(parse_status_porcelain(b"?? new_file.txt\n"), (false, true));
        assert_eq!(parse_status_porcelain(b" M changed.txt\n"), (true, false));
        assert_eq!(
            parse_status_porcelain(b" M changed.txt\n?? new_file.txt\n"),
            (true, true)
        );
    }

    #[test]
    fn a_linked_worktree_is_recognised_and_keeps_the_shared_git_dir() {
        let dirs = parse_git_dirs(
            b"/repo/.git/worktrees/feature\n/repo/.git\n",
            Path::new("/repo-feature"),
        )
        .expect("two non-empty lines");

        assert!(dirs.linked_worktree);
        // Already absolute, so `asked_from` does not enter it.
        assert_eq!(dirs.common_dir, PathBuf::from("/repo/.git"));
    }

    #[test]
    fn the_main_checkout_is_not_a_linked_worktree() {
        let dirs =
            parse_git_dirs(b".git\n.git\n", Path::new("/repo")).expect("two non-empty lines");

        assert!(!dirs.linked_worktree);
    }

    /// The load-bearing conversion. Git answers a bare `.git` in a main
    /// checkout, so two unrelated repositories produce byte-identical
    /// output here — and HORO-1511 groups worktrees by `common_dir`. Left
    /// relative, every repository on the machine would look like one
    /// family.
    #[test]
    fn a_relative_common_dir_is_resolved_against_the_repo_it_came_from() {
        let one = parse_git_dirs(b".git\n.git\n", Path::new("/work/alpha")).expect("two lines");
        let other = parse_git_dirs(b".git\n.git\n", Path::new("/work/beta")).expect("two lines");

        assert_eq!(one.common_dir, PathBuf::from("/work/alpha/.git"));
        assert_ne!(
            one.common_dir, other.common_dir,
            "two unrelated main checkouts must not share a family key"
        );
    }

    /// Unparsed output is not an answer. Before HORO-1511 this returned
    /// `false`, i.e. "a main checkout" — a claim the output does not
    /// support.
    #[test]
    fn malformed_output_is_not_an_answer() {
        assert!(parse_git_dirs(b"", Path::new("/repo")).is_none());
        assert!(parse_git_dirs(b"only_one_line\n", Path::new("/repo")).is_none());
    }

    /// The one deliberate real-subprocess smoke test allowed by this
    /// ticket's scope, chosen because `git` is guaranteed present in
    /// this repo's own CI (the checkout itself is a git repository).
    /// Tolerates CI running from a plain clone rather than a worktree,
    /// so it does not assert on `worktree`'s value.
    #[test]
    fn real_git_probe_against_this_repo_observes_some_git_state() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        match GitCliProbe.state_of(manifest_dir, Duration::from_secs(5)) {
            ProbeOutcome::Observed(Some(state)) => {
                assert!(!state.repo_root.as_os_str().is_empty());
                // Whether CI runs from a clone or from a worktree, the
                // family key has to be a path something can be grouped
                // by — which a bare `.git` is not.
                assert!(state.common_dir.is_absolute(), "{:?}", state.common_dir);
                assert!(state.common_dir.starts_with(&state.repo_root) || state.worktree);
            }
            other => panic!("expected Observed(Some(_)) against this repo, got {other:?}"),
        }
    }
}
