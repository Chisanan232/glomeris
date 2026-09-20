# Security & Privacy

## No telemetry, no cloud requirement

Glomeris has no backend, no account system, and no telemetry. Every command
documented in this book runs entirely locally. The only network access anywhere
in the codebase is to whatever BYOK endpoint you configure yourself, from
exactly two commands — `glomeris llm-plan` (the planner) and `glomeris
llm-check` (the connection test, which sends two fixed words and nothing about
this machine). See [BYOK LLM Planner](byok.md). Nothing else in the crate makes
a network request.

## No raw filesystem inventory, and no filesystem paths, sent to any LLM

As of HORO-1008, `glomeris llm-plan` is wired into the binary (see
[BYOK LLM Planner](byok.md)). No filesystem data is sent anywhere unless you
explicitly run that subcommand without `--plan-file` *and* have all three
provider settings configured — every other command in this book either makes no
network request at all, or (`glomeris llm-check`) sends two fixed words that
describe nothing local. Since HORO-1309 those three settings may come from the
menu-bar app's Settings rather than from `GLOMERIS_LLM_*` in a shell, which
changes where they are stored and nothing about what is sent: the GUI's AI Plan
card spawns the same subcommand with the same bounded payload. Even then, what is sent is bounded and
explicit: `LlmResourceView` is a hand-maintained projection of one
`Evidence` record containing a resource's kind, size estimate, age,
regenerability, completeness, and the action ids offered for it — never raw
file contents, never a directory listing, never anything beyond that fixed
set of fields. Adding a field to `Evidence` later has no effect on what a
model sees unless a human explicitly adds it to `LlmResourceView` too, and
adding a field to `LlmResourceView` itself fails a test that pins its
serialized key set.

**Nor are resource paths sent.** Until HORO-1298 each view identified its
resource by its real `ResourceId`, which for the five path-backed resource
kinds renders as an absolute path — and an absolute path under `$HOME`
discloses the OS account name and the machine's directory layout. Note that
`--project-root` never bounded this: it scopes the cargo and node detectors
only, while the Xcode detector is `$HOME`-bounded and the Homebrew and
Docker detectors shell out and are bounded by neither. Each view now
carries a positional wire alias — `resource_1`, `resource_2`, … — and the
table mapping an alias back to a real `ResourceId` has no `Serialize`
derive and stays in the process's memory. The aliases are positional rather
than hashed on purpose: a hashed path would be both brute-forceable (one
unknown segment in `/Users/<name>/Library/...`) and a stable handle for
correlating your machine across requests.

Run `glomeris llm-plan --print-payload` to see the exact request a live run
would send, including the local alias table, without sending it and without
configuring a credential. Three tests enforce the property, the strongest
of them (`tests/llm_plan_egress_privacy.rs`) by capturing the real HTTP
request with a loopback listener and asserting the transmitted bytes
contain no path separator at all.

## BYOK secret handling

- `OpenAiCompatibleProvider` deliberately does not derive `Debug`, so an
  accidental `{:?}`-log of the provider value cannot leak the API key.
- Verified by test: neither `LlmError`'s `Debug` output nor a real failed
  `complete()` call's error output contains the key.
- `glomeris llm-plan` and `glomeris llm-check` read the key only from
  `GLOMERIS_LLM_API_KEY` via `actions::llm::provider_from_env` —
  `std::env::var` for this value is called nowhere else in the crate. The key
  is never accepted as a CLI flag: `--api-key`/`--key`/`--token` are
  explicitly rejected with an error pointing at the environment variable
  instead, so a key never appears in `ps` output or shell history. The
  rejection message names the **flag only**, never the value beside it, so
  `--api-key=<secret>` does not get echoed back. See the
  [BYOK page](byok.md) for the full configuration contract.
- In the menu-bar app (HORO-1309) the key lives in the **login keychain**, not
  in `UserDefaults`, not in a file the app writes, and not in a log line. It is
  read at the moment a `glomeris` child process is spawned, placed in that
  child's environment, and not cached. `GlomerisLlmSettingsStore` deliberately
  exposes no getter for it — the only code that can read the value is the one
  function that builds a child environment — and the settings screen binds it to
  a `SecureField` with no reveal control and no read-back. Removing it is an
  explicit destructive control, not a side effect of clearing a field.
  `scripts/check-credential-store-uses-keychain.sh` enforces all of that in CI
  over every line in the app that touches the key, so a future shortcut —
  stashing it in a plist "just for now", printing it in a debug view — fails a
  check rather than shipping.

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
