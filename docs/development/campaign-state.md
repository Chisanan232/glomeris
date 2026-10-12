# HORO-1822 Campaign State

> **Resume from this file, not from chat history.** This is the durable record of
> where the HORO-1822 post-incident / process-aware-recovery campaign stands.
> Chat context will be lost across sessions; this file will not.

- **Owning Epic:** HORO-1043 — Glomeris MVP 2.0 — Interactive Daily Driver / Trustworthy Recovery UX
- **Active ticket:** HORO-1829 (independent QA — now fully unblocked, every other ticket in the dependency DAG is Done).
- **Last session state (2026-10-12):** HORO-1822 through HORO-1629 — the entire dependency DAG feeding HORO-1829 — are fully complete and merged: HORO-1822 (ADR-0001, PR #156), HORO-1825 (PR #157, 4 review rounds), HORO-1827 (PR #158), HORO-1823 (PR #160, 1 review round + 1 found/fixed timeout-budget bug), HORO-1824 (PR #161, 2 review rounds — round 1 caught a real untrusted-`rust-toolchain.toml` code-execution risk in `cargo metadata` invocation plus a workspace/source-overlap scope bug; round 2 caught 2 more real gaps in those same two fixes), HORO-1826 (PR #163, `main`@fbb81c3, 4 review rounds — round 1: 3 of 4 `execute()` callers silently discarded computed `host_integrity`/`volume_free_delta`; round 2: per-dependency restat failures collapsed to a pointer-less unresolved variant, reintroducing sibling-masking, plus Autopilot audit-log wiring found directly fixable; round 3: `(source,pointer)` itself wasn't a unique dependency key, masking bug recurred one level deeper, plus a false "None is the only honest value" claim with zero real-path test coverage; round 4: both fixed and mutation-tested clean), HORO-1828 (2 PRs — #165 `main`@422518d eligibility/guarded/feasibility, 1 review round PASS; #166 `main`@5ead5d9 explain structure, 1 review round PASS — reviewer independently re-verified every privacy/mutation claim rather than trusting the diff; an automated scanner also caught 2 real bugs mid-development, both fixed and reviewer-confirmed absent from committed code: a resolved-path leak in the dependency `pointer` field, and the visible holders label missing fail-closed branches its accessibility-label sibling already had), HORO-1629 (PR #168, `main`@8d1f89d, 3 review rounds — round 1: 2 MUST-FIX, missing CLI docs plus zero test coverage on the one new UI check-before-add flow, both fixed; round 2: re-verified round 1 held including re-mutation-testing AC4's fail-closed path, caught that round 1's own fix commit broke the Swift-6 concurrency CI guard, then found the ticket's own literal worked example (`~/code`) silently resolved to `undetermined` for want of tilde-expansion; round 3: tilde-expansion added on both Swift and Rust sides, mutation-tested, PASS). All Jira-Done. Founder approved all 3 of ADR-0001 §16's open decisions on 2026-10-10 — recorded in ADR-0001 §16 directly.

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
| HORO-1822 | Story | **Done** | High | [Post-Incident] Reconstruct full-disk rescue and approve process-aware recovery architecture — ADR-0001, PR #156 |
| HORO-1823 | Story | **Done** | High | [Process Evidence] Distinguish abandoned builds from valid idle daemons without automatic kill authority — PR #160, `main`@d3b155d |
| HORO-1824 | Story | **Done** | High | [Cargo] Reconcile target-dir ownership and safe scoped cleanup across shared and local builds — PR #161, `main`@7b747ea |
| HORO-1825 | Story | **Done** | **Highest** | [Executable Safety] Protect configured agent-hook binaries residing in disposable build caches — PR #157, `main`@07cffa5 |
| HORO-1826 | Story | **Done** | High | [Recovery Integrity] Verify post-cleanup function and honest degradation beyond free bytes — PR #163, `main`@fbb81c3 |
| HORO-1827 | Story | **Done** | Medium | [Pressure History] Observe 12h stability and honestly plan safe recovery toward 1 TiB free — PR #158, `main`@1b41814 |
| HORO-1828 | Story | **Done** | Medium | [Operator UX] Explain uncertain process ownership, blocked cleanup and rebuild blast radius — PR #165 `main`@422518d + PR #166 `main`@5ead5d9 |
| HORO-1829 | Story | To Do | High | [Independent QA] Prove process-aware recovery and no hook regressions on real multi-agent macOS — **ACTIVE**, now fully unblocked |
| HORO-1629 | Bug | **Done** | Medium | A configured project root that is a parent of projects yields a clean bill of health with nothing examined below it — PR #168 `main`@8d1f89d |
| HORO-1073 | Task | **In Progress** | High | MVP 2.0 founder dogfood pass #2 + release readiness record — **final release gate** |

### HORO-1629 — AC5 now resolved; ready to implement

HORO-1629 was `needs-founder-decision`, with AC5 requiring founder review of the
chosen wording before the ticket could be considered satisfied. **This is now
resolved**: the founder approved the exact wording on 2026-10-10, recorded verbatim
in ADR-0001 §16.1 — the settings warning and scan-coverage line strings are fixed,
approved defaults, not to be reworded during implementation. No recursive discovery
by default; implement within the existing HORO-1629 ticket, no duplicate.
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

## Founder decisions (ADR-0001 §16) — RESOLVED 2026-10-10

1. HORO-1629 final wording — **approved**, exact strings recorded in ADR-0001 §16.1. HORO-1629 itself not yet implemented; still requires that exact wording to be used verbatim when it is.
2. Shared/redirected Cargo targets staying permanently non-preauthorizable — **approved, confirmed**.
3. `toml` crate (Codex `notify` parsing) — **conditionally approved**, subject to the supply-chain/security checks listed in ADR-0001 §16.3, at whichever ticket actually needs it (not consumed by HORO-1825 — it deferred Codex `config.toml` parsing and stayed fail-closed/ASK for that source instead).

## Known review-process lesson (apply to every remaining ticket)

HORO-1825 took 4 independent review rounds; each of rounds 1-3 found a real fail-open the previous round's fix had either missed a sibling of or newly introduced. The pattern was always the same shape: a sub-probe's `Err`/`None`/timeout result silently became an empty/default/"no findings" value instead of propagating as unresolved/UNKNOWN. Round 4 was only accepted after the reviewer *mutation-tested* the fix (reverted it, confirmed the new tests went RED, confirmed GREEN with the real fix) rather than just reading the diff. **Apply this standard to HORO-1828/1829 too** — any ticket introducing a new subprocess probe (ps, launchctl, cargo metadata, etc.) is exactly the shape where this bug class recurs. Don't accept a review as final on inspection alone; prefer mutation-tested confirmation for anything touching policy classification.

HORO-1826 confirmed the same lesson generalizes beyond "a probe result collapses to a default": a **key-uniqueness** assumption (`(source, pointer)` identifying one dependency) silently broke the same masking invariant twice, once at the source level (round 2) and again one level deeper once pointer was added but still not unique (round 3) — only fixed for real once `resolved` (the actual resolved path) was added to the key (round 4). When a fix narrows a bug by adding a field to a matching/dedup key, check whether the new key is actually unique for the real data shape, not just "more unique than before."

HORO-1828 (both PRs) were the first tickets in this campaign to pass independent review on round 1 — but each still had at least one real bug caught mid-development by an automated scanner before the PR was finalized (a resolved-path privacy leak in PR B, a `Supervisor::None`/fail-open false alarm later confirmed as a transient uncommitted-state artifact rather than a real bug, and a visible-vs-accessibility-label inconsistency where the sighted-user string skipped a fail-closed branch its VoiceOver sibling already had). **Lesson: UI/wording layers get the same fail-open treatment as evidence/policy layers** — a visible string and its accessibility string must be checked for behavioral agreement, not just independently "look correct."

HORO-1629 added two more lessons. First, a genuine ticket-scoped bug can hide behind the ticket's OWN worked example: the literal `~/code` path named in HORO-1629's Jira description never got tilde-expanded, so the fix's own motivating scenario silently fell through to `undetermined` instead of firing the warning — found only because a reviewer actually ran the literal example rather than a sanitized equivalent. Second, a build-log-based CI guard (`check-no-swift6-concurrency-errors.sh`) can silently pass for the wrong reason: an incremental/cached `xcodebuild` log can scan a suspiciously low line count (4, in one case) and report PASS without having recompiled the changed files at all. **Always force a clean build (`xcodebuild clean` first) before trusting a build-log-based guard script's result**, and treat a much-lower-than-expected scanned-line count as a signal to redo it clean, not as a pass.

## HORO-1043 epic-level release gate — broader than this DAG

This file tracks the HORO-1822 post-incident sub-campaign, which is the dependency chain feeding HORO-1829. But HORO-1043 (the owning Epic) has its OWN separately-stated release/QA gate (7 items, Epic description) that is not fully covered by this DAG and must be checked independently before claiming epic-level release-readiness:

1. Independent adversarial review of the `glomeris execute` surface specifically (not just the post-incident additions around it) — verify whether this has already happened in an earlier, pre-this-campaign phase, or still needs a dedicated pass.
2. A review with a *mechanical* check (not just reviewer say-so) confirming zero duplicated authorization/policy/execution semantics in Swift — the `check-no-policy-label-branching.sh`/`check-vocabulary-covers-cli-tokens.sh` guards built up across this campaign likely satisfy this, but confirm they cover the `execute` surface itself, not only the reporting/UX additions this campaign made.
3. Every `execute` refusal path (PROTECTED, ASK-without-confirm, ASK-with-stale-fingerprint, revalidation-abort, busy-lock) exercised end-to-end through the actual UI — not just unit/integration tests.
4. Golden DTO fixtures green both sides — continuously true throughout this campaign, re-verify at epic-gate time.
5. `cargo fmt`/`clippy`/`cargo test`/`cargo deny` all green — continuously true, re-verify at epic-gate time.
6. **A fresh founder-dogfood pass, tracked as HORO-1073, verdict `READY_FOR_NEXT_STAGE`.** This is explicitly a human-in-the-loop gate — HORO-1073's AC requires the founder personally exercising the actual menu-bar app against a real project root with both a Cargo `target/` and `node_modules`, including at least one ASK/UNKNOWN_INCOMPLETE execution through the UI. **No agent session can self-satisfy this** — it is not an engineering task to automate, it is the founder's own acceptance act. If this hasn't happened, report it as a genuine outstanding blocker, not a thing to simulate or claim on the founder's behalf.
7. The `.app` artifact installs and launches from a clean machine following only published instructions.

HORO-1073 itself says "Blocked by: everything — this is the last ticket in the Epic." Do not treat HORO-1829's completion as release-readiness; HORO-1073's human dogfood pass is the actual final gate, and items 1-3 above may need dedicated verification work this DAG never covered.

## Status line

> HORO-1822 through HORO-1629 — the entire dependency DAG — complete, merged,
> Done. HORO-1829 (independent QA) now fully unblocked and active. HORO-1073
> (final epic-level release gate, requires a literal founder-dogfood pass —
> human-in-the-loop, cannot be agent-simulated) not yet started. Every merged
> ticket went through at least one independent review round that found and
> fixed a real issue — none merged clean on round 1 except HORO-1828's two
> PRs (which still each had a real bug caught by an automated scanner
> mid-development) and HORO-1629's round 3 (the tilde-expansion fix, after
> 2 prior rounds each found something real). HORO-1825/1826 each took 4
> rounds; HORO-1629 took 3. A secret (local test-DB credential, unrelated
> horologium repo, disposable/localhost-only) was found exposed in an
> earlier session's incident-evidence scratchpad file — flagged for
> rotation, deliberately not rotated (would have disrupted other
> concurrent sessions' test runs for no safety gain since it was never
> reachable off localhost), not committed anywhere, not reproduced here.
