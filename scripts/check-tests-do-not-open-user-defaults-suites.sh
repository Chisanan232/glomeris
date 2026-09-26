#!/usr/bin/env bash
#
# check-tests-do-not-open-user-defaults-suites.sh
#
# HORO-1486, as a mechanical check rather than a convention.
#
# A test that calls UserDefaults(suiteName:) leaves a preference file on the
# machine that ran it, and there is no teardown that removes it: emptying the
# domain does not delete the file, and unlinking the file from inside the test
# process does not stick because cfprefsd rewrites it after the process
# disconnects (measured: 40 of 40 reappeared). Before this ticket eight sites in
# this target created a uniquely-named suite each, and 1,892 plists had
# accumulated in one developer's preferences directory.
#
# So the rule is not "remember to tear the suite down", which five of those
# eight sites already did. The rule is that the test target opens no named
# suite at all, except the one documented place that has to.
#
# WHAT IS CHECKED, AND WHY EACH ONE
# ---------------------------------
# 1. The helper exists and offers in-memory storage. Without it this check
#    would be a prohibition with nothing to point at.
# 2. No file under Tests/ except the helper constructs UserDefaults(suiteName:).
#    Comments are stripped first: the helper's own doc comment and
#    BundleIdentityTests both name the call in prose to explain why it is not
#    used, and naming it there is the explanation, not the act.
# 3. The helper's single real suite has a fixed name — no UUID() in it. A unique
#    name is what made the accumulation unbounded; a fixed one occupies one file
#    however many times the suite runs.
# 4. That real suite is opened in exactly one place. Two would be two files, and
#    the second would arrive without the argument the first one had to make.
#
# Exit 0 = every property holds. Exit 1 = a violation, printed with file:line.
# An extraction that comes back empty is a failure too: each check asserts it
# found something before asserting what it found.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TESTS_DIR="macos/GlomerisMenuBar/Tests"
HELPER_FILE="${TESTS_DIR}/TestUserDefaults.swift"

abs_tests="${REPO_ROOT}/${TESTS_DIR}"
abs_helper="${REPO_ROOT}/${HELPER_FILE}"

violations=0

fail() {
  echo "VIOLATION: $1"
  violations=$((violations + 1))
}

if [[ ! -d "$abs_tests" ]]; then
  echo "FAIL: ${TESTS_DIR} is missing, so nothing was scanned."
  echo "If the test target moved, update this script — do not delete the check."
  exit 1
fi

if [[ ! -f "$abs_helper" ]]; then
  echo "FAIL: ${HELPER_FILE} is missing."
  echo "It is the only sanctioned way for a test in this target to obtain a UserDefaults."
  echo "If it was renamed, update this script in the same commit."
  exit 1
fi

# Strip whole-line comments before scanning, keeping original line numbers so a
# violation can be pointed at.
strip_comments() {
  grep -nv -E '^[[:space:]]*(//|\*|/\*)' "$1" || true
}

# ---------------------------------------------------------------------------
# 1. The helper offers in-memory storage.
# ---------------------------------------------------------------------------
helper_body="$(strip_comments "$abs_helper")"

if ! grep -qE 'class InMemoryUserDefaults: UserDefaults' <<<"$helper_body"; then
  echo "FAIL: ${HELPER_FILE} declares no InMemoryUserDefaults subclass."
  echo "Every site this script forbids has to have somewhere else to go, and that is it."
  exit 1
fi

if ! grep -qE 'static func inMemory\(' <<<"$helper_body"; then
  echo "FAIL: ${HELPER_FILE} declares no TestUserDefaults.inMemory()."
  echo "That is the entry point the migrated call sites use."
  exit 1
fi

# ---------------------------------------------------------------------------
# 2. No test file but the helper constructs a named suite.
# ---------------------------------------------------------------------------
scanned=0
while IFS= read -r file; do
  scanned=$((scanned + 1))
  [[ "$file" == "$abs_helper" ]] && continue

  while IFS= read -r match; do
    line="${match%%:*}"
    text="${match#*:}"
    fail "${file#"${REPO_ROOT}"/}:${line}: constructs a UserDefaults suite directly. It will leave a preference file behind that no teardown can remove — use TestUserDefaults.inMemory(). See ${HELPER_FILE} for why emptying the domain is not enough: ${text}"
  done < <(grep -E 'UserDefaults\([[:space:]]*$|UserDefaults\(suiteName' <<<"$(strip_comments "$file")" || true)
done < <(find "$abs_tests" -name '*.swift' -type f | sort)

if [[ "$scanned" -eq 0 ]]; then
  echo "FAIL: found no Swift files under ${TESTS_DIR}."
  echo "A scan that examines nothing passes for the wrong reason."
  exit 1
fi

# ---------------------------------------------------------------------------
# 3. The one real suite has a fixed name.
# ---------------------------------------------------------------------------
real_suite_line="$(grep -E 'static let realSuiteName' <<<"$helper_body" || true)"

if [[ -z "$real_suite_line" ]]; then
  echo "FAIL: ${HELPER_FILE} declares no realSuiteName."
  echo "The one file this target does leave behind must be named in source, not only"
  echo "observable on a machine that has run the suite."
  exit 1
fi

if grep -qF 'UUID()' <<<"$real_suite_line"; then
  fail "${HELPER_FILE}: realSuiteName contains UUID(), so it names a different domain on every run and the files accumulate without bound. That is the defect HORO-1486 fixed: ${real_suite_line#*:}"
fi

# ---------------------------------------------------------------------------
# 4. The real suite is opened in exactly one place.
# ---------------------------------------------------------------------------
real_opens="$(grep -cE 'UserDefaults\(suiteName' <<<"$helper_body" || true)"

if [[ "$real_opens" -ne 1 ]]; then
  fail "${HELPER_FILE}: opens ${real_opens} named suite(s); exactly 1 is allowed. Each one is a file on every developer's machine, and the second would arrive without the argument the first had to make."
fi

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} violation(s): the Swift test target would leave preference"
  echo "files behind that no teardown can remove."
  echo "Use TestUserDefaults.inMemory(). See ${HELPER_FILE}."
  exit 1
fi

echo "PASS: ${scanned} test file(s) scanned; none opens a UserDefaults suite except ${HELPER_FILE}, whose one real suite is fixed-named."
exit 0
