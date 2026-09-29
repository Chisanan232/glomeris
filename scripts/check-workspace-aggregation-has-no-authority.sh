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
# HORO-1542 extends the same boundary to two new modules without changing its
# shape. `src/workspace/graph.rs` is more evidence inside the zone this script
# already watches, so checks 1–3 cover it unchanged. `src/planner` is new: it
# is the one place a resource alias and an action id are allowed to meet,
# because the model has to be told what is on offer and `crate::workspace` may
# not say. Being allowed to name an action id makes it exactly as dangerous as
# the aggregation was, so check 7 keeps the deciding layers out of it too, and
# check 6 holds its egress half to a stricter rule than any other module here:
# it may not name a path type at all.
#
# HORO-1546 extends check 6's forbidden list rather than adding an eighth
# check. The optional GitHub and Jira providers live under `src/workspace`, so
# checks 1-3 already hold them; what is new is that their domain types carry
# identity — a repository subject, a branch, an issue key — and the model-facing
# DTO must keep projecting those to bounded tokens instead of being able to name
# them. AC 2, the other half of that ticket, is a separate script:
# `scripts/check-external-context-is-read-only.sh`.
#
# HORO-1547 narrows check 3 in one place and exempts one file in another, both
# because `src/workspace` gained a second `classify`.
# `crate::workspace::history::classify` reads stored observations into a
# workflow mode — the thing this guard exists to keep *away* from authority
# rather than an exercise of it — so the pattern is now the qualified
# `policy::classify`, with its braced and glob import forms beside it, instead
# of the bare word. The bare word was an adequate proxy only while one function
# in the crate owned the verb, and a proxy that has started matching the module
# it protects would be read as noise and then deleted.
#
# The exempt file is `src/workspace/history/fixtures.rs`, a whole file gated by
# `#[cfg(test)] mod fixtures;` in its sibling module root: it builds throwaway
# git repositories under `$TMPDIR` and removes them again, which is the
# `std::fs::remove` pattern in code that never ships. The in-file `#[cfg(test)]`
# cut below cannot see a marker that lives in another file, so the loop asks the
# module root whether the file it is about to scan is test-only.
#
# Seven checks, each one a separate way the boundary could be crossed. The
# first three are about the aggregation module, the next two about the macOS
# surface that renders it — because a group with no authority shown by a card
# that can act is the same defect one layer up — and the last two about the
# planner:
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
#   6. `src/planner/dto.rs` — the complete set of types that may be serialized
#      to an LLM provider — may not name `PathBuf`, `Path`, `std::path`,
#      `ResourceId` or `Evidence`. Every other guard on egress in this
#      repository tests an *output*, which is only ever as good as the fixture
#      that produced it: a field populated from a path on a code path no test
#      exercises leaks in production and passes CI. This check makes the first
#      rule of the campaign a property of the module instead. A module that
#      cannot name a path cannot serialize one, whatever anybody adds to it
#      later and whatever the tests happen to cover.
#
#   7. No module under src/policy, src/executor, src/autopilot or src/actions
#      may reference `crate::planner`. Check 1's argument, unchanged, for the
#      module that holds the alias table: resolving an alias yields a resource
#      to *re-evaluate*, and a deciding layer that could resolve one itself
#      would be taking a model's word for which resource it meant. The
#      direction is one-way — planner → {workspace, actionability, actions} —
#      and nothing that decides may look back up it.
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

# True when `$1` is a whole file that only exists under `cfg(test)`, which its
# own `mod` declaration in the sibling module root is the evidence for.
#
# Deliberately narrow, and it fails towards scanning: a module root that is
# missing, unreadable, or declares the file without the attribute leaves the
# file checked. So the way to be skipped is to be declared test-only, which is
# also the way to not ship.
test_only_module_file() {
  local dir base
  dir="$(dirname "$1")"
  base="$(basename "$1" .rs)"
  [[ -f "${dir}/mod.rs" ]] || return 1
  grep -B1 -E "\bmod ${base};" "${dir}/mod.rs" 2>/dev/null |
    grep -qE '^#\[cfg\(test\)\]'
}

violations=0

# ---------------------------------------------------------------------------
# Checks 1 and 7: the deciding and mutating layers cannot reach the
# aggregation, nor the planner that projects it.
#
# One loop over both targets rather than two loops: the argument is identical
# and so is the single-file handling, and a copy would be a second place to
# forget when a third explanatory module is added.
# ---------------------------------------------------------------------------

DECIDING_PATHS=(
  "src/policy"
  "src/executor"
  "src/autopilot"
  "src/actions"
)

# "<module>;;<what reaching it would let a deciding layer do>"
UNREACHABLE_MODULES=(
  'workspace;;read a group fact, which would let sibling state decide a member'
  'planner;;resolve a model-supplied alias, which would be taking the model'"'"'s word for which resource it meant'
)

for rel_dir in "${DECIDING_PATHS[@]}"; do
  abs_dir="${REPO_ROOT}/${rel_dir}"
  if [[ ! -d "$abs_dir" ]]; then
    echo "FAIL: ${rel_dir}/ is missing — this script's list of deciding layers is out of date."
    exit 1
  fi
done

for entry in "${UNREACHABLE_MODULES[@]}"; do
  module="${entry%%;;*}"
  why="${entry#*;;}"

  if [[ ! -d "${REPO_ROOT}/src/${module}" ]]; then
    echo "FAIL: src/${module}/ is missing — UNREACHABLE_MODULES is out of date."
    echo "Its absence would mean the module moved somewhere this script no longer watches."
    exit 1
  fi

  for rel_dir in "${DECIDING_PATHS[@]}"; do
    while IFS= read -r -d '' file; do
      rel_file="${file#"${REPO_ROOT}"/}"
      while IFS= read -r match; do
        echo "VIOLATION: ${rel_file}:${match%%:*}: a deciding layer references crate::${module} — it could ${why}"
        echo "    ${match#*:}"
        violations=$((violations + 1))
      done < <(grep -nE "\\b(crate|super)::${module}\\b" "$file" | strip_comments)
    done < <(find "${REPO_ROOT}/${rel_dir}" -name '*.rs' -print0)

    # `src/policy.rs`-style single-file modules would sit next to the
    # directory rather than inside it, so check those too if they exist.
    single_file="${REPO_ROOT}/${rel_dir}.rs"
    [[ -f "$single_file" ]] || continue
    while IFS= read -r match; do
      echo "VIOLATION: ${rel_dir}.rs:${match%%:*}: a deciding layer references crate::${module} — it could ${why}"
      echo "    ${match#*:}"
      violations=$((violations + 1))
    done < <(grep -nE "\\b(crate|super)::${module}\\b" "$single_file" | strip_comments)
  done
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
  '\bpolicy::classify\b;;calls the policy classifier, so it would be deciding'
  '\bpolicy::\{[^}]*\bclassify\b;;imports the policy classifier, so it would be deciding'
  '\bpolicy::\*;;glob-imports the deciding layer, so any of it could be called unqualified'
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
    # A file whose own module declaration gates it on `cfg(test)` ships
    # nothing, and the in-file cut below cannot see a marker in another file.
    test_only_module_file "$file" && continue
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

# ---------------------------------------------------------------------------
# Check 6: the model-facing DTO cannot name a path.
# ---------------------------------------------------------------------------

DTO_FILE="src/planner/dto.rs"
abs_dto="${REPO_ROOT}/${DTO_FILE}"

if [[ ! -f "$abs_dto" ]]; then
  # Not skippable, for the same reason as the missing-module cases above. If
  # the model-facing types moved, they moved out from under the one check that
  # constrains them by construction rather than by fixture.
  echo "FAIL: ${DTO_FILE} is missing."
  echo "It holds every type that may be serialized to an LLM provider. If it moved, update DTO_FILE."
  exit 1
fi

FORBIDDEN_IN_DTO=(
  '\bPathBuf\b;;names an owned path type, and a field of that type would serialize an absolute path'
  '\bPath\b;;names a path type'
  'std::path;;imports the path module'
  '\bResourceId\b;;names the real resource identity, which embeds a path — the opaque alias exists so this type never has to'
  '\bEvidence\b;;names the local evidence type, whose fields are the thing being withheld'
  # HORO-1546 adds the external-context domain to the same list, for the same
  # reason and one step further. Campaign section 5 says adding a local domain
  # field must not automatically expand provider egress — and the way that
  # happens is not a deliberate decision, it is a `#[derive(Serialize)]` on a
  # domain type that somebody later adds a field to. `ExternalContext` holds
  # the repository subject, the branch it was correlated on and the issue key
  # that was looked up: every one of them a description of what this machine's
  # owner is working on. The projection in `src/planner/project.rs` reads those
  # and emits three bounded tokens. A DTO that could *name* the domain type
  # could hold one, and then the projection would no longer be the only path.
  '\bExternalContext\b;;names the local external-context type, which carries the repository subject, the branch and the issue key'
  '\bExternalFact\b;;names the local fact type, which carries the correlated identity alongside the state'
  '\bPullRequestState\b;;names the domain enum rather than projecting it to a token, so a variant that later gained a payload would serialize it'
  '\bTaskState\b;;names the domain enum rather than projecting it to a token, and its Other variant carries the tracker'"'"'s own status name'
  '\bTaskKey\b;;names the issue key type, which is the one identifier a Jira correlation must not send'
  '\bRepositoryBranchSubject\b;;names the correlation subject, which is host, owner, repository and branch together'
)

for entry in "${FORBIDDEN_IN_DTO[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"

  compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi

  while IFS= read -r match; do
    [[ -n "$match" ]] || continue
    echo "VIOLATION: ${DTO_FILE}:${match%%:*}: ${why}"
    echo "    ${match#*:}"
    violations=$((violations + 1))
  done < <(grep -nE "$pattern" "$abs_dto" | strip_comments)
done

# Non-vacuity. Every check above passes when its file is empty, and this one
# would pass if `dto.rs` were reduced to a stub or if `strip_comments` started
# eating code. Require the module to still hold the view type whose key set
# `tests/planner_model_egress_contract.rs` pins, and the `Reported` wrapper
# that keeps an unavailable probe from looking like a zero.
for needle in 'pub struct ModelGraphView' 'pub struct Reported' 'pub struct ExternalFactView'; do
  if ! grep -nF "$needle" "$abs_dto" | strip_comments | grep -q .; then
    echo "VIOLATION: ${DTO_FILE}: '${needle}' is gone — the checks above would now pass over a stub."
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

echo "PASS: crate::workspace and crate::planner are unreachable from ${DECIDING_PATHS[*]}, and crate::workspace carries no permission-shaped field."
echo "PASS: the developer-projects card has nothing to act with and still reads each member's own verdict."
echo "PASS: ${DTO_FILE} cannot name a path type or an external-context domain type, so the model-facing types cannot serialize one."
exit 0
