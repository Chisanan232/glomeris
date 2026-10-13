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

// ---- M4: TOCTOU — hook added between plan-time discovery and the
// `execute()` revalidation collector's re-probe ----

/// Independent adversarial review of PR #170 found mutations M4/M5
/// (mutating the `execute`-revalidation and Autopilot call sites' real
/// `project_roots` to `&[]`) were not caught by any committed test, and
/// proved the gap with an ad-hoc test of their own. This commits that
/// proof as regression coverage for M4 (the `execute`-revalidation call
/// site). M5 — the Autopilot call site at `src/main.rs`'s `autopilot_run`
/// (`let collector = live_evidence_collector(&project_roots);`, around
/// line 2316) — is NOT exercised by this test or by
/// `m4b_execute_consented_ask_deletion_requires_the_revalidation_collector_to_see_real_project_roots`
/// below; it remains an open gap. Unlike `execute`, Autopilot's
/// `run_autopilot` re-classifies each candidate using the SAME collector
/// instance it later revalidates with (`src/autopilot/run.rs`'s
/// `collector.collect()` at its per-candidate loop, then `execute()`'s
/// own internal revalidation reuses that same `collector` reference) —
/// so an M5 mutation would make BOTH reads blind in the same way,
/// needing no TOCTOU window at all: a hook present from the very start
/// (a static fixture, no race) would already be invisible to Autopilot's
/// own authorizing classification, and a cheap `glomeris autopilot run`
/// subprocess test could catch it directly. Not added here to keep this
/// commit scoped to M4.
///
/// This is a library-level test, not a subprocess one like A1-A4 above:
/// it needs to control the exact ordering of discovery vs. mutation vs.
/// revalidation, which this specific `executor::execute` TOCTOU window
/// is too narrow to hit reliably from outside a single process (a real
/// subprocess race would make the test non-deterministic). It drives the
/// `executor::execute` function directly — the same one
/// `cli::resolve_and_execute` calls at the end of `run_execute_command`
/// (`src/main.rs`) — with the project hook written in between two
/// otherwise-identical collector constructions:
///
/// 1. `discover_and_classify` against a project root with NO `.claude`
///    hook yet — this is "plan time", and must decide `AutoSafe`.
/// 2. `authorize()` the `AutoSafe` decision — `AutoSafe` always
///    authorizes, no consent needed, exactly like a real `execute` call
///    with no `--confirm-ask`.
/// 3. Only now, write the project's `.claude/settings.json` hook
///    referencing the target dir — the TOCTOU window: a config edit (or
///    attacker-controlled edit) landing after the decision was made but
///    before execution.
/// 4. Call `executor::execute()` — the exact function
///    `cli::resolve_and_execute` calls — with a collector scoped to the
///    REAL project roots, matching the call site at `src/main.rs`'s
///    `run_execute_command` (`let collector =
///    live_evidence_collector(&project_roots);`, which is
///    `DefaultEvidenceCollector::live(project_roots)` — this test builds
///    the same probe composition, but against a fixture `$HOME` instead
///    of `::live`'s real-process-env one, since this is an in-process
///    test rather than a subprocess). This must see the now-present
///    hook, reclassify as `Protected`, and abort with
///    `AbortedByRevalidation(PolicyClassDowngraded)` — never mutating
///    anything.
///
/// Manually verified mutation (not committed): repeating step 4 with
/// `fixture_scoped_collector(&home, &[])` instead — the shape M4/M5
/// mutate `src/main.rs`'s call sites to — reproduces the reviewer's
/// finding exactly: `report.outcome` becomes `Succeeded` and
/// `target_dir.exists()` becomes `false` (the fixture Cargo target dir
/// is actually deleted). That is exactly the bug this whole ticket
/// exists to prevent, so this test's `AbortedByRevalidation` assertion
/// below is what that mutation flips to `Succeeded` plus a deleted
/// target.
///
/// This test alone cannot catch the real `src/main.rs:1432` mutation —
/// by construction, not by oversight: it links against the `glomeris`
/// *library* crate and never calls into the `glomeris` *binary*'s
/// `run_execute_command`, so it stays GREEN no matter what that line
/// says. Only a real subprocess invocation can reach that specific call
/// site. See
/// `m4b_execute_consented_ask_deletion_requires_the_revalidation_collector_to_see_real_project_roots`
/// below for the subprocess-level test that does — though its fixture
/// happens to land on the SAFE side of the mutation's effect (the
/// mutated collector disagrees with the plan and `execute()` fails
/// closed with an abort), not the dangerous `Succeeded`-plus-deletion
/// side this test's own in-test mutation reproduces above. Catching the
/// real call site AND reproducing the dangerous symptom end-to-end would
/// need the same race this test avoids; the two tests together cover the
/// mechanism (this one) and the call site (M4b) separately instead.
#[test]
fn m4_execute_revalidation_aborts_on_a_hook_added_after_plan_time_toctou() {
    use glomeris::actions::ActionRegistry;
    use glomeris::cli::{discover_and_classify, find_candidate, resolve_action_for};
    use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
    use glomeris::evidence::correlate::{
        DefaultEvidenceCollector, GitCliProbe, HostDependencyRoots, LiveHostDependencyProbe,
        LsofOpenFileProbe, LsofProcessCwdProbe, SystemToolLivenessProbe,
    };
    use glomeris::executor::{execute, AbortReason, ExecutionOutcome};
    use glomeris::policy::approval::authorize;
    use glomeris::policy::{PolicyClass, PolicyConfig};
    use std::time::SystemTime;

    // A collector scoped to the fixture `$HOME`, never the real
    // developer `$HOME` — `DefaultEvidenceCollector::live` reads the
    // real process `$HOME` via `HostDependencyRoots::from_env`, which
    // this in-process (non-subprocess) test must not touch, unlike the
    // subprocess tests above which isolate `$HOME` via `Command::env`.
    fn fixture_scoped_collector(
        home: &Path,
        project_roots: &[PathBuf],
    ) -> DefaultEvidenceCollector {
        let roots = HostDependencyRoots {
            home: home.to_path_buf(),
            path_dirs: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            plutil_bin: PathBuf::from("/usr/bin/plutil"),
            lsof_bin: PathBuf::from("lsof"),
            managed_settings: None,
            project_roots: project_roots.to_vec(),
        };
        DefaultEvidenceCollector::new(
            Box::new(LsofOpenFileProbe),
            Box::new(LsofProcessCwdProbe),
            Box::new(GitCliProbe),
            Box::new(SystemToolLivenessProbe),
            Some(Box::new(LiveHostDependencyProbe::new(roots))),
        )
    }

    let home = unique_dir("m4-home");
    let project_root = unique_dir("m4-project");

    // Step 0: a real fixture-hook binary exists on disk from the start
    // (it has to, to be referenced later), but NO `.claude/settings.json`
    // references it yet — discovery below must see a clean project.
    let hook_bin = build_fixture_hook(&project_root);
    let target_dir = project_root.join("target");
    let project_roots = vec![project_root.clone()];

    let registry = DetectorRegistry::builtin();
    let ctx = DiscoveryContext::from_process_env(home.clone())
        .with_known_project_roots(project_roots.clone());
    let discovery_collector = fixture_scoped_collector(&home, &project_roots);
    let cfg = PolicyConfig::default();
    let now = SystemTime::now();

    let candidates = discover_and_classify(&registry, &ctx, &discovery_collector, &cfg, now);

    let target_dir_query = canonical(&target_dir);
    let Some((ev, decision)) = find_candidate(&target_dir_query, &candidates) else {
        panic!(
            "expected the fixture target dir to be a discoverable candidate, \
             got candidates: {:?}",
            candidates
                .iter()
                .map(|(e, _)| e.resource.to_string())
                .collect::<Vec<_>>()
        );
    };

    // Step 1: plan-time decision must be AutoSafe — no hook exists yet.
    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "discovery must decide AutoSafe before the project hook is written, got: {decision:?}"
    );

    let actions = ActionRegistry::builtin();
    let action = resolve_action_for(ev, &actions)
        .expect("the fixture Cargo target dir must resolve to cargo.clean.target_dir");

    // Step 2: AutoSafe always authorizes — no consent, exactly like a real
    // `execute` invocation with no `--confirm-ask`/`--observed-fingerprint`.
    let approval = authorize(decision.clone(), ev.fingerprint.clone(), None)
        .expect("AutoSafe must always authorize");

    // Step 3: the TOCTOU window — the project hook appears only now,
    // strictly after the decision above was made.
    write_project_hook(&project_root, "settings.json", &hook_bin);

    // Step 4: the exact revalidation call site under test — a collector
    // built from the REAL project roots, matching the unmutated
    // `run_execute_command` in `src/main.rs`.
    let revalidation_collector = fixture_scoped_collector(&home, &project_roots);
    let report = execute(action, &approval, &revalidation_collector, &cfg, now, None);

    assert_eq!(
        report.outcome,
        ExecutionOutcome::AbortedByRevalidation(AbortReason::PolicyClassDowngraded),
        "a hook added after plan time but before execution must abort revalidation \
         with PolicyClassDowngraded, got: {:?}",
        report.outcome
    );

    assert!(
        target_dir.exists(),
        "the target dir must never be deleted once revalidation aborts"
    );
    assert!(
        hook_bin.exists(),
        "the hook binary must never be deleted once revalidation aborts"
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}

// ---- M4b: CLI-level proof that `run_execute_command`'s revalidation
// collector (`src/main.rs`) is the one that must see real project roots
// ----

/// The M4 test above proves the TOCTOU guard inside `executor::execute`
/// itself is correct, but it calls that function directly — it never
/// routes through `src/main.rs`'s `run_execute_command`, so mutating
/// THAT call site (`let collector = live_evidence_collector(&project_roots);`
/// at `src/main.rs`) cannot make it fail. This test closes that specific
/// remaining gap with a real subprocess invocation of `glomeris execute`.
///
/// It does not need an artificial filesystem race: a project's
/// `.claude/settings.json` that exists but fails to PARSE makes the
/// executable-dependency probe report `source_unreadable` for
/// `claude_project` and classify `Ask[ExecutableDependencyUnknown]` —
/// but only when the probe actually examines that file, i.e. only when
/// the collector it runs against was built with this project's real
/// root. A collector built with no project roots at all (`&[]`, exactly
/// what the M4/M5 mutation substitutes) never attempts to read that file
/// and reports a clean probe instead, landing on `AutoSafe`.
///
/// `run_execute_command` calls `discover_and_classify_now_with_progress`
/// (plan time) and the revalidation inside `execute()` with TWO
/// SEPARATE collectors built from the SAME `project_roots` variable —
/// discovery's is built by an untouched helper, so it always sees this
/// malformed file and always decides `Ask`, regardless of the mutation.
/// Only the revalidation collector is the mutated one. So:
///
/// - Unmutated: plan time decides `Ask[ExecutableDependencyUnknown]`;
///   `--confirm-ask` with the real observed fingerprint authorizes it;
///   revalidation (real roots) reaches the SAME `Ask[ExecutableDependencyUnknown]`
///   decision, matches the plan, and the consented deletion proceeds —
///   `succeeded`, `node_modules` actually deleted.
/// - Mutated (`&[]` at that one call site): revalidation can no longer
///   see the malformed file, decides `AutoSafe` instead, disagrees with
///   the planned `Ask` class, and `execute()` aborts with
///   `AbortedByRevalidation(PolicyClassDowngraded)` — `node_modules`
///   preserved.
///
/// Manually verified against the real `src/main.rs:1432` mutation:
/// temporarily changing that line to
/// `live_evidence_collector(&[])` and rebuilding made this exact test
/// fail — exit 4, `outcome: "aborted_by_revalidation"`,
/// `abort_reason: "PolicyClassDowngraded"`, `node_modules` preserved —
/// then reverting the line and rebuilding restored a passing `exit 0`,
/// `outcome: "succeeded"`, `node_modules` deleted. This test commits the
/// unmutated (correct) `succeeded`-and-deleted outcome as the
/// assertion, which is exactly what that mutation flips.
#[test]
fn m4b_execute_consented_ask_deletion_requires_the_revalidation_collector_to_see_real_project_roots(
) {
    let home = unique_dir("m4b-home");
    let project_root = unique_dir("m4b-project");
    let node_modules = project_root.join("node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    fs::write(node_modules.join("package.json"), "{}").expect("write package.json");

    // A project `.claude/settings.json` that EXISTS but does not parse —
    // the executable-dependency probe must report it as
    // `source_unreadable`, never silently skip it (that would be the
    // HORO-1865 bug reappearing in a different shape).
    fs::create_dir_all(project_root.join(".claude")).expect("create .claude dir");
    fs::write(
        project_root.join(".claude").join("settings.json"),
        "not valid json {{{",
    )
    .expect("write malformed settings.json");

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
    let explain_report = parse_json(&explain);
    assert!(
        explain_report["reasons"]
            .as_array()
            .expect("reasons must be an array")
            .iter()
            .any(|r| r == "executable_dependency_unknown"),
        "a malformed project settings file must classify ExecutableDependencyUnknown \
         (Ask), got: {explain_report}"
    );
    assert!(
        explain_report["executable_dependency"]["unresolved"]
            .as_array()
            .expect("unresolved must be an array")
            .iter()
            .any(|u| u["source"] == "claude_project" && u["kind"] == "source_unreadable"),
        "expected the malformed file to be reported source_unreadable, not silently \
         skipped, got: {explain_report}"
    );
    let fingerprint_token = explain_report["fingerprint_token"]
        .as_str()
        .expect("fingerprint_token must be present for an Ask-eligible resource")
        .to_string();

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
            "--confirm-ask",
            "--observed-fingerprint",
            &fingerprint_token,
            "--json",
        ],
        &home,
    );
    assert_eq!(
        execute.status.code(),
        Some(0),
        "a consented Ask deletion where plan-time and revalidation agree must succeed; \
         stderr: {}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let result = parse_json(&execute);
    assert_eq!(
        result["outcome"], "succeeded",
        "expected the consented deletion to succeed — if this instead reports \
         aborted_by_revalidation/PolicyClassDowngraded, the execute-revalidation \
         collector at src/main.rs's run_execute_command has stopped seeing real \
         project roots, got: {result}"
    );
    assert!(
        !node_modules.exists(),
        "the consented node_modules deletion must actually happen"
    );

    fs::remove_dir_all(&home).ok();
    fs::remove_dir_all(&project_root).ok();
}
