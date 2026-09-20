#!/usr/bin/env bash
#
# check-credential-store-uses-keychain.sh
#
# HORO-1309 AC 2 and AC 5, as a mechanical check rather than a convention.
#
# The BYOK API key is the only secret GlomerisMenuBar ever holds. It must
# live in the keychain, reach the CLI only through a child process's
# environment, and never be rendered, logged, argued or persisted anywhere
# else. Those are all one-line mistakes to make and invisible in review:
# `defaults.set(key, forKey: "llmApiKeyLastUsed")` so the settings screen
# can show the last four characters is a perfectly reasonable-looking commit.
#
# WHY A SOURCE CHECK AND NOT A TEST
# ---------------------------------
# An unsigned `xcodebuild` test binary has no keychain-access-group
# entitlement, so `SecItemAdd` can return `errSecMissingEntitlement`
# (-34018) in CI while succeeding on a developer machine. A test that
# skipped itself on that status would be a permanently green no-op — the
# HORO-1253 failure mode, where a gate that never ran read as a gate that
# passed. So the *behaviour* is tested against an in-memory double, and the
# two facts that double cannot establish — that production is wired to the
# keychain, and that nothing else in the app touches the secret — are
# established here, where no entitlement is needed and nothing can be
# skipped.
#
# Exit 0 = every property holds. Exit 1 = a violation, printed with
# file:line. An extraction that comes back empty is a failure too: each
# check below asserts it found something before asserting what it found.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SOURCES_DIR="macos/GlomerisMenuBar/Sources"
STORE_FILE="${SOURCES_DIR}/CredentialStore.swift"
SETTINGS_FILE="${SOURCES_DIR}/GlomerisLlmSettingsStore.swift"

abs_sources="${REPO_ROOT}/${SOURCES_DIR}"
abs_store="${REPO_ROOT}/${STORE_FILE}"
abs_settings="${REPO_ROOT}/${SETTINGS_FILE}"

violations=0

fail() {
  echo "VIOLATION: $1"
  violations=$((violations + 1))
}

for required in "$abs_store" "$abs_settings"; do
  if [[ ! -f "$required" ]]; then
    echo "FAIL: ${required#"${REPO_ROOT}"/} is missing, so the credential path cannot be verified."
    echo "If it moved, update this script — do not delete the check."
    exit 1
  fi
done

# Strip full-line comments before every scan. Both files document at length
# why they must not do these things, and naming `UserDefaults` in that prose
# is the explanation, not the act.
without_comments() {
  grep -nv -E '^[[:space:]]*(//|\*|/\*)' "$1" || true
}

# ---------------------------------------------------------------------------
# 1. The real store really is the keychain.
# ---------------------------------------------------------------------------
store_body="$(without_comments "$abs_store")"

for needle in \
  'import Security' \
  'kSecClassGenericPassword' \
  'SecItemCopyMatching' \
  'SecItemAdd' \
  'SecItemUpdate' \
  'SecItemDelete' \
  'kSecAttrAccessibleWhenUnlockedThisDeviceOnly'; do
  if ! grep -qF "$needle" <<<"$store_body"; then
    fail "${STORE_FILE}: no mention of ${needle}. The keychain-backed store must actually use the keychain API, and a BYOK key must not be iCloud-synced or backed up."
  fi
done

# A keychain-backed store has no business knowing about UserDefaults. This
# is the check that catches "fall back to UserDefaults if the keychain
# fails", which would silently downgrade a secret to a plist on disk.
while IFS= read -r match; do
  fail "${STORE_FILE}:${match%%:*}: mentions UserDefaults. The secret store must have no plaintext fallback: ${match#*:}"
done < <(grep -E '\bUserDefaults\b' <<<"$store_body" || true)

# ---------------------------------------------------------------------------
# 2. Production is wired to the keychain, and the test double is not used.
# ---------------------------------------------------------------------------
settings_body="$(without_comments "$abs_settings")"

if ! grep -qF 'KeychainCredentialStore()' <<<"$settings_body"; then
  fail "${SETTINGS_FILE}: does not construct KeychainCredentialStore(). Whatever the tests inject, the app's own default must be the keychain."
fi

# `InMemoryCredentialStore` may exist, but only as its own declaration. Any
# other non-comment mention under Sources/ means production can be handed a
# store that keeps the key in a dictionary — which is what the tests use
# precisely because it is not safe for real use.
while IFS= read -r match; do
  file="${match%%:*}"
  rest="${match#*:}"
  lineno="${rest%%:*}"
  content="${rest#*:}"
  if [[ "$content" =~ (class|struct)[[:space:]]+InMemoryCredentialStore ]]; then
    continue
  fi
  fail "${file#"${REPO_ROOT}"/}:${lineno}: uses InMemoryCredentialStore outside its own declaration. The in-memory store is a test double: ${content}"
done < <(
  grep -rn --include='*.swift' -E '\bInMemoryCredentialStore\b' "$abs_sources" \
    | grep -vE ':[0-9]+:[[:space:]]*(//|\*|/\*)' \
    || true
)

# ---------------------------------------------------------------------------
# 3. Nothing anywhere in the app persists, prints, argues or displays it.
# ---------------------------------------------------------------------------
# Identifiers that name the secret itself. Deliberately NOT `hasStoredApiKey`
# or `apiKeyAccount`/`apiKeyEnvironmentVariable`, which name a Bool and two
# constants — the exclusion is applied below so the pattern here can stay
# simple and over-broad rather than clever and leaky.
SECRET_PATTERN='\bapiKey\b|\bapi_key\b|GLOMERIS_LLM_API_KEY|setApiKey|secret\(forKey'

# "<extended regex>;;<why a line may not do this to the secret>"
FORBIDDEN_WITH_SECRET=(
  '\bUserDefaults\b|\bdefaults\.(set|string|object|value)\(;;would persist the secret to a plist on disk'
  '\bprint\(|\bNSLog\(|\bos_log\b|\bdebugPrint\(|\bdump\(;;would write the secret to a log'
  '\barguments\b|"--;;would put the secret on a command line, where ps shows it (and the CLI refuses credential flags by name for exactly this reason)'
  '\bTextField\(;;would render the secret in a visible field — only SecureField may be bound to it'
  '\bwrite\(to:|\.write\(|FileManager;;would write the secret to a file'
)

scanned_lines=0
while IFS= read -r match; do
  file="${match%%:*}"
  rest="${match#*:}"
  lineno="${rest%%:*}"
  content="${rest#*:}"

  # `apiKeyAccount`, `apiKeyEnvironmentVariable` and `hasStoredApiKey` name a
  # keychain account, an environment-variable name and a Bool. None of them
  # is the secret, and the first two necessarily appear right beside the code
  # that handles it.
  stripped="${content//apiKeyAccount/}"
  stripped="${stripped//apiKeyEnvironmentVariable/}"
  stripped="${stripped//hasStoredApiKey/}"
  if ! grep -qE "$SECRET_PATTERN" <<<"$stripped"; then
    continue
  fi

  scanned_lines=$((scanned_lines + 1))

  for entry in "${FORBIDDEN_WITH_SECRET[@]}"; do
    pattern="${entry%%;;*}"
    why="${entry#*;;}"

    # A regex grep cannot compile matches nothing, and the `|| true`s in this
    # script would turn that into a silent PASS. Compile each one against
    # empty input first: any output is grep complaining.
    compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
    if [[ -n "$compile_error" ]]; then
      echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
      echo "    grep said: ${compile_error}"
      exit 1
    fi

    if grep -qE "$pattern" <<<"$stripped"; then
      fail "${file#"${REPO_ROOT}"/}:${lineno}: ${why}: ${content}"
    fi
  done
done < <(
  grep -rn --include='*.swift' -E "$SECRET_PATTERN" "$abs_sources" \
    | grep -vE ':[0-9]+:[[:space:]]*(//|\*|/\*)' \
    || true
)

# The sweep above is the whole point of this script, so a version of it that
# examines nothing must be a failure rather than a PASS. There is always at
# least `setApiKey` in the settings store.
if [[ "$scanned_lines" -eq 0 ]]; then
  echo "FAIL: found no lines handling the API key under ${SOURCES_DIR}."
  echo "Either the credential path was renamed — in which case update SECRET_PATTERN — or this"
  echo "check is now scanning nothing and reporting a pass for it."
  exit 1
fi

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} violation(s) in the BYOK credential path."
  echo "The API key belongs in the keychain and in a child process's environment. It must never be"
  echo "written to UserDefaults or a file, logged, passed as a command-line argument, or rendered in"
  echo "anything but a SecureField. See macos/GlomerisMenuBar/Sources/CredentialStore.swift."
  exit 1
fi

echo "PASS: the BYOK key is keychain-backed; ${scanned_lines} line(s) handling it persist, log, argue or display nothing."
exit 0
