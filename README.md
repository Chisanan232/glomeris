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

Prebuilt macOS release artifacts will be published under
[Releases](https://github.com/Chisanan232/glomeris/releases) once available.

## Quick start

```sh
glomeris detect
glomeris scan
glomeris free --target 10GB
```

See the [CLI reference](https://chisanan232.github.io/glomeris/cli_reference.html)
for the full, current command surface — it is still evolving.

## Docs

Full documentation (safety model, evidence model, emergency mode, BYOK
configuration, architecture) is published at
[chisanan232.github.io/glomeris](https://chisanan232.github.io/glomeris/).

## License

Apache-2.0 — see [LICENSE](LICENSE).
