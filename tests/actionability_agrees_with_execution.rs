//! HORO-1358: what Glomeris *offers* must agree with what execution can
//! actually do.
//!
//! The founder was shown a Homebrew download-cache candidate labelled
//! `AUTO_SAFE` with an enabled Clean button. Pressing Clean failed
//! immediately and deterministically: `brew cleanup -s` takes no path to
//! scope, so its step carries `scoped_path: None`, and the executor refuses
//! an unscoped mutating step on sight — correctly, since there is nothing
//! for deletion-time identity revalidation to check.
//!
//! The executor was right. The *offer* was wrong. These tests pin the
//! agreement between the two, from the reporting layer the menu-bar client
//! reads (`executable` / `offered_actions` / `refusal_reason`) down to the
//! executor's own rule.
//!
//! Nothing here touches the real Homebrew cache, the real `/opt/homebrew`
//! prefix, or spawns `brew`. Every fixture is a temporary directory, and the
//! one test that reaches `execute` is the one proving execution refuses
//! *before* mutating anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glomeris::actions::{Action, ActionRegistry};
use glomeris::evidence::model::{
    NativeCleanup, Recoverability, ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::evidence::{Evidence, ProbeOutcome, ProbeReason};
use glomeris::executor::structural_refusal;
use glomeris::policy::{classify, PolicyClass, PolicyConfig};
use glomeris::reporting::dto::{DetectCandidateReport, ExplainReport};
use glomeris::reporting::ImpactContext;

/// A unique temp dir under the OS temp dir. Mirrors the executor tests'
/// own helper rather than taking a `tempfile` dependency for two tests.
fn make_temp_dir(tag: &str) -> PathBuf {
    let unique = format!(
        "glomeris-h1358-{tag}-{}-{:?}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

/// Evidence shaped to classify as `AUTO_SAFE` — the most permissive class,
/// deliberately, so every test below proves the policy label alone never
/// buys an unscoped action an offer. [`detect_report`] asserts the class it
/// actually got rather than trusting this helper.
///
/// `collected_at` must be *now*: the policy engine's staleness gate sends
/// evidence older than `max_evidence_age` straight to `ASK`, which would
/// quietly rob every test here of the AUTO_SAFE precondition they exist to
/// test against.
fn auto_safe_evidence(kind: ResourceKind, path: &Path, reclaimable: u64) -> Evidence {
    Evidence {
        resource: ResourceId::new(kind, ResourceLocator::Path(path.to_path_buf())),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: glomeris::detectors::DetectorId("test"),
        logical_bytes: ProbeOutcome::Observed(reclaimable),
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Observed(reclaimable),
        reclaimable_bytes_is_lower_bound: false,
        last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability: kind.regenerability(),
        recoverability: Recoverability::RegenerableByTool,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Observed(Vec::new()),
        process_cwd_match: ProbeOutcome::Observed(Vec::new()),
        git_state: ProbeOutcome::Observed(None),
        tool_liveness: ProbeOutcome::Observed(false),
        collected_at: SystemTime::now(),
        sources: Vec::new(),
    }
}

/// Builds a minimal but real cargo project so `cargo.clean.target_dir` can
/// actually plan — its planner stats `Cargo.toml` and refuses without one.
/// This is the positive control for the whole file: without it, a change
/// that marked *everything* non-executable would pass every other test
/// here.
fn cargo_project(root: &Path) -> PathBuf {
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write Cargo.toml");
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create target dir");
    fs::write(target.join("artifact.bin"), vec![0u8; 4096]).expect("write artifact");
    target
}

/// Builds the report the menu-bar client actually reads, and asserts on the
/// way through that the decision really reached the most permissive policy
/// class. That assertion lives here rather than in each test because it is
/// the shared precondition for all of them: every claim below is of the form
/// "AUTO_SAFE alone buys nothing", and a fixture that silently drifted to
/// `ASK` would make each of those claims vacuously true.
fn detect_report(ev: &Evidence, action: Option<&dyn Action>) -> DetectCandidateReport {
    let now = SystemTime::now();
    let decision = classify(ev, &PolicyConfig::default(), now);
    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "fixture for {:?} must reach AUTO_SAFE, got {:?} ({:?})",
        ev.resource.kind,
        decision.class,
        decision.reasons
    );
    DetectCandidateReport::from_evidence_and_decision(
        ev,
        &decision,
        action,
        ImpactContext::default(),
    )
}

fn registry() -> ActionRegistry {
    ActionRegistry::builtin()
}

/// The headline regression: the Homebrew cache candidate must not be
/// offered at all, and must say why, *before* anyone presses Clean.
#[test]
fn homebrew_cache_is_not_offered_as_executable_and_explains_why() {
    let root = make_temp_dir("homebrew-not-offered");
    let cache = root.join("Homebrew");
    fs::create_dir_all(&cache).expect("create cache dir");
    fs::write(cache.join("download.tar.gz"), vec![0u8; 8192]).expect("write download");

    let ev = auto_safe_evidence(ResourceKind::HomebrewCache, &cache, 8192);
    let now = SystemTime::now();
    let decision = classify(&ev, &PolicyConfig::default(), now);

    // Vacuity guard: if the fixture stopped reaching the most permissive
    // class, the test below would prove nothing about policy labels.
    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "fixture must reach AUTO_SAFE for this test to mean anything"
    );

    let registry = registry();
    let action = registry
        .find_for_kind(ResourceKind::HomebrewCache)
        .expect("homebrew_cache has a registered action");
    let report = DetectCandidateReport::from_evidence_and_decision(
        &ev,
        &decision,
        Some(action),
        ImpactContext::default(),
    );

    assert_eq!(
        report.policy_label, "AUTO_SAFE",
        "the candidate is still honestly AUTO_SAFE — that was never the bug"
    );
    assert!(
        !report.executable,
        "an action execution refuses on sight must never be reported executable"
    );
    assert!(
        report.offered_actions.is_empty(),
        "a non-executable candidate must offer no action, got {:?}",
        report.offered_actions
    );
    let reason = report
        .refusal_reason
        .as_deref()
        .expect("a non-executable candidate must say why");
    assert!(
        reason.contains("scoped_path"),
        "the reason must name the real cause, got: {reason}"
    );
    // Not a PROTECTED refusal and not a missing-action refusal — the two
    // pre-existing shapes this must stay distinguishable from.
    assert!(
        !reason.contains("PROTECTED"),
        "wrong refusal shape: {reason}"
    );
    assert!(
        !reason.contains("no registered cleanup action"),
        "an action IS registered; the reason must not claim otherwise: {reason}"
    );

    fs::remove_dir_all(&root).ok();
}

/// `explain --json` feeds the same three fields to the same client, so it
/// must not disagree with `detect --json` about them.
#[test]
fn explain_agrees_with_detect_about_homebrew_actionability() {
    let root = make_temp_dir("homebrew-explain-agrees");
    let cache = root.join("Homebrew");
    fs::create_dir_all(&cache).expect("create cache dir");

    let ev = auto_safe_evidence(ResourceKind::HomebrewCache, &cache, 8192);
    let now = SystemTime::now();
    let decision = classify(&ev, &PolicyConfig::default(), now);
    let registry = registry();
    let action = registry
        .find_for_kind(ResourceKind::HomebrewCache)
        .expect("homebrew_cache has a registered action");

    let detect = DetectCandidateReport::from_evidence_and_decision(
        &ev,
        &decision,
        Some(action),
        ImpactContext::default(),
    );
    let explain = ExplainReport::from_evidence_and_decision(&ev, &decision, Some(action));

    assert_eq!(detect.executable, explain.executable);
    assert_eq!(detect.refusal_reason, explain.refusal_reason);
    assert_eq!(detect.offered_actions, explain.offered_actions);
    assert!(!explain.executable);

    fs::remove_dir_all(&root).ok();
}

/// The invariant that keeps this bug from coming back in a different shape:
/// for every registered action, the reporting layer's `executable` must be
/// exactly "the executor would not refuse this plan on sight".
///
/// Covers all three registered actions in one sweep, including the two that
/// *are* executable — so this fails both if an unexecutable action gets
/// offered and if an executable one stops being offered.
#[test]
fn executable_agrees_with_the_executors_own_rule_for_every_registered_action() {
    let root = make_temp_dir("agreement-sweep");
    let registry = registry();

    // (kind, resource path) pairs covering every registered action.
    let node_modules = root.join("proj/node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    let cargo_root = root.join("cargoproj");
    fs::create_dir_all(&cargo_root).expect("create cargo root");
    let target_dir = cargo_project(&cargo_root);
    let brew_cache = root.join("Homebrew");
    fs::create_dir_all(&brew_cache).expect("create brew cache");

    let cases = [
        (ResourceKind::NodeModules, node_modules.clone()),
        (ResourceKind::CargoTargetDir, target_dir.clone()),
        (ResourceKind::HomebrewCache, brew_cache.clone()),
    ];

    let mut checked = 0;
    let mut offered = 0;
    let mut refused = 0;

    for (kind, path) in cases {
        let action = registry
            .find_for_kind(kind)
            .unwrap_or_else(|| panic!("{kind:?} must have a registered action"));
        let ev = auto_safe_evidence(kind, &path, 4096);
        let report = detect_report(&ev, Some(action));

        // The executor's own answer, derived independently here.
        let executor_verdict = action
            .plan(&ev)
            .ok()
            .map(|plan| structural_refusal(&plan).is_none())
            // A plan that cannot even be built is not executable.
            .unwrap_or(false);

        assert_eq!(
            report.executable, executor_verdict,
            "{kind:?}: reporting says executable={} but the executor's structural rule says {}",
            report.executable, executor_verdict
        );

        // The two fields that gate a UI must never contradict `executable`.
        assert_eq!(
            report.executable,
            !report.offered_actions.is_empty(),
            "{kind:?}: offered_actions must be non-empty exactly when executable"
        );
        assert_eq!(
            report.executable,
            report.refusal_reason.is_none(),
            "{kind:?}: refusal_reason must be set exactly when not executable"
        );

        checked += 1;
        if report.executable {
            offered += 1;
        } else {
            refused += 1;
        }
    }

    assert_eq!(checked, 3, "every registered action must be covered");
    // Vacuity guards in both directions: this test is worthless if nothing
    // is offered, and it is not testing HORO-1358 if nothing is refused.
    assert_eq!(
        offered, 2,
        "cargo and node must still be offered — a fix that disabled everything is not a fix"
    );
    assert_eq!(refused, 1, "homebrew must be the one refused");

    fs::remove_dir_all(&root).ok();
}

/// The refusal the user is shown up front must be the *same sentence*
/// execution would have produced — not a paraphrase that could drift away
/// from the real reason.
#[test]
fn the_reported_refusal_is_verbatim_what_execution_would_say() {
    let root = make_temp_dir("verbatim-refusal");
    let cache = root.join("Homebrew");
    fs::create_dir_all(&cache).expect("create cache dir");

    let ev = auto_safe_evidence(ResourceKind::HomebrewCache, &cache, 8192);
    let registry = registry();
    let action = registry
        .find_for_kind(ResourceKind::HomebrewCache)
        .expect("homebrew_cache has a registered action");

    let report = detect_report(&ev, Some(action));
    let plan = action.plan(&ev).expect("homebrew planning succeeds");
    let executor_message =
        structural_refusal(&plan).expect("the executor must refuse this plan on sight");

    assert_eq!(
        report.refusal_reason.as_deref(),
        Some(executor_message.as_str()),
        "the offer's reason must be the executor's own words"
    );

    fs::remove_dir_all(&root).ok();
}

/// A cargo target dir whose manifest has since vanished cannot be cleaned —
/// `cargo clean --manifest-path` would fail — so it must not be offered
/// either. Proves the plan-error branch is reachable and honest, and that
/// the same target dir *with* a manifest is offered (so this is about the
/// manifest, not about cargo).
#[test]
fn a_cargo_target_dir_is_offered_only_while_its_manifest_exists() {
    let root = make_temp_dir("cargo-manifest-gone");
    let target_dir = cargo_project(&root);
    let registry = registry();
    let action = registry
        .find_for_kind(ResourceKind::CargoTargetDir)
        .expect("cargo_target_dir has a registered action");

    let ev = auto_safe_evidence(ResourceKind::CargoTargetDir, &target_dir, 4096);

    let with_manifest = detect_report(&ev, Some(action));
    assert!(
        with_manifest.executable,
        "a real cargo project must still be offered: {:?}",
        with_manifest.refusal_reason
    );

    fs::remove_file(root.join("Cargo.toml")).expect("remove Cargo.toml");

    let without_manifest = detect_report(&ev, Some(action));
    assert!(
        !without_manifest.executable,
        "a target dir with no manifest cannot be cleaned, so it must not be offered"
    );
    let reason = without_manifest
        .refusal_reason
        .as_deref()
        .expect("must say why");
    assert!(
        reason.contains("Cargo.toml"),
        "the reason should carry the planner's own words, got: {reason}"
    );

    fs::remove_dir_all(&root).ok();
}

/// A decoy `scoped_path` — one that does not appear in the step's own args
/// — must still be refused, and must be refused by the *same* predicate the
/// offer consults, so no action can buy itself an offer by attaching a
/// plausible-looking but unrelated path (HORO-1005 / HORO-1358 AC3).
#[test]
fn a_decoy_scoped_path_is_refused_by_the_rule_the_offer_consults() {
    use glomeris::actions::{ActionPlan, ActionStep, ToolBinary};

    let root = make_temp_dir("decoy-scoped-path");
    let real_target = root.join("real-target");
    let decoy = root.join("decoy");
    fs::create_dir_all(&real_target).expect("create real target");
    fs::create_dir_all(&decoy).expect("create decoy");

    let resource = ResourceId::new(
        ResourceKind::CargoTargetDir,
        ResourceLocator::Path(real_target.clone()),
    );

    // A plan whose `scoped_path` is a real, existing directory that would
    // pass identity revalidation — but which the command never mentions, so
    // revalidating it says nothing about what the command mutates.
    let decoyed = ActionPlan {
        action: glomeris::actions::ActionId("test.decoy"),
        resource: resource.clone(),
        steps: vec![ActionStep::RunTool {
            tool: ToolBinary::Cargo,
            args: vec![
                "clean".to_string(),
                "--target-dir".to_string(),
                real_target.to_string_lossy().into_owned(),
            ],
            scoped_path: Some(decoy.clone()),
        }],
        expected_reclaimed_bytes: ProbeOutcome::Observed(0),
        explain: "decoy".to_string(),
    };

    let refusal = structural_refusal(&decoyed)
        .expect("a scoped_path absent from the step's own args must be refused");
    assert!(
        refusal.contains("does not appear in this step's own args"),
        "unexpected refusal: {refusal}"
    );

    // The honest counterpart: the same plan scoped to the path it really
    // passes to the tool clears the rule. Without this, the assertion above
    // would also pass for a predicate that refused everything.
    let scoped = ActionPlan {
        steps: vec![ActionStep::RunTool {
            tool: ToolBinary::Cargo,
            args: vec![
                "clean".to_string(),
                "--target-dir".to_string(),
                real_target.to_string_lossy().into_owned(),
            ],
            scoped_path: Some(real_target.clone()),
        }],
        ..decoyed
    };
    assert!(
        structural_refusal(&scoped).is_none(),
        "a correctly scoped plan must not be refused on sight"
    );

    fs::remove_dir_all(&root).ok();
}

/// An unscoped mutating step is refused whatever the policy class says —
/// asserted here across every class, since the founder's candidate was
/// `AUTO_SAFE` and the whole point is that the label buys nothing.
#[test]
fn an_unscoped_mutating_step_is_refused_regardless_of_policy_class() {
    use glomeris::actions::{ActionPlan, ActionStep, ToolBinary};

    let root = make_temp_dir("unscoped-any-class");
    let cache = root.join("Homebrew");
    fs::create_dir_all(&cache).expect("create cache dir");

    let resource = ResourceId::new(
        ResourceKind::HomebrewCache,
        ResourceLocator::Path(cache.clone()),
    );
    let unscoped = ActionPlan {
        action: glomeris::actions::ActionId("homebrew.cleanup.cache"),
        resource,
        steps: vec![ActionStep::RunTool {
            tool: ToolBinary::Brew,
            args: vec!["cleanup".to_string(), "-s".to_string()],
            scoped_path: None,
        }],
        expected_reclaimed_bytes: ProbeOutcome::Observed(0),
        explain: "unscoped".to_string(),
    };

    // `structural_refusal` takes no policy input at all — which is exactly
    // the property being asserted. There is no argument to vary, and that
    // is stronger than testing each class in turn: the rule is structurally
    // incapable of consulting a policy class.
    let refusal = structural_refusal(&unscoped).expect("must refuse an unscoped mutating step");
    assert!(
        refusal.contains("no scoped_path"),
        "unexpected refusal: {refusal}"
    );
    assert!(
        refusal.contains("regardless of policy class"),
        "the refusal should say the label buys nothing: {refusal}"
    );

    fs::remove_dir_all(&root).ok();
}
