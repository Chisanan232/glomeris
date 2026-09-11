# BYOK LLM Planner

`actions::llm` (`src/actions/llm.rs`) is an optional, "bring your own key"
LLM planner. As of this ticket, **it exists as a library module and is not
wired into the `glomeris` binary** — there is no `glomeris` subcommand or
flag that reads an API key or invokes it. This page documents the module as
it exists in the library today.

## What it is

An advisory ranking suggestion over evidence the crate already collected.
`LlmResourceView` is an explicit, bounded projection of one `Evidence`
record — deliberately not a `Serialize` derive on `Evidence` itself, so
adding a field to `Evidence` later has no effect on what an LLM would see
unless a human explicitly adds it here too. `plan_with_llm` is the entry
point: it serializes a set of these views, calls a provider, and defensively
validates the response.

## What it is not, and never will be

`plan_with_llm`'s output is a ranking suggestion only. It never calls
`policy::classify`/`authorize`, and it must not: every surviving
`(ResourceId, ActionId, priority)` tuple still has to go through the exact
same policy classification any other candidate would, in the caller, before
anything executes. A dedicated test
(`llm_plan_item_never_bypasses_policy`) demonstrates this concretely:
constructing evidence for SSH key material, getting an LLM response that
"approves" deleting it, confirming the item survives `plan_with_llm`'s
validation — and then showing `policy::classify` still returns `Protected`
for it regardless.

## Configuration (today: caller-supplied, not env-var-driven)

`OpenAiCompatibleProvider` is the one real provider implementation — any
OpenAI-compatible `/chat/completions` endpoint (OpenAI itself, OpenRouter, a
self-hosted gateway):

```rust
pub struct OpenAiCompatibleProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}
```

Its own doc comment states the intended contract: "the caller (CLI layer) is
responsible for reading `api_key` from an environment variable and never
logging/printing it." No such CLI layer exists yet, so there is currently no
defined environment variable name to set — a future ticket wiring this into
the binary will need to define one.

**Never commit an API key to source control, a Jira ticket, a commit
message, or a PR description.** Treat it as you would any other credential.

## Safety properties that are already verified

- `OpenAiCompatibleProvider` deliberately does **not** derive `Debug` — a
  derived `Debug` would print `api_key` verbatim. A test confirms a real
  failed `complete()` call's error output never contains the key.
- `LlmError`'s variants never embed the key either, confirmed by a
  dedicated test.
- `LlmPlanItem` uses `#[serde(deny_unknown_fields)]`: a model trying to
  smuggle an extra field (e.g. a `"command"`) fails deserialization of the
  *whole* plan, not just that item — confirmed by
  `unexpected_field_rejects_whole_plan`.
- An unknown `resource_id` or `action_id` in the model's response drops just
  that one item (counted in `dropped_unknown_resource`/
  `dropped_unknown_action`); it never fails or invalidates the rest of the
  plan.
- `LlmPlan`/`LlmPlanItem` are the *only* two `#[derive(Deserialize)]` types
  in the entire crate. Everything they can produce is a `String` resolved
  against real, already-in-memory data, or a plain `u32`/`Option<String>`
  used only for display/ranking — never a path, never a shell fragment,
  never anything that reaches `ActionStep` construction directly.
- The response parser handles a raw JSON object, a fenced ` ```json ` block,
  or a bare fenced block, and either produces a well-formed `LlmPlan` or
  nothing — never a partial parse.

## Known limitations (from the PR that introduced this)

- Only one provider shape is implemented — no Anthropic-native,
  Azure-OpenAI-specific auth, or other provider shape.
- No retry/backoff on transient network failures.
- No streaming support — `complete()` returns the full response text at
  once.
- No integration into any existing rule-only ranking loop (`glomeris free`)
  — deliberately out of scope for the ticket that introduced this module. A
  future ticket may wire it in as an optional enhancement, falling back to
  rule-only ranking whenever the provider call fails.
