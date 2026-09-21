# Introduction

Glomeris is an evidence-first, policy-constrained developer storage autopilot
for macOS. When a machine enters disk pressure, Glomeris discovers meaningful
reclaimable storage (Cargo target dirs, `node_modules`, Homebrew's cache,
Xcode DerivedData, Docker's reported build cache), explains why a resource is
or is not safe to remove, and executes only policy-approved cleanup actions —
re-measuring actual freed bytes rather than trusting an estimate.

## The canonical safety invariant

> **AI can recommend. Policy decides. Executor verifies. Filesystem reality
> wins.**

Concretely, as implemented today:

- An optional BYOK LLM planner (`glomeris::actions::llm`) may rank and explain
  candidates, but it can only select from a closed, typed set of pre-registered
  action IDs and real resource IDs the crate already found — it can never
  compose a raw command or an arbitrary path (see
  [BYOK LLM Planner](byok.md)).
- A single deterministic module (`glomeris::policy::engine::classify`) decides
  `AUTO_SAFE` / `ASK` / `PROTECTED` for every resource. Nothing upstream of it,
  including any LLM output, can bypass that decision (see
  [Safety Model](safety_model.md)).
- The executor (`glomeris::executor::execute`) never trusts a previously
  computed decision at face value: it re-collects evidence and re-runs
  `classify` immediately before mutating anything, and aborts rather than acts
  if the fresh read disagrees with what was approved.
- Every execution re-measures actual reclaimed bytes after the fact rather
  than assuming the estimate was correct.

## What Glomeris is not

- **Not cross-platform.** The MVP targets macOS only. There is no Windows or
  Linux support, and the `platform::macos` module is compiled out entirely on
  other operating systems.
- **Not a cloud service.** There is no backend, no telemetry, and no account
  system. Everything runs locally: a CLI binary, an optional per-user
  `launchd` agent, and an optional menu-bar app. The one thing that can leave
  your machine is a BYOK LLM request you configure yourself, to an endpoint
  you name — bounded and previewable (see [BYOK LLM Planner](byok.md)).
- **Not a generic system optimizer.** Glomeris only understands a fixed,
  named set of developer-tool-owned resource kinds (Cargo, npm/pnpm/yarn,
  Homebrew, Xcode, Docker). It does not attempt to clean arbitrary "junk"
  files or ordinary user documents.
- **Not an automatic deleter of ordinary user documents.** Every resource
  kind Glomeris knows about is a build/tool cache with a defined owning
  tool; an unrecognized resource kind (`ResourceKind::Unknown`) is
  classified `PROTECTED` unconditionally rather than falling through to any
  default treated as safe.
- **Not a GUI with authority of its own.** There *is* a SwiftUI menu-bar app
  (see [Menu Bar App](menu_bar_app.md)) — an earlier version of this page said
  there was not. It is a thin client: it shells out to the same `glomeris`
  binary and renders what comes back. It classifies nothing and decides
  nothing; a CI guard (`scripts/check-no-policy-label-branching.sh`) fails the
  build if Swift code branches on a policy label, so the one way the GUI could
  quietly grow its own safety opinion is checked rather than trusted.

## Two meanings of "autopilot"

Both are used in this book, so they are worth separating once:

- The **product** is a storage autopilot in the sense above — it discovers,
  explains and verifies on its own rather than asking you to audit paths by
  hand. That is what the first paragraph means.
- **`glomeris autopilot`** is one specific command: an unattended run inside a
  policy envelope you set on the command line — byte, action, time and
  resource-kind limits, `AUTO_SAFE` only unless you pre-authorise otherwise.
  See [Autopilot](autopilot.md).

`glomeris autopilot run` is the only thing that *deletes* unattended.
`detect`, `explain`, `clean`, `execute`, `free` and `emergency` all need you to
invoke them. The optional `launchd` agent does run unattended, but it only
polls disk pressure and notifies — it executes no action and never touches the
filesystem it is watching (see [Daemon Lifecycle](daemon_lifecycle.md)).

## Project status

This is an experimental MVP, not a stable release — see
[Known Limitations](known_limitations.md) for a specific, current accounting
of what is and is not implemented.

The command set described here is held to the code mechanically:
`tests/help_golden.rs` pins every help surface byte for byte, and
`scripts/check-docs-cover-cli-commands.sh` fails CI if a command ships without
a section in [CLI Reference](cli_reference.md). Prose can still go stale, which
is what [Known Limitations](known_limitations.md) is for.
