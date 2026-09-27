# Quick Start

A short tour of the commands worth running first. The complete list, with
every flag and exit code, is in [CLI Reference](cli_reference.md) — and in the
binary itself, which is the better habit: `glomeris --help`, then
`glomeris help <command>` for any one of them.

If you would rather click than type, the menu-bar app covers detect, explain,
clean and AI Plan over the same binary — see [Menu Bar App](menu_bar_app.md).

## Find out where to start

```sh
glomeris
```

Prints the version and one line pointing at `glomeris --help`. Nothing else;
a bare invocation reads no disks and changes nothing.

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

Set a goal for how full the disk should end up, and let the loop work toward
it:

```sh
glomeris free --goal-used-percent 60
```

Or state the same thing as a free-space floor, which is what the loop itself
works in:

```sh
glomeris free --target 10GB
# or
glomeris free --target 15%
```

Exactly one of the two is required, and they are different axes:
`--goal-used-percent` is target disk **used**, `--target` is a **free-space**
floor. `--goal-used-percent 60` and `--target 40%` ask for the same end state.
`--target` accepts either an absolute size (`B`/`KB`/`MB`/`GB`/`TB`,
binary/1024-based) or a percentage of total capacity (`0`–`100`, suffixed
`%`). A `--goal-used-percent` that is not an improvement on your current usage
is refused before anything is deleted, rather than run and reported as a
success.

**This one deletes.** An earlier version of this page said no real deletion
could complete through this path; that stopped being true in HORO-994. Read
[Safety Model](safety_model.md) first, and note that `free` declines every
`ASK` candidate rather than prompting (see
[Known Limitations](known_limitations.md)) — so it reclaims only what policy
classified `AUTO_SAFE` on its own.

To see what reaching the goal would take without touching anything:

```sh
glomeris free --goal-used-percent 60 --dry-run
```

That prints current usage, the free bytes still needed, and the estimated
reclaimable opportunity split by what policy would actually permit. It takes
no execution lock and mutates nothing. `glomeris clean --dry-run` remains the
way to see a plan for one pass rather than a goal.

## Run unattended, inside limits you grant

```sh
glomeris autopilot                                  # what am I allowing today?
glomeris autopilot enable --kinds node_modules \
    --max-actions 1 --max-bytes 1073741824
glomeris autopilot run --dry-run                    # the real bounded plan
```

The one command that acts without you watching, so the grant is written to a
file you can read rather than inferred. It grants nothing until you `enable`
it, `--kinds` is required, and `--max-bytes` is a plain byte count. `AUTO_SAFE`
only unless you pre-authorise one specific `ASK` reason on one specific kind;
`PROTECTED` refuses unconditionally and no flag here changes that. See
[Autopilot](autopilot.md).

## Try emergency mode (macOS only)

```sh
glomeris emergency
```

Takes no arguments by design, and acts machine-wide on everything it finds
`AUTO_SAFE`. See [Emergency Mode](emergency_mode.md) for exactly what it does
and does not do before running it on a machine you care about.
