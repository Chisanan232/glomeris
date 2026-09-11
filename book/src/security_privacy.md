# Security & Privacy

## No telemetry, no cloud requirement

Glomeris has no backend, no account system, and no telemetry. Every command
documented in this book runs entirely locally. The only network access
anywhere in the codebase is the optional BYOK LLM planner's HTTP call to
whatever endpoint you configure (see [BYOK LLM Planner](byok.md)) — nothing
else in the crate makes a network request.

## No raw filesystem inventory sent to any LLM by default

The LLM planner is not wired into the binary at all today (see
[BYOK LLM Planner](byok.md)), so no filesystem data is sent to any LLM in
current builds. Even the library-level design constrains this for the
future: `LlmResourceView` is an explicit, bounded, hand-maintained
projection of one `Evidence` record — never a serialization of `Evidence`
itself — so a future field added to `Evidence` does not automatically start
flowing to a model; a human has to explicitly add it to the projection.

## BYOK secret handling

- `OpenAiCompatibleProvider` deliberately does not derive `Debug`, so an
  accidental `{:?}`-log of the provider value cannot leak the API key.
- Verified by test: neither `LlmError`'s `Debug` output nor a real failed
  `complete()` call's error output contains the key.
- There is currently no CLI/env-var wiring for this key at all — see BYOK
  page for what that means in practice today.

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
