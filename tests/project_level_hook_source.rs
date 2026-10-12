//! HORO-1865 real-subprocess proof: a project-level `.claude/settings.json`
//! or `.claude/settings.local.json` hook that references a Cargo target
//! dir / `node_modules` directory makes that resource PROTECTED, closing
//! the exact gap HORO-1829's QA pass found (a Cargo target dir referenced
//! only by a project-level hook got AUTO_SAFE and was deleted with zero
//! consent).
//!
//! Every test here spawns the real compiled `glomeris` binary against a
//! disposable, isolated fixture `$HOME` and a disposable fixture project
//! root passed via `--project-root` — never the developer's real `$HOME`
//! or real project settings (ADR-0001 §4.3's own privacy invariant).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn unique_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-project-hook-{prefix}-{}-{}-{n}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn run_glomeris(args: &[&str], home: &Path) -> Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

fn parse_json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "--json must be valid JSON, got parse error {e}; stdout: {stdout}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// A real, compiled Cargo binary at `<root>/target/debug/fixture-hook` —
/// the exact HORO-1829 QA repro shape, built for real rather than faked as
/// a plain file, so this proves the real dev/ino identity path too.
/// `<root>`'s own `.cargo/config.toml` pins `target-dir` to `<root>/target`
/// explicitly, overriding this machine's global shared `target-dir`
/// setting (`~/.cargo/config.toml`, see `~/.claude/CLAUDE.md`) — without
/// it the binary would land in the shared dir, not inside the resource
/// this test means to protect.
fn build_fixture_hook(root: &Path) -> PathBuf {
    fs::create_dir_all(root).expect("create project root");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture-hook\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::create_dir_all(root.join("src")).expect("create src dir");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write main.rs");
    fs::create_dir_all(root.join(".cargo")).expect("create .cargo dir");
    fs::write(
        root.join(".cargo/config.toml"),
        format!(
            "[build]\ntarget-dir = \"{}\"\n",
            root.join("target").display()
        ),
    )
    .expect("write .cargo/config.toml");

    // `CARGO_TARGET_DIR`, if inherited from this test binary's own
    // environment (e.g. a developer or CI invocation that sets it for
    // the outer `cargo test` run), takes priority over both the local
    // `.cargo/config.toml` above AND the machine's global shared
    // `target-dir` setting — so it is pinned explicitly here too, to
    // the same local directory, rather than left to whatever the outer
    // process happened to be built with.
    let status = Command::new("cargo")
        .arg("build")
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to spawn cargo build for the fixture-hook crate");
    assert!(status.success(), "cargo build for fixture-hook failed");

    let hook_bin = root.join("target").join("debug").join("fixture-hook");
    assert!(
        hook_bin.exists(),
        "expected a real built fixture-hook binary at {}",
        hook_bin.display()
    );
    hook_bin
}

/// Writes `<root>/.claude/<file_name>` with a single `PostToolUse` hook
/// whose command is the given absolute path.
fn write_project_hook(root: &Path, file_name: &str, hook_command: &Path) {
    fs::create_dir_all(root.join(".claude")).expect("create .claude dir");
    fs::write(
        root.join(".claude").join(file_name),
        format!(
            r#"{{"hooks":{{"PostToolUse":[{{"hooks":[{{"command":"{}"}}]}}]}}}}"#,
            hook_command.display()
        ),
    )
    .expect("write project hook settings file");
}

fn canonical(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|e| panic!("failed to canonicalize {}: {e}", path.display()))
        .to_string_lossy()
        .into_owned()
}

// ---- A1: the exact QA repro — settings.json, Cargo target dir ----

#[test]
fn a1_project_settings_json_hook_protects_a_real_cargo_target_dir() {
    let home = unique_dir("a1-home");
    let project_root = unique_dir("a1-project");

    let hook_bin = build_fixture_hook(&project_root);
    write_project_hook(&project_root, "settings.json", &hook_bin);

    let target_dir = canonical(&project_root.join("target"));

    let explain = run_glomeris(
        &[
            "explain",
            &target_dir,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert!(
        explain.status.success(),
        "explain --json should succeed; stderr: {}",
        String::from_utf8_lossy(&explain.stderr)
    );
    let report = parse_json(&explain);
    assert!(
        report["reasons"]
            .as_array()
            .expect("reasons must be an array")
            .iter()
            .any(|r| r == "protected_executable_dependency"),
        "expected protected_executable_dependency among reasons, got: {report}"
    );
    assert!(
        report["executable_dependency"]["references"]
            .as_array()
            .expect("references must be an array")
            .iter()
            .any(|r| r["source"] == "claude_project"),
        "expected a claude_project reference, got: {report}"
    );

    let resource_id = format!("cargo_target_dir:{target_dir}");
    let execute = run_glomeris(
        &[
            "execute",
            "--action-id",
            "cargo.clean.target_dir",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    // No --confirm-ask/--observed-fingerprint: PROTECTED must refuse
    // unconditionally. This also proves PROTECTED takes precedence even
    // when the ownership probe (`cargo metadata`, run against the real
    // cargo installation under this isolated $HOME) might independently
    // fail or disagree — step 2b runs before the completeness/ownership
    // gate (ADR-0001 §5).
    assert_eq!(
        execute.status.code(),
        Some(3),
        "PROTECTED refusal must exit 3; stderr: {}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let refusal = parse_json(&execute);
    assert_eq!(
        refusal["reason"], "protected",
        "expected the exact 'protected' refusal reason, got: {refusal}"
    );

    assert!(
        project_root.join("target").exists(),
        "the target dir must never be deleted"
    );
    assert!(hook_bin.exists(), "the hook binary must never be deleted");

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}

// ---- A2: settings.local.json, node_modules ----

#[test]
fn a2_project_settings_local_json_hook_protects_a_node_modules_dir() {
    let home = unique_dir("a2-home");
    let project_root = unique_dir("a2-project");
    let node_modules = project_root.join("node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    fs::write(node_modules.join("package.json"), "{}").expect("write package.json");

    let hook_bin = node_modules.join(".bin").join("x");
    fs::create_dir_all(hook_bin.parent().unwrap()).expect("create node_modules/.bin");
    fs::write(&hook_bin, b"#!/bin/sh\n").expect("write fixture hook");

    write_project_hook(&project_root, "settings.local.json", &hook_bin);

    let node_modules_query = canonical(&node_modules);

    let explain = run_glomeris(
        &[
            "explain",
            &node_modules_query,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert!(
        explain.status.success(),
        "explain --json should succeed; stderr: {}",
        String::from_utf8_lossy(&explain.stderr)
    );
    let report = parse_json(&explain);
    assert!(
        report["reasons"]
            .as_array()
            .expect("reasons must be an array")
            .iter()
            .any(|r| r == "protected_executable_dependency"),
        "expected protected_executable_dependency among reasons, got: {report}"
    );
    assert!(
        report["executable_dependency"]["references"]
            .as_array()
            .expect("references must be an array")
            .iter()
            .any(|r| r["source"] == "claude_project"),
        "expected a claude_project reference, got: {report}"
    );

    let resource_id = format!("node_modules:{node_modules_query}");
    let execute = run_glomeris(
        &[
            "execute",
            "--action-id",
            "node.clean.node_modules",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert_eq!(
        execute.status.code(),
        Some(3),
        "PROTECTED refusal must exit 3; stderr: {}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let refusal = parse_json(&execute);
    assert_eq!(
        refusal["reason"], "protected",
        "expected the exact 'protected' refusal reason, got: {refusal}"
    );

    assert!(node_modules.exists(), "node_modules must never be deleted");
    assert!(hook_bin.exists(), "the hook binary must never be deleted");

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}

// ---- A3: negative controls — must NOT over-trigger ----

/// A project `settings.json` with only `permissions`/`env` config (no
/// `hooks` key at all) must never make an unrelated `node_modules`
/// resource PROTECTED — it must stay AUTO_SAFE and actually get deleted.
#[test]
fn a3_project_settings_with_no_hooks_key_stays_auto_safe_and_deletable() {
    let home = unique_dir("a3-home");
    let project_root = unique_dir("a3-project-no-hooks");
    let node_modules = project_root.join("node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    fs::write(node_modules.join("package.json"), "{}").expect("write package.json");

    fs::create_dir_all(project_root.join(".claude")).expect("create .claude dir");
    fs::write(
        project_root.join(".claude").join("settings.json"),
        r#"{"permissions":{"allow":["Bash(ls:*)"]},"env":{"FOO":"bar"}}"#,
    )
    .expect("write settings.json with no hooks key");

    let node_modules_query = canonical(&node_modules);

    // Proves this is a genuine negative, not the HORO-1865 bug reappearing
    // (the project settings file silently never read at all): the probe
    // must have actually examined it.
    let explain = run_glomeris(
        &[
            "explain",
            &node_modules_query,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert!(
        explain.status.success(),
        "explain --json should succeed; stderr: {}",
        String::from_utf8_lossy(&explain.stderr)
    );
    let explain_report = parse_json(&explain);
    assert!(
        explain_report["executable_dependency"]["sources_examined"]
            .as_array()
            .expect("sources_examined must be an array")
            .iter()
            .any(|s| s == "claude_project"),
        "the project settings file must have actually been examined (and found clean), \
         not silently skipped — got: {explain_report}"
    );

    let resource_id = format!("node_modules:{node_modules_query}");

    let execute = run_glomeris(
        &[
            "execute",
            "--action-id",
            "node.clean.node_modules",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert_eq!(
        execute.status.code(),
        Some(0),
        "an unrelated node_modules must execute AUTO_SAFE successfully; stderr: {}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let result = parse_json(&execute);
    assert_eq!(
        result["outcome"], "succeeded",
        "expected the succeeded outcome, got: {result}"
    );
    assert!(
        !node_modules.exists(),
        "the real node_modules fixture must actually be deleted"
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}

/// A project hook whose command is an absolute path pointing OUTSIDE the
/// resource entirely must also leave that resource AUTO_SAFE/deletable —
/// proving this isn't "any project with a settings.json goes ASK", only
/// ones with a reference actually inside the resource.
#[test]
fn a3_project_hook_pointing_outside_the_resource_stays_auto_safe_and_deletable() {
    let home = unique_dir("a3-home");
    let project_root = unique_dir("a3-project-outside-hook");
    let node_modules = project_root.join("node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    fs::write(node_modules.join("package.json"), "{}").expect("write package.json");

    // A real path, but well outside node_modules — never on a chain into
    // the resource.
    let outside_hook = project_root.join("tools").join("unrelated-hook.sh");
    fs::create_dir_all(outside_hook.parent().unwrap()).expect("create tools dir");
    fs::write(&outside_hook, b"#!/bin/sh\n").expect("write unrelated hook");

    write_project_hook(&project_root, "settings.json", &outside_hook);

    let node_modules_query = canonical(&node_modules);

    // Proves the hook was actually read and resolved (not silently
    // skipped) — the reference it produces must land outside the
    // resource, never inside it.
    let explain = run_glomeris(
        &[
            "explain",
            &node_modules_query,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert!(
        explain.status.success(),
        "explain --json should succeed; stderr: {}",
        String::from_utf8_lossy(&explain.stderr)
    );
    let explain_report = parse_json(&explain);
    assert!(
        explain_report["executable_dependency"]["sources_examined"]
            .as_array()
            .expect("sources_examined must be an array")
            .iter()
            .any(|s| s == "claude_project"),
        "the project settings file must have actually been examined (and found clean), \
         not silently skipped — got: {explain_report}"
    );
    assert!(
        explain_report["executable_dependency"]["references"]
            .as_array()
            .expect("references must be an array")
            .is_empty(),
        "a hook pointing outside the resource must produce no reference INTO it, got: {explain_report}"
    );

    let resource_id = format!("node_modules:{node_modules_query}");

    let execute = run_glomeris(
        &[
            "execute",
            "--action-id",
            "node.clean.node_modules",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert_eq!(
        execute.status.code(),
        Some(0),
        "a hook pointing outside the resource must not block AUTO_SAFE execution; stderr: {}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let result = parse_json(&execute);
    assert_eq!(
        result["outcome"], "succeeded",
        "expected the succeeded outcome, got: {result}"
    );
    assert!(
        !node_modules.exists(),
        "the real node_modules fixture must actually be deleted"
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}

// ---- A4: the recovery-loop (`free`) call site ----

/// A real `glomeris free` run, pointed at the A1 fixture (a project hook
/// protecting a real Cargo target dir). `--target 100%` is never trivially
/// met, so the loop actually runs a discovery/classify pass instead of
/// short-circuiting at the top-of-iteration goal check (`--target 0%`
/// WOULD short-circuit there — see `target_met`, which treats 0% as always
/// satisfied — so this test deliberately avoids that shape). With no
/// AutoSafe candidate available (the only candidate is PROTECTED) and
/// `auto_approve_ask: false`, the run must stop `safe_exhausted` having
/// executed nothing, never touching the protected target dir.
///
/// Safe to run on any host, unlike the plausible-goal caution
/// `tests/pressure_to_recovery_chain.rs`'s module doc comment states (a
/// low/realistic goal would delete real host state because discovery
/// is not confined to `$HOME`): `ActionRegistry::builtin()` registers
/// exactly three actions — `CargoCleanTargetDir`, `NodeCleanNodeModules`,
/// `HomebrewCleanupCache` — and no Docker or Xcode action at all, so a
/// non-fixture Docker/Xcode resource this run might also discover on the
/// real host can never actually be executed against (nothing to run).
/// `HomebrewCleanupCache` itself plans an unscoped `RunTool` step, which
/// `executor::structural_refusal` refuses on sight regardless of policy
/// class (see `tests/actionability_agrees_with_execution.rs`'s own
/// documentation of exactly this). That leaves Cargo/`node_modules` as
/// the only kinds this run can ever mutate, and discovery for both is
/// confined to `--project-root`-scoped directories under an isolated,
/// empty fixture `$HOME` — so the only such resource this run can find
/// at all is this test's own fixture.
#[test]
fn a4_free_recovery_loop_never_deletes_a_project_hook_protected_target_dir() {
    let home = unique_dir("a4-home");
    let project_root = unique_dir("a4-project");

    let hook_bin = build_fixture_hook(&project_root);
    write_project_hook(&project_root, "settings.json", &hook_bin);

    let run = run_glomeris(
        &[
            "free",
            "--target",
            "100%",
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    assert_eq!(
        run.status.code(),
        Some(0),
        "a direct (non-autopilot) free run must exit 0; stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report = parse_json(&run);
    assert_eq!(
        report["stop_reason"], "safe_exhausted",
        "expected nothing safe left to act on (the only candidate is PROTECTED), got: {report}"
    );
    assert_eq!(
        report["actions_executed"], 0,
        "the protected target dir must never actually be executed against, got: {report}"
    );

    assert!(
        project_root.join("target").exists(),
        "the target dir must never be deleted by the recovery loop"
    );
    assert!(hook_bin.exists(), "the hook binary must never be deleted");

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}
