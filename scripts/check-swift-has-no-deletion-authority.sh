#!/usr/bin/env bash
#
# check-swift-has-no-deletion-authority.sh
#
# HORO-1922 mechanical CI guard for the thin-client rule: GlomerisMenuBar
# displays what the Rust CLI decided and asks the CLI to act. It must never
# delete, move, trash or truncate files itself, and must never start a shell
# or any process other than the one `glomeris` invocation point. Before this
# guard the property held only by inspection (HORO-1313 AC2 audit): a future
# `FileManager.removeItem(at: resourcePath)` would have passed CI.
#
# WHAT IS CHECKED (Sources/ only, whole-line comments stripped)
# -------------------------------------------------------------
# 1. File-mutation APIs that can destroy or displace data
#    (removeItem, trashItem, moveItem, replaceItem, unlink(), rmdir(),
#    recycle(), truncate()) appear nowhere except the allowlist below.
# 2. Process creation (`Process()`, NSTask, posix_spawn, system(), popen())
#    appears only in GlomerisClient.swift, exactly once.
# 3. No shell or deletion binary is named as a string: /bin/sh, /bin/bash,
#    /bin/zsh, /bin/rm, /usr/bin/trash, "rm", "-rf".
#
# THE ALLOWLIST (deliberately tiny)
# ---------------------------------
# RecoveryStopRequest.swift may call removeItem exactly once. It deletes the
# zero-byte stop sentinel the app itself created in its own temp directory
# when a recovery run starts; it never names a resource path. Adding a second
# site, or any other file, must be a reviewed change to THIS script, with the
# reason written here.
#
# `--self-test` plants each forbidden shape in a temporary copy of Sources/
# and requires the guard to reject it, and requires the unmodified tree and
# the legitimate sentinel site to pass. A guard that cannot fail is worthless,
# and a regex that silently stops matching is exactly how this one would rot.
#
# Exit 0 = every property holds. Exit 1 = a violation, printed with file:line.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCES_REL="macos/GlomerisMenuBar/Sources"
ALLOWED_REMOVE_FILE="RecoveryStopRequest.swift"
ALLOWED_PROCESS_FILE="GlomerisClient.swift"

FILE_MUTATION='(removeItem|trashItem|moveItem|replaceItem)[[:space:]]*\(|(^|[^.[:alnum:]_])(unlink|rmdir|truncate)[[:space:]]*\(|(^|[^[:alnum:]_])recycle[[:space:]]*\('
PROCESS_CREATION='(^|[^[:alnum:]_.])(Process[[:space:]]*\(|NSTask|posix_spawn|system[[:space:]]*\(|popen[[:space:]]*\()'
SHELL_OR_DELETER='/bin/(sh|bash|zsh|rm)([^[:alnum:]_]|$)|/usr/bin/trash|"rm"|"-rf"'

strip_comments() {
  # Whole-line comments only; keeps line numbers. Doc comments name these APIs
  # in prose to explain why they are not used, which is the explanation, not the act.
  grep -nv -E '^[[:space:]]*(//|\*|/\*)' "$1" || true
}

# check_tree <sources-dir> : prints violations, returns the count via stdout's
# last line "COUNT=<n>" so callers (and --self-test) can assert on it.
check_tree() {
  local dir="$1" violations=0 scanned=0 remove_sites=0 process_sites=0 file base body

  while IFS= read -r file; do
    scanned=$((scanned + 1))
    base="$(basename "$file")"
    body="$(strip_comments "$file")"

    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      if [[ "$base" == "$ALLOWED_REMOVE_FILE" && "$hit" =~ removeItem ]]; then
        remove_sites=$((remove_sites + 1))
        continue
      fi
      echo "VIOLATION: ${base}:${hit%%:*}: file-mutation API outside the allowlist. The app must ask the CLI to act, never delete or move files itself: ${hit#*:}"
      violations=$((violations + 1))
    done < <(grep -E "$FILE_MUTATION" <<<"$body" || true)

    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      if [[ "$base" == "$ALLOWED_PROCESS_FILE" ]]; then
        process_sites=$((process_sites + 1))
        continue
      fi
      echo "VIOLATION: ${base}:${hit%%:*}: process creation outside ${ALLOWED_PROCESS_FILE}: ${hit#*:}"
      violations=$((violations + 1))
    done < <(grep -E "$PROCESS_CREATION" <<<"$body" || true)

    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      echo "VIOLATION: ${base}:${hit%%:*}: names a shell or deletion binary: ${hit#*:}"
      violations=$((violations + 1))
    done < <(grep -E "$SHELL_OR_DELETER" <<<"$body" || true)
  done < <(find "$dir" -name '*.swift' -type f | sort)

  if [[ "$scanned" -eq 0 ]]; then
    echo "FAIL: found no Swift files under ${dir}. A scan that examines nothing passes for the wrong reason."
    echo "COUNT=1"
    return
  fi
  if [[ "$remove_sites" -gt 1 ]]; then
    echo "VIOLATION: ${ALLOWED_REMOVE_FILE} calls removeItem ${remove_sites} times; exactly one (the stop sentinel) is allowed."
    violations=$((violations + 1))
  fi
  if [[ "$process_sites" -gt 1 ]]; then
    echo "VIOLATION: ${ALLOWED_PROCESS_FILE} creates ${process_sites} processes; exactly one invocation point is allowed."
    violations=$((violations + 1))
  fi
  echo "SCANNED=${scanned}"
  echo "COUNT=${violations}"
}

run_check() {
  local dir="$1" out count
  out="$(check_tree "$dir")"
  count="$(sed -n 's/^COUNT=//p' <<<"$out")"
  grep -v -E '^(COUNT|SCANNED)=' <<<"$out" || true
  return $((count > 0 ? 1 : 0))
}

self_test() {
  local work failures=0
  work="$(mktemp -d "${TMPDIR:-/tmp}/swift-deletion-guard.XXXXXX")"
  # shellcheck disable=SC2064
  trap "rm -rf '$work'" EXIT

  fresh() {
    rm -rf "$work/src"
    mkdir -p "$work/src"
    cp "${REPO_ROOT}/${SOURCES_REL}"/*.swift "$work/src/"
  }
  expect_fail() { # name, file, line-to-append
    fresh
    printf '\n%s\n' "$3" >>"$work/src/$2"
    if run_check "$work/src" >/dev/null 2>&1; then
      echo "SELF-TEST FAIL: planted '$1' was not rejected"
      failures=$((failures + 1))
    fi
  }

  fresh
  if ! run_check "$work/src" >/dev/null 2>&1; then
    echo "SELF-TEST FAIL: the unmodified tree must pass"
    failures=$((failures + 1))
  fi

  # Each forbidden shape, planted in a file that is NOT on the allowlist.
  expect_fail "removeItem in a view" GlomerisPopoverView.swift 'let _ = try? FileManager.default.removeItem(at: URL(fileURLWithPath: "/x"))'
  expect_fail "trashItem" GlomerisPopoverView.swift 'try FileManager.default.trashItem(at: u, resultingItemURL: nil)'
  expect_fail "moveItem" GlomerisPopoverView.swift 'try FileManager.default.moveItem(at: a, to: b)'
  expect_fail "replaceItem" GlomerisPopoverView.swift 'try FileManager.default.replaceItem(at: a, withItemAt: b, backupItemName: nil, options: [], resultingItemURL: nil)'
  expect_fail "unlink" GlomerisPopoverView.swift 'unlink(path)'
  expect_fail "rmdir" GlomerisPopoverView.swift 'rmdir(path)'
  expect_fail "NSWorkspace recycle" GlomerisPopoverView.swift 'NSWorkspace.shared.recycle(urls) { _, _ in }'
  expect_fail "second Process" GlomerisPopoverView.swift 'let p = Process()'
  expect_fail "NSTask" GlomerisPopoverView.swift 'let t = NSTask()'
  expect_fail "posix_spawn" GlomerisPopoverView.swift 'posix_spawn(&pid, "/x", nil, nil, nil, nil)'
  expect_fail "C system()" GlomerisPopoverView.swift 'system("true")'
  expect_fail "shell string" GlomerisPopoverView.swift 'let sh = "/bin/sh"'
  expect_fail "rm binary" GlomerisPopoverView.swift 'let rm = "/bin/rm"'
  expect_fail "rm -rf argument" GlomerisPopoverView.swift 'let args = ["-rf", path]'
  # The allowlist is one file and one call, not a licence.
  expect_fail "removeItem in the allowlisted file, a second site" RecoveryStopRequest.swift 'try? FileManager.default.removeItem(at: other)'
  expect_fail "second Process in GlomerisClient" GlomerisClient.swift 'let again = Process()'
  expect_fail "moveItem in the allowlisted file" RecoveryStopRequest.swift 'try? FileManager.default.moveItem(at: a, to: b)'

  # Things that must NOT trip it: prose in comments and unrelated look-alikes.
  fresh
  printf '\n%s\n' '// we never call removeItem or Process() here; that is the CLI'"'"'s job' >>"$work/src/GlomerisPopoverView.swift"
  printf '%s\n' 'static let f: Font = .system(.body)' >>"$work/src/GlomerisPopoverView.swift"
  if ! run_check "$work/src" >/dev/null 2>&1; then
    echo "SELF-TEST FAIL: comments and Font.system(...) must not be flagged"
    failures=$((failures + 1))
  fi

  if [[ "$failures" -gt 0 ]]; then
    echo "FAIL: ${failures} self-test failure(s)."
    exit 1
  fi
  echo "PASS: self-test — unmodified tree passes; 17 planted violations are each rejected; comments and Font.system are not flagged."
  exit 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
fi

abs_sources="${REPO_ROOT}/${SOURCES_REL}"
if [[ ! -d "$abs_sources" ]]; then
  echo "FAIL: ${SOURCES_REL} is missing, so nothing was scanned."
  echo "If the app moved, update this script — do not delete the check."
  exit 1
fi

if run_check "$abs_sources"; then
  echo "PASS: no Swift source deletes, moves or trashes files; the only process is the glomeris invocation point; the only removeItem is the stop sentinel."
  exit 0
fi
echo ""
echo "FAIL: the menu-bar app would hold deletion authority. It must ask the Rust CLI to act."
exit 1
