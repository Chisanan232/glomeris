# Safety Model

## The three-class decision

`policy::classify()` (`src/policy/engine.rs`) is the single deterministic
module that decides `AUTO_SAFE` / `ASK` / `PROTECTED` for a resource. It is
pure: no I/O, no ambient clock (`now` is always a parameter) — which is what
makes fail-closed behavior testable and lets the executor call the exact
same function again on freshly re-collected evidence at deletion time.

```rust
pub enum PolicyClass {
    AutoSafe,
    Ask,
    Protected,
}
```

There is no separate "Unknown" fourth outcome. Missing, stale, or failed
evidence maps *inside* `classify` to `Ask` or `Protected` with a specific
reason code — it never becomes a state a caller might misread as "not
Protected, therefore fine."

## `classify()`'s fail-closed ordering

`classify` checks conditions in this exact order, and the order is itself
part of the safety guarantee:

1. **Unknown resource kind** → unconditionally `Protected`. Never falls
   through to any evidence-based judgment.
2. **Path/pattern-based protected check** (see below) → `Protected`, checked
   *before* any freshness/completeness logic. A `Protected` classification
   never depends on evidence freshness — a stale-but-protected resource is
   still `Protected`, never "upgraded" by fresher evidence.
3. **Staleness** — evidence older than `PolicyConfig::max_evidence_age`
   (default 5 minutes), or whose `collected_at` is somehow in the future →
   `Ask` + `EvidenceStale`.
4. **Completeness** — `Failed` → `Ask` + `EvidenceProbeFailed`; `Partial` →
   `Ask` + `EvidenceIncomplete`.
5. **Active-use signals** (most-significant first) — an open file handle or
   matching process cwd → `ResourceInActiveUse`; a dirty or linked git
   worktree → `GitWorktreeDirty`; a live owning-tool daemon →
   `OwningToolLive`. Any of these → `Ask`.
6. **Per-instance regenerability** — `NotRegenerable` → `Ask` +
   `RebuildCostHigh`, regardless of how clean the rest of the evidence looks.
7. Otherwise → `AutoSafe`, with reasons `EvidenceFreshAndComplete` (+
   `RegenerableByTool` if applicable) + `NoActiveUseObserved`.

## The `PROTECTED` matcher

`policy::protected::protected_reason()` is an explicitly **conservative,
MVP-scope, non-exhaustive** denylist, not a claim of completeness. It
performs no filesystem I/O (no `canonicalize`, no `read_link`, no
`metadata`) — it only inspects the path/locator text already carried by the
resource, which is what keeps `classify` pure. What it actually matches
today:

- **Docker image cache** — every `DockerImageCache` resource is treated as a
  possible persistent volume and classified `Protected` unconditionally,
  because detectors do not yet distinguish a persistent volume from
  disposable image cache. (`DockerBuildCache` is *not* covered by this rule.)
- **Credential material** — any path component `.ssh` or `.gnupg`, or a
  filename ending `.pem`/`.key`, or containing `credentials`.
- **Git internals** — any path with a `.git` path component (not merely
  "inside a git-tracked project," which is the separate,
  evidence-driven `GitWorktreeDirty` reason).
- **Infra state** — a `.terraform` path component, or a filename ending
  `.tfstate`/`.tfstate.backup`.
- **System paths** — `/System`, `/usr` (except `/usr/local`), `/bin`,
  `/sbin`, `/private/var/db`.
- **Unsafe mount/symlink targets** — `/Volumes`, `/dev`, `/Network`, `/net`.

This is a starting denylist to extend, not a guarantee that every dangerous
path is covered.

## `ASK` and consent

`policy::approval::authorize()` is the *only* way to construct an `Approval`
— the type an executor is required to hold before acting. It is built with a
private, unconstructible-outside-the-module marker type, so nothing outside
`approval.rs` can fabricate one.

- `AutoSafe` → always authorizes, no consent needed.
- `Protected` → never authorizes, unconditionally, with no override
  parameter that can change that.
- `Ask` → authorizes only if a matching `UserConsent` is supplied: the
  consent's resource identity *and* fingerprint must match exactly. Consent
  granted for one resource instance does not carry over to a different
  instance at the same path, or to the same resource after its underlying
  fingerprint has changed.

**There is no interactive prompt implemented in this MVP.** The CLI's
recovery loop (`glomeris free`) runs with `auto_approve_ask: false` — `Ask`
candidates are reported as declined/skipped, never executed. A future ticket
(HORO-955) owns real interactive approval UX.

## Deletion-time TOCTOU revalidation

`executor::execute()` never trusts a previously computed `PolicyDecision` at
face value. Immediately before mutating anything, it:

1. Snapshots the resource's live filesystem identity via `symlink_metadata`
   (never `metadata`, so a symlink is detected as a symlink, never resolved
   through) — captured *before* the slower correlation re-probe runs, so a
   symlink swap during that window can't backdate the anchor.
2. Re-collects evidence from scratch (re-probes size/mtime/fingerprint
   directly, re-runs the same `EvidenceCollector`).
3. Aborts (`ResourceIdentityChanged`) if the fresh fingerprint doesn't match
   the one the approval was granted against.
4. Re-runs `classify()` on the fresh evidence.
5. Aborts (`PolicyClassDowngraded`) if the class no longer matches what was
   approved.
6. Aborts (`PolicyReasonsWidened`) if any *new* reason appears that wasn't
   present at approval time — `Ask` is a heterogeneous bucket, so consent
   granted against `RebuildCostHigh` does not cover a freshly observed
   `ResourceInActiveUse`.
7. Aborts (`EvidenceDegraded`) if completeness got worse since approval — a
   defensive, forward-looking guard.
8. Only then builds the actual plan from the same fresh evidence just
   validated, and runs it. Immediately before any `RunTool`/`DeletePath`
   step actually mutates the filesystem, the resource's identity is
   re-verified one more time against the early snapshot from step 1.

A plan with more than one step is refused outright rather than executed,
because partial-deletion byte accounting has no way to report a correct
total if a later step fails after an earlier one already succeeded. No
registered action emits more than one step today.

## AUTO_SAFE is end-to-end reachable through real execution

Detectors populate `reclaimable_bytes` at discovery time (HORO-992), and the
deletion-time revalidation path (`executor::build_fresh_evidence`) reuses
the same bounded recursive size estimate (HORO-1016) for `reclaimable_bytes`
that it already used for `logical_bytes` (HORO-994) — it no longer hardcodes
the field back to `Unavailable`. A real `AutoSafe` approval built from a detector's
evidence genuinely survives revalidation and executes for real, proven by
`tests/golden_chain_execute.rs`. See [Known Limitations](known_limitations.md)
for what's still out of scope (Docker build cache's own completeness gap).

## Actions never receive raw commands

Every registered `Action` produces a typed `ActionPlan`/`ActionStep` from
`Evidence` — never a caller-supplied string. `ActionStep::RunTool` invokes
one of a closed set of binaries (`ToolBinary::{Brew,Cargo,Npm,Pnpm,Yarn}`) via
argument-array `Command::new(tool).args(args)` calls — never a shell string.
`ActionPlan`/`ActionStep` deliberately never derive `Deserialize`, so no
external input (network payload, LLM text) can ever materialize one directly;
the only way external input reaches execution is by selecting a
pre-registered `ActionId`, whose real `Action::plan` implementation then
decides the actual steps from `Evidence` it independently trusts. See
[BYOK LLM Planner](byok.md) for how this applies to the optional LLM path
specifically.
