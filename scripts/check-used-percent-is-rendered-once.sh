#!/usr/bin/env bash
#
# check-used-percent-is-rendered-once.sh
#
# HORO-1506 mechanical CI guard: a percentage on the used-disk axis is rendered
# by one rule per language, and nowhere else.
#
# ## What went wrong, and why a test could not keep it from recurring
#
# Three figures in this product sit on the same axis — where the disk is now,
# the alert threshold the user configured, and the recovery goal — and two of
# them are compared against each other on every poll. The comparison is always
# against the unrounded `f64`. The *renderings* were local: eleven sites in Rust
# and seven in Swift, each with its own `{:.1}%` / `{:.2}%` / `%.1f%% used` /
# `Int(fraction * 100)`.
#
# Round to nearest disagrees with `>=` in the band just under each tenth:
#
#     measured used_percent      = 89.96
#     displayed (round to 1 dp)  = "90.0% used"
#     alert threshold            = 90.0% used
#     decision                   = 89.96 >= 90.0  ->  no notification
#
# The screen states the threshold and nothing happens. The same arithmetic in
# the other direction hit the configured figures, which were rendered `{:.2}`:
# a stored threshold of 87.456 displayed as `87.46% used`, above the boundary
# the daemon notifies at.
#
# Every one of those sites now calls a shared renderer — `reporting::used_percent`
# in Rust, `GlomerisUsedPercent` in Swift, pinned to each other by
# `tests/fixtures/used_percent_golden.tsv`. Both rules are thoroughly tested. But
# a test can only assert about a function that is called: the eighteenth site,
# added next quarter with a local `format!("{pct:.1}%")`, would be covered by
# nothing and would look exactly like the seventeen that were wrong. That is
# what this script is for. It is the same reasoning as
# `check-no-policy-label-branching.sh`: the invariant is "no *other* code does
# this", which is a property of the tree rather than of any one function.
#
# ## What it checks
#
# 1. Rust, under src/ excluding the canonical module: no format specifier that
#    renders a number to a fixed number of decimal places immediately followed
#    by a percent sign — `{x:.1}%`, `{:.2}%`.
# 2. Rust: no function returning a `String` with `percent` in its name outside
#    the canonical module. This is the shape both of the deleted private
#    `trim_percent` copies had, and renaming one is how the first check would
#    otherwise be sidestepped.
# 3. Swift, under Sources/ excluding the canonical file: no line that formats to
#    a fixed number of decimal places *and* mentions a percent. `GlomerisByteFormat`
#    legitimately uses `%.1f` for byte counts, so the percent mention is what
#    discriminates a percentage rendering from a size one.
# 4. Swift: no `Int(... * 100)`. That is the capacity bar's original defect — a
#    second truncation to a whole number, on an axis it never named, which was
#    the only thing a screen-reader user heard from the bar.
# 5. Both canonical modules and the golden fixture still exist, so the checks
#    above cannot pass because the rule they protect was deleted.
#
# Full-line comments are stripped before scanning. This repo documents its
# defects in prose — several of the strings above appear verbatim in the header
# of the very module that fixed them — and that prose is not executable code.
#
# `--self-test` asserts each pattern matches a known-violating line and spares a
# known-good one, because a pattern that matches nothing at all would otherwise
# report PASS forever.
#
# Exit 0 = pass. Exit 1 = fail, with file:line detail on stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

RUST_CANONICAL="src/reporting/used_percent.rs"
SWIFT_CANONICAL="macos/GlomerisMenuBar/Sources/GlomerisUsedPercent.swift"
GOLDEN_FIXTURE="tests/fixtures/used_percent_golden.tsv"

# A fixed-precision format specifier with a percent sign welded to it.
RUST_LOCAL_RENDER='\{[^}]*:\.[0-9]+\}%'
# A `String`-returning function that names the axis — the shape of the private
# helpers this rule replaced.
RUST_LOCAL_HELPER='fn [a-z_]*percent[a-z_]*\([^)]*\)[[:space:]]*->[[:space:]]*String'
# Swift's equivalent of the first: `%.1f` on a line that also mentions a percent.
SWIFT_PRECISION='%\.[0-9]+f'
SWIFT_PERCENT='%%|[Pp]ercent'
# The capacity bar's original whole-number truncation.
SWIFT_WHOLE_PERCENT='Int\([^)]*\* *100'

# Strip full-line comments: `//`, `///` and Rust's `//!`.
strip_comments() {
  grep -vE '^[0-9]+:[[:space:]]*(//|///|//!)' || true
}

self_test() {
  local failures=0

  check_pattern() {
    local label="$1" pattern="$2" violating="$3" innocent="$4"

    if ! printf '%s\n' "$violating" | grep -qE "$pattern"; then
      echo "SELF-TEST FAIL: ${label} did not match a line it must reject:"
      echo "    ${violating}"
      failures=$((failures + 1))
    fi
    if printf '%s\n' "$innocent" | grep -qE "$pattern"; then
      echo "SELF-TEST FAIL: ${label} matched a line it must allow:"
      echo "    ${innocent}"
      failures=$((failures + 1))
    fi
  }

  check_pattern "RUST_LOCAL_RENDER" "$RUST_LOCAL_RENDER" \
    '    println!("used: {used:.1}%");' \
    '    println!("{:.1} GB", gigabytes);'
  check_pattern "RUST_LOCAL_HELPER" "$RUST_LOCAL_HELPER" \
    'fn trim_percent(value: f64) -> String {' \
    '    fn the_rendered_file_says_which_axis_its_percentages_are_on() {'
  check_pattern "SWIFT_PRECISION+SWIFT_PERCENT" \
    "(${SWIFT_PRECISION}).*(${SWIFT_PERCENT})" \
    '        return String(format: "%.1f%% used", value)' \
    '        return String(format: "%.1f", locale: nil, value) + " \(units[i])"'
  check_pattern "SWIFT_WHOLE_PERCENT" "$SWIFT_WHOLE_PERCENT" \
    '            .accessibilityValue("\(Int(fraction * 100)) percent")' \
    '            .accessibilityValue(capacity.spokenBarValue)'

  # The innocent Swift line above is the real one from GlomerisByteFormat, so a
  # future widening of SWIFT_PERCENT that swallowed byte formatting would be
  # caught here rather than by a confusing failure in CI.

  if [[ "$failures" -gt 0 ]]; then
    echo ""
    echo "FAIL: ${failures} self-test assertion(s) failed. The scan above this line cannot be"
    echo "trusted — a pattern that matches nothing reports PASS over a violating tree."
    exit 1
  fi

  echo "PASS: all 4 patterns reject a violating line and allow an innocent one."
  exit 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
fi

violations=0

report() {
  echo "VIOLATION: $1:$2: $3"
  echo "    $4"
  violations=$((violations + 1))
}

scan() {
  local rel_file="$1" pattern="$2" why="$3"
  while IFS= read -r match; do
    report "$rel_file" "${match%%:*}" "$why" "${match#*:}"
  done < <(grep -nE "$pattern" "${REPO_ROOT}/${rel_file}" | strip_comments)
}

# ---------------------------------------------------------------------------
# 5 first: the rule has to still be there for the rest to mean anything.
# ---------------------------------------------------------------------------
for required in "$RUST_CANONICAL" "$SWIFT_CANONICAL" "$GOLDEN_FIXTURE"; do
  if [[ ! -f "${REPO_ROOT}/${required}" ]]; then
    echo "FAIL: ${required} is missing."
    echo "The canonical used-percent rule, its Swift port and the fixture pinning them to each"
    echo "other are what every render site was routed to. If one moved, update the paths at the"
    echo "top of this script; if one was deleted, the local renderings are back."
    exit 1
  fi
done

# ---------------------------------------------------------------------------
# 1 and 2: Rust
# ---------------------------------------------------------------------------
while IFS= read -r -d '' file; do
  rel_file="${file#"${REPO_ROOT}"/}"
  [[ "$rel_file" == "$RUST_CANONICAL" ]] && continue

  scan "$rel_file" "$RUST_LOCAL_RENDER" \
    "renders a percentage with a local format specifier instead of reporting::used_percent"
  scan "$rel_file" "$RUST_LOCAL_HELPER" \
    "defines its own percentage-to-String helper; there is one, in ${RUST_CANONICAL}"
done < <(find "${REPO_ROOT}/src" -name '*.rs' -print0)

# ---------------------------------------------------------------------------
# 3 and 4: Swift
# ---------------------------------------------------------------------------
while IFS= read -r -d '' file; do
  rel_file="${file#"${REPO_ROOT}"/}"
  [[ "$rel_file" == "$SWIFT_CANONICAL" ]] && continue

  scan "$rel_file" "(${SWIFT_PRECISION}).*(${SWIFT_PERCENT})" \
    "formats a percentage itself instead of calling GlomerisUsedPercent"
  scan "$rel_file" "$SWIFT_WHOLE_PERCENT" \
    "truncates a fraction to a whole percentage — the capacity bar's original defect"
done < <(find "${REPO_ROOT}/macos/GlomerisMenuBar/Sources" -name '*.swift' -print0)

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} local rendering(s) of a used-disk percentage."
  echo ""
  echo "Call the shared rule instead:"
  echo "  Rust  — reporting::used_percent::used_percent_text / used_percent_figure for a"
  echo "          measurement, configured_used_percent_figure for a threshold or goal the"
  echo "          user typed."
  echo "  Swift — GlomerisUsedPercent.text / .figure / .spoken."
  echo ""
  echo "A measurement is truncated to a tenth so it can never read as having reached a"
  echo "threshold the comparison did not cross; a configured figure is echoed back exactly so"
  echo "it can never read as above the boundary its own comparison uses. A local format"
  echo "specifier gets one of those two directions wrong, and the symptom is a card stating an"
  echo "alert threshold while the daemon stays silent."
  exit 1
fi

echo "PASS: every used-disk percentage is rendered by ${RUST_CANONICAL} or ${SWIFT_CANONICAL}."
exit 0
