# Daemon Lifecycle

`glomeris daemon <install|uninstall|status|run>` manages an optional
background poller via macOS's `launchd`, implemented in
`src/platform/macos/launchd.rs`.

## User-level, not root

Everything here runs at the per-user level: the plist is written to
`~/Library/LaunchAgents/com.glomeris.monitor.plist`, and it is loaded with
plain `launchctl load`/`unload` (`gui/<uid>` session semantics). Nothing
writes to a system-level `LaunchDaemons` location, and no root/sudo access is
required for any of these commands.

## `glomeris daemon install [--force]`

Writes a plist that runs `<program_path> daemon run` every 60 seconds
(`StartInterval`) by default, with `RunAtLoad` set, and stdout/stderr
redirected to `~/Library/Logs/Glomeris/monitor.log`/`.err.log`. Then runs
`launchctl load -w <plist_path>`. If `launchctl` fails, the plist file is
left in place (so `status`/a manual retry can still see it) — only the
final `launchctl` result is treated as a hard failure.

**What this owns, preserves, and refuses to clobber (HORO-1021):** the
plist at `~/Library/LaunchAgents/com.glomeris.monitor.plist` is a wholly
Glomeris-owned artifact — no other tool reads or writes it. Even so,
`install` never silently discards a hand-edited value:

- **Idempotent.** Re-running `install` against an already-up-to-date plist
  (matching a fresh default install) writes nothing.
- **Preserves a hand-edited `StartInterval`.** If you've changed the poll
  interval by hand, re-running `install` keeps your value — only the
  program path is refreshed (the actual point of reinstalling after
  rebuilding/moving the binary).
- **Refuses any other unrecognized customization without `--force`.** If
  the on-disk plist has been changed in a way this tool doesn't manage
  (a hand-added key, a flipped `RunAtLoad`, etc.), `install` makes zero
  changes and exits with status `2`, explaining the refusal. Pass
  `--force` to overwrite anyway — a backup of the current file is taken
  first, at `<plist_path>.plist.bak`.
- **Atomic, verified writes.** Every real write goes to a temp file in the
  same directory, is renamed into place, and is read back and compared
  before `install` reports success — a crash or concurrent `install` mid-write
  can't leave a corrupt/partial plist.
- **Aborts on concurrent modification.** If the plist changes on disk
  between `install`'s read and its write (e.g. two `daemon install`s
  racing), the later write aborts with zero mutation and exit status `2`
  rather than risk clobbering the concurrent change — rerun to retry.

## `glomeris daemon uninstall`

Runs `launchctl unload -w <plist_path>` (best-effort — an already-unloaded
agent reporting an error from `launchctl` is not treated as fatal here),
backs up the current plist to `<plist_path>.plist.bak`, then removes the
plist file if present. Idempotent — uninstalling an already-missing plist
is not an error.

## `glomeris daemon status`

Prints whether the plist file exists, its path, and whether `launchctl list
<label>` currently reports the job loaded. This never errors: an unreachable
`launchctl` (e.g. a non-macOS or sandboxed CI environment) is reported as
`loaded: false`, not surfaced as a failure.

## `glomeris daemon run`

Runs the polling loop in the foreground — this is the exact command the
installed `launchd` agent invokes on its own schedule. It watches `/`,
using `ThresholdConfig::default()` (see [Pressure Model](pressure_model.md)),
the real macOS `statvfs`-backed `FsStat`, and the real `osascript`-backed
notifier, appending confirmed pressure transitions to
`~/Library/Application Support/Glomeris/history.tsv`.

## Known CI limitation

Only plist generation and path/file logic are unit tested in this
repository's CI. Actually asking `launchd` to load/run the agent needs a
real macOS user session and is not exercised by `cargo test`.
