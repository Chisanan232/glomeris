# Glomeris Founder DogFood — 2026-10-16 Personal-Mac Disk Incident (First Formal Pass)

> **Internal research record — not public product documentation.**
> Not part of the mdBook site (`book/src/`), not linked from `SUMMARY.md`,
> not referenced from `README.md`. Lives under `docs/internal/` specifically
> so the `docs.yml` GitHub Pages pipeline (which only builds `book/` via
> mdBook and `cargo doc` from Rust source comments) never touches it.

## Status

**OPEN.** Per explicit founder direction (2026-10-16): the real personal-Mac
disk-pressure incident on this host (disk fell to ~1.4 GiB free with ~29
concurrent sessions, tracked at SPE-109) is the **first formal end-to-end
Founder DogFooding pass** for this incident and the current product version.
Earlier CLI smoke tests, monitor heartbeat history, staged builds, or
automated QA (HORO-1829, this session's own sandbox work) are **not**
founder acceptance and must not be represented as such. This record stays
open until the founder personally performs and confirms the manual GUI
journey in Phase 4 below.

This supersedes a prior same-day note (on PR #174 / `campaign-state.md`)
that mischaracterized HORO-1073 as satisfiable without founder involvement
— see the correction comment on HORO-1073.

## Metadata

- **Date:** 2026-10-16
- **Trigger:** real personal-Mac disk-pressure incident, tracked at SPE-109
  (Common Task) and HORO-1874.
- **Primary references:** HORO-1073, HORO-1313, HORO-1337, HORO-1829,
  HORO-1874, SPE-109, ADR-0001.
- **Exact artifacts tested (Phase 1, read-only):**
  - CLI binary `main`@`82808ae` (storage hash `16f90df6…`)
  - CLI binary `main`@`fa7379a` (storage hash `0f58f5e3…`, currently wired
    into `com.glomeris.monitor.plist`)
  - Both sha256-verified to match their own content-addressed storage
    directory names — no corruption found.
- **GUI `.app`:** not yet built or launched on this host this pass (no
  `dev.glomeris.GlomerisMenuBar` defaults domain exists at all — confirmed
  this machine's first real GUI launch has not happened yet, so Phase 4
  below will be a genuine first run, not a repeat).

## Phase 1 — first-run condition audit (read-only, agent-verified)

- **Prior runs:** the CLI daemon has run historically on this host.
  `~/Library/Application Support/Glomeris/history.tsv` has real entries
  2026-09-11 through 2026-10-15, including EMERGENCY/CRITICAL
  disk-pressure states consistent with this incident. The product has
  **not** never been launched — do not assume a clean slate.
- **LaunchAgent:** `com.glomeris.monitor` is installed
  (`~/Library/LaunchAgents/com.glomeris.monitor.plist`,
  `StartInterval=60`, `RunAtLoad=true`) but currently **disabled**
  (`launchctl print gui/$(id -u)` → `disabled`). No live auto-trigger risk
  at the time of this audit.
- **Autopilot reachability:** confirmed via source read
  (`daemon_run()` in `src/main.rs`; ADR-0001 §14, "no new scheduler/daemon
  ... sampling is statvfs only") that the daemon **cannot** reach
  `execute()`/Autopilot under any configuration — it only polls `statvfs`,
  writes history, and queues notifications. Starting/observing the daemon
  carries no deletion risk.
- **Real defect found:** `daemon_run()` has no single-instance guard
  (no pidfile/flock), so launchd's `StartInterval=60` with no
  `ThrottleInterval` can overlap invocations — confirmed by garbled
  `monitor.log` entries (`daemon daemon daemon run` instead of one
  `daemon run` per line). Filed as **HORO-1907** (logging/supervision
  only, not a safety-authority gap). Not yet fixed.
- **`detect` performance:** a bounded 60s `glomeris detect --json`
  against this real host (no project roots configured) **timed out with
  zero output.** Recorded as UNVERIFIED, not claimed working. Matches the
  pattern already noted in HORO-1874 (a ~529GiB shared-target `explain`
  timing out at 120s). Not retried further this pass (avoiding wasteful
  retries against a host still under real disk pressure).
- **Baseline/rollback:** no GUI state exists to roll back (never launched).
  CLI daemon state (`history.tsv`, `execution.lock`) is historical
  evidence, preserved, not modified by this audit.

## Phase 2/3 — non-destructive observation and controlled execution tests

**Not yet performed this pass** beyond the Phase 1 read-only audit above.
Given (a) the host was still under real, recent disk pressure (33 GiB
free / 99% used at audit time) and (b) ~10+ other sessions were actively
building/mutating this same shared repo and its build infrastructure
concurrently, no new heavy builds or live-host `detect`/`execute` runs
beyond the single bounded, read-only probe above were attempted this
pass. To be continued in a follow-up pass once host pressure and
concurrent build load are lower, using disposable, independently-owned
fixtures only — never live personal data, shared Cargo targets,
worktrees, or Docker volumes.

## Phase 4 — Founder GUI acceptance checklist (BLANK — pending founder)

**Do not fill these in on the founder's behalf.** Every field below is
intentionally blank until the founder personally performs the journey
through the real installed Menu Bar app and reports back.

| Item | Founder observation | Pass/Fail/N-A |
|---|---|---|
| Ease of installation and initial setup | _(pending)_ | |
| Navigation and information clarity | _(pending)_ | |
| Candidate accuracy (vs. real known disk state) | _(pending)_ | |
| Understanding of cleanup risks (ASK/PROTECTED wording) | _(pending)_ | |
| AI Plan usefulness | _(pending)_ | |
| Apply/Clean workflow | _(pending)_ | |
| Error and recovery UX | _(pending)_ | |
| Actual trust in the product's recommendations | _(pending)_ | |

**Overall verdict:** _(pending — not `READY_FOR_NEXT_STAGE` until the
founder records one)_.

## Findings log (this pass)

| Finding | Ticket | Severity | Status |
|---|---|---|---|
| Daemon has no single-instance guard; launchd overlap corrupts `monitor.log` | HORO-1907 | Low (logging/supervision only) | Filed, not yet fixed |
| `detect --json` times out (60s bound) against real, loaded host with no project roots | — (noted against HORO-1874/SPE-109) | Unknown — needs reproduction under less host load before triaging | UNVERIFIED, not filed as a standalone ticket yet |

No release-blocking findings identified in Phase 1. This is not a release
readiness verdict — Phases 2-4 remain open.
