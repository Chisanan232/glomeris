# Troubleshooting

## External tools Glomeris shells out to

| Tool | Used by | Purpose | If absent |
|---|---|---|---|
| `docker` | `detectors::docker` | `docker system df --format '{{json .}}'` to report build/image cache size | `ToolAbsent` — normal, expected, not an error |
| `brew` | `detectors::homebrew` | `brew --cache` to find Homebrew's cache path | `ToolAbsent` — normal, expected |
| `lsof` | `evidence::correlate::open_files`, `::process` | Find processes with a resource open, or with it as their cwd | That correlation field becomes `Unavailable(ToolAbsent)` for the affected resource; other fields are unaffected |
| `git` | `evidence::correlate::git` | Determine repo root, dirty/untracked state, worktree-ness | `git_state` becomes `Unavailable(ToolAbsent)` for the affected resource |
| `pgrep` | `evidence::correlate::tool_liveness` | Check whether Xcode.app or Docker's backend daemon is running | `tool_liveness` becomes `Unavailable(ToolAbsent)` for the affected resource |
| `launchctl` | `platform::macos::launchd` | Load/unload/query the background daemon | `daemon status` reports `loaded: false`; `install`/`uninstall` report an error but the plist file itself is still written/removed |
| `osascript` | `platform::macos::notify` | Post a macOS notification on a confirmed pressure transition | Notification failure is captured as `notify_error` on the poll outcome; the monitor loop keeps running |

A missing tool is treated as a **normal, expected state** everywhere in this
codebase, not an error — every detector's own doc comment says so
explicitly (e.g. "not every detector's tool is installed on every machine").
The one nuance: for Docker specifically, a running-but-unreachable daemon
(e.g. Docker Desktop not started) is *also* folded into `ToolAbsent`, not a
separate `Failed` state.

Cargo, Node, and Xcode detectors never shell out to `cargo`/`node`/`npm`/
`xcodebuild` at all — they only check the filesystem (`known_project_roots`
for a `target`/`node_modules` directory, or
`~/Library/Developer/Xcode/DerivedData`). Their `ToolAbsent` really means
"expected resource not present" (no project roots configured, or the
directory doesn't exist), not "binary missing from PATH."

## "Why does `glomeris detect` show `tool_absent` for something I have installed?"

For Cargo/Node, `tool_absent` also appears if no `known_project_roots` were
configured for the `DiscoveryContext` used — these two detectors never
search the filesystem on their own; they only check specific roots handed to
them. Check how the caller (CLI/daemon) constructed the `DiscoveryContext`.

## "Why did a probe come back `Unavailable(Failed)` instead of `ToolAbsent`?"

`ProbeReason::Failed` means the tool ran but something about its output or
exit status wasn't a recognized "nothing found" or "tool absent" shape — for
example `lsof` exiting non-zero *with* stderr content, or `brew --cache`
succeeding but reporting a path this process can't `canonicalize`. This is
deliberately never coerced into a safe default; treat it the same as "we
don't know," not "nothing to clean up."

## "The menu-bar app says the `glomeris` CLI was not found, but I installed it"

The app lists the locations it searched in the same message. If your binary
is not in one of them, that is the mismatch — move or symlink it into
`/opt/homebrew/bin` or `/usr/local/bin`, or launch the app from a shell
whose `PATH` contains its directory. A GUI app launched from Finder gets
only `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so a `PATH` that works in your
terminal is not visible to the app. See
[Menu Bar App](menu_bar_app.md#how-the-app-finds-the-glomeris-cli) for the
full resolution order.

You do not need to restart the app after installing the CLI: it re-resolves
on every invocation, so the next poll picks it up.

## macOS permissions

Glomeris's own I/O runs as the invoking user; it does not request or use
Full Disk Access, and nothing in this codebase currently prompts for or
checks any macOS privacy permission (TCC). If a probe or detector needs to
read a location gated by such a permission on your system, expect a
`PermissionDenied`-flavored `Failed`/`Unavailable` outcome rather than a
silent empty result — check the affected detector/probe's specific error
message.

## `glomeris free`/`glomeris emergency` doesn't actually delete anything

This is expected today, not a bug you need to work around — see
[Known Limitations](known_limitations.md) for exactly why, and
[Safety Model](safety_model.md) for the revalidation logic responsible.

## Non-macOS platforms

`daemon`, `emergency`, and `free` print an error to stderr and exit `1` on
any OS other than macOS, because `platform::macos` (real `statvfs`,
`osascript`, `launchd`) does not exist in that build at all. `scan` and
`detect` are not gated this way and should work cross-platform, though the
project's CI only exercises `macos-14`.
