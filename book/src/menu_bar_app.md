# Menu Bar App

`GlomerisMenuBar.app` is a native SwiftUI menu-bar app (Epic HORO-1043,
"Glomeris MVP 2.0") that gives the `glomeris` CLI a graphical front end. See
[Installation](installation.md#installing-the-menu-bar-app) for how to
download and open it.

## Thin client, not a second decision-maker

This is the project's core invariant for the app, stated verbatim in the
Swift source (`macos/GlomerisMenuBar/Sources/GlomerisMenuBarApp.swift`):

> GlomerisMenuBar is a THIN CLIENT ONLY, over the existing Rust `glomeris`
> CLI's `--json` output. This Swift codebase renders and lets a human
> approve/decline what the Rust CLI already decided.
>
> NO policy classification (`AUTO_SAFE`/`ASK`/`PROTECTED`), NO evidence
> correlation, NO action planning, and NO filesystem execution logic may
> EVER live here.

In practice this means every screen below is a formatting step over one of
`glomeris`'s own `--json` reports (`status`, `daemon status`, `detect`,
`explain`, `history`, `actions history`, `execute`), spawned as a
subprocess. The app never re-derives "is this safe to clean" from
`policy_label` or `reasons` — it reads the already-computed `executable`,
`offered_actions`, and `refusal_reason` fields the CLI provides for exactly
this purpose (see [CLI Reference](cli_reference.md)). If you trust the CLI's
policy decisions on the command line, you're trusting the exact same
decisions in the menu bar — the app has no separate opinion.

## What the popover shows

Clicking the menu-bar icon opens a popover with three sections, top to
bottom:

### Status & daemon health

Polled on appear and every 10 seconds while the popover is open. Two
independent facts are always shown side by side, deliberately never
collapsed into a single "healthy" indicator:

- Current disk pressure (`glomeris status --json`) — used percent, free
  space, pressure state (`OK`/`WARN`/`CRITICAL`).
- Daemon health (`glomeris daemon status --json`) — whether the `launchd`
  agent is loaded, and how long ago the poll loop last recorded a
  heartbeat (`"Last heartbeat: 42s ago"`, or `"no heartbeat recorded"`).

A `launchd`-loaded-but-wedged daemon and an actually-polling one stay
visibly distinguishable — the app never merges `loaded` and
`heartbeat_age_secs` into one boolean.

### Candidates

Shows the most recent `glomeris detect --json` scan ("Last scanned:
&lt;time&gt;") plus a **Refresh** button. Nothing is scanned automatically:
there is no appear-triggered scan, no timer, and no background polling
loop for candidates — `detect` runs only when you explicitly tap Refresh,
streaming live per-detector progress via `--progress-json` while it works
(button label switches to "Scanning…"). Before the first refresh, the
section shows "No scan yet — tap Refresh to scan."

Each row shows the candidate's kind and reclaimable size. Tapping a row
opens its detail view — there is no inline "Clean" button in this list.

### Candidate detail

Opened by tapping a candidate row; this is the sole place a "Clean" button
exists in the whole app. It calls `glomeris explain --json` for the exact
resource, which supplies `fingerprint_token` — the same token
`execute --observed-fingerprint` later pins consent to. This is also why
there's no per-row Clean button in the candidates list above: cleaning a
resource always goes through one `explain` call that captures the
fingerprint the confirmation flow needs.

- The **Clean** button's enabled state reads `executable` from the
  `explain` report directly — never a re-derived guess from
  `policy_label`.
- If the resolved action `requires_confirmation` (an `ASK` or
  `UNKNOWN_INCOMPLETE` candidate), tapping Clean shows a confirmation
  alert before doing anything.
- Confirming runs `glomeris execute --action-id <id> --resource-id <id>
  --confirm-ask --observed-fingerprint <token> --progress-json`, using
  the exact fingerprint token captured from the `explain` call above —
  never a fingerprint freshly re-observed at click time, which would
  defeat the whole point of fingerprint-pinning.
- The outcome (`succeeded`, `failed`, `aborted_by_revalidation`, or a
  specific refusal reason like `protected`/`ask_consent_mismatch`) is
  rendered from the real `ExecuteReport`/`ExecuteRefusalReport` JSON the
  CLI printed — one specific message per outcome, not a generic
  success/failure toast.

### Recent history & action audit

Two independent, already-computed lists, each polled the same way as the
status section:

- **Recent History** — the last N pressure transitions from `glomeris
  history --json` (e.g. `WARN -> CRITICAL`, with the used-percent/free-space
  reading at the time).
- **Action Audit** — the last N real-execution records from `glomeris
  actions history --json`: which action ran against which resource, its
  policy label, outcome, and which of `execute`/`free`/`emergency`
  produced it. The `AUTO_SAFE`-vs-refused/aborted visual distinction reads
  `outcome`/`abort_reason` directly, never the `policy_label` text.

Neither list accepts a `--project-root` flag — both read global
daemon-state files (`history.tsv`/`actions.jsonl`), not project-scoped
detection state.

## Preferences — Project Roots

The app menu's **Settings…** (`Cmd+,`) opens a simple list with add/remove
controls for the project-roots preference, backed by a small local store.
These are the same paths you'd otherwise pass repeatedly as `--project-root
<path>` on the command line — the CLI's cargo/node detectors only look
under directories they're told about. This view is pure presentation: it
edits the stored list and does not itself call `detect`/`explain`/
`execute`; the roots are appended as `--project-root` arguments the next
time another section (Status, Candidates, …) spawns the CLI.

## Every screen maps back to a CLI command

| Section | CLI command(s) |
|---|---|
| Status & daemon health | `glomeris status --json`, `glomeris daemon status --json` |
| Candidates + Refresh | `glomeris detect --json --progress-json` |
| Candidate detail | `glomeris explain <resource_id> --json --progress-json` |
| Clean (with confirmation) | `glomeris execute --action-id <id> --resource-id <id> [--confirm-ask --observed-fingerprint <token>] --json --progress-json` |
| Recent History | `glomeris history --json` |
| Action Audit | `glomeris actions history --json` |

See [CLI Reference](cli_reference.md) for the full flag/exit-code/JSON-shape
reference behind every one of these.
