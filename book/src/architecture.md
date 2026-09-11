# Architecture

## Module map

From `src/lib.rs`, the crate's `pub mod` declarations:

```rust
pub mod actions;
pub mod detectors;
pub mod emergency;
pub mod evidence;
pub mod executor;
pub mod monitor;
pub mod platform;
pub mod policy;
pub mod scanner;
```

`platform::macos` is itself gated `#[cfg(target_os = "macos")]` — it does
not exist in a build on any other OS. `src/main.rs` is a thin CLI shim over
this library; the pure logic in `monitor`/`scanner` and their unit tests do
not depend on being reachable from a binary at all.

| Module | Responsibility |
|---|---|
| `monitor` | Disk-pressure state machine and the polling loop that drives it. Cheap, O(1) capacity checks only (`statvfs`) — never walks a directory tree. See [Pressure Model](pressure_model.md). |
| `scanner` | A generic, bounded, streaming filesystem walker that reports the top-K largest entries by size. Unrelated to the detectors below — it has no notion of resource kind, regenerability, or policy. |
| `detectors` | Bounded, shallow, per-tool probes (Cargo, Homebrew, Node, Xcode, Docker) of known roots — not a full filesystem walk. Produces discovery-stage `Evidence` with the four correlation fields always `Unavailable(NotAttempted)`. |
| `evidence` | The `Evidence`/`ProbeOutcome`/`Completeness` domain model (`evidence::model`, `evidence::probe`), plus the `correlate` submodule that fills in the four correlation fields via `lsof`/`git`/`pgrep`. See [Evidence Model](evidence_model.md). |
| `policy` | The single deterministic `classify()`/`authorize()` pair that decides `AUTO_SAFE`/`ASK`/`PROTECTED`. See [Safety Model](safety_model.md). |
| `actions` | Typed, pre-registered cleanup actions (`ActionRegistry`) that turn `Evidence` into a closed `ActionPlan`/`ActionStep` — never a caller-supplied string. Also home to the optional BYOK LLM planner (`actions::llm`). |
| `executor` | Dry-run, real execution with deletion-time TOCTOU revalidation, and the bounded closed-loop recovery orchestration (`executor::recovery_loop`, `glomeris free`). |
| `emergency` | The degraded, AUTO_SAFE-only, no-LLM/no-network recovery path (`glomeris emergency`). |
| `platform::macos` | The only place real OS-level I/O happens: `statvfs` (disk stats), `osascript` (notifications), `launchd` (daemon lifecycle). |

## Data flow

```text
Observe (monitor)
    -> statvfs, classify pressure, debounce, notify/persist (best-effort)

Discover (scanner | detectors)
    -> scanner: generic top-K-by-size walk (no resource semantics)
    -> detectors: bounded per-tool probes -> discovery-stage Evidence
       (correlation fields still Unavailable(NotAttempted))

Correlate (evidence::correlate)
    -> lsof / git / pgrep, per-field timeout and failure handling
    -> fills open_by_process, process_cwd_match, git_state, tool_liveness

Decide (policy::classify / policy::approval::authorize)
    -> AUTO_SAFE | ASK | PROTECTED, fail-closed ordering
    -> Approval is the only thing an executor may act on

Plan (actions::Action::plan)
    -> typed ActionPlan/ActionStep from Evidence
    -> optional: actions::llm ranks candidates, never authorizes them

Execute & revalidate (executor::execute)
    -> re-collect evidence, re-run classify, abort on any disagreement
    -> re-verify filesystem identity immediately before mutating
    -> re-measure actual reclaimed bytes after the fact
```

`executor::recovery_loop` (the orchestration behind `glomeris free`) and
`emergency` are both thin callers that wire the above stages together,
one candidate at a time, without reimplementing or loosening any of them —
neither introduces its own execution or authorization primitive.

## Why the scanner and the detectors are separate

The scanner (`scanner::walker`) is a generic, dependency-free (`std`-only),
non-recursive directory walker with no concept of what it finds — it reports
size and depth only. The detectors are the opposite: narrow, tool-aware
probes of specific known locations (e.g. `~/Library/Developer/Xcode/DerivedData`,
`brew --cache`'s reported path, `<project>/target`) that produce
policy-relevant `Evidence`. `glomeris scan` exercises the former; `glomeris
detect`, `glomeris free`, and `glomeris emergency` all exercise the latter.
