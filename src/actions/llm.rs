//! Optional BYOK LLM planner (HORO-954).
//!
//! **What this module is**: an advisory ranking suggestion over evidence
//! the crate already collected. [`LlmResourceView`] is an explicit,
//! bounded projection of one [`Evidence`] record, safe to serialize and
//! hand to a model. [`LlmPlan`]/[`LlmPlanItem`] are the model-facing
//! response shape, and [`plan_with_llm`] is the entry point that calls a
//! provider and validates its response.
//!
//! **What this module is NOT, and never will be**: an authorization
//! mechanism. `plan_with_llm`'s output is a ranking suggestion only — it
//! never calls [`crate::policy::classify`]/`authorize`, and it must not.
//! Every surviving `(ResourceId, ActionId, priority)` tuple still has to
//! go through the exact same policy classification any other candidate
//! resource would, in the caller, before anything executes. Keeping
//! "recommend" (this module) and "decide" (`crate::policy`) in separate
//! modules with no call edge from this one to that one is what makes it
//! structurally impossible for a compromised/hallucinating LLM response
//! to bypass policy — see `llm_plan_item_never_bypasses_policy` in this
//! module's tests for a concrete demonstration.
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

use std::path::PathBuf;

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

/// Label naming the wire protocol a provider speaks, recorded in
/// [`LlmError::ProviderStatus`] so a failure says which surface it was
/// talking to. This is a diagnostic string, not a dispatch mechanism:
/// [`OpenAiCompatibleProvider`] speaks exactly one protocol, and a second
/// protocol should arrive as a second provider type rather than as a
/// branch inside this one.
pub const API_STYLE_CHAT_COMPLETIONS: &str = "openai:chat_completions";

/// How many bytes of a provider's error body [`LlmError::ProviderStatus`]
/// retains. Bounded because a misconfigured base URL frequently returns an
/// HTML error page or a multi-megabyte proxy dump, and this string ends up
/// in `--json` output and in logs.
const PROVIDER_ERROR_BODY_LIMIT: usize = 512;

/// Why an [`LlmProvider`] call failed. Its `Debug` and `Display` output are
/// both safe to log: no variant embeds the API key. For
/// [`LlmError::ProviderStatus`] that is enforced actively rather than
/// assumed — `redact_secret` removes the key from the provider's own
/// response body before it is stored, because a provider that echoes the
/// credential it rejected back in its error message would otherwise leak
/// it into every log and `--json` report (see
/// `provider_status_redacts_echoed_api_key` in this module's tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmError {
    NotConfigured,
    /// The request never produced an HTTP response: DNS failure, TLS
    /// failure, connection refused, timeout. Distinct from
    /// [`LlmError::ProviderStatus`], where the provider *did* answer —
    /// conflating the two is what made a BYOK 401 undiagnosable
    /// (HORO-1299).
    NetworkError(String),
    /// The provider answered with a non-2xx status. Carries only
    /// non-secret diagnostics, deliberately excluding the scheme, host and
    /// query string of the request: the path alone is what distinguishes a
    /// base-URL mistake (`/chat/completions` vs `/v1/chat/completions`),
    /// while a host may be private infrastructure and a query string may
    /// carry a credential.
    ProviderStatus {
        status: u16,
        api_style: String,
        endpoint_path: String,
        request_id: Option<String>,
        body_excerpt: String,
    },
    InvalidResponse(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::NotConfigured => write!(
                f,
                "LLM provider is not configured (GLOMERIS_LLM_API_KEY, \
                 GLOMERIS_LLM_BASE_URL, GLOMERIS_LLM_MODEL must all be set)"
            ),
            LlmError::NetworkError(detail) => {
                write!(f, "provider unreachable: {detail}")
            }
            LlmError::ProviderStatus {
                status,
                api_style,
                endpoint_path,
                request_id,
                body_excerpt,
            } => {
                write!(
                    f,
                    "provider returned HTTP {status} for {api_style} POST {endpoint_path}"
                )?;
                if let Some(id) = request_id {
                    write!(f, " (request_id={id})")?;
                }
                if !body_excerpt.is_empty() {
                    write!(f, ": {body_excerpt}")?;
                }
                Ok(())
            }
            LlmError::InvalidResponse(detail) => {
                write!(f, "provider response was unusable: {detail}")
            }
        }
    }
}

/// Returns the path component of `url` for diagnostics — everything from
/// the first `/` after the authority up to any `?`/`#`. Drops the scheme
/// and host (potentially private infrastructure) and the query string
/// (which some gateways use to carry an API key). Falls back to `"/"` when
/// `url` has no path, and returns the input unchanged when it has no
/// recognizable authority, so a malformed base URL is still visible in the
/// message that reports the failure.
fn diagnostic_endpoint_path(url: &str) -> String {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => return url.to_string(),
    };
    let path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => return "/".to_string(),
    };
    let end = path.find(['?', '#']).unwrap_or(path.len());
    path[..end].to_string()
}

/// Removes every occurrence of `secret` from `text`, collapses whitespace
/// so the result is one log-safe line, and truncates it to
/// [`PROVIDER_ERROR_BODY_LIMIT`] bytes on a character boundary.
///
/// The empty-`secret` guard is load-bearing: `str::replace("", _)` inserts
/// the replacement between every character, so without it an unset API key
/// would corrupt the diagnostic instead of redacting nothing.
fn redact_secret(text: &str, secret: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = if secret.is_empty() {
        collapsed
    } else {
        collapsed.replace(secret, "<REDACTED>")
    };

    if redacted.len() <= PROVIDER_ERROR_BODY_LIMIT {
        return redacted;
    }
    let mut cut = PROVIDER_ERROR_BODY_LIMIT;
    while cut > 0 && !redacted.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… (truncated)", &redacted[..cut])
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
            .config()
            // Report a non-2xx status as a *response* rather than a
            // transport error, so its status, request id and body are
            // available to build an `LlmError::ProviderStatus`. With
            // ureq's default (`true`), every HTTP status collapsed into an
            // opaque `NetworkError("http status: 401")` and the provider's
            // own explanation was discarded unread — HORO-1299.
            .http_status_as_error(false)
            .build()
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

        let status = response.status().as_u16();
        // Read before `into_body()` consumes the response. `x-request-id`
        // is the near-universal convention among OpenAI-compatible
        // gateways; a provider that omits it simply yields `None`.
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        let body_text = response.into_body().read_to_string();

        if !(200..300).contains(&status) {
            // A failed body read must not mask the status: the status is
            // the most diagnostic part of the failure, so it is reported
            // either way.
            let body_excerpt = match &body_text {
                Ok(text) => redact_secret(text, &self.api_key),
                Err(e) => format!("<error body unreadable: {e}>"),
            };
            return Err(LlmError::ProviderStatus {
                status,
                api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
                endpoint_path: diagnostic_endpoint_path(&url),
                request_id,
                body_excerpt,
            });
        }

        let text = body_text
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

/// A [`LlmProvider`] backing `glomeris llm-plan --plan-file <path>`
/// (HORO-1008): returns the fixture file's raw bytes as-is, exactly as if
/// they were the model's raw response text. This lets the CLI's
/// `--plan-file` path exercise the identical `extract_plan`/`LlmPlan`/
/// [`plan_with_llm`] validation pipeline the live provider uses,
/// deterministically and with no network call.
pub struct FilePlanProvider {
    pub path: PathBuf,
}

impl LlmProvider for FilePlanProvider {
    fn complete(&self, _system_prompt: &str, _user_prompt: &str) -> Result<String, LlmError> {
        std::fs::read_to_string(&self.path).map_err(|e| {
            LlmError::InvalidResponse(format!(
                "failed to read plan file {}: {e}",
                self.path.display()
            ))
        })
    }
}

/// Reads BYOK LLM credentials for live mode from the environment:
/// `GLOMERIS_LLM_API_KEY`, `GLOMERIS_LLM_BASE_URL`, `GLOMERIS_LLM_MODEL`.
/// All three are required, with no default `base_url` — a missing or empty
/// value for any of them returns [`LlmError::NotConfigured`] rather than
/// partially constructing a provider. `std::env::var` is called ONLY
/// inside this function: the key value is held just long enough to build
/// the returned [`OpenAiCompatibleProvider`] and is never logged, printed,
/// or returned any other way.
pub fn provider_from_env() -> Result<OpenAiCompatibleProvider, LlmError> {
    let api_key = std::env::var("GLOMERIS_LLM_API_KEY").unwrap_or_default();
    let base_url = std::env::var("GLOMERIS_LLM_BASE_URL").unwrap_or_default();
    let model = std::env::var("GLOMERIS_LLM_MODEL").unwrap_or_default();

    if api_key.is_empty() || base_url.is_empty() || model.is_empty() {
        return Err(LlmError::NotConfigured);
    }

    Ok(OpenAiCompatibleProvider {
        base_url,
        api_key,
        model,
    })
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
            reclaimable_bytes_is_lower_bound: false,
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
    fn openai_error_never_leaks_api_key() {
        // OpenAiCompatibleProvider deliberately does not derive Debug (see
        // its doc comment) — this test proves the invariant a different
        // way: a real complete() call's error output never contains the
        // key, regardless of which branch produced it.
        let fake_key = "sk-super-secret-test-key-should-not-leak";
        let provider = OpenAiCompatibleProvider {
            // Loopback with (almost certainly) nothing listening: the
            // kernel refuses the connection immediately (ECONNREFUSED)
            // rather than timing out, unlike a genuinely unroutable
            // address or port 0 (which some platforms handle
            // unpredictably) — keeps this test fast and deterministic.
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: fake_key.to_string(),
            model: "test-model".to_string(),
        };
        let result = provider.complete("system", "user");
        let output = format!("{result:?}");
        assert!(
            !output.contains(fake_key),
            "provider error output must never contain the API key"
        );
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
    fn llm_plan_item_never_bypasses_policy() {
        // Construct evidence for a genuinely Protected resource (SSH key
        // material — mirrors policy::engine's own
        // protected_path_wins_even_with_stale_evidence test).
        let mut ev = evidence_for(ResourceKind::CargoTargetDir, "/Users/x/.ssh/id_ed25519");
        ev.resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/.ssh/id_ed25519")),
        );

        let resource_id = ev.resource.to_string();
        let text = format!(
            r#"{{"items": [{{"resource_id": "{resource_id}", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": "looks stale"}}]}}"#
        );
        let provider = FakeProvider { response: Ok(text) };
        let evidence_set = vec![ev.clone()];
        let actions = ActionRegistry::builtin();

        // The item survives plan_with_llm's validation: resource_id and
        // action_id are both real.
        let result = plan_with_llm(&provider, &evidence_set, &actions);
        assert!(result.provider_error.is_none());
        assert_eq!(result.validated_items.len(), 1);

        // But policy — which the caller is REQUIRED to re-run on every
        // surviving item — still classifies this resource as Protected,
        // regardless of what the LLM said. This is exactly why
        // plan_with_llm never calls policy itself: the enforcement lives
        // entirely at the policy layer.
        let decision = crate::policy::classify(
            &ev,
            &crate::policy::PolicyConfig::default(),
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(decision.class, crate::policy::PolicyClass::Protected);
        assert_eq!(
            decision.reasons,
            vec![crate::policy::ReasonCode::ProtectedCredentialMaterial]
        );
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
    fn file_plan_provider_round_trips_unfenced_fixture_text() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-file-plan-provider-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.json");
        let text =
            r#"{"items": [{"resource_id": "a", "action_id": "b", "priority": 1, "reason": null}]}"#;
        std::fs::write(&path, text).unwrap();

        let provider = FilePlanProvider { path: path.clone() };
        let raw = provider.complete("system", "user").unwrap();
        assert_eq!(raw, text);
        let plan = extract_plan(&raw).unwrap();
        assert_eq!(plan.items.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_plan_provider_round_trips_fenced_fixture_text() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-file-plan-provider-fenced-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.md");
        let text = "Here is the plan:\n```json\n{\"items\": []}\n```\n";
        std::fs::write(&path, text).unwrap();

        let provider = FilePlanProvider { path: path.clone() };
        let raw = provider.complete("system", "user").unwrap();
        assert_eq!(raw, text);
        let plan = extract_plan(&raw).unwrap();
        assert_eq!(plan.items.len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_plan_provider_reports_unreadable_path() {
        let provider = FilePlanProvider {
            path: PathBuf::from("/definitely/does/not/exist/glomeris-fixture.json"),
        };
        let result = provider.complete("system", "user");
        assert!(matches!(result, Err(LlmError::InvalidResponse(_))));
    }

    #[test]
    fn provider_from_env_is_not_configured_when_env_vars_are_unset() {
        // Deliberately does not set/read any of the three GLOMERIS_LLM_*
        // vars — only asserts the NotConfigured outcome for whatever state
        // the test process's environment is actually in for these
        // crate-specific names, never a secret's value.
        assert!(std::env::var("GLOMERIS_LLM_API_KEY").is_err());
        assert!(std::env::var("GLOMERIS_LLM_BASE_URL").is_err());
        assert!(std::env::var("GLOMERIS_LLM_MODEL").is_err());
        assert!(matches!(provider_from_env(), Err(LlmError::NotConfigured)));
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

    // ---- Provider wire-protocol tests (HORO-1299) -------------------
    //
    // These drive the real `OpenAiCompatibleProvider` against a loopback
    // mock so endpoint construction, the Authorization header, model
    // serialization and every failure classification are asserted against
    // actual bytes on a socket rather than against a hand-written fake.
    // The mock is ~40 lines of `std::net` and needs no dev-dependency.

    /// Everything the mock observed about the provider's request.
    struct CapturedRequest {
        request_line: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    /// Serves exactly one HTTP request on an ephemeral loopback port and
    /// replies with `status_line`/`extra_headers`/`body`. Returns the base
    /// URL to configure a provider with — deliberately including a `/v1`
    /// segment so every test also pins the "base URL is the API root, and
    /// `/chat/completions` is appended to it verbatim" contract — plus a
    /// handle yielding the captured request.
    ///
    /// `extra_headers`, when non-empty, must be CRLF-terminated.
    fn serve_one(
        status_line: &str,
        extra_headers: &str,
        body: &str,
    ) -> (String, std::thread::JoinHandle<CapturedRequest>) {
        use std::io::{BufRead, BufReader, Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local_addr").port();

        let status_line = status_line.to_string();
        let extra_headers = extra_headers.to_string();
        let body = body.to_string();

        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);

            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read request line");

            let mut headers = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read header line");
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if let Some((key, value)) = trimmed.split_once(':') {
                    let key = key.trim().to_string();
                    let value = value.trim().to_string();
                    if key.eq_ignore_ascii_case("content-length") {
                        content_length = value.parse().unwrap_or(0);
                    }
                    headers.push((key, value));
                }
            }

            let mut body_buf = vec![0u8; content_length];
            reader.read_exact(&mut body_buf).expect("read request body");

            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n{extra_headers}\r\n{body}",
                body.len()
            );
            let mut stream = reader.into_inner();
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");

            CapturedRequest {
                request_line: request_line.trim_end().to_string(),
                headers,
                body: String::from_utf8_lossy(&body_buf).to_string(),
            }
        });

        (format!("http://127.0.0.1:{port}/v1"), handle)
    }

    const OK_COMPLETION: &str = r#"{"choices":[{"message":{"content":"{\"items\": []}"}}]}"#;

    /// A key shaped like a real one, so a redaction assertion that passed
    /// only because the needle was trivially short would not fool us.
    const TEST_KEY: &str = "sk-test-do-not-leak-0123456789abcdef";

    fn provider_for(base_url: &str) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider {
            base_url: base_url.to_string(),
            api_key: TEST_KEY.to_string(),
            model: "example-model".to_string(),
        }
    }

    #[test]
    fn provider_appends_chat_completions_to_base_url_verbatim() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", OK_COMPLETION);
        provider_for(&base).complete("system", "user").expect("ok");
        let request = server.join().expect("server thread");

        // `GLOMERIS_LLM_BASE_URL` is the API ROOT: the provider appends
        // `/chat/completions` and inserts no `/v1` of its own, so a base
        // already ending in `/v1` yields exactly one `/v1` segment.
        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
        assert!(!request.request_line.contains("/v1/v1"));
    }

    #[test]
    fn provider_tolerates_trailing_slash_on_base_url() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", OK_COMPLETION);
        let mut provider = provider_for(&base);
        provider.base_url.push('/');
        provider.complete("system", "user").expect("ok");
        let request = server.join().expect("server thread");

        assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
    }

    #[test]
    fn provider_sends_bearer_authorization_header() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", OK_COMPLETION);
        provider_for(&base).complete("system", "user").expect("ok");
        let request = server.join().expect("server thread");

        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TEST_KEY}").as_str())
        );
        assert_eq!(request.header("content-type"), Some("application/json"));
    }

    #[test]
    fn provider_serializes_model_and_both_message_roles() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", OK_COMPLETION);
        provider_for(&base)
            .complete("SYSTEM-PROMPT", "USER-PROMPT")
            .expect("ok");
        let request = server.join().expect("server thread");

        let sent: serde_json::Value = serde_json::from_str(&request.body).expect("request is JSON");
        assert_eq!(sent["model"], "example-model");
        assert_eq!(sent["messages"][0]["role"], "system");
        assert_eq!(sent["messages"][0]["content"], "SYSTEM-PROMPT");
        assert_eq!(sent["messages"][1]["role"], "user");
        assert_eq!(sent["messages"][1]["content"], "USER-PROMPT");
        // The request body carries the prompts and nothing else that could
        // be mistaken for a credential.
        assert!(!request.body.contains(TEST_KEY));
    }

    #[test]
    fn provider_returns_completion_content_on_success() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", OK_COMPLETION);
        let text = provider_for(&base).complete("system", "user").expect("ok");
        server.join().expect("server thread");

        assert_eq!(text, r#"{"items": []}"#);
    }

    #[test]
    fn provider_401_yields_provider_status_not_network_error() {
        // The exact regression behind HORO-1299: this used to surface as
        // `NetworkError("http status: 401")` with the body discarded.
        let (base, server) = serve_one(
            "HTTP/1.1 401 Unauthorized",
            "",
            r#"{"error":{"message":"invalid api key","code":"invalid_api_key"}}"#,
        );
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("401 must be an error");
        server.join().expect("server thread");

        let LlmError::ProviderStatus {
            status,
            api_style,
            endpoint_path,
            body_excerpt,
            ..
        } = &error
        else {
            panic!("expected ProviderStatus, got {error:?}");
        };
        assert_eq!(*status, 401);
        assert_eq!(api_style, API_STYLE_CHAT_COMPLETIONS);
        assert_eq!(endpoint_path, "/v1/chat/completions");
        assert!(body_excerpt.contains("invalid_api_key"));
    }

    #[test]
    fn provider_403_yields_provider_status_with_body() {
        let (base, server) = serve_one(
            "HTTP/1.1 403 Forbidden",
            "",
            r#"{"error":{"message":"Selected provider is forbidden"}}"#,
        );
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("403 must be an error");
        server.join().expect("server thread");

        let LlmError::ProviderStatus {
            status,
            body_excerpt,
            ..
        } = &error
        else {
            panic!("expected ProviderStatus, got {error:?}");
        };
        assert_eq!(*status, 403);
        assert!(body_excerpt.contains("forbidden"));
    }

    #[test]
    fn provider_404_reports_the_path_that_was_missing() {
        // The single most useful base-URL-misconfiguration signal: the
        // path actually requested is in the error, so "host root vs API
        // root" is diagnosable without a packet capture.
        let (base, server) = serve_one("HTTP/1.1 404 Not Found", "", "not found");
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("404 must be an error");
        server.join().expect("server thread");

        let LlmError::ProviderStatus {
            status,
            endpoint_path,
            ..
        } = &error
        else {
            panic!("expected ProviderStatus, got {error:?}");
        };
        assert_eq!(*status, 404);
        assert_eq!(endpoint_path, "/v1/chat/completions");
    }

    #[test]
    fn provider_status_redacts_echoed_api_key() {
        // A gateway that echoes the rejected credential back in its error
        // message must not turn Glomeris's own diagnostics into the leak.
        let (base, server) = serve_one(
            "HTTP/1.1 401 Unauthorized",
            "",
            &format!(r#"{{"error":{{"message":"key {TEST_KEY} is not valid"}}}}"#),
        );
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("401 must be an error");
        server.join().expect("server thread");

        let rendered = format!("{error:?} {error}");
        assert!(
            !rendered.contains(TEST_KEY),
            "provider diagnostics must never echo the API key: {rendered}"
        );
        assert!(rendered.contains("<REDACTED>"));
    }

    #[test]
    fn provider_status_captures_request_id_when_present() {
        let (base, server) = serve_one(
            "HTTP/1.1 500 Internal Server Error",
            "x-request-id: req-abc-123\r\n",
            "upstream exploded",
        );
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("500 must be an error");
        server.join().expect("server thread");

        let LlmError::ProviderStatus { request_id, .. } = &error else {
            panic!("expected ProviderStatus, got {error:?}");
        };
        assert_eq!(request_id.as_deref(), Some("req-abc-123"));
        assert!(format!("{error}").contains("request_id=req-abc-123"));
    }

    #[test]
    fn provider_status_body_excerpt_is_bounded() {
        let huge = "x".repeat(50_000);
        let (base, server) = serve_one("HTTP/1.1 502 Bad Gateway", "", &huge);
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("502 must be an error");
        server.join().expect("server thread");

        let LlmError::ProviderStatus { body_excerpt, .. } = &error else {
            panic!("expected ProviderStatus, got {error:?}");
        };
        assert!(
            body_excerpt.len() < PROVIDER_ERROR_BODY_LIMIT + 32,
            "excerpt must stay bounded, got {} bytes",
            body_excerpt.len()
        );
        assert!(body_excerpt.ends_with("… (truncated)"));
    }

    #[test]
    fn provider_empty_success_body_yields_invalid_response() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", "");
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("empty body is unusable");
        server.join().expect("server thread");

        assert!(matches!(error, LlmError::InvalidResponse(_)));
    }

    #[test]
    fn provider_non_json_success_body_yields_invalid_response() {
        let (base, server) = serve_one("HTTP/1.1 200 OK", "", "<html>gateway splash</html>");
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("non-JSON body is unusable");
        server.join().expect("server thread");

        let LlmError::InvalidResponse(detail) = &error else {
            panic!("expected InvalidResponse, got {error:?}");
        };
        assert!(detail.contains("not JSON"));
    }

    #[test]
    fn provider_success_body_missing_content_yields_invalid_response() {
        // Well-formed JSON, valid HTTP 200, but not a completion — e.g. a
        // model/provider error returned with a 200 status, which some
        // gateways do.
        let (base, server) = serve_one(
            "HTTP/1.1 200 OK",
            "",
            r#"{"error":{"message":"model not found"}}"#,
        );
        let error = provider_for(&base)
            .complete("system", "user")
            .expect_err("missing content is unusable");
        server.join().expect("server thread");

        let LlmError::InvalidResponse(detail) = &error else {
            panic!("expected InvalidResponse, got {error:?}");
        };
        assert!(detail.contains("choices[0].message.content"));
    }

    #[test]
    fn provider_connection_closed_without_response_yields_network_error() {
        // Transport failure with a server that accepts and then hangs up:
        // distinct from a non-2xx response, and must stay a NetworkError.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local_addr").port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            drop(stream);
        });

        let error = provider_for(&format!("http://127.0.0.1:{port}/v1"))
            .complete("system", "user")
            .expect_err("closed connection must fail");
        server.join().expect("server thread");

        assert!(
            matches!(error, LlmError::NetworkError(_)),
            "expected NetworkError, got {error:?}"
        );
        assert!(!format!("{error:?}").contains(TEST_KEY));
    }

    #[test]
    fn plan_with_llm_falls_back_to_empty_on_provider_status() {
        // A provider failure is never fatal and never grants authority: the
        // report comes back with zero validated items and the error set, so
        // the caller falls back to rule-only ranking.
        let (base, server) = serve_one("HTTP/1.1 401 Unauthorized", "", r#"{"error":"nope"}"#);
        let provider = provider_for(&base);
        let evidence_set = vec![evidence_for(
            ResourceKind::CargoTargetDir,
            "/tmp/proj/target",
        )];
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);
        server.join().expect("server thread");

        assert!(matches!(
            result.provider_error,
            Some(LlmError::ProviderStatus { status: 401, .. })
        ));
        assert!(result.validated_items.is_empty());
    }

    // ---- Diagnostic helper units ------------------------------------

    #[test]
    fn diagnostic_endpoint_path_drops_scheme_host_and_query() {
        assert_eq!(
            diagnostic_endpoint_path("https://gateway.example.com/v1/chat/completions"),
            "/v1/chat/completions"
        );
        // A query string may carry a credential on some gateways, so it is
        // never retained.
        assert_eq!(
            diagnostic_endpoint_path("https://gateway.example.com/v1/chat/completions?key=SECRET"),
            "/v1/chat/completions"
        );
        assert_eq!(
            diagnostic_endpoint_path("https://gateway.example.com/chat/completions#frag"),
            "/chat/completions"
        );
        // Host-root base URL with no path at all.
        assert_eq!(diagnostic_endpoint_path("https://gateway.example.com"), "/");
        // Non-URL input is returned as-is so a malformed base URL is still
        // visible in the error that reports it.
        assert_eq!(diagnostic_endpoint_path("not-a-url"), "not-a-url");
    }

    #[test]
    fn diagnostic_endpoint_path_never_contains_the_host() {
        let path =
            diagnostic_endpoint_path("https://internal-gateway.corp.example/v1/chat/completions");
        assert!(!path.contains("internal-gateway"));
        assert!(!path.contains("corp.example"));
    }

    #[test]
    fn redact_secret_removes_every_occurrence_and_collapses_whitespace() {
        let body = format!("line one {TEST_KEY}\n  line two {TEST_KEY}\t end");
        let redacted = redact_secret(&body, TEST_KEY);

        assert!(!redacted.contains(TEST_KEY));
        assert_eq!(redacted.matches("<REDACTED>").count(), 2);
        assert!(!redacted.contains('\n'));
        assert_eq!(redacted, "line one <REDACTED> line two <REDACTED> end");
    }

    #[test]
    fn redact_secret_with_empty_secret_does_not_corrupt_the_body() {
        // `str::replace("", _)` inserts the replacement between every
        // character; the empty-secret guard is what prevents that.
        assert_eq!(redact_secret("plain body", ""), "plain body");
    }

    #[test]
    fn redact_secret_truncates_on_a_character_boundary() {
        // Multi-byte characters straddling the limit must not panic or
        // produce invalid UTF-8.
        let body = "é".repeat(PROVIDER_ERROR_BODY_LIMIT);
        let redacted = redact_secret(&body, TEST_KEY);

        assert!(redacted.ends_with("… (truncated)"));
        assert!(redacted.len() < PROVIDER_ERROR_BODY_LIMIT + 32);
    }

    #[test]
    fn provider_status_display_names_status_style_and_path() {
        let error = LlmError::ProviderStatus {
            status: 401,
            api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
            endpoint_path: "/v1/chat/completions".to_string(),
            request_id: None,
            body_excerpt: r#"{"error":"invalid api key"}"#.to_string(),
        };
        let rendered = format!("{error}");

        assert!(rendered.contains("HTTP 401"));
        assert!(rendered.contains("openai:chat_completions"));
        assert!(rendered.contains("/v1/chat/completions"));
        assert!(rendered.contains("invalid api key"));
        // No request id was present, so no empty placeholder is rendered.
        assert!(!rendered.contains("request_id="));
    }

    /// Not hypothetical: real gateways answer `401` with a completely empty
    /// body. The status, style and path must still be a usable sentence, with
    /// no dangling `:` separator for an excerpt that does not exist.
    #[test]
    fn provider_status_display_omits_the_separator_when_there_is_no_body() {
        let error = LlmError::ProviderStatus {
            status: 401,
            api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
            endpoint_path: "/v1/chat/completions".to_string(),
            request_id: None,
            body_excerpt: String::new(),
        };
        let rendered = format!("{error}");

        assert_eq!(
            rendered,
            "provider returned HTTP 401 for openai:chat_completions POST /v1/chat/completions"
        );
    }

    #[test]
    fn provider_bodyless_error_response_yields_provider_status() {
        let (base_url, handle) = serve_one("HTTP/1.1 401 Unauthorized", "", "");
        let provider = OpenAiCompatibleProvider {
            base_url,
            api_key: TEST_KEY.to_string(),
            model: "example-model".to_string(),
        };

        let error = provider
            .complete("s", "u")
            .expect_err("401 must be an error");
        handle.join().expect("server thread");

        match error {
            LlmError::ProviderStatus {
                status,
                body_excerpt,
                ..
            } => {
                assert_eq!(status, 401);
                assert!(
                    body_excerpt.is_empty(),
                    "an absent body must stay absent, not become a placeholder: {body_excerpt:?}"
                );
            }
            other => panic!("expected ProviderStatus, got {other:?}"),
        }
    }

    #[test]
    fn not_configured_display_names_all_three_env_vars() {
        let rendered = format!("{}", LlmError::NotConfigured);
        assert!(rendered.contains("GLOMERIS_LLM_API_KEY"));
        assert!(rendered.contains("GLOMERIS_LLM_BASE_URL"));
        assert!(rendered.contains("GLOMERIS_LLM_MODEL"));
    }

    #[test]
    fn api_key_never_appears_in_display_output_of_any_variant() {
        let errors = vec![
            LlmError::NotConfigured,
            LlmError::NetworkError("connection refused".to_string()),
            LlmError::InvalidResponse("bad json".to_string()),
            LlmError::ProviderStatus {
                status: 401,
                api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
                endpoint_path: "/v1/chat/completions".to_string(),
                request_id: Some("req-1".to_string()),
                body_excerpt: "<REDACTED> rejected".to_string(),
            },
        ];
        for error in errors {
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains(TEST_KEY), "leaked in: {rendered}");
        }
    }
}
