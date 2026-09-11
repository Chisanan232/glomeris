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

## `glomeris detect`

Not macOS-gated. Runs `DetectorRegistry::builtin()`'s `discover_all` once and
prints, per detector: `found (<N> evidence)`, `tool_absent`, or
`failed: <reason>`.

## `glomeris emergency`

macOS only (exits 1 with an error message on other platforms). Takes no
arguments. See [Emergency Mode](emergency_mode.md).

## `glomeris free --target <N%|NB>`

macOS only (exits 1 with an error message on other platforms). `--target` is
required; any other argument is rejected (usage printed to stderr, exit 2).

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
- `1` — a macOS-only command was run on a non-macOS platform, or a
  platform-level operation (e.g. reading the `launchd` plist path) failed.
- `2` — usage error: unknown top-level command, unknown `daemon` subcommand,
  missing/unrecognized `free` arguments.
