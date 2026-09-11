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

        let worktree = match run_git(
            &repo_root,
            &["rev-parse", "--git-dir", "--git-common-dir"],
            timeout,
        ) {
            Ok(output) if output.status.success() => is_linked_worktree(&output.stdout),
            Ok(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
            Err(outcome) => return outcome,
        };

        ProbeOutcome::Observed(Some(GitState {
            repo_root,
            dirty,
            untracked,
            worktree,
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

/// `git rev-parse --git-dir --git-common-dir` prints one path per line.
/// In the main working copy of a repo they are identical; in a linked
/// worktree (`git worktree add`) `--git-dir` points at
/// `<main-repo>/.git/worktrees/<name>` while `--git-common-dir` points
/// at the shared `<main-repo>/.git` — so the two lines differing is the
/// signal that `path` is a linked worktree, not the main checkout.
fn is_linked_worktree(stdout: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    let git_dir = lines.next().unwrap_or("");
    let common_dir = lines.next().unwrap_or("");
    !git_dir.is_empty() && !common_dir.is_empty() && git_dir != common_dir
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
    fn is_linked_worktree_true_when_git_dir_and_common_dir_differ() {
        assert!(is_linked_worktree(
            b"/repo/.git/worktrees/feature\n/repo/.git\n"
        ));
    }

    #[test]
    fn is_linked_worktree_false_when_git_dir_and_common_dir_match() {
        assert!(!is_linked_worktree(b".git\n.git\n"));
    }

    #[test]
    fn is_linked_worktree_false_on_malformed_output() {
        assert!(!is_linked_worktree(b""));
        assert!(!is_linked_worktree(b"only_one_line\n"));
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
            }
            other => panic!("expected Observed(Some(_)) against this repo, got {other:?}"),
        }
    }
}
