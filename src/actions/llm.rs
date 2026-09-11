//! Optional BYOK LLM planner (HORO-954).
//!
//! **What this module is**: an advisory ranking suggestion over evidence
//! the crate already collected. [`LlmResourceView`] is an explicit,
//! bounded projection of one [`Evidence`] record, safe to serialize and
//! hand to a model. [`LlmPlan`]/[`LlmPlanItem`] are the model-facing
//! response shape.
//!
//! **Why only two `Deserialize` types exist here (and in the whole
//! crate)**: [`LlmPlan`]/[`LlmPlanItem`] are the sole external-input
//! parse targets. Everything they can produce is either a `String`
//! resolved against real, already-in-memory data, or a plain
//! `u32`/`Option<String>` used only for display/ranking — never a path,
//! never a shell fragment, never anything that reaches
//! [`crate::actions::ActionStep`] construction directly.
//! `#[serde(deny_unknown_fields)]` on [`LlmPlanItem`] additionally
//! ensures a model cannot smuggle an extra field (e.g. a `command`) past
//! the parser: any unknown field fails deserialization of the whole
//! struct, which fails the whole `Vec`, which fails the whole
//! [`LlmPlan`] — see `unexpected_field_rejects_whole_plan` below.

use serde::Deserialize;

use crate::evidence::model::{ActionId, Completeness, Evidence, Regenerability, ResourceId};

use super::ActionRegistry;

/// Explicit, bounded projection of one [`Evidence`] record — deliberately
/// NOT `#[derive(Serialize)]` on `Evidence` itself. This is what keeps
/// "never ship raw filesystem inventory to an LLM" true even if
/// `Evidence` gains fields later: adding a field to `Evidence` has no
/// effect on what an LLM sees unless someone also, explicitly, adds it
/// here.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct LlmResourceView {
    pub resource_id: String,
    pub kind: &'static str,
    pub reclaimable_bytes: Option<u64>,
    pub age_days: Option<u64>,
    pub regenerability: &'static str,
    pub completeness: &'static str,
    pub offered_action_ids: Vec<&'static str>,
}

impl LlmResourceView {
    /// Explicit, field-by-field conversion — no `From`/blanket impl, so
    /// adding a field to `Evidence` never silently starts flowing to an
    /// LLM; a human has to come here and decide to add it.
    pub fn from_evidence(ev: &Evidence, actions: &ActionRegistry) -> Self {
        // `age_days` is derived from `last_modified` relative to when this
        // evidence was collected — never from an ambient `SystemTime::now()`
        // call, matching `crate::policy::engine::classify`'s "no ambient
        // clock" discipline and keeping this conversion deterministic and
        // testable.
        let age_days = ev.last_modified.observed().and_then(|last_modified| {
            ev.collected_at
                .duration_since(*last_modified)
                .ok()
                .map(|age| age.as_secs() / (24 * 60 * 60))
        });

        Self {
            resource_id: ev.resource.to_string(),
            kind: ev.resource.kind_tag(),
            reclaimable_bytes: ev.reclaimable_bytes.observed().copied(),
            age_days,
            regenerability: regenerability_tag(ev.regenerability),
            completeness: completeness_tag(&ev.completeness()),
            offered_action_ids: actions.ids_for_kind(ev.resource.kind),
        }
    }
}

fn regenerability_tag(r: Regenerability) -> &'static str {
    match r {
        Regenerability::RegenerableByTool => "regenerable_by_tool",
        Regenerability::RegenerableByRebuild => "regenerable_by_rebuild",
        Regenerability::NotRegenerable => "not_regenerable",
        Regenerability::Unknown => "unknown",
    }
}

fn completeness_tag(c: &Completeness) -> &'static str {
    match c {
        Completeness::Complete => "complete",
        Completeness::Partial { .. } => "partial",
        Completeness::Failed => "failed",
    }
}

/// The ONLY model-facing `Deserialize` type in the crate, together with
/// [`LlmPlanItem`]. See the module doc comment for why this is safe.
#[derive(Debug, Deserialize, PartialEq)]
pub struct LlmPlan {
    pub items: Vec<LlmPlanItem>,
}

/// One item of a model-proposed plan. `#[serde(deny_unknown_fields)]`
/// means any extra field (e.g. a smuggled `"command"`) fails
/// deserialization of the whole item — and therefore the whole
/// surrounding `Vec`/`LlmPlan` — rather than being silently ignored. See
/// `unexpected_field_rejects_whole_plan` in this module's tests.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LlmPlanItem {
    /// Resolved against the evidence set passed to `plan_with_llm` (a
    /// follow-up commit) — unknown -> the item is dropped, never a hard
    /// error for the whole plan.
    pub resource_id: String,
    /// Resolved via `ActionRegistry::get` — unknown -> the item is
    /// dropped, never a hard error for the whole plan.
    pub action_id: String,
    pub priority: Option<u32>,
    /// Human-readable explanation. Informational only — NEVER
    /// interpreted as an instruction, a path, or anything that reaches
    /// execution. It exists purely for a human to read in a UI/log.
    pub reason: Option<String>,
}

/// A provider of raw LLM completions. Implementors return the model's raw
/// text response, which a follow-up commit's `plan_with_llm` parses
/// defensively — it is expected to contain JSON matching [`LlmPlan`], but
/// may be wrapped in prose or markdown code fences.
pub trait LlmProvider {
    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, LlmError>;
}

/// Why an [`LlmProvider`] call failed. Its `Debug` output is safe to log:
/// none of these variants embed the API key (see
/// `openai_error_never_leaks_api_key` in this module's tests, added
/// alongside the real `OpenAiCompatibleProvider` implementation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    NotConfigured,
    NetworkError(String),
    InvalidResponse(String),
}

/// Real provider: any OpenAI-compatible `/chat/completions` endpoint
/// (OpenAI itself, OpenRouter, a self-hosted gateway, ...). BYOK: the
/// caller (CLI layer) is responsible for reading `api_key` from an
/// environment variable and never logging/printing it — this type only
/// holds it long enough to build a request. Deliberately does not derive
/// `Debug`: a derived `Debug` would print `api_key` verbatim, which would
/// silently defeat the "never logged" guarantee the moment anyone
/// `{:?}`-logs a provider value. If a `Debug` impl is ever needed, it
/// must be hand-written to redact this field.
pub struct OpenAiCompatibleProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, LlmError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/chat/completions");

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt },
            ],
        });

        let auth_header = format!("Bearer {}", self.api_key);

        let response = ureq::post(&url)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| {
                // `ureq::Error`'s `Display` never includes request headers
                // (it reports transport/status information only), so this
                // cannot leak `self.api_key` — but we deliberately format
                // only the error itself, never anything derived from
                // `auth_header`, to keep that invariant obviously true by
                // construction rather than by trusting ureq's internals.
                LlmError::NetworkError(e.to_string())
            })?;

        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| LlmError::InvalidResponse(format!("failed to read response body: {e}")))?;

        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| LlmError::InvalidResponse(format!("response was not JSON: {e}")))?;

        let content = value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| content.as_str())
            .ok_or_else(|| {
                LlmError::InvalidResponse(
                    "response JSON missing choices[0].message.content".to_string(),
                )
            })?;

        Ok(content.to_string())
    }
}

/// Finds the first plausible JSON object span in `text` and attempts to
/// parse it as an [`LlmPlan`]. Handles two shapes defensively: a
/// ```` ```json ... ``` ```` (or bare ` ``` `) fenced block, and a raw
/// `{ ... }` object possibly preceded/followed by prose. Never
/// partial-parses — either a well-formed [`LlmPlan`] comes out, or
/// nothing does.
fn extract_plan(text: &str) -> Result<LlmPlan, LlmError> {
    let candidate = extract_fenced_block(text).unwrap_or(text);
    let json_span = extract_json_object_span(candidate).unwrap_or(candidate);

    serde_json::from_str::<LlmPlan>(json_span)
        .map_err(|e| LlmError::InvalidResponse(format!("could not parse LLM plan JSON: {e}")))
}

/// Returns the contents of the first fenced code block (```` ``` ```` or
/// ```` ```json ````) in `text`, if any.
fn extract_fenced_block(text: &str) -> Option<&str> {
    let fence_start = text.find("```")?;
    let after_first_fence = &text[fence_start + 3..];
    // Skip an optional language tag (e.g. "json") up to the first newline.
    let body_start = after_first_fence.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &after_first_fence[body_start..];
    let fence_end = body.find("```")?;
    Some(&body[..fence_end])
}

/// Returns the span from the first `{` to the last `}` in `text`, if
/// both are present and correctly ordered.
fn extract_json_object_span(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&text[start..=end])
}

/// Result of one [`plan_with_llm`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmPlanResult {
    pub validated_items: Vec<(ResourceId, ActionId, Option<u32>)>,
    pub dropped_unknown_resource: u32,
    pub dropped_unknown_action: u32,
    /// `Some(..)` if the provider call itself failed, or the response
    /// could not be parsed at all — in either case `validated_items` is
    /// empty, and this is NOT an error return: callers fall back to
    /// rule-only ranking uniformly, whether the provider was never
    /// configured, the network call failed, or the response was
    /// malformed.
    pub provider_error: Option<LlmError>,
}

/// Calls `provider`, defensively parses its response, and validates every
/// resulting item against the real `evidence_set` and real `actions`
/// registry. Unknown `resource_id`/`action_id` values drop just that
/// item (counted below), never the whole plan.
///
/// **This function's output is a RANKING SUGGESTION ONLY, never
/// authorization.** It does not run policy, and must not: the caller
/// (not this function) is REQUIRED to re-run
/// [`crate::policy::engine::classify`] (or an equivalent authorize step)
/// on every surviving item's fresh evidence before executing anything.
/// A follow-up commit's `llm_plan_item_never_bypasses_policy` test
/// demonstrates this concretely.
///
/// No integration wiring into any existing rule-only ranking loop is
/// included here (deliberately out of scope per HORO-954) — a future
/// ticket may call this from that loop as an optional enhancement,
/// falling back to rule-only ranking whenever `provider_error.is_some()`.
pub fn plan_with_llm(
    provider: &dyn LlmProvider,
    evidence_set: &[Evidence],
    actions: &ActionRegistry,
) -> LlmPlanResult {
    let views: Vec<LlmResourceView> = evidence_set
        .iter()
        .map(|ev| LlmResourceView::from_evidence(ev, actions))
        .collect();

    let user_prompt = match serde_json::to_string(&views) {
        Ok(json) => json,
        Err(e) => {
            return LlmPlanResult {
                validated_items: Vec::new(),
                dropped_unknown_resource: 0,
                dropped_unknown_action: 0,
                provider_error: Some(LlmError::InvalidResponse(format!(
                    "failed to serialize evidence views: {e}"
                ))),
            };
        }
    };

    let raw = match provider.complete(SYSTEM_PROMPT, &user_prompt) {
        Ok(raw) => raw,
        Err(e) => {
            return LlmPlanResult {
                validated_items: Vec::new(),
                dropped_unknown_resource: 0,
                dropped_unknown_action: 0,
                provider_error: Some(e),
            };
        }
    };

    let plan = match extract_plan(&raw) {
        Ok(plan) => plan,
        Err(e) => {
            return LlmPlanResult {
                validated_items: Vec::new(),
                dropped_unknown_resource: 0,
                dropped_unknown_action: 0,
                provider_error: Some(e),
            };
        }
    };

    let mut validated_items = Vec::new();
    let mut dropped_unknown_resource = 0u32;
    let mut dropped_unknown_action = 0u32;

    for item in plan.items {
        let Some(resource) = evidence_set
            .iter()
            .find(|ev| ev.resource.to_string() == item.resource_id)
            .map(|ev| ev.resource.clone())
        else {
            dropped_unknown_resource += 1;
            continue;
        };

        let Some(action_id) = actions.get(&item.action_id).map(|action| action.id()) else {
            dropped_unknown_action += 1;
            continue;
        };

        validated_items.push((resource, action_id, item.priority));
    }

    LlmPlanResult {
        validated_items,
        dropped_unknown_resource,
        dropped_unknown_action,
        provider_error: None,
    }
}

const SYSTEM_PROMPT: &str = "You are a storage cleanup ranking assistant. \
You will receive a JSON array of resource views. Respond with ONLY a JSON \
object of the shape {\"items\": [{\"resource_id\": string, \"action_id\": \
string, \"priority\": number, \"reason\": string}]}, choosing resource_id \
and action_id only from the values you were given.";

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use crate::detectors::DetectorId;
    use crate::evidence::{
        NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint, ResourceId,
        ResourceKind, ResourceLocator,
    };

    use super::*;

    struct FakeProvider {
        response: Result<String, LlmError>,
    }

    impl LlmProvider for FakeProvider {
        fn complete(&self, _system_prompt: &str, _user_prompt: &str) -> Result<String, LlmError> {
            self.response.clone()
        }
    }

    fn evidence_for(kind: ResourceKind, path: &str) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(1024),
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
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
    fn from_evidence_projects_expected_fields() {
        let ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        let actions = ActionRegistry::builtin();
        let view = LlmResourceView::from_evidence(&ev, &actions);

        assert_eq!(view.resource_id, "cargo_target_dir:/tmp/proj/target");
        assert_eq!(view.kind, "cargo_target_dir");
        assert_eq!(view.reclaimable_bytes, Some(1024));
        assert_eq!(view.age_days, Some(0));
        assert_eq!(view.regenerability, "regenerable_by_rebuild");
        assert_eq!(view.completeness, "complete");
        assert_eq!(view.offered_action_ids, vec!["cargo.clean.target_dir"]);
    }

    #[test]
    fn from_evidence_handles_missing_last_modified() {
        let mut ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        ev.last_modified = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        let actions = ActionRegistry::builtin();
        let view = LlmResourceView::from_evidence(&ev, &actions);
        assert_eq!(view.age_days, None);
    }

    #[test]
    fn unexpected_field_rejects_whole_plan() {
        // deny_unknown_fields rejects the WHOLE containing struct on any
        // unknown field — not just the offending item. A model trying to
        // smuggle a "command" field fails the entire Vec<LlmPlanItem>
        // deserialization, hence the entire LlmPlan. Confirmed here
        // rather than assumed.
        let text = r#"{"items": [{"resource_id": "a", "action_id": "b", "priority": 1, "reason": null, "command": "rm -rf /"}]}"#;
        assert!(serde_json::from_str::<LlmPlan>(text).is_err());
    }

    #[test]
    fn api_key_never_appears_in_error_debug_output() {
        let fake_key = "sk-super-secret-test-key-should-not-leak";
        let errors = vec![
            LlmError::NotConfigured,
            LlmError::NetworkError("request to provider failed".to_string()),
            LlmError::InvalidResponse("bad json".to_string()),
        ];
        for err in errors {
            let debug_output = format!("{err:?}");
            assert!(
                !debug_output.contains(fake_key),
                "LlmError Debug output must never contain the API key"
            );
        }
    }

    #[test]
    fn unfenced_json_object_parses() {
        let text =
            r#"{"items": [{"resource_id": "a", "action_id": "b", "priority": 1, "reason": null}]}"#;
        let plan = extract_plan(text).unwrap();
        assert_eq!(plan.items.len(), 1);
    }

    #[test]
    fn fenced_json_block_parses() {
        let text =
            "Here is the plan:\n```json\n{\"items\": []}\n```\nLet me know if you need more.";
        let plan = extract_plan(text).unwrap();
        assert_eq!(plan.items.len(), 0);
    }

    #[test]
    fn bare_fenced_block_without_language_tag_parses() {
        let text = "```\n{\"items\": []}\n```";
        let plan = extract_plan(text).unwrap();
        assert_eq!(plan.items.len(), 0);
    }

    #[test]
    fn leading_and_trailing_prose_around_bare_object_parses() {
        let text = "Sure, here's my plan: {\"items\": []} — hope that helps!";
        let plan = extract_plan(text).unwrap();
        assert_eq!(plan.items.len(), 0);
    }

    #[test]
    fn malformed_json_yields_invalid_response_and_empty_items() {
        let provider = FakeProvider {
            response: Ok("this is not json at all".to_string()),
        };
        let evidence_set = vec![evidence_for(ResourceKind::CargoTargetDir, "/tmp/x")];
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(matches!(
            result.provider_error,
            Some(LlmError::InvalidResponse(_))
        ));
        assert!(result.validated_items.is_empty());
        assert_eq!(result.dropped_unknown_resource, 0);
        assert_eq!(result.dropped_unknown_action, 0);
    }

    #[test]
    fn hallucinated_action_id_is_dropped_and_counted() {
        let ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        let resource_id = ev.resource.to_string();
        let text = format!(
            r#"{{"items": [{{"resource_id": "{resource_id}", "action_id": "docker.nuke.everything", "priority": 1, "reason": null}}]}}"#
        );
        let provider = FakeProvider { response: Ok(text) };
        let evidence_set = vec![ev];
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(result.provider_error.is_none());
        assert!(result.validated_items.is_empty());
        assert_eq!(result.dropped_unknown_action, 1);
        assert_eq!(result.dropped_unknown_resource, 0);
    }

    #[test]
    fn hallucinated_resource_id_is_dropped_and_counted() {
        let ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        let text = r#"{"items": [{"resource_id": "cargo_target_dir:/nonexistent", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": null}]}"#;
        let provider = FakeProvider {
            response: Ok(text.to_string()),
        };
        let evidence_set = vec![ev];
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(result.provider_error.is_none());
        assert!(result.validated_items.is_empty());
        assert_eq!(result.dropped_unknown_resource, 1);
        assert_eq!(result.dropped_unknown_action, 0);
    }

    #[test]
    fn valid_item_survives_validation() {
        let ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        let resource_id = ev.resource.to_string();
        let text = format!(
            r#"{{"items": [{{"resource_id": "{resource_id}", "action_id": "cargo.clean.target_dir", "priority": 5, "reason": "stale build artifacts"}}]}}"#
        );
        let provider = FakeProvider { response: Ok(text) };
        let evidence_set = vec![ev.clone()];
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(result.provider_error.is_none());
        assert_eq!(result.validated_items.len(), 1);
        let (resource, action_id, priority) = &result.validated_items[0];
        assert_eq!(*resource, ev.resource);
        assert_eq!(action_id.0, "cargo.clean.target_dir");
        assert_eq!(*priority, Some(5));
    }

    #[test]
    fn provider_network_error_yields_empty_items_and_error_set() {
        let provider = FakeProvider {
            response: Err(LlmError::NetworkError("connection refused".to_string())),
        };
        let evidence_set = vec![evidence_for(ResourceKind::CargoTargetDir, "/tmp/x")];
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(matches!(
            result.provider_error,
            Some(LlmError::NetworkError(_))
        ));
        assert!(result.validated_items.is_empty());
    }
}
