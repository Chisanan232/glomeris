# Known Limitations

This page collects specific, verified limitations pulled from the actual
code and from the PRs that introduced each piece — not a generic disclaimer.
This is an experimental MVP; treat every claim elsewhere in this book as
scoped by what's on this page.

## Resolved: real detector-produced candidates now complete real deletions

Fixed in HORO-994. Detectors populate `Evidence::reclaimable_bytes` at
discovery time (HORO-992) — Xcode/Cargo/Node/Homebrew via a real shallow
size estimate, Docker via `docker system df`'s own reported figure — and
`executor::execute()`'s deletion-time TOCTOU revalidation
(`executor::build_fresh_evidence`) now reuses the same shallow-size
computation for `reclaimable_bytes` that it already used for
`logical_bytes`, rather than hardcoding it back to `Unavailable`. A golden
end-to-end integration test (`tests/golden_chain_execute.rs`) proves the
full chain — real detector → `AutoSafe` classification → `Approval` →
`execute()` → real deletion → real re-measured freed bytes — against a
disposable fixture. `glomeris free --target` and `glomeris emergency` can
both now actually free real bytes, not just report a dry-run plan.

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

## `ASK` has no interactive handling in this MVP

`RecoveryConfig::auto_approve_ask` is `false` in the CLI's wiring — every
`Ask`-classified candidate is reported as declined/skipped, never executed,
because there is no interactive prompt implemented anywhere in this
codebase yet. Real interactive approval UX is scoped to a separate ticket
(HORO-955) and is deliberately out of scope here — this book documents the
CLI surface that exists on `main` today, not what HORO-955 may add.

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

## The BYOK LLM planner is a library capability only, and PROTECTED is unreachable through the shipped CLI

`actions::llm` has no CLI wiring, no defined environment variable name, and
is not called from `glomeris free` or any other subcommand today. See
[BYOK LLM Planner](byok.md).

Separately, and independently of the LLM planner: no live detector
(Xcode/Homebrew/Cargo/Node/Docker build cache) ever emits a resource whose
path matches any `policy::protected` pattern, or the unconditionally
`Protected` `DockerImageCache` kind — `PolicyClass::Protected` is real and
enforced at the code level (`policy::approval::authorize` unconditionally
refuses it; `Approval` is unconstructible outside that module), but it is
not reachable or observable by a real evaluator driving only the shipped
product. Two independent fresh-context golden-scenario evaluations
confirmed this. HORO-943's golden acceptance scenario step 6 ("prove a
protected resource cannot be deleted even if an LLM plan requests it") is
therefore verified at the code level only, not end-to-end through the CLI.
This is a deliberate, documented scope decision for MVP 1.0 (BYOK is listed
as optional in the epic), not a hidden gap — `HORO-1008` tracks adding a
real LLM-plan CLI/input surface and proving this scenario end-to-end for a
future version.

## Resolved (HORO-957): prebuilt release artifacts

`cargo-dist` packaging produces macOS artifacts for `aarch64-apple-darwin`
and `x86_64-apple-darwin`, published as GitHub Release assets with
checksums. Building from source remains fully supported.

## No GUI

Everything in this book is the `glomeris` CLI binary and its optional
`launchd` background agent. A SwiftUI (or any other) GUI app is not part of
this MVP.

## The CLI surface itself is still evolving

This book documents `main`'s actual `match` arms as of the time this book
was written. A separate, in-flight ticket may extend the CLI (new
subcommands, flags, or output formats) without changing anything in the
safety model described in this book. If a command described here no longer
matches `src/main.rs`, trust the source.
