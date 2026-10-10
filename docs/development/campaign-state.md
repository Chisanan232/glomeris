# HORO-1822 Campaign State

> **Resume from this file, not from chat history.** This is the durable record of
> where the HORO-1822 post-incident / process-aware-recovery campaign stands.
> Chat context will be lost across sessions; this file will not.

- **Owning Epic:** HORO-1043 — Glomeris MVP 2.0 — Interactive Daily Driver / Trustworthy Recovery UX
- **Active ticket:** HORO-1822 — [Glomeris][Post-Incident] Reconstruct full-disk rescue and approve process-aware recovery architecture (Story, To Do)
- **Last session state:** ADR-0001 (`docs/development/adr/0001-process-aware-recovery.md`) drafted by opus-architect, saved on branch `v0.1.0/HORO-1822/recovery_arch_adr`. Next step: independent adversarial review by a FRESH agent (not the one that drafted it), then founder approval, before any HORO-1823+ implementation starts.

Jira tickets, raw per-ticket content, and the full dependency DAG were written this
session to the scratchpad at:
```
/private/tmp/claude-501/-Users-bryant-Bryant-Developments-horonomy/ee01adf3-b6cc-46b3-b8f2-2e3538db1149/scratchpad/horo1822/jira/*.md
/private/tmp/claude-501/-Users-bryant-Bryant-Developments-horonomy/ee01adf3-b6cc-46b3-b8f2-2e3538db1149/scratchpad/horo1822/dag.md
```
That scratchpad is session-scoped and will not survive — the content is reproduced
below so this file is self-contained.

## Full ticket list with status

| Key | Type | Status | Priority | Summary |
|---|---|---|---|---|
| HVDL-26 | Idea (Polaris) | Researching | - | Glomeris — evidence-first, policy-constrained developer storage autopilot |
| HORO-1043 | Epic | (not independently re-fetched) | - | Glomeris MVP 2.0 — Interactive Daily Driver / Trustworthy Recovery UX |
| HORO-1822 | Story | To Do | High | [Post-Incident] Reconstruct full-disk rescue and approve process-aware recovery architecture — **ACTIVE** |
| HORO-1823 | Story | To Do | High | [Process Evidence] Distinguish abandoned builds from valid idle daemons without automatic kill authority |
| HORO-1824 | Story | To Do | High | [Cargo] Reconcile target-dir ownership and safe scoped cleanup across shared and local builds |
| HORO-1825 | Story | To Do | **Highest** | [Executable Safety] Protect configured agent-hook binaries residing in disposable build caches |
| HORO-1826 | Story | To Do | High | [Recovery Integrity] Verify post-cleanup function and honest degradation beyond free bytes |
| HORO-1827 | Story | To Do | Medium | [Pressure History] Observe 12h stability and honestly plan safe recovery toward 1 TiB free |
| HORO-1828 | Story | To Do | Medium | [Operator UX] Explain uncertain process ownership, blocked cleanup and rebuild blast radius |
| HORO-1829 | Story | To Do | High | [Independent QA] Prove process-aware recovery and no hook regressions on real multi-agent macOS |
| HORO-1629 | Bug | To Do | Medium (needs-founder-decision) | A configured project root that is a parent of projects yields a clean bill of health with nothing examined below it |
| HORO-1073 | Task | **In Progress** | High | MVP 2.0 founder dogfood pass #2 + release readiness record — **final release gate** |

### HORO-1629 — founder-preference flag (NOT silently resolved)

HORO-1629 is explicitly labeled `needs-founder-decision`. Its three candidate fixes
(validate at config time / report examined roots / expand one level) are described
in the ticket itself as "a product judgement, not an engineering one," and AC 5
requires founder review of the chosen wording before the ticket can be considered
satisfied. **No founder-preference statement was found in Jira** (zero comments on
the issue). The campaign brief separately states a founder preference (warn when a
configured root looks like a parent-of-projects; report exactly which roots were
examined; no automatic recursive discovery as a default). ADR-0001 §11 assesses that
preference as compatible with AC1-4 but does **not** treat that as satisfying AC5 —
AC5 still requires the founder to review and sign off on the exact chosen wording
before HORO-1629 is mergeable, and before HORO-1829 can pass. This remains open.
Acceptance criteria, quoted verbatim from Jira:

> * AC 1: A configured root that holds no project marker and no build output does not contribute to a clean bill of health without the user being told, at some point, that the folder itself was what was examined.
> * AC 2: Whatever is chosen, a correctly configured project root reports exactly the resources and byte counts it reports today. No regression to HORO-1576 AC 4.
> * AC 3: No new unbounded traversal. Any new read is bounded to a stated depth below paths the user chose.
> * AC 4: Failed or partial reads of a configured root still report as incomplete discovery rather than as an absence.
> * AC 5: The chosen wording is reviewed by the founder, for the same reason HORO-1576 AC 3 existed: it appears on a mistake that is easy to make and therefore common.

## Dependency DAG

```
HVDL-26 (Polaris idea)
  -> HORO-1043 (Epic)
    -> HORO-1822 (architecture approval)  <-- ACTIVE
      -> {HORO-1823, HORO-1824, HORO-1825, HORO-1827}  (parallel, all blocked only by 1822)
        -> HORO-1826 (blocked by 1823 + 1824 + 1825)
          -> HORO-1828 (blocked by 1826 + 1827)
            -> HORO-1829 (blocked by 1823, 1824, 1825, 1826, 1827, 1828, AND 1629)
              -> HORO-1073 (final release-readiness gate: "Blocked by: everything")

HORO-1629 (independent bug, needs-founder-decision)
  -> HORO-1829 (direct block, not routed through HORO-1822's chain)
```

Linear critical path: `HVDL-26 -> HORO-1043 -> HORO-1822 -> {1823,1824,1825} -> 1826 -> 1828 -> 1829 -> 1073`,
with `HORO-1827` joining in parallel (feeds both 1828 and 1829) and `HORO-1629` joining
independently as a second root feeding directly into 1829.

Relates-to (non-blocking): HORO-1825 relates to HORO-1563 (Statusline Epic);
HORO-1073 relates to HORO-1337 (Company DogFooding Epic), HORO-1357, HORO-1358, HORO-1365.

Two tickets' *prose* (not formal Jira links) cross-reference HORO-1629 without a
formal issuelink: HORO-1824 ("Relate existing HORO-1629 rather than duplicate it")
and HORO-1828 ("Tie configuration-folder false-all-clear to existing HORO-1629, do
not duplicate"). The only formal link HORO-1629 carries is `blocks HORO-1829`.

## Findings already established this session (carried forward from chat, not re-derived)

1. **Live hook binary inside a disposable build cache — concrete instance of the HORO-1825 risk.**
   The shared Cargo target dir (`~/.cargo/shared-target`) currently hosts a LIVE hook
   binary, referred to here as `HOOK_BIN_A` (this is a public repo; the real binary
   and project names are in Jira/ADR-0001 §1.1, not reproduced here), wired into the
   local Claude Code hook config for post-tool-use / stop / user-prompt-submit hooks.
   This is a real, present-day example of exactly what HORO-1825 is scoped to guard
   against: an installed executable runtime dependency sitting inside a cache that
   looks deletable.

2. **`HOOK_BIN_B` — unresolved path, open TODO.**
   A second hook-config entry does not resolve on the capturing shell's PATH. Its
   resolved path is not yet known; this may mean its hook is already broken, or it
   may resolve differently in the hook runtime's actual PATH. TODO: locate and
   confirm where this binary actually lives before any cleanup action near it is
   considered safe.

3. **Disk is currently healthy — this is a historical reconstruction, not a live emergency.**
   296 GiB free of 1.8 TiB on `/System/Volumes/Data`, 84% used, captured
   2026-10-10T07:21Z. The incident this campaign is reconstructing already happened
   and was already mitigated; there is no active disk-pressure emergency right now.

4. **A live `cargo doc --workspace --no-deps` process observed under a lock mechanism — looks legitimate, not abandoned.**
   PID 59931, parent PID 86585 (`python3 scripts/qa/resource-lock.py`). The presence
   of a lock-script parent is itself evidence this is a supervised, intentional build,
   not a detached/orphaned process — exactly the kind of case HORO-1823 is designed
   to distinguish from an abandoned build.

## Open decisions reserved for the founder (ADR-0001 §16)

1. HORO-1629 final wording (AC5 sign-off) — not yet done.
2. Shared/redirected Cargo targets staying permanently non-preauthorizable (never AUTO_SAFE) — ADR default; founder may loosen later.
3. Adding the `toml` crate (needed for Codex `notify` config parsing in HORO-1825) — AGENTS.md requires approval before any new dependency.

## Status line

> ADR-0001 drafted on branch `v0.1.0/HORO-1822/recovery_arch_adr`
> (`docs/development/adr/0001-process-aware-recovery.md`). Not yet reviewed,
> not yet approved. No HORO-1823+ implementation has started. A secret
> (local test-DB credential) was found exposed in this session's incident-
> evidence scratchpad file — flagged to the operator for rotation, not
> committed anywhere, not reproduced in this file or the ADR.
