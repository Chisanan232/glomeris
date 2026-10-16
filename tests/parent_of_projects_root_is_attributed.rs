//! HORO-1629 (ADR-0001 §11/§16.1) end-to-end: the exact bug scenario from
//! the ticket, reproduced through the real discovery pipeline rather than a
//! hand-assembled report.
//!
//! The bug: a configured project root that is itself a PARENT of several
//! projects (e.g. `~/code` holding 30 repos) makes
//! `cargo_target_dir`/`node_modules`/`swiftpm_build_dir` all answer
//! `Found(vec![])` — truthfully, about the one path each was handed — with
//! nothing anywhere saying that the thing examined was the parent folder
//! itself. A user reading "0 candidates, discovery complete" concludes "my
//! projects are clean" when the truth is "Glomeris looked at a path that
//! holds no build output of its own".
//!
//! This test proves both halves of the fix fire together on that exact
//! shape: `DetectReport::scan_coverage_note` names how many roots were
//! examined (option 2), and
//! `glomeris::cli::check_project_root`/`build_check_project_root_report`
//! would have flagged the same root at configuration time (option 1) —
//! wired through the real `--project-root` CLI parsing path, the same one
//! `tests/cli_project_root_wiring.rs` (HORO-957) proved for the underlying
//! detectors.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use glomeris::actions::ActionRegistry;
use glomeris::cli::{
    build_check_project_root_report, build_detect_report, check_project_root,
    discover_and_classify_pass, extract_project_roots, scan_coverage_line, ProjectRootCheck,
};
use glomeris::detectors::DetectorRegistry;
use glomeris::evidence::correlate::EvidenceCollector;
use glomeris::evidence::{ProbeOutcome, ProbeReason};
use glomeris::policy::PolicyConfig;
use glomeris::reporting::ImpactContext;

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

/// A collector that never attempts correlation — the same no-op shape
/// several other integration tests in this crate already use when only
/// discovery itself is under test.
struct NoopCollector;
impl EvidenceCollector for NoopCollector {
    fn collect(
        &self,
        _resource: &glomeris::evidence::ResourceId,
        _budget: glomeris::evidence::correlate::ProbeBudget,
    ) -> glomeris::evidence::correlate::CorrelationResult {
        glomeris::evidence::correlate::CorrelationResult {
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        }
    }
}

/// The exact ticket scenario: one configured root holding two real projects
/// one level down, each with real build output. `detect`'s scan-coverage
/// note must name the root that was examined, and `check-project-root`
/// would have flagged it before it was ever added.
#[test]
fn a_parent_of_projects_root_is_attributed_rather_than_a_silent_clean_bill_of_health() {
    let parent_root = make_temp_dir("horo1629-parent-of-projects");
    let repo_a = parent_root.join("repo-a");
    fs::create_dir_all(repo_a.join("target/debug")).unwrap();
    fs::write(repo_a.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(repo_a.join("target/debug/bin"), vec![0u8; 4096]).unwrap();

    let repo_b = parent_root.join("repo-b");
    fs::create_dir_all(repo_b.join("node_modules")).unwrap();
    fs::write(repo_b.join("package.json"), "{}").unwrap();

    // Option 1: the bounded, one-level Settings-time check would have
    // caught this before `parent_root` was ever configured as a root.
    match check_project_root(&parent_root) {
        ProjectRootCheck::ParentOfProjects {
            mut matched_children,
        } => {
            matched_children.sort();
            assert_eq!(matched_children, vec!["repo-a", "repo-b"]);
        }
        other => panic!("expected ParentOfProjects, got {other:?}"),
    }
    let settings_report = build_check_project_root_report(&parent_root);
    assert_eq!(settings_report.status, "parent_of_projects");
    assert_eq!(
        settings_report.warning,
        Some(glomeris::cli::project_root_check::PARENT_OF_PROJECTS_WARNING)
    );

    // Option 2, through the real `--project-root` CLI parsing path (same
    // entry point `tests/cli_project_root_wiring.rs` proves for the
    // detectors themselves) and the real discovery pipeline.
    let raw_args: Vec<String> = vec![
        "--project-root".to_string(),
        parent_root.to_string_lossy().into_owned(),
    ];
    let (roots, _remaining) =
        extract_project_roots(&raw_args).expect("well-formed --project-root flag must parse");
    assert_eq!(roots, vec![parent_root.clone()]);
    let known_project_root_count = roots.len();

    let empty_home = make_temp_dir("horo1629-empty-home");
    let ctx =
        glomeris::detectors::DiscoveryContext::new(&empty_home).with_known_project_roots(roots);
    let registry = DetectorRegistry::builtin();
    let collector = NoopCollector;
    let policy_cfg = PolicyConfig::default();
    let pass = discover_and_classify_pass(
        &registry,
        &ctx,
        &collector,
        &policy_cfg,
        SystemTime::now(),
        |_| {},
    );

    // The bug's exact symptom: both project-scoped detectors answer
    // `Found(vec![])`, truthfully, about the one path they were handed —
    // `<parent_root>/target` and `<parent_root>/node_modules`, neither of
    // which exists, because the real build output sits one level further
    // down than any detector is contracted to look.
    let cargo_outcome = pass
        .detectors
        .iter()
        .find(|(id, _)| id.0 == "cargo_target_dir")
        .map(|(_, outcome)| outcome)
        .expect("cargo_target_dir must be registered");
    assert_eq!(cargo_outcome.candidates_found(), 0);
    let node_outcome = pass
        .detectors
        .iter()
        .find(|(id, _)| id.0 == "node_modules")
        .map(|(_, outcome)| outcome)
        .expect("node_modules must be registered");
    assert_eq!(node_outcome.candidates_found(), 0);

    let actions = ActionRegistry::builtin();
    let report = build_detect_report(
        &pass.candidates,
        &pass.detectors,
        &actions,
        ImpactContext::default(),
        &[],
        known_project_root_count,
        pass.uncorrelated,
    );

    // This is still an honest, complete search (HORO-1576) — AC2/option 2's
    // own constraint: this field is purely additive attribution, never a
    // new completeness signal.
    assert!(report.discovery_complete);
    assert_eq!(
        report.scan_coverage_note.as_deref(),
        Some(scan_coverage_line(1).as_str()),
        "the exact bug scenario must attribute the empty result to the one \
         examined root, byte-identical to ADR-0001 §16.1's approved wording"
    );
    assert_eq!(
        report.scan_coverage_note.as_deref(),
        Some(
            "Examined 1 configured project root(s). Nested projects were not scanned \
             automatically."
        )
    );

    fs::remove_dir_all(&parent_root).ok();
    fs::remove_dir_all(&empty_home).ok();
}

/// AC2 (no regression): a correctly configured project root — pointed
/// DIRECTLY at the one project, not at its parent — reports exactly the
/// resources and byte counts it reported before this change. The
/// scan-coverage note may still appear (node/swiftpm legitimately find
/// nothing under a Rust-only project), but the cargo candidate itself, and
/// its byte count, must be untouched.
#[test]
fn a_correctly_configured_root_reports_unchanged_candidates_and_bytes() {
    let project_root = make_temp_dir("horo1629-correctly-configured");
    let target_dir = project_root.join("target/debug");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(project_root.join("Cargo.toml"), "[package]\n").unwrap();
    fs::write(target_dir.join("artifact.bin"), vec![0u8; 8192]).unwrap();

    let empty_home = make_temp_dir("horo1629-empty-home-ac2");
    let ctx = glomeris::detectors::DiscoveryContext::new(&empty_home)
        .with_known_project_roots(vec![project_root.clone()]);
    let registry = DetectorRegistry::builtin();
    let collector = NoopCollector;
    let policy_cfg = PolicyConfig::default();
    let pass = discover_and_classify_pass(
        &registry,
        &ctx,
        &collector,
        &policy_cfg,
        SystemTime::now(),
        |_| {},
    );

    let actions = ActionRegistry::builtin();
    let report = build_detect_report(
        &pass.candidates,
        &pass.detectors,
        &actions,
        ImpactContext::default(),
        &[],
        1,
        pass.uncorrelated,
    );

    let cargo_candidate = report
        .candidates
        .iter()
        .find(|c| c.kind == "cargo_target_dir")
        .expect("the real cargo target must still be discovered");
    assert_eq!(cargo_candidate.reclaimable_bytes, Some(8192));
    assert!(!cargo_candidate.reclaimable_bytes_is_lower_bound);

    // And `check_project_root` must never flag the project's own root: its
    // own `Cargo.toml` wins over anything else, per the function's own
    // documented precedence.
    assert_eq!(
        check_project_root(&project_root),
        ProjectRootCheck::ProjectRoot
    );

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&empty_home).ok();
}
