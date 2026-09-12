//! HORO-957 regression proof: `--project-root` actually reaches the real
//! detectors through the exact path `main.rs` uses (parse args ->
//! `DiscoveryContext` -> `DetectorRegistry::builtin()`), not just through a
//! `DiscoveryContext` built directly by a unit test.
//!
//! Before this fix, `DiscoveryContext::known_project_roots` was never
//! populated by any real CLI code path — only by unit tests calling
//! `with_known_project_roots` directly. This test proves the missing link:
//! parsing `--project-root <path>` from a raw CLI argument list (via
//! `glomeris::cli::extract_project_roots`, the same function `main.rs`
//! calls for every `detect`/`explain`/`clean`/`free` subcommand) and
//! feeding the result into a real `DiscoveryContext` now makes the real
//! cargo/node detectors actually find fixtures that would otherwise be
//! structurally undiscoverable.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use glomeris::cli::extract_project_roots;
use glomeris::detectors::{DetectorId, DetectorRegistry, DetectorStatus, DiscoveryContext};

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

/// Builds a project-root fixture containing both a Cargo `target/` dir and
/// a `node_modules/` dir — the two "realistic developer storage hotspots"
/// this whole ticket is about — with non-trivial file content so the
/// shallow size probe has something real to sum.
fn make_project_root_fixture() -> PathBuf {
    let root = make_temp_dir("horo957-project-root");

    let target_dir = root.join("target/debug");
    fs::create_dir_all(&target_dir).expect("create cargo target dir");
    fs::write(target_dir.join("build_output.bin"), vec![0u8; 4096]).expect("write cargo fixture");

    let node_modules_dir = root.join("node_modules");
    fs::create_dir_all(&node_modules_dir).expect("create node_modules dir");
    fs::write(node_modules_dir.join("some-package.js"), vec![0xABu8; 4096])
        .expect("write node fixture");

    root
}

#[test]
fn project_root_flag_parsed_from_cli_args_makes_real_detectors_find_fixtures() {
    let project_root = make_project_root_fixture();
    let empty_home = make_temp_dir("horo957-empty-home");

    // Simulate exactly what `main.rs` does with a raw argument list for
    // e.g. `glomeris detect --project-root <fixture> --json`.
    let raw_args: Vec<String> = vec![
        "--project-root".to_string(),
        project_root.to_string_lossy().into_owned(),
        "--json".to_string(),
    ];
    let (roots, remaining) =
        extract_project_roots(&raw_args).expect("well-formed --project-root flag must parse");
    assert_eq!(roots, vec![project_root.clone()]);
    assert_eq!(remaining, vec!["--json".to_string()]);

    let ctx = DiscoveryContext::new(&empty_home).with_known_project_roots(roots);
    let registry = DetectorRegistry::builtin();
    let results = registry.discover_all(&ctx);

    let (_, cargo_status) = results
        .iter()
        .find(|(id, _)| *id == DetectorId("cargo_target_dir"))
        .expect("cargo_target_dir detector must be registered");
    match cargo_status {
        DetectorStatus::Found(evidence) => assert_eq!(evidence.len(), 1),
        other => panic!("expected the real cargo detector to find the fixture, got {other:?}"),
    }

    let (_, node_status) = results
        .iter()
        .find(|(id, _)| *id == DetectorId("node_modules"))
        .expect("node_modules detector must be registered");
    match node_status {
        DetectorStatus::Found(evidence) => assert_eq!(evidence.len(), 1),
        other => panic!("expected the real node detector to find the fixture, got {other:?}"),
    }

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&empty_home).ok();
}

/// The other half of the regression proof: with no `--project-root` flag
/// at all (empty `known_project_roots`, matching every real CLI invocation
/// before this fix), the exact same fixture is NOT discovered — this is
/// the bug HORO-957 fixes: it was structurally impossible to discover a
/// cargo `target/` or `node_modules/` dir through the shipped CLI no
/// matter what was actually on disk.
#[test]
fn without_project_root_flag_the_same_fixture_is_not_discovered() {
    let project_root = make_project_root_fixture();
    let empty_home = make_temp_dir("horo957-empty-home-negative");

    let raw_args: Vec<String> = vec!["--json".to_string()];
    let (roots, remaining) = extract_project_roots(&raw_args).expect("well-formed args must parse");
    assert!(roots.is_empty());
    assert_eq!(remaining, raw_args);

    let ctx = DiscoveryContext::new(&empty_home).with_known_project_roots(roots);
    let registry = DetectorRegistry::builtin();
    let results = registry.discover_all(&ctx);

    let (_, cargo_status) = results
        .iter()
        .find(|(id, _)| *id == DetectorId("cargo_target_dir"))
        .expect("cargo_target_dir detector must be registered");
    assert_eq!(
        *cargo_status,
        DetectorStatus::ToolAbsent,
        "expected no known_project_roots to leave the cargo detector unable to find anything, \
         even though a real target/ dir exists on disk at {}",
        project_root.display()
    );

    let (_, node_status) = results
        .iter()
        .find(|(id, _)| *id == DetectorId("node_modules"))
        .expect("node_modules detector must be registered");
    assert_eq!(
        *node_status,
        DetectorStatus::ToolAbsent,
        "expected no known_project_roots to leave the node detector unable to find anything, \
         even though a real node_modules/ dir exists on disk at {}",
        project_root.display()
    );

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&empty_home).ok();
}
