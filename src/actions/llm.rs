//! Optional BYOK LLM planner (HORO-954).
//!
//! **What this module is**: an advisory ranking suggestion over evidence
//! the crate already collected. [`LlmResourceView`] is an explicit,
//! bounded projection of one [`Evidence`] record, safe to serialize and
//! hand to a model. [`LlmPlan`]/[`LlmPlanItem`] are the model-facing
//! response shape, and [`plan_with_llm`] is the entry point that calls a
//! provider and validates its response.
//!
//! **What never leaves the machine (HORO-1298)**: a resource's real
//! identity. [`LlmRequestPayload`] is the whole egress surface — two
//! strings — and the `resource_id` inside it is a positional alias
//! ([`wire_resource_id`]), so no absolute path, home directory or account
//! name is transmitted even though the ranking task is unchanged: kind,
//! reclaimable size, age, regenerability, completeness and the offered
//! action ids are what a ranking decision is actually made from. The
//! alias table that maps a returned id back to a real [`ResourceId`]
//! exists only in this process's memory, which is why a model's answer
//! can still be resolved exactly while a model's knowledge of this
//! machine stays empty.
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
    /// The resource's WIRE identity — a positional alias
    /// ([`wire_resource_id`]) scoped to one request, never the real
    /// [`ResourceId`] and therefore never a filesystem path (HORO-1298).
    /// The real id lives only in [`LlmRequestPayload::aliases`], on this
    /// machine, and is what the response is resolved back to.
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
    ///
    /// `index` is this resource's position in the payload being built, and
    /// is the ONLY thing `resource_id` is derived from. Taking an index
    /// rather than a caller-supplied string is deliberate: there is no
    /// parameter here that a path could be passed through, so no call site
    /// — present or future — can reintroduce HORO-1298 by handing this
    /// function `ev.resource.to_string()`.
    pub fn from_evidence(ev: &Evidence, actions: &ActionRegistry, index: usize) -> Self {
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
            resource_id: wire_resource_id(index),
            kind: ev.resource.kind_tag(),
            reclaimable_bytes: ev.reclaimable_bytes.observed().copied(),
            age_days,
            regenerability: regenerability_tag(ev.regenerability),
            completeness: completeness_tag(&ev.completeness()),
            offered_action_ids: actions.ids_for_kind(ev.resource.kind),
        }
    }
}

/// The wire identity of the `index`-th resource of one request payload
/// (HORO-1298): `resource_1`, `resource_2`, … — a positional alias and
/// nothing else.
///
/// Positional deliberately, rather than a hash or other derivation of the
/// resource itself. A hashed path would be stable across invocations,
/// which sounds like an improvement until you count the keyspace: a path
/// such as `/Users/<name>/Library/Developer/Xcode/DerivedData` has exactly
/// one unknown segment, so any party holding a candidate username list can
/// confirm it against the hash offline. A stable pseudonym is also, by
/// construction, a cross-session correlation handle for the machine. An
/// index has neither property and costs nothing: the model is ranking the
/// resources it was just handed, not recognizing them from last week.
fn wire_resource_id(index: usize) -> String {
    format!("resource_{}", index + 1)
}

/// Everything machine-derived that leaves this host for one
/// [`plan_with_llm`] call, plus the purely local table needed to resolve
/// the model's answer back to real resources (HORO-1298).
///
/// `system_prompt` and `user_prompt` are the request verbatim — nothing
/// else about the local machine is added downstream (see
/// [`OpenAiCompatibleProvider::complete`], which sends exactly these two
/// strings plus the configured model name). That makes this type the one
/// place to audit for egress, and the thing
/// `glomeris llm-plan --print-payload` prints.
///
/// `aliases` is NOT serializable and never sent: it maps each wire id to
/// the real [`ResourceId`] it stands for, which is how the response's
/// `resource_id` values regain meaning locally without the model ever
/// having been told a path.
pub struct LlmRequestPayload {
    pub system_prompt: &'static str,
    pub user_prompt: String,
    aliases: Vec<(String, ResourceId)>,
}

impl LlmRequestPayload {
    /// The local wire-id -> real-resource table, in payload order. For
    /// local rendering and response resolution only — a caller that sends
    /// this anywhere has defeated the point of the type.
    pub fn aliases(&self) -> &[(String, ResourceId)] {
        &self.aliases
    }

    /// The real resource a returned wire id stands for, or `None` if the
    /// id was never sent. This is a lookup into a table this process built
    /// from resources it already discovered itself — a returned id selects
    /// from that table and can never introduce a resource of its own, let
    /// alone a path.
    fn resolve(&self, wire_id: &str) -> Option<&ResourceId> {
        self.aliases
            .iter()
            .find(|(id, _)| id == wire_id)
            .map(|(_, resource)| resource)
    }
}

/// Builds the exact payload one [`plan_with_llm`] call would send for
/// `evidence_set`, without sending it (HORO-1298). Deterministic: the same
/// evidence in the same order always produces byte-identical prompts, with
/// no clock or randomness involved, which is what makes
/// `--print-payload`'s output a truthful preview of the request rather
/// than an approximation of it.
pub fn build_request_payload(
    evidence_set: &[Evidence],
    actions: &ActionRegistry,
) -> Result<LlmRequestPayload, LlmError> {
    let views: Vec<LlmResourceView> = evidence_set
        .iter()
        .enumerate()
        .map(|(index, ev)| LlmResourceView::from_evidence(ev, actions, index))
        .collect();

    let aliases = evidence_set
        .iter()
        .enumerate()
        .map(|(index, ev)| (wire_resource_id(index), ev.resource.clone()))
        .collect();

    let user_prompt = serde_json::to_string(&views).map_err(|e| {
        LlmError::InvalidResponse(format!("failed to serialize evidence views: {e}"))
    })?;

    Ok(LlmRequestPayload {
        system_prompt: SYSTEM_PROMPT,
        user_prompt,
        aliases,
    })
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
    ///
    /// HORO-1308 carries it onward as
    /// [`ValidatedPlanItem::model_reason`], bounded and stripped by
    /// [`sanitize_model_reason`] first — it is the only display string in
    /// the whole plan pipeline whose content a provider chooses, so it is
    /// cleaned once, here, rather than at each of the surfaces that show it.
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

/// How many characters of an [`LlmPlanItem::reason`] survive into
/// [`ValidatedPlanItem::model_reason`] (HORO-1308).
///
/// Bounded for the same reason [`PROVIDER_ERROR_BODY_LIMIT`] is: this is
/// attacker-or-accident-controlled text from outside the process that ends
/// up in `--json` output, in the text printer, and — once HORO-1308's GUI
/// lands — inside a fixed-width menu-bar popover. A model that answers with
/// a 40 KB essay (or a prompt-injected wall of text designed to push the
/// real policy verdict off screen) must not be able to decide how much of
/// the UI it occupies. 400 characters is enough for a genuine one- or
/// two-sentence rationale and not enough to bury anything.
const MODEL_REASON_LIMIT: usize = 400;

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
    /// All three settings are present, but one of them cannot work —
    /// currently only a base URL [`validate_base_url`] refuses. Distinct from
    /// [`LlmError::NotConfigured`] because "you have not set this up" and
    /// "you set it up wrongly, here is what to change" are different problems
    /// with different fixes, and from [`LlmError::NetworkError`] because no
    /// request was ever attempted. Carries only Glomeris's own message: never
    /// the offending value, since the value a user most plausibly gets wrong
    /// by pasting is the one containing their credential.
    InvalidConfiguration(String),
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
            LlmError::InvalidConfiguration(detail) => {
                write!(f, "LLM provider configuration is invalid: {detail}")
            }
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
                // Last, after the provider's own words: Glomeris's reading of
                // what the provider said is a different kind of statement from
                // the quote itself, and putting it first would look like part
                // of the response. See [`host_root_rejection_hint`].
                if let Some(hint) = host_root_rejection_hint(endpoint_path) {
                    write!(f, " — {hint}")?;
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

/// Folds every run of whitespace into a single space, so a multi-line
/// provider body becomes one log-safe line.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncates to at most `limit` bytes, always on a character boundary, and
/// says so when it cut. Separate from [`collapse_whitespace`] because
/// [`redact_secret`] must do its replacement *between* the two: collapsing
/// first can rejoin a key that a provider wrapped across lines, and
/// truncating last is what stops a key straddling the cut from surviving in
/// half.
fn truncate_on_char_boundary(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… (truncated)", &text[..cut])
}

/// One bounded, single-line excerpt of provider-controlled text, for a
/// surface that has to show *something* the provider said without letting it
/// decide how much room it gets.
///
/// Public because `crate::cli`'s connection-test report needs the identical
/// treatment [`LlmError::ProviderStatus`]'s body excerpt already gets, and a
/// second hand-rolled truncation is exactly how one of them ends up slicing
/// a multi-byte character in half.
pub fn excerpt(text: &str, limit: usize) -> String {
    truncate_on_char_boundary(collapse_whitespace(text), limit)
}

/// Removes every occurrence of `secret` from `text`, collapses whitespace
/// so the result is one log-safe line, and truncates it to
/// [`PROVIDER_ERROR_BODY_LIMIT`] bytes on a character boundary.
///
/// The empty-`secret` guard is load-bearing: `str::replace("", _)` inserts
/// the replacement between every character, so without it an unset API key
/// would corrupt the diagnostic instead of redacting nothing.
fn redact_secret(text: &str, secret: &str) -> String {
    let collapsed = collapse_whitespace(text);
    let redacted = if secret.is_empty() {
        collapsed
    } else {
        collapsed.replace(secret, "<REDACTED>")
    };

    truncate_on_char_boundary(redacted, PROVIDER_ERROR_BODY_LIMIT)
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

/// The full request URL [`OpenAiCompatibleProvider::complete`] posts to,
/// derived from a base URL exactly as it does: one trailing slash tolerated,
/// `/chat/completions` appended verbatim, no `/v1` ever inserted or stripped.
///
/// Public so a caller can report *which* endpoint it is about to talk to
/// without rebuilding that rule — see `book/src/byok.md`, where configuring
/// the host root instead of the API root is documented as the single most
/// common misconfiguration. A second copy of this concatenation is how the
/// diagnostic that diagnoses that mistake would start lying about it.
pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// The secret-free path of [`chat_completions_url`], for diagnostics — see
/// [`diagnostic_endpoint_path`] for what is deliberately dropped.
pub fn chat_completions_endpoint_path(base_url: &str) -> String {
    diagnostic_endpoint_path(&chat_completions_url(base_url))
}

/// The endpoint path [`chat_completions_url`] produces from a base URL with no
/// path component at all — i.e. from the host root.
const HOST_ROOT_ENDPOINT_PATH: &str = "/chat/completions";

/// One sentence naming the host-root misconfiguration, for a rejection whose
/// endpoint path shows it (HORO-1355), or `None` when the path shows a
/// configured API root.
///
/// `book/src/byok.md` calls configuring the host root the most common BYOK
/// misconfiguration and prints the resulting 403 with a caret under the
/// missing segment, and [`validate_base_url`] already refuses the mirror-image
/// mistake by name. The half the documentation calls most common was the half
/// that arrived as three equally-weighted possibilities — a key, a path, a
/// model — with the key first. So a user re-pastes a working credential while
/// the address is what is wrong, which is exactly what happened on the founder
/// pass this ticket came from.
///
/// ## Why this is a hint and not a refusal
///
/// A path-less base URL is deliberately *accepted*: some OpenAI-compatible
/// services really do serve completions at their root, and no particular host
/// or path shape may be baked in here — the `/v1` below is what providers
/// usually do, not a rule this code enforces. So this cannot claim the address
/// is wrong — only that it is the likeliest explanation for a refusal, which is
/// true precisely because the alternative (a provider that serves the root and
/// refused for an unrelated reason) is rarer. Rejections at a configured API
/// root get nothing: a 401 from `…/v1/chat/completions` is about the key, and
/// saying otherwise would trade one misdirection for another.
///
/// Keyed off the endpoint path rather than the base URL because the path is
/// what the error already carries — the base URL is deliberately absent from
/// [`LlmError::ProviderStatus`] (it may be private infrastructure) — and
/// because that path is derived from the same concatenation the request used.
pub fn host_root_rejection_hint(endpoint_path: &str) -> Option<&'static str> {
    if endpoint_path == HOST_ROOT_ENDPOINT_PATH {
        Some(
            "the configured address has no path, so this request went to the host \
             root; most OpenAI-compatible providers serve their API under /v1, so a \
             missing /v1 is the likeliest cause",
        )
    } else {
        None
    }
}

/// The stable outcome token for one connection-test result (HORO-1309):
/// `None` for a provider that answered usably, otherwise the class of failure.
///
/// The SOLE producer of these five strings, which is what lets
/// `scripts/check-vocabulary-covers-cli-tokens.sh` diff them against the
/// macOS app's wording. A second `match` somewhere that also emitted
/// `"rejected"` would make that check misleading rather than merely
/// incomplete — see that script's own note on the three vocabularies it
/// cannot honestly verify for the same reason.
///
/// Why these boundaries: `unreachable` means no answer arrived at all,
/// `rejected` means the provider answered and refused, and conflating those
/// two is precisely what made a BYOK 401 undiagnosable in HORO-1299.
/// `unusable_response` keeps a working endpoint and credential from being
/// reported as a credential problem. `misconfigured` means nothing was sent
/// and the fix is local.
pub fn llm_check_outcome(error: Option<&LlmError>) -> &'static str {
    match error {
        None => "ok",
        Some(LlmError::NetworkError(_)) => "unreachable",
        Some(LlmError::ProviderStatus { .. }) => "rejected",
        Some(LlmError::InvalidResponse(_)) => "unusable_response",
        Some(LlmError::InvalidConfiguration(_)) => "misconfigured",
        // Unreachable from the connection test — a caller cannot obtain a
        // provider without being configured — but mapped rather than
        // panicking, because a connection test that crashes is worse than one
        // that is merely wrong about the category.
        Some(LlmError::NotConfigured) => "misconfigured",
    }
}

/// Rejects base URLs that [`chat_completions_url`] cannot correctly append to
/// (HORO-1309).
///
/// Every message names what to do instead, because this is the error a user
/// hits while setting BYOK up for the first time and the only information
/// they have is what Glomeris tells them. The alternative to checking here is
/// a 404 from a real provider, which diagnoses nothing.
///
/// This lives in Rust and is reported through `glomeris llm-check` rather
/// than being re-implemented in the macOS settings UI: a second copy of these
/// rules in Swift would drift, and the standing project rule keeps decision
/// logic out of the thin client.
///
/// Deliberately *not* rejected: `http://` (a local gateway on loopback is a
/// legitimate BYOK setup) and any particular host or path shape (AC 6 — no
/// company-specific host or model may be baked in).
pub fn validate_base_url(base_url: &str) -> Result<(), String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err("base URL is empty; set the API root of your provider, \
                    for example https://api.openai.com/v1"
            .to_string());
    }
    if trimmed != base_url {
        return Err("base URL has leading or trailing whitespace; \
                    remove it (a pasted URL often carries a trailing newline)"
            .to_string());
    }
    if base_url.contains(char::is_whitespace) {
        return Err("base URL contains a space; a URL cannot contain one".to_string());
    }

    let after_scheme = match base_url.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some((_, _)) => {
            return Err("base URL must start with https:// or http://".to_string());
        }
        None => {
            return Err("base URL has no scheme; it must start with https:// \
                        or http://"
                .to_string());
        }
    };
    if after_scheme.is_empty() || after_scheme.starts_with('/') {
        return Err("base URL has no host".to_string());
    }

    // A query or fragment must be refused rather than tolerated: appending
    // `/chat/completions` to `…/v1?key=…` puts the path *inside the query
    // string*, producing a request that cannot work and a diagnostic path of
    // `/v1` that misdescribes why. Refusing also keeps a credential a user
    // pasted into the URL out of the request Glomeris builds.
    if let Some(i) = base_url.find(['?', '#']) {
        let what = if base_url[i..].starts_with('?') {
            "a query string"
        } else {
            "a fragment"
        };
        return Err(format!(
            "base URL has {what}; give only the API root, because Glomeris \
             appends /chat/completions to it"
        ));
    }

    // The other half of the most common BYOK misconfiguration documented in
    // `book/src/byok.md`: pasting the endpoint URL rather than the API root
    // would produce `/v1/chat/completions/chat/completions`.
    if base_url
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .ends_with("/chat/completions")
    {
        return Err("base URL is the full endpoint URL; give only the API \
                    root — Glomeris appends /chat/completions itself"
            .to_string());
    }

    Ok(())
}

/// The two prompts `glomeris llm-check` sends (HORO-1309).
///
/// Fixed literals, and deliberately about nothing: a connection test must
/// prove the credential, the route and the model name are right without
/// describing the machine it runs on. There is no interpolation here and
/// there must never be — `connection_test_prompts_describe_nothing_local`
/// pins that they contain no path, no home directory and no account name,
/// which is only meaningfully assertable because they are constants.
pub const CONNECTION_TEST_SYSTEM_PROMPT: &str =
    "You are a connection test. Reply with the single word: ok.";
/// See [`CONNECTION_TEST_SYSTEM_PROMPT`].
pub const CONNECTION_TEST_USER_PROMPT: &str = "ok";

impl LlmProvider for OpenAiCompatibleProvider {
    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, LlmError> {
        let url = chat_completions_url(&self.base_url);

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
    provider_from_parts(
        std::env::var("GLOMERIS_LLM_API_KEY").unwrap_or_default(),
        std::env::var("GLOMERIS_LLM_BASE_URL").unwrap_or_default(),
        std::env::var("GLOMERIS_LLM_MODEL").unwrap_or_default(),
    )
}

/// The requirement and validation rules [`provider_from_env`] applies, with
/// the environment read out of the way.
///
/// Split out so those rules are testable without mutating `GLOMERIS_LLM_*`:
/// a test that set them would race the parallel test asserting they are
/// unset, and an env-mutating test is exactly the kind that passes alone and
/// fails in CI. `provider_from_env` stays the only `std::env::var` site.
pub fn provider_from_parts(
    api_key: String,
    base_url: String,
    model: String,
) -> Result<OpenAiCompatibleProvider, LlmError> {
    if api_key.is_empty() || base_url.is_empty() || model.is_empty() {
        return Err(LlmError::NotConfigured);
    }

    // Checked here rather than in each command so every live-mode caller gets
    // the same actionable message, and before the provider exists so a URL
    // that cannot work never reaches a request builder.
    validate_base_url(&base_url).map_err(LlmError::InvalidConfiguration)?;

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

/// One item of a model-proposed plan that survived validation against
/// this process's own evidence set and action registry.
///
/// Every field is either something this process already knew (`resource`,
/// `action_id` — both looked up, never taken from the model's bytes) or
/// something explicitly marked as the model's own claim (`priority`,
/// `model_reason`). Nothing here is authorization: see [`plan_with_llm`]'s
/// doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlanItem {
    /// Resolved from this process's `evidence_set` — a clone of a
    /// `ResourceId` that was already discovered locally, never a value
    /// parsed out of the model's response.
    pub resource: ResourceId,
    /// Resolved through [`ActionRegistry::get`] — a registered action's own
    /// id, never the model's string.
    pub action_id: ActionId,
    /// The model's claimed ordering hint. Advisory: nothing ranks, gates or
    /// authorizes on it.
    pub priority: Option<u32>,
    /// The model's own words explaining why it suggested this item, bounded
    /// and stripped by [`sanitize_model_reason`] (HORO-1308).
    ///
    /// Display-only, and the only field here whose *content* comes from
    /// outside this process. It is never parsed, never matched against, and
    /// never interpreted as a path, a command, or an instruction — the one
    /// thing a caller may do with it is show it to a human, clearly
    /// attributed to the model. Before HORO-1308 it was parsed off the wire
    /// and then dropped on the floor, so a rationale the user paid a
    /// provider to generate reached no surface at all.
    pub model_reason: Option<String>,
}

/// Bounds and cleans an [`LlmPlanItem::reason`] for display (HORO-1308).
///
/// Three things happen, in order, and each exists for its own reason:
///
/// 1. Every control character (including newlines and tabs) becomes a
///    single space. A rationale is rendered inside a one- or two-line row
///    in a fixed-width popover and inside a `println!` in a terminal;
///    embedded newlines would let model output forge what looks like
///    additional Glomeris output, and a stray `\r` or ANSI escape would let
///    it overwrite the line it was printed on.
/// 2. Runs of whitespace collapse and the ends are trimmed, so the bound in
///    step 3 is spent on words rather than on padding.
/// 3. The result is truncated to [`MODEL_REASON_LIMIT`] *characters*
///    (`char_indices`, never a byte slice, which would panic mid-codepoint)
///    with a trailing `…` marking that something was cut, because silently
///    truncated text reads as a complete sentence the model never wrote.
///
/// Returns `None` for input that is empty or whitespace-only after
/// cleaning: "the model said nothing" and "the model said `   `" are the
/// same fact, and a blank rationale row is worse than no row.
fn sanitize_model_reason(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() {
        return None;
    }

    match cleaned.char_indices().nth(MODEL_REASON_LIMIT) {
        Some((cut, _)) => Some(format!("{}…", &cleaned[..cut])),
        None => Some(cleaned),
    }
}

/// Result of one [`plan_with_llm`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmPlanResult {
    pub validated_items: Vec<ValidatedPlanItem>,
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
    let payload = match build_request_payload(evidence_set, actions) {
        Ok(payload) => payload,
        Err(e) => {
            return LlmPlanResult {
                validated_items: Vec::new(),
                dropped_unknown_resource: 0,
                dropped_unknown_action: 0,
                provider_error: Some(e),
            };
        }
    };

    let raw = match provider.complete(payload.system_prompt, &payload.user_prompt) {
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
        // Wire alias first (what a live model was actually given), then the
        // real `ResourceId` string. The second form is not a live-model
        // path — a model is never told a real id — it exists so a
        // hand-written `--plan-file` fixture can keep naming resources the
        // way `glomeris detect` prints them. Both are lookups into sets
        // this process built from its own discovery, so neither can name a
        // resource that was not already found here, and neither decides
        // anything: policy still classifies every survivor.
        let Some(resource) = payload
            .resolve(&item.resource_id)
            .or_else(|| {
                evidence_set
                    .iter()
                    .map(|ev| &ev.resource)
                    .find(|resource| resource.to_string() == item.resource_id)
            })
            .cloned()
        else {
            dropped_unknown_resource += 1;
            continue;
        };

        let Some(action_id) = actions.get(&item.action_id).map(|action| action.id()) else {
            dropped_unknown_action += 1;
            continue;
        };

        validated_items.push(ValidatedPlanItem {
            resource,
            action_id,
            priority: item.priority,
            model_reason: item.reason.as_deref().and_then(sanitize_model_reason),
        });
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
        let view = LlmResourceView::from_evidence(&ev, &actions, 0);

        assert_eq!(view.resource_id, "resource_1");
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
        let view = LlmResourceView::from_evidence(&ev, &actions, 0);
        assert_eq!(view.age_days, None);
    }

    /// The account name embedded in the fixture paths below. Asserting on
    /// this specific string — rather than only on `/` — is what makes the
    /// privacy tests fail for the right reason if the projection ever
    /// starts carrying a path in some other shape (a parent directory, a
    /// `Display`-formatted provenance field, a "home-relative" rewrite
    /// that keeps the leading segment).
    const FIXTURE_ACCOUNT: &str = "someaccount";

    fn realistic_evidence_set() -> Vec<Evidence> {
        vec![
            evidence_for(
                ResourceKind::XcodeDerivedData,
                "/Users/someaccount/Library/Developer/Xcode/DerivedData/MyApp-abc123",
            ),
            evidence_for(
                ResourceKind::CargoTargetDir,
                "/Users/someaccount/src/work-project/target",
            ),
            evidence_for(
                ResourceKind::NodeModules,
                "/Users/someaccount/src/work-project/node_modules",
            ),
        ]
    }

    #[test]
    fn payload_carries_no_path_or_account_name() {
        let actions = ActionRegistry::builtin();
        let payload = build_request_payload(&realistic_evidence_set(), &actions).unwrap();

        // The two strings below are the ENTIRE machine-derived egress
        // surface of a `plan_with_llm` call: `complete()` receives these
        // and nothing else, so proving both are path-free proves the
        // request is.
        for (label, text) in [
            ("system_prompt", payload.system_prompt),
            ("user_prompt", payload.user_prompt.as_str()),
        ] {
            assert!(
                !text.contains(FIXTURE_ACCOUNT),
                "{label} must not contain the account name: {text}"
            );
            assert!(
                !text.contains('/'),
                "{label} must not contain a path separator: {text}"
            );
            assert!(
                !text.contains("Users"),
                "{label} must not contain a home-directory segment: {text}"
            );
            assert!(
                !text.contains("DerivedData"),
                "{label} must not contain a directory name: {text}"
            );
        }

        // ...while the local alias table — which is never serialized —
        // still holds the real resources, or resolution would be
        // impossible rather than private.
        assert_eq!(payload.aliases().len(), 3);
        assert!(payload.aliases()[0].1.to_string().contains(FIXTURE_ACCOUNT));
    }

    #[test]
    fn wire_ids_are_positional_and_independent_of_the_resource() {
        let actions = ActionRegistry::builtin();
        let realistic = build_request_payload(&realistic_evidence_set(), &actions).unwrap();

        let wire_ids: Vec<&str> = realistic
            .aliases()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(wire_ids, vec!["resource_1", "resource_2", "resource_3"]);

        // Three entirely different resources in the same positions
        // produce byte-identical wire ids: the alias is derived from
        // position only, so it leaks nothing about what it names, and it
        // is not a stable handle that could correlate this machine across
        // requests.
        let unrelated = build_request_payload(
            &[
                evidence_for(ResourceKind::CargoTargetDir, "/opt/other/target"),
                evidence_for(ResourceKind::NodeModules, "/opt/other/node_modules"),
                evidence_for(ResourceKind::HomebrewCache, "/opt/homebrew/cache"),
            ],
            &actions,
        )
        .unwrap();
        let unrelated_ids: Vec<&str> = unrelated
            .aliases()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(unrelated_ids, wire_ids);
    }

    #[test]
    fn wire_alias_resolves_to_the_matching_resource() {
        // The form a live model actually answers in: it was shown
        // `resource_2` and says `resource_2`, and that must come back as
        // the second resource of the set — the real path never having
        // crossed the wire in either direction.
        let evidence_set = realistic_evidence_set();
        let text = r#"{"items": [{"resource_id": "resource_2", "action_id": "cargo.clean.target_dir", "priority": 3, "reason": "stale build output"}]}"#;
        let provider = FakeProvider {
            response: Ok(text.to_string()),
        };
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(result.provider_error.is_none());
        assert_eq!(result.dropped_unknown_resource, 0);
        assert_eq!(result.validated_items.len(), 1);
        let item = &result.validated_items[0];
        assert_eq!(item.resource, evidence_set[1].resource);
        assert_eq!(item.action_id.0, "cargo.clean.target_dir");
        assert_eq!(item.priority, Some(3));
        assert_eq!(item.model_reason.as_deref(), Some("stale build output"));
    }

    #[test]
    fn out_of_range_wire_alias_is_dropped_and_counted() {
        let evidence_set = realistic_evidence_set();
        let text = r#"{"items": [{"resource_id": "resource_99", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": null}]}"#;
        let provider = FakeProvider {
            response: Ok(text.to_string()),
        };
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(result.provider_error.is_none());
        assert!(result.validated_items.is_empty());
        assert_eq!(result.dropped_unknown_resource, 1);
    }

    #[test]
    fn model_supplied_path_cannot_name_an_undiscovered_resource() {
        // Opaque wire ids do not become an escape hatch in the other
        // direction: the real-id fallback is still a lookup into this
        // process's own discovery output, so a model that guesses a path
        // — including one it was never shown — names nothing.
        let evidence_set = realistic_evidence_set();
        let text = r#"{"items": [{"resource_id": "cargo_target_dir:/etc/passwd", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": null}]}"#;
        let provider = FakeProvider {
            response: Ok(text.to_string()),
        };
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert!(result.provider_error.is_none());
        assert!(result.validated_items.is_empty());
        assert_eq!(result.dropped_unknown_resource, 1);
    }

    #[test]
    fn serialized_view_exposes_exactly_the_documented_fields() {
        // A field added to `LlmResourceView` is a field that starts
        // leaving the machine, and the privacy assertions above only
        // catch it if its value happens to look like a path. This pins
        // the key set itself, so any new field fails here and has to be
        // justified deliberately.
        let actions = ActionRegistry::builtin();
        let payload = build_request_payload(&realistic_evidence_set(), &actions).unwrap();
        let views: serde_json::Value = serde_json::from_str(&payload.user_prompt).unwrap();

        let mut keys: Vec<&str> = views[0]
            .as_object()
            .expect("each view serializes as a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "age_days",
                "completeness",
                "kind",
                "offered_action_ids",
                "reclaimable_bytes",
                "regenerability",
                "resource_id",
            ]
        );
    }

    #[test]
    fn payload_view_order_matches_alias_order() {
        // Resolution is only correct if the i-th view the model sees and
        // the i-th alias entry describe the same resource; nothing else
        // ties the two `enumerate()` passes in `build_request_payload`
        // together.
        let evidence_set = realistic_evidence_set();
        let actions = ActionRegistry::builtin();
        let payload = build_request_payload(&evidence_set, &actions).unwrap();

        // Compared through the raw JSON rather than by deserializing:
        // `LlmResourceView` is serialize-only by design (nothing inbound
        // may be shaped like an evidence projection), so there is no
        // `Deserialize` impl to lean on here.
        let raw: serde_json::Value = serde_json::from_str(&payload.user_prompt).unwrap();
        assert_eq!(raw.as_array().unwrap().len(), evidence_set.len());
        assert_eq!(payload.aliases().len(), evidence_set.len());

        for (index, ev) in evidence_set.iter().enumerate() {
            let (wire_id, resource) = &payload.aliases()[index];
            assert_eq!(raw[index]["resource_id"], serde_json::json!(wire_id));
            assert_eq!(
                raw[index]["kind"],
                serde_json::json!(LlmResourceView::from_evidence(ev, &actions, index).kind)
            );
            assert_eq!(*resource, ev.resource);
        }
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

    /// HORO-1308: an ordinary rationale survives intact. The bound and the
    /// stripping below must not cost anything in the normal case, or the
    /// feature is worse than not carrying the reason at all.
    #[test]
    fn an_ordinary_model_reason_survives_sanitization_unchanged() {
        assert_eq!(
            sanitize_model_reason("Large stale build output; cargo can rebuild it."),
            Some("Large stale build output; cargo can rebuild it.".to_string())
        );
    }

    /// HORO-1308: newlines, tabs, carriage returns and ANSI escapes all
    /// become plain spaces. The `\r` and the escape are the dangerous two —
    /// in a terminal they can rewrite the line the rationale was printed on,
    /// which is how model output would forge Glomeris's own words.
    #[test]
    fn control_characters_in_a_model_reason_become_spaces() {
        let hostile = "safe\n\nPOLICY: AUTO_SAFE\r\x1b[2Kapproved\tnow";
        let cleaned = sanitize_model_reason(hostile).expect("non-empty");
        assert!(
            !cleaned.chars().any(char::is_control),
            "no control character may survive: {cleaned:?}"
        );
        assert!(!cleaned.contains('\u{1b}'), "no escape byte may survive");
        // The words are kept — this is sanitization, not censorship. What
        // it removes is the model's ability to control layout, not its
        // ability to say something wrong (which the UI answers by
        // attributing it to the model and showing the real verdict beside
        // it).
        assert_eq!(cleaned, "safe POLICY: AUTO_SAFE [2Kapproved now");
    }

    /// HORO-1308: an over-long rationale is cut at the character bound with
    /// a visible ellipsis. Asserted on a multi-byte input specifically:
    /// truncating by byte offset would either panic mid-codepoint or emit
    /// invalid UTF-8, and a naive `&s[..LIMIT]` is the obvious wrong way to
    /// write this.
    #[test]
    fn an_over_long_model_reason_is_cut_at_a_character_boundary() {
        let long = "é".repeat(MODEL_REASON_LIMIT * 2);
        let cleaned = sanitize_model_reason(&long).expect("non-empty");
        assert_eq!(cleaned.chars().count(), MODEL_REASON_LIMIT + 1);
        assert!(cleaned.ends_with('…'), "truncation must be visible");
        assert_eq!(
            cleaned.chars().filter(|c| *c == 'é').count(),
            MODEL_REASON_LIMIT
        );
    }

    /// HORO-1308: a reason that is empty, or only whitespace, or only
    /// control characters, is reported as absent rather than as a blank
    /// line of UI. All three are the same fact.
    #[test]
    fn a_blank_model_reason_is_reported_as_no_reason_at_all() {
        assert_eq!(sanitize_model_reason(""), None);
        assert_eq!(sanitize_model_reason("   \t  "), None);
        assert_eq!(sanitize_model_reason("\n\n\r\n"), None);
    }

    /// HORO-1308: `reason` omitted entirely (it is `Option` on the wire)
    /// yields `None`, not an empty string — so a surface can tell "the model
    /// gave no rationale" apart from "the model gave one" without comparing
    /// against `""`.
    #[test]
    fn a_plan_item_with_no_reason_field_carries_no_model_reason() {
        let evidence_set = realistic_evidence_set();
        let text = r#"{"items": [{"resource_id": "resource_1", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": null}]}"#;
        let provider = FakeProvider {
            response: Ok(text.to_string()),
        };
        let actions = ActionRegistry::builtin();

        let result = plan_with_llm(&provider, &evidence_set, &actions);

        assert_eq!(result.validated_items.len(), 1);
        assert_eq!(result.validated_items[0].model_reason, None);
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
        // Names the resource by its real `ResourceId` string rather than
        // its wire alias, exercising the hand-written-fixture fallback in
        // `plan_with_llm` (HORO-1298) — see
        // `wire_alias_resolves_to_the_matching_resource` for the form a
        // live model actually answers in.
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
        let item = &result.validated_items[0];
        assert_eq!(item.resource, ev.resource);
        assert_eq!(item.action_id.0, "cargo.clean.target_dir");
        assert_eq!(item.priority, Some(5));
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

    /// The shared helper `crate::cli`'s connection-test report uses. Same
    /// character-boundary obligation as `redact_secret`, at a caller-chosen
    /// limit — so it gets the same multi-byte test rather than being trusted
    /// because it happens to share an implementation today.
    #[test]
    fn excerpt_bounds_provider_text_on_a_character_boundary() {
        assert_eq!(excerpt("ok", 120), "ok");
        assert_eq!(excerpt("  ok\n\n  then   more ", 120), "ok then more");

        let bounded = excerpt(&"é".repeat(400), 120);
        assert!(bounded.ends_with("… (truncated)"));
        assert!(bounded.len() < 120 + 32);
    }

    /// A provider that answers with a wall of text does not get to decide how
    /// much of a fixed-width popover it occupies.
    #[test]
    fn excerpt_does_not_let_a_provider_choose_its_own_length() {
        let shouted = "no".repeat(10_000);
        assert!(excerpt(&shouted, 120).len() < 200);
    }

    /// The path rule `book/src/byok.md` documents, and the reason this is a
    /// function rather than two `format!` calls: the diagnostic that tells a
    /// user they configured the host root instead of the API root is only
    /// truthful if it is built from the same concatenation the request was.
    #[test]
    fn chat_completions_url_appends_verbatim_and_tolerates_one_slash() {
        assert_eq!(
            chat_completions_url("https://gateway.example.com/v1"),
            "https://gateway.example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://gateway.example.com/v1/"),
            "https://gateway.example.com/v1/chat/completions"
        );
        // Never inserted for you — this is the misconfiguration, rendered
        // exactly as the user will see it in a failure message.
        assert_eq!(
            chat_completions_endpoint_path("https://gateway.example.com"),
            "/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint_path("https://gateway.example.com/v1"),
            "/v1/chat/completions"
        );
    }

    /// Every state gets its own token, and the set is exactly what the macOS
    /// app has wording for — `scripts/check-vocabulary-covers-cli-tokens.sh`
    /// checks that second half mechanically, and this checks the first: a
    /// mapping that collapsed two states into one token would make the drift
    /// check pass while the GUI lost the distinction.
    #[test]
    fn llm_check_outcome_gives_each_state_its_own_token() {
        let tokens = [
            llm_check_outcome(None),
            llm_check_outcome(Some(&LlmError::NetworkError("x".into()))),
            llm_check_outcome(Some(&LlmError::ProviderStatus {
                status: 401,
                api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
                endpoint_path: "/v1/chat/completions".to_string(),
                request_id: None,
                body_excerpt: String::new(),
            })),
            llm_check_outcome(Some(&LlmError::InvalidResponse("x".into()))),
            llm_check_outcome(Some(&LlmError::InvalidConfiguration("x".into()))),
        ];

        let mut unique = tokens.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            tokens.len(),
            "tokens must be distinct: {tokens:?}"
        );
        assert_eq!(llm_check_outcome(None), "ok");

        // A local problem is never reported as something the provider did.
        for local in [
            LlmError::NotConfigured,
            LlmError::InvalidConfiguration("x".into()),
        ] {
            assert_eq!(llm_check_outcome(Some(&local)), "misconfigured");
        }
    }

    /// The shapes a BYOK user legitimately configures. Includes `http://` on
    /// loopback because a local gateway is a first-class setup, and a host
    /// root with no path at all because some providers really do serve the
    /// API there — that is a *diagnosable* mistake, not an invalid URL, and
    /// refusing it here would block a working configuration.
    #[test]
    fn validate_base_url_accepts_every_shape_a_real_provider_uses() {
        for accepted in [
            "https://api.openai.com/v1",
            "https://api.openai.com/v1/",
            "https://openrouter.ai/api/v1",
            "http://127.0.0.1:11434/v1",
            "http://localhost:1234/v1",
            "https://gateway.internal.example",
            "https://gateway.internal.example/openai/deployments/some-deployment",
        ] {
            assert_eq!(
                validate_base_url(accepted),
                Ok(()),
                "must accept {accepted}"
            );
        }
    }

    /// Each rejection must name what to change, since this is the error a user
    /// meets before anything has ever worked. Asserted on a distinguishing
    /// word rather than the whole sentence so rewording the guidance does not
    /// break the test, while deleting the guidance does.
    #[test]
    fn validate_base_url_rejects_what_cannot_work_and_says_what_to_do() {
        for (rejected, expected_hint) in [
            ("", "empty"),
            ("   ", "empty"),
            ("https://api.openai.com/v1\n", "whitespace"),
            (" https://api.openai.com/v1", "whitespace"),
            ("https://api openai.com/v1", "space"),
            ("api.openai.com/v1", "no scheme"),
            ("ftp://api.openai.com/v1", "https://"),
            ("file:///etc/passwd", "https://"),
            ("https:///v1", "no host"),
            (
                "https://api.openai.com/v1?key=would-be-a-credential",
                "query",
            ),
            ("https://api.openai.com/v1#frag", "fragment"),
            ("https://api.openai.com/v1/chat/completions", "API"),
            ("https://api.openai.com/v1/chat/completions/", "API"),
            ("https://api.openai.com/V1/Chat/Completions", "API"),
        ] {
            let message = validate_base_url(rejected)
                .expect_err(&format!("must reject {rejected:?}"))
                .to_lowercase();
            assert!(
                message.contains(&expected_hint.to_lowercase()),
                "message for {rejected:?} must mention {expected_hint:?}: {message}"
            );
        }
    }

    /// The value a user most plausibly gets wrong by pasting is the one with a
    /// credential in it, so no rejection may echo the input back.
    #[test]
    fn validate_base_url_never_echoes_the_offending_value() {
        let secret = "sk-should-never-appear-anywhere";
        for rejected in [
            format!("https://gw.example/v1?access_token={secret}"),
            format!("https://gw.example/v1#{secret}"),
            format!("https://gw.example/{secret}/chat/completions"),
            format!("ftp://gw.example/{secret}"),
            format!("gw.example/{secret}"),
            format!("https://gw.example/v1/{secret} "),
        ] {
            let message = validate_base_url(&rejected).expect_err("must reject");
            assert!(
                !message.contains(secret) && !message.contains("gw.example"),
                "message echoed the input: {message}"
            );
        }
    }

    /// The reason the query-string rejection exists, pinned as an executable
    /// fact rather than left in a comment: appending to such a URL puts the
    /// endpoint inside the query string, which cannot work.
    #[test]
    fn a_query_string_base_url_would_append_into_the_query_hence_the_refusal() {
        let with_query = "https://gw.example/v1?key=x";
        assert_eq!(
            chat_completions_url(with_query),
            "https://gw.example/v1?key=x/chat/completions"
        );
        assert_eq!(chat_completions_endpoint_path(with_query), "/v1");
        assert!(validate_base_url(with_query).is_err());
    }

    /// `provider_from_parts` is the single choke point every live-mode caller
    /// reaches through `provider_from_env`, so validating there is what makes
    /// the check universal. Tested on the parts rather than through the
    /// environment deliberately: mutating `GLOMERIS_LLM_*` from a test would
    /// race the parallel test that asserts those variables are unset.
    #[test]
    fn provider_from_parts_refuses_an_invalid_base_url_before_building_a_provider() {
        let result = provider_from_parts(
            "not-a-real-key".to_string(),
            "https://gw.example/v1/chat/completions".to_string(),
            "some-model".to_string(),
        );

        match result {
            Err(LlmError::InvalidConfiguration(detail)) => {
                assert!(detail.contains("API root"), "got: {detail}");
                assert!(!detail.contains("not-a-real-key"), "got: {detail}");
                assert!(!detail.contains("gw.example"), "got: {detail}");
            }
            Err(other) => panic!("wrong error class: {other}"),
            Ok(_) => panic!("a full endpoint URL must not build a provider"),
        }
    }

    #[test]
    fn provider_from_parts_requires_all_three_and_distinguishes_absent_from_invalid() {
        for (key, url, model) in [
            ("", "https://gw.example/v1", "m"),
            ("k", "", "m"),
            ("k", "https://gw.example/v1", ""),
        ] {
            assert_eq!(
                provider_from_parts(key.to_string(), url.to_string(), model.to_string()).err(),
                Some(LlmError::NotConfigured),
                "an absent value is NotConfigured, never InvalidConfiguration"
            );
        }

        let provider = provider_from_parts(
            "k".to_string(),
            "https://gw.example/v1".to_string(),
            "m".to_string(),
        )
        .expect("a valid trio builds a provider");
        assert_eq!(provider.base_url, "https://gw.example/v1");
        assert_eq!(provider.model, "m");
    }

    /// A connection test must prove the credential, the route and the model
    /// without describing the machine it runs on. Asserted against the real
    /// environment's own values, so this cannot pass by testing a fiction.
    #[test]
    fn connection_test_prompts_describe_nothing_local() {
        let both = format!("{CONNECTION_TEST_SYSTEM_PROMPT} {CONNECTION_TEST_USER_PROMPT}");

        assert!(!both.contains('/'), "a path separator is in {both:?}");
        if let Ok(home) = std::env::var("HOME") {
            assert!(!home.is_empty());
            assert!(!both.contains(&home));
            if let Some(account) = home.rsplit('/').next() {
                assert!(
                    !account.is_empty() && !both.to_lowercase().contains(&account.to_lowercase()),
                    "the account name appears in {both:?}"
                );
            }
        }
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

    /// HORO-1355. The hint fires for exactly the endpoint paths a path-less
    /// base URL produces, and it is checked *through*
    /// [`chat_completions_endpoint_path`] rather than against a literal: a test
    /// that hard-coded `"/chat/completions"` would keep passing if the URL
    /// rule changed, which is the one way this diagnostic could start lying.
    #[test]
    fn host_root_hint_fires_for_exactly_the_paths_a_path_less_base_url_builds() {
        for host_root in [
            "https://gateway.example.com",
            "https://gateway.example.com/",
            "http://127.0.0.1:11434",
        ] {
            let path = chat_completions_endpoint_path(host_root);
            assert!(
                host_root_rejection_hint(&path).is_some(),
                "{host_root} builds {path}, which is the host-root mistake"
            );
        }

        for api_root in [
            "https://gateway.example.com/v1",
            "https://gateway.example.com/v1/",
            "https://openrouter.ai/api/v1",
            "http://127.0.0.1:11434/v1",
        ] {
            let path = chat_completions_endpoint_path(api_root);
            assert!(
                host_root_rejection_hint(&path).is_none(),
                "{api_root} builds {path}; a configured API root must not be \
                 blamed for a refusal"
            );
        }
    }

    /// A rejection at a configured API root says nothing about the address —
    /// a `401` from `…/v1/chat/completions` is about the key, and volunteering
    /// a guess about the path there would trade one misdirection for another.
    #[test]
    fn provider_status_display_stays_silent_about_the_address_at_an_api_root() {
        let error = LlmError::ProviderStatus {
            status: 403,
            api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
            endpoint_path: "/v1/chat/completions".to_string(),
            request_id: None,
            body_excerpt: r#"{"error":"forbidden"}"#.to_string(),
        };
        let rendered = format!("{error}");

        assert!(!rendered.contains("host root"), "{rendered}");
        assert!(!rendered.contains("likeliest"), "{rendered}");
    }

    /// The failure this ticket came from, end to end through the real provider:
    /// a service that refuses a request posted to its root must be reported in
    /// a way that names the address as the likely cause. The path-less base URL
    /// is derived from `serve_one`'s own by removing the segment it adds, so
    /// this cannot pass by agreeing with a literal that
    /// [`chat_completions_url`] no longer produces.
    #[test]
    fn a_rejection_at_the_host_root_explains_the_address_end_to_end() {
        let (api_root, handle) = serve_one(
            "HTTP/1.1 403 Forbidden",
            "",
            r#"{"error":"Selected provider is forbidden"}"#,
        );
        let host_root = api_root
            .strip_suffix("/v1")
            .expect("serve_one hands back an API root ending in /v1")
            .to_string();

        let provider = OpenAiCompatibleProvider {
            base_url: host_root,
            api_key: TEST_KEY.to_string(),
            model: "example-model".to_string(),
        };

        let error = provider
            .complete("s", "u")
            .expect_err("403 must be an error");
        let captured = handle.join().expect("server thread");

        assert!(
            captured.request_line.contains("POST /chat/completions"),
            "the request itself must show the mistake: {:?}",
            captured.request_line
        );

        let rendered = format!("{error}");
        assert!(rendered.contains("POST /chat/completions"), "{rendered}");
        // The provider's own words survive, and the hint is added after them
        // rather than in place of them.
        assert!(
            rendered.contains("Selected provider is forbidden"),
            "{rendered}"
        );
        assert!(rendered.contains("has no path"), "{rendered}");
        assert!(rendered.contains("/v1"), "{rendered}");
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
