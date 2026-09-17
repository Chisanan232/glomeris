//! Regression test for a HORO-1058 adversarial-review nit on HORO-1055:
//! `glomeris execute --json` previously rendered every refusal/not-found
//! branch as a bare eprintln + exit code, so a `--json` caller (the
//! interactive UI this subcommand exists for) got empty stdout on exactly
//! the cases it most needs structured detail on. Fixed by rendering an
//! `ExecuteRefusalReport` to stdout when `--json` is passed, for every
//! non-`Executed` branch.
//!
//! This test locks the PROTECTED case specifically, since it is the most
//! safety-critical refusal path in the whole Epic.

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
        "glomeris-execute-json-refusal-test-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

/// A real `node_modules` directory discoverable by the Node detector,
/// placed under a path containing a `.ssh` component so it also matches
/// `policy::protected`'s credential-material pattern regardless of its
/// resource kind — the same fixture shape HORO-1008's golden test and
/// HORO-1058's adversarial review both used.
fn make_protected_node_modules_fixture(home: &std::path::Path) -> PathBuf {
    let project_root = home.join(".ssh").join("myproject");
    let node_modules = project_root.join("node_modules");
    std::fs::create_dir_all(&node_modules).expect("create node_modules fixture dir");
    std::fs::write(node_modules.join("package.json"), "{}").expect("write fixture file");
    // The Node detector canonicalizes the path before storing it
    // (`src/detectors/node.rs`) — symlink-resolve here too (macOS's
    // `$TMPDIR` is itself a symlink, e.g. `/var/...` -> `/private/var/...`),
    // or the `--resource-id` built below would never match the candidate
    // `find_candidate` actually sees.
    project_root
        .canonicalize()
        .expect("canonicalize fixture root")
}

#[test]
fn execute_json_renders_structured_refusal_for_protected_resource() {
    let home = make_temp_home("protected");
    let project_root = make_protected_node_modules_fixture(&home);
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
        // No --confirm-ask/--observed-fingerprint: PROTECTED must refuse
        // unconditionally regardless of consent, and a malformed token
        // would short-circuit to the usage-error exit (2) in `main.rs`
        // before `resolve_and_execute` even runs — that's a different
        // (real, but untested-here) code path, not this one.
        .arg("--json")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert_eq!(
        output.status.code(),
        Some(3),
        "PROTECTED refusal must exit 3; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("--json must be valid JSON on a refusal path, got parse error {e}; stdout: {stdout}")
    });
    assert_eq!(
        parsed["reason"], "protected",
        "expected the protected refusal reason, got: {parsed}"
    );
    assert!(
        parsed["message"]
            .as_str()
            .unwrap_or_default()
            .contains("PROTECTED"),
        "refusal message should explain why, got: {parsed}"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn execute_without_json_still_renders_plain_text_on_refusal() {
    let home = make_temp_home("plain-text");
    let project_root = make_protected_node_modules_fixture(&home);
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
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");

    assert_eq!(output.status.code(), Some(3));
    assert!(
        output.stdout.is_empty(),
        "non-JSON mode must not print JSON to stdout on refusal"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("PROTECTED"),
        "expected the existing human-readable refusal message on stderr, got: {stderr}"
    );

    std::fs::remove_dir_all(&home).ok();
}
