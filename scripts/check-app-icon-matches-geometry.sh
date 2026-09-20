#!/usr/bin/env bash
#
# check-app-icon-matches-geometry.sh
#
# HORO-1305 mechanical CI guard, in the same spirit as
# check-xcodeproj-matches-xcodegen.sh: the app icon PNGs under
# `macos/GlomerisMenuBar/Sources/Assets.xcassets/AppIcon.appiconset` are
# generated from `Sources/GlomerisMark.swift`, and both are committed.
#
# Why it is worth a guard. Binary assets are the one thing a reviewer cannot
# actually review — a PNG in a diff is "Binary files differ", so any claim
# about what the icon looks like is taken on trust. Generating it from
# readable geometry only helps if something proves the committed bytes really
# are that geometry's output. Without this check, the mark could be edited
# with the PNGs left stale (Finder and the Dock read the asset catalog, never
# GlomerisMark.swift), or the PNGs could be replaced with unrelated art and
# the code comments would still describe the old design.
#
# It writes nothing: the generator's `--verify` mode renders in memory and
# compares against what is on disk, so this is safe to run on a dirty tree
# (unlike the xcodeproj guard, which must regenerate in place).
#
# Comparison is pixel-wise with a tight tolerance rather than a file hash —
# see macos/tools/icongen/main.swift for why a hash would flake.
#
# Exit 0 = pass (committed icons match the geometry).
# Exit 1 = fail (drift found, or a precondition is not met).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ICON_SET="macos/GlomerisMenuBar/Sources/Assets.xcassets/AppIcon.appiconset"

if [[ ! -d "$ICON_SET" ]]; then
  echo "FAIL: ${ICON_SET} not found."
  echo "Generate it: bash scripts/generate-app-icon.sh"
  exit 1
fi

if bash scripts/generate-app-icon.sh --verify --out "$ICON_SET"; then
  exit 0
fi

echo ""
echo "The committed app icon is not what Sources/GlomerisMark.swift produces."
echo ""
echo "Finder, the Dock, System Settings > Login Items and Cmd-Tab all read the"
echo "compiled asset catalog and never read the geometry, so a stale icon means"
echo "the code describes a mark the user never sees."
echo ""
echo "Fix: regenerate and commit the result in the same commit as the geometry —"
echo "  bash scripts/generate-app-icon.sh"
echo "  git add ${ICON_SET}"
exit 1
