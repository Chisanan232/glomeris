# CLI Reference

This page documents every subcommand and flag present in `src/main.rs` as of
this writing. The CLI surface is still evolving — treat this page as a
snapshot, and check `main.rs`'s `match` arms directly if something here looks
stale.

## `glomeris`

No arguments: prints `glomeris <version>` (from `CARGO_PKG_VERSION`) and
exits.

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

## `glomeris llm-plan [--project-root <path>]... [--plan-file <path>] [--json]`

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

## `glomeris emergency`

macOS only (exits 1 with an error message on other platforms). Takes no
arguments. See [Emergency Mode](emergency_mode.md).

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

See `glomeris llm-plan`'s own section above for that subcommand's exit
codes in full detail.
