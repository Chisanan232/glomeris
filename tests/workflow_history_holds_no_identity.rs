//! What the workflow baseline writes to disk, checked against the bytes
//! (HORO-1547).
//!
//! The types were built so they cannot hold a path or a name: a
//! `RepositoryObservation` carries a [`glomeris::workspace::history::LocalAlias`]
//! and four counts, and the serialized form carries the alias as a number. That
//! is an argument about the code. This test is the observation — a real
//! repository with a real project name and a real branch name is enumerated by
//! the real `git` census, recorded through the real store, and the file it
//! produced is searched for every one of those strings.
//!
//! # Why bytes and not the struct
//!
//! A struct assertion proves what somebody meant. The store serializes through
//! `serde`, and the cheap way to make a future field debuggable is to add
//! `#[serde(rename)]`, a `Debug` string, a "for troubleshooting" path beside the
//! alias, or a `source` field naming where an observation came from. Every one of
//! those compiles, keeps the types honest, and puts a corporate project name in a
//! file. Only reading the file catches it.
//!
//! # The anti-vacuity half
//!
//! A test that searched an empty file would pass for the wrong reason, and this
//! one is set up to be exactly that if the census breaks: no `git`, a failed
//! enumeration and an unwritable path all produce a store with nothing in it.
//! So it first asserts the observation actually saw one repository with two
//! working trees on two named branches, and that the file on disk is
//! non-trivial, before asserting what is absent from it.
//!
//! Checked by mutation while it was written: giving `StoredState` a
//! `collected_from: String` filled with the history file's own path — a
//! plausible troubleshooting field that every other test in the store passes
//! with — fails here, naming the leaked path component. A project name lives
//! inside a path, so that one mutation covers both halves of what is asserted
//! absent.

use glomeris::workspace::history::{
    read, record, GitCliWorktreeCensus, StoreState, WorkspaceObservation,
};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Every string that must not reach the file. Distinctive enough that a match
/// cannot be coincidence, and shaped like what they stand in for: a project
/// name, a ticket-bearing branch, a customer name in a worktree directory.
const PROJECT_NAME: &str = "horo1547-acme-billing-core";
const MAIN_BRANCH: &str = "horo1547-trunk-mainline";
const WORKTREE_BRANCH: &str = "horo1547-feat-acme-invoice-export";
const WORKTREE_DIR: &str = "horo1547-acme-billing-core-invoices";

const TIMEOUT: Duration = Duration::from_secs(10);

fn temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock at or after UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "horo1547-identity-{label}-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create the temp directory");
    path
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        // A committer identity on the command line, before the subcommand, so
        // the test neither depends on nor touches whatever global git identity
        // this machine has.
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository named after a project, on a named branch, with one linked
/// worktree on a second named branch in a named directory.
fn repository_with_a_linked_worktree() -> PathBuf {
    let parent = temp_dir("repo");
    let root = parent.join(PROJECT_NAME);
    fs::create_dir_all(&root).expect("create the repository root");

    git(&root, &["init", "--quiet", "--initial-branch", MAIN_BRANCH]);
    fs::write(root.join("a-file"), b"contents").expect("write a file");
    git(&root, &["add", "a-file"]);
    git(&root, &["commit", "--quiet", "-m", "first"]);
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            WORKTREE_BRANCH,
            parent.join(WORKTREE_DIR).to_str().expect("utf-8 path"),
        ],
    );
    root
}

#[test]
fn the_stored_baseline_names_no_repository_branch_or_path() {
    let root = repository_with_a_linked_worktree();

    let observation = WorkspaceObservation::collect(
        1_800_000_000,
        vec![root.clone()],
        &GitCliWorktreeCensus,
        TIMEOUT,
    );

    // Anti-vacuity, before anything is asserted absent. If `git` were missing or
    // the census failed, the observation would be empty and every assertion
    // below would pass over nothing.
    assert_eq!(
        observation.repositories.len(),
        1,
        "the census did not see the fixture repository, so nothing below is \
         being tested"
    );
    let seen = &observation.repositories[0];
    assert_eq!(
        seen.worktree_count, 2,
        "the census did not see both working trees"
    );
    assert_eq!(seen.linked_worktree_count, 1);
    assert_eq!(seen.detached_worktree_count, 0);
    assert_eq!(
        seen.single_checkout_branch, None,
        "a repository with two working trees has no single checkout, so no \
         branch should have been aliased at all"
    );

    let history = temp_dir("store").join("workflow-history.json");
    let (_, stored) = record(&history, observation).expect("record the observation");
    assert_eq!(stored.len(), 1);

    let bytes = fs::read(&history).expect("read the history file back");
    assert!(
        bytes.len() > 40,
        "the history file is too small to be the record that was just written"
    );
    let text = String::from_utf8_lossy(&bytes);

    for name in [PROJECT_NAME, MAIN_BRANCH, WORKTREE_BRANCH, WORKTREE_DIR] {
        assert!(
            !text.contains(name),
            "the stored baseline holds `{name}`:\n{text}"
        );
    }
    // The path's ancestors too, which no marker above covers. A leaked
    // absolute path — a `collected_from` field, a "for troubleshooting" root
    // beside the alias — carries the account's home or the machine's temp
    // directory even when it carries no project name, and on a corporate
    // machine that prefix is itself identifying.
    //
    // Only the long components: `/var` and macOS's `/T/` would match ordinary
    // JSON by coincidence, and a leak that reached here would bring the long
    // distinctive ones with it.
    for component in root.iter().filter_map(|c| c.to_str()) {
        if component.len() < 8 {
            continue;
        }
        assert!(
            !text.contains(component),
            "the stored baseline holds the path component `{component}`:\n{text}"
        );
    }

    // And the same of what reading it back produces, so a future `read` that
    // enriched observations from the filesystem would be caught too.
    let StoreState::Collected(round_tripped) = read(&history) else {
        panic!("the store that was just written did not read back as collected");
    };
    let debugged = format!("{round_tripped:?}");
    for name in [PROJECT_NAME, MAIN_BRANCH, WORKTREE_BRANCH, WORKTREE_DIR] {
        assert!(
            !debugged.contains(name),
            "the observation read back holds `{name}`:\n{debugged}"
        );
    }
}

/// A lone checkout *does* have its branch aliased, and the alias is a number.
///
/// This is the case the assertion above cannot cover: with two working trees
/// there is no single-checkout branch to record, so `single_checkout_branch`
/// being free of the branch name proves nothing there. Here it holds something,
/// and what it holds still must not be the name.
#[test]
fn a_single_checkouts_branch_is_aliased_rather_than_named() {
    let parent = temp_dir("lone");
    let root = parent.join(PROJECT_NAME);
    fs::create_dir_all(&root).expect("create the repository root");
    git(&root, &["init", "--quiet", "--initial-branch", MAIN_BRANCH]);
    fs::write(root.join("a-file"), b"contents").expect("write a file");
    git(&root, &["add", "a-file"]);
    git(&root, &["commit", "--quiet", "-m", "first"]);

    let observation = WorkspaceObservation::collect(
        1_800_000_000,
        vec![root.clone()],
        &GitCliWorktreeCensus,
        TIMEOUT,
    );
    assert_eq!(observation.repositories.len(), 1);
    let seen = &observation.repositories[0];
    assert_eq!(seen.worktree_count, 1);
    assert!(
        seen.single_checkout_branch.is_some(),
        "a lone checkout on a named branch recorded no branch alias, so this \
         test is asserting nothing"
    );

    let history = temp_dir("lone-store").join("workflow-history.json");
    record(&history, observation).expect("record the observation");
    let bytes = fs::read(&history).expect("read back");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains(MAIN_BRANCH),
        "the branch name reached the file:\n{text}"
    );
    assert!(
        !text.contains(PROJECT_NAME),
        "the project name reached the file:\n{text}"
    );
}
