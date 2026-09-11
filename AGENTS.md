# AGENTS.md — Glomeris

Tool-agnostic instructions for any AI coding agent working in this repo.
Claude Code additionally reads `CLAUDE.md`; where they overlap, the two must
not conflict — this file is the portable subset.

## Safety-critical context

This tool can delete real user data. Treat every change touching
`scanner/`, `detectors/`, `evidence/`, `policy/`, `actions/`, or `executor/`
as safety-critical:

- Missing or failed evidence is never treated as "safe to delete."
- `PROTECTED` classifications cannot be bypassed by any code path,
  including LLM-influenced ones.
- No feature may require a successful SQLite write, network access, or an
  LLM call to function — `emergency` mode must survive all three failing.

## Branching

- Never commit directly to `main`.
- One git worktree per ticket, branched from latest `main`.
- Branch name: `v0.1.0/<TICKET-ID>/<short_summary>` (snake_case summary,
  ≤30 chars), e.g. `v0.1.0/HORO-950/policy_engine`.

## Commits

- Gitmoji style: `<emoji> (<scope>): <summary>`. See
  https://gitmoji.dev/ for emoji choices.
- One logical change per commit — one type, one function, one test, one
  doc edit. Do not bundle implementation + tests + docs into one commit.
- Never squash a PR's commit history at merge time.

## Pull requests

- Every change goes through a PR — no local merges to `main`.
- Title: `[<TICKET-ID>] <emoji> (<scope>): <summary>`.
- Body must follow `.github/PULL_REQUEST_TEMPLATE.md` in full.
- Merge method: **create a merge commit**. Never squash, never rebase-merge.
- Resolve every review thread (or explain why not applicable) before
  merging. Do not merge on red CI.

## Before opening a PR

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check
```

All four must pass locally. Fix failures — do not suppress them.

## Dependencies

- Ask before adding a new crate dependency; justify it over hand-rolling
  or an existing dependency's capability.
- No paid/cloud LLM provider may be required by default CI.
