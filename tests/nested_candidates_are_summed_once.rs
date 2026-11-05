//! HORO-2360 end-to-end: a candidate that lives inside another candidate is
//! reported as contained, and containment is report-only.
//!
//! Real discovery over a real directory tree: project A's `target/` holds
//! project B (with its own `Cargo.toml` and `target/`) below it, so B's target
//! is a candidate inside A's. The test pins that `detect` names the enclosing
//! candidate, that nothing else about either candidate changes because of it,
//! and that a candidate with nothing around it carries no `contained_in`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use glomeris::actions::ActionRegistry;
use glomeris::cli::{build_detect_report, discover_and_classify_pass};
use glomeris::detectors::{DetectorRegistry, DiscoveryContext};
use glomeris::evidence::correlate::{CorrelationResult, EvidenceCollector, ProbeBudget};
use glomeris::evidence::{ProbeOutcome, ProbeReason, ResourceId};
use glomeris::policy::PolicyConfig;
use glomeris::reporting::dto::DetectCandidateReport;
use glomeris::reporting::ImpactContext;

fn temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "glomeris-{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    // Canonical so candidate paths compare equal to what discovery reports.
    dir.canonicalize().unwrap()
}

fn not_attempted<T>() -> ProbeOutcome<T> {
    ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
}

struct NoopCollector;
impl EvidenceCollector for NoopCollector {
    fn collect(&self, _resource: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
        CorrelationResult {
            open_by_process: not_attempted(),
            process_cwd_match: not_attempted(),
            git_state: not_attempted(),
            tool_liveness: not_attempted(),
            executable_dependency: not_attempted(),
        }
    }
}

fn project(root: &Path, payload: usize) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"p\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("target/debug")).unwrap();
    fs::write(root.join("target/debug/artifact"), vec![1u8; payload]).unwrap();
}

fn detect(roots: Vec<PathBuf>, home: &Path) -> Vec<DetectCandidateReport> {
    let ctx = DiscoveryContext::new(home).with_known_project_roots(roots.clone());
    let pass = discover_and_classify_pass(
        &DetectorRegistry::builtin(),
        &ctx,
        &NoopCollector,
        &PolicyConfig::default(),
        SystemTime::now(),
        |_| {},
    );
    build_detect_report(
        &pass.candidates,
        &pass.detectors,
        &ActionRegistry::builtin(),
        ImpactContext::default(),
        &[],
        roots.len(),
        pass.uncorrelated,
    )
    .candidates
    .into_iter()
    .filter(|c| c.kind == "cargo_target_dir")
    .collect()
}

#[test]
fn a_target_inside_another_target_names_its_enclosing_candidate() {
    let base = temp_dir("nested-candidates");
    let home = temp_dir("nested-candidates-home");
    let outer = base.join("outer");
    project(&outer, 8192);
    let inner = outer.join("target/debug/inner");
    project(&inner, 4096);

    let candidates = detect(vec![outer.clone(), inner.clone()], &home);

    let outer_id = format!("cargo_target_dir:{}", outer.join("target").display());
    let inner_id = format!("cargo_target_dir:{}", inner.join("target").display());
    let find = |id: &str| {
        candidates
            .iter()
            .find(|c| c.resource_id == id)
            .unwrap_or_else(|| panic!("{id} not discovered in {candidates:#?}"))
    };
    assert_eq!(find(&outer_id).contained_in, None);
    assert_eq!(
        find(&inner_id).contained_in.as_deref(),
        Some(outer_id.as_str())
    );

    fs::remove_dir_all(&base).ok();
    fs::remove_dir_all(&home).ok();
}

/// Containment is a display pointer: the inner candidate's label, reasons,
/// executability and size are exactly what they are when it is discovered on
/// its own.
#[test]
fn containment_changes_nothing_about_the_nested_candidates_own_projection() {
    let base = temp_dir("nested-invariance");
    let home = temp_dir("nested-invariance-home");
    let outer = base.join("outer");
    project(&outer, 8192);
    let inner = outer.join("target/debug/inner");
    project(&inner, 4096);
    let inner_id = format!("cargo_target_dir:{}", inner.join("target").display());

    let together = detect(vec![outer.clone(), inner.clone()], &home);
    let alone = detect(vec![inner.clone()], &home);

    let mut nested = together
        .into_iter()
        .find(|c| c.resource_id == inner_id)
        .expect("inner discovered with its enclosing project");
    let standalone = alone
        .into_iter()
        .find(|c| c.resource_id == inner_id)
        .expect("inner discovered alone");
    assert!(nested.contained_in.is_some());
    assert!(standalone.contained_in.is_none());
    nested.contained_in = None;
    assert_eq!(nested, standalone);

    fs::remove_dir_all(&base).ok();
    fs::remove_dir_all(&home).ok();
}
