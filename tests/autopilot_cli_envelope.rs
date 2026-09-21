//! `glomeris autopilot` at the process boundary (HORO-1310).
//!
//! The unit tests in `src/autopilot/` prove what the envelope, the gate and
//! the run do. This file proves the CLI that grants and revokes them behaves
//! the way the help text promises: reading is read-only, a limit above its
//! ceiling is refused rather than clamped, and a revoked envelope executes
//! nothing.
//!
//! # How these tests avoid deleting anything
//!
//! Two independent protections, because one of them is the behaviour under
//! test and so cannot be relied on to hold:
//!
//! 1. Every invocation runs with `HOME` pointed at a fresh temp directory, so
//!    the envelope file, the execution lock and the audit log are all this
//!    test's own disposable state and never the real ones.
//! 2. Every invocation that *could* reach the deleting path runs with the
//!    shared execution lock already held by this process — the same technique
//!    `tests/emergency_takes_no_arguments.rs` uses, and for the same reason.
//!    `autopilot run` takes that lock before it discovers anything, so a run
//!    that got past the enable check dies with 75 having touched nothing.
//!
//! `glomeris autopilot run --dry-run` is deliberately never spawned here.
//! Not because it is dangerous — it cannot delete — but because it runs the
//! real detectors, several of which shell out to installed tooling regardless
//! of the `DiscoveryContext` handed to them (see
//! `tests/execution_lock_wiring.rs`). Dry-run behaviour is covered by
//! `src/autopilot/run.rs`'s own tests against disposable fixtures.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn make_temp_home(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-autopilot-cli-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

fn envelope_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("autopilot.conf")
}

/// Runs `glomeris autopilot <args>` against `home`. Never spawns anything
/// that can reach the deleting path — callers that need that use
/// [`autopilot_with_lock_held`].
fn autopilot(home: &Path, args: &[&str]) -> Output {
    Command::new(glomeris_bin())
        .arg("autopilot")
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// Runs `glomeris autopilot <args>` with the execution lock for `home`
/// already held by this process, so the invocation cannot reach discovery,
/// an action or a deletion whatever the code under test decides.
fn autopilot_with_lock_held(home: &Path, args: &[&str]) -> Output {
    let lock_path = home
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("execution.lock");
    std::fs::create_dir_all(lock_path.parent().expect("lock path has a parent"))
        .expect("create lock parent dir");

    let held = glomeris::executor::lock::acquire_execution_lock_at(&lock_path)
        .expect("test process must be able to take the lock first");

    let output = autopilot(home, args);

    drop(held);
    output
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A bare `glomeris autopilot` is `show`: it reports, and it does not even
/// create the file it read. AC 6 — what Autopilot may do is readable before
/// anything is granted.
#[test]
fn a_bare_invocation_reports_a_revoked_envelope_and_writes_nothing() {
    let home = make_temp_home("show");

    let output = autopilot(&home, &[]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("status:            revoked"),
        "an absent envelope must read as revoked, got: {stdout}"
    );
    assert!(
        stdout.contains("allowed kinds:     none — nothing can run"),
        "got: {stdout}"
    );
    assert!(
        stdout.contains("stored at:"),
        "show must say where the envelope lives, got: {stdout}"
    );
    assert!(
        !envelope_path(&home).exists(),
        "reading the envelope must not create one"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn enable_without_kinds_is_a_usage_error_and_grants_nothing() {
    let home = make_temp_home("enable-no-kinds");

    let output = autopilot(&home, &["enable", "--max-actions", "1"]);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("--kinds"),
        "the refusal must name the missing flag, got: {}",
        stderr_of(&output)
    );
    assert!(
        !envelope_path(&home).exists(),
        "a refused enable must not write an envelope"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn an_unknown_resource_kind_is_refused_and_the_known_ones_are_listed() {
    let home = make_temp_home("bad-kind");

    let output = autopilot(&home, &["enable", "--kinds", "everything"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("'everything' is not a resource kind"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("node_modules"),
        "a rejection should show what is valid, got: {stderr}"
    );
    assert!(!envelope_path(&home).exists());

    std::fs::remove_dir_all(&home).ok();
}

/// A limit above its hard ceiling is refused outright rather than silently
/// clamped. Clamping would be the worse behaviour by far: the user would be
/// told 500 and be living under 25, with no way to notice.
#[test]
fn a_limit_above_its_ceiling_is_refused_not_clamped() {
    let home = make_temp_home("ceiling");

    let output = autopilot(
        &home,
        &["enable", "--kinds", "node_modules", "--max-actions", "500"],
    );

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("--max-actions"),
        "got: {}",
        stderr_of(&output)
    );
    assert!(
        !envelope_path(&home).exists(),
        "a refused enable must not leave a partially applied envelope behind"
    );

    std::fs::remove_dir_all(&home).ok();
}

/// A PROTECTED reason is not pre-authorizable by any envelope, so the flag
/// that narrowly authorizes an ASK reason cannot be pointed at one.
#[test]
fn a_protected_reason_cannot_be_preauthorized() {
    let home = make_temp_home("preauth-protected");

    let output = autopilot(
        &home,
        &[
            "enable",
            "--kinds",
            "node_modules",
            "--preauthorize-ask",
            "node_modules:protected_git_internals",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("--preauthorize-ask"),
        "got: {}",
        stderr_of(&output)
    );
    assert!(!envelope_path(&home).exists());

    std::fs::remove_dir_all(&home).ok();
}

/// AC 6 and AC 7 end to end: a grant is written, is readable afterwards in
/// the same words, and one `revoke` takes it away.
#[test]
fn a_grant_is_written_readable_and_revocable() {
    let home = make_temp_home("round-trip");

    let enabled = autopilot(
        &home,
        &[
            "enable",
            "--kinds",
            "node_modules,cargo_target_dir",
            "--max-actions",
            "2",
            "--max-bytes",
            "1073741824",
            "--max-duration",
            "30",
            "--min-pressure",
            "pressured",
        ],
    );
    assert_eq!(
        enabled.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&enabled)
    );
    let enabled_stdout = stdout_of(&enabled);
    assert!(
        enabled_stdout.contains("Autopilot is now ENABLED"),
        "got: {enabled_stdout}"
    );
    assert!(
        enabled_stdout.contains("node_modules") && enabled_stdout.contains("cargo_target_dir"),
        "enable must echo the grant it just wrote, got: {enabled_stdout}"
    );
    assert!(
        envelope_path(&home).exists(),
        "enable must persist the grant"
    );

    // The same grant, read back by a separate process — which is the only
    // way to know the file, not the in-memory value, carried it.
    let shown = stdout_of(&autopilot(&home, &[]));
    assert!(shown.contains("status:            ENABLED"), "got: {shown}");
    assert!(shown.contains("max actions:       2"), "got: {shown}");
    assert!(shown.contains("max duration:      30s"), "got: {shown}");
    assert!(
        shown.contains("runs only at PRESSURED or worse"),
        "got: {shown}"
    );

    let revoked = autopilot(&home, &["revoke"]);
    assert_eq!(revoked.status.code(), Some(0));
    assert!(
        stdout_of(&revoked).contains("Autopilot revoked"),
        "got: {}",
        stdout_of(&revoked)
    );

    let after = stdout_of(&autopilot(&home, &[]));
    assert!(after.contains("status:            revoked"), "got: {after}");
    // Revocation keeps the limits so a later enable cannot return with
    // limits the user never read.
    assert!(after.contains("max actions:       2"), "got: {after}");

    std::fs::remove_dir_all(&home).ok();
}

/// AC 4. A `run` with no grant attempts nothing and says so, and it reaches
/// that answer *before* taking the execution lock — which is how this test
/// can tell "refused because revoked" apart from "blocked by the lock".
#[test]
fn a_run_with_no_grant_attempts_nothing() {
    let home = make_temp_home("run-revoked");

    let output = autopilot_with_lock_held(&home, &["run"]);

    assert_eq!(
        output.status.code(),
        Some(3),
        "a revoked run must exit 3, not 75 (which would mean it got as far as \
         the lock) and not 0; stderr: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Autopilot is not enabled. Nothing was attempted."),
        "got: {stdout}"
    );
    assert!(
        stdout.contains("status:            revoked"),
        "a run must open with the authority it acted under, got: {stdout}"
    );

    std::fs::remove_dir_all(&home).ok();
}

/// The deleting path takes the shared execution lock before it discovers
/// anything, so an enabled real run cannot proceed concurrently with `free`,
/// `execute` or `emergency`.
///
/// This is also the only test here that lets an *enabled* envelope reach
/// `run` without `--dry-run`. It is safe by construction and not by
/// assumption: this process holds the lock, and the exit code asserted below
/// is the proof that the run stopped there. An exit of 0 would mean the lock
/// was not taken — a regression this assertion exists to catch.
#[test]
fn an_enabled_run_takes_the_execution_lock_before_it_discovers_anything() {
    let home = make_temp_home("run-locked");

    let enabled = autopilot(
        &home,
        &[
            "enable",
            "--kinds",
            "node_modules",
            "--max-actions",
            "1",
            "--max-bytes",
            "1048576",
        ],
    );
    assert_eq!(
        enabled.status.code(),
        Some(0),
        "stderr: {}",
        stderr_of(&enabled)
    );

    let output = autopilot_with_lock_held(&home, &["run"]);

    assert_eq!(
        output.status.code(),
        Some(75),
        "an enabled run must be stopped by the held execution lock. Any other \
         code means it proceeded past the lock into real discovery; stdout: {} stderr: {}",
        stdout_of(&output),
        stderr_of(&output)
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    let home = make_temp_home("bad-sub");

    let output = autopilot(&home, &["disable"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("unknown subcommand 'disable'"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("usage: glomeris autopilot"),
        "a usage error must print this command's usage, got: {stderr}"
    );

    std::fs::remove_dir_all(&home).ok();
}
