#!/usr/bin/env bash
#
# check-workspace-aggregation-has-no-authority.sh
#
# HORO-1511 mechanical CI guard: `crate::workspace` — the developer-workspace
# aggregation that groups discovered resources into git worktree families and
# says "this project consumes 30 GiB across nine worktrees" — must remain
# explanatory metadata with no part in deciding what may be deleted.
#
# The danger is specific, not abstract. The facts this module computes are the
# most persuasive things this repository can say about a directory: "that
# branch is already merged into origin/main", "nothing has touched this in
# four months", "this family is 30 GiB". Read as permission rather than as
# context, any one of them would let a whole worktree be removed on the
# strength of what its *siblings* look like — with the dirty, in-use or
# unpushed member swept along. So the rule is structural: the layers that
# decide and the layer that explains do not meet.
#
# Five checks, each one a separate way the boundary could be crossed. The first
# three are about the Rust module; the last two are about the macOS surface that
# renders it, because a group with no authority shown by a card that can act is
# the same defect one layer up:
#
#   1. No module under src/policy, src/executor, src/autopilot or src/actions
#      may reference `crate::workspace` at all. Those four are, in order:
#      what classifies a resource, what mutates the filesystem, what runs
#      unattended, and what defines the mutations themselves. A `use` line in
#      any of them is the failure this guard exists for — reachability is the
#      thing being denied, so the check does not try to judge how the import
#      is used.
#
#   2. The aggregate types may not carry anything that reads as permission.
#      A `WorkspaceFamily`/`WorkspaceWorktree`/`WorkspaceMember` with an
#      `executable` flag, an action id or an offered action would make AC 3
#      ("dirty/active/unique/unpushed work is never made executable because
#      its group looks stale") a matter of convention rather than of shape.
#      A caller that wants to know what may run has to go back to the
#      member's own candidate report, and the only way to guarantee that is
#      for the group to have nothing else to offer.
#
#   3. `crate::workspace` may not import the deciding and mutating layers
#      either. Check 1 stops them reaching in; this stops the module reaching
#      out and, say, re-classifying a member itself to decide a family is
#      spent. `crate::evidence` and `crate::reporting` are allowed — the
#      first is where the resources and their git state come from, the second
#      is what this metadata exists to be rendered by.
#
#   4. The macOS developer-projects card has nothing to act with: no `Button`,
#      no `GlomerisClient`, no tap target, no `.disabled(...)`, no action id.
#      This is the same argument as check 2 at the presentation layer. A card
#      headed "this project accounts for 30 GB" with a control on it is an
#      offer to clean a project, and a project is not a thing the executor has
#      ever been asked to reason about — the members are, one at a time,
#      through the policy class each one's own evidence earned.
#
#   5. That card still goes back to the member for its verdicts. Check 4 only
#      proves the surface cannot act; this asks whether it is still showing
#      whose decision it is, by requiring that the view model constructs a
#      `CandidateActionability` from a candidate and that the view renders the
#      not-permission sentence. A required substring is weak evidence about
#      behaviour — `WorkspaceFamilyRowViewModelTests` is what pins the
#      semantics — but it does catch the join being deleted, which is the
#      change that would turn the card into a summary with nothing under it.
#
# Exit 0 = pass. Exit 1 = fail, with file:line detail on stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

WORKSPACE_DIR="src/workspace"
abs_workspace="${REPO_ROOT}/${WORKSPACE_DIR}"

if [[ ! -d "$abs_workspace" ]]; then
  # Not skippable. The module's absence would mean the aggregation moved
  # somewhere this script no longer watches, which is the regression rather
  # than the absence of one.
  echo "FAIL: ${WORKSPACE_DIR}/ is missing."
  echo "HORO-1511's aggregation lives there precisely so it can be kept out of the deciding layers."
  echo "If it moved, update WORKSPACE_DIR in this script to match."
  exit 1
fi

# Comments are stripped before every check below: these files document at
# length why they must not do these things, and naming `executable` in that
# prose is the explanation, not the act. `^[0-9]+:[[:space:]]*//` drops
# whole-line `//` and `///` comments after grep -n has prefixed the number.
strip_comments() {
  grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' || true
}

violations=0

# ---------------------------------------------------------------------------
# Check 1: the deciding and mutating layers cannot reach the aggregation.
# ---------------------------------------------------------------------------

DECIDING_PATHS=(
  "src/policy"
  "src/executor"
  "src/autopilot"
  "src/actions"
)

for rel_dir in "${DECIDING_PATHS[@]}"; do
  abs_dir="${REPO_ROOT}/${rel_dir}"
  if [[ ! -d "$abs_dir" ]]; then
    echo "FAIL: ${rel_dir}/ is missing — this script's list of deciding layers is out of date."
    exit 1
  fi

  while IFS= read -r -d '' file; do
    rel_file="${file#"${REPO_ROOT}"/}"
    while IFS= read -r match; do
      echo "VIOLATION: ${rel_file}:${match%%:*}: a deciding layer references crate::workspace"
      echo "    ${match#*:}"
      violations=$((violations + 1))
    done < <(grep -nE '\b(crate|super)::workspace\b' "$file" | strip_comments)
  done < <(find "$abs_dir" -name '*.rs' -print0)
done

# `src/policy.rs`-style single-file modules would sit next to the directory
# rather than inside it, so check those too if they exist.
for rel_dir in "${DECIDING_PATHS[@]}"; do
  single_file="${REPO_ROOT}/${rel_dir}.rs"
  [[ -f "$single_file" ]] || continue
  rel_file="${rel_dir}.rs"
  while IFS= read -r match; do
    echo "VIOLATION: ${rel_file}:${match%%:*}: a deciding layer references crate::workspace"
    echo "    ${match#*:}"
    violations=$((violations + 1))
  done < <(grep -nE '\b(crate|super)::workspace\b' "$single_file" | strip_comments)
done

# ---------------------------------------------------------------------------
# Check 2: the aggregate types carry no permission-shaped field.
#
# Each entry is "<extended regex>;;<why this would break the invariant>".
# The separator is `;;` rather than `|` because these patterns contain ERE
# alternations of their own.
# ---------------------------------------------------------------------------

FORBIDDEN_IN_WORKSPACE=(
  '\bexecutable\b;;names the field that gates execution'
  '\brequires_confirmation\b;;names the field that gates the confirmation prompt'
  '\baction_id\b;;names an executable action, which a group must not offer'
  '\boffered_action\b;;offers an action, which a group must not do'
  '\bcrate::executor\b;;imports the layer that mutates the filesystem'
  '\bcrate::autopilot\b;;imports the layer that runs unattended'
  '\bcrate::actions\b;;imports the layer that defines mutations'
  '\bclassify\b;;calls the policy classifier, so it would be deciding'
  '\bstd::fs::remove;;removes files'
  '\bPolicyClass\b;;reads the policy class, whose separation from this module is the point'
)

for entry in "${FORBIDDEN_IN_WORKSPACE[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"

  # A pattern grep cannot compile matches nothing, and the `|| true` inside
  # strip_comments would turn that into a silent PASS. Compile each pattern
  # against an empty input first: no output means it compiled.
  compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi

  while IFS= read -r -d '' file; do
    rel_file="${file#"${REPO_ROOT}"/}"
    # `#[cfg(test)]` code is excluded from this check by taking only the
    # lines before the test module: the group tests legitimately construct
    # `PolicyClass::Protected` candidates to prove that a stale-looking
    # family does not relabel them, which is the invariant rather than a
    # breach of it. `awk` stops at the test-module marker.
    production="$(awk '/^#\[cfg\(test\)\]/ { exit } { print NR ":" $0 }' "$file")"
    while IFS= read -r match; do
      [[ -n "$match" ]] || continue
      echo "VIOLATION: ${rel_file}:${match%%:*}: ${why}"
      echo "    ${match#*:}"
      violations=$((violations + 1))
    done < <(printf '%s\n' "$production" | grep -E ":.*${pattern}" | strip_comments)
  done < <(find "$abs_workspace" -name '*.rs' -print0)
done

# ---------------------------------------------------------------------------
# Check 4: the macOS surface that renders the aggregation cannot act.
# ---------------------------------------------------------------------------

SWIFT_SURFACE=(
  "macos/GlomerisMenuBar/Sources/WorkspaceFamilyRowViewModel.swift"
  "macos/GlomerisMenuBar/Sources/WorkspaceFamiliesSectionView.swift"
)

for rel_file in "${SWIFT_SURFACE[@]}"; do
  if [[ ! -f "${REPO_ROOT}/${rel_file}" ]]; then
    # Same reasoning as the missing-module case above: if the card moved, the
    # guard has to move with it, and a silent skip would leave the strongest
    # claim in the product — "this project accounts for 30 GB" — unwatched.
    echo "FAIL: ${rel_file} is missing."
    echo "HORO-1511's developer-projects card lives there. If it moved, update SWIFT_SURFACE."
    exit 1
  fi
done

FORBIDDEN_IN_SWIFT=(
  '\bButton\b;;offers a control on a surface whose subject is a whole project'
  '\bGlomerisClient\b;;could run the CLI from a surface that only explains'
  '\bclient\b;;holds something to run, which this surface has no use for'
  '\bProcess\b;;spawns a process'
  '\bexecute\b;;names execution on a surface that must not reach it'
  '(action_id|actionId);;constructs an action id, which only a detector may do'
  '\.disabled\(;;gates a control, so there is a control to gate'
  'onTapGesture;;makes something tappable, which is a control by another name'
)

for entry in "${FORBIDDEN_IN_SWIFT[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"

  compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi

  for rel_file in "${SWIFT_SURFACE[@]}"; do
    while IFS= read -r match; do
      [[ -n "$match" ]] || continue
      echo "VIOLATION: ${rel_file}:${match%%:*}: ${why}"
      echo "    ${match#*:}"
      violations=$((violations + 1))
    done < <(grep -nE "$pattern" "${REPO_ROOT}/${rel_file}" | strip_comments)
  done
done

# ---------------------------------------------------------------------------
# Check 5: that surface still reads each member's own verdict.
# ---------------------------------------------------------------------------

REQUIRED_IN_SWIFT=(
  'macos/GlomerisMenuBar/Sources/WorkspaceFamilyRowViewModel.swift;;CandidateActionability(;;the member rows must build their verdict from the candidate, not from the family'
  'macos/GlomerisMenuBar/Sources/WorkspaceFamiliesSectionView.swift;;notAuthoritySentence;;the card must say that a project total is not permission'
  'macos/GlomerisMenuBar/Sources/WorkspaceFamiliesSectionView.swift;;actionability.sentence;;the card must show each member what Glomeris will actually do about it'
)

for entry in "${REQUIRED_IN_SWIFT[@]}"; do
  rel_file="${entry%%;;*}"
  rest="${entry#*;;}"
  needle="${rest%%;;*}"
  why="${rest#*;;}"

  if ! grep -nF "$needle" "${REPO_ROOT}/${rel_file}" | strip_comments | grep -q .; then
    echo "VIOLATION: ${rel_file}: '${needle}' is gone — ${why}"
    violations=$((violations + 1))
  fi
done

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} line(s) breaking HORO-1511's no-authority boundary."
  echo "crate::workspace summarizes worktree families so a person can see where their disk went."
  echo "It must not be reachable from policy/executor/autopilot/actions, and its types must not carry"
  echo "anything that reads as permission — 'this family consumes 30 GiB' is a reason to look, never a"
  echo "reason to delete, and a member's own candidate report stays the only place that says what may"
  echo "run. See the header of src/workspace/mod.rs."
  exit 1
fi

echo "PASS: crate::workspace is unreachable from ${DECIDING_PATHS[*]} and carries no permission-shaped field."
echo "PASS: the developer-projects card has nothing to act with and still reads each member's own verdict."
exit 0
