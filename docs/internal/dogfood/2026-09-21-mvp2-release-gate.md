# Glomeris MVP 2.0 Release Gate — Dogfood and Readiness Record

> **Internal research record — not public product documentation.**
> Not part of the mdBook site (`book/src/`), not linked from `SUMMARY.md`,
> not referenced from `README.md`. Lives under `docs/internal/` specifically
> so the `docs.yml` GitHub Pages pipeline (which only builds `book/` via
> mdBook and `cargo doc` from Rust source comments) never touches it.

This is the HORO-1313 release gate for the MVP 2.0 iteration (HORO-1305
through HORO-1312). It is written criterion by criterion against that
ticket's ten acceptance criteria, and it deliberately separates what was
*verified on this machine* from what *still needs the founder*. The verdict
section at the end does not round anything up.

## Metadata

- **Date:** 2026-09-21
- **Released version/tag tested:** none. This gate ran against `main` at
  `1663bf5` (`Merge pull request #76 … HORO-1321`), which is ahead of the
  last tag (`v0.2.0`). There is no MVP 2.0 tag yet — cutting one is what this
  gate is deciding about.
- **Exact artifact tested:** `GlomerisMenuBar.app` built from `main` at
  `1663bf5` with the release CLI embedded at
  `Contents/MacOS/glomeris`, ad-hoc re-signed
  (`codesign --force --deep --sign -`), staged at a scratch path outside any
  git working tree. `Identifier=dev.glomeris.GlomerisMenuBar`,
  `Signature=adhoc`, `TeamIdentifier=not set`. The embedded CLI reports
  `glomeris 0.2.0`. This is deliberately the same shape
  `macos-app-release.yml` produces in CI — bundle plus embedded CLI, ad-hoc
  signed — rather than a dev `target/release` binary run by hand.
- **Host/platform:** macOS 15.7.7 (build 24G720), Apple M1 Pro, arm64.
  Disk at gate time: `WARN`, 78.0% used, 101.1 GB free of 460.4 GB.
- **Surfaces exercised:** `glomeris --help`; `--help` for `status`, `scan`,
  `detect`, `explain`, `clean`, `llm-plan`, `llm-check`, `history`,
  `actions`; `glomeris help exit-codes`; `glomeris status`; `glomeris
  llm-plan --project-root <fixture> --json` against a loopback HTTP listener;
  `glomeris execute` and `glomeris autopilot run` against disposable `/tmp`
  fixtures (earlier in this pass); the menu-bar item and popover via the
  accessibility API.
- **Real LLM API call count:** 0 to any external provider. One
  OpenAI-compatible request was issued to `http://127.0.0.1:19731`, a local
  capture listener, with a self-invented placeholder key. No real credential
  was used and nothing left this machine.
- **Model name:** `fake-capture-model` (the placeholder sent to the loopback
  listener).
- **Approximate API cost:** $0.00 — no external provider was contacted.

## Artifact under test, and why it had to be rebuilt

The first thing this gate found was about itself rather than the product.

The dogfood instance that had been running since 2026-09-20 (from an older
scratch bundle) has **no embedded CLI** — its `Contents/MacOS/` contains only
`GlomerisMenuBar`. `GlomerisExecutableLocator` therefore falls through to
`PATH` and then to `knownInstallDirectories`, and both on-`PATH` copies on
this machine (`/opt/homebrew/bin/glomeris`, mtime 2026-09-20 17:45, and
`/usr/local/bin/glomeris`, mtime 2026-09-19 20:05) predate HORO-1311 — both
answer `glomeris: unknown command '--version'`. That instance was three
stories behind `main` and would have produced observations about code that is
no longer what ships.

So the rig was rebuilt at a new path rather than repaired in place: nothing
was deleted, the old bundle is still on disk untouched, and neither on-`PATH`
binary was overwritten (`/usr/local/bin` is root-owned and was left alone).

Resolution was then confirmed empirically rather than assumed from the
locator's source order. Sampling `ps` for spawned children missed them — the
CLI invocations the popover makes are too short-lived for a `ps` loop to
catch reliably — so the check used access times instead. After opening the
popover through the accessibility API, the **bundled** CLI's atime had moved
to 11:27:40 (its mtime is 11:21:05, so it was executed after being staged),
while both on-`PATH` binaries still showed atime 11:17:08, the moment they
were last interrogated by hand. The app runs its own bundled, version-matched
CLI and did not touch the stale ones.

Popover reachability was also confirmed through the accessibility API:
`menu bar 2` holds exactly one item, `description = "status menu"`,
`enabled = true`, 36×24; clicking it brings up one `AXWindow` with subrole
`AXSystemDialog`. All four sections (`StatusHealthSectionView`,
`CandidatesSectionView`, `AiPlanSectionView`, `HistoryAuditSectionView`) are
rendered eagerly inside the popover's `ScrollView`, so their `.task` blocks
fire on open.

## Acceptance criteria

### AC1 — all implementation stories merged with CI green: **PASS**

Ten PRs for this iteration are merged into `main`:

| PR | Branch |
|---|---|
| #67 | `v0.0.1/HORO-1305/feat/visual_identity` |
| #68 | `v0.0.1/HORO-1305/docs/menubar_width_claim` |
| #69 | `v0.0.1/HORO-1306/feat/gui_information_arch` |
| #70 | `v0.0.1/HORO-1307/feat/candidate_priority` |
| #71 | `v0.0.1/HORO-1308/feat/gui_ai_plan` |
| #72 | `v0.0.1/HORO-1309/feat/ai_provider_settings` |
| #73 | `v0.0.1/HORO-1311/feat/cli_help_redesign` |
| #74 | `v0.0.1/HORO-1310/feat/autopilot_envelope` |
| #75 | `v0.0.1/HORO-1312/docs/reconcile_terminology` |
| #76 | `v0.0.1/HORO-1321/docs/correct_install_claims` |

On `main` at `1663bf5` both workflows are `completed/success`, and every job
in the `CI` run is green: `test`, `deny`, `macos-app`, `no-policy-in-swift`,
`xcodeproj-drift`, `app-icon-drift`, `vocabulary-covers-cli-tokens`,
`docs-cover-cli-commands`, `credential-store-uses-keychain`.

### AC2 — Swift remains a thin client, confirmed mechanically and adversarially: **PASS**

The CI guard is green, and it was also re-run directly against this tree:

```
PASS: no policyLabel/policy_label branching detected under macos/GlomerisMenuBar/Sources.
PASS: macos/GlomerisMenuBar/Sources/GlomerisVocabulary.swift is display-only
      (no SwiftUI/AppKit, no control, no gating field).
```

The adversarial half asked the opposite question — not "does the guard pass"
but "what authority could Swift exercise if it wanted to":

- **Deletion:** zero occurrences of `removeItem`, `trashItem`, `unlink(`, or
  `FileManager.default.remove` anywhere under
  `macos/GlomerisMenuBar/Sources`. The GUI has no code path that can delete
  anything at all; every deletion it appears to offer is the CLI's.
- **Arbitrary execution:** the only process-spawning site is
  `GlomerisClient.swift:386-387`, `Process()` with
  `process.executableURL = try resolveExecutableURL()`. There is no
  `launchPath`, no `/bin/sh`, no `/bin/bash`, no `system(`, no
  `NSAppleScript`. The client is constructed with a *pinned* executable URL
  (`init(executableURL:environment:)`, "always spawns exactly
  `executableURL`, never resolving anything").
- **Policy:** no policy label is branched on, which is what the guard proves;
  combined with the two points above, the GUI cannot classify, cannot
  override, and cannot act outside the CLI.

### AC3 — real founder dogfood recording blocking and non-blocking observations: **INCOMPLETE — requires the founder**

Everything mechanically checkable was checked and is recorded here. The
subjective half of this criterion — whether the menu-bar icon is *findable*,
whether the status wording is *understood*, whether the candidate ranking
*feels* right, whether the visual hierarchy *guides the eye* — is not
something this pass can honestly self-certify. The rig is built, current, and
running for that pass.

### AC4 — at least one AI Plan against disposable fixtures with a real provider: **INCOMPLETE — requires the founder's own key**

An AI Plan *was* produced end to end against a disposable fixture, through
the real provider code path (`OpenAiCompatibleProvider`, real HTTP request,
real response parsing) — but pointed at a loopback listener with a
placeholder key, because a real provider needs a credential that is the
founder's to supply. No credential on this workstation may be repurposed as
an LLM API key, so the live-provider half of this criterion is deliberately
left open rather than satisfied with the wrong key.

What that loopback run *does* establish is the egress contract, which is AC9
below.

### AC5 — one safe manual action and one bounded Autopilot action execute successfully: **PASS**

Both are recorded in the product's own audit log
(`~/Library/Application Support/Glomeris/actions.jsonl`), on disposable
`/tmp` fixtures, not on real machine resources:

| When | Source | Resource | Class | Outcome | Reclaimed |
|---|---|---|---|---|---|
| 2026-09-21 10:33:13 | `execute` | `node_modules:/private/tmp/horo1313/proj-a/node_modules` | `AUTO_SAFE` | `succeeded` | 9,437,184 B |
| 2026-09-21 10:34:55 | `autopilot_auto_safe` | `node_modules:/private/tmp/horo1313/proj-b/node_modules` | `AUTO_SAFE` | `succeeded` | 4,194,304 B |

The Autopilot run was bounded by a real envelope, and the bound was the
binding constraint rather than decoration: `allowed_kinds = node_modules`,
`max_actions = 1`, `max_bytes = 5242880` (5 MiB — the 4 MiB fixture fits, a
larger one would not have), `max_duration_secs = 60`, `min_pressure = none`.
The envelope was revoked afterwards and is still revoked: `autopilot.conf`
reads `enabled = false` today.

### AC6 — one PROTECTED/UNKNOWN case proves fail-closed behaviour: **PASS**

Two independent fail-closed refusals were observed on this machine.

**A live detector reaching `PROTECTED`.** A real `node_modules` tree created
under a path containing an `.ssh` component was discovered by the live Node
detector, classified `Protected` with reason
`protected_credential_material` and `executable: false`, and refused by the
real executor twice — once by a plain `execute`, once with a valid
`--confirm-ask` and a matching `--observed-fingerprint`. Exit 3 both times,
nothing deleted. This disproved a standing claim in
`book/src/known_limitations.md` that no live detector could reach a protected
path; that page has since been corrected (`ba30c9b`, merged in PR #76), and
the safety conclusion came out *stronger*, not weaker: a protected
pattern matches a path *component*, so anything discoverable beneath one is
protected.

**An unscoped `RunTool` step refused on the real machine.** Four `emergency`
invocations earlier in this pass each reached
`homebrew.cleanup.cache` on the real Homebrew cache, classified `AUTO_SAFE`,
and each logged `"outcome":"failed"` with
`"actual_reclaimed_bytes":null` — HORO-957's unconditional refusal of a
`RunTool` step with `scoped_path: None`, holding on a real machine, against a
real `AUTO_SAFE` classification, four times in a row. Nothing was cleaned.
See Finding 1 for why those four runs happened at all; the refusal itself is
exactly the behaviour this criterion asks for.

### AC7 — VoiceOver/keyboard/accessibility and light/dark exercised: **PARTIAL — mechanical half done, subjective half requires the founder**

Mechanically:

- **Accessibility coverage is mostly centralised, and per-file counts are a
  misleading way to measure it.** Counting `accessibility*` modifiers per file
  shows `HistoryAuditSectionView` 7, `AiProviderPreferencesView` 6,
  `CandidatesSectionView` 6, `AiPlanSectionView` 5, `GlomerisPopoverView` 4,
  `MenuBarAppearance` 4, `StatusHealthSectionView` 3, `GlomerisMenuBarApp` 1,
  and zero in `CandidateDetailView.swift` and
  `ProjectRootsPreferencesView.swift` — but that count is not the story. The
  labels live in `GlomerisDesignSystem`, which is where they belong:
  `GlomerisBadgeView` is one VoiceOver element with
  `.accessibilityElement(children: .ignore)`, `.accessibilityLabel(term
  .accessibilityLabel)` and `.help(term.explanation)`; `GlomerisPathText`
  carries `.accessibilityLabel("Path: \(path)")` plus a `.help(path)` tooltip
  so a middle-truncated path is still read in full; card titles get
  `.accessibilityAddTraits(.isHeader)`. `CandidateDetailView` composes
  exclusively from those shared components (`GlomerisCard`,
  `GlomerisDetailRow`, `GlomerisBadgeView`, `GlomerisPathText`,
  `GlomerisStateMessageView`), so its zero is inheritance, not absence.
- **Two concrete gaps remain, and they are both on controls.**
  `CandidateDetailView`'s `Button("Clean")` takes its VoiceOver label from its
  title, so it is announced — but it has no `.accessibilityHint` saying what
  will be deleted, and it stacks two independent `.disabled` modifiers
  (`!viewModel.isCleanEnabled`, then `isExecuting`) with nothing that conveys
  *which* reason is in force, so a screen-reader user hears an unavailable
  button and cannot find out why. In
  `ProjectRootsPreferencesView`, the per-row remove control is an icon-only
  `Button` whose label is `Image(systemName: "minus.circle")` with no
  `.accessibilityLabel` — it announces as its symbol rather than as "remove
  this project root" — and the row's path is a bare `Text(root)` with
  `.truncationMode(.middle)` instead of `GlomerisPathText`, so it is the one
  place a truncated path is read as truncated. Filed as HORO-1323 (see
  Finding 3).
- **Light/dark is structurally sound.** There are no hardcoded
  `Color(red:…)`, `Color(.sRGB…)` or `Color(white:)` literals anywhere in
  the Swift sources, so nothing is pinned to one appearance.
- **Colour is never load-bearing.** `GlomerisDesignSystem` renders every
  vocabulary chip as symbol **and** word, with the tint as the third channel
  — "The symbol and the word are both always present. The tint is the third
  signal", and "a distinct symbol per state precisely so the tint is never
  load-bearing". This satisfies the standing "do not rely on colour alone"
  rule by construction rather than by review.

The subjective half — an actual VoiceOver pass, actual keyboard-only
navigation, and an eyes-on light/dark comparison — needs the founder.

### AC8 — CLI help validated from a clean install: **PASS, with one contradiction found**

Validated against the *shipping artifact* (the CLI embedded in the freshly
built bundle), not a dev binary. `glomeris --help` renders 45 lines,
exit 0, and groups all fourteen commands by intent — INSPECT / PLAN / ACT /
OBSERVE / SERVICE — with a "Start here" block naming four concrete first
commands. Every non-destructive subcommand renders its own help at exit 0:
`status` (18 lines), `scan` (25), `detect` (29), `explain` (31), `clean`
(25), `llm-plan` (45), `llm-check` (28), `history` (20), `actions` (27).
`glomeris help exit-codes` renders 19 lines and correctly calls out
`execute` as the one worth reading in full.

`--help` was **deliberately not run** for `emergency`, `execute`, `free`,
`autopilot` or `daemon`. Invoking a destructive-capable command merely to
test its help surface is the exact mistake Finding 1 is about; those five are
covered by `tests/help_golden.rs`, which pins every rendered help surface
byte for byte and is green in the CI run cited under AC1.

The contradiction: `help exit-codes` promises that exit 2 means "an unknown
command or subcommand, a missing or **unrecognized argument**". Measured
against the same binary:

```
status   --definitely-not-a-flag  -> exit 0
detect   --definitely-not-a-flag  -> exit 0
explain  --definitely-not-a-flag  -> exit 1
llm-check --definitely-not-a-flag -> exit 2
```

`llm-check` honours the contract; `status` and `detect` silently swallow an
unrecognized flag and report success, and `explain` treats it as a positional
resource id and fails with "not found" rather than a usage error. This is
HORO-1322, already filed — but the new help text now *asserts* the contract
those three break, which strengthens the case (see Finding 2).

### AC9 — no secrets or absolute-path privacy regressions: **PASS, verified on the wire**

Rather than trusting the unit tests, the actual egress was captured. A
loopback HTTP listener stood in for the provider; `glomeris llm-plan
--project-root <disposable fixture> --json` was pointed at it with a
placeholder key and ran to exit 0 against a real 200 response.

The captured request was `POST /chat/completions`, `content-length: 1379`,
`user-agent: ureq/3.4.1`. Against the **full captured bytes**, headers
included:

| Probe | Occurrences |
|---|---|
| `/Users/` | 0 |
| this machine's username | 0 |
| `/private/tmp` | 0 |
| the fixture path | 0 |
| this machine's hostname | 0 |
| the working-tree directory name | 0 |

The JSON body has exactly two top-level keys, `messages` and `model`. Each
resource is described by exactly seven fields — `resource_id`, `kind`,
`reclaimable_bytes`, `age_days`, `regenerability`, `completeness`,
`offered_action_ids` — and `resource_id` is an **opaque alias**
(`resource_1` … `resource_4`), not a path. No file contents, no file names,
no user or host identity. The placeholder key appeared exactly once in the
capture, in the `Authorization` header, which is where a credential sent to
an endpoint the user named is supposed to be; it does not appear in the body.

One thing worth stating plainly rather than filing: `--project-root` does not
scope the payload. The captured body described the fixture's `node_modules`
alongside this machine's real Xcode DerivedData, Homebrew cache and Docker
build cache, with their real sizes. No paths and no identity leave — but the
*inventory* of which developer caches exist and how large they are does. That
is documented behaviour, not a regression: `src/cli/help.rs` already says
`--project-root` does not "bound every detector, so it is not a privacy
control". It is recorded here because it is the kind of thing a first-time
BYOK user could reasonably misread, and the privacy preview in the GUI is the
place that has to carry the point.

### AC10 — release-readiness record ends in `READY_FOR_NEXT_STAGE` or lists exact blocking defects: **this record; verdict below**

## Findings

### Finding 1 (process, non-blocking for the product) — destructive commands were executed to test dispatch

Four `glomeris emergency` runs happened during this campaign for the purpose
of exercising command dispatch and help behaviour, not because a fixture
required them. They are visible in `actions.jsonl` at 03:30:05, 03:31:42,
03:33:45 and 03:35:45 on 2026-09-21, and one `EMERGENCY` transition is in
`history.tsv` (pressure 79.58%, 94.0 GiB free). Nothing was deleted — every
one was refused by HORO-957's unscoped-`RunTool` guard — so the product's
safety held, but the process was wrong: the real Homebrew cache was the
target, and the only reason this is a non-event is that a guard caught it.

The corrective rule now in force for the rest of this campaign, and applied
throughout this gate: destructive-capable commands are not invoked to test
dispatch or help; destructive paths are exercised only against disposable
fixtures; `emergency` is not run at all. AC8 above documents the five help
surfaces deliberately left to `help_golden.rs` for exactly this reason.

### Finding 2 (non-blocking) — the new help text asserts a strictness contract three commands break

Details under AC8. Existing ticket: **HORO-1322**. Worth re-scoping there
that the defect is now a *documented-contract* violation, not just an
inconsistency: `help exit-codes` shipped in HORO-1311 promises exit 2 for an
unrecognized argument, and `status`/`detect` return 0, `explain` returns 1.
Root cause is `split_flags` in `src/main.rs`, which pushes any token not in
`known_flags` into `positionals` instead of rejecting it.

### Finding 3 (non-blocking) — two interactive controls are unlabelled or unexplained

Details under AC7. Existing ticket: **HORO-1323**, filed for
`CandidateDetailView.swift`.

That ticket's premise needs narrowing, and this gate narrowed it. The original
observation was that `CandidateDetailView.swift` contains zero accessibility
modifiers. It does — but it composes entirely from `GlomerisDesignSystem`
components that carry labels, header traits and tooltips centrally, so most of
that view is in fact labelled. Measuring accessibility by per-file modifier
count was the wrong instrument.

What survives is narrower and more specific: the Clean button has no hint and
no conveyed disabled-reason behind two stacked `.disabled` modifiers, and
`ProjectRootsPreferencesView`'s icon-only remove button has no label while its
row path bypasses `GlomerisPathText`. Those are the two things to fix, and
`ProjectRootsPreferencesView` should be added to HORO-1323's scope rather than
tracked separately.

### Finding 4 (BLOCKING for a release) — the release pipeline will fail on the next tag, and `brew install glomeris` cannot work

`dist-workspace.toml` carries `installers = ["homebrew"]`, `tap =
"Chisanan232/homebrew-tap"` and `publish-jobs = ["homebrew"]`. On this state:

- The tap repository does not exist (404).
- The repository has no `HOMEBREW_TAP_TOKEN` secret (`actions/secrets`
  reports `total_count: 0`), and there is no organization to inherit one from.
- `publish-homebrew-formula` in the generated `release.yml` is guarded only
  by a prerelease check, so it *will* run on a real tag, and its first step
  is `actions/checkout` against that nonexistent repository with that missing
  token.
- `announce` is conditioned on
  `needs.publish-homebrew-formula.result == 'skipped' || 'success'`, so a
  failed homebrew job leaves `announce` unrun and the whole release run red —
  even though `host` will already have created the GitHub Release with its
  CLI tarballs attached.

CI never caught this because on pull-request runs the homebrew job reports
`skipping`.

Importantly, `v0.2.0` itself was clean: `git show v0.2.0:dist-workspace.toml`
has `installers = []`, and that release's run had no homebrew job. This is a
post-`v0.2.0` regression introduced with the installer configuration, not a
long-standing breakage.

Tracked as **HORO-1320**, and it needs a founder decision rather than an
implementation: either create `Chisanan232/homebrew-tap` and mint a
`HOMEBREW_TAP_TOKEN`, or back the Homebrew installer out of MVP 2.0. Both
creating a repository and minting a credential are the founder's to do, and
choosing the second option would pre-empt the first, so neither was done
here.

The documentation half was fixed autonomously and is already merged
(**HORO-1321**, PR #76): `book/src/installation.md` no longer presents the
tap as the recommended install, states in the future tense that it does not
work yet, and the prebuilt-archive path is now the recommended one. The book
had been telling users to run a `brew install` that could not succeed.

## Cost

$0.00 in API spend — no external provider was contacted (AC4's live-provider
half is what remains). Machine cost: one CLI release build, one Xcode release
build, four refused `emergency` runs, two real deletions totalling 13.6 MB
inside `/tmp` fixtures.

## Verdict

**NOT `READY_FOR_NEXT_STAGE`.**

One blocking defect, stated exactly:

1. **HORO-1320** — `dist-workspace.toml` configures a Homebrew installer
   against a tap repository that does not exist, with no credential to push
   to it. Tagging MVP 2.0 in this state produces a red release run and a
   documented install path that cannot work. Needs a founder decision (create
   the tap and mint the token, or remove the installer from this milestone).

Three criteria are **incomplete rather than failed**, and none of them can be
closed by more of this kind of work:

- **AC3** — the subjective founder dogfood pass. Rig is built and running.
- **AC4** — one AI Plan through a real provider, which needs the founder's own
  API key.
- **AC7** — the VoiceOver / keyboard-only / eyes-on light-dark pass.

No threshold was waived to produce this verdict. Every criterion that could be
verified on this machine was verified against the shipping artifact and is
either PASS above with its evidence, or listed here.

Two non-blocking defects are filed and should not gate a tag: **HORO-1322**
(unknown-flag strictness on `status`/`detect`/`explain`) and **HORO-1323**
(an unlabelled icon-only remove button, and a Clean button whose
disabled-reason is not conveyed).

## Recommended next experiment

Decide HORO-1320 first, because it is cheap either way and it is the only
thing standing between this state and a tag. Then run the founder pass on the
rig as built — its value is concentrated in three questions this gate cannot
answer: is the menu-bar icon findable without being told where it is, does
the candidate ordering match what the founder would have chosen by hand, and
does the privacy preview make the "inventory leaves, paths do not" distinction
land before a key is entered.

## Explicitly not tested / uncertain

- **`emergency` was not exercised deliberately.** Its dispatch and its
  refusal behaviour are both evidenced above (Finding 1), but no `emergency`
  run was performed *for* this gate, and none should be.
- **`--help` for `emergency`, `execute`, `free`, `autopilot`, `daemon`** was
  not invoked; those surfaces rest on `tests/help_golden.rs`.
- **No live external LLM call.** The provider path is exercised end to end,
  but against a loopback listener.
- **Install from published instructions was not performed end to end.** The
  instructions themselves were corrected in HORO-1321, and the reason a real
  `brew install` was not attempted is Finding 4 — there is nothing to install
  from yet. Downloading and verifying a release tarball was covered by the
  2026-09-13 pass against `v0.2.0`.
- **`GlomerisMenuBar.app.zip` has never been published.**
  `macos-app-release.yml` landed after `v0.2.0` was tagged, so releases so far
  carry only CLI tarballs. The workflow is therefore unproven in a real
  `release: published` run; the bundle shape it produces is what this gate
  built by hand, but CI has never executed it.
- **Gatekeeper quarantine flow was not walked.** The rig was built locally, so
  it carries no `com.apple.quarantine` flag; the documented System Settings
  path in `book/src/installation.md` has not been re-walked in this pass.
- **Single host only.** macOS 15.7.7 on Apple M1 Pro. The `x86_64` target is
  built but was not run.
