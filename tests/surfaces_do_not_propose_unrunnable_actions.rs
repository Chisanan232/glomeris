//! HORO-1359: a surface that *chooses* an action must consult the same
//! answer the reporting layer already gives.
//!
//! HORO-1358 stopped `detect --json` and `explain --json` from promising a
//! Clean button that could not work, by asking whether the executor would
//! refuse the plan on sight. HORO-1360 put that question in one place
//! (`glomeris::actionability`) and wired the LLM prompt view and Autopilot
//! to it. The surfaces covered here are the remaining ones that pick an
//! action for themselves: `clean --dry-run`'s rows and `llm-plan`'s rows.
//!
//! Both built a plan and printed its `explain` as a live proposal.
//! `homebrew.cleanup.cache` plans perfectly well — it is the *step* that
//! carries `scoped_path: None` — so both printed `Run `brew cleanup -s`…`
//! for a resource that `detect --json`, in the same binary, was reporting
//! as `executable: false`.
//!
//! `free`'s candidate selection and `emergency`'s candidate processing are
//! the other two surfaces in this ticket. They are private functions and
//! are covered by `select_candidate_tests` and `emergency::tests` in the
//! crate, next to the code they constrain.
//!
//! Every test here carries its own positive control, because "propose
//! nothing" would satisfy a negative assertion perfectly and is not a fix.
//!
//! Nothing here executes anything, spawns `brew` or `cargo`, or touches the
//! real Homebrew cache. Every fixture is a temporary directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glomeris::actions::llm::FilePlanProvider;
use glomeris::actions::ActionRegistry;
use glomeris::cli::{build_clean_dry_run_item, build_llm_plan_report, resolve_action_for};
use glomeris::evidence::model::{
    NativeCleanup, Recoverability, ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::evidence::{Evidence, ProbeOutcome, ProbeReason};
use glomeris::executor::structural_refusal;
use glomeris::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision};
use glomeris::reporting::ImpactContext;

fn make_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glomeris-h1359-{tag}-{}-{:?}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Evidence shaped to reach `AUTO_SAFE` — the most permissive class — so
/// every refusal asserted below is the action's unrunnability and not the
/// policy label doing the work. `collected_at` must be now: the staleness
/// gate sends older evidence straight to `ASK`.
fn auto_safe_evidence(kind: ResourceKind, path: &Path) -> Evidence {
    Evidence {
        resource: ResourceId::new(kind, ResourceLocator::Path(path.to_path_buf())),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: glomeris::detectors::DetectorId("test"),
        logical_bytes: ProbeOutcome::Observed(4096),
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Observed(4096),
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

/// A real, minimal cargo project, because `cargo.clean.target_dir`'s
/// planner stats `Cargo.toml` and refuses without one. Returns the target
/// dir — the resource a detector would report.
fn cargo_project(root: &Path) -> PathBuf {
    fs::create_dir_all(root).expect("create cargo root");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write Cargo.toml");
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create target dir");
    target
}

/// Classifies `ev` and asserts it really did reach the most permissive
/// class, so the test built on it is not vacuously true.
fn auto_safe_decision(ev: &Evidence) -> PolicyDecision {
    let decision = classify(ev, &PolicyConfig::default(), SystemTime::now());
    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "fixture for {:?} must reach AUTO_SAFE for this test to mean anything, got {:?} ({:?})",
        ev.resource.kind,
        decision.class,
        decision.reasons
    );
    decision
}

/// The executor's own words for why it would refuse `ev`'s resolved action,
/// derived here independently of the code under test.
fn executors_own_refusal(ev: &Evidence, actions: &ActionRegistry) -> String {
    let action = resolve_action_for(ev, actions).expect("an action resolves for this kind");
    let plan = action
        .plan(ev)
        .expect("control invalid: this must be a refused STEP, not a failed plan");
    structural_refusal(&plan).expect("the executor must refuse this plan on sight")
}

/// `clean --dry-run` printed `Run `brew cleanup -s`…` as a proposal for a
/// resource execution refuses on sight. The refusal is now the row.
#[test]
fn clean_dry_run_states_the_refusal_instead_of_proposing_the_action() {
    let root = make_temp_dir("clean-dry-run");
    let actions = ActionRegistry::builtin();

    let cache = root.join("Homebrew");
    fs::create_dir_all(&cache).expect("create brew cache");
    let ev = auto_safe_evidence(ResourceKind::HomebrewCache, &cache);
    let decision = auto_safe_decision(&ev);

    let item = build_clean_dry_run_item(&ev, &decision, &actions);

    assert_eq!(
        item.policy_label, "AUTO_SAFE",
        "the resource is still honestly AUTO_SAFE — that was never the bug"
    );
    assert!(
        item.explain.is_none(),
        "a refused action must not be rendered as a dry-run proposal, got {:?}",
        item.explain
    );
    let reason = item
        .skip_reason
        .as_deref()
        .expect("a row with no proposal must say why");
    assert_eq!(
        reason,
        executors_own_refusal(&ev, &actions),
        "the row's reason must be the executor's own words, not a paraphrase"
    );
    assert_eq!(
        item.action_id,
        Some("homebrew.cleanup.cache"),
        "an action really was resolved, so naming it is more use than implying none was found"
    );

    // The invariant the renderer depends on. `print_clean_dry_run_report`
    // matches on `(&item.explain, &item.skip_reason)` and prints the explain
    // whenever it is present, so an item carrying both would show the
    // proposal and hide the refusal — this ticket's defect restated in a
    // different field.
    assert!(
        !(item.explain.is_some() && item.skip_reason.is_some()),
        "explain and skip_reason must never both be set: the renderer would show only the explain"
    );

    // Positive control: a runnable action IS still proposed, so the
    // assertions above are not satisfied by a change that proposes nothing.
    let target = cargo_project(&root.join("cargoproj"));
    let ok_ev = auto_safe_evidence(ResourceKind::CargoTargetDir, &target);
    let ok_decision = auto_safe_decision(&ok_ev);
    let ok_item = build_clean_dry_run_item(&ok_ev, &ok_decision, &actions);

    assert_eq!(ok_item.action_id, Some("cargo.clean.target_dir"));
    assert!(
        ok_item.skip_reason.is_none(),
        "a runnable action must not be skipped: {:?}",
        ok_item.skip_reason
    );
    let explain = ok_item
        .explain
        .as_deref()
        .expect("a runnable action must still render its dry-run text");
    assert!(
        explain.contains("cargo clean"),
        "the proposal should be the planner's own explain, got {explain:?}"
    );

    fs::remove_dir_all(&root).ok();
}

/// `node.clean.node_modules` is the second runnable built-in, so the
/// positive control above is not a single-action coincidence. Kept separate
/// because it needs no manifest fixture and would otherwise obscure the
/// cargo control's own point about `Cargo.toml`.
#[test]
fn clean_dry_run_still_proposes_node_modules_cleanup() {
    let root = make_temp_dir("clean-dry-run-node");
    let node_modules = root.join("proj/node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");

    let ev = auto_safe_evidence(ResourceKind::NodeModules, &node_modules);
    let decision = auto_safe_decision(&ev);
    let item = build_clean_dry_run_item(&ev, &decision, &ActionRegistry::builtin());

    assert_eq!(item.action_id, Some("node.clean.node_modules"));
    assert!(item.skip_reason.is_none(), "{:?}", item.skip_reason);
    assert!(item.explain.is_some());

    fs::remove_dir_all(&root).ok();
}

/// Writes an LLM response fixture asking for `action_id` against each
/// resource, in order, and returns a provider that reads it. Built at test
/// time rather than checked in, because a resource id contains the temp
/// path.
fn plan_fixture(root: &Path, items: &[(&Evidence, &str)]) -> FilePlanProvider {
    let rows: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(i, (ev, action_id))| {
            format!(
                r#"{{"resource_id":"{}","action_id":"{}","priority":{},"reason":"fixture"}}"#,
                ev.resource, action_id, i
            )
        })
        .collect();
    let path = root.join("llm-response.json");
    fs::write(&path, format!(r#"{{"items":[{}]}}"#, rows.join(","))).expect("write fixture");
    FilePlanProvider { path }
}

/// The surface this ticket's own enumeration of four missed, and the worse
/// of the two: an `llm-plan` row's nested `candidate` already carried
/// `executable: false` with a truthful `refusal_reason` from HORO-1358,
/// while its sibling `explain` field rendered the same action as a live
/// proposal. One JSON object disagreeing with itself.
#[test]
fn an_llm_plan_row_does_not_contradict_its_own_candidate() {
    let root = make_temp_dir("llm-plan-row");
    let actions = ActionRegistry::builtin();

    let cache = root.join("Homebrew");
    fs::create_dir_all(&cache).expect("create brew cache");
    let refused_ev = auto_safe_evidence(ResourceKind::HomebrewCache, &cache);
    let refused_decision = auto_safe_decision(&refused_ev);

    let target = cargo_project(&root.join("cargoproj"));
    let ok_ev = auto_safe_evidence(ResourceKind::CargoTargetDir, &target);
    let ok_decision = auto_safe_decision(&ok_ev);

    // One response asking for both, so the positive control travels through
    // the identical call. A model that names a real action for a real
    // resource is not a hallucination and is not dropped by validation —
    // which is the point: the refusal has to come from this code.
    let provider = plan_fixture(
        &root,
        &[
            (&refused_ev, "homebrew.cleanup.cache"),
            (&ok_ev, "cargo.clean.target_dir"),
        ],
    );
    let candidates = vec![(refused_ev.clone(), refused_decision), (ok_ev, ok_decision)];

    let report = build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());
    assert!(
        report.provider_error.is_none(),
        "{:?}",
        report.provider_error
    );
    assert_eq!(report.dropped_unknown_resource, 0);
    assert_eq!(
        report.dropped_unknown_action, 0,
        "both actions are real: nothing may be dropped by validation, or this proves nothing"
    );
    assert_eq!(report.items.len(), 2);

    let refused = &report.items[0];
    assert_eq!(refused.policy_label, "AUTO_SAFE");
    assert!(
        refused.explain.is_none(),
        "a refused action must not be rendered as a proposal, got {:?}",
        refused.explain
    );
    let reason = refused
        .skip_reason
        .as_deref()
        .expect("a row with no proposal must say why");
    assert_eq!(
        reason,
        executors_own_refusal(&refused_ev, &actions),
        "the row's reason must be the executor's own words"
    );
    assert!(
        !(refused.explain.is_some() && refused.skip_reason.is_some()),
        "explain and skip_reason must never both be set: the renderer would show only the explain"
    );

    // The specific contradiction: the row and its own nested candidate must
    // now agree. Before this fix `candidate.executable` was already `false`
    // with this same reason while `explain` proposed the action anyway.
    assert!(
        !refused.candidate.executable,
        "precondition: HORO-1358 already reports this candidate non-executable"
    );
    assert_eq!(
        refused.candidate.refusal_reason.as_deref(),
        Some(reason),
        "the row's skip_reason and its candidate's refusal_reason must be the same sentence"
    );
    assert_eq!(
        refused.explain.is_some(),
        refused.candidate.executable,
        "a row may propose an action exactly when its own candidate says the action is executable"
    );

    // Positive control, through the same call: a runnable action asked for
    // by the same model response is still proposed.
    let ok = &report.items[1];
    assert_eq!(ok.requested_action_id, Some("cargo.clean.target_dir"));
    assert!(ok.skip_reason.is_none(), "{:?}", ok.skip_reason);
    assert!(ok.candidate.executable);
    let explain = ok
        .explain
        .as_deref()
        .expect("a runnable action must still render its dry-run text");
    assert!(explain.contains("cargo clean"), "got {explain:?}");
    assert_eq!(
        ok.explain.is_some(),
        ok.candidate.executable,
        "the agreement must hold in the affirmative direction too"
    );

    fs::remove_dir_all(&root).ok();
}

/// The model naming a registered action that does not apply to the resource
/// it named is the *other* refusal arm, and was already handled before this
/// ticket. Pinned here so the change above cannot collapse the two into one
/// message: a user told "the action cannot run on this resource" and a user
/// told "no path to scope" are being told different things.
#[test]
fn an_llm_plan_row_keeps_the_planners_reason_distinct_from_the_executors() {
    let root = make_temp_dir("llm-plan-mismatch");
    let actions = ActionRegistry::builtin();

    let node_modules = root.join("proj/node_modules");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    let ev = auto_safe_evidence(ResourceKind::NodeModules, &node_modules);
    let decision = auto_safe_decision(&ev);

    // A real registered action, for the wrong kind of resource.
    let provider = plan_fixture(&root, &[(&ev, "cargo.clean.target_dir")]);
    let candidates = vec![(ev, decision)];

    let report = build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());
    assert_eq!(report.items.len(), 1);
    let item = &report.items[0];

    assert!(item.explain.is_none());
    let reason = item.skip_reason.as_deref().expect("must say why");
    assert!(
        !reason.contains("scoped_path"),
        "this is a plan-error refusal, not the executor's structural one: {reason}"
    );

    fs::remove_dir_all(&root).ok();
}
