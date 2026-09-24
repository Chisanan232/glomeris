# BYOK LLM Planner

`actions::llm` (`src/actions/llm.rs`) is an optional, "bring your own key"
LLM planner. As of HORO-1008, it is wired into the `glomeris` binary as an
**advisory, non-executing** subcommand: `glomeris llm-plan`. This page
documents both the library module and the CLI surface over it.

Since HORO-1308 the menu-bar app has an **AI Plan** card over the same
subcommand. It is a thin client: it spawns `glomeris llm-plan --json
--progress-json`, renders the report, and has no planner, no provider client
and no execution path of its own. Everything on this page — what leaves your
machine, what the validator drops, what the planner can and cannot do —
applies unchanged to the GUI, because it is the same code doing the work. See
[Menu Bar App](menu_bar_app.md#ai-plan) for how the card separates the model's
words from the machine's.

Since HORO-1309 the provider itself can be configured from that app
(**Settings → AI Provider**) instead of only from the shell, with the key in
the login keychain rather than an exported variable — see
[Configuring this from the menu-bar app](#configuring-this-from-the-menu-bar-app)
below. The GUI is still a thin client there too: it writes three values and
launches a child process with them. It does not validate the URL, does not
know which models exist, and never speaks to a provider itself.

## What it is

An advisory ranking suggestion over evidence the crate already collected.
`LlmResourceView` is an explicit, bounded projection of one `Evidence`
record — deliberately not a `Serialize` derive on `Evidence` itself, so
adding a field to `Evidence` later has no effect on what an LLM would see
unless a human explicitly adds it here too. `plan_with_llm` is the entry
point: it serializes a set of these views, calls a provider, and defensively
validates the response.

## What leaves your machine

Two strings: a fixed system prompt and a JSON array of `LlmResourceView`
values. `LlmRequestPayload` is that request, and it is the whole outbound
surface — nothing about the local machine is added downstream of it.

Each view identifies its resource by a **positional wire alias** —
`resource_1`, `resource_2`, … — not by its real `ResourceId`. That matters
because a `ResourceId` for a path-backed resource kind renders as an
absolute path, and an absolute path under `$HOME` carries your OS account
name and your directory layout. Since HORO-1298, no path, home directory
or account name is transmitted; what the model gets is the resource's
kind, reclaimable size, age, regenerability, evidence completeness and the
action ids offered for it, which is what a ranking judgement is actually
made from.

The alias is positional rather than a hash of the resource. A hash would be
stable across runs, which sounds better until you count the keyspace: a
path like `/Users/<name>/Library/Developer/Xcode/DerivedData` has one
unknown segment, so anyone holding a candidate account-name list can
confirm it against the hash offline. A stable pseudonym is also, by
construction, a handle for correlating your machine across requests. An
index is neither, and costs nothing — the model is ranking resources it was
just handed, not recognizing them from last week.

The wire-id → real-`ResourceId` table stays in the process's memory and is
never serialized. It is how a response's `resource_id` regains meaning
locally.

### Checking this yourself

```
glomeris llm-plan --print-payload [--json]
```

Runs discovery, prints the exact request a live run would send plus the
local alias table, and stops — before any provider is constructed. No API
key is needed, and no network call is made, so the command cannot send the
thing it is showing you. The two prompt sections are what leaves; the alias
section is explicitly labelled as not sent, and is the only place the real
absolute paths appear.

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
glomeris llm-plan --print-payload [--project-root <path>]... [--json]
glomeris llm-plan --schema
```

- Without `--plan-file`, calls a real OpenAI-compatible endpoint via
  `actions::llm::provider_from_env`.
- `--plan-file <path>` reads the file's raw bytes and treats them exactly
  as if they were the model's raw response text, through the identical
  `extract_plan`/`LlmPlan`/`plan_with_llm` validation pipeline — useful for
  reproducing a scenario deterministically, with no network call and no API
  key.
- `--print-payload` prints the outbound request without sending it, and
  returns before a provider is constructed — see "What leaves your
  machine" above. Requires no API key.
- `--json` prints the `LlmPlanReport` (or, with `--print-payload`, the
  `LlmPayloadReport`) as JSON instead of the human-readable form.

Human-readable output always opens with:

```
LLM SUGGESTION — advisory only, nothing is executed by this command
```

See [CLI Reference](cli_reference.md) for the full flag/exit-code table.

## `glomeris llm-check` — does the configuration actually work?

```
glomeris llm-check [--json]
```

The narrowest command in the CLI: no project roots, no detectors, no evidence,
no policy, no actions. It sends two fixed prompts — `"You are a connection
test. Reply with the single word: ok."` and `"ok"` — through the same
`LlmProvider::complete` a plan uses, and reports whether the endpoint, the
credential and the model name work.

Two properties are worth being explicit about, because a connection test that
lacked either would be worse than none:

- **It proves the real path.** A cheaper probe — a `GET /models`, a HEAD
  request, a DNS lookup — can pass while a `POST /chat/completions` with your
  model name fails. This sends the request a plan sends, minus the evidence.
- **It describes nothing about this machine.** The two prompts are constants
  with no interpolation, and `connection_test_prompts_describe_nothing_local`
  pins that they contain no path separator, no home directory and no account
  name — an assertion that is only meaningful *because* they are constants.
  So the cost of testing a configuration is one trivial completion, not a
  disclosure.

The outcome is one of five tokens, and they are deliberately distinct because
the fix for each is in a different place:

| `outcome` | Exit | What it means | What to change |
|---|---|---|---|
| `ok` | `0` | The provider answered; its reply is in `response_excerpt` | — |
| `rejected` | `1` | A non-2xx status — the provider answered and said no | The credential, or the base-URL **path**: read the path in `error` |
| `unreachable` | `1` | No HTTP response at all — DNS, TLS, refused, timeout | Network, host name, VPN |
| `unusable_response` | `1` | A 2xx that was empty, not JSON, or missing `choices[0].message.content` | The model name, or a gateway not really speaking the OpenAI shape |
| `misconfigured` | `2` | `validate_base_url` refused before anything was sent | The base URL — see above |

The `1`/`2` split carries a meaning the outcome token alone does not: on `1` a
full report was printed **first**, so the diagnosis is on stdout; on `2` there
is no report at all, and the single line on stderr prefixed `glomeris
llm-check: ` is the whole explanation. `misconfigured` is therefore never a
token you will read out of a report — `InvalidConfiguration` can only come from
constructing the provider, which happens before a report exists. It is in the
vocabulary because it is the right name for the condition, and a caller
branching on exit `2` applies it itself rather than inventing a sixth word. That is what lets the menu-bar app's
connection test branch without parsing prose, and it is also why a missing
configuration is `2` rather than `1` — nothing was attempted, and the fix is
entirely local.

`actions::llm::llm_check_outcome` is the one producer of those five tokens, and
`scripts/check-vocabulary-covers-cli-tokens.sh` diffs the set of tokens the
menu-bar app has wording for against the set that function can return, in CI —
so the CLI and the GUI cannot drift into describing different sets of failures,
and adding a sixth failure mode in Rust fails a check rather than silently
reaching a user as a blank explanation.

## Configuring this from the menu-bar app

**Settings → AI Provider** (HORO-1309) configures the same three values from
the GUI, for the common case of someone who runs the app from Finder and never
exported anything. A Finder-launched `LSUIElement` agent inherits no shell
environment at all, so before this screen existed the GUI's AI Plan card could
only work for someone who had launched the app *from* a configured shell.

| Field | Stored in |
|---|---|
| Endpoint (API root) | `UserDefaults.standard` — for a bundled app, the preference domain named by its bundle identifier |
| Model | the same |
| API key | the **login keychain**, account `llmApiKey`, service = the app's bundle identifier |

Both namespaces are derived from the running bundle rather than written out as
literals (HORO-1456), so a beta, renamed or diagnostic build gets its own stored
settings and its own keychain items instead of the released app's.

### The precedence rule

**Per field, what this app has configured wins over what it inherited.** A
field left empty here falls back to the inherited `GLOMERIS_LLM_*` variable,
including falling back to it being absent. Nothing is ever removed — clearing a
GUI field returns you to the environment rather than switching a working setup
off.

Stated as a rule: *the settings screen never lies.* If the endpoint field shows
a URL, that is the URL used. The alternative — environment-wins — was rejected
precisely because it lets the screen display one endpoint while silently using
another, with no way to correct it from the GUI.

Each field's row says where its effective value came from (`Configured here`,
`From the environment`, or `Not set`), so an inherited value is visible rather
than mysterious. An empty-or-whitespace value counts as absent on both sides,
by the same rule the Rust `provider_from_parts` uses, so an exported-but-empty
`GLOMERIS_LLM_MODEL=` does not masquerade as configuration.

**The CLI is unaffected either way** (AC 3). `glomeris` in a terminal reads that
terminal's environment and knows nothing about this store. Nothing in this
screen changes, shadows or requires anything for a shell user.

### Where the key goes, and where it does not

Into the environment of the `glomeris` child process, and nowhere else. It is
read from the keychain at the moment a command is spawned and not cached.

It is never a command-line argument (visible to `ps`, and refused by the CLI by
flag name), never `UserDefaults`, never a file the app writes, never a log line,
and never rendered — the field is a `SecureField`, there is no reveal control,
and the typed text is cleared the moment it is handed to the keychain, whether
the write succeeded or not. `GlomerisLlmSettingsStore` deliberately has **no
getter for the key**; the only code that reads it is the one function that puts
it straight into a child environment, so "show the user where their key came
from" and "put the key on screen" cannot become the same operation.
`scripts/check-credential-store-uses-keychain.sh` asserts that in CI over every
line in the app that touches the key.

Removing it is explicit (AC 8): a **Remove key** button, worded as the
destructive action it is, which deletes the keychain item. Afterwards the field
falls back to the environment if one is exported, and reads `Not set` if not.

Note what that fallback means, because the precedence rule cuts the other way
here. If `GLOMERIS_LLM_API_KEY` was exported into the environment this app was
launched from, deleting the stored key does not stop Glomeris reaching a
provider — it *promotes* the inherited key into use. Someone pressing a button
labelled **Remove key** is more likely revoking access than tidying a field, so
the app says so in that case rather than reporting a bare "Key removed": it
names the variable and tells you to unset it too. Unsetting it means relaunching
the app from an environment without it — a running process's environment is not
editable from the settings screen.

### The privacy preview

The same screen offers a preview of the outbound payload, over `glomeris
llm-plan --print-payload --json` with the app's configured project roots — the
same scope a real plan would use, so the preview is of *your* payload and not a
generic example. It separates **what leaves this Mac** (the two prompts) from
**what stays on this Mac** (the wire-alias table, which is where the absolute
paths are).

Opening it sends nothing, and cannot: `--print-payload` returns before a
provider is constructed (AC 7). The preview runs without the credential in its
environment at all — only the connection test is given it — so there is no
version of this screen in which looking at the payload transmits it.

## `LlmPlan` schema — the concrete `--plan-file` example (HORO-1048)

`glomeris llm-plan --schema` prints an example, syntactically valid
`LlmPlan` JSON document to stdout — the canonical way to get a starting
point for a `--plan-file` fixture without reading `src/actions/llm.rs`'s
`LlmPlanItem` struct directly. Its `resource_id` is in the
`ResourceId::to_string()` form because a human writing a fixture by hand
names resources the way `glomeris detect` prints them; a live model is
handed wire aliases instead, and answers with those:

```
glomeris llm-plan --schema
```

```json
{
  "items": [
    {
      "resource_id": "cargo_target_dir:/path/to/project/target",
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
| `resource_id` | `String`, required | Must match either a positional wire alias (`resource_1`, …) from the request the model was given, or — for a hand-written `--plan-file` fixture — a `ResourceId::to_string()` from the evidence set `plan_with_llm` was called with. Both are looked up in sets this process built from its own discovery, so neither can name a resource that was not found locally. An unmatched value drops just that item (`dropped_unknown_resource`), never the whole plan. |
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

### What is refused locally, before anything is sent

`actions::llm::validate_base_url` runs when the provider is constructed, so a
base URL that cannot possibly work fails with `LlmError::InvalidConfiguration`
and exit `2` — no request, no round trip to blame. The refusals, each with the
message naming what to change:

| Refused | Why |
|---|---|
| Empty | Nothing to route to; there is deliberately no default base URL |
| Leading or trailing whitespace | A pasted URL routinely carries a trailing newline, and the resulting failure is a 404 nobody can explain by looking at the field |
| An interior space | A URL cannot contain one |
| No scheme, or a scheme other than `https://`/`http://` | Nothing else can be posted to |
| No host (`https://`, `https:///v1`) | Same |
| A query string or a fragment | Appending `/chat/completions` to `…/v1?key=…` puts the path *inside* the query string, and the diagnostic path would then read `/v1` and misdescribe why. Refusing also keeps a credential a user pasted into the URL out of the request Glomeris builds |
| The full endpoint URL (ends in `/chat/completions`) | Would produce `/v1/chat/completions/chat/completions` — the other half of the API-root mistake above |

The message never quotes the offending value, on purpose: of the three
settings, the one a user most plausibly pastes into the wrong field is the
credential.

What is *not* refused is the host-root form above. It is a perfectly valid URL
that some providers really do serve their API at, so only the provider can say
whether it works — which is what `glomeris llm-check` is for.

Do not try to identify this mistake from the status code: a gateway may
answer an unrouted path with `404`, but `403`, `401` and even `400` are all
things real gateways return instead. The **path** in the error message is the
path that was actually requested, so it tells you directly which of the two
forms you configured — and since a path of exactly `/chat/completions` can
only have come from a base URL with no path at all, Glomeris says so itself
rather than leaving you to notice:

```text
provider returned HTTP 403 for openai:chat_completions POST /chat/completions:
{"error":"..."} — the configured address has no path, so this request went to
the host root; most OpenAI-compatible providers serve their API under /v1, so a
missing /v1 is the likeliest cause
```

(One line in reality; wrapped here to fit.)

The sentence comes after the provider's own words, never instead of them, and
it is a hint rather than a verdict: the host-root form is valid and some
providers really do serve their API there, so this cannot be a refusal. A
rejection at a configured API root — `…/v1/chat/completions` — gets no such
sentence at all, because a `401` there is about the key.

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
The classes are deliberately distinct, because each has its fix in a different
place — `glomeris llm-check` reports the same distinction as its five-token
`outcome`:

| Class | Meaning |
|---|---|
| `LLM provider configuration is invalid: …` | Refused locally; no request was attempted at all |
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

When that path is exactly `/chat/completions`, the message also names the
host-root cause described above — the one reading of the path that needs no
knowledge of the host to make.

`actions::llm::provider_from_env` is the only place `std::env::var` is
called for these — the key is held just long enough to build the
`OpenAiCompatibleProvider` and is never logged, printed, or returned any
other way. A missing or empty value for any of the three returns
`LlmError::NotConfigured`; `glomeris llm-plan` and `glomeris llm-check` report
this by naming the three variable *names*, never a value, and exit `2`.

A value that is present but cannot work returns
`LlmError::InvalidConfiguration` instead, and also exits `2` (HORO-1309).
Keeping the two apart matters: "you have not set this up" and "you set it up
wrongly, here is what to change" have different fixes, and reporting the second
as the first sent users to re-export three variables that were already set.

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
- No absolute path, home directory or account name is transmitted. Asserted
  at three levels: on the payload struct
  (`payload_carries_no_path_or_account_name`), on the CLI report
  (`build_llm_payload_report_separates_outbound_prompts_from_local_aliases`),
  and on the bytes of the real HTTP request, captured by a loopback
  listener standing in for the provider
  (`tests/llm_plan_egress_privacy.rs`) — headers included. That last test
  also pins the two halves that make the proof non-vacuous: the payload
  demonstrably *does* describe a path-backed resource, and the real path
  *was* available locally and was withheld deliberately.
- Wire aliases are positional, so the same request shape carries
  byte-identical ids regardless of what the resources are
  (`wire_ids_are_positional_and_independent_of_the_resource`) — there is no
  stable identifier in the payload that could correlate a machine across
  requests.
- An alias resolves only through the request's own table, and a real-id
  string only through the evidence set: a model that invents either — an
  out-of-range `resource_99`, or a guessed path it was never shown — names
  nothing (`out_of_range_wire_alias_is_dropped_and_counted`,
  `model_supplied_path_cannot_name_an_undiscovered_resource`). No
  model-provided identifier gains execution authority; policy still
  classifies every surviving item.
- The serialized view's key set is pinned to its seven documented fields
  (`serialized_view_exposes_exactly_the_documented_fields`), so adding a
  field to `LlmResourceView` — the only way to widen what leaves — fails a
  test rather than passing silently.
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
- The connection test's two prompts are constants, and
  `connection_test_prompts_describe_nothing_local` asserts they contain no path
  separator, no home directory and no account name — so testing a
  configuration costs one trivial completion and discloses nothing.
- The GUI's provider settings are held to the same line by
  `macos/GlomerisMenuBar/Tests/AiProviderPreferencesViewTests.swift`, which
  asserts several things as *absences*, since they are code paths that must not
  exist: the key is bound to a `SecureField` and never read back or revealed;
  the typed text is cleared before the success-or-failure branch, so a failed
  keychain write cannot leave it in memory behind a visible error; neither
  command can run without the user pressing something (no `.onAppear`, no
  `.task`, no `Timer`, no `.refreshable`); exactly one call site is given the
  credential, and it is not the payload preview; and the preview's outcome enum
  has no "not configured" case at all — a preview that could say that would be
  a preview that had tried to use a provider. Both `llm-check` reports are
  additionally pinned by golden fixtures on both sides
  (`tests/dto_golden_fixtures.rs`, `DtoGoldenFixturesTests.swift`), asserting
  no `base_url`, no `api_key` and no `://` reaches a report a UI shows and a
  log keeps.
- The GUI half of AI Plan is held to the same line by
  `macos/GlomerisMenuBar/Tests/AiPlanSectionViewTests.swift`: a row built from
  a confident, plausible recommendation to delete a credential carries the
  refusal and no willingness to act, and the card has no execution call site,
  no fingerprint token and no re-sorting of the model's order. The repo-wide
  `scripts/check-no-policy-label-branching.sh` additionally proves nothing in
  the app — this card included — branches on a `policy_label` string to decide
  what may run.

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
