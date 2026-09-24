# Known Limitations

This page collects specific, verified limitations pulled from the actual
code and from the PRs that introduced each piece — not a generic disclaimer.
This is an experimental MVP; treat every claim elsewhere in this book as
scoped by what's on this page.

## Resolved: real detector-produced candidates now complete real deletions

Fixed in HORO-994. Detectors populate `Evidence::reclaimable_bytes` at
discovery time (HORO-992) — Xcode/Cargo/Node/Homebrew via a real size
estimate, Docker via `docker system df`'s own reported figure — and
`executor::execute()`'s deletion-time TOCTOU revalidation
(`executor::build_fresh_evidence`) now reuses the same size-estimate
computation for `reclaimable_bytes` that it already used for
`logical_bytes`, rather than hardcoding it back to `Unavailable`. A golden
end-to-end integration test (`tests/golden_chain_execute.rs`) proves the
full chain — real detector → `AutoSafe` classification → `Approval` →
`execute()` → real deletion → real re-measured freed bytes — against a
disposable fixture. `glomeris free --target` and `glomeris emergency` can
both now actually free real bytes, not just report a dry-run plan.

## Resolved (HORO-1016): the size estimate was non-recursive and badly under-counted nested trees

Before this fix, `detectors::shallow_logical_bytes` (the shared size probe
behind every detector's `logical_bytes`/`reclaimable_bytes` and behind
`executor::build_fresh_evidence`'s revalidation) summed only a directory's
*immediate* entries — for a subdirectory entry it counted the directory
inode's own size, never its contents. On a real machine this reported a
4.8 GB Xcode DerivedData tree as 34.9 KB, a 1.0 GB Homebrew cache as 1.3 MB,
and a 16 GB Cargo `target/` directory as 4.6 KB. Fixed:
`detectors::estimate_logical_bytes` walks the full subtree via an explicit
stack (never real recursion, so an arbitrarily deep tree cannot overflow
the stack), bounded by a shared 200,000-entry / 750ms budget
(`detectors::size_estimate_budget`) used identically at discovery time and
at deletion-time revalidation. Budget exhaustion always yields a truthful
partial-sum lower bound — never `Unavailable` — so a truncated walk can
never look like a probe failure to `Evidence::completeness()` or trip a
spurious abort in `executor::execute`'s TOCTOU revalidation; when the walk
does stop early, a provenance note is attached via `Evidence::push_source`
(advisory only, never a policy input). `tests/reclaimable_bytes_reaches_auto_safe.rs`
and the new `executor::tests::estimate_matches_total_size_best_effort_on_the_same_tree`
lock the estimate's byte semantics to `executor::total_size_best_effort`'s
existing convention (files and symlinks counted by their own size,
directories contribute 0). On a real developer machine the 750ms deadline
truncates the Xcode DerivedData walk at roughly 42,000 entries, so that
resource's reported bytes are normally a lower bound, by design — no
extrapolation is attempted.

## Docker build cache can never reach `Completeness::Complete`

`DockerBuildCache`/`DockerImageCache` resources use `ResourceLocator::Tool`
(a tool-native id, not a filesystem path) because Docker's build cache has
no single canonical path. `DefaultEvidenceCollector` can only run
`tool_liveness` for a `Tool`-locator resource — `open_by_process`,
`process_cwd_match`, and `git_state` always come back
`Unavailable(NotAttempted)` for it, and `required_evidence()` still requires
all three for `Complete`. `AutoSafe` for Docker build cache is out of reach
until a future ticket gives this resource kind a real path-based or
tool-native correlation strategy.

## Docker image cache is unconditionally `Protected`; Docker has no registered cleanup action

Detectors do not yet distinguish a persistent, named Docker volume from
disposable image cache, so every `DockerImageCache` resource is classified
`Protected` unconditionally (see [Safety Model](safety_model.md)). Separately,
Docker (both build cache and image cache) has no registered `Action` at all
— it stays detect-only, per an accepted design cut. Neither of these is
accidental; both are documented design decisions, not bugs.

## Resolved (HORO-957): Homebrew's cleanup action is registered but never actually executes

`HomebrewCleanupCache`'s `ActionStep::RunTool` has `scoped_path: None`
because there is no narrower, safe way to ask `brew` to clean only one
thing — real `brew cleanup -s` has no path argument to scope to. HORO-957's
independent golden-scenario evaluation found that, before this fix, that
meant `executor::execute()` ran this step with **zero identity/TOCTOU
guard**, and confirmed the real Homebrew cache classifies `AutoSafe` on a
real developer machine today — a live risk that `emergency`/`free --target`
could silently trigger a real, irreversible `brew cleanup -s`. Fixed:
`execute()` now refuses any `RunTool` step with `scoped_path: None`
unconditionally, fail-closed, before ever spawning the tool. `dry_run`/
`clean --dry-run` still render this action's plan; real execution stays
permanently refused unless a scoped equivalent becomes available upstream.
HORO-1358 carried that refusal upstream into reporting: `detect --json` and
`explain --json` now report this candidate as `executable: false` with no
offered action, because reporting puts the action's own plan to the same
pre-mutation structural rule `execute()` applies. HORO-1360 moved that rule
into one shared predicate (`actionability`) and pointed two more surfaces at
it: the BYOK LLM prompt no longer names `homebrew.cleanup.cache` among a
resource's offered action ids, and `autopilot` reports such a candidate as
*ineligible* without spending one of its bounded attempts, rather than
charging an attempt and reporting an execution failure that was certain in
advance. `clean --dry-run`, `free` and `emergency` still resolve actions
independently and do not read the predicate, so they can still nominate
`homebrew.cleanup.cache` — where it remains refused at execution. That
remainder is `HORO-1359`.
See `HORO-1005` for a related, non-blocking follow-up (`scoped_path` isn't
yet structurally tied to what a `RunTool` step's `args` actually mutate —
not currently exploitable, since this was the only unscoped action and it's
now refused outright).

## Resolved (HORO-957): Cargo `target/` and `node_modules` are now discoverable

Before this fix, `DiscoveryContext::known_project_roots` defaulted to an
empty list and was never populated by any real CLI code path — so the two
most canonical "developer storage hotspots" named in the epic were
structurally undiscoverable no matter what was actually on disk. Fixed: a
repeatable `--project-root <path>` flag is now wired into `detect`/
`explain`/`clean`/`free` (deliberately not `emergency`, which takes no
arguments by design).

## `glomeris free` still declines every `ASK` candidate

This section used to say `Ask` had no handling at all, because no interactive
prompt existed. Narrower than that now: `glomeris execute --confirm-ask
--observed-fingerprint <token>`, the menu-bar app's Clean button, and
Autopilot's `--preauthorize-ask` all supply a real `UserConsent` — see
[Safety Model](safety_model.md).

What remains is specific to one command. `glomeris free`'s recovery loop wires
`RecoveryConfig::auto_approve_ask: false` (`src/main.rs`), so every
`Ask`-classified candidate inside that loop is reported as declined/skipped
and never executed, however much of the target it would have reclaimed. There
is still no TTY prompt anywhere in this codebase; a `free` run cannot ask you
mid-loop, so it does not ask at all. To act on an `Ask` candidate, use
`explain --json` to read its `fingerprint_token` and then `execute`.

## A pre-authorized `ASK` under Autopilot cannot complete (HORO-1310)

Autopilot's `--preauthorize-ask <kind>:<reason>` grants narrow advance consent
for one `ASK` reason on one kind. The gate admits such a candidate,
`policy::approval::authorize` issues a real `Approval` for it, and then
`executor::execute`'s deletion-time revalidation *always* aborts it with
`AbortReason::PolicyClassDowngraded`. Nothing is deleted.

The cause is upstream of Autopilot and shared by every deleting command.
`Ask`/`RebuildCostHigh` arises only from a **per-instance**
`Regenerability::NotRegenerable`, while `executor::build_fresh_evidence`
rebuilds regenerability from the resource *kind*'s static default — so the
fresh classification lands on `AutoSafe` and the class comparison trips.

Left as-is deliberately: the failure direction is the safe one (refuse, mutate
nothing), and changing `build_fresh_evidence` changes the TOCTOU anchor
`execute`, `free`, `emergency` and `autopilot run` all depend on. It is pinned
by `src/autopilot/run.rs`'s
`a_preauthorized_ask_still_aborts_at_deletion_time_revalidation`, so the fix
starts from a failing test that names the cause. See
[Autopilot](autopilot.md).

## Correlation depends on `lsof`/`git`/`pgrep` being present and stable

Runtime correlation is subprocess-based. CI validates this against
`macos-14` only, where all three tools are confirmed present; behavior on
other macOS versions is not separately verified. `tool_liveness` for
non-daemon tools (`Cargo`, `Npm`, `Pnpm`, `Yarn`, `Homebrew`) is structurally
always `Unavailable(ToolNotRunning)` — there is no persistent process whose
presence would mean "this tool is active" for them, which caps their
`Evidence::completeness()` at `Partial`/`Confidence::Medium`, never
`Complete`/`High`. This is a deliberate fail-closed consequence, not an
anomaly to "fix."

## `launchd` real-scheduling behavior is not exercised by `cargo test`

Only plist generation and path/file logic are unit tested. Actually asking
`launchd` to load and run the agent on a schedule needs a real macOS user
session and is not covered by CI.

## The protected-path matcher is conservative, not exhaustive

See [Safety Model](safety_model.md) — the denylist covers a specific, named
set of categories (credential material, git internals, infra state, system
paths, unsafe mounts) and is explicitly documented in its own module comment
as a starting point to extend, not a completeness guarantee.

## Resolved (HORO-1008): the BYOK LLM planner now has a CLI surface, and PROTECTED refusal is proven end-to-end

`glomeris llm-plan [--project-root <path>]... [--plan-file <path>] [--json]`
is an advisory, non-executing subcommand wired on top of `actions::llm` —
see [BYOK LLM Planner](byok.md) and [CLI Reference](cli_reference.md). It
never constructs a `policy::Approval` and never calls
`policy::approval::authorize` or `executor::execute`.

`--plan-file <path>` feeds a fixture response through the exact same
`extract_plan`/`LlmPlan`/`plan_with_llm` pipeline the live provider uses,
with no network call. `tests/golden_llm_plan_protected_refusal.rs` uses
this to prove, through the real CLI-facing `crate::cli::build_llm_plan_report`
function, that a plan request to delete SSH key material is refused —
closing the gap the previous version of this section described:
HORO-943's golden acceptance scenario step 6 ("prove a protected resource
cannot be deleted even if an LLM plan requests it") was previously verified
at the code level only (`llm_plan_item_never_bypasses_policy`), never
through an actual CLI input surface.

Remaining, deliberate scope cuts for this subcommand specifically:

- Advisory-only, by design — no `--execute`/`--yes` flag exists or is
  planned for this subcommand.
- No retry/backoff, and no streaming — mirrors `actions::llm`'s own
  existing limitations (see [BYOK LLM Planner](byok.md)).
- Only one provider shape (`OpenAiCompatibleProvider`, any
  OpenAI-compatible `/chat/completions` endpoint) — no Anthropic-native or
  Azure-OpenAI-specific auth.
- Not wired into `glomeris free`'s recovery loop — that remains a future
  ticket's optional enhancement, per `actions::llm`'s own module docs.

Separately, and independently of the LLM planner: a previous version of this
section claimed that no live detector could ever emit a resource whose path
matches a `policy::protected` pattern, and that `PolicyClass::Protected` was
therefore enforced at the code level but not reachable by an evaluator
driving only the shipped product's *detectors*. The first half of that is
wrong, and HORO-1313's release-gate pass disproved it on a disposable
fixture.

What a protected pattern matches is a *path component*, not an installed
location, so anything a detector can discover underneath one is protected.
A real `node_modules` tree created under a path containing an `.ssh`
component was found by the live Node detector, classified `Protected` with
reason `protected_credential_material` and `executable: false`, and then
refused by the real executor twice — once by a plain `execute`, once with a
valid `--confirm-ask` and matching `--observed-fingerprint` — exiting 3 both
times with nothing deleted.

So the accurate statement is narrower, and the safety conclusion is
stronger rather than weaker. The conventional locations of the tool caches
Glomeris knows about do not normally sit under a protected path, which is
why `Protected` is uncommon in day-to-day use; a project root that does sit
under one reaches it through ordinary discovery, and the refusal holds when
it happens. `--project-root` is the usual way to get there, deliberately or
by accident.

`tests/golden_llm_plan_protected_refusal.rs` still reaches `Protected`
through a hand-built `Evidence` fixture, and that remains the right shape
for a hermetic test — it does not depend on a tree existing on the machine
running CI.

## Resolved (HORO-957): prebuilt release artifacts

`cargo-dist` packaging produces macOS artifacts for `aarch64-apple-darwin`
and `x86_64-apple-darwin`, published as GitHub Release assets with
checksums. Building from source remains fully supported.

## Resolved (HORO-1305 through HORO-1309): there is a GUI

This page used to say a SwiftUI app was not part of this MVP. There is one:
a `LSUIElement` menu-bar app, documented in [Menu Bar App](menu_bar_app.md).

What has not changed is where authority lives. The app is a thin client over
the same CLI — it shells out to `glomeris` and renders what comes back. It
classifies nothing, decides nothing, and holds no policy logic, which a CI
guard (`scripts/check-no-policy-label-branching.sh`) enforces mechanically
rather than by convention. Every screen maps back to a named CLI invocation;
that table is at the end of the Menu Bar App page.

What the app offers no way to invoke is *running* something without being
asked: `glomeris emergency` has no button, and neither does `glomeris autopilot
run`. Both still show up in the app's history when run from a terminal, which
is the point of a shared audit trail.

Autopilot's **grant** is a different question, and this page used to get it
wrong too: it said the envelope could only be granted or revoked on the command
line. That is no longer true. Settings → Autopilot reads
`autopilot show --json` and writes through `autopilot enable`/`autopilot
revoke`, so the one authorization in this product that lets something delete
without asking again can be read and withdrawn by someone who never opens a
terminal — see [Menu Bar App](menu_bar_app.md#preferences--autopilot). The
distinction the app keeps is between authorizing and acting, not between the
terminal and the GUI.

## What holds this book to the code

Three mechanical checks, because the drift this page is about was found by
reading rather than by CI:

- `tests/help_golden.rs` pins every rendered help surface byte for byte
  against committed fixtures, so a command's own help text cannot change
  silently.
- `scripts/check-docs-cover-cli-commands.sh` requires
  [CLI Reference](cli_reference.md) to have a section for every command in
  `src/cli/help.rs`'s single `COMMANDS` table. A new subcommand now fails CI
  until it is documented.
- `scripts/check-vocabulary-covers-cli-tokens.sh` compares the CLI's JSON
  tokens against the menu-bar app's wording for them, in both directions.

None of that can catch prose that goes stale, which is what the rest of this
page is for. Where this book and `src/main.rs` disagree, the source is right
and the disagreement is a bug on this page.
