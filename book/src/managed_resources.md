# Managed resources

Glomeris owns the ownership, policy and execution core. Claude Code and Codex
adapters acquire before launching into a NEW explicitly managed resource;
legacy launches remain unchanged. Never enroll an existing worktree, unique
source, session history, database or arbitrary shared cache.

The initial `managed_generated` capability is deliberately narrow: only a
standalone Rust object produced by Glomeris from a retained, tracked input in a
clean committed workspace. A reconstruction description or plausible object
header alone cannot authorize reclamation. The producer retains the pinned
input separately, bounds input/output size and compilation time, and rejects
external or environment dependencies. Symlinks, hard links, mount changes,
unknown/source files, executable outputs and changed manifests refuse cleanup.
General Cargo targets and legacy caches are not enrolled by this capability.
Assembly, external-file and macro-generated input paths are conservatively excluded
until a stronger producer sandbox can prove their closure. Compiler version is
recorded; rebuilding later may require restoring that toolchain.

```text
glomeris resource create --source-root /repo --recipe 'standalone object'
glomeris resource produce ID GENERATION --input lib.rs
glomeris resource hold ID GENERATION --host codex --session SESSION --owner-pid PID
glomeris resource inspect
glomeris resource reconcile
glomeris resource policy show
glomeris resource sweep --unattended
```

Hold is a persistent JSON-lines stream. `renew` extends liveness only; `progress`
requires an advancing checkpoint sequence. `child_pending` must precede launch;
`register_child` records a process generation before use proceeds. Matching
`release` requires registered children and the isolated process group to end.
Any native hold durably invalidates the producer-only consumer-closure proof.
Even successful release does not re-enable automatic reclamation: escaped
descendants require an independently approved closure protocol.
EOF, a crashed holder or an interrupted launch does not prove that all native
agent descendants ended: unconfirmed consumer coverage remains protected.
A holder retains a shared kernel barrier even when idle. Renewal uses a short
resource metadata lock, independent of the global cleanup lock. Sweeping holds
the existing execution lock and the same resource barrier exclusively through
fresh policy/executor checks and durable outcome recording.

Lifecycle reports ACTIVE, IDLE, STALLED, ORPHANED, UNKNOWN, RECLAIMABLE and
RECLAIMED separately from deletion policy. Default inactivity is 24 hours since
meaningful verified use. Renewal, CPU utilization, creation age and silent logs
never establish completion. STALLED needs distinct health faults and remains
protected; no stalled-worker termination category is enabled.

Managed retirement is available only through `resource sweep --unattended`.
Generic execute, free, Autopilot and emergency paths cannot mint its single-use
path/generation/fingerprint/action/byte-bound permit. Discovery remains shared.
Both per-class retention and the existing Autopilot envelope must authorize
unattended execution. Action, byte, duration and pressure budgets remain in
force; revoking Autopilot also revokes managed sweeps. The existing monitor
reconciles and attempts an authorized sweep at its first tick and every 15 ticks.
Traversal uses independent persisted cursors so old/tombstoned or active records
cannot permanently hide generations beyond the bounded batch. Read-only
discovery peeks at the sweep cursor without advancing it.
No second daemon or executor is installed. Emergency mode excludes managed
resources because its degraded entry point lacks the managed execution capability.

The configuration repository supplies versioned policy and thin adapters.
Native `resource policy apply/restore` and `resource authority apply/grant-managed`
accept private expected-current snapshots and perform compare-and-swap under the
execution lock. Installation and rollback must not overwrite intervening changes.
Previously absent configuration rolls back to explicit disabled defaults; resource
records and recovery information are never removed by configuration rollback.

An unknown owner can pass only with independent producer proof, unchanged
manifest, meaningful inactivity, no valid consumers and full native policy checks.
Other unknown resources retain a specific blocker, including native consumer
coverage after an unconfirmed release. There is no unconditional age-only cleanup.

Interrupted retirement retains bounded attempt history and durable intent.
Verified absence with recorded intent is terminal without another deletion.
A complete unchanged producer-only generation with matching recorded identity
can clear its pending intent without deleting anything; a later sweep requires
fresh policy, pressure, budget and permit admission. Partial/changed generations
and legacy intents lacking a verified attempt snapshot remain protected.
An unrelated corrupt record does not prevent other verified generations from
being reconciled or retired. Corruption,
unconfirmed children and changed identities preserve data. Power, network and
session termination are not recovery prerequisites. Only filesystem free-space
delta is physical recovery; logical removed bytes are separate and concurrent
allocation can make the measured physical delta negative.

## Native consumer evidence

`resource native request --json` accepts one bounded stdin request: register a
Cargo target, registered Git worktree, or local Docker socket/builder scope;
acquire before native use; then update a revision-bound task entitlement.
The existing resource Store, execution lock and durable writes are reused.
Capabilities travel over stdin/stdout and private records only, never command
arguments or ordinary inspection. `native inspect --json` and `native reconcile
--json` report protection without inferring release from process death.

Waiting and suspended claims protect future use after lease expiry. Renewal is
not progress. Authenticated handover rotates the claimant epoch and capability;
stale callers cannot reopen a released claim. Child launch intent precedes
launch, and child entitlement needs explicit authenticated relinquishment.
Unconfirmed launches, dead owners and unknown legacy consumers remain evidence
gaps. They are not fabricated completion signals.
Cancellation is an authenticated **owner-attested** non-dispatch declaration,
not an independent observation that a child never launched. It cannot promote
coverage or authorize retirement.

All native coverage remains incomplete in this evidence bridge. Host/session and
Docker builder labels are caller declarations, not verified canonical Host SPI
lineage or historical-consumer closure. No native retirement permit or class
grant is minted. The executor holds a shared namespace barrier through mutation
and refuses paths overlapping enrolled scopes; native admission takes that barrier
exclusively. Registering a scope therefore cannot race an already admitted action.
Existing generated-resource sweep skips native records. Native adoption does not
change the configured 24-hour threshold, permit age-only cleanup, or attest that
Cargo contents, ignored worktree files, or Docker records are reconstructable.


## Fixed standalone Cargo libraries

The separately authorized `managed_cargo_library` class compiles committed
`src/lib.rs` under a fixed Glomeris package configuration. It is **not** a build
of the original package: its edition, features, profiles, dependencies, workspace,
patches and build scripts are not applied. Unsupported source inputs refuse
production rather than expanding the reconstruction boundary.

`resource create-cargo --source-root GIT_WORKSPACE --recipe RECIPE` creates a new
private generation; `resource produce-cargo ID GENERATION` retains the exact
source, effective manifest and lockfile outside the reclaimable generation.
Installed Cargo/rustc are pinned for production, run offline with a controlled
environment, one job, no incremental/debug data, bounded output and time, and an
isolated process group. Failed or interrupted production never seals itself.
No existing target directory is adopted. General native Cargo remains protected.

This class has its own `managed.clean.cargo-library` action and grant. It defaults
to disabled and retains the 24-hour meaningful inactivity threshold. The old
`managed_generated` grant never authorizes it. The existing lifecycle, permit,
locks, policy budgets and deletion executor are reused; no separate service is
introduced. Complete directory/file identities and digests bind production.
Hardlinks must form a complete internal linkset; external links refuse cleanup.

Only independent exports of `artifact.rlib` may leave closed core custody.
Native path exposure permanently invalidates automatic consumer closure, even
when its observed holders later release. Reconstruction inputs and exported
copies survive generation cleanup. Record schema 2 is deliberately refused by
older readers; roll back to a compatible reader or preserve the generation.
Logical removed bytes are not attributed physical recovery. Inspect the observed
filesystem availability delta separately, including concurrent allocation.
