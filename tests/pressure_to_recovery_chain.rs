//! HORO-1510 AC 8: threshold → notification → recovery → loop → stop, walked
//! at the process boundary in the order the product walks it.
//!
//! The legs of this chain are each tested on their own elsewhere —
//! `tests/recovery_goal_cli.rs` for the goal and the Autopilot envelope,
//! `src/monitor/episode.rs` and `src/monitor/episode_store.rs` for episode
//! bookkeeping, `src/executor/recovery_loop.rs` for the loop. What none of them
//! can show is that the legs *join*: that the number an alert publishes is the
//! number `free` accepts, that the answer tokens the alert offers are the ones
//! `respond` parses, and that answering an alert is not by itself permission for
//! a run nobody watched start.
//!
//! # Where the episode comes from
//!
//! An episode is opened by the daemon, and `daemon run` is a blocking poll loop
//! with no single-shot mode — spawning and killing it would make these tests
//! depend on a sleep. So the episode is opened here through the same three
//! library calls `episode_step` in `src/main.rs` makes (`load_tracker_at`,
//! `EpisodeTracker::observe`, `save_tracker_at`), at the state path
//! `pressure show` itself publishes, under the threshold `pressure show` itself
//! reports. Nothing about the record's shape is written out in this file: the
//! producer writes it and the CLI reads it, which is the seam worth proving.
//!
//! What that leaves uncovered, deliberately: the launchd-driven poll loop, its
//! interval and its heartbeat. `src/monitor/poller.rs` owns those.
//!
//! # How these tests avoid deleting anything
//!
//! Three protections, none of them relying on the behaviour under test:
//!
//! 1. Every invocation runs with `HOME` pointed at a fresh temp directory, so
//!    the settings file, the episode record, the Autopilot envelope, the
//!    execution lock and the audit log are this test's own disposable state.
//! 2. The only goal ever *run* is `--target 0%`, which is already met on any
//!    real volume, so the loop returns `TargetReached` at step 2 before
//!    discovery — the same lever `tests/recovery_goal_cli.rs` and
//!    `tests/execution_lock_wiring.rs` use.
//! 3. The stored default goal this file sets is deliberately low, and is
//!    therefore only ever passed to `free` together with `--dry-run`, which
//!    takes no lock and mutates nothing. A low goal on a real run would delete
//!    from the machine running the suite.
//!
//! A plausible used-percent goal (`60`, say) must never be handed to a run in
//! this file for the same reason it must not appear in `recovery_goal_cli.rs`:
//! it would be a genuine improvement on a real disk, and so a genuine deletion.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use glomeris::monitor::{
    load_tracker_at, persistence::unix_now_secs, save_tracker_at, EpisodeConfig,
};

/// A low alert threshold, so that the real volume this suite runs on is over it.
///
/// Any `/` on a Mac is more than 2% used, and `settings` refuses anything below
/// 1, so this is the closest thing to "certainly crossed" the validated range
/// allows. It is a *threshold*, not a goal: nothing is deleted for crossing it.
const NOTIFY_AT: &str = "2";

/// The stored default recovery goal. Must stay below the threshold, and is never
/// run — see the note at the top of this file.
const DEFAULT_GOAL: &str = "1";

const GIB: u64 = 1_073_741_824;

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

/// A `HOME` of this invocation's own, for the reason
/// `tests/recovery_goal_cli.rs` documents: the execution lock lives under it, so
/// two `--target 0%` runs sharing one would have the loser exit 75 `busy` and
/// fail a test that was asserting something else entirely. It also detaches
/// these runs from the developer's real settings, grant and episode record.
fn make_temp_home() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-pressure-chain-{}-{}-{}",
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

fn run_in(home: &Path, command: &str, args: &[&str]) -> Run {
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

fn json_of(run: &Run) -> serde_json::Value {
    serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("parse error {e}; stdout: {}", run.stdout))
}

/// Stores the two settings this chain starts from, and returns the `HOME` they
/// live under.
fn home_with_settings() -> PathBuf {
    let home = make_temp_home();
    let set = run_in(
        &home,
        "settings",
        &[
            "set",
            "--notify-at-used-percent",
            NOTIFY_AT,
            "--default-goal-used-percent",
            DEFAULT_GOAL,
        ],
    );
    assert_eq!(set.code, Some(0), "stderr: {}", set.stderr);
    home
}

fn pressure_show(home: &Path) -> serde_json::Value {
    let run = run_in(home, "pressure", &["show", "--json"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    json_of(&run)
}

fn f64_at(report: &serde_json::Value, path: &[&str]) -> f64 {
    let mut cursor = report;
    for key in path {
        cursor = &cursor[*key];
    }
    cursor
        .as_f64()
        .unwrap_or_else(|| panic!("{path:?} must be a number: {report}"))
}

fn state_path_of(report: &serde_json::Value) -> PathBuf {
    PathBuf::from(
        report["state_path"]
            .as_str()
            .unwrap_or_else(|| panic!("the surface must publish where it reads state: {report}")),
    )
}

/// Opens an episode the way the daemon does, and returns its id.
///
/// The threshold and the path both come from the report the CLI just printed, so
/// this cannot open an episode the CLI would judge against different numbers or
/// look for somewhere else.
fn open_episode(report: &serde_json::Value, used_percent: f64) -> u64 {
    let state_path = state_path_of(report);
    let notify_at = f64_at(report, &["notify_at_used_percent"]);
    let mut tracker = load_tracker_at(&state_path, EpisodeConfig::new(notify_at));
    let outcome = tracker.observe(used_percent, 4 * GIB, unix_now_secs());
    save_tracker_at(&state_path, &tracker).expect("write the episode record");
    outcome.opened.unwrap_or_else(|| {
        panic!("observing {used_percent}% against a {notify_at}% threshold must open an episode")
    })
}

/// Observes a recovered volume, the way the daemon does.
fn close_episode(report: &serde_json::Value) -> Option<u64> {
    let state_path = state_path_of(report);
    let notify_at = f64_at(report, &["notify_at_used_percent"]);
    let clear_at = f64_at(report, &["clear_at_used_percent"]);
    let mut tracker = load_tracker_at(&state_path, EpisodeConfig::new(notify_at));
    let outcome = tracker.observe((clear_at - 1.0).max(0.0), 400 * GIB, unix_now_secs());
    save_tracker_at(&state_path, &tracker).expect("write the episode record");
    outcome.closed
}

/// Puts the chain in the state the app is in when the user presses
/// "Review & recover": episode open, banner raised, answer recorded.
fn home_with_an_answered_alert() -> PathBuf {
    let home = home_with_settings();
    open_episode(&pressure_show(&home), 90.0);
    let notified = run_in(&home, "pressure", &["notified", "--json"]);
    assert_eq!(notified.code, Some(0), "stderr: {}", notified.stderr);
    let answered = run_in(
        &home,
        "pressure",
        &["respond", "review_and_recover", "--json"],
    );
    assert_eq!(answered.code, Some(0), "stderr: {}", answered.stderr);
    home
}

fn grant(home: &Path, extra: &[&str]) {
    let mut args = vec!["enable", "--kinds", "node_modules", "--max-actions", "1"];
    args.extend_from_slice(extra);
    let enable = run_in(home, "autopilot", &args);
    assert_eq!(enable.code, Some(0), "stderr: {}", enable.stderr);
}

// --- Leg 1: the threshold ---

/// Crossing the threshold is not itself an episode, and the surface says so
/// rather than filling the gap.
///
/// The distinction is the whole of AC 1's safety: an app that read
/// `threshold_crossed` as "raise a banner" would raise one on every poll for as
/// long as the disk stayed full, because that field is a comparison and carries
/// no memory. The episode is the record with memory, and until the daemon has
/// opened one there is nothing to notify about.
#[test]
fn a_crossed_threshold_is_not_yet_an_episode_to_notify_about() {
    let home = home_with_settings();
    let report = pressure_show(&home);

    assert_eq!(
        report["threshold_crossed"], true,
        "the volume running this suite must be over a {NOTIFY_AT}% used threshold: {report}"
    );
    assert!(
        report["episode"].is_null(),
        "no daemon has observed anything yet, so there is no episode: {report}"
    );
    assert_eq!(
        report["notification_due"], false,
        "a notification is owed by an episode, never by the comparison alone: {report}"
    );

    // The three answers come from the producer, so a client never has to know
    // them independently — which is what stops it offering a fourth button and
    // finding out only after the user pressed it.
    assert_eq!(
        report["responses"],
        serde_json::json!(["review_and_recover", "remind_later", "ignore_episode"]),
        "got: {report}"
    );
    assert!(
        state_path_of(&report).is_absolute(),
        "the app has to know where the record it polls lives: {report}"
    );
}

// --- Leg 2: the notification ---

/// The daemon's record becomes a banner the app can raise once and then answer,
/// and each verb moves the record on rather than repeating it.
#[test]
fn an_open_episode_becomes_one_banner_and_one_answer() {
    let home = home_with_settings();
    let opened_id = open_episode(&pressure_show(&home), 90.0);

    let owed = pressure_show(&home);
    assert_eq!(
        owed["notification_due"], true,
        "an episode nobody has been told about owes a notification: {owed}"
    );
    assert_eq!(owed["episode"]["episode_id"], opened_id, "got: {owed}");
    assert_eq!(owed["episode"]["notifications_raised"], 0, "got: {owed}");

    let notified = run_in(&home, "pressure", &["notified", "--json"]);
    assert_eq!(notified.code, Some(0), "stderr: {}", notified.stderr);
    let after = json_of(&notified);
    assert_eq!(after["episode"]["notifications_raised"], 1, "got: {after}");
    assert_eq!(
        after["notification_due"], false,
        "the banner is on screen, so it is not owed again: {after}"
    );
    assert!(
        after["episode"]["response"].is_null(),
        "showing a banner is not answering it: {after}"
    );

    let answered = run_in(
        &home,
        "pressure",
        &["respond", "review_and_recover", "--json"],
    );
    assert_eq!(answered.code, Some(0), "stderr: {}", answered.stderr);
    let recorded = json_of(&answered);
    assert_eq!(
        recorded["episode"]["response"], "review_and_recover",
        "got: {recorded}"
    );
    assert_eq!(
        recorded["episode"]["episode_id"], opened_id,
        "the answer belongs to the episode that was raised, not to a new one: {recorded}"
    );
}

/// "Remind me later" defers the same episode instead of answering it, which is
/// the difference between a snooze and a dismissal.
#[test]
fn remind_later_defers_the_same_episode_rather_than_closing_it() {
    let home = home_with_settings();
    let opened_id = open_episode(&pressure_show(&home), 90.0);
    let notified = run_in(&home, "pressure", &["notified", "--json"]);
    assert_eq!(notified.code, Some(0), "stderr: {}", notified.stderr);

    let snoozed = run_in(&home, "pressure", &["respond", "remind_later", "--json"]);
    assert_eq!(snoozed.code, Some(0), "stderr: {}", snoozed.stderr);
    let report = json_of(&snoozed);

    assert_eq!(report["episode"]["episode_id"], opened_id, "got: {report}");
    assert_eq!(report["episode"]["is_snoozed"], true, "got: {report}");
    assert!(
        report["episode"]["snoozed_until_unix_secs"].is_u64(),
        "a snooze with no end is a dismissal: {report}"
    );
    assert_eq!(
        report["notification_due"], false,
        "nothing is owed while the snooze stands: {report}"
    );
}

/// An answer with nothing open is refused with the exit code that means
/// "understood, nothing to record" — not 1, which an app polling every thirty
/// seconds would surface as a fault, and not 0, which would claim an answer was
/// filed.
#[test]
fn an_answer_with_no_open_episode_is_refused_not_recorded() {
    let home = home_with_settings();
    let run = run_in(
        &home,
        "pressure",
        &["respond", "review_and_recover", "--json"],
    );

    assert_eq!(run.code, Some(3), "stderr: {}", run.stderr);
    let report = json_of(&run);
    assert_eq!(report["reason"], "no_open_episode", "got: {report}");
    assert!(
        !report["message"].as_str().unwrap_or_default().is_empty(),
        "a refusal has to be sayable to a user: {report}"
    );
}

// --- Leg 3: the handoff ---

/// The goal the alert publishes is the goal `free` accepts, in one wording.
///
/// This is the join AC 1 is really about. The alert's "Review & recover" has to
/// arrive at the Recovery surface carrying a target, and if the alert published
/// one number while `free` rendered another the user would be shown a goal that
/// is not the goal about to be worked toward. Both come from
/// `RecoveryGoalReport`, and this asserts the two really are the same producer.
#[test]
fn the_goal_the_alert_publishes_is_the_goal_free_accepts() {
    let home = home_with_settings();
    let report = pressure_show(&home);
    let published = &report["default_goal"];
    let used = f64_at(&report, &["default_goal", "used_percent"]);

    // `--dry-run` because this goal is deliberately low: a real run toward it
    // would delete from the machine running the suite.
    let preview = run_in(
        &home,
        "free",
        &[
            "--goal-used-percent",
            &format!("{used}"),
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(preview.code, Some(0), "stderr: {}", preview.stderr);
    let previewed = json_of(&preview);

    assert_eq!(
        previewed["goal"], *published,
        "the alert and the recovery surface must describe one goal, not two: {previewed}"
    );
    assert!(
        published["description"]
            .as_str()
            .unwrap_or_default()
            .contains("used"),
        "a percentage with no axis is the ambiguity this whole contract exists to remove: {report}"
    );
}

/// Answering the alert is not permission for a run nobody watched start.
///
/// The user who pressed "Review & recover" is present, and a run they started
/// needs only the envelope. An unprompted run needs the second consent as well —
/// and the refusal has to name that one, because telling someone who has already
/// enabled Autopilot to enable Autopilot is advice they cannot act on.
#[test]
fn answering_the_alert_admits_a_run_the_user_asked_for_and_not_one_they_did_not() {
    let home = home_with_an_answered_alert();
    grant(&home, &[]);

    let asked = run_in(&home, "free", &["--target", "0%", "--autopilot"]);
    assert_eq!(
        asked.code,
        Some(0),
        "a run the user started under a grant must proceed: {}",
        asked.stderr
    );

    let unasked = run_in(
        &home,
        "free",
        &["--target", "0%", "--autopilot", "--unattended"],
    );
    assert_eq!(unasked.code, Some(3), "stderr: {}", unasked.stderr);
    assert!(
        unasked.stderr.contains("--respond-to-alerts"),
        "the refusal must name the consent that is missing: {}",
        unasked.stderr
    );
    assert!(
        unasked.stdout.is_empty(),
        "an unauthorized run must print no report: {}",
        unasked.stdout
    );
}

// --- Leg 4: the loop, and how it stops ---

/// With both consents the chain completes, and the run says why it stopped in a
/// token a client can branch on — never a bare "Done".
///
/// `--target 0%` is already met, so this is the loop's own termination at step 2
/// rather than a deletion: `target_reached`, nothing executed, nothing reclaimed,
/// and `target_met` decided from the re-measured filesystem rather than from the
/// sum of what the run did.
#[test]
fn the_unprompted_run_completes_the_chain_and_states_why_it_stopped() {
    let home = home_with_an_answered_alert();
    grant(&home, &["--respond-to-alerts"]);

    let run = run_in(
        &home,
        "free",
        &["--target", "0%", "--autopilot", "--unattended", "--json"],
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let report = json_of(&run);

    assert_eq!(report["stop_reason"], "target_reached", "got: {report}");
    assert!(
        !report["stop_reason_detail"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "every stop has to be explainable in a sentence: {report}"
    );
    assert_eq!(report["target_met"], true, "got: {report}");
    assert_eq!(
        report["actions_executed"], 0,
        "an already-met target must stop before it deletes anything: {report}"
    );
    assert_eq!(
        report["bytes_freed_measured"], 0,
        "nothing was deleted, so nothing may be reported as reclaimed: {report}"
    );
    assert!(
        report["envelope_refusal"].is_null(),
        "the envelope admitted this run; it refused nothing: {report}"
    );

    // The run states the authority it acted under, which for a run nobody was
    // present to see start is the only record that it was authorized at all.
    assert!(
        run.stderr.contains("Autopilot envelope:"),
        "stderr: {}",
        run.stderr
    );
}

/// A recovered volume closes the episode, so the next crossing is a new episode
/// rather than a second notification for the old one.
///
/// This is the end of the chain and the reason it is a loop rather than a line:
/// the record that made the banner possible has to be retired by the recovery,
/// or the same episode is answered forever and a genuinely new one can never be
/// told apart from it.
#[test]
fn a_recovered_volume_closes_the_episode_and_the_next_crossing_opens_a_new_one() {
    let home = home_with_settings();
    let first = open_episode(&pressure_show(&home), 90.0);

    let closed = close_episode(&pressure_show(&home));
    assert_eq!(
        closed,
        Some(first),
        "observing a recovered volume must close the episode it opened"
    );

    let quiet = pressure_show(&home);
    assert!(
        quiet["episode"].is_null(),
        "a closed episode is not a current one: {quiet}"
    );
    assert_eq!(
        quiet["notification_due"], false,
        "nothing is owed once the episode is closed: {quiet}"
    );

    let refused = run_in(
        &home,
        "pressure",
        &["respond", "review_and_recover", "--json"],
    );
    assert_eq!(
        refused.code,
        Some(3),
        "there is nothing left to answer: {}",
        refused.stderr
    );

    let second = open_episode(&pressure_show(&home), 90.0);
    assert!(
        second > first,
        "a later crossing is its own episode ({second} must be a new id after {first})"
    );
}
