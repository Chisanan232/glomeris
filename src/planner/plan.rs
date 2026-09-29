//! One version 2 planning round, end to end (HORO-1548).
//!
//! Build the request from a projection, send it, read the answer, reduce the
//! answer to what exists locally. Four steps, each already implemented in its
//! own module; this one exists so there is a single function a caller can use
//! and a single place the order of those steps is decided.
//!
//! # Never an error return
//!
//! [`plan_workspace`] always returns a [`WorkspacePlanResult`]. A provider
//! that is unreachable, answers with prose, answers with a version this build
//! does not implement, or answers about a different machine entirely all
//! produce an empty plan and a populated `provider_error` — the same shape a
//! caller already handles for "no provider is configured", which is the
//! overwhelmingly common case. Making any of those a `Result::Err` would turn
//! a provider's bad day into a failure of `glomeris`, when the correct
//! behaviour is to fall back to rule-only ranking and say so.
//!
//! # And never authorization
//!
//! Every resource in the returned plan must be re-judged by
//! [`crate::policy::classify`] against freshly collected evidence before
//! anything executes. That was true of version 1 and nothing here changes it:
//! the contract got richer, not more powerful.

use super::dto::PlannerRequestView;
use super::project::GraphProjection;
use super::prompt::system_prompt;
use super::response::{read_planner_response, PlannerResponse};
use super::validate::{validate_response, ValidatedWorkspacePlan};
use crate::actions::llm::{LlmError, LlmProvider};
use crate::actions::ActionRegistry;

/// The exact bytes one planning round would send.
///
/// Held as a type of its own so `--print-payload` shows the request rather
/// than a rendering of it: the same projection always produces the same two
/// strings, with no clock and no randomness involved, so what is printed is
/// what would be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRequest {
    /// [`super::prompt::system_prompt`]. A `String` rather than a
    /// `&'static str` because it is assembled from the contract's own
    /// vocabularies.
    pub system_prompt: String,
    /// The serialized [`PlannerRequestView`] — the only local information
    /// that leaves this machine, and the thing
    /// `tests/planner_model_egress_contract.rs` pins key by key.
    pub user_prompt: String,
}

/// What one planning round produced.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspacePlanResult {
    pub plan: ValidatedWorkspacePlan,
    /// Whether the provider declared the contract version it was answering.
    /// A response that omits it is still read as version 2 if it parses as
    /// one, so this distinguishes a provider following the contract from one
    /// that happened to produce a conforming shape — worth reporting, and not
    /// worth refusing over.
    pub contract_declared: bool,
    /// `Some(..)` when the round produced nothing usable. The plan is empty in
    /// that case, and the caller falls back to rule-only ranking exactly as it
    /// does when no provider is configured.
    pub provider_error: Option<LlmError>,
}

impl WorkspacePlanResult {
    fn failed(error: LlmError) -> Self {
        Self {
            plan: ValidatedWorkspacePlan::default(),
            contract_declared: false,
            provider_error: Some(error),
        }
    }
}

/// Builds the request for `projection` without sending it.
///
/// Separate from [`plan_workspace`] so the preview and the real call cannot
/// diverge: `--print-payload` prints what this returns, and
/// [`plan_workspace`] sends what this returns.
pub fn build_workspace_request(projection: &GraphProjection) -> Result<WorkspaceRequest, LlmError> {
    let request = PlannerRequestView::of(projection.view.clone());
    let user_prompt = serde_json::to_string(&request).map_err(|e| {
        LlmError::InvalidResponse(format!("failed to serialize the workspace projection: {e}"))
    })?;

    Ok(WorkspaceRequest {
        system_prompt: system_prompt(),
        user_prompt,
    })
}

/// Runs one planning round against `provider`.
pub fn plan_workspace(
    provider: &dyn LlmProvider,
    projection: &GraphProjection,
    actions: &ActionRegistry,
) -> WorkspacePlanResult {
    let request = match build_workspace_request(projection) {
        Ok(request) => request,
        Err(error) => return WorkspacePlanResult::failed(error),
    };

    let raw = match provider.complete(&request.system_prompt, &request.user_prompt) {
        Ok(raw) => raw,
        Err(error) => return WorkspacePlanResult::failed(error),
    };

    match read_planner_response(&raw) {
        Ok(PlannerResponse::V2 { claim, declared }) => WorkspacePlanResult {
            plan: validate_response(&claim, projection, actions),
            contract_declared: declared,
            provider_error: None,
        },
        // A version 1 response to a version 2 request is not promoted. Its
        // items carry no disposition and no confidence, and supplying either
        // here would be this function inventing the two fields that decide
        // whether a human is asked before anything happens. Reported as the
        // mismatch it is, and the caller ranks by rule instead.
        Ok(PlannerResponse::V1(_)) => WorkspacePlanResult::failed(LlmError::InvalidResponse(
            "the provider answered with the version 1 ranking shape, which carries no \
             disposition and no confidence"
                .to_string(),
        )),
        Err(error) => WorkspacePlanResult::failed(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint,
        ResourceId, ResourceKind, ResourceLocator,
    };
    use crate::planner::contract::{Disposition, PLANNER_CONTRACT_VERSION};
    use crate::policy::{PolicyClass, PolicyDecision, ReasonCode};
    use crate::workspace::{MachineContext, WorkspaceEvidenceGraph, WorkspaceSurvey};
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    const T0: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    /// Records what it was asked and replies with whatever it was told to.
    struct Scripted {
        reply: Result<String, LlmError>,
        seen: RefCell<Vec<(String, String)>>,
    }

    impl Scripted {
        fn replying(raw: &str) -> Self {
            Self {
                reply: Ok(raw.to_string()),
                seen: RefCell::new(Vec::new()),
            }
        }

        fn failing(error: LlmError) -> Self {
            Self {
                reply: Err(error),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl LlmProvider for Scripted {
        fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, LlmError> {
            self.seen
                .borrow_mut()
                .push((system_prompt.to_string(), user_prompt.to_string()));
            self.reply.clone()
        }
    }

    fn temp_cargo_project() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("glomeris-plan-{}-{nanos}-{n}", std::process::id()));
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).expect("create temp target dir");
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write manifest");
        target_dir
    }

    fn candidate(path: &Path) -> (Evidence, PolicyDecision) {
        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(path.to_path_buf()),
        );
        let evidence = Evidence {
            resource: resource.clone(),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(4096),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(T0),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::CargoTargetDir.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            collected_at: at(86_400 * 7),
            sources: Vec::new(),
        };
        let decision = PolicyDecision {
            resource,
            class: PolicyClass::AutoSafe,
            reasons: vec![ReasonCode::NoActiveUseObserved],
            evidence_collected_at: T0,
            evaluated_at: T0,
            policy_version: 1,
        };
        (evidence, decision)
    }

    fn projection() -> GraphProjection {
        let target = temp_cargo_project();
        let candidates = vec![candidate(&target)];
        let graph = WorkspaceEvidenceGraph::build(
            &candidates,
            &WorkspaceSurvey::unsurveyed(),
            MachineContext::unmeasured(),
            at(86_400 * 7),
        );
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let root = target.parent().expect("a target has a project root");
        assert!(root.starts_with(std::env::temp_dir()));
        let _ = fs::remove_dir_all(root);
        projection
    }

    const GOOD_RESPONSE: &str = r#"{"contract_version":2,
        "items":[{"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"ask_user","confidence":"inferred"}]}"#;

    #[test]
    fn a_round_sends_the_prompt_and_the_projection_and_nothing_else() {
        let projection = projection();
        let provider = Scripted::replying(GOOD_RESPONSE);
        let result = plan_workspace(&provider, &projection, &ActionRegistry::builtin());

        assert_eq!(result.provider_error, None);
        assert_eq!(result.plan.items.len(), 1);
        assert_eq!(result.plan.items[0].disposition, Disposition::AskUser);
        assert!(result.contract_declared);

        let seen = provider.seen.borrow();
        assert_eq!(seen.len(), 1, "one round is one provider call");
        let (system, user) = &seen[0];
        // What is sent is what the preview prints, byte for byte.
        let request = build_workspace_request(&projection).expect("the request builds");
        assert_eq!(*system, request.system_prompt);
        assert_eq!(*user, request.user_prompt);
        // And the request carries the version the response is held to.
        assert!(user.contains(&format!("\"contract_version\":{PLANNER_CONTRACT_VERSION}")));
    }

    #[test]
    fn the_request_is_byte_identical_across_calls() {
        let projection = projection();
        let first = build_workspace_request(&projection).expect("the request builds");
        let second = build_workspace_request(&projection).expect("the request builds");
        assert_eq!(first, second);
    }

    #[test]
    fn a_provider_that_cannot_be_reached_produces_an_empty_plan_and_says_why() {
        let error = LlmError::NetworkError("connection refused".to_string());
        let provider = Scripted::failing(error.clone());
        let result = plan_workspace(&provider, &projection(), &ActionRegistry::builtin());

        assert_eq!(result.plan, ValidatedWorkspacePlan::default());
        assert_eq!(result.provider_error, Some(error));
    }

    #[test]
    fn prose_instead_of_a_response_produces_an_empty_plan_and_says_why() {
        let provider = Scripted::replying("I would start with the build directories.");
        let result = plan_workspace(&provider, &projection(), &ActionRegistry::builtin());

        assert_eq!(result.plan, ValidatedWorkspacePlan::default());
        assert!(matches!(
            result.provider_error,
            Some(LlmError::InvalidResponse(_))
        ));
    }

    #[test]
    fn a_response_declaring_another_contract_version_is_refused_as_such() {
        let provider = Scripted::replying(r#"{"contract_version":99,"items":[]}"#);
        let result = plan_workspace(&provider, &projection(), &ActionRegistry::builtin());

        let Some(LlmError::InvalidResponse(detail)) = result.provider_error else {
            panic!("a foreign contract version was not refused");
        };
        assert!(detail.contains("99"), "{detail}");
        assert!(
            detail.contains(&PLANNER_CONTRACT_VERSION.to_string()),
            "the refusal does not say which version this build implements: {detail}"
        );
    }

    /// A version 1 response is not promoted. Its items have no disposition, so
    /// promoting one would mean this function choosing between recommending
    /// and asking — which is the whole decision the field exists to carry.
    #[test]
    fn a_version_one_response_is_not_promoted_into_a_version_two_plan() {
        let provider = Scripted::replying(
            r#"{"items":[{"resource_id":"resource_1",
                          "action_id":"cargo.clean.target_dir","priority":1}]}"#,
        );
        let result = plan_workspace(&provider, &projection(), &ActionRegistry::builtin());

        assert!(
            result.plan.items.is_empty(),
            "a version 1 item reached a version 2 plan"
        );
        let Some(LlmError::InvalidResponse(detail)) = result.provider_error else {
            panic!("the shape mismatch was not reported");
        };
        assert!(detail.contains("version 1"), "{detail}");
    }

    /// An undeclared response that happens to conform is used, and the fact
    /// that it never declared the contract is reported rather than assumed
    /// away.
    #[test]
    fn an_undeclared_but_conforming_response_is_used_and_flagged() {
        let provider = Scripted::replying(
            r#"{"items":[{"resource_id":"resource_1",
                          "action_id":"cargo.clean.target_dir",
                          "disposition":"keep","confidence":"unknown"}]}"#,
        );
        let result = plan_workspace(&provider, &projection(), &ActionRegistry::builtin());

        assert_eq!(result.provider_error, None);
        assert_eq!(result.plan.items.len(), 1);
        assert!(
            !result.contract_declared,
            "a response that declared nothing was reported as declaring the contract"
        );
    }

    /// The load-bearing one. A provider answering about resources it made up,
    /// with an action it was not offered and a probe this build does not
    /// implement, changes nothing about the machine and is reported as having
    /// lost every item.
    #[test]
    fn a_response_about_a_different_machine_yields_nothing_and_counts_it() {
        let provider = Scripted::replying(
            r#"{"contract_version":2,
                "items":[{"resource_id":"/Users/someone/code/target",
                          "action_id":"shell.run","disposition":"recommend_now",
                          "confidence":"observed"}],
                "evidence_requests":[{"probe_id":"run_command","subject_ref":"resource_1"}]}"#,
        );
        let result = plan_workspace(&provider, &projection(), &ActionRegistry::builtin());

        assert_eq!(result.provider_error, None, "this is not a failed round");
        assert!(result.plan.items.is_empty());
        assert!(result.plan.evidence_requests.is_empty());
        assert_eq!(result.plan.counters.dropped_unknown_resource, 1);
        assert_eq!(result.plan.counters.dropped_unknown_probe, 1);
    }
}
