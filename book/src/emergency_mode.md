# Emergency Mode

`glomeris emergency` (`src/emergency/mod.rs`, macOS only) is a **degraded**
recovery path that must produce a useful result even when SQLite/history/log
writes fail, there is no network, there is no LLM provider configured, and no
GUI is running. It is not `glomeris free --target` with a flag — its scope
and limitations are deliberately narrower.

The menu-bar app does not expose this command, deliberately: `emergency` takes
no arguments and acts machine-wide, which is not a thing to put behind a
one-click control. It is CLI-only.

## The invariant is unchanged

Emergency pressure is not permission to weaken the safety model. This module
reuses `policy::classify`/`policy::approval::authorize` and
`executor::execute` exactly as they are elsewhere — it does not define any
separate, looser emergency-only rule set, and it never references anything
network- or LLM-related (enforced by a test that scans the module for such
references).

## What it does, in order

1. **Frees its own disposable state first, unconditionally.** Before
   anything else, it deletes the tool's own best-effort pressure-history
   file (the same `history.tsv` the daemon appends to) — never gated on
   policy, because this file is not a developer resource under policy's
   purview; it's this tool's own append-only log, and deleting it is safe by
   construction. A missing file is not an error: the function silently does
   nothing rather than fabricating work.
2. **Discovers candidates via the same bounded detector registry** used
   elsewhere (`DetectorRegistry::discover_all`) — never the scanner's
   slower, unbounded filesystem walk.
3. **Classifies and, for `AutoSafe` candidates only, attempts execution**
   through the same `classify`/`authorize`/`execute` pipeline as normal
   operation.

## AUTO_SAFE-only, no interactive Ask

A degraded, no-time, no-mechanism-for-consent path can only auto-execute
`AutoSafe` candidates. Everything else (`Ask`, `Protected`) is refused and
counted in `EmergencyReport::denied_candidates` — never escalated to
interactive consent. This is a deliberate MVP scope decision documented in
the module itself, not an oversight.

## No LLM, no network, no required database

`run_emergency` never calls an LLM and never makes a network request — this
is enforced by a dedicated test, not just a comment. Persistence-write
failures (a plain write failure, or a failing directory creation) do not
stop the run: `run_emergency` still completes and still frees the
self-owned-state fixture, per its own fault-injection tests.

## `EmergencyReport`

Every field is a plain, dependency-free primitive (`u32`/`u64`/`String`), so
the report stays printable even if every other subsystem in the process has
already failed:

- `actions_attempted` / `actions_succeeded`
- `total_bytes_freed`
- `denied_candidates` — every candidate whose `classify()` result was not
  `AutoSafe`. Never incremented for an unrelated wiring gap (e.g. no
  registered action for an `AutoSafe` kind) — those go to `errors` instead.
- `errors` — non-fatal error strings, bounded to at most 8 entries so an
  adversarial run can't grow this field without bound.

## Documented limitations (from the module's own doc comments)

- **The preallocated emergency-reserve-file idea is deferred, not
  implemented.** The originating ticket named it as an experimental idea to
  reject or defer without reliable measured evidence; this module does not
  build it.
- **Resolved (HORO-994): the candidate loop now frees real bytes via a real
  detector-produced candidate.** `executor::build_fresh_evidence`'s
  deletion-time revalidation reuses the same bounded recursive size
  estimate (HORO-1016) for `reclaimable_bytes` that it already used for
  `logical_bytes`, so a real `AutoSafe` approval survives revalidation and
  executes. Both the self-owned-disposable-state step (step 1 above) and
  real detector-found candidates can now reclaim bytes.
- **Near-zero-real-disk-space testing was not performed.** Driving a real
  machine's free space to near zero to test this path was judged too
  destructive to be worth the risk; fault injection via fakes covers the
  equivalent failure modes instead.
