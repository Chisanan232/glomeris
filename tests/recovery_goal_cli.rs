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
//! destructive run against the machine running the tests. Since HORO-1509 the
//! same warning covers any *plausible* used-percent goal — `60`, say — because
//! the flag combinations that used to be refused at parse time (`--progress-json`
//! on a real run) are now honoured, and the only thing that was keeping such an
//! invocation harmless was the refusal.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

/// A `HOME` of this invocation's own, which is what keeps these tests from
/// racing each other.
///
/// Four of the tests below take the execution lock for real — every `--target
/// 0%` run does, before it discovers that the target is already met. The lock is
/// `$HOME/Library/Application Support/Glomeris/execution.lock` and it is
/// exclusive and non-blocking, so under `cargo test`'s default thread pool two
/// of them will sooner or later overlap and the loser exits 75 `busy`. That is
/// not flakiness to be retried away: the refusal is correct, and the test that
/// saw it was asserting something else entirely.
///
/// Relocating `HOME` rather than serialising the tests is the precedent
/// `execution_lock_wiring.rs` sets, and it is the better fix here for a second
/// reason: it also detaches these runs from the developer's real config and
/// history, so what the CLI does under test stops depending on whose machine it
/// is running on.
fn make_temp_home() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-recovery-goal-cli-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_free(args: &[&str]) -> Run {
    run_free_in(&make_temp_home(), args)
}

/// As above, but reusing a `HOME` the caller owns — needed by the `--autopilot`
/// tests, where the envelope one invocation writes is the input to the next.
fn run_free_in(home: &PathBuf, args: &[&str]) -> Run {
    run_in(home, "free", args)
}

fn run_in(home: &PathBuf, command: &str, args: &[&str]) -> Run {
    let output = Command::new(glomeris_bin())
        .arg(command)
        .args(args)
        .env("HOME", home)
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

/// HORO-1509, end to end: a real run streams its own progress as NDJSON on
/// stderr while leaving stdout a single report.
///
/// `--target 0%` is what makes this safe to run for real — it is already met, so
/// the loop measures the volume, finds the target satisfied at step 2 and stops
/// without discovering or deleting anything. That also makes it the sharpest
/// possible assertion: exactly one event can be emitted, so a stream that
/// carried more would mean the loop had gone further than it said it did.
///
/// This test replaces a refusal (`--progress-json` used to require `--dry-run`).
/// It deliberately does *not* use a used-percent goal: see the module header.
#[test]
fn progress_json_on_a_real_run_streams_the_loop_on_stderr() {
    let run = run_free(&["--target", "0%", "--progress-json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    let lines: Vec<&str> = run.stderr.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "an already-met target stops at step 2, so exactly one measurement is \
         reportable; got: {}",
        run.stderr
    );
    let event: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("progress must be NDJSON ({e}): {}", lines[0]));
    assert_eq!(event["phase"], "measured", "got: {event}");
    assert_eq!(event["iteration"], 1, "got: {event}");
    assert_eq!(
        event["bytes_freed_so_far"], 0,
        "nothing ran, so nothing may be claimed as reclaimed: {event}"
    );
    assert!(
        event["free_bytes"].is_u64() && event["total_bytes"].is_u64(),
        "the one measured event is where a client learns current usage: {event}"
    );

    // The progress stream may never contaminate the report stream: a `--json`
    // caller's stdout has to stay parseable as exactly one document.
    assert!(
        !run.stdout.contains("\"phase\""),
        "progress leaked onto stdout: {}",
        run.stdout
    );
}

/// Without the flag, a run is observed by nobody — proven by the absence rather
/// than assumed, because an observer that wrote unconditionally would still pass
/// every assertion in the test above.
#[test]
fn a_run_without_progress_json_emits_no_progress_at_all() {
    let run = run_free(&["--target", "0%"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr.is_empty(),
        "stderr must stay byte-identical to a pre-HORO-1509 run: {}",
        run.stderr
    );
}

/// A sentinel left behind by an earlier run would stop the next one before it
/// did anything, and the report would truthfully say the user stopped it while
/// the user had done nothing at all. Refused, and refused before the run.
#[test]
fn a_stop_file_that_already_exists_is_refused_before_anything_runs() {
    let existing = std::env::temp_dir().join(format!(
        "glomeris-stop-file-exists-{}-{}.stop",
        std::process::id(),
        line!()
    ));
    std::fs::write(&existing, b"").expect("create the stale sentinel");

    let run = run_free(&["--target", "0%", "--stop-file", existing.to_str().unwrap()]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("already exists"),
        "the refusal must say what is wrong with the path: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "a refused run must print no report: {}",
        run.stdout
    );

    std::fs::remove_file(&existing).ok();
}

/// A dry run performs no actions, so there is nothing for a stop request to
/// stop. Accepting the flag and ignoring it would hand a caller a handle on a
/// run it does not have.
#[test]
fn a_stop_file_is_refused_together_with_dry_run() {
    let run = run_free(&[
        "--target",
        "0%",
        "--dry-run",
        "--stop-file",
        "/tmp/unused.stop",
    ]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--dry-run"),
        "the refusal must name the conflicting flag: {}",
        run.stderr
    );
}

/// Watching for a stop request must not itself create the sentinel — a loop that
/// created the file it watches would stop immediately, every time.
#[test]
fn watching_for_a_stop_request_does_not_create_the_sentinel() {
    let sentinel = std::env::temp_dir().join(format!(
        "glomeris-stop-file-unused-{}-{}.stop",
        std::process::id(),
        line!()
    ));
    std::fs::remove_file(&sentinel).ok();

    let run = run_free(&["--target", "0%", "--stop-file", sentinel.to_str().unwrap()]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stdout
            .contains("stop reason:            target_reached"),
        "an unrequested stop must not change why the run ended: {}",
        run.stdout
    );
    assert!(
        !sentinel.exists(),
        "the loop created the sentinel it was watching: {}",
        sentinel.display()
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

// HORO-1510. `--autopilot` narrows this same loop with the stored envelope.
//
// Every invocation below is safe for the same two reasons the rest of this file
// relies on: the refusals happen before the execution lock and before discovery,
// and the one run that proceeds uses `--target 0%`, which is already met and so
// stops at step 2 having deleted nothing. The envelope each test writes lives in
// its own temporary `HOME`, so none of them can read or alter the real grant on
// the machine running the suite.

/// Defaults grant nothing, so `--autopilot` with no stored envelope is a request
/// to act under an authority that does not exist. Refused, by exit code, without
/// attempting anything — the same 3 `autopilot run` uses, so a script can tell
/// "not authorized" from a usage error and from a failed run.
#[test]
fn autopilot_without_a_grant_refuses_rather_than_running_unbounded() {
    let home = make_temp_home();
    let run = run_free_in(&home, &["--target", "0%", "--autopilot"]);
    assert_eq!(run.code, Some(3), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("not enabled") && run.stderr.contains("Nothing was attempted"),
        "the refusal must say both that there is no grant and that nothing ran: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "an unauthorized run must print no recovery report: {}",
        run.stdout
    );
}

/// The important half of the assertion above, proven by contrast rather than
/// assumed: the identical invocation without the flag succeeds. Were the exit 3
/// coming from something other than the missing grant, this would fail too.
#[test]
fn the_same_invocation_without_autopilot_is_unaffected_by_a_missing_grant() {
    let home = make_temp_home();
    let run = run_free_in(&home, &["--target", "0%"]);
    assert_eq!(
        run.code,
        Some(0),
        "a run the user started needs no grant: {}",
        run.stderr
    );
}

/// An envelope is authority to execute. A preview executes nothing, so accepting
/// the flag would hand back the full candidate list looking envelope-filtered —
/// read as "this is what Autopilot would do" when it is everything.
#[test]
fn autopilot_is_refused_together_with_dry_run() {
    let run = run_free(&["--target", "0%", "--autopilot", "--dry-run"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--dry-run") && run.stderr.contains("--autopilot"),
        "the refusal must name both flags: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("autopilot show"),
        "a user who wanted to read the grant must be told how: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "a refused combination must print no pre-flight: {}",
        run.stdout
    );
}

/// With a grant in place the run proceeds, and opens by stating the authority it
/// is acting under — on stderr, so that a `--json` caller's stdout stays exactly
/// one document.
#[test]
fn a_granted_autopilot_run_states_its_envelope_and_keeps_stdout_one_document() {
    let home = make_temp_home();
    let enable = run_in(
        &home,
        "autopilot",
        &[
            "enable",
            "--kinds",
            "node_modules",
            "--max-actions",
            "1",
            "--max-duration",
            "30",
        ],
    );
    assert_eq!(enable.code, Some(0), "stderr: {}", enable.stderr);

    let run = run_free_in(&home, &["--target", "0%", "--autopilot", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("Autopilot envelope:"),
        "a run that may delete must open with the grant it acts under: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("node_modules"),
        "the printed envelope must be the stored one, not a generic sentence: {}",
        run.stderr
    );

    let parsed: serde_json::Value = serde_json::from_str(&run.stdout).unwrap_or_else(|e| {
        panic!(
            "the envelope description leaked into the report stream ({e}); stdout: {}",
            run.stdout
        )
    });
    // Already met, so the loop stopped at step 2. The envelope narrows what may
    // be executed; it has no opinion about whether a target was reached, and a
    // run that stops for a reason the envelope had nothing to do with must not
    // be attributed to it.
    assert_eq!(parsed["stop_reason"], "target_reached", "got: {parsed}");
    assert!(
        parsed.get("envelope_refusal").is_none(),
        "only an envelope-refused stop may name an Autopilot limit: {parsed}"
    );
}

/// A revoked grant is not a weaker grant. `revoke` keeps the limits on file so a
/// later `enable` cannot return with limits nobody read — which is exactly why
/// this needs proving: the envelope still exists and still has generous numbers
/// in it, and only `is_enabled()` stands between it and a run.
#[test]
fn revoking_the_grant_stops_a_later_autopilot_run() {
    let home = make_temp_home();
    let enable = run_in(
        &home,
        "autopilot",
        &["enable", "--kinds", "node_modules", "--max-actions", "5"],
    );
    assert_eq!(enable.code, Some(0), "stderr: {}", enable.stderr);
    let granted = run_free_in(&home, &["--target", "0%", "--autopilot"]);
    assert_eq!(
        granted.code,
        Some(0),
        "the grant must admit the run before revoke can be shown to stop it: {}",
        granted.stderr
    );

    let revoke = run_in(&home, "autopilot", &["revoke"]);
    assert_eq!(revoke.code, Some(0), "stderr: {}", revoke.stderr);

    let run = run_free_in(&home, &["--target", "0%", "--autopilot"]);
    assert_eq!(
        run.code,
        Some(3),
        "a revoked grant must refuse exactly as an absent one does: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "a revoked run must print no report: {}",
        run.stdout
    );
}

// HORO-1510, second half: `--unattended` is a run nobody asked for, and that
// needs a permission separate from the grant's limits.

/// The combination that must not exist: a run nobody asked for, bounded by
/// nothing. Refused at the boundary as a usage error, so that no code path
/// downstream is ever handed a value meaning it.
#[test]
fn unattended_without_a_grant_to_run_inside_is_a_usage_error() {
    let run = run_free(&["--target", "0%", "--unattended"]);
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--unattended") && run.stderr.contains("--autopilot"),
        "the refusal must name both flags, since either one is the fix: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "a refused combination must print no report: {}",
        run.stdout
    );
}

/// The heart of the ticket. An envelope written to bound a run its owner meant
/// to start is not consent to start one unasked, so an enabled grant alone
/// refuses `--unattended` — and says which of the two permissions is missing,
/// because "enable Autopilot" would be wrong advice to someone who already has.
#[test]
fn an_enabled_grant_is_not_by_itself_permission_to_start_unasked() {
    let home = make_temp_home();
    let enable = run_in(
        &home,
        "autopilot",
        &["enable", "--kinds", "node_modules", "--max-actions", "1"],
    );
    assert_eq!(enable.code, Some(0), "stderr: {}", enable.stderr);

    let run = run_free_in(&home, &["--target", "0%", "--autopilot", "--unattended"]);
    assert_eq!(run.code, Some(3), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("--respond-to-alerts"),
        "the refusal must name the permission that is actually missing: {}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("not enabled"),
        "this grant *is* enabled; saying otherwise would send the reader to the \
         wrong setting: {}",
        run.stderr
    );
    assert!(
        run.stdout.is_empty(),
        "an unauthorized run must print no report: {}",
        run.stdout
    );

    // Proven by contrast, so that the exit 3 above is attributable to the
    // unprompted-run permission and not to anything else about the invocation.
    let asked = run_free_in(&home, &["--target", "0%", "--autopilot"]);
    assert_eq!(
        asked.code,
        Some(0),
        "the same grant must still admit a run somebody asked for: {}",
        asked.stderr
    );
}

/// With both permissions the run proceeds, and still opens by stating the
/// authority it acted under — most of all this one, which nobody was present to
/// read a prompt for.
#[test]
fn a_grant_that_says_so_admits_a_run_nobody_asked_for() {
    let home = make_temp_home();
    let enable = run_in(
        &home,
        "autopilot",
        &[
            "enable",
            "--kinds",
            "node_modules",
            "--max-actions",
            "1",
            "--respond-to-alerts",
        ],
    );
    assert_eq!(enable.code, Some(0), "stderr: {}", enable.stderr);

    let run = run_free_in(&home, &["--target", "0%", "--autopilot", "--unattended"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("Autopilot envelope:"),
        "a run nobody watched start must still record the grant it ran under: {}",
        run.stderr
    );
}

/// `revoke` is the single bit its own documentation promises: it stops unprompted
/// runs too, without the setting having to be cleared. Worth proving separately
/// from the revoke test above, because the authority here is a conjunction and a
/// conjunction is exactly the shape that gets half-checked.
#[test]
fn revoking_stops_an_unprompted_run_as_well() {
    let home = make_temp_home();
    let enable = run_in(
        &home,
        "autopilot",
        &[
            "enable",
            "--kinds",
            "node_modules",
            "--max-actions",
            "1",
            "--respond-to-alerts",
        ],
    );
    assert_eq!(enable.code, Some(0), "stderr: {}", enable.stderr);
    let granted = run_free_in(&home, &["--target", "0%", "--autopilot", "--unattended"]);
    assert_eq!(
        granted.code,
        Some(0),
        "the grant must admit the run before revoke can be shown to stop it: {}",
        granted.stderr
    );

    let revoke = run_in(&home, "autopilot", &["revoke"]);
    assert_eq!(revoke.code, Some(0), "stderr: {}", revoke.stderr);

    let run = run_free_in(&home, &["--target", "0%", "--autopilot", "--unattended"]);
    assert_eq!(
        run.code,
        Some(3),
        "a revoked grant authorizes nothing, unasked least of all: {}",
        run.stderr
    );

    // And the setting survived, so a later `enable --respond-to-alerts` is not
    // the only way back — `autopilot show` still reports what was chosen.
    let show = run_in(&home, "autopilot", &["show"]);
    assert_eq!(show.code, Some(0), "stderr: {}", show.stderr);
    assert!(
        show.stdout.contains("dormant while revoked"),
        "revoking must not read as having cleared the preference: {}",
        show.stdout
    );
}
