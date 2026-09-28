//! CLI integration proof for HORO-1506: `glomeris free` accepts a recovery
//! goal on the *used* axis, refuses a goal that would reclaim nothing, and
//! reports both axes by name so no surface ever shows a bare percentage.
//!
//! Every invocation here is deliberately chosen to be safe to spawn as a real
//! subprocess on whatever machine runs the suite:
//!
//! - `--goal-used-percent 100` is rejected before the execution lock is taken
//!   and before any detector runs, because no real volume is more than 100%
//!   used, so it can never be an improvement (see
//!   `RecoveryGoal::progress_toward`). It is the one used-axis value that is
//!   guaranteed to refuse rather than delete.
//! - `--target 0%` is trivially already met, so the loop returns
//!   `TargetReached` at step 2 before discovery — the same reasoning
//!   `execution_lock_wiring.rs` documents for its own use of it.
//! - `--dry-run` takes no lock and mutates nothing.
//!
//! A value like `--goal-used-percent 0` must never appear in this file: it
//! *would* be an improvement on any real disk, so it would start a real
//! destructive run against the machine running the tests.

use std::process::{Command, Stdio};

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_free(args: &[&str]) -> Run {
    let output = Command::new(glomeris_bin())
        .arg("free")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn a_goal_is_required_and_the_message_names_both_forms() {
    let run = run_free(&[]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--goal-used-percent") && run.stderr.contains("--target"),
        "a caller who passed neither must be told both exist: {}",
        run.stderr
    );
}

/// The two flags are different axes, so there is no sensible precedence
/// between them: picking one silently would mean choosing how much of a disk
/// to delete on the user's behalf.
#[test]
fn passing_both_axes_is_refused_rather_than_resolved_by_precedence() {
    let run = run_free(&["--goal-used-percent", "60", "--target", "20%"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("two different axes"),
        "the refusal must explain why, not just that: {}",
        run.stderr
    );
}

#[test]
fn an_out_of_range_or_unparseable_goal_is_refused_on_the_used_axis() {
    let out_of_range = run_free(&["--goal-used-percent", "150"]);
    assert_eq!(
        out_of_range.code,
        Some(2),
        "stderr: {}",
        out_of_range.stderr
    );
    assert!(
        out_of_range.stderr.contains("150% used"),
        "the refusal must quote the offending value on its axis: {}",
        out_of_range.stderr
    );

    let unparseable = run_free(&["--goal-used-percent", "sixty"]);
    assert_eq!(unparseable.code, Some(2), "stderr: {}", unparseable.stderr);
    assert!(
        unparseable.stderr.contains("USED"),
        "even a parse error must say which axis the number is on: {}",
        unparseable.stderr
    );
}

/// HORO-1506 AC4, proven end to end: the refusal happens before the execution
/// lock and before anything is deleted. `stdout` being empty is the load-bearing
/// half — a non-improving goal must not produce a report that reads like a
/// successful cleanup.
#[test]
fn a_goal_that_is_not_an_improvement_is_refused_before_anything_runs() {
    let run = run_free(&["--goal-used-percent", "100"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("not an improvement"),
        "expected the not-an-improvement refusal, got: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "a refused goal must print no recovery report — nothing ran; stdout: {}",
        run.stdout
    );
}

#[test]
fn a_refused_goal_is_machine_readable_under_json() {
    let run = run_free(&["--goal-used-percent", "100", "--json"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&run.stdout).unwrap_or_else(|e| {
        panic!(
            "--json must render a structured rejection on stdout rather than prose on stderr; \
             parse error {e}; stdout: {}",
            run.stdout
        )
    });
    assert_eq!(parsed["reason"], "not_an_improvement", "got: {parsed}");
    assert_eq!(parsed["goal_used_percent"], 100.0, "got: {parsed}");
    assert!(
        parsed["current_used_percent"].is_number(),
        "the refusal must report the measured usage it compared against: {parsed}"
    );
}

/// `--progress-json` currently describes the discovery scan only. Accepting it
/// on a real run and emitting nothing would let a caller believe it had
/// subscribed to progress it will never receive.
#[test]
fn progress_json_on_a_real_run_is_refused_rather_than_silently_ignored() {
    let run = run_free(&["--goal-used-percent", "60", "--progress-json"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--dry-run"),
        "the refusal must say what to do instead: {}",
        run.stderr
    );
}

/// The raw free-space floor keeps its old meaning, its old tolerance for a
/// floor that is already met, and now says which axis it is on.
#[test]
fn a_raw_target_run_still_succeeds_and_names_the_free_axis() {
    let run = run_free(&["--target", "0%"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains("free-space target:      0% free"),
        "the prose report must state the axis, not a bare percentage: {}",
        run.stdout
    );
    // Same token the `--json` report uses (see
    // `a_raw_target_run_reports_structured_json_with_an_explicit_stop_reason`),
    // because a stop reason that is spelled two ways is two things to learn.
    assert!(
        run.stdout
            .contains("stop reason:            target_reached"),
        "expected the unchanged target-reached report: {}",
        run.stdout
    );
}

/// A finished run is machine-readable, and its completion verdict comes from
/// the re-measured filesystem rather than from a sum of estimates.
#[test]
fn a_raw_target_run_reports_structured_json_with_an_explicit_stop_reason() {
    let run = run_free(&["--target", "0%", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("parse error {e}; stdout: {}", run.stdout));

    assert_eq!(parsed["stop_reason"], "target_reached", "got: {parsed}");
    assert_eq!(parsed["target"], "0% free", "got: {parsed}");
    assert_eq!(parsed["target_met"], true, "got: {parsed}");
    assert!(
        parsed.get("goal").is_none(),
        "a raw free-space floor is not a used-axis goal and must not be reported as one: {parsed}"
    );

    // HORO-1509's wording rule, enforced from the first surface that carries
    // it: a termination is explained, never collapsed into a success word.
    let detail = parsed["stop_reason_detail"].as_str().unwrap_or_default();
    assert!(
        detail.len() > "Done".len() && !detail.eq_ignore_ascii_case("done"),
        "the stop reason must be a sentence: {parsed}"
    );
}

/// The pre-flight mutates nothing, so it is safe to run for real. The
/// assertions are structural rather than about this machine's actual usage,
/// which no test can know.
#[test]
fn dry_run_previews_a_goal_on_both_axes_without_mutating_anything() {
    let run = run_free(&["--goal-used-percent", "60", "--dry-run", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("parse error {e}; stdout: {}", run.stdout));

    assert_eq!(parsed["goal"]["used_percent"], 60.0, "got: {parsed}");
    assert_eq!(parsed["goal"]["free_percent"], 40.0, "got: {parsed}");
    assert_eq!(
        parsed["goal"]["description"], "60% used (40% free)",
        "got: {parsed}"
    );

    // The estimate/measurement distinction must be stated, not implied.
    let caveats = parsed["caveats"]
        .as_array()
        .unwrap_or_else(|| panic!("caveats must be an array: {parsed}"));
    assert!(
        caveats.iter().any(|c| c
            .as_str()
            .unwrap_or_default()
            .contains("detector estimates")),
        "the preview must say its reclaimable figures are estimates: {parsed}"
    );

    // Protected space is counted, never summed into an opportunity total.
    assert!(
        parsed["opportunity"]["protected_count"].is_number(),
        "got: {parsed}"
    );
    assert!(
        parsed["opportunity"].get("protected_bytes").is_none(),
        "bytes no run can ever reclaim must not be reported as an opportunity: {parsed}"
    );
}

/// A raw `--target` preview is rendered on the used axis too, so the GUI never
/// has to display a percentage whose meaning is unstated.
#[test]
fn dry_run_on_a_raw_target_still_states_the_used_axis() {
    let run = run_free(&["--target", "40%", "--dry-run", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("parse error {e}; stdout: {}", run.stdout));
    assert_eq!(
        parsed["goal"]["description"], "60% used (40% free)",
        "a 40%-free floor is a 60%-used goal: {parsed}"
    );
}
