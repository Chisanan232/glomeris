//! CLI integration proof for HORO-1046: `glomeris history --json [--limit
//! N]` reads back the exact `history.tsv` `FilePersistence::record` already
//! writes — no new persistence format — as a bounded, oldest-first tail.
//!
//! Spawns the real `glomeris` binary with `$HOME` pointed at a disposable
//! temp directory, same `glomeris_bin()`/temp-`$HOME` idiom as
//! `execution_lock_wiring.rs`, so this exercises the actual CLI wiring
//! (argument parsing, the real `Library/Application Support/Glomeris/
//! history.tsv` path convention) rather than only the pure `cli`/
//! `monitor::persistence` unit tests.

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
        "glomeris-history-cli-test-{prefix}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME dir");
    dir
}

fn history_tsv_path(home: &std::path::Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("history.tsv")
}

fn run_history(home: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    Command::new(glomeris_bin())
        .arg("history")
        .args(extra_args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris binary")
}

#[test]
fn missing_history_file_returns_empty_list_not_an_error() {
    let home = make_temp_home("missing-file");

    let output = run_history(&home, &["--json"]);

    assert!(
        output.status.success(),
        "a missing history.tsv must not be an error; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");
    assert_eq!(
        json.get("events")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(0),
        "expected an empty events list, got: {stdout}"
    );

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn history_json_returns_at_most_the_requested_limit_most_recent_entries() {
    let home = make_temp_home("bounded-limit");
    let path = history_tsv_path(&home);
    std::fs::create_dir_all(path.parent().unwrap()).expect("create history parent dir");
    let mut contents = String::new();
    for i in 0..5u64 {
        contents.push_str(&format!(
            "{}\tHEALTHY\tWARN\t{:.2}\t{}\n",
            1_700_000_000 + i,
            50.0 + i as f64,
            1_000_000 - i
        ));
    }
    std::fs::write(&path, contents).expect("write fixture history.tsv");

    let output = run_history(&home, &["--json", "--limit", "2"]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");
    let events = json
        .get("events")
        .and_then(|v| v.as_array())
        .expect("events array");
    assert_eq!(
        events.len(),
        2,
        "expected exactly --limit entries, got: {stdout}"
    );
    assert_eq!(
        events[0]["unix_time_secs"], 1_700_000_003,
        "expected the two MOST RECENT entries, oldest-first, got: {stdout}"
    );
    assert_eq!(events[1]["unix_time_secs"], 1_700_000_004);

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn history_json_skips_a_malformed_line_without_failing() {
    let home = make_temp_home("malformed-line");
    let path = history_tsv_path(&home);
    std::fs::create_dir_all(path.parent().unwrap()).expect("create history parent dir");
    std::fs::write(
        &path,
        "1700000000\tHEALTHY\tWARN\t50.00\t1000\n\
         this line is not tab-separated history data\n\
         1700000001\tWARN\tPRESSURED\t60.00\t900\n",
    )
    .expect("write fixture history.tsv");

    let output = run_history(&home, &["--json"]);

    assert!(
        output.status.success(),
        "a malformed line must be skipped, not fatal; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");
    let events = json
        .get("events")
        .and_then(|v| v.as_array())
        .expect("events array");
    assert_eq!(
        events.len(),
        2,
        "expected the two valid lines, malformed line skipped, got: {stdout}"
    );
    assert_eq!(events[0]["unix_time_secs"], 1_700_000_000);
    assert_eq!(events[1]["unix_time_secs"], 1_700_000_001);

    std::fs::remove_dir_all(&home).ok();
}
