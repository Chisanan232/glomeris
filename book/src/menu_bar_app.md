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

## How the popover reads

Every section is a titled card in a scrolling column, ordered by the
questions you open the panel with: what the disk is doing now, what could
be reclaimed, what has already happened. Three presentation rules hold
across all of them:

- **Plain language first, the CLI's own token second.** `PRESSURED` reads
  as "Running low", `aborted_by_revalidation` as "Stopped safely" — and
  the raw token stays visible beside or beneath it, because this is an
  evidence-first product and you have to be able to match what the panel
  says against `--json` and against the docs.
- **Three separate axes, never merged.** Storage impact (how much space),
  safety (what the policy allows) and evidence quality (how sure the CLI
  is) are shown as distinct badges. A large `AUTO_SAFE` candidate is an
  opportunity, not a hazard; a small `PROTECTED` one is still protected.
  Size is therefore drawn in a neutral tone at every magnitude.
- **Colour is never the only signal.** Every badge carries a symbol and a
  word as well as a tint, so the state survives greyscale, a
  colour-vision deficiency and a tinted wallpaper. A refusal
  (`PROTECTED`) is deliberately not painted like a failure: it is the
  policy working, and there is nothing for you to fix.

### Disk space, and Background monitor

Two cards, polled on appear and every 10 seconds while the popover is
open. The two facts are deliberately never collapsed into a single
"healthy" indicator:

- Current disk pressure (`glomeris status --json`) — used percent, free
  space, and pressure state (one of `HEALTHY`, `WARN`, `PRESSURED`,
  `CRITICAL`, `EMERGENCY`).
- Daemon health (`glomeris daemon status --json`) — whether the `launchd`
  agent is loaded, and how long ago the poll loop last recorded a
  heartbeat (`"Last heartbeat: 42s ago"`, or `"no heartbeat recorded"`).

A `launchd`-loaded-but-wedged daemon and an actually-polling one stay
visibly distinguishable — the app never merges `loaded` and
`heartbeat_age_secs` into one boolean.

### Reclaimable space

Shows the most recent `glomeris detect --json` scan ("Last scanned:
&lt;time&gt;") plus a **Refresh** button. Nothing is scanned automatically:
there is no appear-triggered scan, no timer, and no background polling
loop for candidates — `detect` runs only when you explicitly tap Refresh,
streaming live per-detector progress via `--progress-json` while it works
(button label switches to "Scanning…").

Four states that are easy to conflate are kept distinct, because each is
a different claim about your disk:

| State | What it says |
|---|---|
| "No scan yet" | Nothing has been looked at. **Not** a clean bill of health. |
| "Nothing worth reclaiming" | Scanned, and there is genuinely nothing — good news. |
| "No candidates match this filter" | Things were found; you are just not looking at them. |
| A scan failure | Says what failed. An empty list is never shown in its place. |

Each row shows the candidate's kind, what cleaning it would free, and its
safety verdict in words. Tapping a row opens its detail view — there is no
inline "Clean" button in this list.

Rows arrive in the CLI's own order — biggest reclaimable size first — and
are shown exactly as `detect` returned them. The app does no ranking of its
own: deciding what matters most is a judgment, and it belongs next to the
evidence in Rust rather than in a thin client that would then drift from it.

The **View options** menu (the funnel next to Refresh) offers two things:

- **Order** — "Biggest first", which is the CLI's order untouched, or
  "Path (A–Z)", an alphabetical index for finding a resource whose path you
  already know. There is deliberately no third "largest first" option: that
  *is* "Biggest first".
- **Show** — All, Safe to reclaim, Asks first, Protected, or Not enough
  evidence. This hides rows and does nothing else; it cannot enable,
  authorise or perform anything, and the list has no action affordance for it
  to unlock. Protected items are a first-class filter value rather than
  something hidden by default: "what on this machine is off-limits, and why"
  is a reasonable question, and quietly omitting them would teach you that
  protection means invisibility.

When a filter is active the header reads "N of M items" so a shortened list
can never be mistaken for a smaller problem.

#### Size is not safety

A third badge appears on the rows worth pausing on — "Biggest wins" or
"Worth a look" — using the `impact_tier` the CLI already computed. It is an
emphasis hint about magnitude only, carries no safety colour, and appears on
no more rows than deserve it (ordinary and unmeasured candidates get no badge
rather than a badge saying "normal", which would be noise).

Storage impact, safety, and evidence confidence are three separate axes and
the popover never collapses them. A large candidate may be protected; a small
one may be perfectly safe to reclaim. Nothing is distinguished by colour
alone — every badge pairs its colour with a distinct symbol and words — and
VoiceOver reads each row as size, then safety, then emphasis, so the badge is
never the only route to the fact.

### Candidate detail

Opened by tapping a candidate row; this is the sole place a "Clean" button
exists in the whole app. It calls `glomeris explain --json` for the exact
resource, which supplies `fingerprint_token` — the same token
`execute --observed-fingerprint` later pins consent to. This is also why
there's no per-row Clean button in the candidates list above: cleaning a
resource always goes through one `explain` call that captures the
fingerprint the confirmation flow needs.

The sheet is ordered by the questions you have when you open it: may I
clean this, is it worth cleaning, on what evidence — and last, collapsed
behind a disclosure, the literal values `explain --json` returned, which
stay selectable so they can be quoted in a bug report.

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

### Disk space history, and What Glomeris has done

Two independent, already-computed lists in two cards, each polled the same
way as the status section:

- **Disk space history** — the last N pressure transitions from `glomeris
  history --json` (e.g. `WARN -> CRITICAL`, with the used-percent/free-space
  reading at the time). The two ends of each transition are named exactly
  as the status card names them, because they are the same enum.
- **What Glomeris has done** — the last N real-execution records from
  `glomeris actions history --json`: which action ran against which
  resource, its policy label, outcome, and which of
  `execute`/`free`/`emergency` produced it. "Triggered by" is spelled out
  in words, because whether something was cleaned that you did not ask for
  is the question this card exists to answer. The
  `AUTO_SAFE`-vs-refused/aborted visual distinction reads
  `outcome`/`abort_reason` directly, never the `policy_label` text — and
  `aborted_by_revalidation` reads as "Stopped safely" rather than as a
  failure, because the resource changed between checking and acting and so
  nothing was touched.

An empty list in either card says only that it is empty. Neither claims an
all-clear it cannot support: an empty pressure history could mean the disk
has been steady, or it could mean nothing has been watching it, and the
report does not distinguish those.

Neither list accepts a `--project-root` flag — both read global
daemon-state files (`history.tsv`/`actions.jsonl`), not project-scoped
detection state.

## Preferences — Project Roots

The popover's **Project roots…** footer button — or `Cmd+,` — opens a
simple list with add/remove controls for the project-roots preference,
backed by a small local store. (The footer exists because the menu-bar item
opens a window rather than a menu, so the popover is the app's only
surface; **Quit** is there for the same reason.)
These are the same paths you'd otherwise pass repeatedly as `--project-root
<path>` on the command line — the CLI's cargo/node detectors only look
under directories they're told about. This view is pure presentation: it
edits the stored list and does not itself call `detect`/`explain`/
`execute`; the roots are appended as `--project-root` arguments the next
time another card (Disk space, Reclaimable space, …) spawns the CLI.

## How the app finds the `glomeris` CLI

Every screen spawns the CLI, so the app has to decide which binary that is.
It checks these locations in order, and uses the first one that exists and
is executable:

1. **Inside the app bundle**, at `Contents/MacOS/glomeris`. Release builds
   ship the CLI here, and it wins because it is version-matched to the app
   and covered by the bundle's signature. Debug builds contain no embedded
   CLI, so development is unaffected.
2. **Each absolute directory in `PATH`**, in order. Relative entries —
   including the empty entry that shells read as "the current directory" —
   are ignored, so nothing can substitute a binary by writing a file named
   `glomeris` next to the running process.
3. **`/opt/homebrew/bin`, then `/usr/local/bin`** — the Homebrew prefix on
   Apple Silicon and on Intel respectively, and where
   [Installation](installation.md) tells you to put a binary from a release
   tarball or a source build.

Step 3 is not redundant with step 2: a GUI app does not inherit your
shell's `PATH`. An app launched from Finder gets
`PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which contains neither Homebrew
prefix — so `PATH` alone would not find a `brew install`ed CLI, even though
running `glomeris` in a terminal works fine.

Resolution happens on every invocation, not once at launch, so installing
the CLI while the app is already running takes effect at the next poll
without a restart.

If no binary is found, each section says so and lists the locations it
searched, rather than failing silently or naming a path it only assumed.

## Every screen maps back to a CLI command

| Card | CLI command(s) |
|---|---|
| Disk space | `glomeris status --json` |
| Background monitor | `glomeris daemon status --json` |
| Reclaimable space + Refresh | `glomeris detect --json --progress-json` |
| Candidate detail | `glomeris explain <resource_id> --json --progress-json` |
| Clean (with confirmation) | `glomeris execute --action-id <id> --resource-id <id> [--confirm-ask --observed-fingerprint <token>] --json --progress-json` |
| Disk space history | `glomeris history --json` |
| What Glomeris has done | `glomeris actions history --json` |

See [CLI Reference](cli_reference.md) for the full flag/exit-code/JSON-shape
reference behind every one of these.
