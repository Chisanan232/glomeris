//! HORO-1008 golden-scenario proof: a `glomeris llm-plan` request that
//! asks to delete SSH key material is refused, end to end through the
//! real `crate::cli::build_llm_plan_report` pipeline, using
//! `FilePlanProvider` pointed at a fixture — no network call, no API key.
//!
//! This closes the gap `known_limitations.md` describes under "The BYOK
//! LLM planner is a library capability only, and PROTECTED is
//! unreachable through the shipped CLI": HORO-943's golden acceptance
//! scenario step 6 ("prove a protected resource cannot be deleted even
//! if an LLM plan requests it") was previously verified at the code
//! level only (`llm_plan_item_never_bypasses_policy` in
//! `src/actions/llm.rs`), never through an actual CLI input surface.
//! This test proves it through the exact input surface `glomeris
//! llm-plan --plan-file <path>` uses.

use std::path::PathBuf;
use std::time::SystemTime;

use glomeris::actions::llm::{plan_with_llm, FilePlanProvider};
use glomeris::actions::ActionRegistry;
use glomeris::cli::build_llm_plan_report;
use glomeris::detectors::DetectorId;
use glomeris::evidence::{
    Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint,
    ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::policy::{classify, PolicyClass, PolicyConfig, ReasonCode};
use glomeris::reporting::ImpactContext;

const FIXTURE_PATH: &str = "tests/fixtures/llm_plan_requests_protected_deletion.json";

/// Hand-built evidence for a `CargoTargetDir`-kind resource whose path
/// happens to be SSH key material — mirrors
/// `policy::engine::protected_path_wins_even_with_stale_evidence` and
/// `actions::llm::tests::llm_plan_item_never_bypasses_policy`'s own
/// fixture exactly, including the resource kind: this deliberately proves
/// the credential-material *path* pattern wins regardless of what
/// resource *kind* a (real or hypothetical) detector attached to it.
fn protected_evidence() -> Evidence {
    Evidence {
        resource: ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/.ssh/id_ed25519")),
        ),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: DetectorId("test"),
        logical_bytes: ProbeOutcome::Observed(1024),
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Observed(1024),
        reclaimable_bytes_is_lower_bound: false,
        last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability: ResourceKind::CargoTargetDir.regenerability(),
        recoverability: Recoverability::RegenerableByRebuild,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Observed(Vec::new()),
        process_cwd_match: ProbeOutcome::Observed(Vec::new()),
        git_state: ProbeOutcome::Observed(None),
        tool_liveness: ProbeOutcome::Observed(false),
        collected_at: SystemTime::UNIX_EPOCH,
        sources: Vec::new(),
    }
}

#[test]
fn golden_llm_plan_protected_refusal() {
    let ev = protected_evidence();
    let cfg = PolicyConfig::default();
    let now = SystemTime::UNIX_EPOCH;

    // Step 1: real policy classification agrees this is Protected
    // credential material.
    let decision = classify(&ev, &cfg, now);
    assert_eq!(decision.class, PolicyClass::Protected);
    assert_eq!(
        decision.reasons,
        vec![ReasonCode::ProtectedCredentialMaterial]
    );

    // Step 2: even a matching UserConsent cannot authorize it —
    // `authorize` unconditionally refuses `Protected`, with no override
    // parameter that could change that.
    let consent = glomeris::policy::UserConsent::new(
        ev.resource.clone(),
        ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        now,
    );
    let approval = glomeris::policy::approval::authorize(
        decision.clone(),
        ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        Some(&consent),
    );
    assert!(
        approval.is_none(),
        "Protected must never authorize, even with matching consent"
    );

    // Step 3: the fixture LLM response (asking to delete this exact
    // resource) survives `plan_with_llm`'s own validation — resource_id
    // and action_id are both real, so this is not a "hallucination
    // dropped" case; the refusal must come from policy, not from
    // validation.
    let provider = FilePlanProvider {
        path: PathBuf::from(FIXTURE_PATH),
    };
    let actions = ActionRegistry::builtin();
    let evidence_set = vec![ev.clone()];
    let plan_result = plan_with_llm(&provider, &evidence_set, &actions);
    assert!(plan_result.provider_error.is_none());
    assert_eq!(plan_result.validated_items.len(), 1);

    // Step 4: `build_llm_plan_report` — the actual `glomeris llm-plan`
    // code path — refuses the item. `execute()`'s unreachability from
    // here is a TYPE-LEVEL fact, not something separately asserted at
    // runtime: `Approval` (`policy::approval::Approval`) can only be
    // constructed by `policy::approval::authorize`, which — as step 2
    // just confirmed directly — returns `None` for this exact decision.
    // `build_llm_plan_report` additionally never calls `authorize` or
    // `execute` at all anywhere in its own code (see its doc comment and
    // `src/cli/mod.rs`'s source), so there is no code path from this
    // report to execution regardless.
    let candidates = vec![(ev, decision)];
    let report = build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());

    assert!(report.provider_error.is_none());
    assert_eq!(report.dropped_unknown_resource, 0);
    assert_eq!(report.dropped_unknown_action, 0);
    assert_eq!(report.items.len(), 1);

    let item = &report.items[0];
    assert_eq!(item.policy_label, "PROTECTED");
    assert!(item.requested_action_id.is_none());
    assert!(item.explain.is_none());
    assert!(item.skip_reason.is_some());

    // Step 5 (HORO-1308): the three fields a UI is required to read before
    // offering to do anything all say no, on the nested `candidate` that a
    // GUI renders with its ordinary candidate-row code.
    //
    // This is the assertion that makes the GUI's AI Plan surface safe by
    // construction rather than by discipline. The model asked, in this
    // fixture, for a real registered action against a real discovered
    // resource — nothing was hallucinated and nothing was dropped by
    // validation (step 3 proved that) — and the recommendation still cannot
    // produce an enabled control, because `executable` is computed from the
    // policy decision and the model is not one of its inputs.
    assert!(
        !item.candidate.executable,
        "a protected resource must never be executable, however it was suggested"
    );
    assert!(
        item.candidate.offered_actions.is_empty(),
        "no action may be offered for a protected resource: {:?}",
        item.candidate.offered_actions
    );
    let refusal = item
        .candidate
        .refusal_reason
        .as_deref()
        .expect("a refused candidate must say why");
    assert!(
        refusal.starts_with("PROTECTED: "),
        "the refusal must name the policy class that caused it, got {refusal:?}"
    );
    assert!(
        refusal.contains("protected_credential_material"),
        "the refusal must carry the real reason code, got {refusal:?}"
    );

    // And the nested candidate agrees with the item about the policy class,
    // so a UI reading either one cannot show a row whose badge and whose
    // button disagree.
    assert_eq!(item.candidate.policy_label, item.policy_label);
    assert_eq!(item.candidate.resource_id, item.resource_id);
}

/// HORO-1308: the model's rationale reaches the report — and reaches it
/// *attributed*, sitting beside this machine's own refusal rather than in
/// place of it.
///
/// Worth its own test because the failure mode is silent in both
/// directions. Before HORO-1308 the rationale was parsed off the wire and
/// dropped, so a user paid a provider for an explanation no surface ever
/// showed. The opposite mistake — letting the model's sentence be the only
/// explanation on the row — would be worse: here the model is confidently
/// recommending the deletion of an SSH private key, and "looks like a stale
/// build directory" reads perfectly plausibly.
#[test]
fn the_models_rationale_is_carried_but_never_replaces_the_real_verdict() {
    let ev = protected_evidence();
    let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
    let provider = FilePlanProvider {
        path: PathBuf::from(FIXTURE_PATH),
    };
    let actions = ActionRegistry::builtin();
    let candidates = vec![(ev, decision)];

    let report = build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());
    let item = &report.items[0];

    // The fixture's own `reason` text, carried through verbatim.
    assert_eq!(
        item.model_reason.as_deref(),
        Some("looks like a stale build directory")
    );

    // The model called it safe. The machine did not, and the machine's
    // answer is the one attached to every field that gates an action.
    assert_eq!(item.policy_label, "PROTECTED");
    assert!(!item.candidate.executable);

    // The rationale is a separate field from `reasons`, which is where the
    // evidence-backed reason codes live. Nothing the model wrote is in
    // there: a surface quoting `reasons` cannot accidentally quote the
    // model.
    let reasons: Vec<&str> = item.candidate.reasons.to_vec();
    assert_eq!(reasons, vec!["protected_credential_material"]);
    assert!(
        !reasons.iter().any(|r| r.contains("stale build directory")),
        "model text must never appear among the evidence reason codes"
    );
}
