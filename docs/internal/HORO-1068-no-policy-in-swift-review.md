# No-Policy-in-Swift Review — GlomerisMenuBar (HORO-1068)

> **Internal research record — not public product documentation.**
> Not part of the mdBook site (`book/src/`), not linked from `SUMMARY.md`,
> not referenced from `README.md`. Lives under `docs/internal/` specifically
> so the `docs.yml` GitHub Pages pipeline (which only builds `book/` via
> mdBook and `cargo doc` from Rust source comments) never touches it. Same
> convention as `docs/internal/dogfood/2026-09-13-v0.2.0-founder-dogfood.md`.

## Metadata

- **Date:** 2026-09-18.
- **Ticket:** HORO-1068, Epic HORO-1043 (Glomeris MVP 2.0), Phase C release/QA gate item.
- **Scope:** confirm zero duplicated authorization/policy/execution semantics in the Swift codebase under `macos/GlomerisMenuBar/`, and back that confirmation with a mechanical CI check rather than reviewer say-so alone.
- **Reviewed commit:** `802a92c` (`origin/main` at the time of this review), plus every file under `macos/GlomerisMenuBar/Sources/` up through HORO-1066 (`HistoryAuditSectionView.swift`), the newest UI addition and the one file not covered by any prior per-ticket review.
- **Related precedent:** HORO-1058 ("Independent adversarial review of the new glomeris execute surface", Phase B gate item) established that this campaign's review records live under `docs/internal/` and are Git-tracked, not public. That review closed via a Jira comment reconfirming `src/policy/approval.rs`/`src/executor/mod.rs` were untouched by PR #43, without a standalone `docs/internal/` file of its own — so this document also follows the founder-dogfood report's file format (Metadata / findings / verdict), the only existing `docs/internal/` artifact in this repo, for structural consistency.

## Invariant under review

Established in `macos/GlomerisMenuBar/Sources/GlomerisMenuBarApp.swift`'s standing project-rule header comment: GlomerisMenuBar is a **thin client only** over the Rust `glomeris` CLI's `--json` output. It must never perform policy classification, evidence correlation, action planning, or filesystem execution logic itself. Concretely, for this ticket:

- Button enablement / execution gating / any authorization-adjacent decision must be a direct read of a structured, typed field the Rust CLI already computed — `executable`, `requiresConfirmation` (per offered action), `outcome`, `abortReason`, `reclaimableBytesIsLowerBound`, etc.
- `policyLabel` (Swift `Codable` mirror of Rust's `policy_label`) may be read and **displayed as text** — that's already done deliberately in several files — but its *value* must never be branched on (`if`/`guard`/`switch`/ternary/equality) to decide anything.

## Method

1. Enumerated every `.swift` file under `macos/GlomerisMenuBar/Sources/` (10 files — see below).
2. `grep -n` for `policyLabel`/`policy_label` in every file, then manually read every match in its surrounding context.
3. For each match, classified it as one of: (a) type/property declaration, (b) plain assignment / passthrough, (c) display (`Text(...)`, string interpolation, `labeledRow(...)`), or (d) a conditional/branch on the value. Only (d) would be a violation.
4. Cross-referenced against the established correct pattern already in the codebase (`CandidateDetailViewModel.isCleanEnabled = report.executable`, `requiresConfirmation = report.offeredActions.first?.requiresConfirmation ?? false`) to confirm gating logic elsewhere follows the same shape.
5. Paid particular attention to `HistoryAuditSectionView.swift` (HORO-1066), the newest file and the only one not previously reviewed under an earlier per-ticket wiring-invariant test.
6. Wrote the mechanical CI guard (`scripts/check-no-policy-label-branching.sh`, see below) and ran it against the current tree, then against a deliberately-injected violation, to confirm it actually enforces this — not just that this review's manual read is correct.

## Files audited

| File | `policyLabel`/`policy_label` usage | Verdict |
|---|---|---|
| `CandidateDetailView.swift` | Property declaration (`let policyLabel: String`), plain assignment from `report.policyLabel`, and one display use — `labeledRow("Policy", viewModel.policyLabel)`. Gating (`isCleanEnabled`, `requiresConfirmation`) is a direct read of `report.executable` / `report.offeredActions.first?.requiresConfirmation`, never of `policyLabel`. | Clean — display only. |
| `CandidatesSectionView.swift` | Comment-only references (`policy_label`, `policyLabel`) explicitly documenting that this file never re-derives cleanability from those fields; row rendering reads `executable`/`offered_actions`/`refusal_reason` directly. No `policyLabel` field access at all in executable code. | Clean — no field access. |
| `GlomerisClient.swift` | No reference. Comment states this file is "plumbing only — no policy/evidence/action/execution logic." | Clean. |
| `GlomerisDtos.swift` | `Codable` property/`CodingKeys` declarations only (`let policyLabel: String`, `case policyLabel = "policy_label"`) across three DTOs. No conditional logic of any kind lives in this file — it is pure decode-shape mirroring of the Rust JSON contract. | Clean — declarations only. |
| `GlomerisMenuBarApp.swift` | No reference. Hosts the standing project-rule header comment this whole review enforces. | Clean. |
| `GlomerisPopoverView.swift` | No reference. | Clean. |
| `HistoryAuditSectionView.swift` (HORO-1066, newly reviewed) | `ActionHistoryRowViewModel.policyLabelText` is a plain assignment (`policyLabelText = dto.policyLabel`) used only in one `Text(...)` display line (`Text("(\(row.policyLabelText), \(row.source))")`). The row's `isSuccess`/`outcomeText` — which drive the view's color-coding — are a `switch` on `dto.outcome` (a structured field), explicitly documented in this file's own doc comments as "a field read of `outcome` — never `policyLabel`." | Clean — display only, and the file's own comments assert the same invariant this review confirms. |
| `ProjectRootsPreferencesView.swift` | No reference. | Clean. |
| `ProjectRootsStore.swift` | No reference. Comment states this file stores UI-local preference, "not policy state." | Clean. |
| `StatusHealthSectionView.swift` | No reference. Comment disclaims making "a policy/health decision of its own." | Clean. |

**No violation was found.** Every `policyLabel`/`policy_label` occurrence in `Sources/` is a type declaration, a `Codable` mirror, a plain passthrough assignment, or a display line. Gating logic in every file that has any (`CandidateDetailView.swift`, `HistoryAuditSectionView.swift`) reads structured typed/boolean fields (`executable`, `requiresConfirmation`, `outcome`) exclusively.

This corroborates — rather than substitutes for — the existing Swift-level mechanical tests already in `macos/GlomerisMenuBar/Tests/` that assert the same thing per-file (e.g. `CandidateDetailViewTests.testNoCodeBranchesOnPolicyLabelText`, which strips comments from `CandidateDetailView.swift` and asserts it contains neither `policyLabel ==` nor `== report.policyLabel`). Those tests only run inside the Swift test target for the files they were written against; they do not protect a brand-new file until someone remembers to write an equivalent test for it. That gap is what the CI guard below closes.

## Mechanical CI guard

Per the ticket's explicit ask — "backed by a mechanical CI check, not reviewer say-so alone" — this review is backstopped by `scripts/check-no-policy-label-branching.sh`, wired into `.github/workflows/ci.yml` as the `no-policy-in-swift` job.

- Scans every `.swift` file under `macos/GlomerisMenuBar/Sources/` for lines mentioning `policyLabel`/`policy_label`, and flags any such line that also looks like a conditional branch: an `if`/`guard`/`switch` keyword, a ternary (`?`...`:`) shape, or a string-literal `==`/`!=` comparison. Full-line `//`/`///` comments are excluded from flagging (this codebase's own doc comments intentionally narrate the anti-pattern in prose as part of documenting why it's avoided).
- Deliberately scoped to `Sources/`, not `Tests/`: the existing test files (e.g. `CandidateDetailViewTests.testNoCodeBranchesOnPolicyLabelText`) legitimately embed the very string patterns this guard looks for as quoted string-literal arguments to `.contains(...)`/`XCTAssertFalse(...)`, to prove their *absence* elsewhere — a grep heuristic cannot tell that apart from a real comparison, and `Sources/` is where an actual UI-gating decision would live regardless.
- **Verified to pass** against the current tree (`bash scripts/check-no-policy-label-branching.sh` → `PASS`, exit 0).
- **Verified to fail** against a deliberately-injected violation: a temporary line `if report.policyLabel == "AutoSafe" { isCleanEnabled = true }` was inserted into `CandidateDetailView.swift`, the guard was run and confirmed to exit non-zero with a clear `VIOLATION: ... conditional keyword (if/guard/switch) on the same line` message pointing at the exact file/line, and the temporary line was then removed before any commit (confirmed via `git diff` showing no residual changes to that file).

## Verdict

**PASS.** No duplicated authorization/policy/execution semantics exist in the Swift codebase as of this review. `policyLabel` is read and displayed as text in two files (`CandidateDetailView.swift`, `HistoryAuditSectionView.swift`) and never used to decide executability, confirmation, or any other authorization-adjacent outcome anywhere in `macos/GlomerisMenuBar/Sources/`. The mechanical CI guard (`no-policy-in-swift` job) now enforces this on every future PR, not just at review time.

## Explicitly not covered by this review

- `macos/GlomerisMenuBar/Tests/` was manually spot-checked (see Method) but is intentionally excluded from the mechanical guard's scope, for the reason given above.
- This review covers only the Swift codebase's *reading* of `policyLabel`. It does not re-review the Rust side's computation of `policy_label`/`executable`/`requires_confirmation` themselves — that ground was covered by HORO-1058's adversarial review of `glomeris execute` and by the individual Phase C tickets' own PR reviews.
- Future Swift files are only protected by this guard if the CI job continues to run and continues to pass — this document is a point-in-time confirmation, not a standing guarantee independent of the CI check it references.
