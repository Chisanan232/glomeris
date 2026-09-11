//! Proof that HORO-992 actually closes the gap it targets: a REAL
//! detector, run against a REAL disposable fixture directory, produces
//! `Evidence::reclaimable_bytes: Observed(_)` with a plausible non-zero
//! value, reaches `Completeness::Complete` once correlation is supplied,
//! and `policy::classify` reaches `PolicyClass::AutoSafe` on that
//! evidence. A synthetic hand-built `Evidence` proving `AutoSafe` is
//! reachable in principle is already covered elsewhere (see
//! `policy::engine::tests`) — this test is the one that proves it against
//! a genuine detector's genuine output, not a hand-built stand-in.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use glomeris::detectors::{DetectorId, DetectorRegistry, DetectorStatus, DiscoveryContext};
use glomeris::evidence::correlate::{
    CorrelationResult, EvidenceCollector, GitProbe, OpenFileProbe, ProbeBudget, ProcessCwdProbe,
    ToolLivenessProbe,
};
use glomeris::evidence::{
    Completeness, Evidence, GitState, OwningTool, ProbeOutcome, ProcessRef, ResourceId,
};
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

/// A fake [`EvidenceCollector`] that reports the cleanest possible
/// correlation state: no process has the resource open, no process's cwd
/// matches it, it is not inside a git working tree at all, and its owning
/// tool is not live. This is exactly the "clean, no active use" signal
/// `policy::classify` needs to reach `AutoSafe` — it does not fabricate
/// evidence about the resource's *size*, only about correlation, and the
/// size evidence under test comes entirely from the real detector.
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

/// Runs the real `cargo_target_dir` detector (via the real builtin
/// registry, never a hand-constructed `Evidence`) against a real fixture
/// `target/` directory, correlates it with a clean fake collector, and
/// asserts the full chain — detector -> completeness -> policy — actually
/// reaches `AutoSafe` end to end.
#[test]
fn real_cargo_detector_evidence_reaches_auto_safe_with_clean_correlation() {
    let project_root = make_temp_dir("reclaimable-proof-cargo");
    let target_dir = project_root.join("target");
    fs::create_dir_all(&target_dir).expect("create target dir");

    // Real files directly at the top level of `target/`: `shallow_logical_bytes`
    // is deliberately non-recursive (sums only immediate directory
    // entries), so content must live at this level for the probe to see
    // it — a nested file would be invisible to the shallow probe and this
    // assertion would spuriously pass on a near-zero value.
    const FILE_BYTES: usize = 64 * 1024;
    fs::write(target_dir.join("libfoo.rlib"), vec![0xABu8; FILE_BYTES])
        .expect("write fixture file");
    fs::write(target_dir.join("libbar.rlib"), vec![0xCDu8; FILE_BYTES])
        .expect("write fixture file");
    // A nested subdirectory too, for realism — its contents are expected
    // to be under-counted by the shallow probe, which is exactly the
    // documented lower-bound behavior.
    fs::create_dir_all(target_dir.join("debug/deps")).expect("create nested dir");
    fs::write(
        target_dir.join("debug/deps/some_dep-abcdef"),
        vec![0xEFu8; FILE_BYTES],
    )
    .expect("write nested fixture file");

    let empty_home = make_temp_dir("reclaimable-proof-home");
    let ctx =
        DiscoveryContext::new(&empty_home).with_known_project_roots(vec![project_root.clone()]);

    let registry = DetectorRegistry::builtin();
    let results = registry.discover_all(&ctx);

    let (_, cargo_status) = results
        .into_iter()
        .find(|(id, _)| *id == DetectorId("cargo_target_dir"))
        .expect("cargo_target_dir detector must be registered");

    let mut evidence: Evidence = match cargo_status {
        DetectorStatus::Found(mut evidence) => {
            assert_eq!(
                evidence.len(),
                1,
                "expected exactly one candidate for the one known project root"
            );
            evidence.remove(0)
        }
        other => {
            panic!("expected the real cargo detector to find the fixture target dir, got {other:?}")
        }
    };

    // 1. The real detector must have actually observed a plausible,
    // non-zero reclaimable size — not merely a non-crashing placeholder.
    match evidence.reclaimable_bytes {
        ProbeOutcome::Observed(bytes) => {
            assert!(
                bytes >= (2 * FILE_BYTES) as u64,
                "expected reclaimable_bytes to cover at least the two top-level \
                 fixture files ({} bytes), got {bytes}",
                2 * FILE_BYTES
            );
        }
        ProbeOutcome::Unavailable(reason) => {
            panic!("expected reclaimable_bytes to be Observed, got Unavailable({reason:?})")
        }
    }
    // The equivalence this ticket documents: for a fully-owned,
    // regenerable build directory, reclaimable_bytes and logical_bytes are
    // computed identically.
    assert_eq!(evidence.reclaimable_bytes, evidence.logical_bytes);

    // 2. Supply clean correlation via a fake EvidenceCollector (HORO-949's
    // seam) — this is the only part of the evidence that is not the real
    // detector's own output, and it only ever asserts "nothing is
    // actively using this resource", never a fabricated size.
    let collector = CleanFakeEvidenceCollector;
    let correlation = collector.collect(
        &evidence.resource,
        ProbeBudget {
            timeout: Duration::from_secs(1),
        },
    );
    glomeris::evidence::correlate::merge_into(&mut evidence, correlation);

    assert_eq!(
        evidence.completeness(),
        Completeness::Complete,
        "expected real detector output + clean correlation to reach \
         Completeness::Complete; evidence: {evidence:?}"
    );

    // 3. The full policy chain, on real evidence, reaches AutoSafe.
    let now = SystemTime::now();
    let decision = classify(&evidence, &PolicyConfig::default(), now);
    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "expected AutoSafe, got {:?} with reasons {:?}",
        decision.class,
        decision.reasons
    );

    fs::remove_dir_all(&project_root).ok();
    fs::remove_dir_all(&empty_home).ok();
}
