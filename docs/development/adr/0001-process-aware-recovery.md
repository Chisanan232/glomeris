# ADR-0001: Process-aware, dependency-aware recovery (post-incident architecture gate)

- Status: **Accepted.** Independent adversarial review complete (2 must-fix findings resolved: §4.3 PATH-shadowing resolution, §4.3 relative-path tokenization; 3 should-address findings incorporated: §4.2 dedup scope, §4.3/§10 closed-source-list gap, §6 lsof cross-UID residual risk; 1 nitpick incorporated: §14 completeness-rank scope). No code-claim inaccuracies found against `main` @ 23e08dc. No break in the hard invariants (no new policy class, no kill capability, no LLM-reachable deletion authority, no automatic ASK/PROTECTED→AUTO_SAFE widening). **Founder decisions on §16 recorded below; HORO-1823+ implementation authorized.**
- Ticket: HORO-1822 (Epic HORO-1043; discovery source HVDL-26)
- Date: 2026-10-10
- Deciders: founder (approval, HORO-1629 wording), independent security reviewer
- Code baseline: `main` @ 23e08dc

## 0. HORO-1822's own acceptance (verbatim, summarized)

- No fabricated causes, current measurements, or successful checks.
- Prove false-positive/negative hypotheses with disposable fixtures and text-only privacy-safe evidence.
- Document the inability to infer "abandoned" from age/PPID/Jira status alone; liveness never grants kill/delete authority.
- No release without founder GO.

This ADR satisfies these via: §1.1 (evidence labelled OBSERVED/INFERRED/UNKNOWN, no fabricated causality claimed for the 2026-10-10 incident), §4.1/§13 (disposable fixtures + anti-vacuity mutation tests), §4.1 claims table + §10.3/§10.4 (process claims never grant kill/delete authority, and no kill capability is introduced at all), and the unchanged founder-GO release gate (HORO-1073, untouched by this ADR).

## 1. Context

Canonical invariant (CLAUDE.md, HVDL-26): **AI recommends. Policy decides. Executor verifies. Filesystem reality wins.**

### 1.1 What the incident and today's capture showed (sanitized; OBSERVED 2026-10-10T07:21Z unless marked)

- The data volume has 296 GiB free of ~1.8 TiB (84% used). There are no local Time Machine snapshots right now. This is a historical reconstruction, not a live emergency.
- `~/.cargo/config.toml` sets `[build] target-dir = "~/.cargo/shared-target"`. That applies to every Cargo invocation on the machine, not just Glomeris worktrees.
- **An installed runtime dependency lives inside a disposable build cache.** `~/.cargo/shared-target/debug/HOOK_BIN_A` (named in HORO-1825) is referenced by absolute path from `~/.claude/settings.json`, for the post-tool-use, stop and user-prompt-submit hooks.
- Dozens of `HOOK_BIN_A daemon run` processes, with PPID=1, run from both `shared-target/debug/` and `shared-target/release/`. Their start dates span several days.
- A peer server binary runs from `shared-target/debug/`. Several peer runner binaries run from Cargo targets inside per-session scratch directories.
- A second hook command, `HOOK_BIN_B`, is configured as a bare name. It does not resolve on the capturing shell's PATH. That is OBSERVED for that shell only: the hook runtime's PATH may differ, and the cause of the post-rescue `command not found` is **not proved** (HORO-1825).
- A long-running `rustdoc` was writing into `shared-target/doc`. Its parent was `cargo doc`, which in turn was a child of a repo resource-lock wrapper — a supervised, legitimate build.
- From Jira HORO-1824 (measured earlier, not re-measured here): about 109 GiB of local `target/` directories and about 525 GiB of shared target.
- **The capture method leaked a secret.** `ps -o command` exposed full argv, including a credential-shaped database URL set as an environment assignment. That is why Glomeris must never collect argv (see §6.5). Flagged to the human operator for rotation; not reproduced in this document, not committed anywhere.

### 1.2 What the code does today (verified by reading)

- **Evidence model.** `src/evidence/model.rs`:
  - `Evidence` carries `ProbeOutcome` fields: `Observed` or `Unavailable(reason)`, with no `Default`.
  - `ProcessRef` is `{pid, command}`, where `command` is lsof's `c` field (the process name, not argv).
  - `ResourceFingerprint` is `{dev_ino, mtime, tool_revision}`.
- **Correlation.** `src/evidence/correlate/`:
  - `lsof -F pcn +D <path>` reports open files; `lsof -a -d cwd ... +D` reports working directories.
  - `git` reports repository state.
  - `pgrep`/`docker version` report tool liveness.
  - `EvidenceCollector::collect` is single-resource on purpose, so the executor can reuse it. `merge_into` overwrites every field unconditionally.
- **Policy.** `src/policy/engine.rs::classify` is pure: no I/O and no ambient clock. Its steps:
  1. Unknown kind → PROTECTED.
  2. `protected.rs` path-text checks.
  3. Staleness check.
  4. Completeness (steps 4–5: `Failed` → ASK `EvidenceProbeFailed`, `Partial` → ASK `EvidenceIncomplete`).
  5. Active use: any holder, a worktree that is dirty **or merely linked**, a live owning tool, or Docker activity.
  6. Recoverability.
  7. Regenerability.
  8. Otherwise AUTO_SAFE.
- **There are three policy classes and no fourth.** "UNKNOWN_INCOMPLETE" is a *label* (`src/reporting/policy_label.rs::label_for`), derived from ASK plus an evidence-quality reason.
- **Approval.** `src/policy/approval.rs::authorize` is the only way to construct an `Approval`; a private `Seal` type enforces this. PROTECTED → never. ASK → only with consent pinned to the resource and its fingerprint.
- **Autopilot.** `src/autopilot/envelope.rs::is_preauthorizable` is an exhaustive match. Only `RebuildCostHigh` can be pre-authorised.
- **Executor.** `src/executor/mod.rs::execute`:
  1. Takes an early `symlink_metadata` (dev, ino) snapshot.
  2. Runs `build_fresh_evidence`, re-running every probe.
  3. Compares fingerprints.
  4. Re-runs `classify`.
  5. Aborts if the class changed or any reason was added.
  6. Applies `structural_refusal`.
  7. Calls `verify_identity_unchanged` immediately before the tool is spawned or the path deleted.
- **Process and action surface.** `ActionStep` has two variants only, `RunTool` and `DeletePath`, and `ToolBinary` is a closed enum. Nothing can signal or kill a process.
- **Cargo.**
  - `src/detectors/cargo.rs::CargoDetector` probes only `<configured root>/target` and canonicalizes it. A redirected or shared target is discovered only if `<root>/target` is a symlink to it.
  - `src/actions/cargo.rs` runs `cargo clean --manifest-path <root>/Cargo.toml --target-dir <exact path>`, which removes the **entire** directory.
- **Workspace.** `src/workspace/**` (branch merged state, Jira/GitHub status, history) is **mechanically barred** from policy, executor, autopilot and actions by `scripts/check-workspace-aggregation-has-no-authority.sh`.
- **Measurement.**
  - `src/monitor/fs_stat.rs::FsUsage` is `{total_bytes, free_bytes}`, with no volume identity.
  - The daemon polls `statvfs` every 60 s (`monitor/poller.rs`). `history.tsv` records only transitions.
  - `RecoveryReport` already separates measured from estimated values and carries `detector_failures` and `detectors_not_examined`.
  - `ExecutionReport.actual_reclaimed_bytes` is always `Unavailable` for `RunTool`.
- **Module names differ from CLAUDE.md.** CLAUDE.md's expected names `core/config`, `persistence`, `providers` and `bin/` **do not exist**. The real equivalents are `src/settings/`, `src/monitor/persistence.rs`, `src/workspace/external/provider.rs` and `src/main.rs`. This ADR uses the real names.

### 1.3 The gap

1. **Next-invocation dependencies are invisible.**
   - `lsof +D` sees a *running* executable through its `txt` mapping.
   - It cannot see a hook binary that is not running at probe time but will be exec'd on the next hook event.
   - Deleting the target would therefore pass every current check and break another product.
2. **Effective target location is not modelled**, so a shared target's blast radius is invisible.
3. **Process facts are pid-only.** They are unsuitable for any multi-sample reasoning, and PPID=1 is ambiguous on macOS: launchd adopts orphans.
4. **Free-space accounting has no volume identity and no snapshot awareness.**

## 2. Conflicts and corrections (flagged, not silently resolved)

| # | Source A | Source B | Resolution proposed here | Needs |
|---|---|---|---|---|
| C1 | HORO-1823 AC: "verified inactive disposable target can lose one active-use veto after fresh evidence" | Brief: ABANDONED_CANDIDATE never authorizes; no automatic widening ASK→AUTO_SAFE | A veto is removed **only by verified absence**: a fresh holder probe at discovery or revalidation returns `Observed(empty)`. No process *claim* (IDLE, STALLED, ABANDONED_CANDIDATE) ever removes a veto. This is already how `execute` works: a class change in either direction aborts, and the next run re-classifies from fresh evidence. | Reviewer confirmation of the reading |
| C2 | Brief: "AUTO_SAFE/ASK/PROTECTED(+UNKNOWN)" | `src/policy/class.rs` deliberately has three classes | No new class. UNKNOWN is the `UNKNOWN_INCOMPLETE` label, produced from new evidence-quality reason codes (§5). | — |
| C3 | Brief's target-dir precedence: `--target-dir` > `CARGO_TARGET_DIR` > nested config > global config | Cargo reference docs | That precedence is incomplete. Config files are discovered from the **cwd** of the cargo invocation, not the manifest. `--config` and `CARGO_BUILD_TARGET_DIR` also apply. `build.build-dir` (`CARGO_BUILD_BUILD_DIR`) can move intermediates elsewhere; cargo 1.98 reports `build_directory` separately. Do not re-implement this: use `cargo metadata` (§4.2). | — |
| C4 | HORO-1825: "missing config … ⇒ protected/unknown" | Usability: read literally, every Cargo target is UNKNOWN on any machine without Claude/Codex | Default: a recognized source that is absent (ENOENT) = `Observed(no references)`. Present but unreadable, unparseable or unsupported = UNKNOWN (fail closed). | Reviewer confirmation |
| C5 | Brief: a founder preference exists for HORO-1629 | `campaign-state.md` and Jira: no preference recorded, zero comments | Record the preference as *brief-stated*; it is not yet on the ticket. See §11. | Founder sign-off (AC5) |
| C6 | HORO-1824: "Reuse … Workspace Intelligence … Policy/Executor" | Workspace modules are barred from policy | Target ownership facts that policy reads live in `evidence`/`detectors`, **not** in `src/workspace`. Workspace may only *display* fan-out. | — |
| C7 | — | `GitWorktreeDirty` fires for any linked worktree (`g.dirty \|\| g.worktree`, `policy/engine.rs` step 6) | Unchanged by this campaign. It explains much of the "hundreds of GiB still guarded" that HORO-1828 must explain. Relaxing it needs its own ticket, because branch-merged and Jira state are no-authority by design. | — |
| C8 | — | HORO-1822's own ticket AC (see §0) was not in the initial scratchpad gather | Added in §0 after a direct Jira fetch; AC map in §12 now covers 1822–1829. | — |

## 3. Decision (summary)

1. **No new authority anywhere.**
   - No 4th policy class.
   - No new `ActionStep` variant: in particular, no signal or kill step.
   - No new deletion primitive. Native `cargo clean` with an exact `--target-dir` remains the only Cargo mutation.
   - Nothing the LLM sees changes. None of the new fields cross `src/planner/dto.rs` or `src/actions/llm.rs`.
2. **New evidence, consumed only as vetoes.** There are three additions:
   - **(a)** An `executable_dependency` correlation probe.
   - **(b)** Process identity added to `ProcessRef`.
   - **(c)** A Cargo `target_scope` resolved via `cargo metadata`.

   Each can only move a resource *toward* ASK or PROTECTED. Nothing new can move a resource toward AUTO_SAFE.
3. **An installed executable dependency inside a resource is PROTECTED.** "Installed executable dependency" means referenced by a recognized hook, daemon or LaunchAgent config, or by a PATH entry inside the resource. **No naming heuristic is used** (no `*-hook*`, no `debug/` rules). A running executable inside a resource is ASK (`ExecutableRunningFromResource`). An unresolvable reference is ASK labelled UNKNOWN_INCOMPLETE.
4. **Shared or redirected Cargo targets are never AUTO_SAFE.** They are ASK (`SharedBuildCacheBlastRadius`), and Autopilot cannot be pre-authorised for them. The cold-rebuild blast radius is displayed.
5. **Process claims (ACTIVE / IDLE_BUT_VALID / STALLED / ABANDONED_CANDIDATE / UNKNOWN) are explanation only.** They never change a class.
6. **Revalidation is identity-agnostic and strictly stronger.**
   - Before mutation, *any* holder of the resource is a veto, whoever it is.
   - Filesystem identity uses the existing (dev, ino) snapshot.
   - Every new probe re-runs inside `build_fresh_evidence`.
7. **Measured and estimated values are never merged.**
   - df delta is the truth; du potential is an upper-bound estimate.
   - APFS snapshots, clones and hardlinks make them differ, and the report says so.
8. **Glomeris never writes host configuration.** It reports and refuses only: it does not touch hooks, binaries or credentials. Repairs belong to the owning product (coding-agent-environment and the product owners).

## 4. Data contracts

Provenance vocabulary for every new fact:
- **OBSERVED** — direct read of the thing itself: stat, an lsof fd, statvfs, a config file's bytes.
- **INFERRED** — derived from a model of another program's behaviour: cargo resolution run in Glomeris's environment, PATH lookup using Glomeris's PATH, process claims, feasibility arithmetic.
- **UNKNOWN** — `ProbeOutcome::Unavailable(ProbeReason)`.

INFERRED is carried as an explicit `provenance` field on the new structs. `ProbeOutcome` itself is not changed.

### 4.1 Process identity (HORO-1823) — extends `ProcessRef` in `src/evidence/model.rs`

```text
ProcessRef {
  pid: u32,
  command: String,                          // lsof `c` (short process name). NEVER argv.
  relation: Holder::{OpenFile | Cwd | Executable},   // lsof `f` field: txt => Executable  [OBSERVED]
  identity: ProbeOutcome<ProcessIdentity>,
}
ProcessIdentity {
  start_time: SystemTime,   // `ps -o lstart=` under LC_ALL=C, 1 s resolution; parse failure => Unavailable  [OBSERVED]
  uid: u32,                 // `ps -o uid=`                                                                 [OBSERVED]
  ppid: u32, pgid: u32,     // `ps -o ppid=,pgid=`                                                          [OBSERVED]
  state_zombie: bool,       // `ps -o stat=` contains 'Z'                                                   [OBSERVED]
  exe: ProbeOutcome<ExeIdentity>,  // lsof -a -p <pid> -d txt -F n (+ stat for dev/ino)                     [OBSERVED]
  supervisor: Supervisor::{LaunchdJob | None | Unknown},  // pid present in `launchctl list` (user domain)   [OBSERVED]
                                                          // PPID=1 alone => Unknown, never "orphan"
}
ExeIdentity { path: PathBuf /*canonical*/, dev: u64, ino: u64 }
```

- The identity key is the tuple **(pid, start_time, uid, exe.dev, exe.ino)**.
  - Any comparison across samples (CPU-time progress, idle duration) must match the full tuple.
  - A mismatch resets the derived claim to UNKNOWN. It is never read as "the process exited".
- `ps` is invoked with explicit columns only. Using `command`/`args` columns is a review-blocking defect.
- The 1 s `lstart` resolution leaves a residual PID-reuse window. It is harmless because identity never removes a veto (C1).
- An optional `libc::proc_pidinfo` path (µs start time) would add a direct `libc` dependency, which AGENTS.md requires approval for. It is not needed for correctness.

**Active-build signal that scales.** Cargo holds `<target>/<profile>/.cargo-lock` for the duration of a build.
- Probe the specific lock files with `lsof -F pcfn <target>/*/.cargo-lock` — no recursive `+D`. A holder there = ACTIVE, OBSERVED.
- This covers "detached still-active build". It also avoids the 5 s `REVALIDATION_TIMEOUT` (`executor/mod.rs`) that `lsof +D` will exceed on a ~500 GiB tree.
- `+D` is retained. Its timeout still fails closed, as Partial evidence → ASK.
- **Verify the lock-file behaviour against the installed cargo 1.98 in HORO-1823's fixtures; until then it is INFERRED.**

**Claims** (`ProcessClaim`, explanation only, attached for reporting):

| Claim | Basis (all must hold) | Policy effect |
|---|---|---|
| ACTIVE | holds build lock, or CPU time advanced between two same-tuple samples | holder => existing `ResourceInActiveUse` |
| IDLE_BUT_VALID | alive, supervisor = LaunchdJob **or** exe is a recognized configured dependency (§4.3), no CPU progress | holder => `ResourceInActiveUse` |
| STALLED | same tuple across window, no CPU progress, still holds lock/files | holder => `ResourceInActiveUse` |
| ABANDONED_CANDIDATE | same tuple, no progress, supervisor None/Unknown, no lock | **none removed**; holder => `ResourceInActiveUse` |
| UNKNOWN | any probe failed, tuple mismatch, zombie, lsof failure, remote-state contradiction | holder => `ResourceInActiveUse`; probe failure => Partial => ASK |

A single-sample pass has no progress dimension, so its claims are UNKNOWN or IDLE_BUT_VALID only. External state (Jira Done, branch merged) may appear in the explanation but never in the claim basis (C6).

### 4.2 Cargo target ownership (HORO-1824) — new kind-specific `Evidence` field

```text
cargo_target_scope: Option<ProbeOutcome<CargoTargetScope>>   // Some(..) only for CargoTargetDir
CargoTargetScope {
  resolved_target: PathBuf,         // canonical `target_directory`     [INFERRED: resolved in Glomeris env]
  resolved_build_dir: Option<PathBuf>, // `build_directory` if != target  [INFERRED; report-only]
  workspace_root: PathBuf,          // `workspace_root`                  [INFERRED]
  scope: Local | Orphan | Redirected,
}
```

- Resolver: `cargo metadata --format-version 1 --no-deps --offline` run with `current_dir = configured root`.
  - Environment: `RUSTUP_AUTO_INSTALL=0` and `CARGO_TARGET_DIR`/`CARGO_BUILD_TARGET_DIR` removed. This gives the config-resolved answer.
  - Glomeris's own environment values are recorded separately as INFERRED.
  - It uses the existing timeout runner. It is verified on this host to return `target_directory` = `~/.cargo/shared-target`, `build_directory` and `workspace_root`.
  - HORO-1824 must prove by fixture that it creates or modifies no files.
- Scope:
  - **Local** — canonical(resolved_target) is under canonical(workspace_root), and the locator equals it.
  - **Orphan** — the locator is `<root>/target`, but the root resolves elsewhere (pre-shared-config leftovers).
  - **Redirected** — canonical(resolved_target) is outside workspace_root. That covers a global config, a nested config, or a symlinked-out `target/`. Fan-out is unbounded: any workspace on the machine may share it.
- Fan-out count (n configured roots resolving to the same (dev, ino)) is computed in reporting for display only.
- **Dedup takes the most-protective scope, not the first-discovered one.** When discovery widening (below) deduplicates multiple configured roots onto the same (dev, ino), the merged resource's `scope` is the most-protective value across every contributing root's independent `cargo metadata` resolution (`Redirected` > `Orphan` > `Local`), not simply the first root's answer. Otherwise a root whose own resolution happens to be `Local` could mask a second root that independently redirects into the same physical directory, hiding that second root's dependency from policy. (Flagged by independent review.)
- **Not observable after the fact**, so permanently residual and never a reason to upgrade: per-invocation `--target-dir`/`--config`, other shells' environment, cwd-dependent nested configs. These are why Redirected is never AUTO_SAFE.
- cargo absent, timeout or parse failure → `Unavailable` → (as a required field) Partial → ASK `TargetOwnershipUnknown`.
- **Discovery widening:** CargoDetector additionally emits the resolved target when it differs from `<root>/target`. Results are deduped by (dev, ino); `source_project_root` = the first root, used only for `--manifest-path`. This newly surfaces the shared target. **It must not merge before §4.3 is on `main`.**

### 4.3 Executable dependency (HORO-1825) — new correlation field

```text
executable_dependency: ProbeOutcome<ExecutableDependencyReport>   // run for every Path locator
ExecutableDependencyReport {
  references_inside: Vec<DependencyRef>,   // non-empty => PROTECTED
  running_inside: Vec<ProcessRef>,         // relation == Executable => ASK
  unresolved: Vec<UnresolvedRef>,          // non-empty => UNKNOWN (for executable-bearing kinds)
  sources_examined: Vec<SourceTag>,        // e.g. "claude_user_settings", never raw content
}
DependencyRef {
  source: SourceTag,                       // claude_user_settings | claude_user_local | claude_managed |
                                           // claude_project(<root alias>) | codex_hooks | codex_notify |
                                           // launch_agent(<label alias>) | path_entry
  pointer: String,                         // structural location, e.g. "hooks.PostToolUse[0].hooks[1]"
  resolved: PathBuf,                       // canonical, after full symlink chain (<=40 hops; each hop checked)
  exe: ExeIdentity,                        // dev/ino of resolved file
  provenance: Observed (absolute or ~ path) | Inferred (bare name via Glomeris PATH),
  next_invocation: bool,                   // true for config refs; false for running-only
}
```

**Recognized sources** form a closed list. All are opened read-only; JSON via `serde_json`, plists via `plutil -convert json -o - <file>` written to stdout.
- Claude Code:
  - `~/.claude/settings.json`, `~/.claude/settings.local.json`
  - managed settings, if present
  - each configured root's `.claude/settings.json` and `.claude/settings.local.json`
  - Fields read: `hooks.*[].hooks[].command` and `statusLine.command`.
- Codex:
  - `~/.codex/hooks.json` (present on this host; **its schema must be confirmed from the installed Codex version in HORO-1825**).
  - `notify` in `~/.codex/config.toml`. Parsing it **requires the `toml` crate; that dependency needs approval**. Until it is approved, a present `config.toml` is reported as an `unresolved` source (fail closed).
- User LaunchAgents: `~/Library/LaunchAgents/*.plist`, keys `Program` / `ProgramArguments[0]`.
- **Explicitly out of scope (known gap, shown in the UX):** MCP server `command` entries (`~/.claude.json`, Codex `mcp_servers`) and shell rc files. A *running* MCP server inside a resource is still caught by the `Executable` relation. Follow-up ticket recommended.
- **The recognized-source list is closed, and an unlisted source degrades silently rather than failing closed.** An unrecognized hook mechanism (a third-party agent's own config, a shell profile sourcing a wrapper, an MCP entry per above) produces no `DependencyRef` and no `unresolved` entry — it simply isn't examined. `sources_examined` is exposed for display so the UX (HORO-1828) can show which sources were actually checked, but the design does not otherwise warn when that set is known to be incomplete for a given host. (Flagged by independent review; not fixed here — see §10 alternative 11.)

**Command tokenization** applies only to simple forms: `[NAME=VALUE ...] head [arg ...]`, with leading environment assignments skipped and their values **discarded, never stored**.
- Any `$`, `` ` ``, `|`, `;`, `&`, `>`, `<`, `(`, glob or unbalanced quote => the whole command is `UnresolvedRef::NonInspectable`.
- The head and every argument that is an absolute or `~/` path are dependencies.
- A bare-name head is resolved against Glomeris's PATH (INFERRED). Not found => `UnresolvedRef::NotOnPath`. Found => **still `UnresolvedRef::PathResolutionDivergent`, not a clean negative**, unless the recognized source itself records an absolute path or Glomeris can prove its resolution environment matches the hook runtime's (e.g. the same `~/.claude/settings.json` process reads its own PATH — not assumed). A bare-name match resolved only via Glomeris's own environment is not sufficient to conclude "no reference," because the hook runtime's actual PATH (different shell init, different launchd environment, or a PATH entry that is itself inside the resource) can diverge and resolve the same name into the target while Glomeris's own PATH resolves it elsewhere. The `HOOK_BIN_B` case (not found on the capturing shell's PATH) still lands in `NotOnPath`; a bare-name match found on Glomeris's PATH is a weaker finding that must stay `unresolved`/UNKNOWN, not a confirmed negative. (Flagged by independent review: this closes a PATH-shadowing bypass where a resource with a real executable dependency could otherwise reach AUTO_SAFE.)
- A relative path argument that is neither absolute nor `~/`-prefixed (contains `/` but doesn't match either form, e.g. `./target/debug/my-hook`, `../shared-target/release/hook`) is **not silently dropped**. It has no defined resolution cwd at probe time, so it is `UnresolvedRef::NonInspectable` unless and until a defined cwd (the resource's own root, or the source config's own location) is established for that source type. (Flagged by independent review: the original tokenization rule had no classification for this case at all, which meant it was neither a `DependencyRef` nor an `unresolved` entry — a silent, unflagged gap rather than a fail-closed one.)
- A PATH entry that is itself inside the resource => `DependencyRef{source: path_entry}` (PROTECTED), because the resource is then a PATH provider.

**Match rule:** canonical(resolved), or any hop in its symlink chain, is component-wise under the resource's canonical locator. Same-device is checked against `st_dev`.
- Hardlinks elsewhere do not count: deleting the target's link does not break a path outside it.
- `cargo install` manifests (`~/.cargo/.crates2.json`) are **not** used: `cargo install` copies into `$CARGO_HOME/bin` and never points into a target.

**Never captured:** raw command strings, environment values, file contents beyond the extracted fields, credential helpers' output.
- Never executed: no hook, `--version` or `which` subprocess.
- Never written: guarded by a new `scripts/check-host-dependency-probe-is-read-only.sh` (modelled on `check-external-context-is-read-only.sh`), plus a test asserting host config bytes and mtime are unchanged.

**Optional content digest** via `/usr/bin/shasum -a 256` (bounded): UX/HORO-1826 identity only, never authority. Matching never uses names, versions or digests.

### 4.4 Measurement (HORO-1826 / 1827)

- **Volume identity.**
  - `VolumeSample { unix_secs, volume_dev, total_bytes, free_bytes }`. `volume_dev` = `st_dev` of the measured mount: an opaque number, no path. OBSERVED.
  - "Different filesystem" <=> the resource's `st_dev` != the measured volume's `st_dev` => the measurement does not apply => UNVERIFIED.
- **Snapshots.** `local_snapshots: ProbeOutcome<u32>` from `tmutil listlocalsnapshots /`, read-only (OBSERVED count).
  - Count > 0 => the report states that freed bytes may not return until macOS thins snapshots.
  - Glomeris never deletes snapshots: `tmutil deletelocalsnapshots` is not and will not be a registered action.
- **Per-action delta.** `ExecutionReport.volume_free_delta: ProbeOutcome<i64>` from statvfs immediately before and after.
  - It is signed, and labelled "includes concurrent writers".
  - `actual_reclaimed_bytes` keeps its meaning and stays `Unavailable` for `RunTool`.
- **Estimates are upper bounds** and differ from df because of hardlinks (Cargo uplifts binaries by hardlink), APFS clones, purgeable space and snapshots.
  - HORO-1824 should make `estimate_logical_bytes` count each inode with `st_nlink > 1` once, tracked in a bounded set.
  - That changes the estimate for every kind, so it needs a before/after fixture.
- **Host integrity (HORO-1826).** The §4.3 report is taken before and after the run, per dependency:
  - **OK** — same resolved path and same (dev, ino).
  - **DEGRADED** — was present, now missing or changed.
  - **UNVERIFIED** — either probe failed, or the run crashed before the post-probe.
  - The run-level status is OK only if every dependency is OK **and** `sources_examined` is complete. Otherwise "not verified", never "all integrations OK".

## 5. Policy changes (`src/policy/*`, all exhaustive-match forced)

| New `ReasonCode` | Class | `as_str` | Evidence-quality (-> UNKNOWN_INCOMPLETE label) | `is_preauthorizable` |
|---|---|---|---|---|
| `ProtectedExecutableDependency` | PROTECTED | `protected_executable_dependency` | no | n/a (refused by `authorize`) |
| `ExecutableRunningFromResource` | ASK | `executable_running_from_resource` | no | **false** |
| `ExecutableDependencyUnknown` | ASK | `executable_dependency_unknown` | **yes** | **false** |
| `SharedBuildCacheBlastRadius` | ASK | `shared_build_cache_blast_radius` | no | **false** (founder may loosen later; loosening is the safe direction to defer) |
| `TargetOwnershipUnknown` | ASK | `target_ownership_unknown` | **yes** | **false** |

New `EvidenceField`s:
- `ExecutableDependency` is required for CargoTargetDir, SwiftPackageManagerBuildDir, NodeModules and XcodeDerivedData. For these kinds, `Unavailable` => Partial => ASK.
- `TargetOwnership` is required for CargoTargetDir.
- For other kinds the dependency probe still runs, and a positive match is still PROTECTED. Its failure does not block those kinds. They are download caches: INFERRED and documented.

`classify` ordering:
- **Step 2b (new)**, after `protected_reason` and before staleness: if `executable_dependency` is `Observed` with `references_inside` non-empty => PROTECTED `[ProtectedExecutableDependency]`. A stale positive is still protected, matching step 2's rationale.
- **Step 6 (extended)**, in this order:
  1. `running_inside` non-empty => `ExecutableRunningFromResource`.
  2. `unresolved` non-empty for an executable-bearing kind => `ExecutableDependencyUnknown`.
  3. Scope `Redirected` => `SharedBuildCacheBlastRadius`.
  4. Scope `Unavailable` is already caught by completeness => `TargetOwnershipUnknown`. Map `Partial{missing: [TargetOwnership]}` to this specific code rather than to the generic `EvidenceIncomplete`.

Other required edits:
- The `policy_version` literal in `engine.rs` goes 1 -> 2.
- `label_for` gains the two evidence-quality codes.
- Swift wording must be added for all five `as_str` values in the same PR, or `scripts/check-vocabulary-covers-cli-tokens.sh` fails by design.

## 6. Threat model

| Threat | Today | After this ADR | Residual (accepted, documented) |
|---|---|---|---|
| **TOCTOU (filesystem)**: resource replaced or symlink-swapped between plan and mutation | Early `symlink_metadata` (dev, ino) snapshot; fingerprint compare; `verify_identity_unchanged` before spawn or delete | Unchanged. New probes run inside `build_fresh_evidence`, *after* the snapshot and *before* the final verify | stat->spawn->cargo's own re-resolution of `--target-dir` (already documented in `executor/mod.rs`) |
| **TOCTOU (dependency)**: a hook is added to config after planning | Not modelled | Dependency probe re-runs at revalidation. A new reference => PROTECTED => class change => abort | Config edited in the seconds between revalidation and spawn |
| **TOCTOU (process)**: a build starts after planning | Holder re-probed at revalidation | Plus the build-lock probe. *Any* holder vetoes, regardless of identity | A build starting after revalidation and during `cargo clean`. Whether `cargo clean` takes the build lock is UNVERIFIED; HORO-1824 fixture must determine. **Additional residual, flagged by independent review:** "verified absence" (C1) relies on `lsof`, which cannot see another UID's processes without elevated privileges and cannot see a container bind-mount writer. This is a more plausible false-"verified-absent" path than process-identity spoofing (which the (pid,start_time,uid,exe) tuple correctly neutralizes by never letting identity alone remove a veto) — a cross-UID or containerized holder can be genuinely invisible to the probe, not merely unidentified. Documented as accepted residual risk, not fixed here. |
| **PID reuse** | pid-only `ProcessRef` | Full tuple for every cross-sample derivation; mismatch => UNKNOWN; identity never removes a veto | 1 s `lstart` resolution; harmless by construction |
| **Symlink races and chains** in hook resolution | n/a | Full chain (<=40 hops), every hop matched; a loop or overflow => NonInspectable => UNKNOWN | — |
| **Shared-cache cross-worktree interference** | Invisible (a shared target is only found through a symlink) | `Redirected` => ASK, not pre-authorisable; blast radius displayed; native clean is whole-directory (§9) | User-consented clean still cold-rebuilds every sharer |
| **Naming spoofing / false match** | n/a | No name, version or digest heuristics; path and inode only. A test-owned false-match path is in the HORO-1825 AC | — |
| **CACHEDIR.TAG trusted as authority** | Not read by policy | Remains a hint only; never auto-created (HORO-1824). A Cargo refusal stays a refusal | — |
| **Privacy: paths, process names, argv secrets** | lsof `c` only; LLM aliasing (HORO-1298) | argv never collected; command strings and env values never stored; launchd labels and paths aliased or `~`-sanitized in reports; none of the new fields enter `src/planner/dto.rs`, enforced by extending `tests/llm_plan_egress_privacy.rs` | Local JSON contains `~`-relative paths (existing contract) |
| **Glomeris mutating host config** | n/a | Read-only open; guard script; preservation test | — |
| **LLM obtaining delete/kill authority** | Seal + `deny_unknown_fields` + registry | Unchanged; no new action IDs; `tests/golden_llm_plan_protected_refusal.rs` extended with a hook-protected case | — |

## 7. State transitions

```text
discover (detectors) --> Evidence{... executable_dependency, cargo_target_scope, ProcessRef.identity}
        |
        v
classify (pure)
  1 Unknown kind -------------------------------> PROTECTED
  2 protected_reason(path text) ----------------> PROTECTED
  2b references_inside != {} -------------------> PROTECTED (ProtectedExecutableDependency)
  3 stale ---------------------------------------> ASK (EvidenceStale)            [UNKNOWN_INCOMPLETE]
  4-5 Failed/Partial (incl. new required fields) -> ASK (...ProbeFailed/Incomplete/TargetOwnershipUnknown) [UNKNOWN_INCOMPLETE]
  6 holders / running_inside / unresolved / Redirected / git / tool / docker -> ASK (vetoes, non-preauthorizable)
  7-8 recoverability / regenerability -----------> ASK
  else -------------------------------------------> AUTO_SAFE
        |
        v
authorize -- PROTECTED => None (no override exists)
          -- ASK => Some only with fingerprint-pinned UserConsent, or Autopilot pre-auth (RebuildCostHigh only)
          -- AUTO_SAFE => Some
        |
        v
autopilot::gate (narrowing only) --> execute:
  0 snapshot (dev,ino) --> build_fresh_evidence (ALL probes re-run) --> fingerprint == ?
  --> classify again: class == AND no reason added ? --> plan --> structural_refusal --> verify_identity_unchanged
  --> spawn `cargo clean --manifest-path ... --target-dir <exact>`
  --> post: statvfs delta, host-integrity post-probe (1826) --> report OK | DEGRADED | UNVERIFIED
Any "no" => AbortedByRevalidation, nothing mutated. A process claim never appears on any arrow.
```

No path widens ASK or PROTECTED to AUTO_SAFE except a *later, independent* discovery pass whose fresh evidence classifies AUTO_SAFE on its own.

## 8. Failure behavior (fail-closed, enumerated)

| Failure | Result |
|---|---|
| lsof missing, timeout or stderr failure (`+D` or lock probe) | `Unavailable` => Partial => ASK `EvidenceIncomplete` (UNKNOWN_INCOMPLETE) |
| `ps`/`launchctl` failure, `lstart` parse failure, zombie | identity `Unavailable`; claim UNKNOWN; holder still vetoes |
| Recognized config present but unreadable, unparseable or schema-unknown | `unresolved` => ASK `ExecutableDependencyUnknown` (executable-bearing kinds) |
| Recognized config absent (ENOENT) | `Observed`, no references from that source (C4) |
| Non-inspectable command, bare name not on PATH, symlink loop | `unresolved` => ASK `ExecutableDependencyUnknown` |
| Codex `config.toml` present while `toml` is not approved | `unresolved` => ASK |
| `cargo metadata` absent, timeout or failure | ASK `TargetOwnershipUnknown` |
| Dependency or scope field missing in revalidation evidence | Completeness degrades => class or reasons change => abort (never execute) |
| Post-action health probe fails or the run crashes before it | Host integrity UNVERIFIED; never "all OK" |
| statvfs fails | Existing `StopReason::Error`; no delta reported |
| Measured volume != resource volume | Delta UNVERIFIED for that action |
| Sample gaps, or an observation span under 12 h | `INSUFFICIENT_OBSERVATION`; never a stability verdict |
| SQLite or history write fails | Ignored (existing best-effort contract; `emergency` unaffected) |

## 9. Ownership model

- **Target directory.**
  - Owned by Cargo. Configured roots whose resolution lands on its (dev, ino) are its *known* sharers.
  - A **Redirected** target is owned by *all* Cargo workspaces whose effective config resolves to it, a set Glomeris cannot enumerate.
  - The native clean scope is the whole directory. `cargo clean -p` cuts across sharers that use the same package names, so it is deferred (§10).
  - Consequence: one installed hook binary anywhere in a shared target makes the **entire** shared target PROTECTED for native clean.
  - On this host today that is the expected outcome for `~/.cargo/shared-target`, until the hook owner installs the binary outside the cache.
- **Process.** Owned by whoever started it. Glomeris never signals, restarts or reparents any process.
- **Installed executable.** Owned by the product that configured it (the coding-agent-environment and product owners). Glomeris may report "your hook references a file inside a disposable cache" and refuse. It may not copy, reinstall, relocate or rewrite.
- **Cold-rebuild blast radius** (HORO-1824/1826/1828 must display it):
  - Cleaning a Redirected or shared target invalidates incremental and dependency artifacts for every sharer.
  - The next build in each sharer is a cold rebuild.
  - The ~/CLAUDE.md incident write-up already records concurrent builds invalidating each other's caches with a shared `target-dir`.
  - Bytes freed are temporary when sharers are active: HORO-1827 must show regrowth.

## 10. Rejected alternatives

1. **Name or layout heuristics** (protect `*-hook*`, executables in `<profile>/`, anything with the exec bit).
   - Filenames are explicitly non-authoritative.
   - They over-protect (every built binary) and under-protect (a hook named anything else).
   - They fail the HORO-1825 "false-match path" case.
2. **CACHEDIR.TAG as authorization**, or auto-creating it to satisfy Cargo.
   - The tag means "regenerable cache", not "nothing depends on this".
   - The live hook binary sits in a tagged Cargo target.
   - Auto-creating it launders a refusal into a permission.
3. **Process reclamation**: a typed `kill`/`Signal` action for ABANDONED_CANDIDATE processes.
   - It creates an LLM-reachable destructive capability over peer products.
   - PPID=1 is ambiguous on macOS, PID reuse exists, and "no CPU" describes every idle daemon.
   - The brief forbids it.
4. **pid-only or `pgrep`-name liveness as authority.**
   - PID reuse; name matching is the HORO-1562 failure mode ("inapplicable probe became a negative observation").
5. **"Repair" mode**: copy the hook binary out of the cache and rewrite `settings.json`.
   - It modifies another product's configuration and possibly credentials.
   - It crosses the ownership boundary.
   - The brief forbids it.
6. **Time-based widening**: ASK becomes AUTO_SAFE after N hours idle or after the 12 h observation.
   - It is automatic widening, and observation is not consent.
   - HORO-1827 explicitly says "existing Autopilot grant only".
7. **A 4th `PolicyClass::Unknown`.**
   - It contradicts the documented design in `policy/class.rs`.
   - Every consumer would need a new arm, risking "not Protected => fine".
8. **Hand-parsing `.cargo/config.toml`, or assuming `<worktree>/target`.**
   - It re-implements cwd-based hierarchical merge, environment and `--config` precedence, and `build-dir`.
   - It needs a TOML parser anyway.
   - `cargo metadata` is the native answer.
9. **`DeletePath` on sub-directories of a shared target** (around the hook binary) **or `cargo clean -p`.**
   - That is a new broad deletion primitive. HORO-1824: "a different native procedure requires independent proof and review."
   - Deferred, not rejected forever.
10. **Workspace or Jira state as a policy input** ("ticket Done => target disposable").
    - Barred by `scripts/check-workspace-aggregation-has-no-authority.sh`; the brief bars destruction based on old Jira status.
11. **Owner-declared additional protected sources/paths**, as a remediation for the closed recognized-source list (§4.3).
    - Considered, **deferred not rejected**: a user- or config-declared "also treat this path/config as a dependency source" escape hatch would close the unlisted-source gap without widening the closed list itself.
    - Deferred because it is itself a new trust surface (a declaration mechanism needs its own abuse analysis — e.g. can it be used to falsely mark something protected to block legitimate cleanup, which is a nuisance not a safety issue, versus falsely *un*-marking something, which the design doesn't allow since declarations could only ever add protection, never remove it). Worth its own ticket if the closed-list gap proves material in practice. (Added per independent review.)

## 11. HORO-1629 (independent bug, blocks HORO-1829)

- **Ticket AC (verbatim, AC1-5),** summarized:
  - AC1: the user is told when a parent folder was itself what got examined.
  - AC2: no change in what a correctly configured project root reports.
  - AC3: no unbounded traversal.
  - AC4: partial reads are reported as incomplete.
  - AC5: **the founder reviews the chosen wording.**
  - The ticket itself leans toward option 1 (validate at configuration time) plus option 2 (report examined roots), and labels this `needs-founder-decision`.
- **Brief-stated founder preference** (not yet on the ticket):
  - Warn when a configured root looks like a parent of projects.
  - Report exactly which roots were examined.
  - No automatic recursive discovery as a default.
- **Assessment:** these are not in conflict.
  - The preference matches options 1 + 2 and rejects option 3 as a default.
  - It satisfies AC1-4: option 1 is a one-level read of a folder the user chose (AC3); option 2 reads nothing new (AC2).
  - AC5 is still open.
- **Recommendation:**
  - Draft candidate wording for the Settings warning and for the "examined roots" line in `detect`/`free`/GUI. Reuse `RecoveryReport::detectors_not_examined`-style single-producer wording.
  - Attach both strings to HORO-1629 for explicit founder sign-off.
  - **HORO-1629 is not mergeable, and HORO-1829 cannot pass, without that sign-off.**
  - Record the preference on the ticket itself, so the decision trail lives in Jira.
- HORO-1824 and HORO-1828 reference HORO-1629 in prose only. Do not duplicate its scope.

## 12. Phase rollout and AC map

### Ordering (adds one edge the Jira DAG does not encode)

- **Phase 0 — HORO-1822:** this ADR approved, plus security review. No code.
- **Phase 1 — HORO-1825 (Highest), first:**
  - The §4.3 probe; the lsof `f` field / `Executable` relation; two reason codes; required field.
  - Plumbing in `build_fresh_evidence`; read-only guard script; Swift wording; book updates (`evidence_model.md`, `safety_model.md`, `known_limitations.md`).
- **Phase 1 (parallel) — HORO-1827:** `monitor/`, `platform/macos/statfs.rs` (volume_dev), the `tmutil` probe, and a samples ring. Independent files.
- **Phase 2 — HORO-1823:** `ProcessRef` identity, the build-lock probe, the launchd supervisor, and claims (reporting).
  - Sequence it after 1825 merges: both edit `ProcessRef`, `CorrelationResult` and `merge_into`.
- **Phase 2 — HORO-1824:** `cargo metadata` resolver, scope, two reason codes, discovery widening, hardlink-deduplicated estimate, df-vs-du display.
  - **The discovery-widening commit must not merge before 1825 is on `main`.**
- **Phase 3 — HORO-1826:** pre/post host integrity on `RecoveryReport` / `cli/recovery.rs::RecoveryRunReport`; per-action `volume_free_delta`.
- **Phase 4 — HORO-1828:** DTOs, Swift and golden fixtures.
- **Phase 5 — HORO-1829:** independent QA; then HORO-1073.
- HORO-1629 runs in parallel, gated on the founder.

### AC map (acceptance prose split into its clauses; no invented numbering)

**HORO-1822** — see §0.

**HORO-1823**
- "detached still-active build protected" -> §4.1 build-lock probe; §7 step 6.
- "legitimate idle daemon protected" -> §4.1 IDLE_BUT_VALID; the holder veto is unchanged.
- "verified inactive disposable target can lose one active-use veto after fresh evidence" -> C1; §7 (only verified absence, only via a fresh pass).
- "PID reuse, zombie, failed lsof, unknown supervisor and remote-state contradiction remain uncertain" -> §4.1 tuple and claims table; §8.
- "Mutation removal of active-use guard must turn test RED" -> §13 anti-vacuity list.
- "Unknown/incomplete policy fail closed" -> §8.
- "No live peer process is killed" -> §3.1 (no signal step exists); §10.3.
- "Independent security review" -> §12 gate.
- Boundaries ("no global pgrep-based authority, no new process manager, no shell execution, no privileged scan, no model-facing raw identifiers") -> §4.1; §6 privacy row; §14.

**HORO-1824**
- Fixtures "symlink swap, wrong manifest, target override, parent repo, no manifest, missing CACHEDIR.TAG, active shared target, stale completed worktree, installed executable" -> §4.2, §4.3, §6, §13.
- "No active source/unpushed or persistent state deletion" -> the existing `GitWorktreeDirty` and protected paths are unchanged; the only mutation is exact `--target-dir`.
- "Display actual df delta versus du potential and cold-rebuild blast radius" -> §4.4; §9.
- "Mutation tests and independent review" -> §13; §12.
- "Relate existing HORO-1629 rather than duplicate it" -> §11.

**HORO-1825**
- "Test-owned fake hook config pointing into target prevents AUTO_SAFE; unreferenced target can remain eligible" -> §4.3; §5 step 2b.
- Cases "version-identical/digest-different binaries, PATH shadowing, missing executable, stale symlink, non-inspectable dynamic command, false-match path, absent native tool" -> the §4.3 match and tokenization rules; §8.
- "Anti-vacuity mutation detects missing dependency guard" -> §13.
- "Read-only host-config preservation verified" -> §4.3 guard script and preservation test.
- "Security review and runtime bound" -> §12; §14 (per-probe timeouts, bounded symlink hops, bounded source list).

**HORO-1826**
- "negative control records DEGRADED/missing binary; valid unchanged hooks remain OK" -> §4.4 host integrity.
- "Unknown probes never show all-clear" -> §4.4; §8.
- "Simulate process exit after deletion, crash before health probe, concurrent target regrowth and different filesystem" -> §4.4 (UNVERIFIED rules, signed delta, volume_dev); §8.
- "Text-only source SHA and executable identity" -> the existing HORO-1612 `--version` SHA; `ExeIdentity`.
- "bounded tests, independent security review" -> §13; §12.

**HORO-1827**
- Fake-clock tests "healthy, volatile, falling, missed sample, rapidly growing Cargo, APFS accounting differences and target unreachable" -> §4.4; §14 (sampling uses the existing `Clock` seam).
- "No double count; measured vs predicted clearly separate" -> §4.4 (inode-deduplicated estimate, signed delta).
- "No new scheduler/daemon if existing one suffices" -> §14 (sample on every k-th tick of the existing 60 s poller, opt-in).
- "storage bounded and privacy-safe" -> a fixed-row ring; no paths.
- "Verify 12h actual observation only after time elapsed; never fabricate" -> `INSUFFICIENT_OBSERVATION` (§8).
- **Honest 1-TiB feasibility (INFERRED):**
  - Gap = 1024 - 296 ~= 728 GiB.
  - Jira's earlier Cargo totals are ~109 + ~525 ~= 634 GiB, which is below the gap even before discounting.
  - The ~525 GiB shared target is PROTECTED while a hook binary lives in it.
  - Expected verdict: **target unreachable by safe means**, with the gap, the per-class opportunity split, and regrowth risk shown.

**HORO-1828**
- Golden fixtures "incomplete scan, legit idle, detached build, shared active target, missing hook, target already reached, target unreachable" -> §4.1, §4.3, §4.4; `tests/dto_golden_fixtures.rs`.
- "Distinguish NOTHING_ELIGIBLE vs DISCOVERY_INCOMPLETE vs UNKNOWN" -> derived from the existing `StopReason::SafeExhausted(RemainingCandidates)` plus `detector_failures`/`detectors_not_examined` plus the label. No Swift inference (`scripts/check-no-policy-label-branching.sh`).
- "expose actual refusal codes" -> the §5 `as_str` values.
- "preserve VoiceOver/keyboard behavior" -> Swift only.
- "Tie configuration-folder false-all-clear to existing HORO-1629" -> §11.
- C7 explains the guarded-bytes share from linked worktrees.

**HORO-1829**
- "Real positive scoped native cargo clean of inactive test target with measured df delta" -> §4.4 `volume_free_delta`.
- Hard negatives "live Cargo, detached active build, valid idle daemon, PID race, wrong manifest/CACHEDIR.TAG, unpushed worktree, unknown lsof, symlink swap, installed binary in cache" -> §4.1-4.3; §6.
- "simulate ENOSPC, partial detectors, post-action check failure" -> §8.
- "Verify no LLM can produce delete authority" -> §3.1; §6.
- "mutation detection on each critical guard" -> §13.
- "12h watch evidence when actually observed" -> §8.
- "benchmark bounded memory/time and no per-tick inference" -> §14.
- "clean clone Rust/Swift applicable gates and independent security review" -> §12.
- Exit "VERIFIED/UNVERIFIED/FAILED with exact SHA", "Do not kill peers or mutate shared live host config during QA", "RC only; no Tag/Release without founder GO" -> process gates, unchanged by this ADR.

## 13. Required tests (anti-vacuity)

Each of the following mutations must turn a named test RED:
1. Delete step 2b in `classify` -> the fake-hook-config test.
2. Delete the `process_active` check -> the detached-build test.
3. Make the dependency probe return `Observed(empty)` on parse error -> the unparseable-config test.
4. Drop `executable_dependency`/`cargo_target_scope` population in `build_fresh_evidence` -> **every** real-execution positive control aborts.
   - Example: the existing `execute_real_run_tool_cleans_cargo_target_dir`.
   - This is the HORO-994 trap. The positive control must keep passing *with* the population in place.
5. Return `true` from `is_preauthorizable` for any new code -> the envelope test.
6. Map `Redirected` to no veto -> the shared-target test.

Fixtures live under `tests/fixtures/` or temp directories. Never read the developer's real home directory: host-config paths come through `DiscoveryContext`, following the `ToolEnvVar` precedent.

## 14. Integration boundaries (real modules)

- **In scope**
  - `src/evidence/model.rs` — `ProcessRef`, new structs, `EvidenceField`s, required-evidence lists.
  - `src/evidence/correlate/{mod.rs, open_files.rs, process.rs}`, plus new `host_dependency.rs` and `process_identity.rs`.
  - `src/detectors/cargo.rs` — resolver and discovery widening.
  - `src/policy/{class.rs, engine.rs}`.
  - `src/autopilot/envelope.rs` — `is_preauthorizable`.
  - `src/reporting/{policy_label.rs, dto.rs}`.
  - `src/executor/{mod.rs, recovery_loop.rs}`; `src/cli/recovery.rs`. **Includes `completeness_rank_from_decision`** (flagged by independent review): it must recognize `TargetOwnershipUnknown` as a degraded-completeness reason alongside the existing `EvidenceProbeFailed`/`EvidenceIncomplete`, or a plan consented to under `TargetOwnershipUnknown` computes `planned_rank = 0` while fresh revalidation correctly computes a higher rank, tripping a spurious `EvidenceDegraded` abort on every execution of that class. Fails closed either way (nothing unsafe happens), but leaves a whole action class permanently unexecutable until fixed — a one-line addendum, not a design change.
  - `src/monitor/{poller.rs, persistence.rs, fs_stat.rs}`; `src/platform/macos/statfs.rs`.
  - `src/settings/` — the opt-in sampling flag only.
  - `macos/GlomerisMenuBar` — wording only.
  - `book/src/*` and `scripts/` — the new read-only guard.
- **Explicitly unchanged**
  - `src/policy/approval.rs` (`Seal`/`authorize`).
  - `ActionStep`, `ToolBinary`, `src/actions/*` — no new action IDs.
  - `src/planner/*` and `src/actions/llm.rs` — no new egress.
  - `src/workspace/**` — stays no-authority.
  - `src/emergency/mod.rs` — inherits the new vetoes automatically through `classify`. Nothing in it is modified.
- **Bounds**
  - Each new subprocess runs under the existing `ProbeBudget`/`run_with_timeout`.
  - Symlink hops <=40; the recognized-source list is closed.
  - No background inference per tick: sampling is statvfs only.
- **Dependencies**
  - None required, except `toml` (Codex `notify`), which is pending approval.
  - `libc` (sub-second start time) is optional and not recommended.

## 15. Compatibility and rollback

- **Behaviour change (intended):**
  - Some previously AUTO_SAFE Cargo, SwiftPM, node and Xcode resources become ASK or PROTECTED.
  - Autopilot reclaims less.
  - Release notes must say so.
- **`policy_version` 1 -> 2.**
  - An outstanding `explain`->`execute` consent now meets new reasons; the executor aborts with `PolicyReasonsWidened` (safe).
  - Fingerprint tokens are unchanged (`v1:`).
- **Serialization.**
  - New reason tags appear in `--json` and in the `actions.jsonl` `policy_label` field. Labels are unchanged.
  - Readers are already tolerant of unknown strings (`AuditRecord.source` precedent).
- **Test churn.** `Evidence` has no `Default` by design, so every literal constructor in tests gains the new fields. Do it mechanically, in the same PR as the field.
- **Rollback.**
  - Each ticket lands as merge commits; revert per PR.
  - **Reverting HORO-1825 requires reverting HORO-1824's discovery widening first.** Shared-target discovery must never be live without the dependency guard.
  - Sampling (1827) is opt-in and off by default. Turning it off leaves a bounded ring file that `emergency` may delete as tool-owned state, following the HORO-1468 precedent.

## 16. Decisions reserved for the founder — RESOLVED 2026-10-10

1. **HORO-1629 final wording — APPROVED.**
   - Settings warning (exact, approved default):
     > "This folder appears to contain multiple projects rather than being a project root. Glomeris checks only the selected folder and does not automatically scan nested projects. Add each project separately to include its build artifacts."
   - Scan-coverage line (exact, approved default):
     > "Examined {count} configured project root(s). Nested projects were not scanned automatically."
   - Show the warning only when bounded evidence supports it (satisfies AC3/AC4: no new unbounded traversal, honest about partial/failed reads). No recursive discovery by default. Implement within the existing HORO-1629 — no duplicate ticket.
2. **Shared/redirected Cargo targets staying permanently non-preauthorizable — APPROVED, confirmed as previously defaulted.**
   - Explicit ASK (or PROTECTED, per evidence) is preserved; absence of active compilation is explicitly **not** sufficient evidence of cleanup safety on its own (consistent with §3.4/§3.5 and the claims table in §4.1 — no claim alone removes a veto).
   - A future instance-specific, manually-approved cleanup workflow is **not authorized by this ADR** — it would need its own ticket, its own verified resource identity, dependency analysis, and its own deterministic authorization path through the existing `policy::approval` seal. This ADR does not expand deletion authority.
3. **`toml` crate for Codex `notify` parsing — CONDITIONALLY APPROVED.** Before adding it, HORO-1825's implementation must, in order:
   - Confirm no already-vendored/transitive dependency can parse the subset of TOML needed (check first; don't assume `toml` is necessary).
   - If still needed: select a maintained version, record the justification in the HORO-1825 PR description.
   - Run `cargo deny check` (license/advisory/supply-chain — already required by CLAUDE.md) and record the `Cargo.lock` diff in the PR.
   - Parsing stays strictly read-only (consistent with §4.3's existing read-only guard and preservation test) — never executes a config value, never exposes a credential it happens to parse near.
   - Invalid/unrecognized security-relevant configuration is rejected, not guessed at (fail-closed, consistent with §8).
   - Positive, malformed, adversarial and fail-closed tests are added (folds into HORO-1825's existing §13 anti-vacuity test obligations).
   - If these checks pass, the crate may be added without a further approval round-trip. If it introduces a material new security risk, HORO-1825 keeps the existing fail-closed fallback (Codex `config.toml` present ⇒ `unresolved` ⇒ ASK, per §4.3/§8) and escalates the finding instead of merging it.
