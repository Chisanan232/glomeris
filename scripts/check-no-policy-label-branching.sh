#!/usr/bin/env bash
#
# check-no-policy-label-branching.sh
#
# HORO-1068 mechanical CI guard: GlomerisMenuBar (the macOS thin client) must
# never branch UI behavior (button enablement, execution gating, or any
# authorization-adjacent decision) on the *content* of the `policyLabel` /
# `policy_label` string field. It may only read structured typed/boolean
# fields (`executable`, `requiresConfirmation`, `outcome`, etc.) to decide
# that. `policyLabel` may be read and displayed as text (e.g. inside
# `Text(...)`, string interpolation, or passed straight through as a plain
# assignment) — that is fine and already done deliberately in several files.
#
# This is a lint/grep-based heuristic (per the ticket's own scope), not a
# full Swift AST parser. For each line that mentions `policyLabel` or
# `policy_label`, it flags the line as a violation if that line also looks
# like a conditional branch or string-equality check:
#
#   - contains `if `, `guard `, or `switch ` as a keyword
#   - contains a ternary shape (`?` ... `:`)
#   - contains `==` or `!=` compared against a string literal
#
# Full-line `//` / `///` comments are skipped — this codebase's source files
# intentionally document the anti-pattern in prose (e.g. "a naive
# `policyLabel == \"ASK\"` check would wrongly...") as part of proving the
# invariant, and those lines are not executable code.
#
# Scope is deliberately `Sources/` only, not `Tests/`. `Tests/` already
# contains its own mechanical, Swift-level assertions of this exact
# invariant (e.g. `CandidateDetailViewTests
# .testNoCodeBranchesOnPolicyLabelText`, which greps stripped source for
# `policyLabel ==` after removing comments) — and those test files
# deliberately embed the very string patterns this guard looks for, as
# quoted string-literal arguments to `.contains(...)`/`XCTAssertFalse(...)`,
# to prove their absence in the *scanned* source. A grep-based heuristic
# cannot tell "the string literal \"policyLabel ==\"\" passed to
# `.contains`" apart from "an actual `policyLabel == ...` comparison", so
# scanning `Tests/` produces irreducible false positives against sound,
# already-existing test code. `Sources/` is where the actual UI-gating
# decisions this ticket is guarding against would appear, so that is what's
# scanned mechanically here.
#
# HORO-1306 addition — the display-vocabulary file
# -------------------------------------------------
# HORO-1306 needed plain-language wording for AUTO_SAFE / ASK / PROTECTED /
# UNKNOWN_INCOMPLETE (its AC #2), which means a `switch` over those four
# strings has to exist somewhere. The heuristic above cannot tell that
# switch apart from a gating one, so the wording lives in
# Sources/GlomerisVocabulary.swift, whose lookups take an anonymous
# `token: String` and therefore never mention `policyLabel` at all. That
# decoupling is real rather than cosmetic — a String-to-copy table does not
# know whether the string it was handed came from the policy field, the
# pressure field or a test literal — but on its own it would let the
# original check be sidestepped by renaming a variable.
#
# So this script ADDS a second, stricter check rather than relaxing the
# first: the vocabulary file must be provably incapable of gating anything.
# It may not import SwiftUI or AppKit (so it cannot build a control or
# touch the filesystem), and it may not mention the fields and constructs
# that DO gate behaviour — `executable`, `requiresConfirmation`, `Button`,
# `.disabled(`, or an `execute` argument. A file that can only return
# strings cannot decide whether an action is authorized, whatever it
# switches on.
#
# Exit 0 = pass (no violations found). Exit 1 = fail (violation found, with
# file:line detail printed to stdout).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SEARCH_DIRS=(
  "macos/GlomerisMenuBar/Sources"
)

FIELD_PATTERN='policyLabel|policy_label'

violations=0

for dir in "${SEARCH_DIRS[@]}"; do
  abs_dir="${REPO_ROOT}/${dir}"
  if [[ ! -d "$abs_dir" ]]; then
    continue
  fi

  while IFS= read -r -d '' file; do
    rel_file="${file#"${REPO_ROOT}"/}"

    while IFS= read -r match; do
      lineno="${match%%:*}"
      content="${match#*:}"

      # Skip full-line comments (leading // or /// after optional whitespace)
      # — prose documenting the anti-pattern is not executable code.
      if [[ "$content" =~ ^[[:space:]]*// ]]; then
        continue
      fi

      is_violation=0
      reason=""

      if echo "$content" | grep -qE '\b(if|guard|switch)\b'; then
        is_violation=1
        reason="conditional keyword (if/guard/switch) on the same line"
      elif [[ "$content" =~ \?.*: ]]; then
        is_violation=1
        reason="ternary-shaped expression (?...: ) on the same line"
      elif [[ "$content" =~ (==|\!=)[[:space:]]*\"[^\"]*\" ]]; then
        is_violation=1
        reason="string-literal equality/inequality comparison on the same line"
      fi

      if [[ "$is_violation" -eq 1 ]]; then
        echo "VIOLATION: ${rel_file}:${lineno}: ${reason}"
        echo "    ${content}"
        violations=$((violations + 1))
      fi
    done < <(grep -nE "$FIELD_PATTERN" "$file" || true)
  done < <(find "$abs_dir" -name '*.swift' -print0)
done

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} line(s) that appear to branch on policyLabel/policy_label string content."
  echo "Button enablement, execution gating, and any authorization-adjacent decision in GlomerisMenuBar"
  echo "must read structured fields (executable, requiresConfirmation, outcome, ...) directly — never infer"
  echo "from policyLabel's text. See docs/internal/HORO-1068-no-policy-in-swift-review.md."
  exit 1
fi

echo "PASS: no policyLabel/policy_label branching detected under ${SEARCH_DIRS[*]}."

# ---------------------------------------------------------------------------
# HORO-1306: the display-vocabulary file must be incapable of gating anything.
# See this script's header for why this is an additional constraint and not a
# carve-out from the check above.
# ---------------------------------------------------------------------------

VOCABULARY_FILE="macos/GlomerisMenuBar/Sources/GlomerisVocabulary.swift"
abs_vocabulary="${REPO_ROOT}/${VOCABULARY_FILE}"

if [[ ! -f "$abs_vocabulary" ]]; then
  # Not an optional check: the file's existence is what makes the wording
  # centralized in the first place. If it has been deleted or moved, the
  # per-section wording it replaced has come back, and that is a regression
  # this script must report rather than silently skip.
  echo "FAIL: ${VOCABULARY_FILE} is missing."
  echo "HORO-1306 centralizes every CLI-token-to-wording mapping there so the copy cannot drift"
  echo "section by section. If it moved, update VOCABULARY_FILE in this script to match."
  exit 1
fi

# Each entry is "<extended regex>;;<why this would break the invariant>".
#
# The separator is `;;` and not `|` because some of these patterns contain an
# ERE alternation of their own; splitting on `|` cut the last pattern in half
# and handed grep `"(execute` — which failed with "parentheses not balanced"
# on stderr while the loop's `|| true` swallowed the non-zero exit, so that
# one pattern silently checked nothing and the script still printed PASS.
FORBIDDEN_IN_VOCABULARY=(
  '^[[:space:]]*import[[:space:]]+SwiftUI;;imports SwiftUI, so it could construct a control'
  '^[[:space:]]*import[[:space:]]+AppKit;;imports AppKit, so it could reach the filesystem or a control'
  '\bexecutable\b;;reads the field that gates the Clean button'
  '\brequiresConfirmation\b;;reads the field that gates the confirmation prompt'
  '\bButton\b;;constructs a control'
  '\.disabled\(;;gates a control'
  # The capability being excluded is "can spawn the CLI", not the word
  # "execute": `source: "execute"` is a legitimate audit-log token this file
  # has to have wording for (`src/monitor/persistence.rs`). So the patterns
  # below name the spawn machinery and the argument-array shape instead.
  '\bGlomerisClient\b;;can spawn the CLI'
  '\bProcess\(;;can spawn a subprocess'
  '\[[[:space:]]*"(execute|detect|explain|free|emergency)";;constructs a CLI argument array'
  '\bpolicyLabel\b;;names the policy field, so its switches would no longer be decoupled from it'
  '\bpolicy_label\b;;names the policy field, so its switches would no longer be decoupled from it'
)

vocabulary_violations=0

for entry in "${FORBIDDEN_IN_VOCABULARY[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"

  # A pattern grep cannot compile matches nothing, and the `|| true` below
  # would turn that into a silent PASS — which is exactly how the `|`
  # separator bug hid itself. Compile every pattern against an empty input
  # first: no output means it compiled, any output is grep's complaint.
  compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi

  # Comments are stripped first: this file documents at length WHY it must
  # not do these things, and naming `requiresConfirmation` in that prose is
  # the explanation, not the act.
  while IFS= read -r match; do
    echo "VIOLATION: ${VOCABULARY_FILE}:${match%%:*}: ${why}"
    echo "    ${match#*:}"
    vocabulary_violations=$((vocabulary_violations + 1))
  done < <(
    grep -nE "$pattern" "$abs_vocabulary" \
      | grep -vE '^[0-9]+:[[:space:]]*//' \
      || true
  )
done

if [[ "$vocabulary_violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: ${VOCABULARY_FILE} must be a pure token-to-wording table."
  echo "It is allowed to switch on an anonymous token string precisely because it cannot act on the"
  echo "result: no SwiftUI/AppKit import, no control, no enablement, no execute arguments, and no"
  echo "mention of the policy field. One of those properties no longer holds, so the switches in that"
  echo "file are no longer provably display-only. Move the new behavior into the view that owns the"
  echo "decision, reading executable/requiresConfirmation directly as CandidateDetailView does."
  exit 1
fi

echo "PASS: ${VOCABULARY_FILE} is display-only (no SwiftUI/AppKit, no control, no gating field)."
exit 0
