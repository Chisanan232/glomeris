# BYOK LLM Planner

`actions::llm` (`src/actions/llm.rs`) is an optional, "bring your own key"
LLM planner. As of HORO-1008, it is wired into the `glomeris` binary as an
**advisory, non-executing** subcommand: `glomeris llm-plan`. This page
documents both the library module and the CLI surface over it.

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

`glomeris llm-plan` (the CLI layer built on top of this module) keeps the
same guarantee: `crate::cli::build_llm_plan_report` never constructs a
`policy::Approval` and never calls `policy::approval::authorize` or
`executor::execute`. Nothing this subcommand prints is ever executed —
there is no `--execute`/`--yes` flag, and there never will be one on this
subcommand.

## `glomeris llm-plan` usage

```
glomeris llm-plan [--project-root <path>]... [--plan-file <path>] [--json]
```

- Without `--plan-file`, calls a real OpenAI-compatible endpoint via
  `actions::llm::provider_from_env`.
- `--plan-file <path>` reads the file's raw bytes and treats them exactly
  as if they were the model's raw response text, through the identical
  `extract_plan`/`LlmPlan`/`plan_with_llm` validation pipeline — useful for
  reproducing a scenario deterministically, with no network call and no API
  key.
- `--json` prints the `LlmPlanReport` as JSON instead of the human-readable
  form.

Human-readable output always opens with:

```
LLM SUGGESTION — advisory only, nothing is executed by this command
```

See [CLI Reference](cli_reference.md) for the full flag/exit-code table.

## Configuration

Live mode (no `--plan-file`) reads three environment variables, all
required, with no default `base_url`:

| Variable | Purpose |
|---|---|
| `GLOMERIS_LLM_API_KEY` | Bearer token sent as `Authorization: Bearer <key>` |
| `GLOMERIS_LLM_BASE_URL` | Base URL of an OpenAI-compatible `/chat/completions` endpoint |
| `GLOMERIS_LLM_MODEL` | Model name sent in the request body |

`actions::llm::provider_from_env` is the only place `std::env::var` is
called for these — the key is held just long enough to build the
`OpenAiCompatibleProvider` and is never logged, printed, or returned any
other way. A missing or empty value for any of the three returns
`LlmError::NotConfigured`; `glomeris llm-plan` reports this by naming the
three variable *names*, never a value, and exits `2`.

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

**The API key is never accepted as a CLI argument.** `glomeris llm-plan`
explicitly rejects `--api-key`/`--key`/`--token` with an error pointing at
`$GLOMERIS_LLM_API_KEY` instead — a key on the command line would be
visible to `ps` and land in shell history.

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
- `LlmPlan`/`LlmPlanItem` remain the *only* two `#[derive(Deserialize)]`
  types in the entire crate — `LlmPlanItemReport`/`LlmPlanReport` (the CLI
  report DTOs) are `Serialize` only. Everything a model response can
  produce is a `String` resolved against real, already-in-memory data, or a
  plain `u32`/`Option<String>` used only for display/ranking — never a
  path, never a shell fragment, never anything that reaches `ActionStep`
  construction directly.
- The response parser handles a raw JSON object, a fenced ` ```json ` block,
  or a bare fenced block, and either produces a well-formed `LlmPlan` or
  nothing — never a partial parse.
- A `PROTECTED` candidate is refused before its action is ever resolved:
  `crate::cli::build_llm_plan_report` checks `PolicyClass::Protected`
  before calling `ActionRegistry::get`/`executor::dry_run` for that item.
  `tests/golden_llm_plan_protected_refusal.rs` proves this end to end
  through the exact `--plan-file` input surface the CLI uses, closing the
  gap [Known Limitations](known_limitations.md) previously described as
  "verified at the code level only, not end-to-end through the CLI."

## Known limitations

- Only one provider shape is implemented — no Anthropic-native,
  Azure-OpenAI-specific auth, or other provider shape.
- No retry/backoff on transient network failures.
- No streaming support — `complete()` returns the full response text at
  once.
- No integration into any existing rule-only ranking loop (`glomeris free`)
  — deliberately out of scope for this ticket. A future ticket may wire it
  in as an optional enhancement, falling back to rule-only ranking whenever
  the provider call fails.
- `glomeris llm-plan` has no `--execute`/`--yes` flag and never will on this
  subcommand — it is advisory-only by design, not an MVP gap.
