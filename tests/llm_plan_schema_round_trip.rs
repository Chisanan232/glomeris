//! CLI integration proof for HORO-1048: `glomeris llm-plan --schema`'s
//! emitted example is accepted unchanged by `glomeris llm-plan
//! --plan-file <emitted-file>` — the ticket's load-bearing AC.
//!
//! Spawns the real `glomeris` binary twice: once for `--schema` to obtain
//! the exact bytes it prints, and once for `--plan-file` pointed at a
//! temp file holding those exact bytes, unmodified. Same
//! `glomeris_bin()`/temp-`$HOME` idiom as `execution_lock_wiring.rs`/
//! `history_cli.rs`.
//!
//! "Accepted" here means what `--plan-file` legitimately does with a
//! well-formed, non-actionable example plan: parses/validates through
//! the real `extract_plan`/`LlmPlan`/`plan_with_llm` pipeline without a
//! parse-error exit code. The example's `resource_id` deliberately does
//! not match any resource `glomeris` will ever actually discover on the
//! disposable temp `$HOME` this test points at, so the item is expected
//! to be dropped as an unknown resource — that is a successful,
//! non-error round trip through validation, not a rejection of the
//! document itself. `--project-root` is intentionally omitted so
//! discovery only walks the disposable temp home, keeping this test fast
//! and independent of the host machine's real filesystem state.

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
        "glomeris-llm-plan-schema-test-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

#[test]
fn schema_output_round_trips_through_plan_file() {
    let home = make_temp_home("round-trip");

    // Step 1: `glomeris llm-plan --schema` — must succeed and print
    // something that looks like the documented example, with no
    // discovery/provider side effects.
    let schema_output = Command::new(glomeris_bin())
        .arg("llm-plan")
        .arg("--schema")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris llm-plan --schema");

    assert!(
        schema_output.status.success(),
        "glomeris llm-plan --schema must exit successfully; stderr: {}",
        String::from_utf8_lossy(&schema_output.stderr)
    );
    let emitted = String::from_utf8_lossy(&schema_output.stdout).into_owned();
    assert!(
        emitted.contains("\"items\""),
        "expected an LlmPlan-shaped document, got: {emitted}"
    );

    // Step 2: feed the exact emitted bytes back in unchanged via
    // --plan-file.
    let plan_file = home.join("emitted-schema.json");
    std::fs::write(&plan_file, &emitted).expect("write emitted schema to plan file");

    let plan_output = Command::new(glomeris_bin())
        .arg("llm-plan")
        .arg("--plan-file")
        .arg(&plan_file)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris llm-plan --plan-file");

    let stdout = String::from_utf8_lossy(&plan_output.stdout);
    let stderr = String::from_utf8_lossy(&plan_output.stderr);

    // Exit code 1 is `llm-plan`'s designated "provider call or response
    // parsing failed" code (see book/src/cli_reference.md) — the emitted
    // schema must never trigger it. Exit code 2 is a usage error, which a
    // well-formed --plan-file invocation must also never hit.
    assert!(
        plan_output.status.success(),
        "glomeris llm-plan --plan-file <emitted-schema> must not fail parsing/validation \
         (exit code {:?}); stdout: {stdout}\nstderr: {stderr}",
        plan_output.status.code()
    );
    assert!(
        stdout.contains("LLM SUGGESTION"),
        "expected the standard advisory banner, got: {stdout}"
    );
    // The emitted example's resource_id will not match any resource
    // discovered on this disposable temp $HOME — that is an expected,
    // non-error drop, proving validation ran (not merely a parse
    // no-op), not a rejection of the schema document itself.
    assert!(
        stdout.contains("dropped: 1 unknown resource(s)"),
        "expected the example item to be dropped as an unknown resource (not a parse \
         failure), got: {stdout}"
    );
}

/// The version 2 equivalent (HORO-1548). Same property, and it needs its own
/// proof because version 2's example is a different document read by a
/// different parser: a fixture that parses but is then discarded by
/// validation would still print, and nobody would notice until they used it.
///
/// The assertions deliberately avoid the item row. Version 2 resolves
/// `resource_id` against the request's own alias table, and which resource
/// lands on `resource_1` depends on what this host happens to have — so on
/// one machine the example's item is dropped as an unknown resource and on
/// another as an action that resource was not offered. What is host
/// independent, and what proves the round trip, is that the profile and the
/// observation survived: both cite references the projection always issues
/// (`workflow_history` and `machine`), so they reach the rendered report on
/// any machine, and they only reach it by going through real validation.
#[test]
fn version_two_schema_output_round_trips_through_plan_file() {
    let home = make_temp_home("round-trip-v2");

    let schema_output = Command::new(glomeris_bin())
        .arg("llm-plan")
        .arg("--contract-version")
        .arg("2")
        .arg("--schema")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris llm-plan --contract-version 2 --schema");

    assert!(
        schema_output.status.success(),
        "--contract-version 2 --schema must exit successfully; stderr: {}",
        String::from_utf8_lossy(&schema_output.stderr)
    );
    let emitted = String::from_utf8_lossy(&schema_output.stdout).into_owned();
    assert!(
        emitted.contains("\"contract_version\": 2"),
        "the version 2 example must declare its contract version, got: {emitted}"
    );
    assert!(
        emitted.contains("\"workspace_profile\""),
        "expected a version 2 shaped document, got: {emitted}"
    );

    let plan_file = home.join("emitted-schema-v2.json");
    std::fs::write(&plan_file, &emitted).expect("write emitted schema to plan file");

    let plan_output = Command::new(glomeris_bin())
        .arg("llm-plan")
        .arg("--contract-version")
        .arg("2")
        .arg("--plan-file")
        .arg(&plan_file)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris llm-plan --contract-version 2 --plan-file");

    let stdout = String::from_utf8_lossy(&plan_output.stdout);
    let stderr = String::from_utf8_lossy(&plan_output.stderr);

    assert!(
        plan_output.status.success(),
        "--contract-version 2 --plan-file <emitted-schema> must not fail parsing/validation \
         (exit code {:?}); stdout: {stdout}\nstderr: {stderr}",
        plan_output.status.code()
    );
    assert!(
        stdout.contains("WORKSPACE PLAN (contract v2)"),
        "expected the version 2 advisory banner, got: {stdout}"
    );
    // Proof that validation ran rather than the document merely parsing: the
    // profile and the observation are rendered with the model attribution
    // that only `build_workspace_plan_report` applies.
    assert!(
        stdout.contains("model workspace profile: unknown"),
        "the example's profile must survive validation, got: {stdout}"
    );
    assert!(
        stdout.contains("model observation [missing_evidence]"),
        "the example's observation must survive validation, got: {stdout}"
    );
    // And the banner must not have been reached by skipping the provider
    // step: a document this build could not read would say so instead.
    assert!(
        !stderr.contains("provider error"),
        "the emitted example must not be reported as a provider error: {stderr}"
    );

    std::fs::remove_dir_all(&home).ok();
}
