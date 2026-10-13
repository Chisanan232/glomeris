//! HORO-1868, real-subprocess proof: the exact HORO-1829 N9(c) finding —
//! a Claude Code hook referencing a bare command name that is guaranteed
//! absent from PATH — classifies `PROTECTED` and is unconditionally
//! refused, never a consentable `ASK`. Modeled on
//! `tests/execute_json_outcomes.rs`'s real-subprocess pattern (`HOME` set
//! to a temp dir, a real `node.clean.node_modules` fixture, the actual
//! `glomeris` binary spawned end to end — no direct call into
//! `policy::classify`).
//!
//! This file also carries the source-level control case, to prove the fix
//! is scoped correctly: a genuinely unparseable `settings.json`
//! (malformed JSON/schema, never examined at the reference level) must
//! stay the consentable `Ask ExecutableDependencyUnknown` and must still
//! permit a real consented deletion — if that regresses, the HORO-1868 fix
//! has over-widened to cover source-read failures too.

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
        "glomeris-horo1868-unresolved-hook-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

/// A real, disposable `node_modules` fixture with no active-use signals —
/// would classify `AUTO_SAFE` under the real `DefaultEvidenceCollector` if
/// the executable-dependency probe found nothing.
fn make_node_modules_fixture(home: &std::path::Path) -> PathBuf {
    let project_root = home.join("project");
    let node_modules = project_root.join("node_modules");
    std::fs::create_dir_all(&node_modules).expect("create node_modules fixture dir");
    std::fs::write(node_modules.join("package.json"), "{}").expect("write fixture file");
    project_root
        .canonicalize()
        .expect("canonicalize fixture root")
}

/// Writes `~/.claude/settings.json` with a hook referencing a bare
/// command name guaranteed to be absent from PATH.
fn write_bare_absent_hook_settings(home: &std::path::Path) -> String {
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("create .claude dir");
    let absent_name = format!(
        "glomeris-horo1868-absent-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos()
    );
    std::fs::write(
        claude_dir.join("settings.json"),
        format!(r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{absent_name}"}}]}}]}}}}"#),
    )
    .expect("write settings.json");
    absent_name
}

/// Writes `~/.claude/settings.json` with genuinely unparseable bytes —
/// the real "unparseable config" shape (`UnresolvedRef::SourceUnreadable`,
/// never examined at the reference level), NOT a bare-name hook.
fn write_malformed_settings(home: &std::path::Path) {
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("create .claude dir");
    std::fs::write(
        claude_dir.join("settings.json"),
        b"{ this is not valid json at all [[[",
    )
    .expect("write malformed settings.json");
}

fn run_glomeris(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// N9(c) regression: a bare, guaranteed-absent hook command name classifies
/// `PROTECTED` — never the previous consentable `ASK`.
#[test]
fn bare_absent_hook_command_is_protected_not_consentable_ask() {
    let home = make_temp_home("n9c-protected");
    write_bare_absent_hook_settings(&home);
    let project_root = make_node_modules_fixture(&home);
    let node_modules_dir = project_root.join("node_modules");
    let resource_id = format!("node_modules:{}", node_modules_dir.display());

    let explain_output = run_glomeris(
        &[
            "explain",
            &node_modules_dir.to_string_lossy(),
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert!(
        explain_output.status.success(),
        "explain --json should succeed; stderr: {}",
        String::from_utf8_lossy(&explain_output.stderr)
    );
    let explain_stdout = String::from_utf8_lossy(&explain_output.stdout);
    let explain: serde_json::Value = serde_json::from_str(&explain_stdout)
        .unwrap_or_else(|e| panic!("explain --json must be valid JSON, got {e}: {explain_stdout}"));

    assert_eq!(
        explain["policy_label"], "PROTECTED",
        "expected PROTECTED, got: {explain}"
    );
    assert_eq!(
        explain["reasons"],
        serde_json::json!(["protected_unresolved_executable_dependency"]),
        "expected the new reason code, got: {explain}"
    );
    let unresolved = explain["executable_dependency"]["unresolved"]
        .as_array()
        .expect("unresolved must be an array");
    assert!(
        unresolved.iter().any(|u| u["kind"] == "not_on_path"),
        "expected a not_on_path unresolved entry, got: {explain}"
    );
    let fingerprint_token = explain["fingerprint_token"]
        .as_str()
        .expect("fingerprint_token must be present (non-null) regardless of class");
    assert!(!fingerprint_token.is_empty());

    // Attempt a consented execute anyway — must be refused outright.
    let execute_output = run_glomeris(
        &[
            "execute",
            "--action-id",
            "node.clean.node_modules",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project_root.to_string_lossy(),
            "--confirm-ask",
            "--observed-fingerprint",
            fingerprint_token,
            "--json",
        ],
        &home,
    );

    assert_eq!(
        execute_output.status.code(),
        Some(3),
        "PROTECTED must exit 3; stderr: {}",
        String::from_utf8_lossy(&execute_output.stderr)
    );
    let execute_stdout = String::from_utf8_lossy(&execute_output.stdout);
    let execute: serde_json::Value = serde_json::from_str(&execute_stdout)
        .unwrap_or_else(|e| panic!("execute --json must be valid JSON, got {e}: {execute_stdout}"));
    assert_eq!(
        execute["reason"], "protected",
        "expected the protected refusal reason, got: {execute}"
    );
    assert!(
        node_modules_dir.exists(),
        "node_modules must NOT be deleted when PROTECTED"
    );

    std::fs::remove_dir_all(&home).ok();
}

/// Source-level control: a genuinely unparseable `settings.json` (never
/// examined at the reference level) stays the consentable `Ask
/// ExecutableDependencyUnknown`, and a real consented execution actually
/// deletes the fixture. If this test fails, the HORO-1868 fix has been
/// over-scoped to cover source-read failures too, not just examined
/// references.
#[test]
fn malformed_settings_json_stays_consentable_ask_and_deletes() {
    let home = make_temp_home("source-level-control");
    write_malformed_settings(&home);
    let project_root = make_node_modules_fixture(&home);
    let node_modules_dir = project_root.join("node_modules");
    let resource_id = format!("node_modules:{}", node_modules_dir.display());

    let explain_output = run_glomeris(
        &[
            "explain",
            &node_modules_dir.to_string_lossy(),
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert!(
        explain_output.status.success(),
        "explain --json should succeed; stderr: {}",
        String::from_utf8_lossy(&explain_output.stderr)
    );
    let explain_stdout = String::from_utf8_lossy(&explain_output.stdout);
    let explain: serde_json::Value = serde_json::from_str(&explain_stdout)
        .unwrap_or_else(|e| panic!("explain --json must be valid JSON, got {e}: {explain_stdout}"));

    assert_eq!(
        explain["policy_label"], "UNKNOWN_INCOMPLETE",
        "expected UNKNOWN_INCOMPLETE, got: {explain}"
    );
    assert_eq!(
        explain["reasons"],
        serde_json::json!(["executable_dependency_unknown"]),
        "expected the source-read-failure reason, got: {explain}"
    );
    let fingerprint_token = explain["fingerprint_token"]
        .as_str()
        .expect("fingerprint_token must be present")
        .to_string();

    let execute_output = run_glomeris(
        &[
            "execute",
            "--action-id",
            "node.clean.node_modules",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project_root.to_string_lossy(),
            "--confirm-ask",
            "--observed-fingerprint",
            &fingerprint_token,
            "--json",
        ],
        &home,
    );

    assert_eq!(
        execute_output.status.code(),
        Some(0),
        "a real consented ASK deletion must succeed; stderr: {}",
        String::from_utf8_lossy(&execute_output.stderr)
    );
    let execute_stdout = String::from_utf8_lossy(&execute_output.stdout);
    let execute: serde_json::Value = serde_json::from_str(&execute_stdout)
        .unwrap_or_else(|e| panic!("execute --json must be valid JSON, got {e}: {execute_stdout}"));
    assert_eq!(
        execute["outcome"], "succeeded",
        "expected the succeeded outcome, got: {execute}"
    );
    assert!(
        !node_modules_dir.exists(),
        "the real node_modules fixture must actually be deleted"
    );

    std::fs::remove_dir_all(&home).ok();
}
