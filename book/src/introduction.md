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
  system. Everything runs locally as a CLI binary and (optionally) a
  per-user `launchd` agent.
- **Not a generic system optimizer.** Glomeris only understands a fixed,
  named set of developer-tool-owned resource kinds (Cargo, npm/pnpm/yarn,
  Homebrew, Xcode, Docker). It does not attempt to clean arbitrary "junk"
  files or ordinary user documents.
- **Not an automatic deleter of ordinary user documents.** Every resource
  kind Glomeris knows about is a build/tool cache with a defined owning
  tool; an unrecognized resource kind (`ResourceKind::Unknown`) is
  classified `PROTECTED` unconditionally rather than falling through to any
  default treated as safe.
- **No SwiftUI app in this MVP.** Everything documented here is the `glomeris`
  command-line binary and its optional `launchd` background daemon. A GUI is
  out of scope for the current milestone.

## Project status

This is an experimental MVP, not a stable release — see
[Known Limitations](known_limitations.md) for a specific, current accounting
of what is and is not implemented. The CLI surface described in this book
reflects `main`'s actual `match` arms as of this writing and is still
evolving (a dedicated CLI-ergonomics ticket may extend it further without
changing the safety model above).
