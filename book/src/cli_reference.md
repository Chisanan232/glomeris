# CLI Reference

**The built-in help is canonical.** `glomeris <command> --help` renders from
`src/cli/help.rs`, which is the one place a command's usage, flags, safety
semantics, examples and exit codes are written down, and whose output is held
to golden snapshots in `tests/fixtures/help/` (HORO-1311). If this page and
`--help` ever disagree, `--help` is right and this page is stale.

What this page adds that `--help` deliberately does not: the JSON payload
shapes, the report field semantics, and the cross-references into the rest of
this book. It is the reference you read at a desk; `--help` is the one you read
mid-task.

## `glomeris`

No arguments: prints `glomeris <version>` (from `CARGO_PKG_VERSION`) and a
pointer to `glomeris --help`. `--version`/`-V` prints the same version line on
its own.

## Help surfaces

Everything below renders from the single `COMMANDS` table in
`src/cli/help.rs`. That table lived in `src/main.rs` until HORO-1311, where no
test could read it — so `tests/shared_command_table.rs` kept a hand-written
mirror of it, which drifted exactly as the pre-HORO-1050 duplication had
(HORO-1034: the unrecognized-command usage omitted `llm-plan`; by HORO-1311 the
test mirror omitted `llm-check`). There is now one table, read directly by both
the binary and its tests.

| Invocation | What it prints |
|---|---|
| `glomeris --help` / `-h` / `help` | Every command, grouped by what it does to your machine, with a one-line summary each. |
| `glomeris <command> --help` / `-h` | That command's usage, safety statement, its subcommands (each with its own safety label), flags, examples, exit codes and related commands. |
| `glomeris help <command>` | Identical to `glomeris <command> --help`. |
| `glomeris help exit-codes` | The exit-status reference (see [Exit codes](#exit-codes)). |
| `glomeris help <unknown-topic>` | The list of topics that exist, to stderr, exit 2. |
| `glomeris <unknown-command>` | A one-line error and a pointer to `--help`, to stderr, exit 2. |
| `glomeris <command> <bad-flag>` | *That command's* usage only, to stderr, exit 2. |

The groups in top-level help are `INSPECT`, `PLAN`, `ACT`, `OBSERVE`,
`SERVICE` and `CONFIGURE`, ordered so that the read-only commands come before
anything that can delete, and so that a first-time reader meets the product
before its preferences. A group heading describes consequence, not category: `INSPECT` says
"nothing is changed", and `ACT` says its commands delete data and that every
deletion is policy-gated.

Note that a command's group and its safety line describe only whether
*Glomeris itself* writes to the filesystem when you run it. They are not policy
classifications: `AUTO_SAFE`, `ASK` and `PROTECTED` classify *resources*, are
decided by the policy engine, and never appear in a help safety label. See
[Safety Model](safety_model.md).

### Safety is declared per verb, not only per command

Five labels exist, in ascending order of consequence:

| Label | Means |
|---|---|
| `Read-only — changes nothing.` | Nothing is written, and no other program is run. |
| `Read-only, and runs installed build tools to locate their caches.` | Glomeris writes nothing of yours. It asks an installed tool where its cache is when no documented environment variable or default location answers, and a tool run that way may maintain its own state — a version manager fronting it may provision a toolchain on first use. |
| `Advisory — proposes, never executes.` | Produces a plan; executes none of it. |
| `Writes only Glomeris's own state — never your files.` | Writes the launch agent plist, the monitor's history and heartbeat, the Autopilot envelope, or your stored preferences. Nothing you own. |
| `Can delete data — every deletion is policy-gated.` | Deletes. |

For a command that takes subcommands, the consequence is a property of the
*verb*, not of the command name: `daemon install` writes a launch agent while
`daemon status` reads, and `autopilot run` deletes while `autopilot show`
prints a config file. Until HORO-1485 one label was declared per command, so
`glomeris daemon --help` printed "Read-only — changes nothing." above
`install`, `uninstall` and `run`, and `glomeris autopilot --help` printed "Can
delete data" above `show`. No wording could have fixed either: the weakest
label is a false reassurance and the strongest a false warning, which is why
relabelling `daemon` as destructive was not an acceptable fix.

Each verb now carries its own label, printed beside it in the `Subcommands`
block. The command's own safety line is the strongest claim reachable through
it, and when its verbs disagree it says so explicitly rather than picking one
of them:

```text
Safety depends on the subcommand; each is labelled below. The strongest of
them: Writes only Glomeris's own state — never your files.
```

Two tests keep this honest, and they are deliberately not tests about strings.
`src/cli/help.rs`'s `command_safety_covers_every_reachable_subcommand` asserts
the command's label *equals* the strongest of its verbs' — equality in both
directions, because under-stating and over-stating are both false — and
`a_single_label_banner_is_true_of_every_verb_under_it` asserts the same of the
rendered banner, since a correct table printed through a banner that ignored it
would still print a false claim. Neither could have caught the original defect
on its own: with no subcommands declared, both pass vacuously.
`tests/subcommand_safety_is_honest.rs` closes that gap from outside the table —
it reads `src/main.rs`'s dispatch arms, so a verb the binary accepts cannot go
undeclared, and it runs every surface labelled read-only against a disposable
`HOME` and requires the tree to be byte-for-byte unchanged, with
`autopilot enable` as the positive control that proves the observation would
notice a write.

Two surfaces are narrower on purpose. An unrecognized top-level command gets a
pointer rather than the manual — it previously reprinted the aggregate usage of
all thirteen commands, a 13-line, 146-column wall in answer to one mistyped
word. And a usage error inside a command prints only that command's usage, so
getting a `free` flag wrong no longer tells you about `daemon`.

## Byte counts: 1024-based, with `KB`/`MB`/`GB` labels

Every byte count this product renders — `free_human`, `total_human`,
`logical_human`, `reclaimable_human`, `expected_reclaimed_human`,
`actual_reclaimed_human`, and the same numbers in text output — comes from one
function, `reporting::human_bytes`. It divides by **1024** and labels the result
`B`/`KB`/`MB`/`GB`/`TB`/`PB`. So `2147483648` renders as `2.0 GB`, where a
1000-based formatter would say `2.15 GB`. Counts below 1024 are a bare integer
and `B`, with no decimal point.

The labels are not the IEC `KiB`/`MiB`/`GiB` spelling that strictly matches the
arithmetic. That is a deliberate, documented inconsistency rather than an
oversight: these are the units `du -h` and `df -h` print for the same
arithmetic, and the alternative is renaming every unit in every report and
fixture to spell out a distinction most readers of a storage tool do not draw.

`--target` parses the same way, so what you type and what you read back agree:
`glomeris free --target 5GB` means 5 × 1024³ bytes. A bare number or a `B`
suffix is raw bytes; a trailing `%` is a percentage of total capacity instead.

**Clients should render the `*_human` string rather than scale the byte count
themselves.** The menu-bar app did the latter with `ByteCountFormatter`, which
is 1000-based, so a 2 GiB cleanup appeared as a `2.0 GB` estimate and a
`2.15 GB` result in the same panel (fixed in HORO-1312). Where a report offers
both fields, the raw `*_bytes` value is for arithmetic and the `*_human` string
is for display. A `*_human` field is `null`, never `"0 B"`, when the underlying
probe produced no number — an aborted execution reclaimed nothing, which is a
different claim from having freed zero bytes.

## `glomeris daemon <subcommand>`

macOS only (exits 1 with an error message on other platforms).

| Subcommand | Effect |
|---|---|
| `install` | Writes a per-user `launchd` plist and `launchctl load -w`s it. |
| `uninstall` | `launchctl unload -w`s the agent (best-effort) and removes the plist file. |
| `status` | Prints whether the plist is installed, its path, and whether `launchctl` reports it loaded. |
| `run` | Runs the polling loop in the foreground (this is what the installed agent actually executes). |

No subcommand, or an unrecognized one, prints usage to stderr and exits 2.
See [Daemon Lifecycle](daemon_lifecycle.md) for details.

`daemon status --json` (HORO-1045) prints a `DaemonStatusReport`.
`loaded` (launchd-reported) and `heartbeat_age_secs` (derived from the poll
loop's own last-write) are deliberately kept as two separate fields, never
collapsed into one `healthy` boolean — a loaded-but-wedged daemon and an
actually-polling one must stay distinguishable:

```sh
glomeris daemon status --json
```

```json
{
  "plist_installed": true,
  "plist_path": "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist",
  "loaded": true,
  "heartbeat_age_secs": 42
}
```

`heartbeat_age_secs` is `null` when no heartbeat file exists yet (the
daemon has never run).

## `glomeris status [--json]`

macOS only (exits 1 with an error message on other platforms). Not part of
`daemon` — this is the same one-shot disk-pressure reading `daemon run`'s
poll loop evaluates each tick, available on demand without needing the
daemon installed at all (HORO-955).

```sh
glomeris status --json
```

```json
{
  "total_bytes": 500000000000,
  "free_bytes": 125000000000,
  "used_percent": 75.0,
  "free_human": "116.4 GB",
  "total_human": "465.7 GB",
  "pressure_state": "WARN"
}
```

`pressure_state` is one of `"OK"`, `"WARN"`, `"CRITICAL"` — see
[Pressure Model](pressure_model.md).

## `glomeris scan [path] [top_k]`

Not macOS-gated. Both arguments are positional and optional:

- `path` — root directory to scan. Defaults to `.`.
- `top_k` — number of largest entries to report. Defaults to `20` if
  omitted or unparseable as `usize`.

Prints a summary line (files visited, stop reason, incomplete-entry count)
followed by one line per candidate: size, depth, path.

## `glomeris detect [--project-root <path>]... [--json] [--progress-json]`

Not macOS-gated. Runs every detector in `DetectorRegistry::builtin()` exactly
once per invocation and prints, per detector, one of five lines:
`found (<N> evidence)`, `tool_absent`, `tool_not_running`,
`not_configured (nothing to look at)`, or `failed: <reason>`. The candidate
report printed below those lines comes out of that same single pass, so the
two halves of the output cannot describe different probes of a filesystem
that changes between them (HORO-1487).

`--project-root <path>` is optional and repeatable — pass it once per
project directory you want the cargo/node/SwiftPM detectors to check for a
`target/`/`node_modules/`/`.build/` dir. Without it, those three detectors
have no project roots to scan and report `not_configured` (HORO-1576): they
never ask whether cargo, node or Swift is installed, so the older
`tool_absent` they used to print was a claim about your machine that no
observation supported. `not_configured` is the one of the five states you can
close yourself, and passing this flag is how.

`--json` prints a `DetectReport` — one candidate line per discovered
resource, including the already-computed
`executable`/`offered_actions`/`refusal_reason` triple (HORO-1053) a caller
(e.g. the menu-bar app) reads to decide what it can offer, without ever
re-deriving that from `policy_label`/`reasons` itself:

Candidates are returned **biggest reclaimable size first** (HORO-1307).
Before that they came back in detector-registration order, so a 40 GB Cargo
`target/` could be printed below a 2 MB npm cache. Ties are broken first by
measurement quality — an exact size outranks a `≥` lower bound of the same
number, because the exact one is the claim you can act on — and then by
`resource_id`, so two runs over an unchanged machine produce the same order.
Candidates whose size could not be measured at all sort last rather than
being treated as zero. Ordering is applied at the single point where the
report is assembled, so `--json` and the human-readable output can never
disagree about it.

`impact_tier` is `"unknown"`, `"normal"`, `"notable"` or `"large"`: a
pre-computed magnitude band, so a UI does not have to invent thresholds of
its own. It escalates on **either** an absolute size (≥ 1 GB is `notable`,
≥ 10 GB is `large`) **or** a share of remaining free space (≥ 5% is
`notable`, ≥ 20% is `large`) — measured against free rather than total space,
because the problem someone opens Glomeris with is "I am running out of
room". A 400 MB cache is `large` on a machine with 1.6 GB left.

`impact_tier` is a size signal and nothing else. It is **not** a safety
signal, and it must never be read as one: a `large` candidate can be
`PROTECTED`, and a `normal` one can be `AUTO_SAFE`. `executable`,
`offered_actions` and `refusal_reason` remain the only statement about what
Glomeris is permitted to do.

```sh
glomeris detect --project-root ~/dev/myproject --json
```

```json
{
  "candidates": [
    {
      "resource_id": "cargo_target_dir:/Users/dev/proj/target",
      "kind": "cargo_target_dir",
      "reclaimable_bytes": 2147483648,
      "reclaimable_human": "2.0 GB",
      "reclaimable_bytes_is_lower_bound": false,
      "impact_tier": "notable",
      "policy_label": "AUTO_SAFE",
      "reasons": ["no_active_use_observed"],
      "executable": true,
      "offered_actions": [
        {
          "action_id": "cargo.clean.target_dir",
          "requires_confirmation": false
        }
      ],
      "refusal_reason": null
    },
    {
      "resource_id": "docker_build_cache:docker",
      "kind": "docker_build_cache",
      "reclaimable_bytes": 10737418240,
      "reclaimable_human": "10.0 GB",
      "reclaimable_bytes_is_lower_bound": true,
      "impact_tier": "large",
      "policy_label": "UNKNOWN_INCOMPLETE",
      "reasons": ["evidence_incomplete"],
      "executable": false,
      "offered_actions": [],
      "refusal_reason": "no registered cleanup action for this resource kind"
    }
  ],
  "detectors": [
    {"detector": "cargo_target_dir", "status": "found", "candidates_found": 1},
    {"detector": "docker_build_cache", "status": "found", "candidates_found": 1},
    {"detector": "docker_objects", "status": "tool_not_running", "candidates_found": 0},
    {"detector": "node_modules", "status": "not_configured", "candidates_found": 0},
    {
      "detector": "homebrew_cache",
      "status": "failed",
      "candidates_found": 0,
      "reason": "brew --cache exited with status exit status: 1"
    }
  ],
  "discovery_complete": false
}
```

`detectors` reports one entry per detector that ran, in registry order, and
`status` is one of five tokens — `found`, `tool_absent`, `tool_not_running`,
`not_configured`, `failed` — the same five `--progress-json` uses below.
`reason` is present only on `failed`, and is the detector's own account of what
went wrong. The other four carry no reason, so a client must branch on `status`
rather than on the presence of `reason`.

`discovery_complete` (HORO-1484) is `false` when at least one detector either
failed or was never pointed at anything, and it is derived from `detectors`
rather than tracked separately, so the summary cannot disagree with the array it
summarises. When it is `false`, `candidates` is **not** a complete account of
what could be reclaimed, and no consumer may present it as one — "nothing worth
reclaiming" is a claim about the machine, and a search that did not finish has
not established it.

Which of the five make discovery incomplete, and why:

| Status | Incomplete? | What it means |
|---|---|---|
| `found` | no | The detector looked and reported what it saw. |
| `tool_absent` | no | The tool is not installed, so it has nothing to report. |
| `tool_not_running` | no | The tool is installed but its daemon is not up (HORO-1544) — still an answer about a reachable state, not a gap. |
| `not_configured` | **yes** | The detector was handed no project root, so nobody looked (HORO-1576). |
| `failed` | **yes** | The detector was asked and could not answer. |

`not_configured` and `failed` are the two where no observation was made, which
is why both clear the flag. They are still distinct, and a consumer must keep
them apart: on a default installation the three project-scoped detectors are
`not_configured` and nothing has failed, so presenting the two alike reports a
malfunction on a working machine. `not_configured` is also the only one of the
five the user can resolve, by naming a project root.

`--progress-json` (HORO-1052) emits one NDJSON-encoded `ProgressEvent` line
to **stderr** per detector start/finish while discovery runs — a way for a
spawning UI to distinguish "still working" from "hung" on a slow/contended
host (v0.2.0 founder-dogfood measured a single discovery pass up to 3m40s).
Stdout is completely unaffected either way, `--progress-json` output can be
combined with `--json`, and nothing is emitted at all unless the flag is
passed:

```sh
glomeris detect --progress-json 2>&1 1>/dev/null
```

```
{"phase":"detector_started","detector":"cargo_target_dir"}
{"phase":"detector_finished","detector":"cargo_target_dir","candidates_found":1,"outcome":"found"}
{"phase":"detector_started","detector":"docker_images"}
{"phase":"detector_finished","detector":"docker_images","candidates_found":0,"outcome":"tool_absent"}
{"phase":"detector_started","detector":"node_modules"}
{"phase":"detector_finished","detector":"node_modules","candidates_found":0,"outcome":"not_configured"}
{"phase":"detector_started","detector":"homebrew_cache"}
{"phase":"detector_finished","detector":"homebrew_cache","candidates_found":0,"outcome":"failed","reason":"brew --cache exited with status exit status: 1"}
```

`outcome` (HORO-1484) carries the same five tokens as `status` above, and
`reason` is present only on `failed`. The last three lines are why it exists:
every one of them reports `candidates_found: 0`, exactly like a detector that
looked and found nothing, so a consumer reading only the count shows "0 found"
for three checks with three different stories. `docker_images` being absent is
normal and expected; `node_modules` having no configured root is normal too but
is a gap the user can close; `homebrew_cache` failing is neither. None of the
three may be presented alike.

`detect`, `explain`, `llm-plan`, and `execute` all share this same
discovery phase and all support `--progress-json` identically.

## `glomeris explain <resource_id_or_path> [--project-root <path>]... [--json] [--progress-json]`

Not macOS-gated. Runs the same discovery-and-classification pipeline as
`detect`, then prints the full evidence-and-policy picture for exactly the
one resource matching `<resource_id_or_path>` (a `detect` report's
`resource_id`, or a filesystem path) — evidence provenance, size (logical
vs. reclaimable, explicitly labeled as different), active-use signals, and
policy classification (HORO-955). Exits `1` if no discovered candidate
matches the query.

`--project-root <path>` and `--progress-json` mean exactly what they mean
for `detect` above.

```sh
glomeris explain cargo_target_dir:/Users/dev/proj/target --json
```

```json
{
  "resource_id": "cargo_target_dir:/Users/dev/proj/target",
  "kind": "cargo_target_dir",
  "detector": "cargo_target_dir",
  "sources": ["cargo metadata: target-dir"],
  "logical_bytes": 2147483648,
  "logical_human": "2.0 GB",
  "reclaimable_bytes": 2147483648,
  "reclaimable_human": "2.0 GB",
  "reclaimable_bytes_is_lower_bound": false,
  "completeness": "complete",
  "confidence": "high",
  "active_use_signals": [],
  "regenerability": "regenerable_by_rebuild",
  "policy_label": "AUTO_SAFE",
  "reasons": ["no_active_use_observed"],
  "native_cleanup_available": true,
  "native_cleanup_action_id": "cargo.clean.target_dir",
  "fingerprint_token": "<opaque token — copy verbatim, never hand-construct>",
  "executable": true,
  "offered_actions": [
    {
      "action_id": "cargo.clean.target_dir",
      "requires_confirmation": false
    }
  ],
  "refusal_reason": null
}
```

`fingerprint_token` (HORO-1051) is an opaque, wire-safe encoding of the
resource's identity fingerprint — `null` for a resource with no dev/inode/
mtime identity (e.g. Docker's build cache). This is the exact token
`execute --observed-fingerprint` later expects back for an `ASK`-classified
resource; see `execute`'s section below.

## `glomeris clean --dry-run [--target <resource_id_or_path>] [--project-root <path>]...`

Not macOS-gated. Renders what would be cleaned, without executing
anything — `--dry-run` is required; there is no non-dry-run execution path
on this subcommand (real destructive execution is `glomeris free --target`
or `glomeris execute`'s job). Without `--target`, every discovered
candidate is considered; with it, only the one matching resource is.

```sh
glomeris clean --dry-run
```

Human-readable output only — `clean --dry-run` has no `--json` mode. One
line per considered resource, either its rendered `ActionPlan.explain` text
or a `skip_reason` (e.g. `PROTECTED`, no registered action).

## `glomeris llm-plan [--contract-version <1|2>] [--evidence-rounds <n>] [--project-root <path>]... [--plan-file <path>] [--json] [--progress-json] [--schema] [--print-payload]`

Not macOS-gated. ADVISORY, NON-EXECUTING (HORO-1008) — never constructs a
`policy::Approval` and never calls `policy::approval::authorize` or
`executor::execute`. See [BYOK LLM Planner](byok.md) for the full
configuration and safety-property writeup.

- `--contract-version <1|2>` (HORO-1548) selects the planner contract.
  **1 is the default and is what every bullet below describes.** 2 sends a
  privacy-safe projection of the whole workspace — repositories, working
  trees, branch and activity state, caches, and a local usage baseline — and
  asks for a *disposition* and a *confidence* per resource plus what the
  model could not establish, instead of a ranking. It prints a
  `WorkspacePlanReport`, whose human-readable form opens with `WORKSPACE PLAN
  (contract v2)`. Version 2 is not more powerful: it still executes nothing,
  and still cannot name a path, an action it was not offered, or a command.
  A version this build does not implement exits `2` rather than being rounded
  to one it does. See
  [BYOK LLM Planner](byok.md#planner-contract-version-2-horo-1548).
- `--evidence-rounds <n>` (HORO-1549) runs the read-only probes the model
  asked for in `evidence_requests` and asks it again with the answers, up to
  `n` rounds — `1` to `8`, contract version 2 only. The model supplies a probe
  name and an alias it was already given, never a path, a command or a URL;
  a probe that cannot answer reports that, which is never read as an answer of
  no. Twelve probes, 5s per probe and 30s in total bound the run regardless of
  `n`, and the report names which ceiling stopped it — only
  `nothing_more_asked` is convergence. Refused with `--contract-version 1`
  (nothing to answer into) and with `--print-payload` (whose preview is the
  first round only). See
  [Asking for more evidence](byok.md#asking-for-more-evidence---evidence-rounds-horo-1549).
- Without `--plan-file`, credentials are read only from
  `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL` (all
  three required, no default base URL) — never from a CLI flag.
  `GLOMERIS_LLM_BASE_URL` is the **API root**: `/chat/completions` is
  appended to it verbatim, so it usually ends in `/v1`
  (`https://gateway.example.com/v1`). See
  [BYOK LLM Planner](byok.md#glomeris_llm_base_url-is-the-api-root-not-the-host-root).
- A failed provider call is reported on the `provider error:` line (or the
  `provider_error` JSON field) as one secret-free sentence naming the HTTP
  status, API style, request path, request id, and a bounded excerpt of the
  provider's error body — never the `Authorization` header, the key, the
  host, or the request payload.
- `--plan-file <path>` reads the file's raw bytes as if they were the
  model's raw response text, through the same validation pipeline the live
  provider uses — no network call, no API key required.
- `--project-root <path>` is optional and repeatable, same meaning as
  `detect`'s flag above.
- `--api-key`/`--key`/`--token` are explicitly rejected (not accepted and
  ignored) — the error names `$GLOMERIS_LLM_API_KEY` instead.
- `--json` prints the report as JSON. Each item carries the model's own
  `priority`/`model_reason` alongside the machine's `policy_label`,
  `requested_action_id`, `explain`, `skip_reason`, `completeness`,
  `confidence`, and a nested `candidate` — the byte-for-byte same projection
  `detect --json` prints for that resource, including
  `executable`/`offered_actions`/`refusal_reason` (HORO-1308). A consumer
  deciding what may be done reads `candidate`; `priority`/`model_reason` are
  the only two fields a provider chose, and the **order of `items` is the
  provider's too** — `plan_with_llm` never sorts, it only drops. `--json`
  output is printed *before* a `provider_error` exit, so an exit `1` report is
  still complete and readable.
- `--progress-json` streams the same NDJSON discovery progress as `detect`,
  on stderr, leaving stdout a single clean JSON document. This is the exact
  pair (`--json --progress-json`) the menu-bar app's AI Plan card spawns; see
  [Menu Bar App](menu_bar_app.md#ai-plan).
- `--print-payload` (HORO-1298) runs discovery, prints the exact request a
  live run would send — the system prompt, the user prompt, and the local
  wire-id-to-real-resource table under a heading marking it as *not* sent —
  then returns before any provider is constructed. Requires no credential
  and makes no network call, so it cannot send what it displays. The
  outbound prompts identify resources only by positional alias
  (`resource_1`, …); absolute paths appear in the alias table and nowhere
  else. See [BYOK LLM Planner](byok.md#what-leaves-your-machine).
- `--schema` (HORO-1048) prints an example, syntactically valid response
  document for the selected contract version to stdout and exits — an
  `LlmPlan` for version 1, a version 2 response document for version 2. It is
  a distinct, self-contained mode that never runs discovery, never reads
  `--plan-file`, and never checks live-mode credentials, regardless of what
  else is passed alongside it.
  See [BYOK LLM Planner](byok.md#llmplan-schema--the-concrete---plan-file-example-horo-1048)
  for the version 1 example and field table, and
  [Planner contract version 2](byok.md#planner-contract-version-2-horo-1548)
  for version 2's.

```sh
glomeris llm-plan --schema
```

```json
{
  "items": [
    {
      "resource_id": "cargo_target_dir:/path/to/project/target",
      "action_id": "cargo.clean.target_dir",
      "priority": 1,
      "reason": "stale build artifacts, not modified in 30 days"
    }
  ]
}
```

This exact output round-trips unchanged through `glomeris llm-plan
--plan-file <path>` — see the linked BYOK page for the full field table and
the round-trip test that proves it. The `resource_id` form shown here is
the one a human writes by hand in a fixture; a live model is given
positional wire aliases and answers with those, and both forms resolve.

Human-readable output always opens with `LLM SUGGESTION — advisory only,
nothing is executed by this command` under version 1, and `WORKSPACE PLAN
(contract v2) — advisory only, nothing is executed by this command` under
version 2. Both forms say it on the first line, because a reader who stops
there should still know nothing happened.

Exit codes for this subcommand specifically:

- `0` — success, including zero suggestions or every suggestion being
  `PROTECTED`.
- `1` — the provider call or response parsing failed, `--plan-file`
  named an unreadable path, or `--print-payload` could not serialize the
  request.
- `2` — usage error: an unrecognized argument, `--plan-file` with no value,
  `--contract-version` with no value or with a version this build does not
  implement, `--evidence-rounds` with no value, with a count outside `1`–`8`,
  or combined with `--contract-version 1` or `--print-payload`, missing
  live-mode environment configuration, or an `--api-key`/`--key`/`--token`
  flag.

## `glomeris llm-check [--json]`

Not macOS-gated. Tests the configured BYOK setup and reports whether the
endpoint, credential and model work (HORO-1309). Runs no detectors, reads no
project roots, collects no evidence, and consults no policy — it is not a
planning command, and there is nothing it could execute.

- Sends the two fixed prompts in `actions::llm::CONNECTION_TEST_SYSTEM_PROMPT`
  / `CONNECTION_TEST_USER_PROMPT` ("You are a connection test. Reply with the
  single word: ok." / "ok") through the same `LlmProvider::complete` a real
  plan uses. Same code path, so a pass means a plan will route and authenticate
  — not that a cheaper probe succeeded.
- The prompts are constants with no interpolation, so a connection test
  describes nothing about this machine: no path, no home directory, no account
  name (`connection_test_prompts_describe_nothing_local`).
- Configuration comes only from
  `GLOMERIS_LLM_API_KEY`/`GLOMERIS_LLM_BASE_URL`/`GLOMERIS_LLM_MODEL`, all
  three required, exactly as for `llm-plan`.
  `--api-key`/`--key`/`--token` are rejected by flag name, and the error names
  the flag only — never the value beside it.
- `--json` prints an `LlmCheckReport`: `outcome`, `model`, `endpoint_path`,
  `error`, `response_excerpt`. Human-readable output opens with
  `LLM CONNECTION OK` or `LLM CONNECTION FAILED (<outcome>)`.
- `outcome` is one of five tokens, produced by the single
  `actions::llm::llm_check_outcome` mapping so the CLI, the book and the
  menu-bar app cannot disagree about what a failure was:

| `outcome` | What happened | Where the fix is |
|---|---|---|
| `ok` | The provider answered and its reply is excerpted in `response_excerpt` | — |
| `unreachable` | No HTTP response at all — DNS, TLS, refused connection, timeout | Network, host name, or VPN |
| `rejected` | The provider answered with a non-2xx status | Credential, or the base-URL path — read the path in `error` |
| `unusable_response` | A 2xx response that was empty, not JSON, or missing `choices[0].message.content` | Model name, or a gateway not actually speaking the OpenAI shape |
| `misconfigured` | A base URL that cannot work — `validate_base_url` refused it before anything was sent | The base URL itself; see [BYOK LLM Planner](byok.md#glomeris_llm_base_url-is-the-api-root-not-the-host-root) |

  The fifth is the odd one out: `LlmError::InvalidConfiguration` can only come
  from constructing the provider, which happens *before* any report exists, so
  `glomeris llm-check` never prints a report whose `outcome` is
  `misconfigured` — it exits `2` with one line on stderr instead. The token is
  in the vocabulary because that is the name for what happened, and a caller
  branching on exit `2` (the menu-bar app does) classifies it that way itself
  rather than inventing a sixth word for the same condition.

- `error` carries the same secret-free sentence `llm-plan`'s `provider_error`
  does — status, API style, request path, `x-request-id` when the provider
  sends one, and a bounded excerpt of the provider's own error body, with the
  configured key scrubbed out of it. Never the `Authorization` header, the
  key, the scheme, the host, or the query string.

```sh
glomeris llm-check --json
```

Exit codes for this subcommand specifically:

- `0` — the provider answered and `outcome` is `ok`.
- `1` — a check ran and did not pass (`unreachable`, `rejected`,
  `unusable_response`). **The report is printed first**, so an exit `1` still
  carries a complete, readable diagnosis on stdout.
- `2` — nothing was sent and there is no report at all: an unrecognized
  argument, an `--api-key`/`--key`/`--token` flag, a base URL
  `validate_base_url` refused, or missing configuration (the message names
  the three variable *names*, never a value). Stdout is empty; the reason is
  one line on stderr prefixed `glomeris llm-check: `.

The split matters for a caller branching on the code without parsing output,
which is exactly what the menu-bar app's connection test does: `2` means "fix
your invocation or setup", never "the network is having a bad day". A `2` with
no report is the one case where a UI has to quote the CLI's own sentence rather
than render a report.

## `glomeris execute --action-id <id> --resource-id <id> [--project-root <path>]... [--confirm-ask --observed-fingerprint <token>] [--json] [--progress-json]`

macOS only (exits 1 with an error message on other platforms). The sole
interactive destructive-execution subcommand (HORO-1055) — the only place
in this CLI where a caller can trigger one specific, real destructive
action against one specific, real resource. `--action-id` and
`--resource-id` are both required; the caller supplies ONLY these
selectors (plus, for `ASK`, an observed fingerprint token) — there is no
flag to pass a `PolicyClass`, a raw filesystem path as a direct target, a
shell string, or any `--force`/override.

Internal flow, in one process:

1. Acquires the same HORO-1054 execution lock `free`/`emergency` use —
   held for the whole call, released on exit.
2. Runs the same discovery-and-classification pipeline `detect`/`explain`
   use (`--project-root <path>` is optional and repeatable, same meaning
   as elsewhere).
3. Resolves `--resource-id` against the discovered candidates and
   `--action-id` against that resource's own registered action —
   refusing if either does not resolve, or if the resolved action's id
   differs from `--action-id`.
4. For an `ASK`-classified resource, builds consent ONLY from decoding
   `--observed-fingerprint` (via the same token format `explain --json`'s
   `fingerprint_token` field emits) — never from a fingerprint freshly
   observed by this same process, which would defeat the whole
   fingerprint-pinning purpose. `--confirm-ask` and
   `--observed-fingerprint` must be passed together or not at all.
5. Calls the real, unmodified `policy::approval::authorize`, then — only
   if it returns an approval — the real, unmodified `executor::execute`.
   `PROTECTED` refuses unconditionally regardless of any flag
   combination; `execute`'s own deletion-time revalidation can still
   abort a plan that was authorized a moment earlier if the resource
   changed in between.

`--json` prints an `ExecuteReport` (action id, resource id, outcome,
failure/abort detail, expected vs. actual reclaimed bytes — the latter is
a real measurement, taken after execution, not an estimate) on the
`Executed` path. Every refusal/not-found/busy path instead prints an
`ExecuteRefusalReport` (`{"reason": "...", "message": "..."}`) to stdout
before exiting with the matching code below, so a `--json` caller never
gets silent stdout on a non-`Executed` outcome. `reason` is one of
`resource_not_found`, `action_not_found`, `action_mismatch`, `protected`,
`ask_no_consent`, `ask_consent_mismatch`, `auto_safe_contract_violation`,
or `busy` (HORO-1056: the lock-contention case below, the one refusal
that happens before discovery/resolution even runs) — each a distinct,
machine-readable value naming exactly which refusal/abort path fired,
never a generic error string.

An `AUTO_SAFE` resource needs no confirmation flags at all:

```sh
glomeris execute --action-id cargo.clean.target_dir \
  --resource-id cargo_target_dir:/Users/dev/proj/target --json
```

```json
{
  "action_id": "cargo.clean.target_dir",
  "resource_id": "cargo_target_dir:/Users/dev/proj/target",
  "outcome": "succeeded",
  "failure_message": null,
  "abort_reason": null,
  "expected_reclaimed_bytes": 2147483648,
  "actual_reclaimed_bytes": 2147483648,
  "expected_reclaimed_human": "2.0 GB",
  "actual_reclaimed_human": "2.0 GB"
}
```

The two `*_human` strings are new in HORO-1312 and are what a UI should
display — see [Byte counts](#byte-counts-1024-based-with-kbmbgb-labels).

An `ASK`-classified resource requires `--confirm-ask` plus the exact
`--observed-fingerprint` token captured from a prior `explain --json` call
on that same resource (never a fingerprint freshly observed by `execute`
itself) — `$FINGERPRINT_TOKEN` below is that call's `fingerprint_token`
field, copied verbatim, never hand-constructed:

```sh
glomeris execute --action-id cargo.clean.target_dir \
  --resource-id cargo_target_dir:/Users/dev/proj/target \
  --confirm-ask --observed-fingerprint "$FINGERPRINT_TOKEN" \
  --json --progress-json
```

If the resource's identity changed between the `explain` call and this
`execute` call, revalidation aborts the plan rather than proceeding:

```json
{
  "action_id": "cargo.clean.target_dir",
  "resource_id": "cargo_target_dir:/Users/dev/proj/target",
  "outcome": "aborted_by_revalidation",
  "failure_message": null,
  "abort_reason": "ResourceIdentityChanged",
  "expected_reclaimed_bytes": 2147483648,
  "actual_reclaimed_bytes": null,
  "expected_reclaimed_human": "2.0 GB",
  "actual_reclaimed_human": null
}
```

Note both `actual_*` fields are `null` rather than `0`/`"0 B"`. Nothing was
deleted, which is not the same report as a cleanup that freed no bytes.

Every refusal path (e.g. `PROTECTED`, no consent supplied, a stale
fingerprint) prints an `ExecuteRefusalReport` instead, with `--json`:

```json
{
  "reason": "protected",
  "message": "refused — this resource is PROTECTED; no flag combination can authorize executing against it"
}
```

Exit codes for this subcommand specifically:

- `0` — the action executed and succeeded.
- `1` — the action executed but failed (a step of the plan errored).
- `2` — usage error: an unrecognized/missing argument, `--confirm-ask`
  without `--observed-fingerprint` (or vice versa), or a malformed
  `--observed-fingerprint` token.
- `3` — refused by policy: `PROTECTED` (unconditional), `ASK` with no
  consent supplied, or `ASK` with a supplied consent that did not match
  the freshly observed fingerprint.
- `4` — aborted by `execute`'s own deletion-time revalidation (a TOCTOU-
  style guard: the resource's identity or policy classification changed
  between authorization and execution).
- `5` — `--resource-id` matched no discovered candidate, the resource had
  no registered action, or the resolved action's id did not match the
  supplied `--action-id`.
- `75` — the execution lock is already held by another `glomeris`
  invocation (see `free`'s exit codes above; `execute` reuses the exact
  same `EXIT_EXECUTION_LOCK_BUSY` constant — this ticket's own AC
  described this case as exit `6`, but the already-established lock
  convention from HORO-1054 is kept rather than introducing a second,
  conflicting "busy" code). With `--json`, this prints an
  `ExecuteRefusalReport` with `reason: "busy"` to stdout (HORO-1056) —
  previously this path was silent on stdout even under `--json`.

## `glomeris emergency`

macOS only (exits 1 with an error message on other platforms). Takes no
arguments, and rejects any with exit 2 — until HORO-1311 it silently discarded
them, so `glomeris emergency --dry-run` performed a real recovery run. See
[Emergency Mode](emergency_mode.md).

## `glomeris history [--json] [--limit <N>]`

Reads back a bounded, oldest-first tail of the monitor's `history.tsv`
(HORO-1046) — the same append-only file `daemon run`'s poll loop already
writes via `PersistenceBackend::record`. No new persistence format; this is
a read path only.

`--limit <N>` is optional and defaults to 20. It bounds how many of the
most recent pressure transitions are returned — a malformed line in
`history.tsv` is skipped rather than failing the whole read, and a missing
history file (the daemon has never run, or never recorded a transition)
renders as an empty list rather than an error.

With `--json`, prints a `HistoryReport` (`{"events": [...]}`); each event
has `unix_time_secs`, `from`, `to`, `used_percent`, `free_bytes`, and
`free_human`. Without `--json`, prints one line per event as plain text.

```sh
glomeris history --json --limit 2
```

```json
{
  "events": [
    {
      "unix_time_secs": 1700000000,
      "from": "OK",
      "to": "WARN",
      "used_percent": 82.5,
      "free_bytes": 80000000000,
      "free_human": "74.5 GB"
    },
    {
      "unix_time_secs": 1700000600,
      "from": "WARN",
      "to": "CRITICAL",
      "used_percent": 95.1,
      "free_bytes": 20000000000,
      "free_human": "18.6 GB"
    }
  ]
}
```

## `glomeris actions <list [--json]|history [--json] [--limit <N>]>`

Neither subcommand is macOS-gated — `list` touches no filesystem/launchd
state at all, and `history` only reads a plain file.

### `glomeris actions list [--json]`

Enumerates every action currently registered in `ActionRegistry::builtin()`
(HORO-1047) — the read path that replaced having to read
`src/actions/homebrew.rs` source directly to find a real action id string.
`applies_to` is a direct projection of each action's own `Action::applies_to`,
never a hand-maintained list.

```sh
glomeris actions list --json
```

```json
{
  "actions": [
    { "action_id": "cargo.clean.target_dir", "applies_to": ["cargo_target_dir"] },
    { "action_id": "node.clean.node_modules", "applies_to": ["node_modules"] },
    { "action_id": "homebrew.cleanup.cache", "applies_to": ["homebrew_cache"] }
  ]
}
```

### `glomeris actions history [--json] [--limit <N>]`

Reads back a bounded, oldest-first tail of `actions.jsonl` (HORO-1057) — the
real-execution audit trail that `execute`, `free`, `emergency` and
`autopilot run` each append to, best-effort, after their own outcome is
already decided. Unlike `history.tsv` (which records pressure transitions
only), this is the audit trail of what was actually executed: action id,
resource id, the policy label it was authorized under, outcome, abort reason
(when applicable), actual reclaimed bytes, and which real-execution path
produced it.

`--limit <N>` is optional and defaults to 20, same bounding/malformed-line-
skip/missing-file-empty contract as `glomeris history`. An audit-write
failure never affects the execution it was trying to record — the write is
best-effort and its result is never surfaced to the caller.

With `--json`, prints an `ActionHistoryReport` (`{"events": [...]}`); each
event has `timestamp`, `action_id`, `resource_id`, `policy_label`,
`outcome`, `abort_reason`, `actual_reclaimed_bytes`, `actual_reclaimed_human`,
and `source`. Without `--json`, prints one line per event as plain text.

`source` is one of five values, produced by `ActionSource::as_str` in
`src/monitor/persistence.rs`:

| `source` | The path that executed it |
|---|---|
| `execute` | `glomeris execute`, one action against one named resource |
| `free` | `glomeris free --target`, the recovery loop |
| `emergency` | `glomeris emergency`, machine-wide, `AUTO_SAFE` only |
| `autopilot_auto_safe` | `glomeris autopilot run`, action policy allowed on its own |
| `autopilot_preauthorized_ask` | `glomeris autopilot run`, action attempted only because `autopilot enable --preauthorize-ask` had already named that kind and reason |

The last two are deliberately distinct rather than one `autopilot` value: the
question an audit trail has to answer is not just *what ran* but *who
permitted it*, and a pre-authorized `ASK` was permitted by the operator
naming that resource kind, not by policy alone.

Note "attempted" in that last row. A record is written for a failed or aborted
attempt too — not for a *refused* one, which never reached the filesystem and
so appears in `autopilot run`'s own report instead. Today every
pre-authorized `ASK` aborts at
deletion-time revalidation — so `autopilot_preauthorized_ask` currently only
ever appears alongside `outcome: "aborted_by_revalidation"`. That is a known
limitation with a named cause and a pinning test, not the intended end state:
see [Known Limitations](known_limitations.md).

```sh
glomeris actions history --json --limit 2
```

```json
{
  "events": [
    {
      "timestamp": 1700000000,
      "action_id": "cargo.clean.target_dir",
      "resource_id": "cargo_target_dir:/Users/dev/proj/target",
      "policy_label": "AUTO_SAFE",
      "outcome": "succeeded",
      "abort_reason": null,
      "actual_reclaimed_bytes": 2147483648,
      "actual_reclaimed_human": "2.0 GB",
      "source": "execute"
    },
    {
      "timestamp": 1700000600,
      "action_id": "node.clean.node_modules",
      "resource_id": "node_modules:/Users/dev/proj/node_modules",
      "policy_label": "ASK",
      "outcome": "aborted_by_revalidation",
      "abort_reason": "ResourceIdentityChanged",
      "actual_reclaimed_bytes": null,
      "actual_reclaimed_human": null,
      "source": "free"
    }
  ]
}
```

## `glomeris free (--goal-used-percent <N> | --target <N%|NB>) [--dry-run] [--json] [--progress-json] [--stop-file <path>] [--autopilot [--unattended]] [--project-root <path>]...`

macOS only (exits 1 with an error message on other platforms). Exactly one of
the two goal flags is required; any other argument is rejected (usage printed
to stderr, exit 2).

### The two axes

The goal can be stated on either axis, and the two flags are **not
interchangeable wordings of one thing**:

| Flag | Axis | Meaning |
|---|---|---|
| `--goal-used-percent <N>` | disk **used** | Stop when the volume is at most `N` percent used. |
| `--target <N%\|NB>` | free space | Stop when at least this much of the volume is free. |

`--goal-used-percent 60` and `--target 40%` request the same end state.
`--goal-used-percent` is the product-facing form — it is the number a person
reads off a status bar, and it is what the Glomeris app sends. `--target` is
the raw free-space floor the recovery loop itself works in, and its meaning
and value formats are unchanged.

Passing both exits 2 rather than picking one: they are different numbers about
how much of a disk to delete, so there is no safe precedence between them.

Accepted `--target` value formats:

- A percentage: a number followed by `%`, in the range `0`–`100`
  (e.g. `--target 15%`). Target is "at least this percent of total capacity
  free."
- An absolute byte amount: a number optionally followed by `B`, `KB`, `MB`,
  `GB`, or `TB` (case-insensitive; no suffix means raw bytes). Multipliers
  are binary/1024-based — `--target 5GB` means `5 * 1024^3` bytes free, not
  `5 * 10^9`.

`--goal-used-percent` takes a plain number from `0` to `100`; a trailing `%`
is tolerated. It is additionally checked against the current reading and
**refused with exit 2 if it is not an improvement** on current usage, because
recovering toward a goal you already satisfy would delete nothing and still
print a report that reads like a successful cleanup. That check runs before
the execution lock is taken and before anything is deleted. `--target`
deliberately keeps its older, looser behaviour: a free-space floor you already
exceed is a legitimate no-op probe.

### Other flags

- `--dry-run` prints the pre-flight for the goal — current usage, free bytes
  still needed, and the estimated reclaimable opportunity split by what policy
  would actually permit — then exits. It takes no execution lock and mutates
  nothing. A `--target` pre-flight is rendered on the used axis too, so no
  surface has to show a percentage whose axis is unstated.
- `--json` prints a machine-readable report instead of prose. With
  `--dry-run` that is a `RecoveryPreviewReport`; without it, a
  `RecoveryRunReport`. A goal refused before the run starts prints a
  `RecoveryGoalRejectionReport` (`reason`, `message`, `goal_used_percent`,
  `current_used_percent`) and still exits 2. A busy execution lock prints the
  same `{"reason": "busy", ...}` refusal `execute --json` does, and exits 75.
- `--progress-json` streams NDJSON progress on stderr — one complete object per
  line, never on stdout, so a `--json` report stays parseable as exactly one
  document. With `--dry-run` it is the discovery-scan stream `detect` emits.
  Without it, it reports the recovery loop itself; see
  [Watching a run](#watching-a-run) below.
- `--stop-file <path>` asks the loop to stop **after the action it is currently
  running**, once `path` exists. See [Stopping a run](#stopping-a-run).
- `--autopilot` bounds the run by the stored Autopilot envelope instead of by
  this command's own limits. See [Running inside the
  grant](#running-inside-the-grant).
- `--unattended` declares that nobody asked for this run. Only valid with
  `--autopilot`, and it needs a second permission the grant states separately.
  See [Runs nobody asked for](#runs-nobody-asked-for).
- `--project-root <path>` is optional and repeatable, same meaning as
  `detect`'s flag above — it feeds the same `DiscoveryContext` the recovery
  loop discovers candidates from.

### Watching a run

With `--progress-json`, a real run emits one JSON object per line on stderr as
it proceeds. Every line carries a `phase` and the 1-based `iteration` it belongs
to:

| `phase` | When | Also carries |
|---|---|---|
| `measured` | The volume was read (loop step 1). The only statement of fact about free space in the stream. | `total_bytes`, `free_bytes`, `used_percent`, `free_human`, `bytes_freed_so_far` |
| `discovering` | Detectors are being asked what exists now. Re-entered every iteration — no pass reuses an earlier one's list. | — |
| `discovered` | That pass finished. | `candidates` (how many have a resolvable action), `detectors_failed` |
| `revalidating` | Evidence is being re-collected and reclassified before anything is chosen. | — |
| `action_started` | A real mutation is about to run. | `resource`, `action`, `policy_label`, `estimated_bytes` |
| `action_finished` | It finished. | `resource`, `action`, `outcome`, `reclaimed_bytes`, `bytes_freed_so_far` |
| `stop_requested` | A stop was observed, between actions. | — |

Two rules hold across every line:

- **`estimated_bytes` is the only estimate.** `reclaimed_bytes` and
  `bytes_freed_so_far` are measured from the filesystem. Never accumulate the
  estimate as progress — it is what the candidate claimed, not what happened.
- **A count that could not be measured is absent, not zero.** An
  `action_finished` line with no `reclaimed_bytes` means the size was not
  determinable; `"0 B"` there would be a measurement nobody took.

These phase names never collide with `detect`'s discovery stream, so a client
reading both cannot mistake a scan for a run.

### Stopping a run

`--stop-file <path>` is a sentinel: create the file and the loop stops after the
action it is currently running, finishing with
`stop_reason: "stopped_by_user"` and a complete report.

- **The path must not exist yet** (exit 2 otherwise). A sentinel left behind by
  an earlier run would stop the next one before it did anything, and the report
  would truthfully say the user stopped it while the user had done nothing.
- **Cooperative, deliberately.** Nothing here can interrupt a deletion
  mid-flight. A signal delivered partway through one would leave the filesystem
  in a state neither the loop nor the audit log could describe, so the loop asks
  between actions instead.
- Not valid with `--dry-run`, which performs no actions to stop.

### Running inside the grant

`--autopilot` runs this same loop under the standing grant
`glomeris autopilot enable` wrote. It starts no second loop and introduces no
second policy: every candidate is still classified, revalidated and executed
exactly as it would be otherwise, and the envelope can only **withhold** one.

- **Purely subtractive.** Anything an `--autopilot` run does, a run you started
  yourself would also have done. Nothing the envelope says can make a `PROTECTED`
  resource executable, admit an `ASK` whose exact kind and reason were not
  pre-authorized, or reach past a scoped-path or revalidation check.
- **No grant, no run.** Defaults grant nothing, so with Autopilot revoked or
  never enabled the command exits **3** having attempted nothing — the same code
  `autopilot run` uses, so a script can tell "not authorized" apart from a usage
  error (2) and from a failed run (1). A revoked envelope refuses exactly as an
  absent one does; `revoke` keeps the limits on file, and only `enabled` stands
  between them and a run.
- **Its limits replace this command's, and are the stricter of the two.** The
  run-level action count and wall-clock budget come down to the envelope's own
  figures. Left at their defaults, the loop's 600-second ceiling could cut short
  a run the grant had authorized for the full 15 minutes and report
  `budget_exceeded` — a true sentence about the wrong budget.
- **The envelope is printed first**, on stderr, before anything is discovered.
  Output that may end in deletions opens with the authority it acted under
  rather than asking you to go and look it up afterwards. stdout still carries
  exactly one report.
- **A stop the envelope caused says so.** `stop_reason: "envelope_refused"` with
  an `envelope_refusal` token naming which limit it was — an exhausted action,
  byte or time budget, a kind outside the grant, a pressure floor not met, a
  grant revoked mid-run. That is deliberately *not* `safe_exhausted`: "this disk
  has nothing safe left" and "Autopilot reached the limit you set" call for
  opposite next steps, and only one of them is a finding about the disk.
- **Not valid with `--dry-run`** (exit 2). An envelope is authority to execute; a
  preview executes nothing, so the flag would have nothing to narrow and the
  candidate list would look envelope-filtered while being the whole of it. Read
  the grant with `glomeris autopilot show`.

### Runs nobody asked for

An envelope answers "what may a run do". `--unattended` is about a different
question — "may a run *start* when nobody pressed anything" — and that is a
second permission, granted by
`glomeris autopilot enable --respond-to-alerts` and off by default.

- **Two consents, not one.** An enabled envelope alone does not authorize an
  unprompted run, and `--unattended` without that permission exits **3** having
  attempted nothing. The reason is what the alternative would mean for a grant
  already on disk: somebody wrote it to bound a run they intended to start, and
  reading it as consent to act while they are away would widen a grant they
  already read and approved.
- **The check is here, in this binary.** `--unattended` is the caller's own
  statement about itself, and it exists so that whatever launched the process
  cannot be the thing that decides it was allowed to. Without the flag, a client
  starting an `--autopilot` run on its own initiative would be indistinguishable
  from a person typing the same command.
- **It narrows nothing else.** Every budget, the kind allowlist and the ASK
  pre-authorizations apply to an unprompted run exactly as they do to one you
  started. `--unattended --autopilot` is not a mode with different limits; it is
  the same bounded run with one additional permission checked before it begins.
- **`revoke` stops it** along with everything else, without clearing the
  preference — so revoking never looks as though it had silently reset what you
  chose.
- **Only with `--autopilot`** (exit 2 otherwise). A run nobody asked for is
  precisely the one that must be bounded by a grant somebody read, so the
  combination that would be unbounded is refused at the boundary rather than
  later.

### Reported figures

Every percentage in the output names its axis (`60% used (40% free)`,
`20% free`). Every byte figure in a finished run is **measured**, and
`target_met` is decided from the re-measured free space after the run, never
from the sum of what the detectors estimated. The reclaimable figures in a
`--dry-run` preview are estimates and are labelled as such; protected space is
counted but never added into any opportunity total.

The prose output prints a `RecoveryReport`: the goal or target, stop reason
(as both a token and a sentence — a run that stopped because nothing safe
remained says so rather than printing a success word), iterations run, actions
executed, actions declined/skipped, bytes freed, and free space before/after.

A run that stopped because nothing safe remained also reports **what is still
there**, as four separate counts rather than one total: candidates awaiting your
confirmation, candidates whose action cannot run against the resource as it
currently stands (a live tool, work in progress), protected resources, and — for
an `--autopilot` run — candidates this grant was not authorized to take. Each is
a different next step, so they are never summed, and the fourth is kept apart
from the other three for a further reason: those are facts about what is on the
disk, and that one is a fact about the run's authority. Folded into the protected
count it would tell you something is off limits when it is one setting away. They
are counts of candidates, not bytes, because space the run was not permitted to
take is not an opportunity.

Under `--json` this is the `remaining` object, present only for
`stop_reason: "safe_exhausted"` and for an `envelope_refused` stop that had
already discovered candidates: no other stop concluded anything about the
candidates it never reached, so zeros there would be a claim the run did not
make. An envelope refused before discovery — revoked, or a pressure floor not
met — omits the object entirely for exactly that reason, rather than publishing a
breakdown of zeros that would read as a completed search.
See [Safety Model](safety_model.md) and
[Known Limitations](known_limitations.md) for what this loop can and cannot
currently do end to end.

## `glomeris autopilot <show|enable|revoke|run>`

The subcommand is optional and defaults to `show`, so a bare
`glomeris autopilot` reads the grant rather than acting on it. `run` is macOS
only (exits 1 with an error message on other platforms); the other three verbs
work anywhere.

| Verb | What it does | Can it delete? |
|---|---|---|
| `show` | Prints the stored envelope and its path. Does not create the file it reads. | No |
| `enable` | Writes a new envelope from this command line's flags and turns Autopilot on. Requires `--kinds`. | No |
| `revoke` | Turns Autopilot off, keeping the limits. Effective for the next run; nothing to restart. | No |
| `run` | Considers discovered candidates within the envelope. Deletes unless `--dry-run`. Holds the execution lock. | Yes |

The flags, their defaults and their hard ceilings are documented in
[Autopilot](autopilot.md), which is also where the argument for why an LLM
plan file cannot expand authority lives. What this page adds:

- `--max-bytes` takes raw bytes only — unlike `free --target`, there is no
  `GB`/`MB` suffix parsing here, because an envelope is written once and read
  many times and an exact number is easier to audit than a rounded one.
- `--min-pressure` accepts any `PressureState` name case-insensitively
  (`healthy`, `warn`, `pressured`, `critical`, `emergency`) plus the literal
  `none`. An unobservable reading fails any floor you set, rather than passing
  it.
- `--respond-to-alerts` belongs to `enable` and takes no value. It grants the
  one thing the limits above cannot express: that a run may *begin* without
  being asked, in answer to a disk-pressure alert (`free --autopilot
  --unattended`). Off by default and deliberately not implied by `enable`
  itself; see [Runs nobody asked for](#runs-nobody-asked-for) for why those are
  two consents. It changes nothing about what a run may do once it starts, and
  `revoke` suspends it with the rest of the grant while keeping it on file.
- `--preauthorize-ask` is repeatable and takes `kind:reason` using the same
  tags `glomeris actions list` and `glomeris explain` print. Like the four
  limit flags above it, it belongs to `enable` — `run` accepts only
  `--dry-run`, `--plan-file` and `--project-root`, and exits 2 on anything
  else, so a run cannot widen its own grant on the command line.
- `--json` belongs to `show`, `enable` and `revoke`, and prints one shape from
  all three: the grant (`enabled`, `allowed_kinds`, `ask_preauthorizations`,
  the four limits, `min_pressure`, and the two unprompted-run fields below),
  the hard `ceilings`, every choice `enable`
  would accept (`allowlistable_kinds`, `preauthorizable_reasons`,
  `pressure_states`), what no envelope can authorize
  (`never_allowlistable_kinds`, `never_preauthorizable_reasons`,
  `never_executable_labels`), the `ai_authority` sentences, and `stored_at`.
  It always describes what is in force *after* the command ran — `enable`
  reports from what reached the file, not from what it was about to write — so
  a client reads one shape to learn one thing. `run` does not take it: the one
  reason to add it would be to make triggering a run from a GUI easier, and
  [Menu Bar App](menu_bar_app.md#preferences--autopilot) explains why there is
  no such button.
- The JSON carries **two** fields about unprompted runs, and they answer
  different questions. `respond_to_alerts` is the setting as you left it, in
  force or not — the one a settings toggle binds to, so that revoking Autopilot
  does not read as having silently cleared the preference. `starts_unprompted`
  is whether an unprompted run is authorized *right now*: this grant is in force
  **and** it says so. Anything that acts consults the second, and is not the
  thing that computes it — `enabled && respond_to_alerts` evaluated in a client
  would be that client deciding its own authority.
- `run` prints an `AutopilotReport`: one line per candidate with its kind,
  policy label, model rank and outcome, then the run totals (actions
  attempted, actions succeeded, bytes freed, whether a budget stopped it
  early) and the envelope it ran under. Refusals appear here and nowhere
  else — `glomeris history` is a log of what happened to the filesystem, and
  a refusal did not touch it.

Exit codes: `0` success, including a run that found nothing it was allowed to
do; `1` an action failed, or the envelope file could not be read or written;
`2` usage error, including an unknown resource kind, a limit above its ceiling,
or `enable` without `--kinds`; `3` Autopilot is not enabled, so nothing was
attempted; `75` another invocation holds the execution lock.

`3` exists so that "no grant" is distinguishable from "granted, ran, found
nothing" in a script — both of which are quiet, and only one of which means
the user has something to configure.

## `glomeris settings <show|set> [--notify-at-used-percent <N>] [--default-goal-used-percent <N>] [--json]`

The subcommand is optional and defaults to `show`, so a bare
`glomeris settings` reads your preferences rather than changing them.

| Verb | What it does | Can it delete? |
|---|---|---|
| `show` | Prints both preferences and their path. Does not create the file it reads. | No |
| `set` | Stores one or both preferences. Requires at least one flag. | No |

Two preferences live here, and the whole point of the surface is that they are
**not the same number**:

| Preference | Question it answers | Range | Default |
|---|---|---|---|
| `--notify-at-used-percent` | When should Glomeris call my attention to disk usage? | 1–99 percent used | `75` |
| `--default-goal-used-percent` | Where should recovery stop? | 0–100 percent used | `70` |

Both are **percent of capacity USED, never free** — the same axis
`free --goal-used-percent` takes, converted to the core's percent-free
`--target` by the single adapter documented in the `glomeris free` section
above. Every line this command prints names its axis, in prose and in JSON,
because `75` beside `70` with no label is the one misreading this feature
cannot afford.

What this page adds:

- The goal must be **below** the threshold. A goal at or above the point that
  raised the alert would be satisfied the moment it was announced, so the pair
  is refused with the reason `goal_not_below_notify_threshold`.
- Both flags are validated **as a pair, in one step**. Moving from `(75, 70)`
  to `(60, 55)` is a valid destination that no single-field order can reach —
  whichever field moved first would be momentarily invalid against the old
  value of the other. So `set` refuses you for the destination you asked for,
  never for the order your flags happened to appear in.
- A trailing `%` is accepted on either value, because a user typing what they
  read on screen has not made a mistake.
- `set` with neither flag is a usage error rather than a successful no-op: a
  command line that did not say what it wanted should not report that it did
  it.
- The four disk-pressure states (`healthy`, `warn`, `pressured`, `critical`,
  `emergency` boundaries) are **not** configurable and this command cannot
  reach them. That is deliberate: if the `warn` boundary were a preference,
  lowering it would change what a `warn` recorded last week meant, and raising
  it would make the next poll report a transition the disk never made. The
  alert threshold is a separate scalar layered over that machine, and its
  default tracks the `warn` boundary so the product is no quieter than it is
  today.
- Neither preference is an authorization. Raising a goal cannot make a
  `PROTECTED` resource deletable, cannot bypass an `ASK`, and cannot widen an
  Autopilot envelope. These two numbers decide when the product speaks and
  where recovery aims; every gate between a candidate and its deletion is
  elsewhere.
- `--json` belongs to both verbs and prints one shape from both:
  `notify_at_used_percent` and `notify_at_description` for the threshold, a
  `default_goal` object carrying `used_percent`, `free_percent` and
  `description` for the goal, a `bounds` object carrying the four limits above
  as `notify_at_minimum_used_percent`, `notify_at_maximum_used_percent`,
  `goal_minimum_used_percent` and `goal_maximum_used_percent`, `stored_at`, and
  `loaded_from_file`. `bounds` is there so a settings screen can offer a
  control that cannot compose a value this command would refuse, without
  knowing the limits independently and drifting from them; the cross-field rule
  is deliberately absent from it, because a limit that moves as the other
  number moves is not a bound on either one. It always
  describes what is in force *after* the command ran. `loaded_from_file` is
  reported rather than an `is_default` flag, because a user may legitimately
  store the default numbers and a client must be able to tell "nothing chosen
  yet" from "these were chosen". A refused `set` prints a rejection object on
  stdout instead — `reason` (a stable token), `message`, and only whichever of
  `notify_at_used_percent`/`goal_used_percent` the refusal actually involved.
- The file is `~/Library/Application Support/Glomeris/settings.conf`, beside
  the Autopilot envelope and in the same versioned format: a `version` key,
  one `key = value` per line, unknown keys an error rather than noise, and
  loading routed through the validating constructor so a hand-edited file
  cannot install a pair the CLI would have refused.

Exit codes: `0` the preferences were printed, or the change was stored; `1` the
settings file could not be read or written, or what it contains is not valid;
`2` usage error, including `set` with neither flag, or a value on this command
line that was refused.

A refused *stored* file is `1`, not `2`: it is not this command line's mistake,
and a script must be able to tell "you typed 120" from "the file on disk
disagrees with itself".

## `glomeris external-context [--json]`

What optional remote context may reach the model, and what never will. Read-only
and off by default: with nothing configured it prints that nothing leaves this
machine, which is a fact about the shipped product rather than a hypothetical.

Glomeris can optionally read **a pull request's state** from GitHub and **a work
item's state** from Jira, as supporting evidence for ranking a working tree that
looks stale. Neither is deletion authority — see the list at the end of this
section — and neither is on until you write the configuration file.

**This command asks nothing.** Answering "what may leave" by leaving is the one
shape it must not have: you would be making the very requests you are trying to
understand, against a service that logs them, before deciding whether you want
that. Everything it prints comes from your configuration file and from the value
vocabularies the code itself serializes.

Three kinds of line, and the difference between them is the point:

| Section | What it describes |
|---|---|
| Providers | Your local setup. Whether each provider is configured, whether it is *usable*, the environment variable its credential is read from, the endpoint, and for GitHub the one repository host in scope. |
| Fields that may be sent | What could actually leave, one line per serialized key, each with its complete set of possible values. |
| Never sent | The explicit negative. |

What this page adds:

- **"Not configured" and "configured but not usable" are different rows.** A
  provider whose credential environment variable is unset is reported as
  configured with a refusal naming the variable, not as absent. The same
  distinction runs all the way through the feature: a provider that could not
  answer never reports an absence, and `no pull request was found` is a
  different value from `GitHub could not be reached`.
- **The field list is the complete set, and it is derived rather than written.**
  Each state vocabulary comes from the enum the projection serializes, so a
  variant added later cannot widen what really travels while this page keeps
  understating it. A remote state reaches a model as a bounded token —
  `merged`, `in_progress`, `none_observed` — with how many days ago it was read,
  and with the *kind* of failure when a provider could not answer.
- **Nothing identifying goes.** Not the repository, the branch, the issue key,
  the pull request's number, title, body or author, your account, or any path.
  The command prints that negative explicitly, because a list of what travels
  does not answer "did you send my branch name".
- **Your Jira account address is not echoed**, even though the configuration
  file holds it. It identifies a person rather than a setting anybody debugs,
  and this report gets pasted into bug reports. The site is shown, because you
  need to see which one is asked.
- **No credential value appears anywhere**, in prose or in JSON. The variable is
  named; the value is never read by this command at all.
- **Jira has no host scope and does not claim one.** Correlation there requires
  an explicit, deterministic issue key in the branch name — `HORO-1234`, not a
  fuzzy match from `fix-storage-stuff` — so there is no remote to compare
  against. GitHub's `repository_host` is the most privacy-relevant line in the
  report: a working tree whose remote is anywhere else is never named to that
  provider at all.
- **None of it is permission.** A merged pull request and a closed ticket are
  context for *ranking*. They cannot make a working tree deletable: a dirty
  tree, an untracked file, a local commit made after the merge, or a process
  holding the directory open each keep it protected or uncertain whatever a
  remote service says. That is enforced in the policy engine, which never sees
  external context, and pinned by
  `tests/external_context_grants_no_authority.rs`.
- **Read-only by construction, not by convention.** The providers can name
  exactly one transport, whose whole vocabulary is a single GET;
  `scripts/check-external-context-is-read-only.sh` proves the module names no
  mutating verb, no second transport, and nothing that prints or writes.
- `--json` prints the same report — each provider's configured and usable state,
  its credential variable, endpoint and host scope, every field that may be sent
  with its vocabulary, and the `never_sent` list. This is what the menu-bar
  app's privacy preview reads.
- The file is
  `~/Library/Application Support/Glomeris/external-context.conf`, beside
  `settings.conf` and in the same versioned format.

Exit codes: `0` the report was printed; `2` usage error — this command takes no
arguments besides `--json`.

An unparseable configuration file is `0`, not `1`, which is the opposite of
`settings show`. The two commands answer different questions. `settings show` is
asked "what are my preferences", and an unreadable file means it has no answer.
This one is asked "what could leave this machine", and an unreadable file has a
complete and reassuring answer — nothing is configured, so nothing leaves — that
a non-zero exit and a bare stderr line would throw away. The refusal is reported
as the first line of the report instead.

## `glomeris pressure <show|notified|respond> [<answer>] [--json]`

The seam between the background monitor and the menu-bar app. You are unlikely
to type it; the app runs it every poll.

It exists because of a constraint neither side can work around alone: only an
**app bundle** can put buttons on a macOS notification, and the monitor
(`glomeris daemon`) is a bare `launchd` process, not a bundle. So the monitor
records that a notification is *owed* and the app raises it. This command is
how the app asks what is owed and reports which button was pressed.

The subcommand is optional and defaults to `show`, so a bare
`glomeris pressure` reads rather than writes.

| Verb | What it does | Can it delete? |
|---|---|---|
| `show` | Prints the current episode, whether a notification is owed, your alert threshold, your recovery goal, and the answers that can be given. | No |
| `notified` | Records that the notification was put on screen, so it is not raised again for the same episode. | No |
| `respond` | Records which button was pressed. Takes one of the three answers below. | No |

### What an episode is

One continuous stretch of disk usage being at or above
`settings --notify-at-used-percent`. It opens the first time an observation
reaches that threshold and closes only once usage has fallen **three percentage
points below** it. That gap is the hysteresis, and it is the whole reason one
spell of a full disk produces **one** notification rather than one per poll: a
volume hovering exactly on the threshold stays inside one episode, and it takes
a real recovery rather than a rounding wobble to end it.

Episode ids are monotonic and never reused, because the app uses the id as the
notification's identifier — two notifications about one episode coalesce, and
two episodes never do.

### The three answers

| Answer | What it means |
|---|---|
| `review_and_recover` | Open the recovery screen for this episode. **Opens** it — it does not start recovery, and deletes nothing. |
| `remind_later` | Say nothing for two hours, then notify again if usage is still above the threshold. The episode stays open, so this is a delay, not a dismissal. |
| `ignore_episode` | Say nothing more about **this** spell of disk pressure. Monitoring continues and the next episode notifies again. |

Hyphens are accepted too (`respond review-and-recover`), because that is what a
command line looks like. Nothing else is: an unrecognized answer is refused
rather than mapped onto the nearest match, since the nearest match to a
misspelled `ignore_episode` is the one that opens recovery.

`ignore_episode` **cannot switch notifications off.** It is stored on the
episode, so it expires with it — there is no answer here, and no flag anywhere,
that silences monitoring indefinitely. That is the point of storing the answer
on the episode rather than in settings.

### What this command does not do

- **It does not observe.** `show` never opens or closes an episode and never
  decides that a notification is owed; only the monitor's own poll does. On a
  machine where the monitor is not installed there is therefore no episode at
  all, and `show` says so while still reporting whether usage is above your
  threshold.
- **It does not delete anything, and it cannot widen what may be deleted.** An
  episode decides when you are spoken to, never what is permitted. Answering
  `review_and_recover` opens a screen. Every gate between a candidate and its
  deletion is elsewhere and unchanged.
- **It does not start recovery**, automatically or otherwise. Crossing a
  threshold raises a question; a human answers it.

### `--json`

Belongs to all three verbs and prints one shape from all three, always
describing the state **after** the command ran. This is what the menu-bar app
reads.

`current` carries the disk reading with both percentages under separate names
and the byte figures; `notify_at_used_percent`/`notify_at_description` the
threshold; a `default_goal` object the recovery goal, in the same shape
`settings --json` prints it; `threshold_crossed` and `notification_due` the two
facts the app acts on; `episode` the episode's identity, its opened/peak/latest
readings, how many notifications have been raised, the answer if one was given,
and the snooze deadline if one is pending; `responses` the answers that can be
given, so the app offers buttons it did not invent.

A refusal prints a **rejection object on stdout** instead — `reason` as a stable
token (`no_open_episode`, `no_notification_due`) and `message` — rather than
writing to stderr, because a refusal here is an ordinary outcome that the app
has to read and display.

Exit codes: `0` the episode was printed, or the answer was recorded; `1` the
episode state or your settings could not be read or written, or filesystem usage
could not be measured; `2` usage error, including an answer that is not one of
the three; `3` there was **nothing to record**.

`3` rather than `1` for that last case, and this is the load-bearing part of the
contract: no episode is open, or no notification was owed. It is **not a
failure**. The usual cause is that the disk recovered while the notification was
on screen, and the app polls this command every thirty seconds — a client that
treated it as an error would retry at poll speed forever.

## `glomeris workflow-profile [show|record] [--json] [--project-root <path>]`

How this machine has been used, over several looks. `show` reports the
stored baseline and is the default; `record` takes one observation and adds it.

A single look cannot honestly say how you usually work. Six working trees of one
repository today might be this week's shape or this afternoon's, and a product
that called it a habit on one sighting would be inventing the most persuasive
thing it knows about you. So the baseline is built from observations taken over
time, and until enough of them exist it says so.

| Shape | What it means |
|---|---|
| `serial_single_checkout` | One checkout at a time, staying on one branch. |
| `serial_multi_branch` | One checkout at a time, moving between branches. |
| `parallel_multi_worktree` | Several working trees of a repository at once. |
| `mixed` | Some repositories with several working trees, some with one. |
| `unknown` | No layout claimed — either too few observations to say, or looks that found no repository to say anything about. |

**None of these is the good one.** There is no ordering between them and no
scale, and the vocabulary deliberately contains no word for the person operating
the machine — not `advanced`, not `beginner`, not any synonym.
`scripts/check-workflow-history-labels-no-one.sh` holds that mechanically across
the whole feature, because the temptation is real: `parallel_multi_worktree` is
the shape of somebody juggling six checkouts, and calling that "advanced" would
make `serial_single_checkout` a judgement.

What this page adds:

- **Three observations before any shape is claimed.** Below that the shape is
  `unknown` and the confidence is `insufficient` — and the counts behind that
  verdict are still printed, because two observations three minutes apart and
  two three weeks apart are both insufficient and only the counts say which this
  is.
- **The counts are the supporting evidence, not decoration.** How many days the
  observations span, how many repositories were seen, how many looks found one
  working tree per repository, how many found several, how many found both, how
  many times a lone checkout had moved branch since the last look, and the most
  working trees ever seen at once.
- **"Never collected", "could not be read" and "collected" are three states,
  not two.** A baseline whose file could not be read is reported as a read
  failure, never as a baseline holding nothing: the two have different next
  steps, and a failed probe that arrived as an empty answer would be the whole
  defect this feature exists to avoid.
- **What is stored is counts and opaque local ids.** No path, no branch name, no
  repository name, no file contents. Two repositories are never named, only
  counted — which is also why the report can be pasted into a bug thread.
  `tests/workflow_history_holds_no_identity.rs` asserts it against the bytes on
  disk rather than against the type.
- **Bounded on both axes.** At most 64 observations, kept at most 90 days,
  oldest dropped first. Age before count, so a burst of recent observations
  cannot push out the records that give the baseline its span.
- **`record` declines when the last observation is too recent** — at most one an
  hour — and when the clock has moved backwards. Both are the bounds working:
  spacing is what stops a loop from manufacturing a pattern, so "too soon" is
  the answer rather than a failure to get one, and both exit `0`.
- **`show` writes nothing.** A `show` that recorded an observation "so there is
  something to show" would make the observation count a measure of how often
  somebody looked; `tests/subcommand_safety_is_honest.rs` runs it against a
  disposable `$HOME` and requires the tree to be byte-identical afterwards.
- **`record` looks at the same project roots `detect` does**, and accepts the
  same repeated `--project-root <path>` to narrow them. Counting only the
  current directory would make the baseline a record of where the command was
  typed.
- **None of it is permission.** The shape here can order and explain what
  `detect` found. A working tree that is dirty, in use, or holds commits that
  exist nowhere else stays protected by its own evidence, whatever the baseline
  says about how you usually work — enforced by the policy engine never seeing
  it, and pinned by `tests/workflow_history_has_no_authority.rs`.
- The file is `~/Library/Application Support/Glomeris/workflow-history.json`,
  beside `settings.conf`.

Exit codes: `0` the report was printed, including when `record` declined; `1`
the history file could not be located or written; `2` usage error — an unknown
verb, or an argument besides `--json` and `--project-root`.

## Exit codes

Also available as `glomeris help exit-codes`, which is the copy to trust — it
renders from the same table as the per-command help, so a command's exit codes
cannot drift between its own `--help` and the summary.

- `0` — success.
- `1` — a macOS-only command was run on a non-macOS platform, a
  platform-level operation (e.g. reading the `launchd` plist path) failed,
  or (for `llm-plan` specifically) the LLM provider call/response parsing
  failed, or `--plan-file` named an unreadable path, or (for `llm-check`
  specifically) the check ran and did not pass.
- `2` — usage error: unknown top-level command, unknown `daemon` subcommand,
  missing/unrecognized `free` arguments, or (for `llm-plan`/`llm-check`
  specifically) an unrecognized argument, a missing flag value, missing
  live-mode LLM environment configuration, a base URL that cannot work, or an
  `--api-key`/`--key`/`--token` flag.
- `3` — (`autopilot run` and `free --autopilot` only) this run was not
  authorized, so nothing was attempted: either Autopilot is not enabled, or
  `--unattended` was given and the grant does not allow starting a run unasked.
  One code for both because to a script they are the same fact.
- `75` — (`free`/`emergency`/`execute`/`autopilot run` only) the HORO-1054
  execution lock is already held by another `glomeris` invocation.

`glomeris execute` has its own, more specific set of exit codes (`0`–`5`
plus `75`) — see its own section above for the full table; a couple of
those codes (`1`, `2`) overlap this list's meanings but are worth reading
in full since `execute` is the one subcommand with real destructive
consequences.

See `glomeris llm-plan`'s and `glomeris llm-check`'s own sections above for
those subcommands' exit codes in full detail — `llm-check`'s `1`/`2` split in
particular carries a meaning this list cannot: whether a report exists.
