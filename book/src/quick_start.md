# Quick Start

The commands below are the actual top-level subcommands `src/main.rs`
dispatches on today. This surface is still evolving — check
[CLI Reference](cli_reference.md) and `main.rs` itself for the current,
authoritative list before relying on any specific flag.

## See the version

```sh
glomeris
```

With no arguments, it just prints `glomeris <version>` and exits.

## Discover what tool caches exist on this machine

```sh
glomeris detect
```

Runs every built-in detector (Xcode DerivedData, Homebrew cache, Cargo target
dirs, `node_modules`, Docker build cache) once and prints one line per
detector: `found (<N> evidence)`, `tool_absent`, or `failed: <reason>`.
`tool_absent` is a normal, expected state — it means that tool isn't
installed or has no cache yet, not an error.

## Scan a directory for the largest entries

```sh
glomeris scan [path] [top_k]
```

Both arguments are positional, not flags, and both are optional: `path`
defaults to `.`, `top_k` defaults to `20`. This walks the tree without
buffering it fully in memory and prints the top-K largest entries by
logical size. It is a generic size scan, unrelated to the detectors above —
see [Architecture](architecture.md) for how the two differ.

## Try the bounded recovery loop (macOS only)

```sh
glomeris free --target 10GB
# or
glomeris free --target 15%
```

`--target` is required and accepts either an absolute size (`B`/`KB`/`MB`/
`GB`/`TB`, binary/1024-based) or a percentage of total capacity (`0`–`100`,
suffixed `%`). See [Safety Model](safety_model.md) and
[Known Limitations](known_limitations.md) before running this against a real
machine — as of this ticket, no detector-produced candidate can currently
complete a real deletion through this path (see Known Limitations for why).

## Try emergency mode (macOS only)

```sh
glomeris emergency
```

See [Emergency Mode](emergency_mode.md) for exactly what this does and does
not do today.
