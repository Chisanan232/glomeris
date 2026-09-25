#!/usr/bin/env bash
#
# check-no-swift6-concurrency-errors.sh
#
# HORO-1478 mechanical CI guard: the macOS target builds at
# `SWIFT_VERSION = 5.0`, so a diagnostic that Swift 6 treats as an error is
# only a warning here. `xcodebuild` still exits 0, the suite is still green,
# and nothing stops such a diagnostic from being added.
#
# That is how HORO-1478 happened. `GlomerisClient.run` declares its callback
# `@Sendable`, one test passed a closure that appended into a captured `var`,
# and the resulting "mutation of captured var ... in concurrently-executing
# code" warning sat in the build log through several green runs. It was not a
# future problem: `@Sendable` is the client's statement that it may call the
# closure from another context, so the appends had no declared ordering
# against the test thread reading the array afterwards.
#
# WHY THIS IS NOT A NO-WARNINGS GATE
#
# Counting warnings would be easier and would be wrong. `CredentialStore` and
# `CredentialStoreAccessTests` deliberately call `SecAccessCreate`,
# `SecTrustedApplicationCreateFromPath`, `SecAccessCopyACLList`,
# `SecACLCopyAuthorizations` and `SecACLCopyContents` — all deprecated since
# macOS 10.10 — because HORO-1455 needs the legacy keychain ACL API to assert
# what it asserts. There is no non-deprecated replacement that exposes an
# ACL's trusted-application list, so those warnings are intended and permanent.
# A gate that counted them would either be permanently red or would have to be
# given an exception list that grows silently.
#
# So this names the one thing it cares about: diagnostics Swift itself marks as
# an error in the Swift 6 language mode. Those are not stylistic. Each one is a
# concurrency rule the compiler can prove is broken.
#
# Usage:
#   check-no-swift6-concurrency-errors.sh <build-log>
#
# Exit 0 = pass (no such diagnostic in the log).
# Exit 1 = fail (found some, or the log is unusable).

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "FAIL: usage: $(basename "$0") <build-log>"
  exit 1
fi

LOG="$1"

if [[ ! -f "$LOG" ]]; then
  echo "FAIL: build log not found: ${LOG}"
  exit 1
fi

# A log that exists but holds nothing is not a pass. Zero findings over zero
# input is the vacuous green this whole guard exists to avoid, so require that
# the compiler actually said something.
if [[ ! -s "$LOG" ]]; then
  echo "FAIL: build log is empty: ${LOG}"
  echo "Nothing was compiled, so 'no findings' would mean nothing."
  exit 1
fi

# `|| true` on both: `grep` exits 1 when it matches nothing, which under
# `pipefail` is the pipeline's status, which under `set -e` would end the
# script at the assignment — before the reporting below could run. Matching
# nothing is the PASS case here, so it has to survive.
#
# Two patterns, because the phrasing is not stable across Swift releases but
# the marker is: the compiler says the diagnostic is an error in Swift 6.
findings="$(grep -nE "this is an error in the Swift 6 language mode|will be an error in Swift 6|error in Swift 6 language mode" "$LOG" || true)"

# Independent evidence the log is a real Swift build, so that a log full of
# unrelated text cannot pass by simply not containing the phrase.
compiled="$(grep -cE "SwiftCompile |SwiftDriver |swift-frontend|CompileSwiftSources" "$LOG" || true)"
if [[ "${compiled:-0}" -eq 0 ]]; then
  echo "FAIL: ${LOG} contains no sign of a Swift compilation."
  echo "Refusing to report 'clean' for a log this guard cannot read."
  exit 1
fi

if [[ -z "$findings" ]]; then
  echo "PASS: no Swift 6 language-mode errors in ${LOG} (${compiled} Swift build lines scanned)."
  exit 0
fi

count="$(printf '%s\n' "$findings" | wc -l | tr -d ' ')"

echo ""
echo "FAIL: ${count} diagnostic(s) that Swift 6 treats as an error, not a warning."
echo ""
echo "The target builds at SWIFT_VERSION = 5.0, so xcodebuild still exited 0 and"
echo "the tests still passed. That is not reassurance: each of these is a"
echo "concurrency rule the compiler can already prove is broken, and the pass is"
echo "only because the language mode has not moved yet."
echo ""
printf '%s\n' "$findings" | sed 's|.*/macos/|  macos/|'
echo ""
echo "Fix the diagnostic. Do not raise the language mode to silence it, and do"
echo "not add it to an exception list — there isn't one, deliberately."
echo ""
echo "For a callback declared @Sendable, collect into a lock-guarded box rather"
echo "than a captured var. ProgressEventLog in CandidatesSectionViewTests.swift"
echo "and InvocationLog in GlomerisClientTests.swift are both examples."
exit 1
