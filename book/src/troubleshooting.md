# Troubleshooting

## External tools Glomeris shells out to

| Tool | Used by | Purpose | If absent |
|---|---|---|---|
| `docker` | `detectors::docker` | `docker system df --format '{{json .}}'` to report build/image cache size | `ToolAbsent` — normal, expected, not an error; installed but stopped is `ToolNotRunning` instead |
| `brew` | `detectors::homebrew` | `brew --cache` to find Homebrew's cache path | `ToolAbsent` — normal, expected |
| `lsof` | `evidence::correlate::open_files`, `::process` | Find processes with a resource open, or with it as their cwd | That correlation field becomes `Unavailable(ToolAbsent)` for the affected resource; other fields are unaffected |
| `git` | `evidence::correlate::git` | Determine repo root, dirty/untracked state, worktree-ness | `git_state` becomes `Unavailable(ToolAbsent)` for the affected resource |
| `pgrep` | `evidence::correlate::tool_liveness` | Check whether Xcode.app or Docker's backend daemon is running | `tool_liveness` becomes `Unavailable(ToolAbsent)` for the affected resource |
| `launchctl` | `platform::macos::launchd` | Load/unload/query the background daemon | `daemon status` reports `loaded: false`; `install`/`uninstall` report an error but the plist file itself is still written/removed |
| `osascript` | `platform::macos::notify` | Post a macOS notification on a confirmed pressure transition | Notification failure is captured as `notify_error` on the poll outcome; the monitor loop keeps running |

A missing tool is treated as a **normal, expected state** everywhere in this
codebase, not an error — every detector's own doc comment says so
explicitly (e.g. "not every detector's tool is installed on every machine").
The one nuance: for Docker specifically, an installed-but-unreachable daemon
(e.g. Docker Desktop not started) is its own status, `ToolNotRunning`
(HORO-1544), not `ToolAbsent` and not `Failed` — "Docker is not installed" and
"Docker is installed and stopped" are different facts, and the second one is
the one you can act on.

Cargo, Node, SwiftPM and Xcode detectors never shell out to
`cargo`/`node`/`npm`/`swift`/`xcodebuild` at all — they only check the
filesystem (`known_project_roots` for a `target`/`node_modules`/`.build`
directory, or `~/Library/Developer/Xcode/DerivedData`). The first three
therefore cannot report `ToolAbsent` at all (HORO-1576): a detector that never
asks whether a tool is installed has no grounds to say it is not. With no
project roots configured they report `NotConfigured`; with roots configured and
nothing found there, `Found` with no evidence. Xcode's `ToolAbsent` still means
"the DerivedData directory does not exist", not "binary missing from PATH".

## "Why does `glomeris detect` show `not_configured` for Cargo/Node/SwiftPM?"

Because no project root was handed to the `DiscoveryContext` (HORO-1576).
These three detectors never search the filesystem on their own; they only look
under roots they are given. On the CLI, pass `--project-root <path>` once per
project; in the menu-bar app, add folders under Settings › Projects. Until then
`candidates` is silent about your projects rather than claiming there is nothing
under them, and `discovery_complete` is `false` to say so.

Before HORO-1576 this state was reported as `tool_absent`, which is why older
output appeared to claim cargo or node was not installed on a machine where it
was.

## "Why did a probe come back `Unavailable(Failed)` instead of `ToolAbsent`?"

`ProbeReason::Failed` means the tool ran but something about its output or
exit status wasn't a recognized "nothing found" or "tool absent" shape — for
example `lsof` exiting non-zero *with* stderr content, or `brew --cache`
succeeding but reporting a path this process can't `canonicalize`. This is
deliberately never coerced into a safe default; treat it the same as "we
don't know," not "nothing to clean up."

## "Part of the scan did not look — where do I see which detector?"

A detector that failed, one whose tool is absent and one that was handed
nothing to examine all contribute zero candidates, so a count alone cannot
tell them apart while they mean different things: "we don't know what is
there", "there is nothing there", and "nobody looked". Every surface therefore
reports the outcome and not just the count.

| Surface | Where a gap appears |
|---|---|
| `glomeris detect` | With no candidates at all, the line reads `no candidates discovered by the detectors that looked — N failed and M had nothing configured to look at, so this is not a clean bill of health` rather than the bare `no candidates discovered`. Each clause is printed only when it has a count, so the sentence never says "0 failed" |
| `glomeris detect --json` | A `detectors` array — one entry per detector, in registration order, with `status` (`found`/`tool_absent`/`tool_not_running`/`not_configured`/`failed`), `candidates_found`, and a `reason` present only on `failed` — plus a derived `discovery_complete` |
| `glomeris detect --progress-json` | Each `detector_finished` event carries `outcome`, and `reason` when it failed |
| `glomeris free`, `glomeris emergency` | A `discovery incomplete: N detector(s) failed` block naming each one, and a separate `not examined: M detector(s) had nothing configured to look at` block naming those; if the run stopped at `SafeExhausted`, it also says in so many words that this is not a finding that nothing safe is left |
| Menu-bar app | "Nothing found where Glomeris could look" in place of the all-clear, or "This list may be incomplete" below a non-empty list, when something failed. When the only gap is configuration it says "Nothing found where Glomeris was told to look" / "This list does not cover your projects" instead, and points at Settings › Projects — either way naming the checks concerned |

The two gaps are always reported as two, never summed into one "N incomplete"
count (HORO-1576). On a default installation the three project-scoped detectors
are `not_configured` and nothing has failed, so a combined count would tell
every new user that three checks broke and send them looking for a defect in
Glomeris instead of for a setting.

An absent tool appears as `tool_absent`, a stopped daemon as
`tool_not_running`, and neither makes `discovery_complete` false. Those are
real answers, not missing ones, and flagging them would make the caveat
permanent on any machine without Docker — which is the fastest way to teach
people to ignore it. `not_configured` and `failed` do make it false, because in
both cases nothing was observed.

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
