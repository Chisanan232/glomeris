//! HORO-1056: real-subprocess proof that `glomeris execute --json`
//! distinguishes the AC's named outcomes as distinct machine-readable
//! values, not a generic exit code:
//!
//! - `Protected` — already covered by `tests/execute_json_refusal.rs`.
//! - `Busy` — already covered by `tests/execution_lock_wiring.rs`'s
//!   `execute_json_renders_structured_busy_report_when_the_execution_lock_is_already_held`
//!   (HORO-1056's actual gap: previously silent under `--json`).
//! - `AskNotConfirmed` / `FingerprintMismatch` — this file's
//!   `ask_no_consent`/`ask_consent_mismatch` tests, against a real
//!   git-dirty fixture the real `DefaultEvidenceCollector` classifies
//!   `ASK` for.
//! - `RevalidationAborted` — already covered at the `executor::execute`
//!   level by `src/executor/mod.rs`'s
//!   `execute_aborts_when_resource_identity_changed_between_approval_and_execution`/
//!   `execute_aborts_when_fresh_reasons_widen_beyond_planned_ask_bucket`,
//!   and at the DTO level by `src/cli/mod.rs`'s
//!   `build_execute_report_distinguishes_every_execution_outcome` — real
//!   subprocess timing cannot reliably force this TOCTOU race, so those
//!   two levels are the faithful proof instead of a flaky subprocess test.
//! - `Succeeded` — this file's `succeeded` test, a real AutoSafe
//!   `node_modules` fixture executed for real via the CLI, same real-chain
//!   idiom as `tests/golden_chain_execute.rs` but through the actual
//!   `--json` subcommand rather than calling `executor::execute` directly.

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
        "glomeris-execute-json-outcomes-test-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

/// A real, disposable `node_modules` fixture with no active-use signals at
/// all (no git repo, no open files, no live owning tool) — classifies
/// `AUTO_SAFE` under the real `DefaultEvidenceCollector`.
fn make_autosafe_node_modules_fixture(home: &std::path::Path) -> PathBuf {
    let project_root = home.join("autosafe-project");
    let node_modules = project_root.join("node_modules");
    std::fs::create_dir_all(&node_modules).expect("create node_modules fixture dir");
    std::fs::write(node_modules.join("package.json"), "{}").expect("write fixture file");
    project_root
        .canonicalize()
        .expect("canonicalize fixture root")
}

/// A real `node_modules` fixture whose containing directory is a git
/// working tree with a genuinely dirty (modified-but-uncommitted, not
/// merely untracked) tracked file — `git.rs`'s own
/// `parse_status_porcelain` only sets `dirty` for a non-`??` status line,
/// so an untracked file alone would NOT trigger this. `classify`'s
/// `GitWorktreeDirty` active-use reason classifies this `ASK`.
fn make_ask_node_modules_fixture(home: &std::path::Path) -> PathBuf {
    let project_root = home.join("ask-project");
    std::fs::create_dir_all(&project_root).expect("create project root");

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&project_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("failed to spawn git");
        assert!(status.success(), "git {args:?} failed");
    };

    run_git(&["init", "--quiet"]);
    let tracked = project_root.join("tracked.txt");
    std::fs::write(&tracked, "a\n").expect("write tracked fixture file");
    run_git(&["add", "tracked.txt"]);
    run_git(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=test",
        "commit",
        "--quiet",
        "-m",
        "init",
    ]);
    // Modify the tracked file WITHOUT staging — `git status --porcelain`
    // now reports " M tracked.txt", a genuinely dirty (not just
    // untracked) working tree.
    std::fs::write(&tracked, "a\nb\n").expect("modify tracked fixture file");

    let node_modules = project_root.join("node_modules");
    std::fs::create_dir_all(&node_modules).expect("create node_modules fixture dir");
    std::fs::write(node_modules.join("package.json"), "{}").expect("write fixture file");

    project_root
        .canonicalize()
        .expect("canonicalize fixture root")
}

#[test]
fn execute_json_renders_succeeded_for_a_real_autosafe_execution() {
    let home = make_temp_home("succeeded");
    let project_root = make_autosafe_node_modules_fixture(&home);
    let node_modules_dir = project_root.join("node_modules");
    let resource_id = format!("node_modules:{}", node_modules_dir.display());

    let output = Command::new(glomeris_bin())
        .arg("execute")
        .arg("--action-id")
        .arg("node.clean.node_modules")
        .arg("--resource-id")
        .arg(&resource_id)
        .arg("--project-root")
        .arg(&project_root)
        .arg("--json")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert_eq!(
        output.status.code(),
        Some(0),
        "a real AutoSafe execution must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json must be valid JSON, got parse error {e}: {stdout}"));
    assert_eq!(
        parsed["outcome"], "succeeded",
        "expected the succeeded outcome, got: {parsed}"
    );
    assert!(
        !node_modules_dir.exists(),
        "the real node_modules fixture must actually be deleted"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn execute_json_renders_ask_no_consent_for_an_unconfirmed_ask_resource() {
    let home = make_temp_home("ask-no-consent");
    let project_root = make_ask_node_modules_fixture(&home);
    let resource_id = format!(
        "node_modules:{}",
        project_root.join("node_modules").display()
    );

    let output = Command::new(glomeris_bin())
        .arg("execute")
        .arg("--action-id")
        .arg("node.clean.node_modules")
        .arg("--resource-id")
        .arg(&resource_id)
        .arg("--project-root")
        .arg(&project_root)
        .arg("--json")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert_eq!(
        output.status.code(),
        Some(3),
        "ASK-with-no-consent must exit 3; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json must be valid JSON, got parse error {e}: {stdout}"));
    assert_eq!(
        parsed["reason"], "ask_no_consent",
        "expected the fixture to classify ASK and refuse for lack of consent, got: {parsed}"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn execute_json_renders_ask_consent_mismatch_for_a_stale_fingerprint() {
    let home = make_temp_home("ask-consent-mismatch");
    let project_root = make_ask_node_modules_fixture(&home);
    let resource_id = format!(
        "node_modules:{}",
        project_root.join("node_modules").display()
    );

    // `v1:00` is a validly-encoded fingerprint token (flags byte `0x00`:
    // no fields present at all) — it decodes successfully, so this
    // exercises the ASK-CONSENT-MISMATCH branch specifically, not the
    // separate malformed-token usage-error path (exit 2) `execute_json_refusal.rs`'s
    // own doc comment calls out as untested there.
    let output = Command::new(glomeris_bin())
        .arg("execute")
        .arg("--action-id")
        .arg("node.clean.node_modules")
        .arg("--resource-id")
        .arg(&resource_id)
        .arg("--project-root")
        .arg(&project_root)
        .arg("--confirm-ask")
        .arg("--observed-fingerprint")
        .arg("v1:00")
        .arg("--json")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert_eq!(
        output.status.code(),
        Some(3),
        "ASK with a mismatched fingerprint must exit 3; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json must be valid JSON, got parse error {e}: {stdout}"));
    assert_eq!(
        parsed["reason"], "ask_consent_mismatch",
        "expected a fingerprint mismatch refusal distinct from ask_no_consent, got: {parsed}"
    );

    std::fs::remove_dir_all(&home).ok();
}
