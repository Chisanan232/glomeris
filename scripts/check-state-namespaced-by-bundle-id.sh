#!/usr/bin/env bash
#
# check-state-namespaced-by-bundle-id.sh
#
# HORO-1456, as a mechanical check rather than a convention.
#
# Every name GlomerisMenuBar files persistent state under — the keychain
# service holding the BYOK API key, and the UserDefaults domain holding the
# endpoint, model and project roots — must be bound to the running bundle's
# identifier by construction. Before this ticket all three were the string
# literal `dev.glomeris.GlomerisMenuBar`, which is the release identifier: any
# other build still read and wrote the release app's state, and the only way to
# isolate a diagnostic build was to patch three source files by hand.
#
# A literal that happens to be correct is invisible in review, which is exactly
# why this is a script and not a code-review convention.
#
# WHAT IS CHECKED, AND WHY EACH ONE
# ---------------------------------
# 1. The derivation exists, and reads the bundle rather than a constant.
# 2. Info.plist declares CFBundleIdentifier as $(PRODUCT_BUNDLE_IDENTIFIER).
#    This is the weakest link in the chain: a literal here would silently
#    decouple the running app's identifier from project.yml, and the derivation
#    would then bind state to a stale name while still looking derived.
# 3. No non-comment line under Sources/ spells the release identifier out.
#    Comments are stripped first — several files explain at length what the
#    literal used to be and why it is gone, and naming it there is the
#    explanation, not the act.
# 4. The keychain service is BundleIdentity.current, not anything else.
# 5. Nothing under Sources/ passes a UserDefaults suite name. The app uses
#    .standard, which for a bundled app IS the domain named by its bundle
#    identifier; a named suite here is either the old mistake returning
#    (Foundation rejects a suite named after the caller's own bundle) or a new
#    domain that orphans everything already stored.
# 6. Both preference stores default to .standard explicitly.
# 7. The app's bundle identifier still equals the one whose state is on real
#    machines. Not a style rule: since state is now namespaced by it, changing
#    it moves every user's stored API key and every preference. That has to be
#    a deliberate act with a migration behind it, not a rename that passes CI.
#
# Exit 0 = every property holds. Exit 1 = a violation, printed with file:line.
# An extraction that comes back empty is a failure too: each check asserts it
# found something before asserting what it found.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SOURCES_DIR="macos/GlomerisMenuBar/Sources"
PROJECT_YML="macos/GlomerisMenuBar/project.yml"
IDENTITY_FILE="${SOURCES_DIR}/BundleIdentity.swift"
KEYCHAIN_FILE="${SOURCES_DIR}/CredentialStore.swift"
INFO_PLIST="${SOURCES_DIR}/Info.plist"
ROOTS_FILE="${SOURCES_DIR}/ProjectRootsStore.swift"
SETTINGS_FILE="${SOURCES_DIR}/GlomerisLlmSettingsStore.swift"

# The identifier whose state exists on real machines. Pinned here, and compared
# against project.yml by check 7 — see the note above before changing it.
STATE_COMPATIBLE_BUNDLE_ID="dev.glomeris.GlomerisMenuBar"

abs_sources="${REPO_ROOT}/${SOURCES_DIR}"

violations=0

fail() {
  echo "VIOLATION: $1"
  violations=$((violations + 1))
}

for required in "$IDENTITY_FILE" "$KEYCHAIN_FILE" "$INFO_PLIST" "$ROOTS_FILE" "$SETTINGS_FILE" "$PROJECT_YML"; do
  if [[ ! -f "${REPO_ROOT}/${required}" ]]; then
    echo "FAIL: ${required} is missing, so state namespacing cannot be verified."
    echo "If it moved, update this script — do not delete the check."
    exit 1
  fi
done

# Strip whole-line comments before scanning source text. Keep the original line
# numbers so a violation can be pointed at.
without_comments() {
  grep -nv -E '^[[:space:]]*(//|\*|/\*)' "${REPO_ROOT}/$1" || true
}

# ---------------------------------------------------------------------------
# 1. The derivation exists and reads the running bundle.
# ---------------------------------------------------------------------------
identity_body="$(without_comments "$IDENTITY_FILE")"

if ! grep -qF 'Bundle.main.bundleIdentifier' <<<"$identity_body"; then
  fail "${IDENTITY_FILE}: does not read Bundle.main.bundleIdentifier. The identity must come from the running bundle; a constant here would defeat every other check in this script."
fi

if ! grep -qE 'static (let|func) (current|identity)' <<<"$identity_body"; then
  fail "${IDENTITY_FILE}: declares neither BundleIdentity.current nor identity(declaredBy:). If they were renamed, update this script and the call-site checks below."
fi

# ---------------------------------------------------------------------------
# 2. The plist takes the identifier from the build setting, not a literal.
# ---------------------------------------------------------------------------
plist_identifier="$(
  /usr/bin/awk '
    /<key>CFBundleIdentifier<\/key>/ { want = 1; next }
    want && /<string>/ {
      line = $0
      sub(/.*<string>/, "", line)
      sub(/<\/string>.*/, "", line)
      print line
      exit
    }
  ' "${REPO_ROOT}/${INFO_PLIST}"
)"

if [[ -z "$plist_identifier" ]]; then
  echo "FAIL: found no CFBundleIdentifier in ${INFO_PLIST}."
  echo "Without it the app has no identifier at all and every derived name falls back."
  exit 1
fi

if [[ "$plist_identifier" != '$(PRODUCT_BUNDLE_IDENTIFIER)' ]]; then
  fail "${INFO_PLIST}: CFBundleIdentifier is '${plist_identifier}', not \$(PRODUCT_BUNDLE_IDENTIFIER). A literal here decouples the running app's identifier from project.yml, so state would be filed under a stale name while the code still looked derived."
fi

# ---------------------------------------------------------------------------
# 3. No non-comment line under Sources/ spells the release identifier out.
# ---------------------------------------------------------------------------
# Read from project.yml rather than from the pin above, so this scan cannot
# drift away from whatever the app is actually built as.
declared_bundle_id="$(
  /usr/bin/awk '
    $1 == "GlomerisMenuBar:" { in_target = 1; next }
    in_target && /^  [A-Za-z]/ && $1 != "GlomerisMenuBar:" { in_target = 0 }
    in_target && $1 == "PRODUCT_BUNDLE_IDENTIFIER:" { print $2; exit }
  ' "${REPO_ROOT}/${PROJECT_YML}"
)"

if [[ ! "$declared_bundle_id" =~ ^[A-Za-z0-9.-]+\.[A-Za-z0-9-]+$ ]]; then
  echo "FAIL: could not read the GlomerisMenuBar target's PRODUCT_BUNDLE_IDENTIFIER from ${PROJECT_YML}."
  echo "Got: '${declared_bundle_id}'. The scan below compares against it, so an empty or"
  echo "malformed read would make this script pass by examining nothing."
  exit 1
fi

while IFS= read -r match; do
  file="${match%%:*}"
  rest="${match#*:}"
  fail "${file#"${REPO_ROOT}"/}:${rest%%:*}: spells the app's bundle identifier out in code: ${rest#*:}"
done < <(
  grep -rn --include='*.swift' -F "$declared_bundle_id" "$abs_sources" \
    | grep -vE ':[0-9]+:[[:space:]]*(//|\*|/\*)' \
    || true
)

# ---------------------------------------------------------------------------
# 4. The keychain service is the derived identity.
# ---------------------------------------------------------------------------
# No -n: without_comments already prefixes each line with its number, and a
# second pass would report a violation at "24:117:".
service_line="$(grep -E 'static let service' <<<"$(without_comments "$KEYCHAIN_FILE")" || true)"

if [[ -z "$service_line" ]]; then
  echo "FAIL: found no 'static let service' in ${KEYCHAIN_FILE}."
  echo "That is the keychain service every stored item is filed under. If it was renamed,"
  echo "update this check rather than leaving it scanning for something that is gone."
  exit 1
fi

if ! grep -qF 'BundleIdentity.current' <<<"$service_line"; then
  fail "${KEYCHAIN_FILE}: the keychain service is not BundleIdentity.current: ${service_line}"
fi

# ---------------------------------------------------------------------------
# 5. Nothing under Sources/ names a UserDefaults suite.
# ---------------------------------------------------------------------------
while IFS= read -r match; do
  file="${match%%:*}"
  rest="${match#*:}"
  fail "${file#"${REPO_ROOT}"/}:${rest%%:*}: names a UserDefaults suite. The app's state belongs in .standard, which for a bundled app is the domain named by its bundle identifier — a suite named after the app's own identifier is rejected by Foundation, and any other suite orphans what is already stored: ${rest#*:}"
done < <(
  grep -rn --include='*.swift' -F 'UserDefaults(suiteName' "$abs_sources" \
    | grep -vE ':[0-9]+:[[:space:]]*(//|\*|/\*)' \
    || true
)

# ---------------------------------------------------------------------------
# 6. Both preference stores default to .standard, explicitly.
# ---------------------------------------------------------------------------
for store in "$ROOTS_FILE" "$SETTINGS_FILE"; do
  store_body="$(without_comments "$store")"

  if ! grep -qE 'defaults[[:space:]]*\?\?[[:space:]]*\.standard' <<<"$store_body"; then
    fail "${store}: does not default its UserDefaults to .standard. Whatever a test injects, the app's own default must be the domain named by its bundle identifier."
  fi
done

# ---------------------------------------------------------------------------
# 7. The bundle identifier is still the one users' state is filed under.
# ---------------------------------------------------------------------------
if [[ "$declared_bundle_id" != "$STATE_COMPATIBLE_BUNDLE_ID" ]]; then
  echo "FAIL: the app's bundle identifier changed."
  echo "  ${PROJECT_YML}: ${declared_bundle_id}"
  echo "  state on real machines is filed under: ${STATE_COMPATIBLE_BUNDLE_ID}"
  echo ""
  echo "Since HORO-1456 the keychain service and the UserDefaults domain are both derived from"
  echo "this identifier, so changing it makes every already-stored BYOK API key and every stored"
  echo "preference unreachable to the new build. That is a migration, not a rename."
  echo ""
  echo "If the change is deliberate and migration is handled, update STATE_COMPATIBLE_BUNDLE_ID"
  echo "in this script in the same commit — so the decision is visible in the diff."
  exit 1
fi

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} violation(s) in how the app namespaces its persistent state."
  echo "Every name stored state is filed under must derive from BundleIdentity.current, so that a"
  echo "build with a different bundle identifier cannot read the release app's keychain items or"
  echo "preferences. See macos/GlomerisMenuBar/Sources/BundleIdentity.swift."
  exit 1
fi

echo "PASS: keychain service and preference domain both derive from the running bundle (${declared_bundle_id}); no suite name and no bundle-identifier literal under ${SOURCES_DIR}."
exit 0
