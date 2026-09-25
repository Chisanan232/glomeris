#!/usr/bin/env bash
#
# check-cli-identity-is-reported.sh
#
# HORO-1466, as a mechanical check rather than a convention.
#
# The app resolves a `glomeris` binary and hands it all policy and execution
# authority. Before this ticket the resolved path and source were surfaced in
# exactly one place — the `executableNotFound` error, which by construction
# only renders when NOTHING was found — so an app driving a CLI that predates
# the current fail-closed executor work looked, from inside the product,
# exactly like one driving a matching CLI.
#
# The specific regression this guards against is subtler than losing the card
# altogether: it is the card coming back with a PATH but no IDENTITY beside it.
# That reads as coverage while restoring the original defect, because a path is
# not a build — two binaries measured on one machine sat at different paths,
# answered `--version` identically, and disagreed about whether an unscoped
# mutating action may be offered at all.
#
# WHAT IS CHECKED, AND WHY EACH ONE
# ---------------------------------
# 1. The identity type exists and models content as an enum, not an optional
#    hash. An optional is what lets a call site render a path and silently drop
#    the bytes; with two cases there is no "absent" to forget.
# 2. No case of the outcome enum carries a location without content. This is
#    check 1's guarantee at the level above it: `identified` is the only case
#    allowed to carry a `GlomerisExecutableLocation`.
# 3. The identity is a content hash, and the version string is NOT consulted.
#    A version comparison here cannot fail for the case this exists to catch,
#    which is worse than no check — it reads as coverage.
# 4. The view that renders the resolved path also renders the fingerprint and
#    the expectation. Checked on the rendering surface, not just the model,
#    because a correct model rendered incompletely is the same defect.
# 5. The release workflow stamps the expected hash, under exactly the key the
#    Swift reads. If the stamp is missing or the key differs, a released app
#    reports "this build records no expected tool" — the developer-build state —
#    and does so silently.
#    Its ORDER relative to signing is checked too, and HORO-1480 reversed what
#    that check requires. A deep `codesign` re-signs the embedded CLI and
#    rewrites its bytes, so hashing before it records something the release does
#    not ship. The required order is: deep sign, hash, stamp, then an
#    outer-bundle re-seal that is NOT deep. The last clause matters as much as
#    the first, and this script enforced the opposite of all of it until
#    HORO-1480.
# 6. The release workflow verifies its own stamp against the embedded bytes.
# 7. The documentation states that a locally built app embeds no CLI.
#
# Exit 0 = every property holds. Exit 1 = a violation, printed with file:line
# where there is one. An extraction that comes back empty is a failure too:
# each check asserts it found something before asserting what it found.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SOURCES_DIR="macos/GlomerisMenuBar/Sources"
IDENTITY_FILE="${SOURCES_DIR}/GlomerisCliIdentity.swift"
STATUS_VIEW="${SOURCES_DIR}/StatusHealthSectionView.swift"
VOCABULARY_FILE="${SOURCES_DIR}/GlomerisVocabulary.swift"
RELEASE_WORKFLOW=".github/workflows/macos-app-release.yml"
DOC_FILE="book/src/menu_bar_app.md"

violations=0

fail() {
  echo "VIOLATION: $1"
  violations=$((violations + 1))
}

for required in "$IDENTITY_FILE" "$STATUS_VIEW" "$VOCABULARY_FILE" "$RELEASE_WORKFLOW" "$DOC_FILE"; do
  if [[ ! -f "${REPO_ROOT}/${required}" ]]; then
    echo "FAIL: ${required} is missing, so CLI-identity reporting cannot be verified."
    echo "If it moved, update this script — do not delete the check."
    exit 1
  fi
done

# Strip whole-line comments before scanning source text, keeping original line
# numbers so a violation can be pointed at. Several files explain at length what
# the defect was; naming it there is the explanation, not the act.
without_comments() {
  grep -nv -E '^[[:space:]]*(//|\*|/\*|#)' "${REPO_ROOT}/$1" || true
}

identity_body="$(without_comments "$IDENTITY_FILE")"
workflow_body="$(without_comments "$RELEASE_WORKFLOW")"

# ---------------------------------------------------------------------------
# 1. Content is an enum with a hash case, never an optional hash.
# ---------------------------------------------------------------------------
if ! grep -qE 'enum GlomerisCliContent' <<<"$identity_body"; then
  fail "${IDENTITY_FILE}: declares no 'enum GlomerisCliContent'. The bytes of the resolved binary must be modelled as an enum, so that every path-carrying value also answers what the bytes are. If it was renamed, update this script."
fi

if ! grep -qE 'case sha256\(' <<<"$identity_body"; then
  fail "${IDENTITY_FILE}: GlomerisCliContent has no sha256 case, so there is nothing to identify a build by."
fi

# The failure mode this catches: someone "simplifies" the enum back to a
# `String?`, and every rendering site silently gains an absent-hash path.
while IFS= read -r match; do
  fail "${IDENTITY_FILE}:${match%%:*}: stores the content hash as an optional String. An optional is exactly what lets a caller render the resolved path and drop the identity, which is the defect HORO-1466 fixed one level up: ${match#*:}"
done < <(
  grep -E '(let|var) (content|contentHash|hash)[[:space:]]*:[[:space:]]*String\?' <<<"$identity_body" || true
)

# ---------------------------------------------------------------------------
# 2. Only the identified case carries a location.
# ---------------------------------------------------------------------------
outcome_cases="$(
  /usr/bin/awk '
    /^enum GlomerisCliIdentityOutcome/ { inside = 1; next }
    inside && /^}/ { exit }
    inside && /^[[:space:]]*case / { print }
  ' "${REPO_ROOT}/${IDENTITY_FILE}" || true
)"

if [[ -z "$outcome_cases" ]]; then
  echo "FAIL: found no cases in 'enum GlomerisCliIdentityOutcome' in ${IDENTITY_FILE}."
  echo "The checks below compare against them, so an empty read would make this script"
  echo "pass by examining nothing."
  exit 1
fi

while IFS= read -r case_line; do
  [[ -z "$case_line" ]] && continue
  if grep -qF 'GlomerisExecutableLocation' <<<"$case_line" \
    && ! grep -qF 'identified' <<<"$case_line"; then
    fail "${IDENTITY_FILE}: an outcome case other than 'identified' carries a GlomerisExecutableLocation, so a resolved path can be reported without its identity: ${case_line}"
  fi
done <<<"$outcome_cases"

if ! grep -qF 'case identified(GlomerisCliIdentity)' <<<"$outcome_cases"; then
  fail "${IDENTITY_FILE}: 'identified' does not carry a whole GlomerisCliIdentity. Carrying a location or a path directly would let the identity be omitted."
fi

# ---------------------------------------------------------------------------
# 3. The version string is not what the comparison is made on.
# ---------------------------------------------------------------------------
while IFS= read -r match; do
  fail "${IDENTITY_FILE}:${match%%:*}: consults a version string. Two binaries measured on one machine both reported 0.2.0 with opposite safety posture, so a version comparison here cannot fail for the case this file exists to catch: ${match#*:}"
done < <(
  grep -E '(--version|marketingVersion|CFBundleShortVersionString)' <<<"$identity_body" || true
)

if ! grep -qE 'static func sha256OfFile' <<<"$identity_body"; then
  fail "${IDENTITY_FILE}: declares no sha256OfFile. Something has to actually hash the resolved binary; without it the expectation can only ever be notDeclared."
fi

# ---------------------------------------------------------------------------
# 4. The card that renders the path renders the identity beside it.
# ---------------------------------------------------------------------------
# Scoped to the body of `cliCard` rather than to the whole file, and looking
# for CALL SITES rather than declarations. Both of those are deliberate, and
# the second was a defect in this script's first draft: it asserted that
# `fingerprintText` existed, so deleting every call to it from the card still
# passed. A helper nothing calls renders nothing — that mutation reproduced the
# exact regression this check exists to catch, and the check did not bite.
# Comment lines are dropped from the extracted body for the same reason they are
# dropped from the other files: this card explains at length what it renders and
# why, and a checked call spelled out in a comment would satisfy the searches
# below while the card rendered nothing.
cli_card="$(
  /usr/bin/awk '
    /private var cliCard: some View/ { inside = 1 }
    inside { print }
    inside && /^    }$/ { exit }
  ' "${REPO_ROOT}/${STATUS_VIEW}" | grep -vE '^[[:space:]]*(//|\*|/\*)' || true
)"

if [[ -z "$cli_card" ]]; then
  echo "FAIL: found no 'private var cliCard: some View' in ${STATUS_VIEW}."
  echo "That is the card reporting which binary is in use. If it was renamed, update this"
  echo "script — the checks below scan its body, so an empty read would make them pass by"
  echo "examining nothing."
  exit 1
fi

# Each of the four is asserted separately so the failure names what went
# missing, and each one is a distinct fact the card has to carry.
if ! grep -qF 'location.url.path' <<<"$cli_card"; then
  fail "${STATUS_VIEW}: the cliCard body never renders the resolved binary's path. The whole point of this ticket is that a user can see which binary is in use without triggering an error."
fi

if ! grep -qE 'fingerprintText\(' <<<"$cli_card"; then
  fail "${STATUS_VIEW}: the cliCard body renders a path but calls no fingerprint renderer. A path alone is not a build identity — two binaries at two paths reported the same version and behaved differently. This is the regression this check exists for: the helper existing is not the same as the card calling it."
fi

# Fed from the identity, not from anything else. Checked separately from the
# call above because the two fail differently: the call can survive while only
# the `differs` branch's `fingerprintText(.sha256(expected))` remains, which
# renders nothing at all in the matches/notDeclared/notComparable states — a
# fingerprint row that is blank or constant for most users.
if ! grep -qF 'fingerprintText(identity.content)' <<<"$cli_card"; then
  fail "${STATUS_VIEW}: the cliCard body never renders identity.content. Whatever the fingerprint row shows has to come from the bytes that were hashed; a constant or a value from elsewhere reports an identity the app did not measure."
fi

if ! grep -qF 'GlomerisVocabulary.cliExpectation' <<<"$cli_card"; then
  fail "${STATUS_VIEW}: the cliCard body never states whether the resolved binary is the build this app ships. Reporting the identity without comparing it leaves the user to do the comparison by hand against a number they do not have."
fi

if ! grep -qE 'GlomerisVocabulary\.cliSource' <<<"$cli_card"; then
  fail "${STATUS_VIEW}: the cliCard body never states which locator rule chose the binary (AC 1 requires the source, not only the path)."
fi

# And the card is actually on screen. A card built but never placed in the
# view's body is the same as no card.
#
# Read from the file rather than from `status_body`: that variable is
# `grep -nv` output and so carries `NN:` line-number prefixes, which an anchored
# pattern can never match. The first draft of this check did exactly that, and
# so failed on unmutated code — which then made every mutation in the matrix
# "fail" for that reason instead of its own, i.e. an anti-vacuity run that
# proved nothing.
if ! grep -qE '^[[:space:]]+cliCard$' "${REPO_ROOT}/${STATUS_VIEW}"; then
  fail "${STATUS_VIEW}: cliCard is declared but never placed in the view body, so nothing renders it."
fi

# All four states must have wording, or one of them renders as nothing.
#
# `\b` on the right is load-bearing. Without it, renaming the arm to
# `case .notDeclaredRenamed` satisfies a search for `case .notDeclared` by
# prefix, so the check passes over a file that no longer has wording for the
# state — which is how this check read as coverage while proving nothing.
for state in matches differs notDeclared notComparable; do
  if ! grep -qE "case \.?${state}\b" <<<"$(without_comments "$VOCABULARY_FILE")"; then
    fail "${VOCABULARY_FILE}: GlomerisVocabulary has no wording for the '${state}' identity state."
  fi
done

# ---------------------------------------------------------------------------
# 5. The release workflow stamps the expected hash, under the key Swift reads,
#    before codesign.
# ---------------------------------------------------------------------------
swift_key="$(
  /usr/bin/sed -n 's/.*static let expectedHashInfoKey = "\([^"]*\)".*/\1/p' \
    "${REPO_ROOT}/${IDENTITY_FILE}" || true
)"

if [[ -z "$swift_key" ]]; then
  echo "FAIL: could not read expectedHashInfoKey from ${IDENTITY_FILE}."
  echo "The workflow comparison below is made against it, so an empty read would make"
  echo "this script pass by comparing nothing."
  exit 1
fi

# Anchored on the PlistBuddy write verb rather than on a bare mention of the
# key. The key also appears in this workflow's own log lines and error messages,
# so a bare `grep` for it is satisfied by a run that never writes anything —
# which is how deleting the write used to be reported as an ORDERING problem
# (the surviving mention sits in the verify step, after signing) instead of as
# the missing stamp it is. Both PlistBuddy write verbs are accepted.
if ! grep -qE "(Add|Set) :${swift_key}\b" <<<"$workflow_body"; then
  fail "${RELEASE_WORKFLOW}: never writes '${swift_key}', the Info.plist key the app reads. Without the stamp, every released app reports that it records no expected tool — indistinguishable from a developer build, and silently so."
fi

# `|| true` per HORO-1479: there is a `-z` check on this variable below, and
# without tolerance here a non-matching grep would end the script at this line
# under `set -e` and that check could never run. Found by
# check-failure-diagnostics-are-reachable.sh, on this very change.
stamp_line="$(
  grep -nE "(Add|Set) :${swift_key}\b" "${REPO_ROOT}/${RELEASE_WORKFLOW}" \
    | head -n 1 | cut -d: -f1 || true
)"

# HORO-1480: this check used to require the stamp to come BEFORE signing, and
# that requirement was the defect. A deep sign signs the embedded CLI as nested
# code and rewrites its bytes, so a hash taken before it records bytes that are
# not the ones shipped. The stamp and the binary could never agree, the verify
# step further down the workflow compares exactly those two values, and the
# release job therefore failed on itself.
#
# The old reasoning — editing Info.plist breaks the seal, so the re-sign must be
# last — is correct and is not the whole picture. Both requirements hold at once
# in one order, because the nested binary's final bytes are settled by its own
# signature and do not depend on Info.plist content:
#
#   1. deep-sign, settling the embedded CLI's bytes
#   2. hash the embedded CLI
#   3. write the stamp, which invalidates the seal
#   4. re-seal the OUTER bundle only
#
# Step 4 must not be deep. A second deep sign would re-sign the embedded CLI
# again and change its bytes again, reintroducing the mismatch. That is the
# non-obvious half, so it is asserted rather than left to a comment.
deep_sign_line="$(
  grep -nE '^[[:space:]]*codesign .*--deep' "${REPO_ROOT}/${RELEASE_WORKFLOW}" \
    | head -n 1 | cut -d: -f1 || true
)"
last_sign_line="$(
  grep -nE '^[[:space:]]*codesign ' "${REPO_ROOT}/${RELEASE_WORKFLOW}" \
    | tail -n 1 | cut -d: -f1 || true
)"

if [[ -z "$deep_sign_line" ]]; then
  echo "FAIL: found no deep codesign invocation in ${RELEASE_WORKFLOW}."
  echo "The ordering checks below depend on it. If signing moved, update this script."
  exit 1
fi

if [[ -z "$stamp_line" ]]; then
  echo "FAIL: could not locate the ${swift_key} write in ${RELEASE_WORKFLOW}."
  echo "The presence check above passed, so this is a bug in this script rather than"
  echo "in the workflow. Refusing to report an ordering verdict over nothing."
  exit 1
fi

hash_line="$(
  grep -nE 'shasum -a 256' "${REPO_ROOT}/${RELEASE_WORKFLOW}" \
    | head -n 1 | cut -d: -f1 || true
)"

if [[ -z "$hash_line" ]]; then
  fail "${RELEASE_WORKFLOW}: computes no SHA-256, so whatever it stamps is not a hash of the shipped bytes."
elif [[ "$hash_line" -lt "$deep_sign_line" ]]; then
  fail "${RELEASE_WORKFLOW}:${hash_line}: hashes the embedded CLI at line ${hash_line}, before the deep codesign at line ${deep_sign_line}. A deep sign rewrites the embedded binary, so this records bytes the release does not ship and the app would report its own CLI as the wrong build (HORO-1480)."
fi

if [[ "$stamp_line" -lt "$deep_sign_line" ]]; then
  fail "${RELEASE_WORKFLOW}:${stamp_line}: writes ${swift_key} at line ${stamp_line}, before the deep codesign at line ${deep_sign_line}. The value stamped there is a hash of the pre-signature binary (HORO-1480)."
fi

if [[ -z "$last_sign_line" || "$last_sign_line" -lt "$stamp_line" ]]; then
  fail "${RELEASE_WORKFLOW}: nothing re-seals the bundle after ${swift_key} is written at line ${stamp_line}. Editing Info.plist invalidates the seal codesign applied, so a released bundle would ship with a broken signature."
elif grep -nE '^[[:space:]]*codesign ' "${REPO_ROOT}/${RELEASE_WORKFLOW}" \
  | awk -F: -v after="$stamp_line" '$1 > after' | grep -q -- '--deep'; then
  fail "${RELEASE_WORKFLOW}: deep-signs again after ${swift_key} is written at line ${stamp_line}. That re-signs the embedded CLI and changes its bytes, so the stamp stops matching what ships — the same defect as stamping too early, arrived at from the other side (HORO-1480). The final re-seal must be an outer-bundle sign without --deep."
fi

# ---------------------------------------------------------------------------
# 6. The workflow proves its own stamp survived and still matches.
# ---------------------------------------------------------------------------
# `\b` for the same reason as the wording loop above: `Print :<key>_GONE`
# contains `Print :<key>`, so an unanchored search passes over a workflow that
# no longer reads the real key back.
if ! grep -qE "Print :${swift_key}\b" <<<"$workflow_body"; then
  fail "${RELEASE_WORKFLOW}: never reads ${swift_key} back. PlistBuddy fails loudly, but codesign --deep runs afterwards and a stamp that did not survive it would ship silently."
fi

# ---------------------------------------------------------------------------
# 7. Documentation says a local build embeds no CLI.
# ---------------------------------------------------------------------------
if ! grep -qiE 'embeds no|no embedded|does not embed' "${REPO_ROOT}/${DOC_FILE}"; then
  fail "${DOC_FILE}: does not state that a locally built app embeds no CLI and therefore resolves one from the machine. A developer who does not know that cannot tell which binary their build is driving (AC 6)."
fi

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} violation(s) in how the app reports the CLI it resolved."
  echo "The app must never be able to report a resolved binary without also reporting its"
  echo "identity, and the identity must be a hash of its bytes rather than its version"
  echo "string. See ${IDENTITY_FILE}."
  exit 1
fi

echo "PASS: the resolved CLI's path, source and content hash are reported together; ${swift_key} is hashed and stamped after the deep sign, re-sealed without --deep, and verified afterwards."
exit 0
