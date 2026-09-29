#!/usr/bin/env bash
#
# check-external-context-is-read-only.sh
#
# HORO-1546 mechanical CI guard: the optional external-context providers —
# GitHub pull-request state and Jira work-item state, read as supporting
# evidence for ranking a stale-looking working tree — must remain incapable of
# changing anything on either side.
#
# Two sides, and both matter. Outward: campaign §10 and §11 say no GitHub
# mutation and no Jira mutation belong in this feature, and a Jira transition
# posted by a storage-cleanup tool is not a bug a user can undo by deleting a
# file. Inward: this is the one module in the repository that holds a
# credential and talks to the network, so it is also the one where a stray
# `println!` writes a token into a terminal, a CI log or a pasted bug report.
#
# The corresponding integration tests already prove that a provider *failure*
# is not an absence and that a merged pull request grants no authority. What
# they cannot prove is that no future code path mutates: a test only covers
# the calls a fixture makes. So this guard constrains the module by shape
# instead — a module that cannot name a mutating verb cannot issue one,
# whatever anybody adds to it later and whatever the tests happen to reach.
#
# Six checks:
#
#   1. No mutating HTTP verb anywhere in the module. Not `POST`, `PUT`,
#      `PATCH` or `DELETE` as a method string, and not `.post(`, `.put(`,
#      `.patch(` or `.delete(` as a call. This is AC 2 stated as a property of
#      the source rather than as a claim about the two adapters that exist
#      today.
#
#   2. `ureq` may be named in `http.rs` and nowhere else. One transport
#      chokepoint: the adapters are handed a `ReadOnlyHttp` and cannot reach
#      past it, so check 1 only has to hold for one file in order to hold for
#      the module. An adapter that constructed its own request would make
#      check 1 the only thing standing between it and a mutation, and check 1
#      is a list of verbs somebody could get creative about.
#
#   3. `ReadOnlyHttp` declares exactly one method, and it is `get_json`. The
#      trait is the entire vocabulary the adapters have for reaching the
#      network. A second method added to it is how "read-only by
#      construction" quietly becomes "read-only by convention", and the
#      count is the only way to notice.
#
#   4. The module's production code does not print. A credential is the one
#      value in this repository that must never reach stdout, stderr, a log or
#      a debug format, and the module that holds one has no legitimate reason
#      to write a line anywhere — it returns typed errors, and the surfaces
#      above it do the printing. `#[cfg(test)]` code is exempt: a test
#      asserting on a message is not a leak.
#
#   5. The module's production code does not write to the filesystem. It reads
#      a configuration file and it reads git output; a cache of remote answers
#      written to disk would put a pull request's state somewhere no
#      `external-context` preview describes and no policy revalidation knows
#      about. Test fixtures that build scratch repositories are exempt for the
#      same reason as above.
#
#   6. Non-vacuity. Every check above passes over a deleted module, a stub, or
#      a `strip_comments` that started eating code. Require the transport, its
#      one method, the real GET call and both adapters to still be there.
#
# Exit 0 = pass. Exit 1 = fail, with file:line detail on stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

EXTERNAL_DIR="src/workspace/external"
abs_external="${REPO_ROOT}/${EXTERNAL_DIR}"
HTTP_FILE="${EXTERNAL_DIR}/http.rs"
abs_http="${REPO_ROOT}/${HTTP_FILE}"

if [[ ! -d "$abs_external" ]]; then
  # Not skippable. The module's absence would mean the providers moved
  # somewhere this script no longer watches, which is the regression rather
  # than the absence of one.
  echo "FAIL: ${EXTERNAL_DIR}/ is missing."
  echo "HORO-1546's optional providers live there precisely so they can be held to one read-only transport."
  echo "If they moved, update EXTERNAL_DIR in this script to match."
  exit 1
fi

if [[ ! -f "$abs_http" ]]; then
  echo "FAIL: ${HTTP_FILE} is missing."
  echo "It is the module's only transport. If it moved, update HTTP_FILE."
  exit 1
fi

# These files document at length why they must not do these things, and naming
# `POST` in that prose is the explanation rather than the act. `-vE` drops
# whole-line `//` and `///` comments after `grep -n` has prefixed the number.
strip_comments() {
  grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' || true
}

# Numbered production lines only — everything before the first `#[cfg(test)]`.
# Used by checks 4 and 5, where the test code legitimately does the thing the
# production code may not.
production_lines() {
  awk '/^#\[cfg\(test\)\]/ { exit } { print NR ":" $0 }' "$1"
}

# A pattern grep cannot compile matches nothing, and `strip_comments`'s
# `|| true` would turn that into a silent PASS.
require_compilable() {
  local pattern="$1"
  local compile_error
  compile_error="$(grep -E "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi
}

violations=0

# ---------------------------------------------------------------------------
# Check 1: no mutating HTTP verb, anywhere in the module.
#
# Each entry is "<extended regex>;;<what naming it would mean>".
# ---------------------------------------------------------------------------

MUTATING_VERBS=(
  '"POST";;names the HTTP verb that creates'
  '"PUT";;names the HTTP verb that replaces'
  '"PATCH";;names the HTTP verb that modifies'
  '"DELETE";;names the HTTP verb that deletes'
  '\.post\(;;issues a POST'
  '\.put\(;;issues a PUT'
  '\.patch\(;;issues a PATCH'
  '\.delete\(;;issues a DELETE'
  '\.method\(;;chooses a verb at run time, which means the verb is no longer fixed at GET'
)

for entry in "${MUTATING_VERBS[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"
  require_compilable "$pattern"

  while IFS= read -r -d '' file; do
    rel_file="${file#"${REPO_ROOT}"/}"
    while IFS= read -r match; do
      [[ -n "$match" ]] || continue
      echo "VIOLATION: ${rel_file}:${match%%:*}: ${why}"
      echo "    ${match#*:}"
      violations=$((violations + 1))
    done < <(grep -nE "$pattern" "$file" | strip_comments)
  done < <(find "$abs_external" -name '*.rs' -print0)
done

# ---------------------------------------------------------------------------
# Check 2: only the transport file may name `ureq`.
# ---------------------------------------------------------------------------

while IFS= read -r -d '' file; do
  rel_file="${file#"${REPO_ROOT}"/}"
  [[ "$rel_file" != "$HTTP_FILE" ]] || continue
  while IFS= read -r match; do
    [[ -n "$match" ]] || continue
    echo "VIOLATION: ${rel_file}:${match%%:*}: names ureq outside ${HTTP_FILE} — an adapter that builds its own request is past the one place the verb is fixed"
    echo "    ${match#*:}"
    violations=$((violations + 1))
  done < <(grep -nE '\bureq\b' "$file" | strip_comments)
done < <(find "$abs_external" -name '*.rs' -print0)

# ---------------------------------------------------------------------------
# Check 3: the transport trait has exactly one method, and it is a GET.
#
# Counted between `pub trait ReadOnlyHttp {` and the first line that closes it
# at column zero. A trait with two methods still compiles, still passes every
# other check here, and is the end of "read-only by construction".
# ---------------------------------------------------------------------------

trait_body="$(awk '
  /^pub trait ReadOnlyHttp \{/ { inside = 1; next }
  inside && /^\}/ { exit }
  inside { print }
' "$abs_http")"

if [[ -z "$trait_body" ]]; then
  echo "VIOLATION: ${HTTP_FILE}: could not find the body of 'pub trait ReadOnlyHttp' — it was renamed, reshaped or removed, and checks 2 and 3 now constrain nothing."
  violations=$((violations + 1))
else
  method_count="$(printf '%s\n' "$trait_body" | grep -cE '^[[:space:]]*fn [a-z_]+' || true)"
  if [[ "$method_count" -ne 1 ]]; then
    echo "VIOLATION: ${HTTP_FILE}: ReadOnlyHttp declares ${method_count} methods, not 1 — the transport's whole vocabulary is one GET, and a second method is where that stops being true by construction."
    printf '%s\n' "$trait_body" | grep -nE '^[[:space:]]*fn [a-z_]+' | sed 's/^/    /'
    violations=$((violations + 1))
  fi
  if ! printf '%s\n' "$trait_body" | grep -qE '^[[:space:]]*fn get_json\('; then
    echo "VIOLATION: ${HTTP_FILE}: ReadOnlyHttp's one method is not get_json — check 1's list of verbs is not a substitute for the method being a read."
    violations=$((violations + 1))
  fi
fi

# ---------------------------------------------------------------------------
# Checks 4 and 5: production code neither prints nor writes.
# ---------------------------------------------------------------------------

FORBIDDEN_IN_PRODUCTION=(
  'println!;;prints, and this is the one module holding a credential'
  'eprintln!;;prints to stderr, and this is the one module holding a credential'
  '\bprint!;;prints, and this is the one module holding a credential'
  '\beprint!;;prints to stderr, and this is the one module holding a credential'
  'dbg!;;debug-prints, which is how a token reaches a CI log'
  'fs::write;;writes a file, and a cached remote answer would sit somewhere no privacy preview describes'
  'fs::remove;;removes a path'
  'fs::create;;creates a path'
  'fs::rename;;renames a path'
  'File::create;;creates a file'
  'OpenOptions;;opens a file for something other than reading'
)

for entry in "${FORBIDDEN_IN_PRODUCTION[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"
  require_compilable "$pattern"

  while IFS= read -r -d '' file; do
    rel_file="${file#"${REPO_ROOT}"/}"
    while IFS= read -r match; do
      [[ -n "$match" ]] || continue
      echo "VIOLATION: ${rel_file}:${match%%:*}: ${why}"
      echo "    ${match#*:}"
      violations=$((violations + 1))
    done < <(production_lines "$file" | grep -E ":.*${pattern}" | strip_comments)
  done < <(find "$abs_external" -name '*.rs' -print0)
done

# ---------------------------------------------------------------------------
# Check 6: non-vacuity.
# ---------------------------------------------------------------------------

# "<file>;;<needle>;;<what its absence would mean>"
REQUIRED=(
  "${HTTP_FILE};;ureq::get(;;the real transport is gone, so checks 1 and 2 are passing over a module that no longer reaches the network at all"
  "${HTTP_FILE};;pub trait ReadOnlyHttp;;the transport abstraction is gone, and with it the reason the adapters cannot choose a verb"
  "${EXTERNAL_DIR}/github.rs;;impl PullRequestProvider;;the GitHub adapter is gone, so this guard is asserting about nothing"
  "${EXTERNAL_DIR}/jira.rs;;impl TaskProvider;;the Jira adapter is gone, so this guard is asserting about nothing"
)

for entry in "${REQUIRED[@]}"; do
  rel_file="${entry%%;;*}"
  rest="${entry#*;;}"
  needle="${rest%%;;*}"
  why="${rest#*;;}"

  if [[ ! -f "${REPO_ROOT}/${rel_file}" ]]; then
    echo "VIOLATION: ${rel_file} is missing — ${why}"
    violations=$((violations + 1))
    continue
  fi
  if ! grep -nF "$needle" "${REPO_ROOT}/${rel_file}" | strip_comments | grep -q .; then
    echo "VIOLATION: ${rel_file}: '${needle}' is gone — ${why}"
    violations=$((violations + 1))
  fi
done

if [[ "$violations" -gt 0 ]]; then
  echo ""
  echo "FAIL: found ${violations} line(s) breaking HORO-1546's read-only boundary."
  echo "The external-context providers read a pull request's state and a work item's state as"
  echo "supporting evidence. They must never mutate either service — campaign sections 10 and 11 —"
  echo "and because this is the one module that holds a credential, they must never print or persist"
  echo "anything either. See the header of src/workspace/external/mod.rs."
  exit 1
fi

echo "PASS: ${EXTERNAL_DIR}/ names no mutating HTTP verb, and only ${HTTP_FILE} names ureq."
echo "PASS: ReadOnlyHttp declares exactly one method and it is get_json."
echo "PASS: the module's production code neither prints nor writes to the filesystem."
exit 0
