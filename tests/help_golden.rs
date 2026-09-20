//! Golden snapshots of every help surface (HORO-1311 AC 8).
//!
//! # What this catches that `tests/shared_command_table.rs` does not
//!
//! That file asserts properties: every subcommand is listed, every one has a
//! usage line, exit-code guidance is a topic. Properties cannot notice a
//! sentence quietly becoming wrong, a safety caveat dropping out of a
//! paragraph, or an example losing the flag that made it work — and help text
//! is a place where those are the likely regressions, because nothing breaks
//! when they happen. So the full text of every surface is checked in, and any
//! change to any character of it has to be an intentional edit to a file in
//! `tests/fixtures/help/`.
//!
//! # Updating a snapshot
//!
//! Run the suite with `GLOMERIS_UPDATE_HELP_GOLDEN=1` to rewrite the fixtures
//! from the current binary, then read the resulting diff before committing it.
//! Reading the diff is the point: the mechanism exists to make a help change
//! visible in review, not to make it effortless.
//!
//! Deliberately gated on an environment variable rather than a `cargo` feature
//! or a separate binary, so that it cannot be switched on inside CI by a
//! config change that looks unrelated to help text. The guard below fails the
//! suite if that variable is set in a CI environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use glomeris::cli::help;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/help")
}

fn updating() -> bool {
    std::env::var_os("GLOMERIS_UPDATE_HELP_GOLDEN").is_some()
}

fn stdout_of(args: &[&str]) -> String {
    let output = Command::new(glomeris_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");
    assert!(
        output.status.success(),
        "`glomeris {}` must succeed; stderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Compares one surface against its fixture, or rewrites it when updating.
///
/// The version string is substituted out. Otherwise every release would
/// invalidate the top-level snapshot, and a fixture that has to be regenerated
/// for an unrelated reason is a fixture nobody reads the diff of.
fn assert_matches_golden(name: &str, args: &[&str]) {
    let actual = stdout_of(args).replace(env!("CARGO_PKG_VERSION"), "{VERSION}");
    let path = fixture_dir().join(format!("{name}.txt"));

    if updating() {
        std::fs::create_dir_all(fixture_dir()).expect("failed to create fixture directory");
        std::fs::write(&path, &actual).unwrap_or_else(|e| panic!("failed writing {path:?}: {e}"));
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing help snapshot {path:?} ({e}). If `glomeris {}` is a new surface, run the \
             suite with GLOMERIS_UPDATE_HELP_GOLDEN=1 and review the generated file.",
            args.join(" ")
        )
    });

    assert_eq!(
        actual,
        expected,
        "\n`glomeris {}` no longer matches its snapshot.\n\nIf the new text is what you \
         intended, re-run with GLOMERIS_UPDATE_HELP_GOLDEN=1 and review the diff.\n",
        args.join(" ")
    );
}

#[test]
fn top_level_help_matches_its_snapshot() {
    assert_matches_golden("top-level", &["--help"]);
}

#[test]
fn the_exit_codes_topic_matches_its_snapshot() {
    assert_matches_golden("topic-exit-codes", &["help", "exit-codes"]);
}

#[test]
fn every_subcommand_help_matches_its_snapshot() {
    for command in help::COMMANDS {
        assert_matches_golden(
            &format!("command-{}", command.name),
            &[command.name, "--help"],
        );
    }
}

/// A fixture for a command that no longer exists is a snapshot nothing tests,
/// which is how a golden-file suite rots into decoration. Checked in the same
/// direction as the drift it replaced: the files on disk must match the table,
/// not merely be a superset of it.
#[test]
fn no_snapshot_exists_for_a_command_that_is_gone() {
    let mut expected: Vec<String> = vec!["top-level.txt".into(), "topic-exit-codes.txt".into()];
    for command in help::COMMANDS {
        expected.push(format!("command-{}.txt", command.name));
    }
    expected.sort();

    let mut found: Vec<String> = std::fs::read_dir(fixture_dir())
        .expect("help fixture directory must exist")
        .map(|entry| entry.expect("unreadable directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".txt"))
        .collect();
    found.sort();

    assert_eq!(
        found, expected,
        "the help snapshots on disk do not match the command table"
    );
}

/// The escape hatch must not be usable by accident in CI, where nobody would
/// see the rewritten fixtures and the suite would report green on help text no
/// human approved.
#[test]
fn the_update_escape_hatch_is_not_enabled_in_ci() {
    if std::env::var_os("CI").is_some() {
        assert!(
            !updating(),
            "GLOMERIS_UPDATE_HELP_GOLDEN is set in CI, which would rewrite the help snapshots \
             instead of checking them"
        );
    }
}
