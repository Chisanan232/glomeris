#!/usr/bin/env bash
#
# check-app-version-matches-crate.sh
#
# HORO-1329 mechanical CI guard: the version GlomerisMenuBar.app reports must
# be the version of the `glomeris` crate it embeds.
#
# Why it is worth a guard. The app bundle is not built from Cargo.toml — it is
# built from macos/GlomerisMenuBar/project.yml, and before this ticket that
# file carried a hardcoded CFBundleShortVersionString of 0.1.0 while the crate
# was at 0.2.0. Nothing noticed, because nothing compares them: `cargo` never
# reads project.yml and `xcodebuild` never reads Cargo.toml. The consequence is
# specific rather than cosmetic — macos-app-release.yml downloads the CLI
# published for tag vX, embeds it in the bundle, and uploads the result as an
# asset of that same release, so a single .app answered "which version is
# this" two different ways: the CLI said one number and About / Finder / the
# Info.plist said another.
#
# What this asserts, and why each part is needed:
#
#   1. project.yml's CFBundleShortVersionString is the build-setting reference
#      `$(MARKETING_VERSION)`, not a literal. Without this, someone could
#      reintroduce a hardcoded number and every check below would still pass
#      while the shipped bundle drifted again — the comparison would be
#      measuring a setting the plist no longer uses.
#   2. The committed Sources/Info.plist agrees. That file is generated from
#      project.yml, but check-xcodeproj-matches-xcodegen.sh only diffs the
#      .xcodeproj, so the generated plist is otherwise unguarded — and it is
#      the file xcodebuild actually processes.
#   3. Every MARKETING_VERSION in the committed .pbxproj equals the crate
#      version. This is what xcodebuild reads, and asserting it directly keeps
#      this guard self-sufficient: it needs no XcodeGen, no Xcode and no macOS
#      runner, so it cannot be silently reduced to "the spec looks right".
#   4. project.yml's MARKETING_VERSION equals the crate version.
#
# Scope note: CFBundleVersion is deliberately not checked. It is the build
# number, never shown to the user, and nothing in this repo produces a
# monotonic counter to derive it from.
#
# This guard plus cargo-dist's own tag/version check is what makes the full
# claim hold: the guard gives app == crate, dist gives crate == tag, so the
# bundle uploaded for a tag reports that tag.
#
# It writes nothing and reads only tracked files, so it is safe on a dirty
# tree.
#
# Exit 0 = pass (the app version and the crate version agree).
# Exit 1 = fail (drift found, or a precondition is not met).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CARGO_TOML="Cargo.toml"
PROJECT_DIR="macos/GlomerisMenuBar"
SPEC="${PROJECT_DIR}/project.yml"
PLIST="${PROJECT_DIR}/Sources/Info.plist"
PBXPROJ="${PROJECT_DIR}/GlomerisMenuBar.xcodeproj/project.pbxproj"

for required in "$CARGO_TOML" "$SPEC" "$PLIST" "$PBXPROJ"; do
  if [[ ! -f "$required" ]]; then
    echo "FAIL: ${required} not found — run this from a full checkout."
    exit 1
  fi
done

fail() {
  echo ""
  echo "FAIL: $1"
  echo ""
  echo "The app bundle reports its version from ${SPEC}; the CLI it embeds"
  echo "reports the version in ${CARGO_TOML}. macos-app-release.yml ships both"
  echo "inside one .app, so they have to be the same number."
  echo ""
  echo "Fix: set MARKETING_VERSION in ${SPEC} to the crate version, then"
  echo "regenerate and commit the project and plist together —"
  echo "  bash scripts/check-xcodeproj-matches-xcodegen.sh"
  echo "  git add ${PBXPROJ} ${PLIST}"
  exit 1
}

# `version = "..."` from the [package] table only — the file has other tables
# with their own version keys.
crate_version="$(
  awk '
    /^\[/ { in_package = ($0 == "[package]") }
    in_package && /^version[[:space:]]*=/ {
      gsub(/^version[[:space:]]*=[[:space:]]*"|"[[:space:]]*$/, "")
      print; exit
    }
  ' "$CARGO_TOML"
)"
if [[ -z "$crate_version" ]]; then
  echo "FAIL: no 'version' under [package] in ${CARGO_TOML}."
  exit 1
fi

# 1. The spec must reference the build setting, not a literal.
spec_plist_value="$(
  grep -E '^[[:space:]]*CFBundleShortVersionString:' "$SPEC" \
    | head -1 | sed -E 's/^[^:]*:[[:space:]]*"?([^"]*)"?[[:space:]]*$/\1/'
)"
# shellcheck disable=SC2016  # the literal string $(MARKETING_VERSION) is the
# thing being asserted; expanding it here would defeat the check.
if [[ "$spec_plist_value" != '$(MARKETING_VERSION)' ]]; then
  fail "${SPEC} sets CFBundleShortVersionString to '${spec_plist_value:-<missing>}', not \$(MARKETING_VERSION).
       A literal version here is the original defect: it makes MARKETING_VERSION
       decorative, so every other check in this guard would pass vacuously."
fi

# 2. The generated plist — the file xcodebuild processes — must agree.
plist_value="$(
  grep -A1 '<key>CFBundleShortVersionString</key>' "$PLIST" \
    | grep '<string>' | head -1 | sed -E 's;.*<string>(.*)</string>.*;\1;'
)"
# shellcheck disable=SC2016  # as above: a literal, not an expansion.
if [[ "$plist_value" != '$(MARKETING_VERSION)' ]]; then
  fail "${PLIST} sets CFBundleShortVersionString to '${plist_value:-<missing>}', not \$(MARKETING_VERSION).
       That file is generated from ${SPEC}; it is stale."
fi

# 3. Every MARKETING_VERSION in the committed project (one per build
#    configuration) must be the crate version. `xcodebuild` reads this file and
#    nothing else here, so this is the assertion that actually binds the build.
#    (A while-read loop rather than `mapfile`: macOS ships bash 3.2 and this
#    script is meant to be runnable locally, not only on the Linux runner.)
pbx_versions=""
pbx_count=0
while IFS= read -r v; do
  [[ -z "$v" ]] && continue
  pbx_count=$((pbx_count + 1))
  pbx_versions="${pbx_versions:+${pbx_versions} }${v}"
  if [[ "$v" != "$crate_version" ]]; then
    fail "${PBXPROJ} has MARKETING_VERSION = ${v}, but the crate is ${crate_version}."
  fi
done < <(
  grep -E '^[[:space:]]*MARKETING_VERSION[[:space:]]*=' "$PBXPROJ" \
    | sed -E 's/^[[:space:]]*MARKETING_VERSION[[:space:]]*=[[:space:]]*"?([^";]*)"?;.*$/\1/'
)
if [[ "$pbx_count" -eq 0 ]]; then
  fail "no MARKETING_VERSION in ${PBXPROJ}.
       xcodebuild builds that file and never reads ${SPEC}, so without it the
       bundle would expand \$(MARKETING_VERSION) to an empty string."
fi

# 4. And the spec the project is generated from.
spec_version="$(
  grep -E '^[[:space:]]*MARKETING_VERSION:' "$SPEC" \
    | head -1 | sed -E 's/^[^:]*:[[:space:]]*"?([^"]*)"?[[:space:]]*$/\1/'
)"
if [[ "$spec_version" != "$crate_version" ]]; then
  fail "${SPEC} has MARKETING_VERSION ${spec_version:-<missing>}, but the crate is ${crate_version}."
fi

echo "PASS: the app bundle and the glomeris crate both report ${crate_version}."
echo "  ${CARGO_TOML}: ${crate_version}"
echo "  ${SPEC}: MARKETING_VERSION ${spec_version}, CFBundleShortVersionString \$(MARKETING_VERSION)"
echo "  ${PBXPROJ}: MARKETING_VERSION ${pbx_versions} (${pbx_count} build configurations)"
exit 0
