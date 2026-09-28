# Autopilot

Autopilot is a standing grant with limits, written down where you can read it.

Every other deleting command in Glomeris is something you type at the moment
you want it: `execute` names one action on one resource, `free` runs a bounded
recovery loop you asked for, `emergency` is the button you press when the disk
is nearly full. Autopilot is the one that can act without you present — so the
whole of its design is about bounding what "without you present" is allowed to
mean.

The invariant the rest of this book states as

> AI can recommend. Policy decides. Executor verifies. Filesystem reality wins.

gains one clause here:

> …and the envelope bounds the outcome.

## The envelope

An `AutopilotEnvelope` is the complete statement of what Autopilot may do. It
is a small file of `key = value` lines at
`~/Library/Application Support/Glomeris/autopilot.conf`, and nothing else
grants authority — not an environment variable, not the plan you hand `run`,
and not the menu-bar app's Autopilot tab, which grants by running
`autopilot enable` and keeps no second copy of the answer. There is one grant
and it is that file.

| Field | Default | Hard ceiling | What it bounds |
|---|---|---|---|
| enabled | revoked | — | Whether `run` may execute anything at all. |
| allowed kinds | none | — | Which `ResourceKind`s a candidate may be. An empty allowlist reaches nothing. |
| max actions | 3 | 25 | How many actions one run may perform. |
| max bytes | 5 GiB | 64 GiB | The byte total one run may reclaim. |
| max duration | 60s | 900s | Wall-clock budget for one run. |
| min pressure | none | — | A disk-pressure floor below which the run refuses. |
| pre-authorized ASK | none | — | Exactly which `(kind, reason)` pairs an `ASK` classification may proceed on. |
| respond to alerts | off | — | Whether a run may *begin* without being asked. The one row that is not a ceiling on outcomes — see [Runs nobody asked for](#runs-nobody-asked-for). |

Two properties of that table matter more than the numbers in it:

**The defaults grant nothing.** `AutopilotEnvelope::default()` and
`::revoked()` are the same value, and a missing file reads as revoked rather
than as an error. Autopilot being off is not a setting someone has to
remember to choose; it is what the absence of a decision means.

**A limit above its ceiling is refused, not clamped.** Asking for 500 actions
is a usage error (exit 2) and writes no envelope. Clamping would be the worse
behaviour by a wide margin: you would have been told 500, be living under 25,
and have no way to notice the difference.

Reading the envelope is read-only in the strict sense — `glomeris autopilot`
with no arguments prints the grant and does not create the file it just read.

## What the envelope cannot authorize

`PROTECTED` is unconditional. No flag on `autopilot enable` can reach it, and
no ordering in a plan file can make a `PROTECTED` candidate executable. The
allowlist is a narrowing filter over what policy already permits, never a
widening one.

`UNKNOWN_INCOMPLETE` is likewise unreachable. It is the reporting label for an
`ASK` decision whose *evidence* is incomplete, stale, or came from a probe that
failed — so `--preauthorize-ask` refuses every evidence-quality reason.
Pre-authorizing "I accept the rebuild cost" is a judgment a person can make in
advance. "I accept that we do not know what this is" is not.

`--preauthorize-ask` takes one `kind:reason` pair at a time, e.g.
`node_modules:rebuild_cost_high`. There is no wildcard, because the only
purpose of a wildcard here would be to turn a narrow consent into a blanket
one.

## What a model may do, exactly

`autopilot run --plan-file <path>` reads an `LlmPlan` — the same schema
[BYOK LLM Planner](byok.md) documents, and produced by the same `llm-plan`
command — and uses it to **order** the candidates this machine already
discovered. That is its entire authority.

It cannot:

- add a candidate (a resource ID the local scan did not produce is dropped);
- choose an action (`ValidatedPlanItem::action_id` is deliberately ignored by
  the run; the action comes from the local registry, by kind);
- supply a path, a command, or a command fragment;
- raise any limit in the envelope;
- change any policy class;
- invent an action that is not registered.

The argument that this is airtight is structural rather than defensive:
ordering is a permutation of a list, and reordering a list cannot add a member
to it. The plan is consumed as a sort key over locally-derived candidates, so
the set of things that can be deleted is fixed before the plan is read. A
regression test hands the run a plan whose `reason` field is
`"; rm -rf /Users/dev/company-repo && echo pwned"` and asserts, among other
things, that the string never reaches the audit log — but the reason that test
passes is that `model_reason` is never read by anything but the record writer.

There is deliberately **no live-provider mode**. `autopilot run` accepts a
plan file and nothing else, so no deletion in this product waits on a network
call. Asking a model is `llm-plan`'s job, and the two are separate commands so
that the deleting one has no network path at all.

## What one run actually does

For each candidate, in the order the plan (or the local ranking) put them:

1. **Admission gate** — a pure function over the envelope, the candidate's
   policy decision, the observed pressure and the run's budget ledger. It
   returns either admission or a `RefusalReason` that names which bound
   stopped it: kind not allowed, policy class, unauthorized `ASK` reason,
   pressure floor, action budget, byte budget, time budget.
2. **Authorization** — the normal `policy::approval::authorize` path, which is
   the only way an `Approval` can exist. A pre-authorized `ASK` supplies a real
   `UserConsent` matched to the resource and fingerprint; nothing forges one.
3. **Execution** — the normal `executor::execute`, including deletion-time
   TOCTOU revalidation. An identity change, a class downgrade, widened reasons
   or degraded evidence aborts the action having deleted nothing. See
   [Safety Model](safety_model.md).
4. **Ledger** — the attempt is charged exactly once, at `max(expected,
   actual)` bytes, so an action that reclaimed more than predicted cannot
   under-charge the budget.

The action budget stops the run; a byte-budget refusal does not. A candidate
too large for the remaining byte budget is refused and the run continues to
the next one, because the next one may well fit — whereas an exhausted action
count is exhausted for everything.

A run that finds nothing it is allowed to do exits 0. "Allowed nothing" is a
successful outcome, not an error.

## The other consumer of the grant: `free --autopilot`

The envelope is not `autopilot run`'s private property. `glomeris free
--autopilot` bounds the goal-driven recovery loop by the same stored grant, using
the same admission gate above as one more narrowing stage between selection and
authorization — so a goal-driven run can never reach further than a grant allows,
and the grant remains the single place that authority is written down.

Two consequences worth stating, because they are what makes the second consumer
safe rather than merely convenient:

- **The grant cannot widen anything.** It has no power to add a candidate, choose
  an action, supply a path or raise a limit — only to withhold. Everything an
  `--autopilot` run does, the same command without the flag would also have done.
- **A stop the grant caused is reported as such**, never as an exhausted disk:
  `stop_reason: "envelope_refused"` plus the refusal token. `revoke` therefore
  stops a `free --autopilot` run exactly as it stops `autopilot run` — both
  re-read the file, and neither has a cached copy.

See [CLI Reference](cli_reference.md) for the flag's exit codes and its refusal
alongside `--dry-run`.

## Runs nobody asked for

Everything above bounds what a run may *do*. Whether a run may *begin* when
nobody pressed anything — the standing answer to a disk-pressure alert — is a
separate question, and the envelope answers it separately:
`autopilot enable --respond-to-alerts`, off by default, exercised by
`free --autopilot --unattended`.

**It is two consents, not one.** An enabled envelope does not authorize an
unprompted run; `--unattended` without that permission exits 3 having attempted
nothing. Reading "enabled" as consent to act unattended would widen every
envelope already on disk — each written by somebody bounding a run they meant to
start — into authority to delete while they were away. That is not a grant those
people gave, so it is not one this reads out of their file.

**It is the only field here that is not a ceiling on outcomes**, which is why
the table above says so rather than glossing it. It is still narrowing in the
direction that matters: off, the default, means every run has a human behind it,
and turning it on changes nothing about what a run may do once it starts. The
budgets, the allowlist and the pre-authorizations all still apply, unchanged.

**The check lives in Rust, twice over.** The grant exposes
`starts_unprompted` — in force *and* set — as one value, so that no client ever
composes its own permission out of two fields; and `free` itself refuses an
`--unattended` run the grant does not cover, so the check cannot be skipped by
whatever launched the process. `--unattended` is the caller's statement about
itself, and demanding it is what makes a run nobody started distinguishable from
one somebody typed.

**`revoke` stops it** with everything else, and keeps the setting on file — so
revoking never reads as having silently cleared what you chose.

## The audit trail

Every *executed* attempt appends one JSON line to
`~/Library/Application Support/Glomeris/actions.jsonl`, readable with
`glomeris history`. Autopilot's records carry two fields the interactive paths
do not populate:

- `source` — `autopilot_auto_safe` or `autopilot_preauthorized_ask`, so the
  authority an action ran under is recoverable from the log rather than
  inferred. Interactive records read `execute`, `free` or `emergency`.
- `model_rank` — where the plan ranked that candidate, or `null` when no plan
  was involved. This is how you tell "the model suggested it and policy allowed
  it" from "policy allowed it and no model was consulted." It is 1-based, and
  it is the same number the run's own report printed as `[AI rank N]` — a log
  offset by one from the report it came from would be worse than no log.

"Where the plan ranked it" means its position in the plan's `items` array, not
its `priority` field. `priority` is advisory and its direction was never
specified anywhere — nothing says whether `1` means most urgent or least — so
ordering on it would mean inventing a semantics and then depending on it.

Refusals are **not** written to `actions.jsonl` — it is a log of what was done
to the filesystem, and a refusal did nothing to the filesystem. They are in the
`AutopilotReport` that `run` prints, which is where you look to find out why a
run was quiet.

## Revocation

`glomeris autopilot revoke` takes effect on the next run, and there is nothing
to restart: every run re-reads the file. Revocation keeps the limits, so a
later `enable` cannot come back carrying limits you never read.

`enable` replaces the previous envelope rather than merging into it, for the
same reason. A grant should be one line you can read out loud, never the
accumulated union of every `enable` you have ever typed.

Nothing here expires on its own. The envelope is a file: it survives quitting,
restarting and logging out, and it stays in force until it is revoked. That is
a deliberate omission rather than a missing feature — a grant that lapsed on a
timer would mean the honest answer to "what may Autopilot do right now" depended
on the clock, and `show` would have to be read differently depending on when you
read it.

## Granting it without a terminal

Everything above can also be read, granted and revoked in the menu-bar app,
under Settings → Autopilot (`Cmd+,`). Reading a standing deletion grant only
from `--help` and a config file would have meant that in practice most people
who enabled it had never read it, so the GUI is part of the feature rather than
a convenience on top of it.

It changes nothing about where authority lives. The tab renders
`autopilot show --json` — the same envelope, the same ceilings, the same lists
of what can never be allowlisted, pre-authorized or executed — and writes by
running `autopilot enable` or `autopilot revoke`. It holds no policy logic, and
it offers no choice that did not arrive from the CLI as data, which is the
project-wide rule for that app and is checked in CI rather than trusted.

Two things it deliberately does not do: it cannot start a run (`autopilot run`
has no `--json` for the same reason), and it cannot construct a grant the CLI
would reject. See
[Menu Bar App](menu_bar_app.md#preferences--autopilot) for the screen itself,
including why its form is pre-filled from the envelope in force and why its
byte budget rounds up.

## Known limitation: a pre-authorized `ASK` cannot complete today

A pre-authorized `ASK` candidate is admitted, authorized with real consent,
and then *always* aborts inside deletion-time revalidation with
`PolicyClassDowngraded`. Nothing is deleted.

The cause is upstream of Autopilot. `ASK`/`RebuildCostHigh` arises only from a
**per-instance** `Regenerability::NotRegenerable`, while
`executor::build_fresh_evidence` rebuilds regenerability from the resource
*kind*'s static default — so the fresh classification lands on `AutoSafe` and
the class comparison trips.

This is left as-is deliberately. The failure direction is the safe one (refuse,
mutate nothing), and "make `build_fresh_evidence` carry a per-instance
judgment" changes the TOCTOU anchor every deleting command shares. It is
asserted by a test named for it —
`a_preauthorized_ask_still_aborts_at_deletion_time_revalidation` — rather than
left to be discovered, so whoever fixes it upstream starts from a failing test
that points at the cause.

See [Known Limitations](known_limitations.md) for the rest.
