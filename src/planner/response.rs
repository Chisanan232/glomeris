//! The only place bytes from a provider become typed values (HORO-1548).
//!
//! # Everything here is a claim, and the types say so
//!
//! Each struct in this module is named `…Claim` because that is what it is: a
//! record of what a provider said, not a record of anything true. A
//! `PlanItemClaim` holds a `resource_id: String` — not a
//! [`crate::evidence::ResourceId`] — because the model's string is not a
//! resource identity until [`super::validate`] has found it in the alias table
//! that was issued for this very request. The type would let you forget that;
//! the name will not.
//!
//! Every field is a `String`, a `u32`, a `Vec<String>` or an `Option` of one of
//! those. Nothing here can be a path, an action, a resource or a command,
//! because no type in this module can name one — the same discipline
//! [`super::dto`] applies on the way out, applied on the way in.
//!
//! # Unknown fields fail the whole response
//!
//! `#[serde(deny_unknown_fields)]` is on every struct. A response carrying a
//! `"command"`, an `"argv"`, a `"path"` or a `"shell"` alongside the legal
//! fields does not get those fields ignored — it fails to parse at all, and
//! Glomeris falls back to rule-only ranking. That is deliberately harsher than
//! the per-field handling in [`super::validate`]: an unrecognised *word* in a
//! known field is a model being imprecise, while an unrecognised *field* is a
//! response that was not written against this contract, and guessing which
//! parts of it to trust is not a thing worth attempting. Version 1 settled the
//! same question the same way (`unexpected_field_rejects_whole_plan`).
//!
//! # Version detection, and why v1 is recognised by its silence
//!
//! Version 1 never declared a version, so the absence of `contract_version` is
//! how a v1 response is identified — not a default, an inference from the
//! shape. [`read_planner_response`] therefore:
//!
//! 1. Reads `contract_version` alone, tolerating everything else.
//! 2. If it names a version this build does not implement — including `1`,
//!    which no genuine v1 response ever sent — refuses outright. A provider
//!    that answers version 3 to a version 2 request has been asked a question
//!    it did not answer, and the fields it did send cannot be assumed to mean
//!    what this build would read them as.
//! 3. If it names [`PLANNER_CONTRACT_VERSION`], parses strictly as v2 and
//!    reports a parse failure as a parse failure — never quietly retrying as
//!    v1, which would turn a malformed v2 response into a silently downgraded
//!    plan.
//! 4. If it names nothing, tries v2 and then v1, and reports which one was
//!    read. The migration is visible in the result rather than inferred from
//!    behaviour, which is the whole reason the version exists.

use serde::Deserialize;

use super::contract::PLANNER_CONTRACT_VERSION;
use crate::actions::llm::{extract_fenced_block, extract_json_object_span, LlmError, LlmPlan};

/// What a provider's bytes turned out to be.
///
/// Not `Clone`, because [`LlmPlan`] is not, and there is no reason to copy a
/// parsed response around — it is consumed once by validation.
#[derive(Debug)]
pub enum PlannerResponse {
    /// Read against this build's contract.
    V2 {
        claim: PlannerResponseClaim,
        /// Whether the response said so itself. A v2-shaped response that
        /// declared nothing is still read as v2 — it parsed — but the
        /// distinction is kept because "the provider confirmed the contract"
        /// and "the bytes happened to fit" are different levels of assurance
        /// and a report should not present them as one.
        declared: bool,
    },
    /// Read against the unversioned ranking contract. Reached only when the
    /// response declared no version *and* did not parse as v2, which is
    /// exactly the shape a pre-HORO-1548 `--plan-file` fixture has.
    V1(LlmPlan),
}

/// Why a provider's bytes could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseError {
    /// Not a JSON object either contract recognises. Carries serde's message
    /// for the *current* contract, since that is the one that was asked for.
    NotParseable(String),
    /// The response named a contract this build does not implement.
    ///
    /// Its own variant rather than a [`Self::NotParseable`] message, because
    /// the two want different fixes — one is a malformed response, the other
    /// is a provider or a fixture written against a different Glomeris — and
    /// because a mutation test needs to assert that a mismatched version is
    /// refused *for that reason* and not incidentally.
    UnsupportedContractVersion { declared: u32, supported: u32 },
}

impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotParseable(detail) => {
                write!(f, "could not parse planner response: {detail}")
            }
            Self::UnsupportedContractVersion {
                declared,
                supported,
            } => write!(
                f,
                "planner response declares contract version {declared}, \
                 but this build implements version {supported}"
            ),
        }
    }
}

impl From<ResponseError> for LlmError {
    /// Both kinds of failure reach the caller as the one thing they mean in
    /// practice: the response was unusable, so rank by rules alone. The typed
    /// distinction is preserved up to this boundary, where the CLI has a
    /// single error surface to keep.
    fn from(error: ResponseError) -> Self {
        LlmError::InvalidResponse(error.to_string())
    }
}

/// Reads `contract_version` and tolerates everything else.
///
/// Deliberately *without* `deny_unknown_fields` — the one struct here that
/// omits it. Its job is to answer "which contract does this claim to be"
/// before strict parsing can decide what counts as an unknown field, and a
/// strict version probe would refuse every response it was meant to classify.
#[derive(Debug, Deserialize)]
struct VersionProbe {
    contract_version: Option<u32>,
}

/// A complete planner response, as claimed.
///
/// The four parts campaign section 14 specifies. `workspace_profile` is
/// `Option` because a provider with nothing to say about the shape of the
/// workspace should say nothing rather than guess a mode; the three lists
/// default to empty for the same reason, and an empty response is a legal one.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannerResponseClaim {
    pub contract_version: Option<u32>,
    pub workspace_profile: Option<WorkspaceProfileClaim>,
    #[serde(default)]
    pub items: Vec<PlanItemClaim>,
    #[serde(default)]
    pub observations: Vec<ObservationClaim>,
    #[serde(default)]
    pub evidence_requests: Vec<EvidenceRequestClaim>,
}

/// The claimed shape of this developer's workspace.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfileClaim {
    /// A [`crate::workspace::WorkflowMode`] tag, unresolved.
    pub mode: String,
    /// A [`super::ClaimConfidence`] tag, unresolved.
    pub confidence: String,
    /// Aliases from the request this claim rests on. An alias that was never
    /// issued is dropped by validation — a profile citing `workspace_9` in a
    /// request that contained two worktrees is citing nothing.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub summary: Option<String>,
}

/// One claimed recommendation about one resource.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItemClaim {
    /// A `resource_N` alias. Named `resource_id` rather than `resource_ref`
    /// only because v1 called it that and the prompt should not have to teach
    /// two names for the same thing; it is an alias either way, and a value
    /// that is not in this request's alias table resolves to nothing.
    pub resource_id: String,
    pub action_id: String,
    /// A [`super::Disposition`] tag, unresolved.
    pub disposition: String,
    /// A [`super::ClaimConfidence`] tag, unresolved.
    pub confidence: String,
    pub priority: Option<u32>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// What the model says it does not know about this resource.
    ///
    /// The field campaign section 14 adds that v1 had no room for, and the one
    /// most worth having: a model that can say "no process probe answered for
    /// this worktree" beside its recommendation has produced something a human
    /// can act on, where the same recommendation without it reads as
    /// confidence.
    #[serde(default)]
    pub uncertainties: Vec<String>,
    pub reason: Option<String>,
}

/// One claimed remark not attached to any resource.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationClaim {
    /// An [`super::ObservationKind`] tag, unresolved.
    pub kind: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub detail: Option<String>,
}

/// One claimed request for more evidence.
///
/// Three fields, and the two that matter are both lookups: `probe_id` against
/// [`super::ProbeId::from_tag`], `subject_ref` against the aliases this request
/// issued. There is deliberately no field for a program, an argument, a path,
/// a URL or a timeout — not because validation would reject them, but because
/// `deny_unknown_fields` means a response that sends one fails to parse, and
/// because a struct with nowhere to put a command cannot carry one to a
/// caller that later grows an executor. Campaign section 16 draws this line and
/// the shape of this struct is where it is drawn.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRequestClaim {
    pub probe_id: String,
    /// An alias from this request — `workspace_2`, `resource_7`. The subject
    /// of the question, selected from what the model was shown, never
    /// described by it.
    pub subject_ref: String,
    pub reason: Option<String>,
}

/// Turns a provider's raw text into a [`PlannerResponse`].
///
/// Prose around the JSON, a fenced code block, or a bare object are all
/// accepted, through the same two helpers version 1 uses — see
/// [`extract_fenced_block`].
pub fn read_planner_response(raw: &str) -> Result<PlannerResponse, ResponseError> {
    let candidate = extract_fenced_block(raw).unwrap_or(raw);
    let json_span = extract_json_object_span(candidate).unwrap_or(candidate);

    let declared = serde_json::from_str::<VersionProbe>(json_span)
        .map_err(|e| ResponseError::NotParseable(e.to_string()))?
        .contract_version;

    match declared {
        Some(version) if version != PLANNER_CONTRACT_VERSION => {
            Err(ResponseError::UnsupportedContractVersion {
                declared: version,
                supported: PLANNER_CONTRACT_VERSION,
            })
        }
        Some(_) => parse_v2(json_span).map(|claim| PlannerResponse::V2 {
            claim,
            declared: true,
        }),
        None => match parse_v2(json_span) {
            Ok(claim) => Ok(PlannerResponse::V2 {
                claim,
                declared: false,
            }),
            // Undeclared and not v2-shaped: the one case where falling back is
            // honest, because this is what every response written before the
            // contract was versioned looks like. The v2 error is what gets
            // reported if v1 does not fit either, since v2 is what was asked
            // for.
            Err(v2_error) => match serde_json::from_str::<LlmPlan>(json_span) {
                Ok(plan) => Ok(PlannerResponse::V1(plan)),
                Err(_) => Err(v2_error),
            },
        },
    }
}

fn parse_v2(json_span: &str) -> Result<PlannerResponseClaim, ResponseError> {
    serde_json::from_str::<PlannerResponseClaim>(json_span)
        .map_err(|e| ResponseError::NotParseable(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete, legal v2 response. Every optional field populated, so a
    /// field silently lost in parsing shows up here.
    const FULL_V2: &str = r#"{
        "contract_version": 2,
        "workspace_profile": {
            "mode": "parallel_multi_worktree",
            "confidence": "inferred",
            "evidence_refs": ["workflow_history", "repo_1"],
            "summary": "Three linked worktrees, two with local commits."
        },
        "items": [
            {
                "resource_id": "resource_1",
                "action_id": "cargo-clean",
                "disposition": "recommend_now",
                "confidence": "observed",
                "priority": 1,
                "evidence_refs": ["resource_1", "workspace_1"],
                "uncertainties": ["no process probe answered"],
                "reason": "Regenerable and nothing references it."
            }
        ],
        "observations": [
            {
                "kind": "conflicting_evidence",
                "evidence_refs": ["workspace_2"],
                "detail": "Pull request merged, worktree dirty."
            }
        ],
        "evidence_requests": [
            {
                "probe_id": "process_activity",
                "subject_ref": "workspace_2",
                "reason": "The activity state is unknown."
            }
        ]
    }"#;

    fn v2_claim(raw: &str) -> PlannerResponseClaim {
        match read_planner_response(raw).expect("should parse") {
            PlannerResponse::V2 { claim, .. } => claim,
            PlannerResponse::V1(_) => panic!("read as v1"),
        }
    }

    #[test]
    fn a_full_v2_response_parses_with_every_field_carried() {
        let claim = v2_claim(FULL_V2);

        assert_eq!(claim.contract_version, Some(2));

        let profile = claim.workspace_profile.expect("profile");
        assert_eq!(profile.mode, "parallel_multi_worktree");
        assert_eq!(profile.confidence, "inferred");
        assert_eq!(profile.evidence_refs, ["workflow_history", "repo_1"]);
        assert!(profile.summary.is_some());

        assert_eq!(claim.items.len(), 1);
        let item = &claim.items[0];
        assert_eq!(item.resource_id, "resource_1");
        assert_eq!(item.action_id, "cargo-clean");
        assert_eq!(item.disposition, "recommend_now");
        assert_eq!(item.confidence, "observed");
        assert_eq!(item.priority, Some(1));
        assert_eq!(item.evidence_refs, ["resource_1", "workspace_1"]);
        assert_eq!(item.uncertainties, ["no process probe answered"]);
        assert!(item.reason.is_some());

        assert_eq!(claim.observations.len(), 1);
        assert_eq!(claim.observations[0].kind, "conflicting_evidence");

        assert_eq!(claim.evidence_requests.len(), 1);
        assert_eq!(claim.evidence_requests[0].probe_id, "process_activity");
        assert_eq!(claim.evidence_requests[0].subject_ref, "workspace_2");
    }

    /// The minimum legal response: a declared version and nothing else.
    ///
    /// Worth pinning because the alternative — requiring at least one item —
    /// would push a provider with nothing to recommend into inventing
    /// something, which is the failure mode this whole contract is arranged
    /// against.
    #[test]
    fn an_empty_v2_response_is_legal() {
        let claim = v2_claim(r#"{"contract_version": 2}"#);
        assert!(claim.workspace_profile.is_none());
        assert!(claim.items.is_empty());
        assert!(claim.observations.is_empty());
        assert!(claim.evidence_requests.is_empty());
    }

    /// A fenced block and surrounding prose are both tolerated, because
    /// providers produce both and refusing them would fail over a wrapper
    /// rather than over content.
    #[test]
    fn fenced_and_prose_wrapped_responses_are_read() {
        let fenced = format!("Here is the plan:\n```json\n{FULL_V2}\n```\nHope that helps.");
        assert_eq!(v2_claim(&fenced).items.len(), 1);

        let prose = format!("I looked at everything. {FULL_V2} That is my answer.");
        assert_eq!(v2_claim(&prose).items.len(), 1);
    }

    /// An unknown field fails the whole response, at every level of nesting.
    ///
    /// Each case is a field a prompt-injected or simply wrong response would
    /// plausibly add, and the list is chosen to cover the ones that would
    /// matter if they were ignored instead of refused: a command, an argument
    /// vector, a path, a URL, a policy class the model does not assign, and a
    /// bare extra key. The assertion is on the *error*, not on a missing
    /// field, because "silently ignored" and "rejected" are indistinguishable
    /// from the parsed value alone.
    #[test]
    fn an_unknown_field_anywhere_fails_the_whole_response() {
        let cases = [
            r#"{"contract_version": 2, "command": "rm -rf /"}"#,
            r#"{"contract_version": 2, "note": "hello"}"#,
            r#"{"contract_version": 2, "workspace_profile": {"mode": "mixed", "confidence": "inferred", "script": "x"}}"#,
            r#"{"contract_version": 2, "items": [{"resource_id": "resource_1", "action_id": "a",
                 "disposition": "keep", "confidence": "unknown", "path": "/Users/someone/code"}]}"#,
            r#"{"contract_version": 2, "items": [{"resource_id": "resource_1", "action_id": "a",
                 "disposition": "keep", "confidence": "unknown", "policy_class": "auto_safe"}]}"#,
            r#"{"contract_version": 2, "observations": [{"kind": "workflow_shape", "url": "http://x"}]}"#,
            r#"{"contract_version": 2, "evidence_requests": [{"probe_id": "tool_liveness",
                 "subject_ref": "resource_1", "argv": ["docker", "rm"]}]}"#,
            r#"{"contract_version": 2, "evidence_requests": [{"probe_id": "tool_liveness",
                 "subject_ref": "resource_1", "credential": "sk-x"}]}"#,
        ];

        for case in cases {
            let result = read_planner_response(case);
            assert!(
                matches!(result, Err(ResponseError::NotParseable(_))),
                "an unknown field was tolerated: {case}"
            );
        }
    }

    /// A required field is required. No `Default` quietly filling in a
    /// disposition or a confidence the model never chose — a missing
    /// disposition is not `keep`, and a missing confidence is not `unknown`,
    /// because both of those would be this parser making the claim.
    #[test]
    fn a_missing_required_field_fails_rather_than_defaulting() {
        let cases = [
            r#"{"contract_version": 2, "items": [{"resource_id": "r", "action_id": "a", "confidence": "observed"}]}"#,
            r#"{"contract_version": 2, "items": [{"resource_id": "r", "action_id": "a", "disposition": "keep"}]}"#,
            r#"{"contract_version": 2, "items": [{"action_id": "a", "disposition": "keep", "confidence": "observed"}]}"#,
            r#"{"contract_version": 2, "items": [{"resource_id": "r", "disposition": "keep", "confidence": "observed"}]}"#,
            r#"{"contract_version": 2, "workspace_profile": {"mode": "mixed"}}"#,
            r#"{"contract_version": 2, "observations": [{"detail": "x"}]}"#,
            r#"{"contract_version": 2, "evidence_requests": [{"probe_id": "tool_liveness"}]}"#,
            r#"{"contract_version": 2, "evidence_requests": [{"subject_ref": "resource_1"}]}"#,
        ];

        for case in cases {
            assert!(
                matches!(
                    read_planner_response(case),
                    Err(ResponseError::NotParseable(_))
                ),
                "a required field was defaulted: {case}"
            );
        }
    }

    /// A version this build does not implement is refused, and refused for
    /// that reason.
    ///
    /// `1` is in the list on purpose. No genuine version 1 response ever sent
    /// a version, so a response that declares `1` is not a v1 response — it is
    /// something claiming to be one, and reading it as the legacy contract
    /// would mean a declared version selecting a parser, which is precisely
    /// the confusion the field exists to prevent.
    #[test]
    fn a_foreign_contract_version_is_refused_as_such() {
        for declared in [0, 1, 3, 99, u32::MAX] {
            let raw = format!(r#"{{"contract_version": {declared}, "items": []}}"#);
            assert_eq!(
                read_planner_response(&raw).map(|_| ()),
                Err(ResponseError::UnsupportedContractVersion {
                    declared,
                    supported: PLANNER_CONTRACT_VERSION,
                }),
                "version {declared} was not refused"
            );
        }
    }

    /// A response that declares version 2 and then fails to parse as version 2
    /// is an error, not a quiet downgrade.
    ///
    /// This is the case that would be easiest to get wrong by writing the
    /// fallback as a blanket "try v2, else try v1": a malformed v2 response
    /// whose `items` happen to be v1-shaped would then produce a plan, and the
    /// provider's own declaration that it was answering v2 would have been
    /// discarded to make that possible.
    #[test]
    fn a_declared_v2_response_never_falls_back_to_v1() {
        let v1_shaped_but_declared_v2 = r#"{
            "contract_version": 2,
            "items": [{"resource_id": "resource_1", "action_id": "cargo-clean", "priority": 1}]
        }"#;
        assert!(
            matches!(
                read_planner_response(v1_shaped_but_declared_v2),
                Err(ResponseError::NotParseable(_))
            ),
            "a declared v2 response was silently read as v1"
        );
    }

    /// An undeclared v1-shaped response is read as v1, and says so.
    ///
    /// The migration requirement: the `--plan-file` fixtures written before
    /// this contract existed keep working, and the result records which
    /// contract was used rather than leaving a caller to infer it.
    #[test]
    fn an_undeclared_v1_shaped_response_is_read_as_v1() {
        let raw = r#"{"items": [
            {"resource_id": "resource_1", "action_id": "cargo-clean", "priority": 1,
             "reason": "stale build output"}
        ]}"#;

        match read_planner_response(raw).expect("should parse") {
            PlannerResponse::V1(plan) => {
                assert_eq!(plan.items.len(), 1);
                assert_eq!(plan.items[0].resource_id, "resource_1");
            }
            PlannerResponse::V2 { .. } => panic!("a v1 response was read as v2"),
        }
    }

    /// An undeclared v2-shaped response is read as v2, with `declared` false.
    #[test]
    fn an_undeclared_v2_shaped_response_is_read_as_v2_and_flagged() {
        let raw =
            r#"{"observations": [{"kind": "recovery_outlook", "detail": "Goal not reachable."}]}"#;
        match read_planner_response(raw).expect("should parse") {
            PlannerResponse::V2 { claim, declared } => {
                assert!(!declared, "an undeclared response reported itself declared");
                assert_eq!(claim.observations.len(), 1);
            }
            PlannerResponse::V1(_) => panic!("read as v1"),
        }
    }

    /// Bytes that are neither contract fail, and the reported error is the
    /// version-2 one — the contract that was actually requested.
    #[test]
    fn unreadable_bytes_report_the_current_contracts_error() {
        for raw in [
            "",
            "I am sorry, I cannot help with that.",
            "{",
            "[]",
            r#"{"items": "not a list"}"#,
            r#"{"items": [{"resource_id": 7, "action_id": "a", "disposition": "keep", "confidence": "observed"}]}"#,
        ] {
            assert!(
                matches!(
                    read_planner_response(raw),
                    Err(ResponseError::NotParseable(_))
                ),
                "{raw:?} did not fail"
            );
        }
    }

    /// Both failures reach a caller as one `LlmError::InvalidResponse`, and
    /// the unsupported-version message names both versions so the message is
    /// actionable without a debugger.
    #[test]
    fn errors_convert_to_an_invalid_response_that_says_what_happened() {
        let converted: LlmError = ResponseError::UnsupportedContractVersion {
            declared: 7,
            supported: 2,
        }
        .into();
        match converted {
            LlmError::InvalidResponse(message) => {
                assert!(message.contains('7'), "{message}");
                assert!(message.contains('2'), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let converted: LlmError = ResponseError::NotParseable("bad".to_string()).into();
        assert!(matches!(converted, LlmError::InvalidResponse(_)));
    }
}
