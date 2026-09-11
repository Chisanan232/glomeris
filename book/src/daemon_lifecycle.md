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

## `glomeris daemon install`

Writes a plist that runs `<program_path> daemon run` every 60 seconds
(`StartInterval`), with `RunAtLoad` set, and stdout/stderr redirected to
`/tmp/glomeris-monitor.log`/`.err.log`. Then runs `launchctl load -w
<plist_path>`. If `launchctl` fails, the plist file is left in place (so
`status`/a manual retry can still see it) — only the final `launchctl`
result is treated as a hard failure.

## `glomeris daemon uninstall`

Runs `launchctl unload -w <plist_path>` (best-effort — an already-unloaded
agent reporting an error from `launchctl` is not treated as fatal here), then
removes the plist file if present.

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
