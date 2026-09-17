//! CLI integration proof for HORO-1054: the advisory execution lock is
//! actually wired around `glomeris free --target`'s real-execution call
//! site, without changing its uncontended behavior.
//!
//! `--target 0%` is used throughout because [`glomeris::executor::recovery_loop::target_met`]
//! is trivially true for a 0% free-space target on any real filesystem,
//! so the loop returns `StopReason::TargetReached` at step 2 — BEFORE any
//! detector discovery or real destructive execution ever runs (see
//! `recovery_loop::run`'s step-by-step doc comment). That makes this the
//! one `free` invocation shape that is completely safe to spawn as a real
//! subprocess in CI while still exercising the exact call site the lock
//! wraps (`free_run` acquires the lock before calling `run_recovery_loop`
//! and holds it for the whole call, including this early return).
//!
//! `emergency`'s equivalent real-subprocess proof is not included here:
//! several of its built-in detectors (e.g. the Homebrew detector) shell
//! out to real, already-installed system tools regardless of the
//! `DiscoveryContext` passed in, so a real `glomeris emergency` subprocess
//! could mutate real state on whatever machine runs this test suite — see
//! `recovery_loop.rs`'s own `FakeDetector` doc comment for the same
//! concern. `emergency` goes through the exact same
//! `acquire_execution_lock_or_exit`/`glomeris::executor::lock` primitive
//! this test exercises via `free`, so this test's coverage of the shared
//! lock module (plus `src/executor/lock.rs`'s own unit tests) is the
//! practical substitute.

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
        "glomeris-execution-lock-test-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

#[test]
fn free_with_target_already_met_behaves_identically_when_uncontended() {
    let home = make_temp_home("uncontended");

    let output = Command::new(glomeris_bin())
        .arg("free")
        .arg("--target")
        .arg("0%")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert!(
        output.status.success(),
        "uncontended `glomeris free --target 0%` must still exit successfully; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stop reason:            TargetReached"),
        "expected the same TargetReached report as before HORO-1054, got: {stdout}"
    );
    assert!(
        stdout.contains("iterations run:         0"),
        "expected zero iterations — the lock must not change the recovery loop's own logic, \
         got: {stdout}"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn free_exits_via_the_busy_path_when_the_execution_lock_is_already_held() {
    let home = make_temp_home("contended");
    let lock_path = home
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("execution.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).expect("create lock parent dir");

    // Hold the exact lock `free_run` acquires (same path convention:
    // `$HOME/Library/Application Support/Glomeris/execution.lock`) from
    // THIS test process — a real `flock(2)` held via a real, separate
    // open file description, faithfully modeling a second concurrent
    // `glomeris` invocation. See `src/executor/lock.rs`'s module doc for
    // why this is a faithful (not faked) contention proof.
    let held = glomeris::executor::lock::acquire_execution_lock_at(&lock_path)
        .expect("test process must be able to take the lock first");

    let output = Command::new(glomeris_bin())
        .arg("free")
        .arg("--target")
        .arg("0%")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    drop(held);

    assert!(
        !output.status.success(),
        "a `glomeris free` invocation racing a held execution lock must not exit successfully"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "expected the dedicated execution-lock-busy exit code, got status: {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("execution lock busy") || stderr.contains("already in progress"),
        "expected a clear busy-lock message, got: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a busy-lock refusal must never print a recovery report — nothing ran"
    );

    std::fs::remove_dir_all(&home).ok();
}

/// HORO-1055: `glomeris execute` acquires the SAME shared execution lock
/// as `free`/`emergency` — proven here by holding the lock externally and
/// confirming `execute` exits via the busy path before ever reaching
/// discovery/resolution. The `--action-id`/`--resource-id` values are
/// deliberately nonsense: if the lock did not engage first, this
/// invocation would instead fail with the "resource not found" exit code
/// (5), never 75 — so observing 75 here is proof the lock check runs
/// before candidate resolution, exactly mirroring `free`'s own wiring.
#[test]
fn execute_exits_via_the_busy_path_when_the_execution_lock_is_already_held() {
    let home = make_temp_home("execute-contended");
    let lock_path = home
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("execution.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).expect("create lock parent dir");

    // Same real `flock(2)` contention proof as the `free` test above.
    let held = glomeris::executor::lock::acquire_execution_lock_at(&lock_path)
        .expect("test process must be able to take the lock first");

    let output = Command::new(glomeris_bin())
        .arg("execute")
        .arg("--action-id")
        .arg("does.not.matter")
        .arg("--resource-id")
        .arg("does-not-matter")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    drop(held);

    assert!(
        !output.status.success(),
        "a `glomeris execute` invocation racing a held execution lock must not exit successfully"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "expected the dedicated execution-lock-busy exit code, got status: {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("execution lock busy") || stderr.contains("already in progress"),
        "expected a clear busy-lock message, got: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a busy-lock refusal must never print an execute report — nothing ran"
    );

    std::fs::remove_dir_all(&home).ok();
}
