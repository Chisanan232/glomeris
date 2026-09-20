//! `glomeris emergency` takes no arguments, and must say so rather than
//! silently discarding them (HORO-1311).
//!
//! # The defect
//!
//! Until HORO-1311, `main`'s dispatch arm called `run_emergency_command()` with
//! no argument slice at all, so every argument was dropped before the function
//! was entered. `glomeris emergency --dry-run` therefore performed a real,
//! unannounced recovery run: the flag a user is most likely to reach for on
//! this command is the one meaning "don't actually do it", and it was the one
//! argument the command was guaranteed to ignore. Every other subcommand
//! rejects an unrecognized argument with exit 2.
//!
//! # Why this test cannot simply run it and check the exit code
//!
//! Because the failure mode *is* execution. A test that spawns
//! `glomeris emergency --bogus` and asserts exit 2 performs a real recovery run
//! on the machine running the suite the day the guard regresses — which is
//! precisely what happened while this ticket was being written, when a
//! dispatch-coverage probe in `tests/shared_command_table.rs` passed a bogus
//! flag to every subcommand in the table.
//!
//! So the invocation below is made safe *independently of the behaviour under
//! test*: this process first takes the shared execution lock, exactly as
//! `tests/execution_lock_wiring.rs` does. The argument guard runs before the
//! lock is acquired, so a working guard still answers 2 — and a regressed guard
//! falls through to `acquire_execution_lock_or_exit` and dies with 75 before
//! any detector, action or deletion is reached. Neither outcome can touch the
//! filesystem, and the two are distinguishable, which is all the test needs.
//!
//! See `tests/execution_lock_wiring.rs`'s module doc for why a plain
//! `glomeris emergency` subprocess is never safe to spawn: several built-in
//! detectors shell out to real installed tooling regardless of the
//! `DiscoveryContext` handed to them.

use std::path::PathBuf;
use std::process::{Command, Stdio};
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
        "glomeris-emergency-args-test-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

/// Runs `glomeris emergency <args>` with the execution lock already held by
/// this process, so that the invocation cannot reach real work regardless of
/// what the argument guard does.
fn emergency_with_lock_held(args: &[&str]) -> std::process::Output {
    let home = make_temp_home("guard");
    let lock_path = home
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("execution.lock");
    std::fs::create_dir_all(lock_path.parent().expect("lock path has a parent"))
        .expect("create lock parent dir");

    let held = glomeris::executor::lock::acquire_execution_lock_at(&lock_path)
        .expect("test process must be able to take the lock first");

    let output = Command::new(glomeris_bin())
        .arg("emergency")
        .args(args)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    drop(held);
    std::fs::remove_dir_all(&home).ok();
    output
}

#[test]
fn an_argument_to_emergency_is_a_usage_error_not_a_silent_recovery_run() {
    // `--dry-run` specifically: the argument whose silent acceptance was the
    // actual defect, and the one whose meaning is inverted by being ignored.
    for arg in ["--dry-run", "--not-a-real-flag", "somepath"] {
        let output = emergency_with_lock_held(&[arg]);

        assert_eq!(
            output.status.code(),
            Some(2),
            "`glomeris emergency {arg}` must be a usage error. An exit of 75 means the argument \
             guard has regressed and only the held execution lock stopped a real recovery run; \
             stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("unrecognized argument '{arg}'")),
            "the refusal must name the argument it did not understand, got: {stderr}"
        );
        assert!(
            stderr.contains("usage: glomeris emergency"),
            "a usage error must print this command's usage, got: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "a refused invocation must print no recovery report — nothing ran; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// `--help` must reach the help renderer rather than the argument guard, so
/// that reading about the command is never itself a usage error. Safe to run
/// without the lock: help never dispatches.
#[test]
fn emergency_help_is_help_and_not_a_usage_error() {
    for flag in ["--help", "-h"] {
        let output = Command::new(glomeris_bin())
            .arg("emergency")
            .arg(flag)
            .stdin(Stdio::null())
            .output()
            .expect("failed to spawn glomeris binary");

        assert!(
            output.status.success(),
            "`glomeris emergency {flag}` must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with("glomeris emergency — "),
            "expected this command's help, got: {stdout}"
        );
    }
}
