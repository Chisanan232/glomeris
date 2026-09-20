//! HORO-1297 regression: `glomeris daemon status --json` must put exactly one
//! JSON document on stdout and nothing else — *including* while the launch
//! agent is loaded, which is the state every user reaches immediately after
//! `daemon install`.
//!
//! The defect was that `platform::macos::launchd::run_launchctl` built its
//! child with `.status()`, which **inherits** the parent's stdout, so
//! `launchctl list <label>`'s property dictionary landed on Glomeris's own
//! stdout ahead of the report. It only reproduced while loaded, because the
//! not-loaded path writes to stderr instead — which is why it survived to
//! dogfooding.
//!
//! `cargo test` cannot load a real launch agent (the module's own docs say
//! so), and a test that only reproduced under a real `launchctl` would not
//! protect `main`. So these tests put a **fake `launchctl` first on the
//! child's `PATH`** and drive every branch that matters: loaded, not
//! loaded, absent entirely, and deliberately hostile output. That is
//! stricter than real `launchctl`, not weaker — the fake emits the property
//! dictionary unconditionally, so a regression fails here on every run
//! rather than only on a machine that happens to have the agent loaded.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

const LABEL: &str = "com.glomeris.monitor";

/// The four keys `DaemonStatusReport` serializes. Asserting on the exact
/// set — not just "contains" — is what catches leading launchctl output:
/// extra top-level keys mean something else got onto stdout.
const REPORT_KEYS: [&str; 4] = [
    "plist_installed",
    "plist_path",
    "loaded",
    "heartbeat_age_secs",
];

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn unique_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-daemon-status-purity-{tag}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A realistic `launchctl list <label>` property dictionary. Verbatim in
/// shape from the real output observed on macOS 15.7.7 while reproducing
/// HORO-1297 — tab-indented, `=`-separated, trailing `};` — with the paths
/// replaced by placeholders. Its keys double as the leak markers the
/// assertions below look for.
const LAUNCHCTL_LIST_DICT: &str = "{
\t\"StandardOutPath\" = \"/example/Logs/Glomeris/monitor.log\";
\t\"LimitLoadToSessionType\" = \"Aqua\";
\t\"StandardErrorPath\" = \"/example/Logs/Glomeris/monitor.err.log\";
\t\"Label\" = \"com.glomeris.monitor\";
\t\"OnDemand\" = true;
\t\"LastExitStatus\" = 0;
\t\"PID\" = 4242;
\t\"Program\" = \"/example/bin/glomeris\";
};";

/// Marker substrings that may only ever appear in the *fake launchctl's*
/// output. If any reaches Glomeris's own stdout, the child's stream was
/// inherited rather than captured.
const LEAK_MARKERS: [&str; 4] = [
    "StandardOutPath",
    "LimitLoadToSessionType",
    "LastExitStatus",
    "OnDemand",
];

/// Writes an executable fake `launchctl` into a fresh directory and returns
/// that directory, ready to be prepended to a child's `PATH`.
///
/// The script dispatches on the subcommand so one fake serves both
/// `status` (`launchctl list`) and `uninstall` (`launchctl unload`):
///
/// - `list` reproduces the defect's trigger — a property dictionary on
///   **stdout**, exiting `list_exit`.
/// - `unload` reproduces the secondary complaint in the ticket — the
///   `Unload failed: 5` / "try `launchctl bootout` as root" pair on
///   stderr, exiting `1`, exactly as real `launchctl` does for an
///   already-unloaded per-user agent.
fn fake_launchctl(tag: &str, list_stdout: &str, list_stderr: &str, list_exit: i32) -> PathBuf {
    let dir = unique_dir(tag);
    let script_path = dir.join("launchctl");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  list)
    printf '%s\n' '{list_stdout}'
    printf '%s\n' '{list_stderr}' >&2
    exit {list_exit}
    ;;
  unload)
    echo 'Unload failed: 5: Input/output error' >&2
    echo 'Try running launchctl bootout as root for richer errors.' >&2
    exit 1
    ;;
  load)
    exit 0
    ;;
esac
exit 0
"#
    );
    std::fs::write(&script_path, script).expect("write fake launchctl");
    set_executable(&script_path);
    dir
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .expect("stat fake launchctl")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod fake launchctl");
}

/// A temp `$HOME` with the launch-agent plist present, so
/// `plist_installed` is `true` and the interesting field is `loaded`.
fn home_with_plist(tag: &str) -> PathBuf {
    let home = unique_dir(tag);
    let agents = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).expect("create LaunchAgents dir");
    std::fs::write(
        agents.join(format!("{LABEL}.plist")),
        "<?xml version=\"1.0\"?>\n",
    )
    .expect("write placeholder plist");
    home
}

fn write_heartbeat(home: &Path, last_poll_unix_secs: u64) {
    let dir = home
        .join("Library")
        .join("Application Support")
        .join("Glomeris");
    std::fs::create_dir_all(&dir).expect("create support dir");
    let json = format!(
        r#"{{"last_poll_unix_secs":{last_poll_unix_secs},"state":"HEALTHY","used_percent":71.6,"free_bytes":123456789}}"#
    );
    std::fs::write(dir.join("heartbeat.json"), json).expect("write heartbeat");
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_secs()
}

/// Runs the real binary with `$HOME` and `$PATH` fully controlled. `PATH`
/// deliberately contains `path_dirs` first and then the standard system
/// directories, so the fake `launchctl` wins while `/bin/sh` (which the
/// fake itself needs) still resolves.
fn run_glomeris(args: &[&str], home: &Path, path_dirs: &[&Path]) -> std::process::Output {
    let mut path = path_dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>();
    path.push("/usr/bin".to_string());
    path.push("/bin".to_string());

    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .env("PATH", path.join(":"))
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

/// Asserts stdout is exactly one JSON object carrying exactly the report's
/// keys, and returns it. Every failure message includes the raw stdout,
/// because the whole point of this defect was extra bytes on that stream.
fn parse_report(output: &std::process::Output) -> serde_json::Map<String, serde_json::Value> {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    for marker in LEAK_MARKERS {
        assert!(
            !stdout.contains(marker),
            "launchctl's own output reached Glomeris's stdout (found {marker:?}); \
             the child's stream was inherited instead of captured. stdout was:\n{stdout}"
        );
    }

    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout did not parse as a single JSON document ({e}). stdout was:\n{stdout}")
    });
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object, got:\n{stdout}"))
        .clone();

    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = REPORT_KEYS;
    expected.sort_unstable();
    assert_eq!(
        keys,
        expected.to_vec(),
        "report had unexpected top-level keys; stdout was:\n{stdout}"
    );

    object
}

#[test]
fn loaded_agent_still_yields_exactly_one_json_document_on_stdout() {
    let home = home_with_plist("loaded-home");
    let bin = fake_launchctl("loaded-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(
        output.status.success(),
        "daemon status must exit 0 while loaded, got {:?}; stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_report(&output);
    assert_eq!(
        report["loaded"],
        serde_json::Value::Bool(true),
        "a launchctl list that exits 0 means loaded"
    );
    assert_eq!(report["plist_installed"], serde_json::Value::Bool(true));
}

#[test]
fn unloaded_agent_yields_clean_json_with_loaded_false() {
    let home = home_with_plist("unloaded-home");
    // Real `launchctl list` for a missing service: message on stderr,
    // nothing on stdout, non-zero exit.
    let bin = fake_launchctl(
        "unloaded-bin",
        "",
        "Could not find service \"com.glomeris.monitor\" in domain for port",
        113,
    );

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(output.status.success(), "daemon status must still exit 0");
    let report = parse_report(&output);
    assert_eq!(report["loaded"], serde_json::Value::Bool(false));
    assert_eq!(
        report["plist_installed"],
        serde_json::Value::Bool(true),
        "the plist is on disk even though the agent is not loaded — the two \
         fields are independent and must not collapse"
    );
}

#[test]
fn absent_launchctl_reports_not_loaded_rather_than_failing() {
    let home = home_with_plist("absent-home");
    // An empty directory: no `launchctl` anywhere on the child's PATH.
    let empty = unique_dir("absent-bin");

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&empty]);

    assert!(
        output.status.success(),
        "an unreachable launchctl is reported as loaded:false, never surfaced as failure"
    );
    let report = parse_report(&output);
    assert_eq!(report["loaded"], serde_json::Value::Bool(false));
}

#[test]
fn hostile_launchctl_output_cannot_corrupt_the_report() {
    let home = home_with_plist("hostile-home");
    // Not paranoia for its own sake: this is the failure mode that makes a
    // "just strip the leading dictionary" fix wrong. A child writing a
    // *plausible* report of its own could otherwise be mistaken for the
    // real one by any recovery heuristic. Capturing is the only fix that
    // holds.
    let bin = fake_launchctl(
        "hostile-bin",
        r#"{"plist_installed": false, "loaded": false, "heartbeat_age_secs": 99999}"#,
        "",
        0,
    );

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(
        report["loaded"],
        serde_json::Value::Bool(true),
        "the report must come from Glomeris's own state, not from anything the child printed"
    );
    assert_eq!(report["plist_installed"], serde_json::Value::Bool(true));
    assert_eq!(
        report["heartbeat_age_secs"],
        serde_json::Value::Null,
        "no heartbeat file was written, so the age must be null — not the child's 99999"
    );
}

#[test]
fn missing_heartbeat_is_null_not_an_error() {
    let home = home_with_plist("no-heartbeat-home");
    let bin = fake_launchctl("no-heartbeat-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["heartbeat_age_secs"], serde_json::Value::Null);
    assert_eq!(
        report["loaded"],
        serde_json::Value::Bool(true),
        "loaded and heartbeat are independent: loaded with no heartbeat yet is a real state"
    );
}

#[test]
fn fresh_heartbeat_reports_a_small_age() {
    let home = home_with_plist("fresh-heartbeat-home");
    write_heartbeat(&home, now_unix_secs());
    let bin = fake_launchctl("fresh-heartbeat-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(output.status.success());
    let report = parse_report(&output);
    let age = report["heartbeat_age_secs"]
        .as_u64()
        .expect("a heartbeat file was written, so the age must be a number");
    assert!(
        age < 120,
        "a heartbeat stamped now must read as fresh, got {age}s"
    );
}

#[test]
fn stale_heartbeat_reports_a_large_age_rather_than_hiding_it() {
    let home = home_with_plist("stale-heartbeat-home");
    // Two hours old: the agent claims to be loaded but has not polled in
    // 120 poll intervals. Surfacing the number is the whole point — the
    // report must never collapse "loaded" and "recently polled" into one
    // healthy flag.
    write_heartbeat(&home, now_unix_secs() - 7200);
    let bin = fake_launchctl("stale-heartbeat-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(output.status.success());
    let report = parse_report(&output);
    let age = report["heartbeat_age_secs"]
        .as_u64()
        .expect("age must be a number");
    assert!(
        age >= 7200,
        "a two-hour-old heartbeat must report >= 7200s, got {age}s"
    );
    assert_eq!(
        report["loaded"],
        serde_json::Value::Bool(true),
        "loaded stays true — the staleness is carried by the age, not by flipping loaded"
    );
}

#[test]
fn heartbeat_stamped_in_the_future_saturates_to_zero_rather_than_underflowing() {
    let home = home_with_plist("future-heartbeat-home");
    // Clock skew, or a heartbeat restored from a backup. `u64` subtraction
    // must not wrap into a nonsensical multi-billion-second age.
    write_heartbeat(&home, now_unix_secs() + 3600);
    let bin = fake_launchctl("future-heartbeat-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(
        report["heartbeat_age_secs"]
            .as_u64()
            .expect("age is a number"),
        0,
        "a future-stamped heartbeat must saturate to 0"
    );
}

#[test]
fn malformed_heartbeat_file_is_reported_as_no_heartbeat_not_a_crash() {
    let home = home_with_plist("malformed-heartbeat-home");
    let dir = home
        .join("Library")
        .join("Application Support")
        .join("Glomeris");
    std::fs::create_dir_all(&dir).expect("create support dir");
    std::fs::write(dir.join("heartbeat.json"), "{ this is not json").expect("write garbage");
    let bin = fake_launchctl("malformed-heartbeat-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "status", "--json"], &home, &[&bin]);

    assert!(
        output.status.success(),
        "an unreadable heartbeat must not take the whole command down"
    );
    let report = parse_report(&output);
    assert_eq!(
        report["heartbeat_age_secs"],
        serde_json::Value::Null,
        "an unparseable heartbeat reads as absent — never as a fabricated age"
    );
}

#[test]
fn uninstall_does_not_echo_launchctls_misleading_unload_advice() {
    let home = home_with_plist("uninstall-home");
    let bin = fake_launchctl("uninstall-bin", LAUNCHCTL_LIST_DICT, "", 0);

    let output = run_glomeris(&["daemon", "uninstall"], &home, &[&bin]);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "uninstall is idempotent and must exit 0; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("uninstalled launch agent"),
        "the success line must still be printed; stdout was:\n{stdout}"
    );
    // The ticket's secondary complaint: `launchctl`'s own failure text
    // contradicted the success line one line later, and advised running a
    // command as root that a per-user LaunchAgent never needs.
    assert!(
        !combined.contains("Unload failed"),
        "launchctl's unload failure text must not be echoed; output was:\n{combined}"
    );
    assert!(
        !combined.contains("bootout"),
        "advising `launchctl bootout` as root is wrong for a per-user agent \
         and must not be surfaced; output was:\n{combined}"
    );
}
