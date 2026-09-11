//! CLI integration test (HORO-955): `glomeris clean` must refuse to run —
//! with a non-zero exit code and no destructive side effect — unless
//! `--dry-run` is explicitly passed. This is the confirmation-gating
//! proof the ticket's AC calls for: a piped/non-interactive invocation
//! (e.g. `echo | glomeris clean`, or `glomeris clean` from a script with
//! no terminal at all) must never be able to trigger destructive
//! automation just by omitting a flag.

use std::process::{Command, Stdio};

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

#[test]
fn clean_without_dry_run_flag_exits_nonzero_and_prints_a_clear_message() {
    let output = Command::new(glomeris_bin())
        .arg("clean")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert!(
        !output.status.success(),
        "`glomeris clean` without --dry-run must not exit successfully"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--dry-run is required"),
        "expected a clear message explaining --dry-run is required, got: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "no report should be printed when the confirmation gate refuses to run"
    );
}

#[test]
fn clean_without_dry_run_flag_and_no_stdin_at_all_still_refuses() {
    // Simulates a fully non-interactive/piped invocation with nothing on
    // stdin — the exact shape the AC's "prevent accidental destructive
    // automation" is guarding against.
    let output = Command::new(glomeris_bin())
        .arg("clean")
        .arg("--target")
        .arg("cargo_target_dir:/tmp/does-not-matter")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--dry-run is required"));
}

#[test]
fn clean_with_dry_run_flag_exits_successfully() {
    let output = Command::new(glomeris_bin())
        .arg("clean")
        .arg("--dry-run")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert!(
        output.status.success(),
        "`glomeris clean --dry-run` should succeed even with no candidates; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("DRY RUN"),
        "dry-run output must be visibly marked as a dry run, got: {stdout}"
    );
}
