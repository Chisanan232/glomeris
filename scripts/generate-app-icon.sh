#!/usr/bin/env bash
#
# generate-app-icon.sh
#
# HORO-1305. Regenerates the committed macOS AppIcon set from the canonical
# geometry in `macos/GlomerisMenuBar/Sources/GlomerisMark.swift`.
#
# The PNGs under Assets.xcassets are DERIVED ARTEFACTS, not source. Editing
# them by hand is a mistake that `check-app-icon-matches-geometry.sh` will
# reject in CI — change the geometry and re-run this instead.
#
# The generator is compiled against the app's own GlomerisMark.swift (not a
# copy of it), which is what makes the app icon and the menu-bar glyph the
# same shape by construction. That also means this script is the only place
# that knows how to build the tool, so the drift check shells out to it with
# `--verify` rather than duplicating the compile line.
#
# Usage:
#   bash scripts/generate-app-icon.sh              # rewrite the committed set
#   bash scripts/generate-app-icon.sh --verify     # compare, write nothing
#   bash scripts/generate-app-icon.sh --out DIR    # write somewhere else

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MARK_SOURCE="macos/GlomerisMenuBar/Sources/GlomerisMark.swift"
GENERATOR_SOURCE="macos/tools/icongen/main.swift"
DEFAULT_OUT="macos/GlomerisMenuBar/Sources/Assets.xcassets/AppIcon.appiconset"

out_dir="$DEFAULT_OUT"
verify=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --verify) verify="--verify"; shift ;;
    --out) out_dir="${2:?--out needs a directory}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

for required in "$MARK_SOURCE" "$GENERATOR_SOURCE"; do
  if [[ ! -f "$required" ]]; then
    echo "FAIL: ${required} not found — run this from a full checkout." >&2
    exit 1
  fi
done

if ! command -v xcrun >/dev/null 2>&1; then
  echo "FAIL: xcrun not on PATH — the Xcode command line tools are required." >&2
  exit 1
fi

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

# `-O` matters for more than speed: at `-Onone` the float arithmetic is the
# same, but the build is slow enough that contributors skip re-running this.
#
# The deployment target is pinned to the app's own (project.yml, macOS 13.0)
# so the generator cannot start depending on an API the app itself could not
# use — GlomerisMark.swift is shared code and must stay 13.0-clean.
xcrun swiftc \
  -O \
  -target "$(uname -m)-apple-macos13.0" \
  -o "${build_dir}/icongen" \
  "$MARK_SOURCE" "$GENERATOR_SOURCE"

"${build_dir}/icongen" "$out_dir" ${verify:+$verify}
