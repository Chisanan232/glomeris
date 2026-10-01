//! `glomeris --version` must not silently claim to be the exact tagged
//! release when the build can tell it isn't (HORO-1612/HORO-1609).
//!
//! CI and local development always build from a commit that is not exactly
//! `v<CARGO_PKG_VERSION>` with a clean tree (a PR branch, a dev checkout with
//! local edits, etc.), so every real test run here exercises the "dev build"
//! branch of `version_line()` in `src/main.rs` -- the one regression this
//! guards against is a future change that drops the distinction and goes
//! back to printing the bare version unconditionally.

use std::process::{Command, Stdio};

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn stdout_of(args: &[&str]) -> String {
    let output = Command::new(glomeris_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");
    assert!(
        output.status.success(),
        "`glomeris {}` must exit 0",
        args.join(" ")
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn version_flag_marks_a_non_release_build_as_dev() {
    let stdout = stdout_of(&["--version"]);
    let expected_prefix = format!("glomeris {}", env!("CARGO_PKG_VERSION"));
    assert!(
        stdout.starts_with(&expected_prefix),
        "expected version output to start with {expected_prefix:?}, got {stdout:?}"
    );
    assert!(
        stdout.contains("(dev build, rev "),
        "a build not exactly at its release tag must not print the bare \
         version string unconditionally: {stdout:?}"
    );
}

#[test]
fn bare_invocation_prints_the_same_version_line_as_the_version_flag() {
    let bare = stdout_of(&[]);
    let versioned = stdout_of(&["--version"]);
    let first_line_of_bare = bare.lines().next().unwrap_or_default();
    assert_eq!(first_line_of_bare, versioned.trim_end());
}
