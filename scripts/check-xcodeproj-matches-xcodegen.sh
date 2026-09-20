#!/usr/bin/env bash
#
# check-xcodeproj-matches-xcodegen.sh
#
# HORO-1296 mechanical CI guard: `macos/GlomerisMenuBar/GlomerisMenuBar.xcodeproj`
# is generated from `project.yml` by XcodeGen, and both are committed. Nothing
# stopped a `project.yml` edit from landing without the regenerated project —
# HORO-1294 needed exactly that regeneration and relied on remembering to do
# it. Since `xcodebuild` (CI here, and `macos-app-release.yml`) builds the
# committed `.xcodeproj` and ignores `project.yml` entirely, a stale project
# means the spec silently describes a build nobody performs.
#
# The check is "regenerate with the pinned XcodeGen and diff": exact, with no
# heuristic about which `project.yml` keys matter.
#
# It regenerates IN PLACE rather than into a scratch directory, deliberately.
# XcodeGen resolves source paths relative to the output location, so
# `xcodegen generate --project /tmp/somewhere` emits
#   path = "../../Users/you/.../macos/GlomerisMenuBar/Sources";
# where the committed project has
#   path = Sources;
# — i.e. an out-of-tree copy differs from the committed one for reasons that
# have nothing to do with drift. (Measured, not assumed.) So the comparison
# has to happen at the real path, and `git diff` is what reads the result.
#
# Because it writes, it refuses to run when the project already has
# uncommitted changes — otherwise it would silently overwrite work in
# progress. When drift IS found the regenerated project is left in the working
# tree on purpose: that file is the fix, so `git add` it.
#
# Exit 0 = pass (committed project matches `project.yml`).
# Exit 1 = fail (drift found, or a precondition is not met).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_DIR="macos/GlomerisMenuBar"
PROJECT_PATH="${PROJECT_DIR}/GlomerisMenuBar.xcodeproj"
SPEC_PATH="${PROJECT_DIR}/project.yml"
VERSION_FILE="${PROJECT_DIR}/.xcodegen-version"

cd "$REPO_ROOT"

for required in "$SPEC_PATH" "$VERSION_FILE"; do
  if [[ ! -f "$required" ]]; then
    echo "FAIL: ${required} not found — run this from a full checkout."
    exit 1
  fi
done

# `version=` / `sha256=` lines only; everything else in the file is prose.
expected_version="$(grep -E '^version=' "$VERSION_FILE" | head -1 | cut -d= -f2)"
if [[ -z "$expected_version" ]]; then
  echo "FAIL: no 'version=' line in ${VERSION_FILE}."
  exit 1
fi

if ! command -v xcodegen >/dev/null 2>&1; then
  echo "FAIL: xcodegen not on PATH."
  echo "Install the pinned version ${expected_version} — see ${VERSION_FILE}."
  exit 1
fi

# `xcodegen --version` prints "Version: 2.46.0".
actual_version="$(xcodegen --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)"
if [[ "$actual_version" != "$expected_version" ]]; then
  echo "FAIL: xcodegen version mismatch — this guard cannot tell drift from a toolchain difference."
  echo "  pinned (${VERSION_FILE}): ${expected_version}"
  echo "  on PATH:                  ${actual_version:-unknown}"
  echo ""
  echo "Install ${expected_version}, or — if the upgrade is intentional — bump ${VERSION_FILE},"
  echo "regenerate, and commit the resulting .xcodeproj in the same commit."
  exit 1
fi

# Refuse to clobber uncommitted project changes (see header).
if [[ -n "$(git status --porcelain -- "$PROJECT_PATH")" ]]; then
  echo "FAIL: ${PROJECT_PATH} has uncommitted changes."
  echo "This guard regenerates that file in place and will not overwrite them."
  echo "Commit or stash them, then re-run."
  exit 1
fi

echo "Regenerating ${PROJECT_PATH} with xcodegen ${actual_version}..."
( cd "$PROJECT_DIR" && xcodegen generate --spec project.yml --quiet )

if git diff --quiet --exit-code -- "$PROJECT_PATH"; then
  echo "PASS: ${PROJECT_PATH} matches what xcodegen ${expected_version} generates from ${SPEC_PATH}."
  exit 0
fi

echo ""
echo "FAIL: ${PROJECT_PATH} does not match ${SPEC_PATH}."
echo ""
echo "Someone changed project.yml without regenerating the project (or edited the"
echo "project by hand). xcodebuild builds the committed .xcodeproj and never reads"
echo "project.yml, so the spec currently describes a build that does not happen."
echo ""
echo "Fix: run this script locally and commit the regenerated project —"
echo "  bash scripts/check-xcodeproj-matches-xcodegen.sh"
echo "  git add ${PROJECT_PATH}"
echo ""
echo "Diff (regenerated vs committed):"
git --no-pager diff --stat -- "$PROJECT_PATH"
git --no-pager diff -- "$PROJECT_PATH"
exit 1
