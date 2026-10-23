#!/usr/bin/env bash
# HORO-2176: the DEV-QA synthetic-data seam must exist only in DEBUG builds.
#
#   check-qa-fixtures-absent-from-release.sh            source check only
#   check-qa-fixtures-absent-from-release.sh <App.app>  source check + binary check
#
# Source check: every reference to the seam outside its own file sits inside a
# `#if DEBUG` block. Binary check: none of the seam's strings survive in the
# app's main executable, which is what a Release/DEV-QA/shipped build must show.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/macos/GlomerisMenuBar/Sources"
markers=("GLOMERIS_QA_FIXTURE_CLI" "DEV-QA SYNTHETIC DATA" "glomeris.qa.banner" "devqa-synthetic")
fail=0

# --- source: seam references must be DEBUG-guarded -------------------------------
while IFS= read -r file; do
  base="$(basename "$file")"
  [[ "$base" == "GlomerisQAFixtures.swift" ]] && continue
  awk -v f="$base" '
    /^[[:space:]]*#if[[:space:]]+DEBUG/ { depth++ ; next }
    /^[[:space:]]*#(else|elseif)/ { if (depth > 0) depth-- ; next }
    /^[[:space:]]*#endif/ { if (depth > 0) depth-- ; next }
    /GlomerisQAFixtures\.(fixtureCLI|isActive|cliEnvironmentKey)/ {
      if (depth == 0) { printf "VIOLATION: %s:%d: seam used outside #if DEBUG: %s\n", f, NR, $0; bad = 1 }
    }
    END { exit bad }
  ' "$file" || fail=1
done < <(grep -l "GlomerisQAFixtures" "$src"/*.swift)

# The seam file itself must wrap everything but the Release stub in #if DEBUG.
if ! grep -q '^#if DEBUG' "$src/GlomerisQAFixtures.swift" || ! grep -q '^#else' "$src/GlomerisQAFixtures.swift"; then
  echo "VIOLATION: GlomerisQAFixtures.swift lost its #if DEBUG / #else structure"; fail=1
fi
if awk '/^#else/{r=1} r && /GLOMERIS_QA|DEV-QA|fixtureCLI/{found=1} END{exit !found}' "$src/GlomerisQAFixtures.swift"; then
  echo "VIOLATION: the Release branch of GlomerisQAFixtures.swift mentions the seam"; fail=1
fi

# --- binary ----------------------------------------------------------------------
if [[ $# -ge 1 ]]; then
  app="$1"
  exe="$app/Contents/MacOS/GlomerisMenuBar"
  [[ -f "$exe" ]] || { echo "ERROR: no executable at $exe"; exit 2; }
  # Every file in the bundle (code, Resources, plists), not just the main
  # executable: a Debug build keeps the app's code in GlomerisMenuBar.debug.dylib
  # beside a stub executable, so scanning only the stub proves nothing.
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' EXIT
  scanned=0
  while IFS= read -r -d '' bin; do
    if file -b "$bin" | grep -q 'Mach-O'; then scanned=$((scanned + 1)); fi
    # To a file first: `strings | grep -q` under pipefail reports "not found"
    # whenever grep exits early and strings dies of SIGPIPE, which is a silent
    # false PASS on exactly the binary this guard exists to catch.
    strings -a "$bin" >"$tmp"
    for m in "${markers[@]}"; do
      if grep -qF -- "$m" "$tmp"; then
        echo "VIOLATION: ${bin#"$app"/} contains '$m'"; fail=1
      fi
    done
  done < <(find "$app" -type f -print0 2>/dev/null)
  if [[ $scanned -eq 0 ]]; then echo "ERROR: no Mach-O files found under $app"; exit 2; fi
  echo "binary check: scanned $scanned Mach-O file(s) in $app"
fi

if [[ $fail -ne 0 ]]; then exit 1; fi
echo "PASS: the DEV-QA fixture seam is DEBUG-only${1:+ and absent from $1}"
