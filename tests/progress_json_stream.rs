//! HORO-1052 regression proof: `--progress-json` adds an opt-in NDJSON
//! stream on stderr for `detect`/`explain`/`llm-plan`'s shared discovery
//! phase, without changing stdout's report shape or stderr's content at
//! all when the flag is omitted (the ticket's explicit, testable AC).
//!
//! Every test here spawns the real compiled binary — never calls library
//! functions directly — so it proves the actual CLI-visible behavior, not
//! just what the underlying `cli::discover_and_classify_with_progress`
//! function does in isolation.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn make_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Builds a `<root>/target/debug/...` fixture — the same shape
/// `cli_project_root_wiring.rs` uses — so the real `cargo_target_dir`
/// detector finds exactly one, fully deterministic resource regardless of
/// whatever real Xcode/Homebrew/Docker state happens to exist on the host
/// running the test.
fn make_project_root_fixture() -> PathBuf {
    let root = make_temp_dir("horo1052-project-root");
    let target_dir = root.join("target/debug");
    fs::create_dir_all(&target_dir).expect("create cargo target dir");
    fs::write(target_dir.join("build_output.bin"), vec![0u8; 4096]).expect("write cargo fixture");
    root
}

/// The `cargo_target_dir` detector reports a *canonicalized* resource
/// path (see `detectors::cargo::CargoDetector::discover`) — `explain`'s
/// query must match that exactly, not the non-canonicalized path the
/// fixture was built from (on macOS, `std::env::temp_dir()` lives under a
/// symlinked `/var/folders/...` path whose canonical form differs).
fn canonical_target_dir_query(project_root: &std::path::Path) -> String {
    project_root
        .join("target")
        .canonicalize()
        .expect("fixture target dir must exist and canonicalize")
        .to_string_lossy()
        .into_owned()
}

/// Every builtin detector id (`detectors::DetectorRegistry::builtin`) —
/// kept as a literal list here (rather than importing the registry) since
/// this test proves the CLI-observable NDJSON stream, not the registry's
/// internals.
const BUILTIN_DETECTOR_IDS: &[&str] = &[
    "xcode_derived_data",
    "homebrew_cache",
    "cargo_target_dir",
    "node_modules",
    "docker_build_cache",
];

/// Runs the real binary with an isolated, empty `$HOME` (so the
/// `xcode_derived_data`/`homebrew_cache` detectors don't pick up whatever
/// real, actively-changing state happens to exist on the host running the
/// test — see the module doc comment) and no stdin.
fn run_glomeris(args: &[&str], home: &std::path::Path) -> Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// Parses `stderr` as NDJSON: one `serde_json::Value` per non-empty line,
/// panicking with the offending line on any parse failure.
fn parse_ndjson_lines(stderr: &str) -> Vec<serde_json::Value> {
    stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"))
        })
        .collect()
}

/// Asserts `events` contains exactly one `detector_started` and one
/// `detector_finished` line for every builtin detector — "one line per
/// detector that actually ran" from the ticket's AC.
fn assert_one_start_and_finish_per_builtin_detector(events: &[serde_json::Value]) {
    for detector in BUILTIN_DETECTOR_IDS {
        let started = events
            .iter()
            .filter(|e| e["phase"] == "detector_started" && e["detector"] == *detector)
            .count();
        let finished = events
            .iter()
            .filter(|e| e["phase"] == "detector_finished" && e["detector"] == *detector)
            .count();
        assert_eq!(
            started, 1,
            "expected exactly one detector_started event for '{detector}', got {started}: {events:?}"
        );
        assert_eq!(
            finished, 1,
            "expected exactly one detector_finished event for '{detector}', got {finished}: {events:?}"
        );
    }
    // No unrecognized phase or detector name snuck in.
    for event in events {
        let phase = event["phase"]
            .as_str()
            .expect("phase field must be a string");
        assert!(
            phase == "detector_started" || phase == "detector_finished",
            "unexpected phase '{phase}' in event: {event}"
        );
        if phase == "detector_finished" {
            assert!(
                event["candidates_found"].is_u64(),
                "detector_finished event missing numeric candidates_found: {event}"
            );
        }
    }
}

/// AC 1 (regression): without `--progress-json`, `detect --json` prints
/// nothing extra on stderr and a valid `DetectReport` on stdout.
#[test]
fn detect_without_progress_json_flag_emits_no_extra_stderr() {
    let home = make_temp_dir("horo1052-home-detect-baseline");

    let output = run_glomeris(&["detect", "--json"], &home);

    assert!(
        output.status.success(),
        "glomeris detect --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr must be empty without --progress-json, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert!(
        report.get("candidates").is_some(),
        "expected a DetectReport shape with a 'candidates' field, got: {stdout}"
    );

    fs::remove_dir_all(&home).ok();
}

/// AC 1 (regression), `explain` variant.
#[test]
fn explain_without_progress_json_flag_emits_no_extra_stderr() {
    let home = make_temp_dir("horo1052-home-explain-baseline");
    let project_root = make_project_root_fixture();
    let query = canonical_target_dir_query(&project_root);

    let output = run_glomeris(
        &[
            "explain",
            &query,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );

    assert!(
        output.status.success(),
        "glomeris explain --json should succeed for the fixture target dir; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr must be empty without --progress-json, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    assert_eq!(report["kind"], "cargo_target_dir");

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&home).ok();
}

/// AC 2: with `--progress-json`, stderr is a valid NDJSON stream with one
/// start/finish pair per detector that ran.
#[test]
fn detect_with_progress_json_flag_emits_valid_ndjson_per_detector() {
    let home = make_temp_dir("horo1052-home-detect-progress");

    let output = run_glomeris(&["detect", "--progress-json"], &home);

    assert!(
        output.status.success(),
        "glomeris detect --progress-json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let events = parse_ndjson_lines(&stderr);
    assert!(
        !events.is_empty(),
        "expected at least one progress event on stderr, got none"
    );
    assert_one_start_and_finish_per_builtin_detector(&events);

    fs::remove_dir_all(&home).ok();
}

/// AC 3: stdout's report is byte-identical whether or not
/// `--progress-json` is passed, for the same inputs. Uses the
/// `explain`+fixture combination (see `make_project_root_fixture`'s doc
/// comment) so the comparison is not vulnerable to real, independently
/// changing Xcode/Homebrew/Docker state on the host between the two
/// invocations — a real flakiness source observed manually while building
/// this test (a live Xcode build on the dev host changed
/// `xcode_derived_data`'s reported size between two back-to-back `detect`
/// calls).
#[test]
fn explain_stdout_is_identical_with_and_without_progress_json() {
    let home = make_temp_dir("horo1052-home-explain-identical");
    let project_root = make_project_root_fixture();
    let query = canonical_target_dir_query(&project_root);

    let without_flag = run_glomeris(
        &[
            "explain",
            &query,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
        ],
        &home,
    );
    let with_flag = run_glomeris(
        &[
            "explain",
            &query,
            "--project-root",
            &project_root.to_string_lossy(),
            "--json",
            "--progress-json",
        ],
        &home,
    );

    assert!(without_flag.status.success());
    assert!(with_flag.status.success());
    assert_eq!(
        without_flag.stdout, with_flag.stdout,
        "stdout must be identical regardless of --progress-json"
    );
    assert!(without_flag.stderr.is_empty());
    assert!(!with_flag.stderr.is_empty());
    let events = parse_ndjson_lines(&String::from_utf8_lossy(&with_flag.stderr));
    assert_one_start_and_finish_per_builtin_detector(&events);

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&home).ok();
}

/// `llm-plan` shares the same discovery path — proves the flag is wired
/// there too, with no `--plan-file`/API key required since a missing LLM
/// configuration is reported (exit code 1) only *after* discovery runs,
/// which is all this test needs.
#[test]
fn llm_plan_with_progress_json_flag_emits_valid_ndjson_before_the_missing_config_error() {
    let home = make_temp_dir("horo1052-home-llm-plan-progress");

    let output = run_glomeris(&["llm-plan", "--progress-json"], &home);

    let stderr = String::from_utf8_lossy(&output.stderr);
    let events = parse_ndjson_lines(
        &stderr
            .lines()
            .filter(|l| l.trim_start().starts_with('{'))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(
        !events.is_empty(),
        "expected progress events on stderr even though llm-plan itself refuses \
         for missing LLM configuration; full stderr: {stderr}"
    );
    assert_one_start_and_finish_per_builtin_detector(&events);

    fs::remove_dir_all(&home).ok();
}

/// `llm-plan` regression: without the flag, stderr contains only the
/// existing missing-configuration message — no progress lines at all.
#[test]
fn llm_plan_without_progress_json_flag_emits_no_progress_lines() {
    let home = make_temp_dir("horo1052-home-llm-plan-baseline");

    let output = run_glomeris(&["llm-plan"], &home);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("\"phase\":"),
        "no progress event should appear on stderr without --progress-json, got: {stderr}"
    );

    fs::remove_dir_all(&home).ok();
}
