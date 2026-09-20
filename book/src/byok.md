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

## `LlmPlan` schema — the concrete `--plan-file` example (HORO-1048)

`glomeris llm-plan --schema` prints an example, syntactically valid
`LlmPlan` JSON document to stdout — the canonical way to get a starting
point for a `--plan-file` fixture without reading `src/actions/llm.rs`'s
`LlmPlanItem` struct directly:

```
glomeris llm-plan --schema
```

```json
{
  "items": [
    {
      "resource_id": "cargo_target_dir:/Users/you/project/target",
      "action_id": "cargo.clean.target_dir",
      "priority": 1,
      "reason": "stale build artifacts, not modified in 30 days"
    }
  ]
}
```

Field meanings, all on `LlmPlanItem`:

| Field | Type | Meaning |
|---|---|---|
| `resource_id` | `String`, required | Must match a `ResourceId::to_string()` from the evidence set `plan_with_llm` was called with — an unmatched value drops just that item (`dropped_unknown_resource`), never the whole plan. |
| `action_id` | `String`, required | Must match a registered `ActionRegistry` action id — an unmatched value drops just that item (`dropped_unknown_action`). |
| `priority` | `Option<u32>` | Informational ranking hint only. |
| `reason` | `Option<String>` | Human-readable explanation. Never interpreted as an instruction, a path, or anything that reaches execution. |

`#[serde(deny_unknown_fields)]` on `LlmPlanItem` means any extra field
(e.g. a smuggled `"command"`) fails deserialization of the whole
document, not just that item — see "Safety properties" below.

This example is not a fixed, hand-maintained fixture: `--schema`'s output
is proven, by a real subprocess round-trip test
(`tests/llm_plan_schema_round_trip.rs`), to be accepted unchanged by
`glomeris llm-plan --plan-file <path>` — i.e. it parses and validates
through the exact pipeline above without a parse-error exit code. (The
`resource_id` in the shipped example is deliberately one no real
discovery run will ever produce, so a round trip against a real evidence
set still drops it as an unknown resource — that is an expected,
non-error validation outcome, not a parse failure.)

## Configuration

Live mode (no `--plan-file`) reads three environment variables, all
required, with no default `base_url`:

| Variable | Purpose |
|---|---|
| `GLOMERIS_LLM_API_KEY` | Bearer token sent as `Authorization: Bearer <key>` |
| `GLOMERIS_LLM_BASE_URL` | **API root** of an OpenAI-compatible service — see below |
| `GLOMERIS_LLM_MODEL` | Model name sent in the request body |

### `GLOMERIS_LLM_BASE_URL` is the API root, not the host root

Glomeris appends `/chat/completions` to the base URL **verbatim**. It never
inserts a `/v1` segment for you, and never strips one. A single trailing
slash is tolerated. So the base URL must be the path prefix your provider
serves its API under — for most providers that includes `/v1`:

```sh
export GLOMERIS_LLM_BASE_URL="https://gateway.example.com/v1"
# request goes to https://gateway.example.com/v1/chat/completions
```

Setting the host root instead is the most common misconfiguration:

```sh
export GLOMERIS_LLM_BASE_URL="https://gateway.example.com"
# request goes to https://gateway.example.com/chat/completions  ← wrong path
```

Because Glomeris does not rewrite the path, a base URL that already ends in
`/v1` produces exactly one `/v1` segment — there is no `/v1/v1` failure mode.

Do not try to identify this mistake from the status code: a gateway may
answer an unrouted path with `404`, but `403`, `401` and even `400` are all
things real gateways return instead. Read the **path** in the error message
— it is the path that was actually requested, so it tells you directly which
of the two forms you configured:

```
provider returned HTTP 403 for openai:chat_completions POST /chat/completions
                                                            ^ no /v1 — host root was configured
```

A full configuration, using placeholders throughout:

```sh
export GLOMERIS_LLM_API_KEY="<token>"          # never commit; never pass in argv
export GLOMERIS_LLM_BASE_URL="https://gateway.example.com/v1"
export GLOMERIS_LLM_MODEL="example-model"
glomeris llm-plan --project-root ~/code/my-project
```

Prefer `export` in your shell (or a secret manager that exports into the
process environment) over a plaintext `.env` file, and never pass the key on
the command line — `glomeris llm-plan` rejects `--api-key`/`--key`/`--token`
outright for that reason.

### Diagnosing a provider failure

Provider failures are reported in the `provider_error` field (and on the
text output's `provider error:` line) as one readable, secret-free sentence.
The three classes are deliberately distinct:

| Class | Meaning |
|---|---|
| `provider unreachable: …` | No HTTP response at all — DNS, TLS, refused connection, timeout |
| `provider returned HTTP <status> …` | The provider answered with a non-2xx status |
| `provider response was unusable: …` | A 2xx response that was empty, not JSON, or missing `choices[0].message.content` |

An HTTP failure names the status, the API style, the request path, the
provider's `x-request-id` when it sends one, and a bounded excerpt of the
provider's own error body:

```
provider returned HTTP 401 for openai:chat_completions POST /v1/chat/completions \
  (request_id=req-abc-123): {"error":{"code":"invalid_api_key"}}
```

What is deliberately **not** in that message: the `Authorization` header,
the API key, the request payload, the scheme, the host, and the query
string. The host is omitted because it may be private infrastructure and
the query string because some gateways accept a credential there; the path
alone is what diagnoses a base-URL mistake. The body excerpt is truncated
to a bounded length and is scrubbed of the configured API key, so a
provider that echoes the credential it just rejected cannot turn Glomeris's
diagnostics into the leak.

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
- `LlmError`'s variants never embed the key either — every variant's
  `Display` output is asserted key-free by
  `api_key_never_appears_in_display_output_of_any_variant`, including the
  `ProviderStatus` body excerpt, which actively scrubs the configured key
  (`redact_secret_removes_every_occurrence_and_collapses_whitespace`) so a
  provider echoing the credential back cannot leak it through Glomeris.
- The diagnostic endpoint path in a provider error is the path only: two
  tests (`diagnostic_endpoint_path_drops_scheme_host_and_query`,
  `diagnostic_endpoint_path_never_contains_the_host`) assert the scheme,
  host, and query string are all dropped.
- The wire protocol itself is pinned against a loopback mock HTTP server
  rather than mocked at the trait: the appended path, trailing-slash
  tolerance, `Authorization: Bearer` scheme, model and both message roles,
  and the 401/403/404/empty-body/non-JSON/connection-closed failure modes
  each have a test that inspects the bytes actually sent.
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
