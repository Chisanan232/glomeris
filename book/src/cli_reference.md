# CLI Reference

This page documents every subcommand and flag present in `src/main.rs` as of
this writing. The CLI surface is still evolving — treat this page as a
snapshot, and check `main.rs`'s `match` arms directly if something here looks
stale.

## `glomeris`

No arguments: prints `glomeris <version>` (from `CARGO_PKG_VERSION`) and
exits.

## `glomeris --help` / `-h` / `help`, and unrecognized-command usage

`--help`/`-h`/`help` and the usage line printed on an unrecognized top-level
command (exit 2) both render from the same `COMMANDS` table in `src/main.rs`
(HORO-1050) — there is exactly one place to add or edit a subcommand's usage
line, so the two surfaces cannot independently drift out of sync the way they
once did (HORO-1034: the unrecognized-command usage omitted `llm-plan`).

Every top-level subcommand also supports its own `glomeris <subcommand>
--help` / `-h`, looked up in that same table, which prints just that
subcommand's one-line usage and a short description instead of being
rejected as an unrecognized argument.

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

## `glomeris scan [path] [top_k]`

Not macOS-gated. Both arguments are positional and optional:

- `path` — root directory to scan. Defaults to `.`.
- `top_k` — number of largest entries to report. Defaults to `20` if
  omitted or unparseable as `usize`.

Prints a summary line (files visited, stop reason, incomplete-entry count)
followed by one line per candidate: size, depth, path.

## `glomeris detect [--project-root <path>]...`

Not macOS-gated. Runs `DetectorRegistry::builtin()`'s `discover_all` once and
prints, per detector: `found (<N> evidence)`, `tool_absent`, or
`failed: <reason>`.

`--project-root <path>` is optional and repeatable — pass it once per
project directory you want the cargo/node detectors to check for a
`target/`/`node_modules/` dir. Without it, those two detectors have no
project roots to scan and always report `tool_absent`.

## `glomeris llm-plan [--project-root <path>]... [--plan-file <path>] [--json] [--schema]`

Not macOS-gated. ADVISORY, NON-EXECUTING (HORO-1008) — never constructs a
`policy::Approval` and never calls `policy::approval::authorize` or
`executor::execute`. See [BYOK LLM Planner](byok.md) for the full
configuration and safety-property writeup.

- Without `--plan-file`, credentials are read only from
  `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL` (all
  three required, no default base URL) — never from a CLI flag.
- `--plan-file <path>` reads the file's raw bytes as if they were the
  model's raw response text, through the same validation pipeline the live
  provider uses — no network call, no API key required.
- `--project-root <path>` is optional and repeatable, same meaning as
  `detect`'s flag above.
- `--api-key`/`--key`/`--token` are explicitly rejected (not accepted and
  ignored) — the error names `$GLOMERIS_LLM_API_KEY` instead.
- `--json` prints the report as JSON.
- `--schema` (HORO-1048) prints an example, syntactically valid `LlmPlan`
  JSON document to stdout and exits — a distinct, self-contained mode
  that never runs discovery, never reads `--plan-file`, and never checks
  live-mode credentials, regardless of what else is passed alongside it.
  See [BYOK LLM Planner](byok.md#llmplan-schema--the-concrete---plan-file-example-horo-1048)
  for the full example and field table.

Human-readable output always opens with `LLM SUGGESTION — advisory only,
nothing is executed by this command`.

Exit codes for this subcommand specifically:

- `0` — success, including zero suggestions or every suggestion being
  `PROTECTED`.
- `1` — the provider call or response parsing failed, or `--plan-file`
  named an unreadable path.
- `2` — usage error: an unrecognized argument, `--plan-file` with no value,
  missing live-mode environment configuration, or an `--api-key`/`--key`/
  `--token` flag.

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
arguments. See [Emergency Mode](emergency_mode.md).

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

## Exit codes

- `0` — success.
- `1` — a macOS-only command was run on a non-macOS platform, a
  platform-level operation (e.g. reading the `launchd` plist path) failed,
  or (for `llm-plan` specifically) the LLM provider call/response parsing
  failed, or `--plan-file` named an unreadable path.
- `2` — usage error: unknown top-level command, unknown `daemon` subcommand,
  missing/unrecognized `free` arguments, or (for `llm-plan` specifically)
  an unrecognized argument, a missing flag value, missing live-mode LLM
  environment configuration, or an `--api-key`/`--key`/`--token` flag.
- `75` — (`free`/`emergency`/`execute` only) the HORO-1054 execution lock
  is already held by another `glomeris` invocation.

`glomeris execute` has its own, more specific set of exit codes (`0`–`5`
plus `75`) — see its own section above for the full table; a couple of
those codes (`1`, `2`) overlap this list's meanings but are worth reading
in full since `execute` is the one subcommand with real destructive
consequences.

See `glomeris llm-plan`'s own section above for that subcommand's exit
codes in full detail.
