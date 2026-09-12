//! HORO-1017 regression: the full discover -> policy -> execute round
//! trip against a REAL fixture where a Cargo project's `target/` is a
//! symlink to a physically different directory that sits right next to
//! an UNRELATED ("decoy") project's `Cargo.toml`.
//!
//! Before the fix, `CargoCleanTargetDir::plan()` derived the manifest
//! path from the canonicalized (symlink-resolved) target dir's parent —
//! which is the decoy project's directory here — so the rendered
//! `--manifest-path` argument (and, had the decoy manifest been
//! malformed, the actual `cargo clean` invocation) would have targeted
//! the WRONG project. This test proves the real execution path now binds
//! to the real project's manifest and that `execute()` actually succeeds
//! and removes the real `target/` fixture, using the same real
//! detector -> classify -> authorize -> execute chain as
//! `tests/golden_chain_execute.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use glomeris::actions::ActionRegistry;
use glomeris::detectors::{DetectorId, DetectorRegistry, DetectorStatus, DiscoveryContext};
use glomeris::evidence::correlate::{
    CorrelationResult, EvidenceCollector, GitProbe, OpenFileProbe, ProbeBudget, ProcessCwdProbe,
    ToolLivenessProbe,
};
use glomeris::evidence::{Evidence, GitState, OwningTool, ProbeOutcome, ProcessRef, ResourceId};
use glomeris::executor::{execute, ExecutionOutcome};
use glomeris::policy::approval::authorize;
use glomeris::policy::{classify, PolicyClass, PolicyConfig};

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

/// Same clean-correlation fake used by `golden_chain_execute.rs`.
struct CleanFakeEvidenceCollector;

impl OpenFileProbe for CleanFakeEvidenceCollector {
    fn processes_with_open_files_under(
        &self,
        _path: &Path,
        _timeout: Duration,
    ) -> ProbeOutcome<Vec<ProcessRef>> {
        ProbeOutcome::Observed(Vec::new())
    }
}

impl ProcessCwdProbe for CleanFakeEvidenceCollector {
    fn processes_with_cwd_under(
        &self,
        _path: &Path,
        _timeout: Duration,
    ) -> ProbeOutcome<Vec<ProcessRef>> {
        ProbeOutcome::Observed(Vec::new())
    }
}

impl GitProbe for CleanFakeEvidenceCollector {
    fn state_of(&self, _path: &Path, _timeout: Duration) -> ProbeOutcome<Option<GitState>> {
        ProbeOutcome::Observed(None)
    }
}

impl ToolLivenessProbe for CleanFakeEvidenceCollector {
    fn is_running(&self, _tool: OwningTool, _timeout: Duration) -> ProbeOutcome<bool> {
        ProbeOutcome::Observed(false)
    }
}

impl EvidenceCollector for CleanFakeEvidenceCollector {
    fn collect(&self, id: &ResourceId, budget: ProbeBudget) -> CorrelationResult {
        CorrelationResult {
            open_by_process: self.processes_with_open_files_under(
                path_of(id).unwrap_or_else(|| Path::new("")),
                budget.timeout,
            ),
            process_cwd_match: self.processes_with_cwd_under(
                path_of(id).unwrap_or_else(|| Path::new("")),
                budget.timeout,
            ),
            git_state: self.state_of(path_of(id).unwrap_or_else(|| Path::new("")), budget.timeout),
            tool_liveness: self.is_running(id.kind.owning_tool(), budget.timeout),
        }
    }
}

fn path_of(id: &ResourceId) -> Option<&Path> {
    match &id.locator {
        glomeris::evidence::ResourceLocator::Path(p) => Some(p.as_path()),
        glomeris::evidence::ResourceLocator::Tool { .. } => None,
    }
}

#[test]
fn symlinked_target_with_decoy_neighbor_executes_against_the_real_manifest() {
    // Real project: has its own Cargo.toml, its `target/` is a symlink.
    let real_root = make_temp_dir("horo1017-real-project");
    fs::write(
        real_root.join("Cargo.toml"),
        "[package]\nname = \"glomeris-real-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write real Cargo.toml");
    fs::create_dir_all(real_root.join("src")).expect("create real src dir");
    fs::write(real_root.join("src/lib.rs"), "").expect("write real lib.rs");

    // Physical target directory lives elsewhere, right next to a decoy
    // project's Cargo.toml.
    let physical_container = make_temp_dir("horo1017-physical-container");
    let physical_target = physical_container.join("target");
    fs::create_dir_all(physical_target.join("debug")).expect("create physical target dir");
    fs::write(
        physical_target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("write CACHEDIR.TAG");
    fs::write(
        physical_target.join("debug/build_output.bin"),
        vec![0u8; 4096],
    )
    .expect("write fixture build output");
    fs::write(
        physical_container.join("Cargo.toml"),
        "[package]\nname = \"glomeris-decoy-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write decoy Cargo.toml");

    // real_root/target -> physical_target
    let symlinked_target = real_root.join("target");
    std::os::unix::fs::symlink(&physical_target, &symlinked_target).expect("create target symlink");

    let empty_home = make_temp_dir("horo1017-home");
    let ctx = DiscoveryContext::new(&empty_home).with_known_project_roots(vec![real_root.clone()]);

    let registry = DetectorRegistry::builtin();
    let results = registry.discover_all(&ctx);
    let (_, cargo_status) = results
        .into_iter()
        .find(|(id, _)| *id == DetectorId("cargo_target_dir"))
        .expect("cargo_target_dir detector must be registered");

    let mut evidence: Evidence = match cargo_status {
        DetectorStatus::Found(mut evidence) => evidence.remove(0),
        other => panic!("expected the real cargo detector to find the fixture, got {other:?}"),
    };

    assert_eq!(
        evidence.resource.source_project_root.as_deref(),
        Some(real_root.as_path()),
        "discovered evidence must carry the real project root, not the physical target's parent"
    );

    let collector = CleanFakeEvidenceCollector;
    let correlation = collector.collect(
        &evidence.resource,
        ProbeBudget {
            timeout: Duration::from_secs(1),
        },
    );
    glomeris::evidence::correlate::merge_into(&mut evidence, correlation);

    let now = SystemTime::now();
    let cfg = PolicyConfig::default();
    let decision = classify(&evidence, &cfg, now);
    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "expected AutoSafe, got {:?} with reasons {:?}",
        decision.class,
        decision.reasons
    );

    let approval = authorize(decision, evidence.fingerprint.clone(), None)
        .expect("AutoSafe must always authorize");

    let actions = ActionRegistry::builtin();
    let action = actions
        .find_for_kind(evidence.resource.kind)
        .expect("a cargo action must be registered for CargoTargetDir");

    let report = execute(action, &approval, &collector, &cfg, now);

    match &report.outcome {
        ExecutionOutcome::Succeeded => {}
        other => panic!(
            "expected the round trip to reach ExecutionOutcome::Succeeded against the real \
             project's manifest, got {other:?} — if execution failed because it tried to use \
             the decoy manifest, this is exactly the HORO-1017 regression"
        ),
    }

    assert!(
        !physical_target.exists(),
        "expected the real (physical) target dir to actually be removed by cargo clean"
    );
    assert!(
        physical_container.join("Cargo.toml").is_file(),
        "the decoy project's Cargo.toml must be untouched"
    );
    assert!(
        real_root.join("Cargo.toml").is_file(),
        "the real project's Cargo.toml must be untouched (only target/ is cleaned)"
    );

    fs::remove_dir_all(&real_root).ok();
    fs::remove_dir_all(&physical_container).ok();
    fs::remove_dir_all(&empty_home).ok();
}
