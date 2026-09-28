#!/usr/bin/env bash
#
# check-recovery-progress-is-not-persisted.sh
#
# HORO-1512 mechanical guard, verification items 11 and 12: an app restart while
# no recovery is running, and stale UI state that must not fabricate a running or
# recovered status.
#
# ============================================================================
# WHAT IS BEING PROTECTED, AND WHY IT IS A SOURCE CHECK
# ============================================================================
# `RecoveryRestartGateTests` asserts that a relaunched panel reports no run in
# flight and nothing reclaimed. It can assert that by constructing a second
# `RecoveryState()`, because a relaunch *is* a second construction: the macOS
# target has exactly two things that outlive a process — the LLM endpoint and
# model, and the project roots — and no `@AppStorage` anywhere. Nothing about a
# recovery run is written down.
#
# That premise is what this script keeps true. The day a run's phase, its round
# number or its reclaimed total is persisted, those tests keep passing — a fresh
# object seeded from a store is still a fresh object — while the product starts
# doing the thing they were written to forbid. The regression is the *absence* of
# a write becoming a presence, and an absence is not something a test can
# observe; it is a fact about the source.
#
# The harm is specific. A recovery run deletes things, so the numbers it produces
# are the only account the user has of what is now gone. A panel reopening on the
# last run's figures presents them as this session's: bytes somebody else's run
# reclaimed, read as bytes just reclaimed. A persisted `isRecovering` is worse
# still — a process killed mid-run would leave a flag no run exists to release,
# and the compare-and-set in `beginRecovering()` would refuse every subsequent
# run on the strength of it. There is no honest restore for either, which is why
# the answer is not to restore carefully but not to store at all.
#
# Three checks:
#
#   1. No recovery surface writes to, or reads from, any state-restoration
#      mechanism. `UserDefaults`, `@AppStorage` and `@SceneStorage` are the three
#      routes a SwiftUI app takes by default; the others are the ones someone
#      reaches for when the first three are being watched.
#
#   2. The live figures stay unassignable from outside. `liveProgress` is
#      `private(set)` and `beginRecovering()` clears it, which together are what
#      make a stale value unreachable: nothing but a decoded CLI event can put a
#      figure there, and a new run cannot open on the previous one's.
#
#   3. The runtime half still exists. A required-file check is weak evidence
#      about behaviour, but it catches the change that would leave this script
#      guarding a claim nobody asserts any more.
#
# Deliberately NOT in scope: `Sources/UnpromptedRecovery.swift` and
# `Sources/PressureEpisodeMonitor.swift`. Both hold session state — which banner
# was raised, whether an unprompted attempt was already spent — and neither can
# make a panel report a run or a reclaimed total. If a future ticket needs one of
# those remembered across a launch, that is a legitimate preference and this
# guard should not be the thing standing in its way. `RecoveryStopRequest.swift`
# *is* in scope even though it writes a file on purpose: the file is a sentinel in
# the temporary directory, freshly named per run, and the point of checking it
# here is that it must never become a durable record of a stop.
#
# Exit 0 = pass. Exit 1 = fail, with file:line detail on stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SWIFT_DIR="macos/GlomerisMenuBar/Sources"

# The recovery-run surface. Every file here either holds a figure describing a
# run or decides what the panel shows about one.
RECOVERY_SOURCES=(
  "RecoveryLiveProgress.swift"
  "RecoverySectionView.swift"
  "RecoveryStopRequest.swift"
  "RecoveryDeepLinkWindow.swift"
)

# `OverviewState.swift` is handled separately: it holds `ScanState` and
# `PlanState` as well, and a defaults-backed store legitimately appearing beside
# them later must not turn this red. Only the `RecoveryState` class is checked,
# which is where a run's state lives.
STATE_FILE="Sources/OverviewState.swift"
STATE_CLASS="final class RecoveryState"

RUNTIME_HALF="macos/GlomerisMenuBar/Tests/RecoveryRestartGateTests.swift"

# These files document at length why they must not persist anything, and naming
# `UserDefaults` in that prose is the explanation rather than the act.
strip_comments() {
  grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' || true
}

violations=0

# ---------------------------------------------------------------------------
# Check 1: nothing on the recovery surface reaches a restoration mechanism.
# ---------------------------------------------------------------------------

FORBIDDEN=(
  'UserDefaults;;persists state a relaunch would show as this session'\''s'
  '@AppStorage;;binds a view to persisted state, which is a restore path by another name'
  '@SceneStorage;;is macOS state restoration, which would reopen the panel on a dead run'
  'NSUbiquitousKeyValueStore;;would carry a run'\''s figures to another Mac entirely'
  'NSKeyedArchiver;;archives a value, and a run'\''s state has nowhere honest to be archived to'
  'PropertyListEncoder;;serializes a value for storage'
  'applicationSupportDirectory;;names the durable location a run'\''s state must not reach'
)

check_pattern_in_text() {
  # $1 = pattern, $2 = why, $3 = label for output, $4 = numbered text on stdin
  local pattern="$1" why="$2" label="$3"
  while IFS= read -r match; do
    [[ -n "$match" ]] || continue
    echo "VIOLATION: ${label}:${match%%:*}: ${why}"
    echo "    ${match#*:}"
    violations=$((violations + 1))
  done < <(grep -E ":.*${pattern}" | strip_comments)
}

for entry in "${FORBIDDEN[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"

  # A pattern grep cannot compile matches nothing, and `|| true` inside
  # strip_comments would turn that into a silent PASS. Compile it against empty
  # input first: no output means it compiled.
  compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi

  for name in "${RECOVERY_SOURCES[@]}"; do
    rel_file="${SWIFT_DIR}/${name}"
    abs_file="${REPO_ROOT}/${rel_file}"
    if [[ ! -f "$abs_file" ]]; then
      # Not skippable. A recovery surface this script no longer watches is the
      # regression, not the absence of one.
      echo "FAIL: ${rel_file} is missing."
      echo "If it moved, update RECOVERY_SOURCES in this script to match."
      exit 1
    fi
    check_pattern_in_text "$pattern" "$why" "$rel_file" \
      < <(grep -n '' "$abs_file")
  done

  # The `RecoveryState` window of the shared state file.
  abs_state="${REPO_ROOT}/macos/GlomerisMenuBar/${STATE_FILE}"
  if [[ ! -f "$abs_state" ]]; then
    echo "FAIL: macos/GlomerisMenuBar/${STATE_FILE} is missing."
    exit 1
  fi
  if ! grep -qF "$STATE_CLASS" "$abs_state"; then
    echo "FAIL: '${STATE_CLASS}' is not in ${STATE_FILE}."
    echo "The recovery run's state moved. Update STATE_FILE/STATE_CLASS so this guard follows it."
    exit 1
  fi
  check_pattern_in_text "$pattern" "$why" "${STATE_FILE} (RecoveryState)" \
    < <(awk -v marker="$STATE_CLASS" \
          'index($0, marker) { found = 1 } found { print NR ":" $0 }' "$abs_state")
done

# ---------------------------------------------------------------------------
# Check 2: the live figures cannot be assigned from outside, and a new run
# does not open on the last one's.
# ---------------------------------------------------------------------------

abs_state="${REPO_ROOT}/macos/GlomerisMenuBar/${STATE_FILE}"

needle="@Published private(set) var liveProgress"
if ! grep -nF "$needle" "$abs_state" | strip_comments | grep -q .; then
  echo "VIOLATION: ${STATE_FILE}: '${needle}' is gone — only RecoveryState may write the"
  echo "    live figures, so that nothing outside it can put a number there the CLI never reported."
  violations=$((violations + 1))
fi

# Windowed on `beginRecovering()`'s body rather than the whole file, because
# `resetGoalProgress()` contains the same line: a file-wide grep would stay green
# with the reset deleted from exactly the place it matters. `func beginRecovering`
# to the next declaration at the same indentation.
begin_body="$(
  awk '
    /func beginRecovering/ { inside = 1; print; next }
    inside && /^    (@|\/\/\/|func |var |let |private|public|static)/ { inside = 0 }
    inside { print }
  ' "$abs_state"
)"
if [[ -z "$begin_body" ]]; then
  echo "FAIL: could not find beginRecovering()'s body in ${STATE_FILE}."
  echo "The run guard moved or was renamed. Update this script so check 2 follows it."
  exit 1
fi
if ! printf '%s\n' "$begin_body" | grep -qF "liveProgress = RecoveryLiveProgress()"; then
  echo "VIOLATION: ${STATE_FILE}: beginRecovering() no longer clears the previous run's figures —"
  echo "    a new run would open showing what the last one reclaimed, which is the same lie a"
  echo "    restored panel would tell, one run later instead of one launch later."
  violations=$((violations + 1))
fi

# ---------------------------------------------------------------------------
# Check 3: the runtime half of items 11 and 12 still exists.
# ---------------------------------------------------------------------------

if [[ ! -f "${REPO_ROOT}/${RUNTIME_HALF}" ]]; then
  echo "VIOLATION: ${RUNTIME_HALF} is gone."
  echo "    That file asserts what a relaunched panel reports. Without it this script guards a"
  echo "    premise nothing checks the consequences of."
  violations=$((violations + 1))
fi

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} problem(s) with HORO-1512's no-persisted-recovery boundary."
  echo "A recovery run deletes things, so its figures are the only account the user has of what is"
  echo "gone. A panel that reopened on a previous run's numbers would present them as this session's,"
  echo "and a persisted 'a run is in flight' would refuse every future run with nothing to release it."
  echo "Nothing about a run is stored, which is what lets RecoveryRestartGateTests prove a relaunch"
  echo "claims nothing. See the header of macos/GlomerisMenuBar/Tests/RecoveryRestartGateTests.swift."
  exit 1
fi

echo "PASS: no recovery surface persists a run, and its live figures stay writable only by the state that owns them."
exit 0
