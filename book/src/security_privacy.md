# Security & Privacy

## No telemetry, no cloud requirement

Glomeris has no backend, no account system, and no telemetry. Every command
documented in this book runs entirely locally. The only network access
anywhere in the codebase is the optional BYOK LLM planner's HTTP call to
whatever endpoint you configure (see [BYOK LLM Planner](byok.md)) — nothing
else in the crate makes a network request.

## No raw filesystem inventory sent to any LLM, except resource paths when you explicitly invoke `llm-plan` with live config

As of HORO-1008, `glomeris llm-plan` is wired into the binary (see
[BYOK LLM Planner](byok.md)). No filesystem data is sent anywhere unless you
explicitly run that subcommand without `--plan-file` *and* have all three
`GLOMERIS_LLM_*` environment variables set — every other command in this
book never makes a network request. Even then, what is sent is bounded and
explicit: `LlmResourceView` is a hand-maintained projection of one
`Evidence` record containing a resource's path/id, kind, size estimate,
age, regenerability, and completeness — never raw file contents, never a
directory listing, never anything beyond that fixed set of fields. Adding a
field to `Evidence` later has no effect on what a model sees unless a human
explicitly adds it to `LlmResourceView` too.

## BYOK secret handling

- `OpenAiCompatibleProvider` deliberately does not derive `Debug`, so an
  accidental `{:?}`-log of the provider value cannot leak the API key.
- Verified by test: neither `LlmError`'s `Debug` output nor a real failed
  `complete()` call's error output contains the key.
- `glomeris llm-plan` reads the key only from `GLOMERIS_LLM_API_KEY` via
  `actions::llm::provider_from_env` — `std::env::var` for this value is
  called nowhere else in the crate. The key is never accepted as a CLI
  flag: `--api-key`/`--key`/`--token` are explicitly rejected with an error
  pointing at the environment variable instead, so a key never appears in
  `ps` output or shell history. See the [BYOK page](byok.md) for the full
  configuration contract.

## Subprocess invocation

Every external tool Glomeris shells out to — `docker`, `brew`, `lsof`,
`git`, `pgrep`, `launchctl`, `cargo`, `npm`/`pnpm`/`yarn`, `brew` (as an
action) — is invoked via `std::process::Command::new(program).args(args)`
with a `Vec<String>`/`&[&str]` argument array. There is no `sh -c` anywhere
in this codebase, and no place where untrusted content is interpolated into
a shell command string.

The one place a *string* is built and passed to an external tool is
`platform::macos::notify`'s `osascript` invocation, which constructs an
AppleScript source snippet via `format!`. That string's only two
substituted values are the notification title and body produced by
`monitor::notifier::notification_text`, which only ever emits fixed template
text derived from the closed `PressureState` enum — never external or
attacker-controlled input — and the whole script is still passed to
`osascript` as a single argument in an argument array, never through
`sh -c`.

## No arbitrary LLM-composed commands reach execution

`ActionPlan`/`ActionStep` deliberately never implement `Deserialize`, so no
externally-sourced input (a parsed LLM response, a network payload) can ever
construct one directly. The only way external input reaches execution is by
selecting a pre-registered `ActionId` string, which a real `Action::plan`
implementation then interprets against `Evidence` this process already
collected and trusts independently. See [Safety Model](safety_model.md) for
how this interacts with policy classification.

## Bounded, printable failure reporting

`EmergencyReport` caps the number of retained error strings at 8, and no
type in this crate's execution/reporting path derives `Serialize` in a way
that would let an unbounded or adversarial input grow a report without
limit.
