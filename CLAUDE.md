# CLAUDE.md — Glomeris

Project-specific overrides for `~/.claude/CLAUDE.md`. Read the global file
first; this file wins where they conflict.

## Repository Identity

- Repo: `Chisanan232/glomeris`, public, Apache-2.0.
- Language/runtime: Rust (2021 edition), single workspace/package for MVP.
- Product: evidence-first, policy-constrained developer storage autopilot
  for macOS. See `HORO-943` (Jira) for the full product brief and North
  Star. Canonical safety invariant: **AI can recommend. Policy decides.
  Executor verifies. Filesystem reality wins.**
- Dependency policy: prefer mature, widely-used crates over hand-rolled
  primitives; every new dependency gets `cargo deny` license/advisory
  review before merge.

## Architecture Constraints

- Start as one Cargo package with strong module boundaries. Split crates
  only when a real compile/reuse/release boundary justifies it — do not
  pre-create module structure ahead of real code.
- Expected top-level responsibilities as code lands: `core/config`,
  `monitor`, `scanner`, `detectors`, `evidence`, `policy`, `actions`,
  `executor`, `persistence`, `providers`, `platform/macos`, `bin/`.
- No arbitrary LLM-generated shell/command strings may reach process
  execution — LLM output selects only typed, pre-registered action IDs.
- Policy classes `AUTO_SAFE` / `ASK` / `PROTECTED` (+ `UNKNOWN`/incomplete
  evidence) are enforced in one deterministic module; nothing upstream of
  it (including LLM planning) may bypass it.
- SQLite persistence is best-effort only — every code path that can run
  without a successful DB write must be tested doing so (this is load-
  bearing for `emergency` mode).

## Package and Build Commands

```sh
cargo build              # debug build
cargo build --release    # release build
cargo test                # unit + integration tests
cargo fmt --check         # formatting gate
cargo clippy --all-targets -- -D warnings   # lint gate
cargo deny check           # license/advisory/dependency gate
```

## Testing Tooling

- Test runner: built-in `cargo test` (+ `cargo nextest` if added later for
  speed — evaluate before adding).
- Fixture strategy: synthetic fixture directory trees under `tests/fixtures/`
  for scanner/detector/policy tests; never depend on the developer's real
  home directory contents in a test.
- No paid LLM provider credentials in CI — LLM planner tests use mocked
  responses only.

## Type Checker / Linting

- `cargo clippy --all-targets -- -D warnings` is the type/lint gate. No
  `#[allow(...)]` without a comment explaining why.
- `rustfmt` (default config) is the formatter; run before every commit.

## Source-of-Truth Systems

- Issue tracker: Jira, project `HORO`, epic `HORO-943`.
- Discovery source of truth: `HVDL-26` (Jira Product Discovery).

## Merge Strategy

- **Create a merge commit** on every PR. Never squash, never rebase-merge.
  Do not let GitHub's default squash setting silently apply — verify the
  merge method per PR.
- Commit history inside a PR is deliberately fine-grained (see Gitmoji /
  small-commit policy in the global CLAUDE.md) and must not be squashed
  away.

## Polling Intervals

- No standing PR-health/release-watch automation configured yet for this
  repo; use the global defaults (30 min / 5 min) if/when cron polling is
  set up.

## Language-Specific Repair Skills

- Rust has no dedicated repair skill in the current skill set. Debug
  `cargo test`/`clippy`/`cargo deny` failures directly; escalate to
  `opus-architect` only for genuine design-level breakage, not routine
  fixes.

## Branch / Worktree Convention (repo-specific detail)

Release-or-phase prefix for this MVP: `v0.1.0`. Example:
`v0.1.0/HORO-947/bounded_scanner`. Worktrees are siblings of
`~/Bryant-Developments/glomeris/`.
