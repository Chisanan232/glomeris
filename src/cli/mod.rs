//! Thin CLI orchestration (HORO-955) for `glomeris status|detect|explain|
//! clean`. Every function here only calls already-existing evidence/
//! policy/action APIs — no policy/evidence/execution logic is duplicated
//! here, only wired together and projected into
//! [`crate::reporting`]'s report DTOs.
//!
//! `glomeris scan`/`free`/`emergency`/`daemon` are unaffected and stay
//! wired directly in `main.rs`, per this ticket's scope.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::actions::{Action, ActionRegistry};
use crate::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Evidence, NativeCleanup, ResourceLocator};
use crate::executor::dry_run;
use crate::monitor::{FsUsage, ThresholdConfig};
use crate::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision};
use crate::reporting::dto::{
    CleanDryRunItem, CleanDryRunReport, DetectCandidateReport, DetectReport, ExplainReport,
    StatusReport,
};

/// Correlation-refresh timeout for one candidate at the CLI layer. Mirrors
/// `executor::recovery_loop::CANDIDATE_CORRELATION_TIMEOUT`'s reasoning
/// (that constant is private to its module, so this is a separate,
/// intentionally short budget for an interactive CLI command rather than
/// a background recovery loop).
const CLI_CORRELATION_TIMEOUT: Duration = Duration::from_secs(3);

/// Builds a [`StatusReport`] from a raw [`FsUsage`] reading and the
/// pressure thresholds it's classified against. Pure — no I/O.
pub fn build_status_report(usage: &FsUsage, thresholds: &ThresholdConfig) -> StatusReport {
    let used_percent = usage.used_percent();
    let pressure_state = thresholds.classify(used_percent, usage.free_bytes).as_str();

    StatusReport {
        total_bytes: usage.total_bytes,
        free_bytes: usage.free_bytes,
        used_percent,
        free_human: crate::reporting::human_bytes(usage.free_bytes),
        total_human: crate::reporting::human_bytes(usage.total_bytes),
        pressure_state,
    }
}

/// Discovers every candidate `registry` finds under `ctx`, refreshes each
/// one's correlation fields via `collector`, and classifies it — the same
/// discover-refresh-classify shape
/// `executor::recovery_loop::select_candidate` uses internally,
/// reimplemented at this thin CLI layer only because that function (and
/// its module-private timeout constant) is not `pub`. Detector-level
/// `ToolAbsent`/`Failed` statuses are silently dropped here exactly as
/// they are in the recovery loop and emergency mode — a tool being absent
/// is normal, expected state, never an error a CLI report needs to
/// surface per-candidate.
pub fn discover_and_classify(
    registry: &DetectorRegistry,
    ctx: &DiscoveryContext,
    collector: &dyn EvidenceCollector,
    policy_cfg: &PolicyConfig,
    now: SystemTime,
) -> Vec<(Evidence, PolicyDecision)> {
    registry
        .discover_all(ctx)
        .into_iter()
        .filter_map(|(_, status)| match status {
            DetectorStatus::Found(evidences) => Some(evidences),
            DetectorStatus::ToolAbsent | DetectorStatus::Failed(_) => None,
        })
        .flatten()
        .map(|mut ev| {
            let correlation = collector.collect(
                &ev.resource,
                ProbeBudget {
                    timeout: CLI_CORRELATION_TIMEOUT,
                },
            );
            merge_into(&mut ev, correlation);
            ev.collected_at = now;
            let decision = classify(&ev, policy_cfg, now);
            (ev, decision)
        })
        .collect()
}

/// Extracts every repeatable, valued `--project-root <path>` occurrence
/// from `args` (HORO-957): returns the collected roots in the order they
/// appeared, plus every other token from `args` — in its original
/// order, with the `--project-root`/value pairs removed — for the
/// caller's own subcommand-specific parser to keep handling exactly as
/// it does today. A trailing `--project-root` with no following value is
/// reported as an error rather than silently dropped or left for the
/// caller's parser to stumble over, mirroring how `clean`'s own
/// `--target` parsing already reports a missing value.
///
/// Never validates or canonicalizes a collected path — a
/// nonexistent-looking root is passed through as-is; the detectors'
/// own real filesystem probes already degrade gracefully (`ToolAbsent`/
/// empty results) for a root that isn't there.
pub fn extract_project_roots(args: &[String]) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let mut roots = Vec::new();
    let mut remaining = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--project-root" {
            match args.get(i + 1) {
                Some(value) => {
                    roots.push(PathBuf::from(value));
                    i += 2;
                }
                None => return Err("--project-root requires a value".to_string()),
            }
        } else {
            remaining.push(args[i].clone());
            i += 1;
        }
    }
    Ok((roots, remaining))
}

/// Finds the candidate whose [`crate::evidence::ResourceId::to_string`]
/// output, or whose raw filesystem path, matches `query` exactly. Accepts
/// either form per this ticket's `glomeris explain`/`clean --target` AC.
pub fn find_candidate<'a>(
    query: &str,
    candidates: &'a [(Evidence, PolicyDecision)],
) -> Option<&'a (Evidence, PolicyDecision)> {
    candidates.iter().find(|(ev, _)| {
        if ev.resource.to_string() == query {
            return true;
        }
        match &ev.resource.locator {
            ResourceLocator::Path(p) => p.to_string_lossy() == query,
            ResourceLocator::Tool { .. } => false,
        }
    })
}

/// Builds a [`DetectReport`] from already discovered-and-classified
/// candidates.
pub fn build_detect_report(candidates: &[(Evidence, PolicyDecision)]) -> DetectReport {
    DetectReport {
        candidates: candidates
            .iter()
            .map(|(ev, d)| DetectCandidateReport::from_evidence_and_decision(ev, d))
            .collect(),
    }
}

/// Builds an [`ExplainReport`] for one already-classified candidate.
pub fn build_explain_report(ev: &Evidence, decision: &PolicyDecision) -> ExplainReport {
    ExplainReport::from_evidence_and_decision(ev, decision)
}

/// Resolves the [`Action`] to dry-run for `ev`: prefers
/// [`crate::evidence::model::NativeCleanup::Available`] when it names a
/// real registered action, otherwise falls back to
/// [`ActionRegistry::find_for_kind`] — mirroring
/// `executor::recovery_loop::resolve_action_id`'s exact fallback
/// reasoning (that function is private to its module).
pub fn resolve_action_for<'a>(
    ev: &Evidence,
    actions: &'a ActionRegistry,
) -> Option<&'a dyn Action> {
    if let NativeCleanup::Available(id) = ev.native_cleanup {
        if let Some(action) = actions.get(id.0) {
            return Some(action);
        }
    }
    actions.find_for_kind(ev.resource.kind)
}

/// Builds one [`CleanDryRunItem`] for `ev`/`decision`: resolves an action
/// and renders its [`crate::actions::ActionPlan::explain`] via
/// [`crate::actions::dry_run`] — never executing anything. A resource
/// with no resolvable action, or whose `Action::plan` refuses (e.g.
/// `ActionError::Unsupported`), is reported with `skip_reason` set rather
/// than silently omitted.
///
/// SAFETY-CRITICAL: a [`PolicyClass::Protected`] decision short-circuits
/// here, before `resolve_action_for`/`dry_run` ever run — this is what
/// makes "Protected items never render an executable cleanup action" (this
/// ticket's AC) true even for `clean --dry-run --target <protected>`,
/// which bypasses `build_clean_dry_run_report`'s own `AutoSafe | Ask`
/// filter (that filter only applies to the no-`--target` "dry-run
/// everything" path).
pub fn build_clean_dry_run_item(
    ev: &Evidence,
    decision: &PolicyDecision,
    actions: &ActionRegistry,
) -> CleanDryRunItem {
    let policy_label = crate::reporting::label_for(decision).as_str();
    let resource_id = ev.resource.to_string();

    if decision.class == PolicyClass::Protected {
        return CleanDryRunItem {
            resource_id,
            policy_label,
            action_id: None,
            explain: None,
            skip_reason: Some(
                "PROTECTED — no cleanup action is ever rendered for this resource".to_string(),
            ),
        };
    }

    match resolve_action_for(ev, actions) {
        Some(action) => match dry_run(action, ev) {
            Ok(plan) => CleanDryRunItem {
                resource_id,
                policy_label,
                action_id: Some(action.id().0),
                explain: Some(plan.explain),
                skip_reason: None,
            },
            Err(e) => CleanDryRunItem {
                resource_id,
                policy_label,
                action_id: Some(action.id().0),
                explain: None,
                skip_reason: Some(format!("{e:?}")),
            },
        },
        None => CleanDryRunItem {
            resource_id,
            policy_label,
            action_id: None,
            explain: None,
            skip_reason: Some("no registered action for this resource kind".to_string()),
        },
    }
}

/// Builds a [`CleanDryRunReport`]: without `target`, dry-runs every
/// [`PolicyClass::AutoSafe`]/[`PolicyClass::Ask`] candidate (skipping
/// `Protected`, which is never actionable); with `target`, dry-runs just
/// the one candidate [`find_candidate`] resolves it to, erroring if none
/// matches. Never executes anything — see [`build_clean_dry_run_item`].
pub fn build_clean_dry_run_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
    target: Option<&str>,
) -> Result<CleanDryRunReport, String> {
    let selected: Vec<&(Evidence, PolicyDecision)> = match target {
        Some(query) => {
            let found = find_candidate(query, candidates)
                .ok_or_else(|| format!("no discoverable candidate matches '{query}'"))?;
            vec![found]
        }
        None => candidates
            .iter()
            .filter(|(_, d)| matches!(d.class, PolicyClass::AutoSafe | PolicyClass::Ask))
            .collect(),
    };

    Ok(CleanDryRunReport {
        items: selected
            .into_iter()
            .map(|(ev, d)| build_clean_dry_run_item(ev, d, actions))
            .collect(),
    })
}

/// Prints a [`StatusReport`] as concise, human-readable text.
pub fn print_status_report(report: &StatusReport) {
    println!("disk pressure state: {}", report.pressure_state);
    println!(
        "used: {:.1}%   free: {} ({} bytes)   total: {} ({} bytes)",
        report.used_percent,
        report.free_human,
        report.free_bytes,
        report.total_human,
        report.total_bytes
    );
}

/// Prints a [`DetectReport`] as concise, human-readable text.
pub fn print_detect_report(report: &DetectReport) {
    if report.candidates.is_empty() {
        println!("no candidates discovered");
        return;
    }
    for c in &report.candidates {
        let size = c.reclaimable_human.as_deref().unwrap_or("unknown");
        println!(
            "[{}] {:<28} {:<10} reclaimable(est.)={:<10} reasons={}",
            c.policy_label,
            c.kind,
            c.resource_id,
            size,
            c.reasons.join(",")
        );
    }
}

/// Prints an [`ExplainReport`] as a multi-line human-readable explanation.
pub fn print_explain_report(report: &ExplainReport) {
    println!("resource:      {}", report.resource_id);
    println!("kind:          {}", report.kind);
    println!("detector:      {}", report.detector);
    if report.sources.is_empty() {
        println!("provenance:    none recorded");
    } else {
        println!("provenance:");
        for source in &report.sources {
            println!("  - {source}");
        }
    }
    println!(
        "logical size:  {}",
        report.logical_human.as_deref().unwrap_or("unknown")
    );
    println!(
        "reclaimable:   {} (estimate, not a measured result)",
        report.reclaimable_human.as_deref().unwrap_or("unknown")
    );
    println!("completeness:  {}", report.completeness);
    println!("confidence:    {}", report.confidence);
    println!("regenerable:   {}", report.regenerability);
    if report.active_use_signals.is_empty() {
        println!("active use:    none observed");
    } else {
        println!("active use:");
        for signal in &report.active_use_signals {
            println!("  - {signal}");
        }
    }
    println!(
        "native cleanup: {}",
        match report.native_cleanup_action_id {
            Some(id) => format!("available ({id})"),
            None => "unsupported".to_string(),
        }
    );
    println!(
        "policy:        [{}] {}",
        report.policy_label,
        report.reasons.join(", ")
    );
}

/// Prints a [`CleanDryRunReport`] as concise, human-readable text. Always
/// opens with an explicit "DRY RUN" banner — this ticket's AC requires
/// dry-run and real-execution output to be visually/semantically distinct,
/// and `free --target`'s real-execution report
/// (`main.rs::print_recovery_report`) never prints this banner.
pub fn print_clean_dry_run_report(report: &CleanDryRunReport) {
    println!("DRY RUN — nothing is executed by this command");
    if report.items.is_empty() {
        println!("no candidates to dry-run");
        return;
    }
    for item in &report.items {
        println!("[{}] {}", item.policy_label, item.resource_id);
        match (&item.explain, &item.skip_reason) {
            (Some(explain), _) => println!("  {explain}"),
            (None, Some(reason)) => println!("  skipped: {reason}"),
            (None, None) => println!("  (no plan rendered)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::detectors::{Detector, DetectorId};
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        ActionId, GitState, Recoverability, ResourceFingerprint, ResourceId, ResourceKind,
    };
    use crate::evidence::{ProbeOutcome, ProbeReason};

    fn evidence(path: &str, kind: ResourceKind, reclaimable: Option<u64>) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(reclaimable.unwrap_or(0)),
            physical_bytes: None,
            reclaimable_bytes: match reclaimable {
                Some(b) => ProbeOutcome::Observed(b),
                None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            },
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    struct FakeDetector(Vec<Evidence>);

    impl Detector for FakeDetector {
        fn id(&self) -> DetectorId {
            DetectorId("fake_cli_test_detector")
        }

        fn resource_kinds(&self) -> &'static [ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            if self.0.is_empty() {
                DetectorStatus::ToolAbsent
            } else {
                DetectorStatus::Found(self.0.clone())
            }
        }
    }

    struct CleanCollector;
    impl EvidenceCollector for CleanCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    fn ctx() -> DiscoveryContext {
        DiscoveryContext::new(PathBuf::from("/nonexistent-glomeris-cli-test-home"))
    }

    #[test]
    fn build_status_report_reflects_pressure_classification() {
        // Realistic disk-sized numbers: `ThresholdConfig::default()` gates
        // on absolute free bytes as well as percentage (see
        // `monitor::config`), so a tiny total/free pair like `(1000, 100)`
        // trips the multi-GiB absolute-bytes floor before the percentage
        // even matters. 200 GiB total / 28 GiB free (86% used, 14% free)
        // clears every absolute-bytes threshold and is classified purely
        // by the 85%-used `Pressured` percentage boundary.
        const GIB: u64 = 1024 * 1024 * 1024;
        let usage = FsUsage::new(200 * GIB, 28 * GIB);
        let thresholds = ThresholdConfig::default();
        let report = build_status_report(&usage, &thresholds);

        assert_eq!(report.total_bytes, 200 * GIB);
        assert_eq!(report.free_bytes, 28 * GIB);
        assert!((report.used_percent - 86.0).abs() < 0.001);
        assert_eq!(report.pressure_state, "PRESSURED");
    }

    #[test]
    fn discover_and_classify_correlates_and_classifies_every_candidate() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let registry = DetectorRegistry::from_detectors(vec![Box::new(FakeDetector(vec![ev]))]);
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let results = discover_and_classify(&registry, &ctx(), &CleanCollector, &cfg, now);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.class, PolicyClass::AutoSafe);
    }

    #[test]
    fn discover_and_classify_drops_tool_absent_detectors() {
        let registry = DetectorRegistry::from_detectors(vec![Box::new(FakeDetector(vec![]))]);
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;

        let results = discover_and_classify(&registry, &ctx(), &CleanCollector, &cfg, now);
        assert!(results.is_empty());
    }

    #[test]
    fn find_candidate_matches_by_resource_id_string() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev.clone(), decision)];

        let found = find_candidate("cargo_target_dir:/tmp/proj/target", &candidates);
        assert!(found.is_some());
    }

    #[test]
    fn find_candidate_matches_by_raw_path() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev.clone(), decision)];

        let found = find_candidate("/tmp/proj/target", &candidates);
        assert!(found.is_some());
    }

    #[test]
    fn find_candidate_returns_none_for_unknown_query() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev, decision)];

        assert!(find_candidate("/tmp/other/target", &candidates).is_none());
    }

    #[test]
    fn build_detect_report_projects_every_candidate() {
        let ev1 = evidence("/tmp/a/target", ResourceKind::CargoTargetDir, Some(100));
        let ev2 = evidence("/tmp/b/node_modules", ResourceKind::NodeModules, Some(9999));
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let candidates = vec![
            (ev1.clone(), classify(&ev1, &cfg, now)),
            (ev2.clone(), classify(&ev2, &cfg, now)),
        ];

        let report = build_detect_report(&candidates);
        assert_eq!(report.candidates.len(), 2);
    }

    #[test]
    fn resolve_action_for_prefers_native_cleanup_when_registered() {
        let mut ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        ev.native_cleanup = NativeCleanup::Available(ActionId("cargo.clean.target_dir"));
        let actions = ActionRegistry::builtin();

        let action = resolve_action_for(&ev, &actions).expect("expected a resolved action");
        assert_eq!(action.id(), ActionId("cargo.clean.target_dir"));
    }

    #[test]
    fn resolve_action_for_falls_back_to_find_for_kind() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        let actions = ActionRegistry::builtin();

        let action = resolve_action_for(&ev, &actions).expect("expected a resolved action");
        assert_eq!(action.id(), ActionId("cargo.clean.target_dir"));
    }

    #[test]
    fn resolve_action_for_returns_none_for_unregistered_kind() {
        let ev = evidence("/tmp/proj/cache", ResourceKind::DockerBuildCache, Some(1));
        let actions = ActionRegistry::builtin();
        assert!(resolve_action_for(&ev, &actions).is_none());
    }

    #[test]
    fn build_clean_dry_run_item_renders_explain_for_a_resolvable_action() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-cli-dry-run-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target_dir = dir.join("target");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let ev = evidence(
            target_dir.to_str().unwrap(),
            ResourceKind::CargoTargetDir,
            Some(4096),
        );
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let actions = ActionRegistry::builtin();

        let item = build_clean_dry_run_item(&ev, &decision, &actions);
        assert_eq!(item.action_id, Some("cargo.clean.target_dir"));
        assert!(item.explain.is_some());
        assert!(item.skip_reason.is_none());
        // Never executes: the fixture directory must still exist.
        assert!(target_dir.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_clean_dry_run_item_reports_skip_reason_for_unresolvable_action() {
        let ev = evidence(
            "/tmp/proj/build-cache",
            ResourceKind::DockerBuildCache,
            Some(1),
        );
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let actions = ActionRegistry::builtin();

        let item = build_clean_dry_run_item(&ev, &decision, &actions);
        assert!(item.action_id.is_none());
        assert!(item.explain.is_none());
        assert!(item.skip_reason.is_some());
    }

    #[test]
    fn build_clean_dry_run_report_without_target_skips_protected_candidates() {
        let mut protected_ev = evidence("/tmp/x", ResourceKind::Unknown, Some(1));
        protected_ev.resource = ResourceId::new(
            ResourceKind::Unknown,
            ResourceLocator::Path(PathBuf::from("/tmp/x")),
        );
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let candidates = vec![(protected_ev.clone(), classify(&protected_ev, &cfg, now))];
        let actions = ActionRegistry::builtin();

        let report = build_clean_dry_run_report(&candidates, &actions, None).unwrap();
        assert!(report.items.is_empty());
    }

    /// SAFETY-CRITICAL regression test: `--target` against a
    /// path-protected resource (credential material) must never render an
    /// executable cleanup action, even though a real `cargo.clean.
    /// target_dir` action WOULD otherwise resolve for `CargoTargetDir` —
    /// this is exactly the gap the no-`--target` filter alone does not
    /// cover, since `--target` bypasses that filter to select one
    /// specific candidate.
    #[test]
    fn build_clean_dry_run_report_with_target_on_protected_resource_renders_no_action() {
        let mut protected_ev = evidence(
            "/Users/x/.ssh/id_ed25519",
            ResourceKind::CargoTargetDir,
            Some(1024),
        );
        protected_ev.resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/.ssh/id_ed25519")),
        );
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let decision = classify(&protected_ev, &cfg, now);
        assert_eq!(decision.class, PolicyClass::Protected);
        let candidates = vec![(protected_ev.clone(), decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_clean_dry_run_report(&candidates, &actions, Some("/Users/x/.ssh/id_ed25519"))
                .unwrap();

        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.policy_label, "PROTECTED");
        assert!(item.action_id.is_none());
        assert!(item.explain.is_none());
        assert!(item.skip_reason.is_some());
    }

    #[test]
    fn build_clean_dry_run_report_with_unknown_target_errors() {
        let candidates: Vec<(Evidence, PolicyDecision)> = Vec::new();
        let actions = ActionRegistry::builtin();

        let result = build_clean_dry_run_report(&candidates, &actions, Some("nope"));
        assert!(result.is_err());
    }

    #[test]
    fn build_clean_dry_run_report_with_target_selects_only_that_candidate() {
        let ev_a = evidence("/tmp/a/target", ResourceKind::CargoTargetDir, Some(1));
        let ev_b = evidence("/tmp/b/target", ResourceKind::CargoTargetDir, Some(1));
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let candidates = vec![
            (ev_a.clone(), classify(&ev_a, &cfg, now)),
            (ev_b.clone(), classify(&ev_b, &cfg, now)),
        ];
        let actions = ActionRegistry::builtin();

        let report =
            build_clean_dry_run_report(&candidates, &actions, Some("/tmp/a/target")).unwrap();
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].resource_id, ev_a.resource.to_string());
    }

    #[test]
    fn extract_project_roots_with_no_flag_returns_all_args_unchanged() {
        let args = vec!["--json".to_string()];
        let (roots, remaining) = extract_project_roots(&args).unwrap();
        assert!(roots.is_empty());
        assert_eq!(remaining, args);
    }

    #[test]
    fn extract_project_roots_collects_a_single_occurrence() {
        let args = vec!["--project-root".to_string(), "/tmp/proj".to_string()];
        let (roots, remaining) = extract_project_roots(&args).unwrap();
        assert_eq!(roots, vec![PathBuf::from("/tmp/proj")]);
        assert!(remaining.is_empty());
    }

    #[test]
    fn extract_project_roots_collects_repeated_occurrences_in_order() {
        let args = vec![
            "--project-root".to_string(),
            "/tmp/a".to_string(),
            "--json".to_string(),
            "--project-root".to_string(),
            "/tmp/b".to_string(),
        ];
        let (roots, remaining) = extract_project_roots(&args).unwrap();
        assert_eq!(
            roots,
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
        assert_eq!(remaining, vec!["--json".to_string()]);
    }

    #[test]
    fn extract_project_roots_does_not_validate_nonexistent_paths() {
        let args = vec![
            "--project-root".to_string(),
            "/definitely/does/not/exist".to_string(),
        ];
        let (roots, _remaining) = extract_project_roots(&args).unwrap();
        assert_eq!(roots, vec![PathBuf::from("/definitely/does/not/exist")]);
    }

    #[test]
    fn extract_project_roots_errors_on_missing_trailing_value() {
        let args = vec!["--project-root".to_string()];
        let result = extract_project_roots(&args);
        assert!(result.is_err());
    }

    #[test]
    fn print_functions_do_not_panic_on_empty_reports() {
        print_status_report(&build_status_report(
            &FsUsage::new(100, 50),
            &ThresholdConfig::default(),
        ));
        print_detect_report(&DetectReport::default());
        print_clean_dry_run_report(&CleanDryRunReport::default());
        let ev = evidence("/tmp/x/target", ResourceKind::CargoTargetDir, None);
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        print_explain_report(&build_explain_report(&ev, &decision));
    }
}
