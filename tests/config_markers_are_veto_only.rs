//! HORO-2359, real-subprocess proof: Config's `.ca-keep` marker and owner
//! receipt can only make Glomeris more careful (ADR-0001 §18).
//!
//! The real `glomeris` binary runs against a disposable Cargo project with
//! `HOME` set to a temp dir, so no developer data is read. Nothing here calls
//! `policy::classify` directly, and the marker is never hand-set on an
//! `Evidence`: the discovery path that production uses is what finds it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

fn glomeris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_glomeris")
}

fn make_temp_home(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-horo2359-{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp HOME");
    dir.canonicalize().expect("canonicalize temp HOME")
}

/// A real Cargo project with build output, returned as (project root, target).
fn make_cargo_fixture(home: &Path) -> (PathBuf, PathBuf) {
    let project = home.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"horo2359-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(project.join("src/lib.rs"), "").unwrap();
    let target = project.join("target");
    std::fs::create_dir_all(target.join("debug")).unwrap();
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(target.join("debug/artifact"), vec![0u8; 4096]).unwrap();
    (
        project.canonicalize().unwrap(),
        target.canonicalize().unwrap(),
    )
}

fn run(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(glomeris_bin())
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn glomeris")
}

fn explain(project: &Path, target: &Path, home: &Path) -> serde_json::Value {
    let out = run(
        &[
            "explain",
            &target.to_string_lossy(),
            "--project-root",
            &project.to_string_lossy(),
            "--json",
        ],
        home,
    );
    assert!(
        out.status.success(),
        "explain failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("explain --json must be JSON")
}

fn reasons(report: &serde_json::Value) -> Vec<String> {
    report["reasons"]
        .as_array()
        .expect("reasons array")
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

/// Non-vacuity baseline: without a marker, neither new reason appears, so the
/// assertions below are about the marker and not about the fixture.
#[test]
fn a_target_without_a_marker_gets_neither_new_reason() {
    let home = make_temp_home("baseline");
    let (project, target) = make_cargo_fixture(&home);

    let report = explain(&project, &target, &home);

    let reasons = reasons(&report);
    assert_ne!(report["policy_label"], "PROTECTED", "{report}");
    for new in [
        "protected_keep_marker",
        "owner_receipt_present",
        "protective_marker_unverified",
    ] {
        assert!(
            !reasons.contains(&new.to_string()),
            "{new} appeared without a marker: {report}"
        );
    }
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn a_keep_marker_makes_the_target_protected() {
    let home = make_temp_home("keep");
    let (project, target) = make_cargo_fixture(&home);
    std::fs::write(target.join(".ca-keep"), b"").unwrap();

    let report = explain(&project, &target, &home);

    assert_eq!(report["policy_label"], "PROTECTED", "{report}");
    assert_eq!(reasons(&report), vec!["protected_keep_marker"], "{report}");
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn a_keep_marker_one_level_down_makes_the_target_protected() {
    let home = make_temp_home("keep-child");
    let (project, target) = make_cargo_fixture(&home);
    std::fs::create_dir_all(target.join("release")).unwrap();
    std::fs::write(target.join("release/.ca-keep"), b"").unwrap();

    let report = explain(&project, &target, &home);

    assert_eq!(report["policy_label"], "PROTECTED", "{report}");
    std::fs::remove_dir_all(&home).ok();
}

/// Even a receipt claiming every autonomy-raising field is only ever a reason
/// to stop: it is never read, and the class is never AUTO_SAFE.
#[test]
fn an_owner_receipt_never_raises_autonomy() {
    let home = make_temp_home("receipt");
    let (project, target) = make_cargo_fixture(&home);
    std::fs::write(
        target.join(".ca-owner.json"),
        r#"{"session_id":"x","reproducible":true,"private":true,"evidence_free":true}"#,
    )
    .unwrap();

    let report = explain(&project, &target, &home);

    assert_ne!(report["policy_label"], "AUTO_SAFE", "{report}");
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn execute_refuses_a_kept_target_and_leaves_it_untouched() {
    let home = make_temp_home("execute");
    let (project, target) = make_cargo_fixture(&home);
    std::fs::write(target.join(".ca-keep"), b"").unwrap();
    let resource_id = format!("cargo_target_dir:{}", target.display());

    let out = run(
        &[
            "execute",
            "--action-id",
            "cargo.clean.target_dir",
            "--resource-id",
            &resource_id,
            "--project-root",
            &project.to_string_lossy(),
            "--json",
        ],
        &home,
    );

    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON refusal");
    assert_eq!(parsed["reason"], "protected", "{parsed}");
    assert!(
        target.join("debug/artifact").exists(),
        "the kept target must survive"
    );
    std::fs::remove_dir_all(&home).ok();
}

/// A non-Cargo resource is not affected by a stray `.ca-keep`-free fixture: the
/// check is scoped to Cargo targets, so a node_modules fixture still gets
/// neither new reason.
#[test]
fn a_non_cargo_resource_gets_no_marker_reasons() {
    let home = make_temp_home("node");
    let project = home.join("proj");
    let node_modules = project.join("node_modules");
    std::fs::create_dir_all(&node_modules).unwrap();
    std::fs::write(node_modules.join("package.json"), "{}").unwrap();
    let project = project.canonicalize().unwrap();
    let node_modules = node_modules.canonicalize().unwrap();

    let report = explain(&project, &node_modules, &home);

    for new in [
        "protected_keep_marker",
        "owner_receipt_present",
        "protective_marker_unverified",
    ] {
        assert!(!reasons(&report).contains(&new.to_string()), "{report}");
    }
    std::fs::remove_dir_all(&home).ok();
}
