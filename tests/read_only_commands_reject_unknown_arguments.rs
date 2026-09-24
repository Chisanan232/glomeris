//! The read-only commands refuse an argument they cannot account for,
//! rather than running as if it had not been typed (HORO-1322).
//!
//! # The defect
//!
//! `split_flags` pushed every token that was not a known flag into the
//! *positionals* vector, and four of its five callers discarded positionals
//! entirely. So `glomeris detect --jsonn` ran every detector, printed prose
//! and exited 0 — the reader had asked for JSON, been given something else,
//! and been told nothing. `glomeris explain --resource-id <id>` was worse
//! than silent: it took `--resource-id` as the resource to explain and
//! answered "no discoverable candidate matches '--resource-id'", reporting a
//! syntax mistake as a fact about the machine. `scan` had its own version,
//! reading `--json` as the directory to walk and reporting a successful scan
//! of nothing.
//!
//! Meanwhile `clean`, `emergency`, `execute`, `free`, `autopilot`, `history`,
//! `llm-plan`, `llm-check`, `daemon install` and `actions history` all
//! rejected an unknown argument with exit 2. Strictness was split by
//! accident, along no line a user could predict.
//!
//! # Why this file spawns the real binary
//!
//! What is under test is what the process does with an argv, which is
//! `main.rs`'s parsing wired to `main.rs`'s exit codes — the same reason
//! `tests/emergency_takes_no_arguments.rs` and `tests/shared_command_table.rs`
//! spawn it. Every command exercised here is read-only and refuses before any
//! detector, tool or filesystem walk is reached, so unlike `emergency` no
//! precaution is needed to make a *regressed* guard safe: the worst a
//! regression does is print a report.
//!
//! Each refusal is paired with a positive control on the same command, so a
//! test that passes because the command is broken in some other way fails
//! here too.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// Asserts the shape every usage error in this CLI has: exit 2, the offending
/// token named, this command's own usage, and nothing on stdout — a refused
/// invocation ran nothing, so it has nothing to report.
///
/// `usage_command` is the first word of the command, because the help table is
/// keyed by subcommand: `actions list`'s usage is `actions`'.
fn assert_usage_error(args: &[&str], offending: &str, usage_command: &str) {
    let output = run(args);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let invocation = args.join(" ");

    assert_eq!(
        output.status.code(),
        Some(2),
        "`glomeris {invocation}` must be a usage error; stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!("unrecognized argument '{offending}'")),
        "`glomeris {invocation}` must name the argument it did not understand, got: {stderr}"
    );
    assert!(
        stderr.contains(&format!("usage: glomeris {usage_command}")),
        "`glomeris {invocation}` must print this command's usage, got: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "`glomeris {invocation}` printed a report despite refusing; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Asserts a command still works when asked correctly. Every refusal below is
/// paired with one of these, so "refuses everything" cannot pass for a fix.
fn assert_accepted(args: &[&str]) {
    let output = run(args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "`glomeris {}` must still be accepted; stderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "`glomeris {}` produced no output",
        args.join(" ")
    );
}

/// An unknown flag, on each command that used to swallow one. `--jsonn` and
/// `--dry-run` specifically: a near-miss of a real flag, and a flag whose
/// meaning on another command is "do not actually do it".
#[test]
fn an_unknown_flag_is_a_usage_error_on_every_read_only_command() {
    for (args, offending, usage_command) in [
        (vec!["status", "--bogus"], "--bogus", "status"),
        (vec!["detect", "--jsonn"], "--jsonn", "detect"),
        (vec!["detect", "--dry-run"], "--dry-run", "detect"),
        (vec!["explain", "--bogus"], "--bogus", "explain"),
        (vec!["actions", "list", "--bogus"], "--bogus", "actions"),
        (vec!["daemon", "status", "--bogus"], "--bogus", "daemon"),
        (vec!["scan", "--json"], "--json", "scan"),
    ] {
        assert_usage_error(&args, offending, usage_command);
    }
}

/// A bare token, on the four commands that take no positional argument at
/// all. These were discarded by the call site rather than by `split_flags`,
/// so they are a separate path to the same silence.
#[test]
fn a_stray_positional_is_a_usage_error_where_none_is_accepted() {
    for (args, usage_command) in [
        (vec!["status", "somepath"], "status"),
        (vec!["detect", "somepath"], "detect"),
        (vec!["actions", "list", "somepath"], "actions"),
        (vec!["daemon", "status", "somepath"], "daemon"),
    ] {
        assert_usage_error(&args, "somepath", usage_command);
    }
}

/// The commands must still accept what they document. `detect` is represented
/// by `--project-root`, which reaches `extract_project_roots` before
/// `split_flags` ever sees the remainder — proving the new rejection sits
/// downstream of the valued flag rather than eating it.
#[test]
fn the_documented_arguments_are_all_still_accepted() {
    assert_accepted(&["status"]);
    assert_accepted(&["status", "--json"]);
    assert_accepted(&["actions", "list"]);
    assert_accepted(&["actions", "list", "--json"]);
    assert_accepted(&["daemon", "status"]);
    assert_accepted(&["daemon", "status", "--json"]);

    let empty_root = make_temp_dir("h1322-project-root");
    assert_accepted(&[
        "detect",
        "--json",
        "--project-root",
        empty_root.to_str().expect("temp path is utf-8"),
    ]);
    std::fs::remove_dir_all(&empty_root).ok();
}

/// The ticket's own repro, and the reason it is worth more than exit 2:
/// `--resource-id` is reached for because `resource_id` is what the JSON
/// output calls the thing, and the old reply sent the reader looking for a
/// resource rather than at their own syntax.
#[test]
fn explain_says_the_resource_id_is_positional_rather_than_searching_for_a_flag() {
    let output = run(&["explain", "--resource-id", "cargo_target_dir:/tmp/x/target"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("unrecognized argument '--resource-id'"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("positional argument, not a flag"),
        "the refusal must say how to name the resource instead, got: {stderr}"
    );
    // The old behaviour, which must not be what a reader sees for this input.
    assert!(
        !stderr.contains("no discoverable candidate matches"),
        "a syntax mistake is still being reported as a missing resource: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit 1 here would still mean 'that resource does not exist'"
    );
}

/// A second resource id was silently dropped: `explain a b` explained `a` and
/// never mentioned `b`.
#[test]
fn explain_refuses_a_second_resource_rather_than_explaining_only_the_first() {
    let output = run(&["explain", "definitely-not-a-resource", "nor-is-this"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("unrecognized argument 'nor-is-this'"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("exactly one resource id or path"),
        "got: {stderr}"
    );
    // Proof that the refusal came from argument parsing and not from the
    // lookup: the first token is not a real resource either, so a run that
    // reached the lookup would have exited 1 complaining about *that* one.
    assert!(
        !stderr.contains("no discoverable candidate matches"),
        "the second argument was accepted and the first was then looked up: {stderr}"
    );
}

/// `scan`'s own two silent fallbacks, beyond the flag covered above. Both
/// positionals were read by index and defaulted on failure.
#[test]
fn scan_refuses_an_unparseable_count_and_a_third_argument() {
    let root = make_temp_dir("h1322-scan");
    let path = root.to_str().expect("temp path is utf-8").to_string();
    std::fs::write(root.join("f.bin"), vec![0u8; 4096]).expect("write scan fixture");

    let output = run(&["scan", &path, "abc"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("top_k must be a non-negative integer, got 'abc'"),
        "a count that is not a count must be said so, not silently replaced \
         with the default; got: {stderr}"
    );
    assert!(output.stdout.is_empty(), "it scanned anyway");

    assert_usage_error(&["scan", &path, "3", "extra"], "extra", "scan");

    // Positive control on the same fixture: the documented form still scans
    // it, so the two refusals above are about the arguments and not about a
    // scanner that has stopped working.
    let output = run(&["scan", &path, "3"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("f.bin"), "got: {stdout}");

    std::fs::remove_dir_all(&root).ok();
}

/// Reading about a command is never itself a usage error — the guard must sit
/// behind the help lookup, not in front of it.
#[test]
fn help_still_reaches_the_help_renderer_on_every_command_made_strict() {
    for command in ["status", "detect", "explain", "scan", "actions", "daemon"] {
        for flag in ["--help", "-h"] {
            let output = run(&[command, flag]);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "`glomeris {command} {flag}` must succeed; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                stdout.starts_with(&format!("glomeris {command} — ")),
                "expected this command's help, got: {stdout}"
            );
        }
    }
}

fn make_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}
