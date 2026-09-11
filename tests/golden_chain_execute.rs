//! Proof that HORO-994 actually closes the gap it targets: a REAL
//! detector's AutoSafe evidence, once authorized and executed for real,
//! actually completes (`ExecutionOutcome::Succeeded`), not
//! `AbortedByRevalidation` — completing the golden-scenario chain
//! discover -> policy -> execute -> real bytes freed. Before this fix,
//! `execute()`'s revalidation always rebuilt `reclaimable_bytes` as
//! `Unavailable`, which degraded completeness/reasons relative to plan
//! time and aborted every real execution unconditionally.

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

/// Same clean-correlation fake used by the HORO-992 proof test — asserts
/// only "nothing is actively using this resource", never a fabricated
/// size.
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

/// The full golden-scenario chain against a real, disposable fixture:
/// real detector -> real AutoSafe classification -> real Approval ->
/// real `execute()` -> real deletion -> real re-measured freed bytes.
#[test]
fn real_autosafe_candidate_executes_successfully_and_frees_real_bytes() {
    // Uses the `node_modules` (DeletePath) action rather than cargo's
    // (RunTool) — a RunTool step honestly reports
    // `actual_reclaimed_bytes: Unavailable` by design (there is no
    // general way to know what an external tool freed), so it can't
    // exercise this test's "real bytes were actually measured" claim.
    // DeletePath's real re-stat-before-delete accounting can.
    let project_root = make_temp_dir("golden-chain-node");
    let node_modules_dir = project_root.join("node_modules");
    fs::create_dir_all(&node_modules_dir).expect("create node_modules dir");

    const FILE_BYTES: usize = 64 * 1024;
    fs::write(
        node_modules_dir.join("some-package.js"),
        vec![0xABu8; FILE_BYTES],
    )
    .expect("write fixture file");
    fs::write(
        node_modules_dir.join("another-package.js"),
        vec![0xCDu8; FILE_BYTES],
    )
    .expect("write fixture file");

    let empty_home = make_temp_dir("golden-chain-home");
    let ctx =
        DiscoveryContext::new(&empty_home).with_known_project_roots(vec![project_root.clone()]);

    let registry = DetectorRegistry::builtin();
    let results = registry.discover_all(&ctx);
    let (_, node_status) = results
        .into_iter()
        .find(|(id, _)| *id == DetectorId("node_modules"))
        .expect("node_modules detector must be registered");

    let mut evidence: Evidence = match node_status {
        DetectorStatus::Found(mut evidence) => evidence.remove(0),
        other => panic!("expected the real node detector to find the fixture, got {other:?}"),
    };

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
        .expect("a node action must be registered for NodeModules");

    let report = execute(action, &approval, &collector, &cfg, now);

    match &report.outcome {
        ExecutionOutcome::Succeeded => {}
        other => panic!(
            "expected the golden chain to reach ExecutionOutcome::Succeeded, got {other:?} \
             (this is exactly the HORO-994 regression: revalidation degrading evidence \
             relative to plan time and aborting every real execution)"
        ),
    }

    assert!(
        !node_modules_dir.exists(),
        "expected the real node_modules/ fixture to actually be deleted"
    );

    match report.actual_reclaimed_bytes {
        ProbeOutcome::Observed(bytes) => assert!(
            bytes > 0,
            "expected a real non-zero actual_reclaimed_bytes measurement"
        ),
        ProbeOutcome::Unavailable(reason) => {
            panic!("expected actual_reclaimed_bytes to be Observed, got Unavailable({reason:?})")
        }
    }

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&empty_home).ok();
}
