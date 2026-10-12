//! HORO-1629 round-2 regression: `glomeris check-project-root ~/code` —
//! this command's own documented example (`src/cli/help.rs`) — must
//! resolve the literal `~/code` against `$HOME` before probing, not treat
//! it as a (nonexistent) literal path named `~`.
//!
//! Spawns the real `glomeris` binary directly via `Command` (never a
//! shell), with `$HOME` pointed at a disposable temp directory — the same
//! `glomeris_bin()`/temp-`$HOME` idiom as `tests/history_cli.rs` — so the
//! tilde arrives exactly as literal as it does from `GlomerisMenuBar`'s
//! `GlomerisClient`, which spawns via `Process.executableURL` +
//! `arguments` and never a shell interpreter either. Before this fix, a
//! shell-less caller handing this command `~/code` always got
//! `"status":"undetermined"` back, no matter what was actually on disk
//! under the real home directory, because `std::fs` performs no
//! shell-style tilde expansion.

use std::fs;
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
        "glomeris-check-project-root-tilde-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

fn run_check_project_root(home: &std::path::Path, path_arg: &str) -> std::process::Output {
    Command::new(glomeris_bin())
        .arg("check-project-root")
        .arg(path_arg)
        .arg("--json")
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// The exact ticket scenario, reached the exact way a no-shell caller
/// reaches it: `~/code` holds one real project one level down, and the
/// literal argument the binary receives is `~/code`, never shell-expanded
/// by anything in this process chain.
#[test]
fn literal_tilde_slash_argument_resolves_against_home_and_is_flagged_as_parent_of_projects() {
    let home = make_temp_home("parent-of-projects");
    let code_dir = home.join("code");
    let repo_a = code_dir.join("repo-a");
    fs::create_dir_all(repo_a.join("target")).expect("create fixture project");
    fs::write(repo_a.join("Cargo.toml"), "[package]\n").expect("write fixture manifest");

    let output = run_check_project_root(&home, "~/code");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");

    assert_eq!(
        json.get("status").and_then(|v| v.as_str()),
        Some("parent_of_projects"),
        "a literal, unexpanded `~/code` must resolve to the real home-relative directory \
         and be flagged, not report undetermined for a nonexistent literal path: {stdout}"
    );
    assert_eq!(
        json.get("path").and_then(|v| v.as_str()),
        Some(code_dir.to_string_lossy().as_ref()),
        "the reported path should be the EXPANDED path, not the literal tilde: {stdout}"
    );

    fs::remove_dir_all(&home).ok();
}

/// Without this fix, the same scenario reported `undetermined` (the
/// correct fail-closed answer for a literal path named `~` that cannot
/// exist) with no warning — this test documents that the fix changed the
/// outcome, not just added expansion in the abstract.
#[test]
fn bare_tilde_argument_resolves_to_the_home_directory_itself() {
    let home = make_temp_home("bare-tilde");
    fs::write(home.join("Cargo.toml"), "[package]\n").expect("write fixture manifest at home");

    let output = run_check_project_root(&home, "~");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");

    assert_eq!(
        json.get("status").and_then(|v| v.as_str()),
        Some("project_root"),
        "a bare `~` must resolve to the home directory itself: {stdout}"
    );
    assert_eq!(
        json.get("path").and_then(|v| v.as_str()),
        Some(home.to_string_lossy().as_ref())
    );

    fs::remove_dir_all(&home).ok();
}
