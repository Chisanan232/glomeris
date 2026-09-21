# Glomeris MVP 2.0 Release Gate — Dogfood and Readiness Record

> **Internal research record — not public product documentation.**
> Not part of the mdBook site (`book/src/`), not linked from `SUMMARY.md`,
> not referenced from `README.md`. Lives under `docs/internal/` specifically
> so the `docs.yml` GitHub Pages pipeline (which only builds `book/` via
> mdBook and `cargo doc` from Rust source comments) never touches it.

This is the HORO-1313 release gate for the MVP 2.0 iteration (HORO-1305
through HORO-1312, plus HORO-1320 through HORO-1325). It is written criterion
by criterion against that ticket's ten acceptance criteria, and it deliberately
separates what was *verified on this machine* from what *still needs the
founder*. The verdict section at the end does not round anything up.

**This is the second pass.** The first pass (merged as PR #77) ran against
`main` at `1663bf5`. Four stories have merged since, two of them fixing things
that pass found, so every criterion was re-verified against a freshly built
artifact rather than carried forward. Where this pass contradicts the first,
the contradiction is called out rather than quietly overwritten — including two
places where the first pass, or work done between the passes, was **wrong**
(see Findings 4 and 6, and the corrected help line counts under AC8).

## Metadata

- **Date:** 2026-09-21
- **Released version/tag tested:** none. This gate ran against `main` at
  `342da6b` (`Merge pull request #80 … HORO-1325`), which is 330 commits ahead
  of the last tag (`v0.2.0`). There is no MVP 2.0 tag yet — cutting one is what
  this gate is deciding about, and Finding 5 is about why one cannot currently
  be cut at all.
- **Exact artifact tested:** `GlomerisMenuBar.app` built from `main` at
  `342da6b` with the release CLI embedded at `Contents/MacOS/glomeris`, ad-hoc
  re-signed (`codesign --force --deep --sign -`), staged at
  `~/glomeris-dogfood-mvp2-final/`, outside any git working tree.
  `Identifier=dev.glomeris.GlomerisMenuBar`, `Signature=adhoc`,
  `TeamIdentifier=not set`. The embedded CLI reports `glomeris 0.2.0` (see
  Finding 5 — that string is stale, not wrong). This is deliberately the same
  shape `macos-app-release.yml` produces in CI — bundle plus embedded CLI,
  ad-hoc signed — rather than a dev `target/release` binary run by hand.
- **Host/platform:** macOS 15.7.7 (build 24G720), Apple M1 Pro, arm64.
  Disk at gate time: `WARN`, 80.0% used, 92.2 GB free of 460.4 GB.
- **Surfaces exercised:** `glomeris --help`; `--help` for `status`, `scan`,
  `detect`, `explain`, `clean`, `llm-plan`, `llm-check`, `history`, `actions`;
  `glomeris help exit-codes`; `status`; `detect`; `explain`; `llm-check`;
  `llm-plan --json` against a loopback HTTP listener; `execute` on `AUTO_SAFE`,
  `PROTECTED` and `ASK` fixtures across five flag combinations; `autopilot
  show`/`enable`/`revoke`/`run` including `--dry-run`, all against disposable
  fixtures under a temporary directory; the menu-bar item and popover via the
  accessibility API; an isolated `.app`-bundled SwiftUI probe for the sheet
  keyboard measurement under AC7.
- **LLM API call count:** 0 served. Requests were issued to a local proxy on
  `127.0.0.1:15721`, which forwarded them to an external gateway and received
  HTTP 401 for every one — so an external provider *was* reached at the
  transport level but never answered a prompt. Separately, several requests went
  to purely local capture listeners (`127.0.0.1:19741`, `127.0.0.1:19742`) with
  self-invented placeholder keys; those never left the host.
- **Model name:** `gpt-6-astra` for the proxied attempts (the name the founder
  specified, confirmed on the wire, never served); `fake-capture-model` for the
  loopback captures.
- **Approximate API cost:** $0.00 — every proxied request was refused upstream
  before any token was generated, and nothing else contacted an external
  provider.
- **What left the machine:** only the metadata-only payload verified under AC9 —
  opaque `resource_N` aliases, no paths, no file contents, no account name, no
  repository names — describing disposable synthetic fixtures. See AC4 and AC9.

## Artifact under test, and why it was rebuilt again

The first pass rebuilt the rig because the then-running dogfood instance had no
embedded CLI and was falling back to stale on-`PATH` binaries. That reasoning
still holds and is not repeated here. This pass rebuilt again for a simpler
reason: four stories merged after `1663bf5`, two of which (HORO-1310's GUI
surface, HORO-1325's accessibility labels) change what a founder would be
looking at. Observing an artifact three stories behind `main` is what the first
pass called out, so it was not repeated.

Nothing was deleted to make room. The older rig is still on disk untouched, and
neither on-`PATH` binary was overwritten — which turns out to matter, because
one of them is now shadowing a real Homebrew install (Finding 6).

## Acceptance criteria

### AC1 — all implementation stories merged with CI green: **PASS**

Fourteen PRs for this iteration are merged into `main`:

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
| #77 | `v0.0.1/HORO-1313/docs/release_readiness_record` |
| #78 | `v0.0.1/HORO-1320/docs/homebrew_tap_live` |
| #79 | `v0.0.1/HORO-1310/feat/autopilot_gui_surface` |
| #80 | `v0.0.1/HORO-1325/fix/a11y_project_roots` |

On `main` at `342da6b` all eleven check runs are `completed/success`: `test`,
`build`, `deny`, `macos-app`, `no-policy-in-swift`, `xcodeproj-drift`,
`app-icon-drift`, `vocabulary-covers-cli-tokens`, `docs-cover-cli-commands`,
`credential-store-uses-keychain`, `deploy`.

### AC2 — Swift remains a thin client, confirmed mechanically and adversarially: **PASS**

All six guard scripts were re-run directly against this tree and pass,
including:

```
PASS: no policyLabel/policy_label branching detected under macos/GlomerisMenuBar/Sources.
PASS: macos/GlomerisMenuBar/Sources/GlomerisVocabulary.swift is display-only
      (no SwiftUI/AppKit, no control, no gating field).
PASS: 10 of 12 vocabularies verified against their Rust producer.
```

The adversarial half asked the opposite question — not "does the guard pass"
but "what authority could Swift exercise if it wanted to":

- **Deletion:** zero occurrences of `removeItem`, `trashItem`, `unlink(`, or
  `FileManager.default.remove` anywhere under
  `macos/GlomerisMenuBar/Sources`. The GUI has no code path that can delete
  anything at all; every deletion it appears to offer is the CLI's.
- **Arbitrary execution:** the only process-spawning site is
  `GlomerisClient.swift`, `Process()` with
  `process.executableURL = try resolveExecutableURL()`. There is no
  `launchPath`, no `/bin/sh`, no `/bin/bash`, no `system(`, no
  `NSAppleScript`. The client is constructed with a *pinned* executable URL.
- **Policy:** no policy label is branched on, which is what the guard proves;
  combined with the two points above, the GUI cannot classify, cannot
  override, and cannot act outside the CLI.

One methodological note, because it produced a wrong answer first: an
unanchored grep for `system(` matches `.system(.headline, …)` font calls. The
count above comes from anchored patterns (`(^|[^.[:alnum:]_])system\(`). The
loose version reports authority the GUI does not have.

That `10 of 12` line is not a pass with a footnote; it is Finding 7.

### AC3 — real founder dogfood recording blocking and non-blocking observations: **INCOMPLETE — requires the founder**

Everything mechanically checkable was checked and is recorded here. The
subjective half of this criterion — whether the menu-bar icon is *findable*,
whether the status wording is *understood*, whether the candidate ranking
*feels* right, whether the visual hierarchy *guides the eye* — is not
something this pass can honestly self-certify. The rig is built, current, and
running for that pass.

### AC4 — at least one AI Plan against disposable fixtures with a real provider: **INCOMPLETE — the configured provider route refuses every request upstream**

**This section supersedes an earlier version of itself, which said the criterion
was blocked because no provider credential existed on the workstation. That was
wrong.** A credential does exist, the founder authorised its use, and the run was
attempted. It failed for an entirely different reason, and the corrected reason is
worth more than the original claim was.

The route under test is the one the founder specified: a local proxy on
`127.0.0.1:15721`, model `gpt-6-astra`, with the credential passed by environment
*reference* so its expanded value never entered a command line, a file, or shell
history.

#### The decisive finding: the client's credential is not used at all

Three requests were sent to the proxy, identical but for authentication — the real
environment credential, a self-invented placeholder string, and no authorization
header whatsoever. **All three responses were byte-identical (same md5).** The
proxy discards the client's credential and authenticates upstream with its own
stored one.

That single result reframes the whole criterion. Which key Glomeris is configured
with has no bearing on this route, so no client-side configuration change — and no
different credential — can affect the outcome.

#### The refusal is model-independent and endpoint-independent

| Request | Result |
|---|---|
| `POST /v1/chat/completions`, model `gpt-6-astra` | upstream HTTP 401 |
| `POST /v1/chat/completions`, a second model name the same proxy also fronts | upstream HTTP 401 |
| `POST /v1/responses`, model `gpt-6-astra` | upstream HTTP 401 |
| `GET /v1/models` | HTTP 200, empty catalogue |
| `GET /health` | HTTP 200, `healthy` |

The versioned and unversioned chat-completions paths normalise onto the same
upstream endpoint, so `chat_completions_url()` appending `/chat/completions` to a
bare origin is correct here and needed no adjustment.

The proxy's own status endpoint is the clearest evidence, and it indicts nothing in
this repository: `success_requests: 0`, `failed_requests: 11`, success rate `0.0`,
`failover_count: 0`, an empty `active_targets` list, and
`last_error: 所有供应商都失败` — *all providers failed*. The proxy process is
healthy and has no working upstream target.

So the three candidate explanations are all excluded. Not a Glomeris defect; not a
wrong or unavailable model name; not a missing or malformed client credential.

#### What the product did at that boundary, which is a positive result

```
{
  "items": [],
  "dropped_unknown_resource": 0,
  "dropped_unknown_action": 0,
  "provider_error": "provider returned HTTP 401 for openai:chat_completions
                     POST /chat/completions: {…upstream body…}"
}
```

Exit 1, empty plan, and a `provider_error` carrying the api style, the request
**path only**, and the upstream body verbatim — with no credential material, no key
prefix, and no echoed header. This is HORO-1299's `ProviderStatus` variant meeting
a real non-2xx from a real gateway for the first time rather than a test stub, and
it held.

#### What is now positively established, and what is not

| Requirement | Status |
|---|---|
| Proxy request succeeds | **NO** — upstream 401 on every request |
| `gpt-6-astra` genuinely reached | **NO** — correct model name on the wire, never served |
| Glomeris parses a provider response | **YES** — loopback 200 → 2 validated items, `provider_error: null`, exit 0 |
| Plan non-empty where appropriate | **YES** under loopback; unverifiable against the refusing provider |
| Hallucinated ids cannot bypass policy | **YES** — see below |
| No destructive action executed | **YES** — audit log empty for the run |
| No secret or identifying metadata leaves | **YES** — see AC9 |

The hallucination test is the strongest result of the pass. A plan containing one
nonexistent `resource_id` and one invented `action_id`, mixed with two valid
entries, produced `dropped_unknown_resource: 1` and `dropped_unknown_action: 1`;
the two survivors were labelled `ASK` and `AUTO_SAFE` **locally from the
filesystem**, not from anything the plan asserted. A model's ranking never becomes
authority, which is the invariant this whole design exists to hold.

Nothing executed: the run's audit log holds zero events, the machine's real audit
log is unchanged with its newest entry predating the attempt by an hour, and both
fixtures are intact (29 files under the cargo target, 1055 under `node_modules`).

#### What would close it

One step, on the proxy rather than on this product: restore a working upstream
target for its `default` provider. That was not attempted — it is shared tooling
holding a company credential, and breaking it would affect other tools on this
machine rather than just this run. The gateway was also not probed directly to
determine whether the stored credential is merely expired, because that would have
sent a corporate credential off the host and bypassed the proxy the founder
directed all traffic through. Once `active_targets` is non-empty the remaining
three requirements take under a minute.

### AC5 — one safe manual action and one bounded Autopilot action execute successfully: **PASS**

Re-run this pass against a fresh disposable fixture set, with the product's own
audit log (`~/Library/Application Support/Glomeris/actions.jsonl`) as the
evidence rather than this document's own narration:

| Source | Resource | Class | Outcome | Reclaimed |
|---|---|---|---|---|
| `execute` | `node_modules:…/gate1313b/proj-a/node_modules` | `AUTO_SAFE` | `succeeded` | 9,437,184 B |
| `execute` | `cargo_target_dir:…/gate1313b/ask-cargo/target` | `ASK` | `succeeded` | not measured — see below |
| `autopilot_auto_safe` | `node_modules:…/gate1313b/proj-b/node_modules` | `AUTO_SAFE` | `succeeded` | 4,194,304 B |

The Autopilot half was exercised as a sequence rather than a single success,
because a bound that is never tested is not a bound:

1. **Default-deny.** With the envelope revoked, `autopilot run` exits 3,
   prints "Autopilot is not enabled. Nothing was attempted", and touches
   nothing.
2. **A bound that bites.** Envelope granted at `max_bytes = 1048576` against a
   4 MiB fixture. `--dry-run` and then a real run both refuse it by name:
   `refused: would reclaim 4.0 MB with 1.0 MB left in the byte budget`.
   Actions attempted 0, bytes freed 0, and **no ledger line was written** —
   the refusal left the audit log untouched because nothing executed.
3. **The kind allowlist gating real machine resources.** In the same runs, this
   machine's real Homebrew cache (`AUTO_SAFE`) and real Xcode DerivedData were
   both discovered and both refused: `refused: homebrew_cache is not on the
   Autopilot allowlist`. The allowlist, not luck, is what kept a bounded
   Autopilot run off real user data.
4. **In-bounds execution.** Envelope widened to `max_bytes = 5242880` with
   `max_actions = 1`. One action, 4.0 MB, `source = autopilot_auto_safe`,
   `actual_reclaimed_bytes = 4194304`.
5. **Revoked again.** `autopilot revoke`, then `autopilot run` exits 3 once
   more. Limits are retained in the file, which is the documented behaviour so
   that a later `enable` cannot return with limits nobody read.

Two things about the `ASK` row are worth stating precisely, because both look
like defects and neither is:

- `actual_reclaimed_bytes` is `null` on a **succeeded** cargo action, while the
  `node_modules` actions report a measured figure. This is deliberate and
  documented at `src/executor/mod.rs`: a `RunTool` step delegates to an
  external tool, "there is no general way to know what the external tool
  actually freed, so that is honestly reported as `Unavailable` rather than
  estimated". Only an all-`DeletePath` plan gets a measured total. The user is
  shown an expectation with no verified actual — which is the correct answer on
  the evidence-confidence axis, not a gap on the storage-impact axis.
- The first attempt at this row **failed**, with `cargo exited with exit status:
  101: no targets specified in the manifest`. That was a defect in the fixture,
  not the product, and the product's behaviour on it was the point: `outcome:
  failed`, `actual_reclaimed_bytes: null`, `expected_reclaimed_bytes` still
  3,145,728 — it did not claim bytes it had not reclaimed. Both the failed and
  the succeeded attempt are in the ledger; neither was tidied away.

### AC6 — one PROTECTED/UNKNOWN case proves fail-closed behaviour: **PASS**

Five distinct execute-time refusals were observed on the shipping artifact this
pass, covering three of the seven reason codes plus two argument-level
rejections. Every one left the target on disk.

| Attempt | Reason | Exit |
|---|---|---|
| `PROTECTED`, plain `execute` | `protected` | 3 |
| `PROTECTED`, valid `--confirm-ask` + freshly matching fingerprint | `protected` | 3 |
| `ASK`, no consent flags | `ask_no_consent` | 3 |
| `ASK`, consent flag with a fingerprint observed for a *different* resource | `ask_consent_mismatch` | 3 |
| `ASK`, malformed fingerprint token | argument error | 2 |

The `PROTECTED` pair is the load-bearing one. A real `node_modules` tree created
under a path containing an `.ssh` component was discovered by the live Node
detector, classified `PROTECTED` with reason `protected_credential_material` and
`executable: false`, and refused with a message that states the rule rather than
the instance: *"no flag combination can authorize executing against it"*.
Supplying a genuinely valid consent token changed nothing, which is the whole
claim. The payload file is still on disk.

This also re-confirms the correction the first pass made to
`book/src/known_limitations.md` (merged in PR #76): a protected pattern matches
a path *component*, so anything discoverable beneath one is protected, and the
safety conclusion comes out stronger rather than weaker.

Separately, and from earlier in the campaign rather than this pass: four
`emergency` invocations each reached `homebrew.cleanup.cache` on the real
Homebrew cache, classified `AUTO_SAFE`, and each logged `"outcome":"failed"`
with `"actual_reclaimed_bytes":null` — HORO-957's unconditional refusal of a
`RunTool` step with `scoped_path: None`, holding on a real machine against a
real `AUTO_SAFE` classification, four times in a row. Nothing was cleaned. See
Finding 1 for why those four runs happened at all.

One observation that is not a refusal but belongs here: `xcode_derived_data`
was classified `ASK` on one pass and `UNKNOWN_INCOMPLETE` on a later one, on
the same real resource, because a build was writing to it in between. The
classification moved on the evidence-quality axis while the safety outcome did
not move at all — both passes refused it. That is the axis separation working,
observed live rather than argued.

### AC7 — VoiceOver/keyboard/accessibility and light/dark exercised: **PARTIAL — mechanical half done, subjective half requires the founder**

HORO-1325 merged since the first pass and closed half of what that pass found.
What follows is the state *after* it.

- **Accessibility coverage is centralised, and per-file counts are a misleading
  way to measure it.** The labels live in `GlomerisDesignSystem`:
  `GlomerisBadgeView` is one VoiceOver element with
  `.accessibilityElement(children: .ignore)`, `.accessibilityLabel(term
  .accessibilityLabel)` and `.help(term.explanation)`; `GlomerisPathText`
  carries `.accessibilityLabel("Path: \(path)")` plus a `.help(path)` tooltip
  so a middle-truncated path is still read in full; card titles get
  `.accessibilityAddTraits(.isHeader)`. `CandidateDetailView` composes
  exclusively from those shared components, so its zero per-file count is
  inheritance, not absence. The first pass reached this conclusion only after
  first reaching the wrong one; it is restated here so the count is not
  re-misread later.
- **`ProjectRootsPreferencesView` is fixed** (HORO-1325, PR #80). Its icon-only
  remove button, its placeholder-only text field and its Add button now carry
  accessible names from a `ProjectRootsWording` enum, and seven new tests pin
  the wording — including that two rows read *differently*, which was the
  actual defect: "Remove" alone is read identically on every row, which on a
  list of paths says nothing. A new design-system test
  (`testNoIconOnlyControlShipsWithoutAName`) now guards the class of defect
  rather than the instance.
- **One gap remains, and it is on a control that deletes.**
  `CandidateDetailView`'s `Button("Clean")` is announced from its title, but has
  no `.accessibilityHint` saying what will be deleted, and it stacks two
  independent `.disabled` modifiers (`!viewModel.isCleanEnabled`, then
  `isExecuting`) with nothing conveying *which* reason is in force — so a
  screen-reader user hears an unavailable button and cannot find out why. This
  is what is left of HORO-1323.
- **A related observation on the same view, recorded not filed.** The cleanup
  confirmation alert's `Confirm` button is the alert's default keyboard action
  and is the only file-deleting control in the target without
  `role: .destructive`. The `Cancel` beside it does carry `role: .cancel`. This
  is a one-line change, but it changes a confirmation dialog's default
  behaviour, so it is left for the founder's AC3 pass to judge rather than
  changed under a documentation ticket.
- **Keyboard: a suspected trap, measured and dismissed.** The AI-provider
  settings sheet presents from the menu-bar popover with no visible dismiss
  control, no `Environment(\.dismiss)` and no `.keyboardShortcut(.cancelAction)`
  — which reads like a keyboard dead end. It is not. Measured on macOS 13+, a
  SwiftUI `.sheet` presented from a
  `MenuBarExtra(…).menuBarExtraStyle(.window)` popover **is** dismissed by
  Escape, and Escape dismisses only the sheet, leaving the popover open. The
  measurement used a control run first — six seconds idle with no keystroke,
  `sheets=1` — so a sheet that dismissed itself could not have been misread as
  a keyboard success. Downgraded from a possible blocker to a discoverability
  point for the founder's pass: it works, but nothing on screen says so.
- **Light/dark is structurally sound.** No hardcoded `Color(red:…)`,
  `Color(.sRGB…)` or `Color(white:)` literals anywhere in the Swift sources, so
  nothing is pinned to one appearance.
- **Colour is never load-bearing.** `GlomerisDesignSystem` renders every
  vocabulary chip as symbol **and** word, with the tint as the third channel.
  This satisfies the standing "do not rely on colour alone" rule by
  construction rather than by review.
- **The GUI has wording for every refusal the CLI can emit.** All seven reason
  codes documented on `ExecuteReport` resolve to a distinct title and
  explanation in `GlomerisVocabulary.refusal`, plus a `busy` case and a
  `default`, and all seven are asserted in the Swift tests. Three of them were
  driven end to end against the shipping artifact under AC6. One of the seven
  explains only one of its two causes — filed as HORO-1326, and the structural
  reason this could drift unnoticed is Finding 7.

A note on method, because it bounds what the above is worth: the accessibility
API cannot read SwiftUI `Button` labels — six popover buttons all report an
empty `name`, and a plainly-titled control does too, so absence proves nothing
there. It also stops being able to coerce `entire contents of window 1` once a
SwiftUI tree re-renders. Claims about the real app's accessibility therefore
rest on source and tests; the one live keyboard measurement above was taken
against an isolated `.app`-bundled probe built for that single question, which
was then removed.

The subjective half — an actual VoiceOver pass, actual keyboard-only
navigation, and an eyes-on light/dark comparison — needs the founder.

### AC8 — CLI help validated from a clean install: **PASS, with one contradiction found**

Validated against the *shipping artifact* (the CLI embedded in the freshly
built bundle), not a dev binary. `glomeris --help` renders 45 lines, exit 0,
and groups all fourteen commands into five bands by intent — INSPECT (4) /
PLAN (3) / ACT (4) / OBSERVE (2) / SERVICE (1) — with a "Start here" block
naming four concrete first commands.

Every non-destructive subcommand renders its own help at exit 0:

| Surface | Lines |
|---|---|
| `status --help` | 19 |
| `scan --help` | 26 |
| `detect --help` | 30 |
| `explain --help` | 32 |
| `clean --help` | 26 |
| `llm-plan --help` | 46 |
| `llm-check --help` | 29 |
| `history --help` | 21 |
| `actions --help` | 28 |
| `help exit-codes` | 20 |

**These counts correct the first pass**, which reported each of them one line
short (18/25/29/31/25/45/28/20/27, and 19 for `help exit-codes`). The first
pass measured them through an unquoted shell variable; zsh does not word-split
an unquoted `$VAR`, so `$G $c` with `c="status --help"` passed *one* argument
and every surface was measured wrong in the same direction. The counts here
come from `${=c}` and were cross-checked against the committed golden fixtures
in `tests/fixtures/help/`, which all eleven surfaces match byte for byte —
except `top-level.txt`, whose single differing line is the deliberate
`{VERSION}` placeholder that `help_golden.rs` substitutes at test time. A dev
`target/release` binary produces the identical one-line difference, which is
how the placeholder was distinguished from a real regression.

`--help` was **deliberately not run** for `emergency`, `execute`, `free`,
`autopilot` or `daemon`. Invoking a destructive-capable command merely to test
its help surface is the exact mistake Finding 1 is about; those five are
covered by `tests/help_golden.rs`, which pins every rendered help surface byte
for byte and is green in the CI run cited under AC1. (`autopilot --help` *was*
read this pass, as part of exercising the envelope under AC5 — reading the help
of a command whose bounded behaviour is being tested against disposable
fixtures is not the pattern Finding 1 prohibits.)

The contradiction: `help exit-codes` promises that exit 2 means "an unknown
command or subcommand, a missing or **unrecognized argument**". Measured
against the same binary:

```
status    --definitely-not-a-flag  -> exit 0
detect    --definitely-not-a-flag  -> exit 0
explain   --definitely-not-a-flag  -> exit 1
llm-check --definitely-not-a-flag  -> exit 2
execute   --definitely-not-a-flag  -> exit 2
```

`llm-check` and `execute` honour the contract; `status` and `detect` silently
swallow an unrecognized flag and report success, and `explain` treats it as a
positional resource id and fails with "not found" rather than a usage error.
This is HORO-1322, already filed — and this pass found the same leniency in a
*released* artifact, which makes it more than a main-branch inconsistency: the
brew-installed `v0.2.0` binary answers `glomeris help exit-codes` by printing a
one-line flat usage string and exiting **0**. Users on the released version get
a success exit for a command that did not run. See Finding 2.

### AC9 — no secrets or absolute-path privacy regressions: **PASS, verified on the wire**

Rather than trusting the unit tests, the actual egress was captured again this
pass against the *shipping artifact*. A loopback HTTP listener stood in for the
provider; `glomeris llm-plan --project-root <disposable fixture> --json` was
pointed at it with a placeholder key and ran to exit 0 against a real 200
response.

The captured request was `POST /chat/completions`, 1597 bytes total,
`content-length: 1379`. Against the **full captured bytes**, headers included:

| Probe | Occurrences |
|---|---|
| `/Users/` | 0 |
| this machine's username | 0 |
| `/private/tmp` | 0 |
| the fixture directory name | 0 |
| this machine's hostname | 0 |
| the working-tree directory name | 0 |
| the string `glomeris` | 0 |

The JSON body has exactly two top-level keys, `messages` and `model`. Each
resource is described by exactly seven fields — `resource_id`, `kind`,
`reclaimable_bytes`, `age_days`, `regenerability`, `completeness`,
`offered_action_ids` — and `resource_id` is an **opaque alias** (`resource_1` …
`resource_4`), not a path. No file contents, no file names, no user or host
identity. The placeholder key appeared exactly once in the capture, in the
`Authorization` header, which is where a credential sent to an endpoint the
user named is supposed to be; it does not appear in the body.

One thing worth stating plainly rather than filing: `--project-root` does not
scope the payload. The captured body described the fixture's `node_modules`
alongside this machine's real Xcode DerivedData, Homebrew cache and Docker
build cache, with their real sizes. No paths and no identity leave — but the
*inventory* of which developer caches exist and how large they are does. That
is documented behaviour, not a regression: `src/cli/help.rs` already says
`--project-root` does not "bound every detector, so it is not a privacy
control". It is recorded here because it is the kind of thing a first-time
BYOK user could reasonably misread, and the privacy preview in the GUI is the
place that has to carry the point. It is also the single most useful thing for
the founder to judge before entering a real key.

### AC10 — release-readiness record ends in `READY_FOR_NEXT_STAGE` or lists exact blocking defects: **this record; verdict below**

## Findings

### Finding 1 (process, non-blocking for the product) — destructive commands were executed to test dispatch

Four `glomeris emergency` runs happened earlier in this campaign for the purpose
of exercising command dispatch and help behaviour, not because a fixture
required them. They are visible in `actions.jsonl` at 03:30:05, 03:31:42,
03:33:45 and 03:35:45 on 2026-09-21, and one `EMERGENCY` transition is in
`history.tsv`. Nothing was deleted — every one was refused by HORO-957's
unscoped-`RunTool` guard — so the product's safety held, but the process was
wrong: the real Homebrew cache was the target, and the only reason this is a
non-event is that a guard caught it.

The corrective rule remains in force and was applied throughout this pass:
destructive-capable commands are not invoked to test dispatch or help;
destructive paths are exercised only against disposable fixtures; `emergency` is
not run at all. Every deletion recorded under AC5 this pass was inside a
temporary fixture directory, and the two real resources Autopilot discovered
were refused by its allowlist.

### Finding 2 (non-blocking) — the strictness contract is broken in a released binary, not just on main

Details under AC8. Existing ticket: **HORO-1322**.

The first pass framed this as the new help text asserting a contract three
commands break. This pass found the more concrete version: the released
`v0.2.0` binary, installed via the now-live Homebrew tap, prints a one-line
usage string and exits **0** for `glomeris help exit-codes`. That is not a
main-branch inconsistency a user will never meet; it is the behaviour of the
artifact the install instructions currently produce. Root cause is unchanged —
`split_flags` in `src/main.rs` pushes any token not in `known_flags` into
`positionals` instead of rejecting it.

Worth recording as a method note too: this was nearly missed because the exit
code was checked before the output. `help exit-codes` returning 0 looked like
support for the command. It is the absence of support, expressed as leniency —
which is the defect itself, arriving disguised as evidence against itself.

### Finding 3 (non-blocking) — one interactive control remains unexplained

Details under AC7. Existing ticket: **HORO-1323**, now narrower than when it
was filed, and narrower than the first pass left it.

Its original premise — that `CandidateDetailView.swift` contains zero
accessibility modifiers — was true but misleading, because that view composes
from shared components that carry labels centrally. The first pass corrected
that and widened the ticket to cover `ProjectRootsPreferencesView`. HORO-1325
has since **fixed** the `ProjectRootsPreferencesView` half and merged (PR #80).

What is left is one thing: the Clean button has no hint saying what will be
deleted, and no conveyed disabled-reason behind two stacked `.disabled`
modifiers. HORO-1323 should be rescoped to exactly that. The confirmation
alert's non-destructive-role `Confirm` button is recorded under AC7 for the
founder's judgement rather than folded into this ticket.

### Finding 4 (BLOCKING for a release) — the Homebrew publish job still has no credential

This finding replaces the first pass's version of it, which is now partly
obsolete and was partly wrong.

**What has changed since:** `Chisanan232/homebrew-tap` now exists,
`brew tap chisanan232/tap` resolves, and `brew install glomeris` installs
formula 0.2.0 from it (6 files, 3.3 MB). The tap is no longer a 404, and the
bootstrap formula's download URLs and SHA-256 sums point at real `v0.2.0`
release assets. The first pass's "the tap repository does not exist" is
resolved, and the documentation half was fixed and merged earlier (HORO-1321,
PR #76).

**What has not changed, and still blocks:** the repository has no
`HOMEBREW_TAP_TOKEN` secret. `actions/secrets` reports `total_count: 0`, and
there is no organization to inherit one from. In the generated `release.yml`,
`publish-homebrew-formula` is guarded only by a prerelease check, so it *will*
run on a real tag, and its first step is `actions/checkout` against
`Chisanan232/homebrew-tap` with `token: ${{ secrets.HOMEBREW_TAP_TOKEN }}`
followed by a `git push`. `announce` is conditioned on that job's result being
`skipped` or `success`, so a failed homebrew job leaves `announce` unrun and
the whole release run red — even though `host` will already have created the
GitHub Release with its CLI tarballs attached.

CI never catches this because on pull-request runs the homebrew job reports
`skipping`.

`v0.2.0` itself was clean: `git show v0.2.0:dist-workspace.toml` has
`installers = []`, and that release's run had no homebrew job. This is a
post-`v0.2.0` regression introduced with the installer configuration.

Tracked as **HORO-1320**. The remaining step is a credential, which is the
founder's to mint and is stated exactly in the hand-back rather than guessed
at here. `release.yml` is generated by cargo-dist and was deliberately not
hand-edited: the `plan` job would drift against it, and editing generated CI
definitions is out of bounds regardless.

### Finding 5 (BLOCKING for a release) — no MVP 2.0 tag can be cut from the current state

New this pass, and not visible in the first one. Filed as **HORO-1328**.

`Cargo.toml` on `main` still declares `version = "0.2.0"` — the same version as
an existing tag — 330 commits later. `v0.2.0` cannot be reused, and any other
tag names a version the workspace does not declare, which on a tag push is fed
straight to `dist host --steps=create --tag=$GITHUB_REF_NAME`.

This also explains a smaller thing that looks like a bug and is not: the
shipping artifact's embedded CLI answers `--version` with `glomeris 0.2.0`
while being 330 commits ahead of that release. The string is stale rather than
wrong, and it means a dogfood build is indistinguishable by `--version` from
the released one — which is worth knowing when reading any report that cites a
version.

What this needs is one value, not an implementation. 0.3.0 is the conventional
pre-1.0 choice; 1.0.0 is the alternative if MVP 2.0 is meant to be the first
stable public release, which is a product-identity decision. It is sequenced
*before* HORO-1320's formula work, because the formula's URLs name the release
tag.

cargo-dist's exact failure mode for a tag/version mismatch was **not**
reproduced locally: installing cargo-dist means piping a remote script into a
shell, which this workstation does not permit without explicit authorization.
The blocking conclusion does not depend on that detail — the tag collision
alone is sufficient — and the unverified part is flagged rather than presented
as measured.

### Finding 6 (non-blocking; corrects an earlier claim in this campaign) — the Homebrew install is installed but not linked

Also new this pass, and it narrows a claim made between the two passes.

`brew install glomeris` genuinely succeeded: the keg is in the Cellar at
`/opt/homebrew/Cellar/glomeris/0.2.0/bin/glomeris`, dated 13 Sep, and invoking
it directly works (`status` exits 0 and reports real disk pressure; `detect
--json` returns three candidates). But the keg was **never linked** —
`/opt/homebrew/var/homebrew/linked/glomeris` does not exist — and brew says why
in its own caveat:

```
The following glomeris executables are shadowed by other commands earlier in your PATH:
  glomeris (shadowed by /opt/homebrew/bin/glomeris)
```

`/opt/homebrew/bin/glomeris` is a hand-placed **regular file** from 20 Sep,
3,398,432 bytes, not a symlink into the Cellar, and it predates HORO-1311 (it
answers `unknown command '--version'`). So `which glomeris` on this machine
resolves to a stale hand-placed binary, not the brew-installed one, and
`brew link --overwrite --dry-run` reports it "would remove"
`/opt/homebrew/bin/glomeris`.

Two consequences, kept separate:

- **For this campaign's record:** the earlier claim of a *clean* brew
  tap/install/CLI verification is too strong and is narrowed here. What is
  verified is that the tap resolves, the formula installs, and the installed
  binary runs. What is *not* verified is a clean first-time install path
  end to end, because on this machine the result is shadowed.
- **For the product:** this is local machine state created by this campaign's
  own earlier steps, not a product defect. It is recorded rather than fixed
  because fixing it means deleting a binary, and nothing gets deleted here
  without the founder saying so. It is on the housekeeping list.

There is a real user-facing point inside it, though: anyone who installed
Glomeris by hand before tapping will silently keep running the old binary, with
only a `brew` caveat to say so. Worth a line in `book/src/installation.md` when
HORO-1320 is settled.

### Finding 7 (non-blocking) — refusal reason codes are the one user-facing vocabulary the drift guard cannot check

New this pass. Filed as **HORO-1327**.

`scripts/check-vocabulary-covers-cli-tokens.sh` verifies 10 of 12 vocabularies
against their Rust producer and names the two it cannot: `outcome` and
`refusal`, "covered by the transcribed Swift tests only". The guard can only
diff a vocabulary with a single `as_str`-style producer, and the seven refusal
reason codes have none — they are documented in a doc comment on
`ExecuteReport` and then written as separate string literals at distinct call
sites in `src/main.rs`.

This is not hypothetical. `ActionSource` had the same shape — five strings at
four call sites — and HORO-1310 then added `autopilot_auto_safe` and
`autopilot_preauthorized_ask` without the GUI learning words for them. Nothing
failed loudly; the history panel would simply have called Autopilot's own rows
an unrecognised trigger. HORO-1312 gave `ActionSource` a single producer, and
the guard now verifies it — that line is in the AC2 output above.

`refusal` is the vocabulary where silence costs most: an unrecognised reason
renders as "The CLI refused for a reason this app has no wording for" at
exactly the moment a user is being told they may not delete something. All
seven codes are correctly worded and test-pinned *today*, which is why this is
a drift risk rather than an outage.

Related but distinct, filed separately as **HORO-1326**: the GUI's wording for
`ask_consent_mismatch` explains only one of that code's two documented causes.
It says "The resource changed after you confirmed" — but the reason also fires
for a fingerprint observed against a *different* resource, which was reproduced
under AC6, and in that case nothing changed. Safety is unaffected (exit 3,
nothing deleted, and the GUI's own flow cannot reach that cause today); the
statement is simply untrue in one branch.

## Cost

$0.00 in API spend. An external gateway *was* contacted, through the local proxy,
but it returned HTTP 401 to every request, so no prompt was ever served and no
tokens were billed (AC4). Machine cost this pass: one CLI release build, one
Xcode release build, six real deletions totalling roughly 20 MB, all inside a
temporary fixture directory, plus one isolated SwiftUI probe app built and
removed.

## Verdict

**NOT `READY_FOR_NEXT_STAGE`.**

Two blocking defects, stated exactly, and they are sequenced:

1. **HORO-1328** — `Cargo.toml` declares `0.2.0`, which is already a released
   tag, 330 commits behind `main`. No MVP 2.0 tag can be cut from this state
   whatever number is chosen. Needs one decision (0.3.0 or 1.0.0), then a bump.
   **First**, because the Homebrew formula's URLs name the release tag.
2. **HORO-1320** — `dist-workspace.toml` configures a Homebrew installer whose
   publish job checks out `Chisanan232/homebrew-tap` with
   `secrets.HOMEBREW_TAP_TOKEN`, and that secret does not exist
   (`total_count: 0`). The tap itself now exists and installs, so this has
   narrowed from "two things missing" to exactly one: a credential the founder
   must mint. Tagging in this state produces a red release run with `announce`
   unrun.

Three criteria are **incomplete rather than failed**, and none can be closed by
more of this kind of work:

- **AC3** — the subjective founder dogfood pass. Rig is built, current, running.
- **AC4** — one AI Plan through a real provider. Attempted with the existing
  authorised credential and refused: the local proxy discards the client's
  credential (three byte-identical responses across a real key, a placeholder
  and no header) and every forwarded request 401s upstream regardless of model
  or endpoint, with the proxy reporting 0 of 11 successes and no active targets.
  Not a defect in this product, and not closable from this side; four of the
  seven requirements are nonetheless positively established.
- **AC7** — the VoiceOver / keyboard-only / eyes-on light-dark pass. Its
  mechanical half is done and one suspected keyboard trap was measured and
  dismissed.

No threshold was waived to produce this verdict. Every criterion that could be
verified on this machine was verified against the shipping artifact and is
either PASS above with its evidence, or listed here. Two places where an earlier
claim in this campaign was too strong are corrected rather than left standing:
the help line counts under AC8, and the brew install verification in Finding 6.

Four non-blocking defects are filed and should not gate a tag: **HORO-1322**
(unknown-flag strictness, now shown to affect a released binary), **HORO-1323**
(the Clean button's missing hint and unconveyed disabled-reason, rescoped),
**HORO-1326** (one refusal reason's wording covers one of two causes) and
**HORO-1327** (refusal reason codes have no single producer, so the drift guard
skips them).

## Recommended next experiment

Settle HORO-1328 then HORO-1320, in that order — both are single decisions, and
together they are the only thing standing between this state and a tag. Then run
the founder pass on the rig as built. Its value is concentrated in four
questions this gate cannot answer:

- Is the menu-bar icon findable without being told where it is?
- Does the candidate ordering match what the founder would have chosen by hand?
- Does the privacy preview make the "inventory leaves, paths do not"
  distinction land *before* a key is entered? AC9 establishes that the
  distinction is real; whether it is legible is the open question.
- Does the Autopilot surface make "AI recommends, policy decides, executor
  verifies" visible, given that AC5 shows the allowlist is what actually kept a
  bounded run off real user data?

## Explicitly not tested / uncertain

- **`emergency` was not exercised.** Its dispatch and its refusal behaviour are
  both evidenced above (Finding 1, AC6), but no `emergency` run was performed
  *for* this gate, and none should be.
- **`--help` for `emergency`, `execute`, `free`, `daemon`** was not invoked;
  those surfaces rest on `tests/help_golden.rs`.
- **No completed external LLM call.** The provider path is exercised end to end
  against a loopback listener, and against a real gateway through the local proxy
  — where it reached the transport and was refused 401 without a prompt ever
  being served. The response-handling half is therefore proven only against
  loopback. This is AC4.
- **Whether the proxy's stored upstream credential is expired, revoked, or
  misrouted was not determined.** Distinguishing them means probing the gateway
  directly, which would send a company credential off this host and bypass the
  proxy all traffic was directed through. Deliberately not done.
- **cargo-dist behaviour was not reproduced locally** (Finding 5). Installing
  it requires piping a remote script into a shell.
- **A clean first-time `brew install` was not verified end to end** (Finding 6).
  The formula installs; the result is shadowed on this machine by a
  pre-existing hand-placed binary that was deliberately not removed.
- **`GlomerisMenuBar.app.zip` has never been published.**
  `macos-app-release.yml` landed after `v0.2.0` was tagged, so releases so far
  carry only CLI tarballs. The bundle shape it produces is what this gate built
  by hand, but CI has never executed it in a real `release: published` run.
- **Gatekeeper quarantine flow was not walked.** The rig was built locally, so
  it carries no `com.apple.quarantine` flag.
- **VoiceOver was not driven programmatically.** The accessibility API cannot
  read SwiftUI `Button` labels, so an automated pass would produce empty names
  for correctly-labelled controls and prove nothing. Claims under AC7 rest on
  source and tests, except the one keyboard measurement taken against an
  isolated probe app.
- **Single host only.** macOS 15.7.7 on Apple M1 Pro. The `x86_64` target is
  built but was not run.
