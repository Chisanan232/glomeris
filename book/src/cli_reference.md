# CLI Reference

**The built-in help is canonical.** `glomeris <command> --help` renders from
`src/cli/help.rs`, which is the one place a command's usage, flags, safety
semantics, examples and exit codes are written down, and whose output is held
to golden snapshots in `tests/fixtures/help/` (HORO-1311). If this page and
`--help` ever disagree, `--help` is right and this page is stale.

What this page adds that `--help` deliberately does not: the JSON payload
shapes, the report field semantics, and the cross-references into the rest of
this book. It is the reference you read at a desk; `--help` is the one you read
mid-task.

## `glomeris`

No arguments: prints `glomeris <version>` (from `CARGO_PKG_VERSION`) and a
pointer to `glomeris --help`. `--version`/`-V` prints the same version line on
its own.

## Help surfaces

Everything below renders from the single `COMMANDS` table in
`src/cli/help.rs`. That table lived in `src/main.rs` until HORO-1311, where no
test could read it — so `tests/shared_command_table.rs` kept a hand-written
mirror of it, which drifted exactly as the pre-HORO-1050 duplication had
(HORO-1034: the unrecognized-command usage omitted `llm-plan`; by HORO-1311 the
test mirror omitted `llm-check`). There is now one table, read directly by both
the binary and its tests.

| Invocation | What it prints |
|---|---|
| `glomeris --help` / `-h` / `help` | Every command, grouped by what it does to your machine, with a one-line summary each. |
| `glomeris <command> --help` / `-h` | That command's usage, safety statement, flags, examples, exit codes and related commands. |
| `glomeris help <command>` | Identical to `glomeris <command> --help`. |
| `glomeris help exit-codes` | The exit-status reference (see [Exit codes](#exit-codes)). |
| `glomeris help <unknown-topic>` | The list of topics that exist, to stderr, exit 2. |
| `glomeris <unknown-command>` | A one-line error and a pointer to `--help`, to stderr, exit 2. |
| `glomeris <command> <bad-flag>` | *That command's* usage only, to stderr, exit 2. |

The groups in top-level help are `INSPECT`, `PLAN`, `ACT`, `OBSERVE` and
`SERVICE`, ordered so that the read-only commands come before anything that can
delete. A group heading describes consequence, not category: `INSPECT` says
"nothing is changed", and `ACT` says its commands delete data and that every
deletion is policy-gated.

Note that a command's group and its per-command safety line describe only
whether *the command itself* writes to the filesystem. They are not policy
classifications: `AUTO_SAFE`, `ASK` and `PROTECTED` classify *resources*, are
decided by the policy engine, and never appear in a help safety label. See
[Safety Model](safety_model.md).

Two surfaces are narrower on purpose. An unrecognized top-level command gets a
pointer rather than the manual — it previously reprinted the aggregate usage of
all thirteen commands, a 13-line, 146-column wall in answer to one mistyped
word. And a usage error inside a command prints only that command's usage, so
getting a `free` flag wrong no longer tells you about `daemon`.

## `glomeris daemon <subcommand>`

macOS only (exits 1 with an error message on other platforms).

| Subcommand | Effect |
|---|---|
| `install` | Writes a per-user `launchd` plist and `launchctl load -w`s it. |
| `uninstall` | `launchctl unload -w`s the agent (best-effort) and removes the plist file. |
| `status` | Prints whether the plist is installed, its path, and whether `launchctl` reports it loaded. |
| `run` | Runs the polling loop in the foreground (this is what the installed agent actually executes). |

No subcommand, or an unrecognized one, prints usage to stderr and exits 2.
See [Daemon Lifecycle](daemon_lifecycle.md) for details.

`daemon status --json` (HORO-1045) prints a `DaemonStatusReport`.
`loaded` (launchd-reported) and `heartbeat_age_secs` (derived from the poll
loop's own last-write) are deliberately kept as two separate fields, never
collapsed into one `healthy` boolean — a loaded-but-wedged daemon and an
actually-polling one must stay distinguishable:

```sh
glomeris daemon status --json
```

```json
{
  "plist_installed": true,
  "plist_path": "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist",
  "loaded": true,
  "heartbeat_age_secs": 42
}
```

`heartbeat_age_secs` is `null` when no heartbeat file exists yet (the
daemon has never run).

## `glomeris status [--json]`

macOS only (exits 1 with an error message on other platforms). Not part of
`daemon` — this is the same one-shot disk-pressure reading `daemon run`'s
poll loop evaluates each tick, available on demand without needing the
daemon installed at all (HORO-955).

```sh
glomeris status --json
```

```json
{
  "total_bytes": 500000000000,
  "free_bytes": 125000000000,
  "used_percent": 75.0,
  "free_human": "116.4 GB",
  "total_human": "465.7 GB",
  "pressure_state": "WARN"
}
```

`pressure_state` is one of `"OK"`, `"WARN"`, `"CRITICAL"` — see
[Pressure Model](pressure_model.md).

## `glomeris scan [path] [top_k]`

Not macOS-gated. Both arguments are positional and optional:

- `path` — root directory to scan. Defaults to `.`.
- `top_k` — number of largest entries to report. Defaults to `20` if
  omitted or unparseable as `usize`.

Prints a summary line (files visited, stop reason, incomplete-entry count)
followed by one line per candidate: size, depth, path.

## `glomeris detect [--project-root <path>]... [--json] [--progress-json]`

Not macOS-gated. Runs `DetectorRegistry::builtin()`'s `discover_all` once and
prints, per detector: `found (<N> evidence)`, `tool_absent`, or
`failed: <reason>`.

`--project-root <path>` is optional and repeatable — pass it once per
project directory you want the cargo/node detectors to check for a
`target/`/`node_modules/` dir. Without it, those two detectors have no
project roots to scan and always report `tool_absent`.

`--json` prints a `DetectReport` — one candidate line per discovered
resource, including the already-computed
`executable`/`offered_actions`/`refusal_reason` triple (HORO-1053) a caller
(e.g. the menu-bar app) reads to decide what it can offer, without ever
re-deriving that from `policy_label`/`reasons` itself:

Candidates are returned **biggest reclaimable size first** (HORO-1307).
Before that they came back in detector-registration order, so a 40 GB Cargo
`target/` could be printed below a 2 MB npm cache. Ties are broken first by
measurement quality — an exact size outranks a `≥` lower bound of the same
number, because the exact one is the claim you can act on — and then by
`resource_id`, so two runs over an unchanged machine produce the same order.
Candidates whose size could not be measured at all sort last rather than
being treated as zero. Ordering is applied at the single point where the
report is assembled, so `--json` and the human-readable output can never
disagree about it.

`impact_tier` is `"unknown"`, `"normal"`, `"notable"` or `"large"`: a
pre-computed magnitude band, so a UI does not have to invent thresholds of
its own. It escalates on **either** an absolute size (≥ 1 GB is `notable`,
≥ 10 GB is `large`) **or** a share of remaining free space (≥ 5% is
`notable`, ≥ 20% is `large`) — measured against free rather than total space,
because the problem someone opens Glomeris with is "I am running out of
room". A 400 MB cache is `large` on a machine with 1.6 GB left.

`impact_tier` is a size signal and nothing else. It is **not** a safety
signal, and it must never be read as one: a `large` candidate can be
`PROTECTED`, and a `normal` one can be `AUTO_SAFE`. `executable`,
`offered_actions` and `refusal_reason` remain the only statement about what
Glomeris is permitted to do.

```sh
glomeris detect --project-root ~/dev/myproject --json
```

```json
{
  "candidates": [
    {
      "resource_id": "cargo_target_dir:/Users/dev/proj/target",
      "kind": "cargo_target_dir",
      "reclaimable_bytes": 2147483648,
      "reclaimable_human": "2.0 GB",
      "reclaimable_bytes_is_lower_bound": false,
      "impact_tier": "notable",
      "policy_label": "AUTO_SAFE",
      "reasons": ["no_active_use_observed"],
      "executable": true,
      "offered_actions": [
        {
          "action_id": "cargo.clean.target_dir",
          "requires_confirmation": false
        }
      ],
      "refusal_reason": null
    },
    {
      "resource_id": "docker_build_cache:docker",
      "kind": "docker_build_cache",
      "reclaimable_bytes": 10737418240,
      "reclaimable_human": "10.0 GB",
      "reclaimable_bytes_is_lower_bound": true,
      "impact_tier": "large",
      "policy_label": "UNKNOWN_INCOMPLETE",
      "reasons": ["evidence_incomplete"],
      "executable": false,
      "offered_actions": [],
      "refusal_reason": "no registered cleanup action for this resource kind"
    }
  ]
}
```

`--progress-json` (HORO-1052) emits one NDJSON-encoded `ProgressEvent` line
to **stderr** per detector start/finish while discovery runs — a way for a
spawning UI to distinguish "still working" from "hung" on a slow/contended
host (v0.2.0 founder-dogfood measured a single discovery pass up to 3m40s).
Stdout is completely unaffected either way, `--progress-json` output can be
combined with `--json`, and nothing is emitted at all unless the flag is
passed:

```sh
glomeris detect --progress-json 2>&1 1>/dev/null
```

```
{"phase":"detector_started","detector":"cargo_target_dir"}
{"phase":"detector_finished","detector":"cargo_target_dir","candidates_found":1}
{"phase":"detector_started","detector":"node_modules"}
{"phase":"detector_finished","detector":"node_modules","candidates_found":0}
```

`detect`, `explain`, `llm-plan`, and `execute` all share this same
discovery phase and all support `--progress-json` identically.

## `glomeris explain <resource_id_or_path> [--project-root <path>]... [--json] [--progress-json]`

Not macOS-gated. Runs the same discovery-and-classification pipeline as
`detect`, then prints the full evidence-and-policy picture for exactly the
one resource matching `<resource_id_or_path>` (a `detect` report's
`resource_id`, or a filesystem path) — evidence provenance, size (logical
vs. reclaimable, explicitly labeled as different), active-use signals, and
policy classification (HORO-955). Exits `1` if no discovered candidate
matches the query.

`--project-root <path>` and `--progress-json` mean exactly what they mean
for `detect` above.

```sh
glomeris explain cargo_target_dir:/Users/dev/proj/target --json
```

```json
{
  "resource_id": "cargo_target_dir:/Users/dev/proj/target",
  "kind": "cargo_target_dir",
  "detector": "cargo_target_dir",
  "sources": ["cargo metadata: target-dir"],
  "logical_bytes": 2147483648,
  "logical_human": "2.0 GB",
  "reclaimable_bytes": 2147483648,
  "reclaimable_human": "2.0 GB",
  "reclaimable_bytes_is_lower_bound": false,
  "completeness": "complete",
  "confidence": "high",
  "active_use_signals": [],
  "regenerability": "regenerable_by_rebuild",
  "policy_label": "AUTO_SAFE",
  "reasons": ["no_active_use_observed"],
  "native_cleanup_available": true,
  "native_cleanup_action_id": "cargo.clean.target_dir",
  "fingerprint_token": "<opaque token — copy verbatim, never hand-construct>",
  "executable": true,
  "offered_actions": [
    {
      "action_id": "cargo.clean.target_dir",
      "requires_confirmation": false
    }
  ],
  "refusal_reason": null
}
```

`fingerprint_token` (HORO-1051) is an opaque, wire-safe encoding of the
resource's identity fingerprint — `null` for a resource with no dev/inode/
mtime identity (e.g. Docker's build cache). This is the exact token
`execute --observed-fingerprint` later expects back for an `ASK`-classified
resource; see `execute`'s section below.

## `glomeris clean --dry-run [--target <resource_id_or_path>] [--project-root <path>]...`

Not macOS-gated. Renders what would be cleaned, without executing
anything — `--dry-run` is required; there is no non-dry-run execution path
on this subcommand (real destructive execution is `glomeris free --target`
or `glomeris execute`'s job). Without `--target`, every discovered
candidate is considered; with it, only the one matching resource is.

```sh
glomeris clean --dry-run
```

Human-readable output only — `clean --dry-run` has no `--json` mode. One
line per considered resource, either its rendered `ActionPlan.explain` text
or a `skip_reason` (e.g. `PROTECTED`, no registered action).

## `glomeris llm-plan [--project-root <path>]... [--plan-file <path>] [--json] [--progress-json] [--schema] [--print-payload]`

Not macOS-gated. ADVISORY, NON-EXECUTING (HORO-1008) — never constructs a
`policy::Approval` and never calls `policy::approval::authorize` or
`executor::execute`. See [BYOK LLM Planner](byok.md) for the full
configuration and safety-property writeup.

- Without `--plan-file`, credentials are read only from
  `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL` (all
  three required, no default base URL) — never from a CLI flag.
  `GLOMERIS_LLM_BASE_URL` is the **API root**: `/chat/completions` is
  appended to it verbatim, so it usually ends in `/v1`
  (`https://gateway.example.com/v1`). See
  [BYOK LLM Planner](byok.md#glomeris_llm_base_url-is-the-api-root-not-the-host-root).
- A failed provider call is reported on the `provider error:` line (or the
  `provider_error` JSON field) as one secret-free sentence naming the HTTP
  status, API style, request path, request id, and a bounded excerpt of the
  provider's error body — never the `Authorization` header, the key, the
  host, or the request payload.
- `--plan-file <path>` reads the file's raw bytes as if they were the
  model's raw response text, through the same validation pipeline the live
  provider uses — no network call, no API key required.
- `--project-root <path>` is optional and repeatable, same meaning as
  `detect`'s flag above.
- `--api-key`/`--key`/`--token` are explicitly rejected (not accepted and
  ignored) — the error names `$GLOMERIS_LLM_API_KEY` instead.
- `--json` prints the report as JSON. Each item carries the model's own
  `priority`/`model_reason` alongside the machine's `policy_label`,
  `requested_action_id`, `explain`, `skip_reason`, `completeness`,
  `confidence`, and a nested `candidate` — the byte-for-byte same projection
  `detect --json` prints for that resource, including
  `executable`/`offered_actions`/`refusal_reason` (HORO-1308). A consumer
  deciding what may be done reads `candidate`; `priority`/`model_reason` are
  the only two fields a provider chose, and the **order of `items` is the
  provider's too** — `plan_with_llm` never sorts, it only drops. `--json`
  output is printed *before* a `provider_error` exit, so an exit `1` report is
  still complete and readable.
- `--progress-json` streams the same NDJSON discovery progress as `detect`,
  on stderr, leaving stdout a single clean JSON document. This is the exact
  pair (`--json --progress-json`) the menu-bar app's AI Plan card spawns; see
  [Menu Bar App](menu_bar_app.md#ai-plan).
- `--print-payload` (HORO-1298) runs discovery, prints the exact request a
  live run would send — the system prompt, the user prompt, and the local
  wire-id-to-real-resource table under a heading marking it as *not* sent —
  then returns before any provider is constructed. Requires no credential
  and makes no network call, so it cannot send what it displays. The
  outbound prompts identify resources only by positional alias
  (`resource_1`, …); absolute paths appear in the alias table and nowhere
  else. See [BYOK LLM Planner](byok.md#what-leaves-your-machine).
- `--schema` (HORO-1048) prints an example, syntactically valid `LlmPlan`
  JSON document to stdout and exits — a distinct, self-contained mode
  that never runs discovery, never reads `--plan-file`, and never checks
  live-mode credentials, regardless of what else is passed alongside it.
  See [BYOK LLM Planner](byok.md#llmplan-schema--the-concrete---plan-file-example-horo-1048)
  for the full example and field table.

```sh
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

This exact output round-trips unchanged through `glomeris llm-plan
--plan-file <path>` — see the linked BYOK page for the full field table and
the round-trip test that proves it. The `resource_id` form shown here is
the one a human writes by hand in a fixture; a live model is given
positional wire aliases and answers with those, and both forms resolve.

Human-readable output always opens with `LLM SUGGESTION — advisory only,
nothing is executed by this command`.

Exit codes for this subcommand specifically:

- `0` — success, including zero suggestions or every suggestion being
  `PROTECTED`.
- `1` — the provider call or response parsing failed, `--plan-file`
  named an unreadable path, or `--print-payload` could not serialize the
  request.
- `2` — usage error: an unrecognized argument, `--plan-file` with no value,
  missing live-mode environment configuration, or an `--api-key`/`--key`/
  `--token` flag.

## `glomeris llm-check [--json]`

Not macOS-gated. Tests the configured BYOK setup and reports whether the
endpoint, credential and model work (HORO-1309). Runs no detectors, reads no
project roots, collects no evidence, and consults no policy — it is not a
planning command, and there is nothing it could execute.

- Sends the two fixed prompts in `actions::llm::CONNECTION_TEST_SYSTEM_PROMPT`
  / `CONNECTION_TEST_USER_PROMPT` ("You are a connection test. Reply with the
  single word: ok." / "ok") through the same `LlmProvider::complete` a real
  plan uses. Same code path, so a pass means a plan will route and authenticate
  — not that a cheaper probe succeeded.
- The prompts are constants with no interpolation, so a connection test
  describes nothing about this machine: no path, no home directory, no account
  name (`connection_test_prompts_describe_nothing_local`).
- Configuration comes only from
  `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL`, all
  three required, exactly as for `llm-plan`.
  `--api-key`/`--key`/`--token` are rejected by flag name, and the error names
  the flag only — never the value beside it.
- `--json` prints an `LlmCheckReport`: `outcome`, `model`, `endpoint_path`,
  `error`, `response_excerpt`. Human-readable output opens with
  `LLM CONNECTION OK` or `LLM CONNECTION FAILED (<outcome>)`.
- `outcome` is one of five tokens, produced by the single
  `actions::llm::llm_check_outcome` mapping so the CLI, the book and the
  menu-bar app cannot disagree about what a failure was:

| `outcome` | What happened | Where the fix is |
|---|---|---|
| `ok` | The provider answered and its reply is excerpted in `response_excerpt` | — |
| `unreachable` | No HTTP response at all — DNS, TLS, refused connection, timeout | Network, host name, or VPN |
| `rejected` | The provider answered with a non-2xx status | Credential, or the base-URL path — read the path in `error` |
| `unusable_response` | A 2xx response that was empty, not JSON, or missing `choices[0].message.content` | Model name, or a gateway not actually speaking the OpenAI shape |
| `misconfigured` | A base URL that cannot work — `validate_base_url` refused it before anything was sent | The base URL itself; see [BYOK LLM Planner](byok.md#glomeris_llm_base_url-is-the-api-root-not-the-host-root) |

  The fifth is the odd one out: `LlmError::InvalidConfiguration` can only come
  from constructing the provider, which happens *before* any report exists, so
  `glomeris llm-check` never prints a report whose `outcome` is
  `misconfigured` — it exits `2` with one line on stderr instead. The token is
  in the vocabulary because that is the name for what happened, and a caller
  branching on exit `2` (the menu-bar app does) classifies it that way itself
  rather than inventing a sixth word for the same condition.

- `error` carries the same secret-free sentence `llm-plan`'s `provider_error`
  does — status, API style, request path, `x-request-id` when the provider
  sends one, and a bounded excerpt of the provider's own error body, with the
  configured key scrubbed out of it. Never the `Authorization` header, the
  key, the scheme, the host, or the query string.

```sh
glomeris llm-check --json
```

Exit codes for this subcommand specifically:

- `0` — the provider answered and `outcome` is `ok`.
- `1` — a check ran and did not pass (`unreachable`, `rejected`,
  `unusable_response`). **The report is printed first**, so an exit `1` still
  carries a complete, readable diagnosis on stdout.
- `2` — nothing was sent and there is no report at all: an unrecognized
  argument, an `--api-key`/`--key`/`--token` flag, a base URL
  `validate_base_url` refused, or missing configuration (the message names
  the three variable *names*, never a value). Stdout is empty; the reason is
  one line on stderr prefixed `glomeris llm-check: `.

The split matters for a caller branching on the code without parsing output,
which is exactly what the menu-bar app's connection test does: `2` means "fix
your invocation or setup", never "the network is having a bad day". A `2` with
no report is the one case where a UI has to quote the CLI's own sentence rather
than render a report.

## `glomeris execute --action-id <id> --resource-id <id> [--project-root <path>]... [--confirm-ask --observed-fingerprint <token>] [--json] [--progress-json]`

macOS only (exits 1 with an error message on other platforms). The sole
interactive destructive-execution subcommand (HORO-1055) — the only place
in this CLI where a caller can trigger one specific, real destructive
action against one specific, real resource. `--action-id` and
`--resource-id` are both required; the caller supplies ONLY these
selectors (plus, for `ASK`, an observed fingerprint token) — there is no
flag to pass a `PolicyClass`, a raw filesystem path as a direct target, a
shell string, or any `--force`/override.

Internal flow, in one process:

1. Acquires the same HORO-1054 execution lock `free`/`emergency` use —
   held for the whole call, released on exit.
2. Runs the same discovery-and-classification pipeline `detect`/`explain`
   use (`--project-root <path>` is optional and repeatable, same meaning
   as elsewhere).
3. Resolves `--resource-id` against the discovered candidates and
   `--action-id` against that resource's own registered action —
   refusing if either does not resolve, or if the resolved action's id
   differs from `--action-id`.
4. For an `ASK`-classified resource, builds consent ONLY from decoding
   `--observed-fingerprint` (via the same token format `explain --json`'s
   `fingerprint_token` field emits) — never from a fingerprint freshly
   observed by this same process, which would defeat the whole
   fingerprint-pinning purpose. `--confirm-ask` and
   `--observed-fingerprint` must be passed together or not at all.
5. Calls the real, unmodified `policy::approval::authorize`, then — only
   if it returns an approval — the real, unmodified `executor::execute`.
   `PROTECTED` refuses unconditionally regardless of any flag
   combination; `execute`'s own deletion-time revalidation can still
   abort a plan that was authorized a moment earlier if the resource
   changed in between.

`--json` prints an `ExecuteReport` (action id, resource id, outcome,
failure/abort detail, expected vs. actual reclaimed bytes — the latter is
a real measurement, taken after execution, not an estimate) on the
`Executed` path. Every refusal/not-found/busy path instead prints an
`ExecuteRefusalReport` (`{"reason": "...", "message": "..."}`) to stdout
before exiting with the matching code below, so a `--json` caller never
gets silent stdout on a non-`Executed` outcome. `reason` is one of
`resource_not_found`, `action_not_found`, `action_mismatch`, `protected`,
`ask_no_consent`, `ask_consent_mismatch`, `auto_safe_contract_violation`,
or `busy` (HORO-1056: the lock-contention case below, the one refusal
that happens before discovery/resolution even runs) — each a distinct,
machine-readable value naming exactly which refusal/abort path fired,
never a generic error string.

An `AUTO_SAFE` resource needs no confirmation flags at all:

```sh
glomeris execute --action-id cargo.clean.target_dir \
  --resource-id cargo_target_dir:/Users/dev/proj/target --json
```

```json
{
  "action_id": "cargo.clean.target_dir",
  "resource_id": "cargo_target_dir:/Users/dev/proj/target",
  "outcome": "succeeded",
  "failure_message": null,
  "abort_reason": null,
  "expected_reclaimed_bytes": 2147483648,
  "actual_reclaimed_bytes": 2147483648
}
```

An `ASK`-classified resource requires `--confirm-ask` plus the exact
`--observed-fingerprint` token captured from a prior `explain --json` call
on that same resource (never a fingerprint freshly observed by `execute`
itself) — `$FINGERPRINT_TOKEN` below is that call's `fingerprint_token`
field, copied verbatim, never hand-constructed:

```sh
glomeris execute --action-id cargo.clean.target_dir \
  --resource-id cargo_target_dir:/Users/dev/proj/target \
  --confirm-ask --observed-fingerprint "$FINGERPRINT_TOKEN" \
  --json --progress-json
```

If the resource's identity changed between the `explain` call and this
`execute` call, revalidation aborts the plan rather than proceeding:

```json
{
  "action_id": "cargo.clean.target_dir",
  "resource_id": "cargo_target_dir:/Users/dev/proj/target",
  "outcome": "aborted_by_revalidation",
  "failure_message": null,
  "abort_reason": "ResourceIdentityChanged",
  "expected_reclaimed_bytes": 2147483648,
  "actual_reclaimed_bytes": null
}
```

Every refusal path (e.g. `PROTECTED`, no consent supplied, a stale
fingerprint) prints an `ExecuteRefusalReport` instead, with `--json`:

```json
{
  "reason": "protected",
  "message": "refused — this resource is PROTECTED; no flag combination can authorize executing against it"
}
```

Exit codes for this subcommand specifically:

- `0` — the action executed and succeeded.
- `1` — the action executed but failed (a step of the plan errored).
- `2` — usage error: an unrecognized/missing argument, `--confirm-ask`
  without `--observed-fingerprint` (or vice versa), or a malformed
  `--observed-fingerprint` token.
- `3` — refused by policy: `PROTECTED` (unconditional), `ASK` with no
  consent supplied, or `ASK` with a supplied consent that did not match
  the freshly observed fingerprint.
- `4` — aborted by `execute`'s own deletion-time revalidation (a TOCTOU-
  style guard: the resource's identity or policy classification changed
  between authorization and execution).
- `5` — `--resource-id` matched no discovered candidate, the resource had
  no registered action, or the resolved action's id did not match the
  supplied `--action-id`.
- `75` — the execution lock is already held by another `glomeris`
  invocation (see `free`'s exit codes above; `execute` reuses the exact
  same `EXIT_EXECUTION_LOCK_BUSY` constant — this ticket's own AC
  described this case as exit `6`, but the already-established lock
  convention from HORO-1054 is kept rather than introducing a second,
  conflicting "busy" code). With `--json`, this prints an
  `ExecuteRefusalReport` with `reason: "busy"` to stdout (HORO-1056) —
  previously this path was silent on stdout even under `--json`.

## `glomeris emergency`

macOS only (exits 1 with an error message on other platforms). Takes no
arguments, and rejects any with exit 2 — until HORO-1311 it silently discarded
them, so `glomeris emergency --dry-run` performed a real recovery run. See
[Emergency Mode](emergency_mode.md).

## `glomeris history [--json] [--limit <N>]`

Reads back a bounded, oldest-first tail of the monitor's `history.tsv`
(HORO-1046) — the same append-only file `daemon run`'s poll loop already
writes via `PersistenceBackend::record`. No new persistence format; this is
a read path only.

`--limit <N>` is optional and defaults to 20. It bounds how many of the
most recent pressure transitions are returned — a malformed line in
`history.tsv` is skipped rather than failing the whole read, and a missing
history file (the daemon has never run, or never recorded a transition)
renders as an empty list rather than an error.

With `--json`, prints a `HistoryReport` (`{"events": [...]}`); each event
has `unix_time_secs`, `from`, `to`, `used_percent`, `free_bytes`, and
`free_human`. Without `--json`, prints one line per event as plain text.

```sh
glomeris history --json --limit 2
```

```json
{
  "events": [
    {
      "unix_time_secs": 1700000000,
      "from": "OK",
      "to": "WARN",
      "used_percent": 82.5,
      "free_bytes": 80000000000,
      "free_human": "74.5 GB"
    },
    {
      "unix_time_secs": 1700000600,
      "from": "WARN",
      "to": "CRITICAL",
      "used_percent": 95.1,
      "free_bytes": 20000000000,
      "free_human": "18.6 GB"
    }
  ]
}
```

## `glomeris actions <list [--json]|history [--json] [--limit <N>]>`

Neither subcommand is macOS-gated — `list` touches no filesystem/launchd
state at all, and `history` only reads a plain file.

### `glomeris actions list [--json]`

Enumerates every action currently registered in `ActionRegistry::builtin()`
(HORO-1047) — the read path that replaced having to read
`src/actions/homebrew.rs` source directly to find a real action id string.
`applies_to` is a direct projection of each action's own `Action::applies_to`,
never a hand-maintained list.

```sh
glomeris actions list --json
```

```json
{
  "actions": [
    { "action_id": "cargo.clean.target_dir", "applies_to": ["cargo_target_dir"] },
    { "action_id": "node.clean.node_modules", "applies_to": ["node_modules"] },
    { "action_id": "homebrew.cleanup.cache", "applies_to": ["homebrew_cache"] }
  ]
}
```

### `glomeris actions history [--json] [--limit <N>]`

Reads back a bounded, oldest-first tail of `actions.jsonl` (HORO-1057) — the
real-execution audit trail that `execute`, `free`, and `emergency` each
append to, best-effort, after their own outcome is already decided. Unlike
`history.tsv` (which records pressure transitions only), this is the audit
trail of what was actually executed: action id, resource id, the policy
label it was authorized under, outcome, abort reason (when applicable),
actual reclaimed bytes, and which of the three real-execution paths
produced it.

`--limit <N>` is optional and defaults to 20, same bounding/malformed-line-
skip/missing-file-empty contract as `glomeris history`. An audit-write
failure never affects the execution it was trying to record — the write is
best-effort and its result is never surfaced to the caller.

With `--json`, prints an `ActionHistoryReport` (`{"events": [...]}`); each
event has `timestamp`, `action_id`, `resource_id`, `policy_label`,
`outcome`, `abort_reason`, `actual_reclaimed_bytes`, `actual_reclaimed_human`,
and `source` (`"execute"`, `"free"`, or `"emergency"`). Without `--json`,
prints one line per event as plain text.

```sh
glomeris actions history --json --limit 2
```

```json
{
  "events": [
    {
      "timestamp": 1700000000,
      "action_id": "cargo.clean.target_dir",
      "resource_id": "cargo_target_dir:/Users/dev/proj/target",
      "policy_label": "AUTO_SAFE",
      "outcome": "succeeded",
      "abort_reason": null,
      "actual_reclaimed_bytes": 2147483648,
      "actual_reclaimed_human": "2.0 GB",
      "source": "execute"
    },
    {
      "timestamp": 1700000600,
      "action_id": "node.clean.node_modules",
      "resource_id": "node_modules:/Users/dev/proj/node_modules",
      "policy_label": "ASK",
      "outcome": "aborted_by_revalidation",
      "abort_reason": "ResourceIdentityChanged",
      "actual_reclaimed_bytes": null,
      "actual_reclaimed_human": null,
      "source": "free"
    }
  ]
}
```

## `glomeris free --target <N%|NB> [--project-root <path>]...`

macOS only (exits 1 with an error message on other platforms). `--target` is
required; any other argument is rejected (usage printed to stderr, exit 2).

`--project-root <path>` is optional and repeatable, same meaning as
`detect`'s flag above — it feeds the same `DiscoveryContext` the recovery
loop discovers candidates from.

Accepted `--target` value formats:

- A percentage: a number followed by `%`, in the range `0`–`100`
  (e.g. `--target 15%`). Target is "at least this percent of total capacity
  free."
- An absolute byte amount: a number optionally followed by `B`, `KB`, `MB`,
  `GB`, or `TB` (case-insensitive; no suffix means raw bytes). Multipliers
  are binary/1024-based — `--target 5GB` means `5 * 1024^3` bytes free, not
  `5 * 10^9`.

Prints a `RecoveryReport`: stop reason, iterations run, actions executed,
actions declined/skipped, bytes freed, and free space before/after. See
[Safety Model](safety_model.md) and
[Known Limitations](known_limitations.md) for what this loop can and cannot
currently do end to end.

## `glomeris autopilot <show|enable|revoke|run>`

The subcommand is optional and defaults to `show`, so a bare
`glomeris autopilot` reads the grant rather than acting on it. `run` is macOS
only (exits 1 with an error message on other platforms); the other three verbs
work anywhere.

| Verb | What it does | Can it delete? |
|---|---|---|
| `show` | Prints the stored envelope and its path. Does not create the file it reads. | No |
| `enable` | Writes a new envelope from this command line's flags and turns Autopilot on. Requires `--kinds`. | No |
| `revoke` | Turns Autopilot off, keeping the limits. Effective for the next run; nothing to restart. | No |
| `run` | Considers discovered candidates within the envelope. Deletes unless `--dry-run`. Holds the execution lock. | Yes |

The flags, their defaults and their hard ceilings are documented in
[Autopilot](autopilot.md), which is also where the argument for why an LLM
plan file cannot expand authority lives. What this page adds:

- `--max-bytes` takes raw bytes only — unlike `free --target`, there is no
  `GB`/`MB` suffix parsing here, because an envelope is written once and read
  many times and an exact number is easier to audit than a rounded one.
- `--min-pressure` accepts any `PressureState` name case-insensitively
  (`healthy`, `warn`, `pressured`, `critical`, `emergency`) plus the literal
  `none`. An unobservable reading fails any floor you set, rather than passing
  it.
- `--preauthorize-ask` is repeatable and takes `kind:reason` using the same
  tags `glomeris actions list` and `glomeris explain` print.
- `run` prints an `AutopilotReport`: one line per candidate with its kind,
  policy label, model rank and outcome, then the run totals (actions
  attempted, actions succeeded, bytes freed, whether a budget stopped it
  early) and the envelope it ran under. Refusals appear here and nowhere
  else — `glomeris history` is a log of what happened to the filesystem, and
  a refusal did not touch it.

Exit codes: `0` success, including a run that found nothing it was allowed to
do; `1` an action failed, or the envelope file could not be read or written;
`2` usage error, including an unknown resource kind, a limit above its ceiling,
or `enable` without `--kinds`; `3` Autopilot is not enabled, so nothing was
attempted; `75` another invocation holds the execution lock.

`3` exists so that "no grant" is distinguishable from "granted, ran, found
nothing" in a script — both of which are quiet, and only one of which means
the user has something to configure.

## Exit codes

Also available as `glomeris help exit-codes`, which is the copy to trust — it
renders from the same table as the per-command help, so a command's exit codes
cannot drift between its own `--help` and the summary.

- `0` — success.
- `1` — a macOS-only command was run on a non-macOS platform, a
  platform-level operation (e.g. reading the `launchd` plist path) failed,
  or (for `llm-plan` specifically) the LLM provider call/response parsing
  failed, or `--plan-file` named an unreadable path, or (for `llm-check`
  specifically) the check ran and did not pass.
- `2` — usage error: unknown top-level command, unknown `daemon` subcommand,
  missing/unrecognized `free` arguments, or (for `llm-plan`/`llm-check`
  specifically) an unrecognized argument, a missing flag value, missing
  live-mode LLM environment configuration, a base URL that cannot work, or an
  `--api-key`/`--key`/`--token` flag.
- `3` — (`autopilot run` only) Autopilot is not enabled, so nothing was
  attempted.
- `75` — (`free`/`emergency`/`execute`/`autopilot run` only) the HORO-1054
  execution lock is already held by another `glomeris` invocation.

`glomeris execute` has its own, more specific set of exit codes (`0`–`5`
plus `75`) — see its own section above for the full table; a couple of
those codes (`1`, `2`) overlap this list's meanings but are worth reading
in full since `execute` is the one subcommand with real destructive
consequences.

See `glomeris llm-plan`'s and `glomeris llm-check`'s own sections above for
those subcommands' exit codes in full detail — `llm-check`'s `1`/`2` split in
particular carries a meaning this list cannot: whether a report exists.
