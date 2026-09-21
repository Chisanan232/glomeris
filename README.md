# Glomeris

Evidence-first, policy-constrained developer storage autopilot for macOS.

## What

When a developer machine enters disk pressure, Glomeris discovers meaningful
reclaimable storage, explains why a resource is or is not safe to remove,
avoids disrupting active work, and executes only policy-approved typed
cleanup actions — re-measuring actual freed bytes until a target is reached
or no safe action remains.

## Why

**AI can recommend. Policy decides. Executor verifies. Filesystem reality
wins.**

Glomeris is not a generic `du`, not a cache-path list, not a system
optimizer, and not an LLM that emits `rm -rf`. An optional BYOK LLM planner
may rank and explain ambiguous candidates, but it can only select from a
typed, closed set of known actions — it can never invent or execute a raw
command, and it can never upgrade a `PROTECTED` resource to safe.

## ⚠️ Safety status

This is an **experimental MVP**, not a stable release. It does not promise
universal safe deletion. Read the safety model before running any
non-dry-run cleanup.

## Install

Build from source (requires Rust/Cargo):

```sh
git clone https://github.com/Chisanan232/glomeris.git
cd glomeris
cargo build --release
```

Prebuilt macOS binaries for `aarch64-apple-darwin` and `x86_64-apple-darwin`
are published with checksums under
[Releases](https://github.com/Chisanan232/glomeris/releases).

There is also a menu-bar app (`macos/GlomerisMenuBar`), built from this repo
with Xcode. It is a thin client over the same binary — see the
[Menu Bar App](https://chisanan232.github.io/glomeris/menu_bar_app.html) page.

## Quick start

```sh
glomeris --help          # every command, grouped by what it can do
glomeris detect          # what tool caches exist here
glomeris clean --dry-run # the real plan, executing nothing
```

`detect`, `scan`, `explain`, `clean` and `llm-plan` never mutate anything.
`execute`, `free`, `emergency` and `autopilot run` delete — read the
[safety model](https://chisanan232.github.io/glomeris/safety_model.html) first.

## Docs

Full documentation is published at
[chisanan232.github.io/glomeris](https://chisanan232.github.io/glomeris/):
safety model, evidence model, emergency mode, [policy-constrained
Autopilot](https://chisanan232.github.io/glomeris/autopilot.html), BYOK LLM
configuration, the menu-bar app, architecture, and a specific accounting of
[known limitations](https://chisanan232.github.io/glomeris/known_limitations.html).

## License

Apache-2.0 — see [LICENSE](LICENSE).
