//! CLI integration proof for HORO-1050: `--help`/`-h`/`help` and
//! `print_usage()` (shown on an unknown command) render from the same
//! `COMMANDS` table in `src/main.rs`, so they cannot independently drift
//! the way `print_usage()` once omitted `llm-plan` (HORO-1034). Also
//! proves `glomeris <sub> --help` looks itself up in that same table
//! instead of being rejected as an unrecognized argument.
//!
//! Spawns the real `glomeris` binary — same `glomeris_bin()` idiom as
//! `tests/history_cli.rs` — rather than testing `main.rs`'s internals
//! directly (there is no library-exposed entry point to unit test here).

use std::process::{Command, Stdio};

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

/// Every currently-registered top-level subcommand (mirrors the `COMMANDS`
/// table in `src/main.rs`) — including `llm-plan`, the exact subcommand
/// HORO-1034 found missing from `print_usage()`, and the three most
/// recently added (`history`, `actions`, `execute`).
const ALL_SUBCOMMANDS: &[&str] = &[
    "daemon",
    "actions",
    "scan",
    "status",
    "detect",
    "explain",
    "clean",
    "llm-plan",
    "execute",
    "emergency",
    "history",
    "free",
];

fn run(args: &[&str]) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

#[test]
fn unknown_command_usage_lists_every_subcommand() {
    let output = run(&["badcmd"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    for cmd in ALL_SUBCOMMANDS {
        assert!(
            stderr.contains(cmd),
            "unknown-command usage is missing subcommand '{cmd}': {stderr}"
        );
    }
}

#[test]
fn help_flag_lists_every_subcommand() {
    let output = run(&["--help"]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for cmd in ALL_SUBCOMMANDS {
        assert!(
            stdout.contains(cmd),
            "--help output is missing subcommand '{cmd}': {stdout}"
        );
    }
}

#[test]
fn help_and_unknown_command_usage_render_the_same_aggregate_line() {
    // The actual proof that they cannot drift by construction: the
    // usage portion of --help's output and the usage portion of the
    // unknown-command error must be byte-identical, since both come
    // from `build_aggregate_usage()` reading the same `COMMANDS` table.
    let help_output = run(&["--help"]);
    let badcmd_output = run(&["badcmd"]);

    let help_stdout = String::from_utf8_lossy(&help_output.stdout);
    let badcmd_stderr = String::from_utf8_lossy(&badcmd_output.stderr);

    // badcmd's stderr has a leading "glomeris: unknown command..." line
    // before the usage block; strip it so we compare only the shared
    // usage text.
    let usage_from_badcmd = badcmd_stderr.lines().skip(1).collect::<Vec<_>>().join("\n");

    assert_eq!(
        help_stdout.trim_end(),
        usage_from_badcmd.trim_end(),
        "usage rendered by --help and by the unknown-command path must be identical"
    );
}

#[test]
fn llm_plan_help_prints_its_own_usage_instead_of_erroring() {
    let output = run(&["llm-plan", "--help"]);

    assert!(
        output.status.success(),
        "glomeris llm-plan --help must succeed, not be rejected as an unrecognized argument; \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("llm-plan"),
        "expected llm-plan-specific usage, got: {stdout}"
    );
    assert!(
        !stdout.contains("unrecognized argument"),
        "must not fall through to the generic unrecognized-argument error, got: {stdout}"
    );
}

#[test]
fn every_subcommand_supports_its_own_help_flag() {
    for cmd in ALL_SUBCOMMANDS {
        let output = run(&[cmd, "--help"]);
        assert!(
            output.status.success(),
            "'glomeris {cmd} --help' must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(cmd),
            "'glomeris {cmd} --help' output should mention '{cmd}': {stdout}"
        );
    }
}
