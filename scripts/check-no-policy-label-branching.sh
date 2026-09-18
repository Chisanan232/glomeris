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
exit 0
