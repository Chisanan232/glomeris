# Evidence Model

`Evidence` (`src/evidence/model.rs`) describes *what a detector observed
about a resource* — never what should be done about it. That judgment
belongs entirely to the [policy layer](safety_model.md).

## `Evidence`

Key fields:

```rust
pub struct Evidence {
    pub resource: ResourceId,
    pub fingerprint: ResourceFingerprint,
    pub detector: DetectorId,
    pub logical_bytes: ProbeOutcome<u64>,
    pub physical_bytes: Option<u64>,       // always None in the MVP
    pub reclaimable_bytes: ProbeOutcome<u64>,
    pub last_modified: ProbeOutcome<SystemTime>,
    pub last_accessed: ProbeOutcome<SystemTime>,
    pub regenerability: Regenerability,
    pub recoverability: Recoverability,
    pub native_cleanup: NativeCleanup,
    pub open_by_process: ProbeOutcome<Vec<ProcessRef>>,
    pub process_cwd_match: ProbeOutcome<Vec<ProcessRef>>,
    pub git_state: ProbeOutcome<Option<GitState>>,
    pub tool_liveness: ProbeOutcome<bool>,
    pub executable_dependency: ProbeOutcome<ExecutableDependencyReport>,
    pub collected_at: SystemTime,
    pub sources: Vec<String>,
}
```

`physical_bytes` is always `None` today — subtree `st_blocks` summation is
out of scope for the current milestone. `git_state`'s `Observed(None)` means
the probe ran successfully and determined the resource genuinely isn't in a
git working tree — a complete, legitimate answer, not a missing one; only
`Unavailable(reason)` means the probe itself failed.

## `ProbeOutcome<T>` — the core honesty guard

```rust
pub enum ProbeOutcome<T> {
    Observed(T),
    Unavailable(ProbeReason),
}

pub enum ProbeReason {
    ToolAbsent,
    ToolNotRunning,
    PermissionDenied,
    TimedOut,
    Failed,
    NotAttempted,
}
```

This is deliberately **not** `Result<T, E>`: there is no `Default` impl and
no `unwrap_or_default()` escape hatch. A caller that wants the observed value
must explicitly branch on this enum — there is no way to silently coerce an
unavailable probe into a zero/empty/false value. This is the structural
guard against "a failed probe becomes a safe default."

## `Completeness` and `Confidence`

```rust
pub enum Completeness {
    Complete,
    Partial { missing: Vec<EvidenceField> },
    Failed,
}

pub enum Confidence {
    High,
    Medium,
    Low,
}
```

`Evidence::completeness()` checks every field `ResourceKind::required_evidence()`
lists for that resource's kind: `Complete` if none are missing, `Failed` if
*all* required fields are missing, `Partial` otherwise. `Confidence` is
derived from completeness (`Complete → High`, `Partial` with ≤1 field missing
→ `Medium`, everything else → `Low`) and is advisory only — the policy layer
does not consume it; it exists to give a human or LLM something short to
point at.

Resource kinds whose owning tool has no persistent daemon to check
(`Cargo`/`Npm`/`Pnpm`/`Yarn`/`Homebrew`) do not require `ToolLiveness` for
`Complete`, since that field would otherwise be structurally unreachable —
see [Known Limitations](known_limitations.md) for a related caveat about
Docker build cache never reaching `Complete` at all.

## `ProbeOutcome` never masquerades as "safe"

Nothing in this codebase treats missing or failed evidence as evidence of
"nothing to clean up." A detector's own status type
(`DetectorStatus::Failed(reason)`) is documented as meaning "we don't know,"
never "nothing found," and `ResourceKind::Unknown` is a deliberate fail-closed
sink that the policy layer maps to `PROTECTED` unconditionally rather than
falling through to any default treated as safe. The actual "therefore refuse
to delete" enforcement lives in [`policy::classify`](safety_model.md), not in
this module — this module only guarantees the data can't fabricate a
misleadingly complete picture.

## Correlation: `open_by_process`, `process_cwd_match`, `git_state`, `tool_liveness`

Detectors (`detectors/`) only ever populate the discovery-stage fields
(size, mtime, regenerability). The four correlation fields above are always
`Unavailable(NotAttempted)` straight out of a detector — a separate
collector, `evidence::correlate::DefaultEvidenceCollector`, is responsible
for actually attempting correlation. This split is what keeps freshly
discovered evidence from ever reporting `Completeness::Complete` before
correlation has actually run.

`DefaultEvidenceCollector` wires in the real, subprocess-backed probes:

| Field | Backed by | Command |
|---|---|---|
| `open_by_process` | `LsofOpenFileProbe` | `lsof -F pcn +D <path>` |
| `process_cwd_match` | `LsofProcessCwdProbe` | `lsof -a -d cwd -F pcn +D <path>` |
| `git_state` | `GitCliProbe` | `git -C <path> rev-parse --show-toplevel`, then `git status --porcelain` and `git rev-parse --git-dir --git-common-dir` on the resolved repo root |
| `tool_liveness` | `PgrepToolLivenessProbe` | `pgrep -x <daemon-name>` (only for tools with a real daemon: Xcode.app, Docker's `com.docker.backend`; other tools skip the subprocess entirely and report `Unavailable(ToolNotRunning)`) |

Each probe runs under a shared per-call timeout (`ProbeBudget`) enforced by
polling `try_wait()` and killing the child on timeout to avoid leaving a
zombie process. A `collect()` call can spend up to roughly four times that
timeout in the worst case, since up to four subprocesses each get their own
full budget.

**Absence and failure handling is per-tool, not global**: a missing `lsof`,
`git`, or `pgrep` binary degrades that specific field to
`Unavailable(ProbeReason::ToolAbsent)` — it does not fail the whole
correlation pass, and it is never silently treated as "nothing found." A
non-zero exit with no output (e.g. `lsof` finding nothing) is treated as a
legitimate empty result; a non-zero exit *with* stderr content is treated as
a genuine probe failure (`Unavailable(ProbeReason::Failed)`).

### Docker's correlation gap

`DockerBuildCache`/`DockerImageCache` resources use `ResourceLocator::Tool`
(a tool-native id, not a filesystem path) because there is no single
canonical path to a Docker build cache. `DefaultEvidenceCollector` can only
run `tool_liveness` for a `Tool`-locator resource; `open_by_process`,
`process_cwd_match`, and `git_state` are always `Unavailable(NotAttempted)`
for it. Since `required_evidence()` still requires all three, Docker build
cache cannot reach `Completeness::Complete` today — see
[Known Limitations](known_limitations.md).

## Executable dependency (HORO-1825)

`executable_dependency: ProbeOutcome<ExecutableDependencyReport>` answers a
question `open_by_process` cannot: not "is something using this resource
right now", but "is something configured to exec a file inside this
resource on its *next* invocation" — a hook or daemon binary that happens
to live inside a disposable build cache survives every liveness check
until the moment it's gone and the next hook event fails.

```rust
pub struct ExecutableDependencyReport {
    pub references_inside: Vec<DependencyRef>,  // non-empty => PROTECTED
    pub running_inside: Vec<ProcessRef>,         // non-empty => ASK
    pub unresolved: Vec<UnresolvedRef>,          // non-empty => ASK (executable-bearing kinds)
    pub sources_examined: Vec<SourceTag>,        // display only, never policy input
}
```

Populated for every `ResourceLocator::Path` resource by
`evidence::correlate::host_dependency::LiveHostDependencyProbe`, which reads
a closed, recognized set of configs read-only — Claude Code's
`settings.json`/`settings.local.json`/managed settings, each
`~/Library/LaunchAgents/*.plist` (via `plutil -convert json -o -`), plus a
PATH-entry check against Glomeris's own `PATH` — tokenizes each command per
the simple-form rule `[NAME=VALUE ...] head [arg ...]`, walks up to 40
symlink hops, and matches by canonical path + device/inode, never by name.
It never executes anything it finds, never writes to host config, and never
collects argv — see `scripts/check-host-dependency-probe-is-read-only.sh`.

Two PATH-shadowing / tokenization rules are load-bearing, both added after
independent review:

- A bare command name that resolves against *Glomeris's own* `PATH` is
  never read as a clean negative (`UnresolvedRef::PathResolutionDivergent`)
  — the hook runtime's actual `PATH` may diverge and resolve the same name
  into the resource while Glomeris's own `PATH` resolves it elsewhere.
- A relative, non-absolute, non-`~/`-prefixed path argument (`./target/debug/hook`)
  has no defined resolution cwd at probe time, so it becomes
  `UnresolvedRef::NonInspectable` rather than being silently ignored.

Deliberately out of scope for HORO-1825 (see that PR's description): the
per-configured-project `claude_project(<root>)` source, Codex
`~/.codex/hooks.json` (schema unconfirmed) and `~/.codex/config.toml`'s
`notify` key (no `toml` dependency added) — a present Codex config file is
reported `unresolved` rather than parsed speculatively, per the fail-closed
contract above.
