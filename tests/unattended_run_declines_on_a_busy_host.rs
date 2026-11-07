//! HORO-2361: a run nobody asked for does not start on a loaded host.
//!
//! The ceiling is an environment override, so the test needs no real load: a
//! near-zero ceiling makes any host "busy" and a huge one makes it "idle".
//!
//! Every invocation runs with `HOME` pointed at a fresh temp directory that
//! holds no autopilot grant, so even a run that got past the gate would be
//! refused for having no grant and delete nothing. That is also what lets the
//! second test tell "declined by the load gate" (75) apart from "got past it"
//! (refused for the missing grant, anything but 75).

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glomeris-busy-gate-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME");
    dir
}

fn unattended_free(home: &PathBuf, ceiling: &str, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_glomeris"))
        .args(["free", "--autopilot", "--unattended", "--target", "0%"])
        .args(extra)
        .env("HOME", home)
        .env(glomeris::scanner::host_load::LOAD_CEILING_ENV, ceiling)
        .stdin(Stdio::null())
        .output()
        .expect("spawn glomeris")
}

#[test]
fn a_busy_host_declines_an_unattended_run_before_scanning() {
    let home = temp_home("busy");
    let out = unattended_free(&home, "0.0000001", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(75), "stderr: {stderr}");
    assert!(stderr.contains("host is busy"), "stderr: {stderr}");
    assert!(
        stderr.contains("nothing was scanned or deleted"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains(glomeris::scanner::host_load::LOAD_CEILING_ENV));
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn the_json_refusal_uses_the_existing_busy_token() {
    let home = temp_home("busy-json");
    let out = unattended_free(&home, "0.0000001", &["--json"]);
    assert_eq!(out.status.code(), Some(75));
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("a JSON refusal on stdout");
    assert_eq!(body["reason"], "busy", "body: {body}");
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn an_idle_host_is_not_declined_by_the_load_gate() {
    let home = temp_home("idle");
    let out = unattended_free(&home, "1000000", &[]);
    assert_ne!(
        out.status.code(),
        Some(75),
        "the load gate declined an idle host; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&home).ok();
}
