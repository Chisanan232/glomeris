//! Throwaway on-disk fixtures shared by this module's tests.
//!
//! Real `git` and real files, because what several of these tests check is how
//! git answers and whether a file survives a round trip — a fake would only
//! restate the assumption under test.
//!
//! Every path lives under [`std::env::temp_dir`] and is named after the calling
//! test, the process id and a counter, so two tests running concurrently in the
//! same binary cannot collide and a leftover directory names what left it.
//!
//! Nothing here touches `$HOME`. The store's real location is
//! `$HOME/Library/Application Support/Glomeris/`, and a test that wrote there
//! would overwrite the baseline of whoever is running the suite — including the
//! founder's dogfood state.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temporary path. Not created — callers decide whether it should be
/// a directory, a file, or absent.
pub fn temp_path(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("glomeris-h1547-{name}-{}-{n}", std::process::id()))
}

/// A repository with one commit on a branch called `trunk`.
///
/// `--initial-branch` explicitly, so the fixture does not depend on the
/// machine's `init.defaultBranch`; the identity and signing flags are passed
/// per-invocation so the fixture works on a machine with no git identity
/// configured and does not try to sign.
pub fn scratch_repo(name: &str) -> PathBuf {
    let root = temp_path(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create scratch repo");
    git(&root, &["init", "--initial-branch=trunk"]);
    std::fs::write(root.join("f"), b"x").expect("write file");
    git(&root, &["add", "f"]);
    git(
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

pub fn git(root: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        status.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}
