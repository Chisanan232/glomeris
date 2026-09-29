#!/usr/bin/env bash
#
# check-workflow-history-labels-no-one.sh
#
# HORO-1547 mechanical CI guard, AC 7: the workflow baseline describes how a
# machine has been used. It never describes the person using it.
#
# The temptation is specific and it is a friendly one. `parallel_multi_worktree`
# is the shape of somebody juggling six checkouts, and a sentence calling that
# "advanced" reads as a compliment — so it gets written, and in the same stroke
# `serial_single_checkout` becomes a judgement and an `unknown` baseline becomes
# a verdict on somebody the product has barely observed. From there the
# vocabulary is a persona, a persona is a claim about competence, and a claim
# about competence is the kind of thing a recommendation starts leaning on.
#
# `crate::workspace::history::classify` cannot break this on its own: its
# vocabulary is four layout names and `unknown`, and there is no word for a
# person in it. Prose can, and this checks the prose that ships.
#
#   1. None of the banned words appears in the shipping part of the files that
#      define the baseline or write sentences about it. "Shipping part" is
#      everything before the first `#[cfg(test)]`, with whole-line comments
#      stripped — because these files argue at length about why they must not
#      say "advanced", and the test module holds the banned list as its own
#      needles. Both are the rule being explained and asserted, not broken.
#
#   2. The shipping part of `src/cli/workflow_profile.rs` still holds the mode
#      vocabulary it is supposed to be writing sentences about. Without this,
#      an empty file, a renamed module or a `#[cfg(test)]` that migrated to the
#      top of the file would all pass check 1 by scanning nothing.
#
# Deliberately out of scope, so the gap is stated rather than implied:
#
#   * `book/src/cli_reference.md` explains this rule to a reader and names the
#     banned words in doing so. Documentation of a prohibition cannot be held
#     to the prohibition.
#   * The rendered sentences are asserted in
#     `src/cli/workflow_profile.rs`'s `no_rendering_labels_the_person_operating_the_machine`,
#     which renders every store state, every mode description and every
#     admission and rejects the same list at runtime. This script covers what
#     a test's fixtures might miss; that test covers what a word list cannot
#     see, which is a sentence assembled from parts.
#   * The macOS surface has no baseline wording yet. HORO-1550 adds it, and the
#     Swift half of this rule belongs with it.
#
# Exit 0 = pass. Exit 1 = fail, with file:line detail on stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The files that define the baseline or write sentences about it. Each one must
# exist: a missing file means the feature moved somewhere this script no longer
# watches, which is the regression rather than the absence of one.
FILES=(
  "src/cli/workflow_profile.rs"
  "src/workspace/history/mod.rs"
  "src/workspace/history/alias.rs"
  "src/workspace/history/classify.rs"
  "src/workspace/history/observation.rs"
  "src/workspace/history/store.rs"
  "src/workspace/graph.rs"
  "src/reporting/dto.rs"
)

# Words that name a person rather than a layout. The campaign brief bans the
# first five by name; the rest are the synonyms somebody reaches for once the
# obvious ones are refused, which is the failure mode a five-word list has.
BANNED='advanced|beginner|novice|expert|proficient|sophisticated|competent|incompetent|skilled|unskilled|amateur|professional|guru|ninja|rockstar|seasoned|inexperienced|casual user|power user|sloppy|undisciplined|disciplined|savvy|clueless'

# Everything before the first `#[cfg(test)]`, numbered, with whole-line
# comments dropped. `grep -n` first so the numbers are the real ones.
shipping_part() {
  grep -n '' "$1" | awk -F: '$2 ~ /^[[:space:]]*#\[cfg\(test\)\]/ { exit } { print }' |
    grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' || true
}

violations=0

for relative in "${FILES[@]}"; do
  file="${REPO_ROOT}/${relative}"
  if [[ ! -f "$file" ]]; then
    echo "FAIL: ${relative} is missing."
    echo "The workflow baseline's wording rule is checked against that file; if it moved, update FILES in this script."
    violations=$((violations + 1))
    continue
  fi

  hits="$(shipping_part "$file" | grep -inE "\\b(${BANNED})\\b" || true)"
  if [[ -n "$hits" ]]; then
    echo "FAIL: ${relative} labels the person operating the machine:"
    # The line numbers above are the file's own, because grep -n ran first and
    # the second grep's -n counts the pipeline, so only the first field is
    # trustworthy — printed as-is rather than re-prefixed.
    while IFS= read -r line; do
      echo "  ${relative}:${line#*:}"
    done <<<"$hits"
    echo "  The baseline reports layouts and counts. It does not rate the developer."
    violations=$((violations + 1))
  fi
done

# ---------------------------------------------------------------------------
# Check 2: the anti-vacuity half.
# ---------------------------------------------------------------------------

prose="${REPO_ROOT}/src/cli/workflow_profile.rs"
if [[ -f "$prose" ]]; then
  scanned="$(shipping_part "$prose")"
  for needle in serial_single_checkout parallel_multi_worktree; do
    if ! grep -q "$needle" <<<"$scanned"; then
      echo "FAIL: the shipping part of src/cli/workflow_profile.rs does not mention ${needle}."
      echo "Check 1 above therefore scanned nothing that writes about the baseline, and passed for that reason."
      violations=$((violations + 1))
    fi
  done
fi

if ((violations > 0)); then
  echo
  echo "FAIL: ${violations} check(s) failed."
  exit 1
fi

echo "PASS: the workflow baseline's shipping wording names no skill, competence or persona."
echo "      ${#FILES[@]} files scanned; $(printf '%s\n' "$BANNED" | tr '|' '\n' | wc -l | tr -d ' ') words rejected."
exit 0
