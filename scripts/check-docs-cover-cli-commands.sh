#!/usr/bin/env bash
#
# check-docs-cover-cli-commands.sh
#
# HORO-1312. `book/src/cli_reference.md` is the published description of the
# command surface, and nothing made it keep up. `known_limitations.md` had
# resolved that tension by disclaiming the whole book — "if a command
# described here no longer matches src/main.rs, trust the source" — which
# turns a reference page into a suggestion.
#
# Since HORO-1311 there is exactly one command table (`COMMANDS` in
# src/cli/help.rs) and the binary renders every help surface from it, so the
# set of commands is now mechanically readable. This check requires the two
# sides to agree:
#
#   - a command in COMMANDS with no section in cli_reference.md  -> the
#     published reference is missing a command that ships
#   - a documented `## `glomeris <name>`` section with no matching entry in
#     COMMANDS -> the reference documents something that no longer exists,
#     or a typo in either place
#
# What this does NOT check: flags, exit codes, JSON shapes or prose accuracy.
# Those have no single canonical producer to diff against, and pretending
# otherwise would invite loosening this check until it passed. Help *text*
# is pinned separately and byte for byte by tests/help_golden.rs; this
# script only guarantees that every command has somewhere in the book to
# look it up.
#
# Exit 0 = the two sets match. Exit 1 = drift, or an extraction that came
# back empty (which would otherwise be a vacuous pass).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

HELP_FILE="src/cli/help.rs"
DOC_FILE="book/src/cli_reference.md"

for relative in "$HELP_FILE" "$DOC_FILE"; do
  if [[ ! -f "${REPO_ROOT}/${relative}" ]]; then
    echo "FAIL: ${relative} is missing, so there is nothing to compare."
    exit 1
  fi
done

# The `name:` field of every CommandSpec. `COMMANDS` is the only place in
# this file where `name:` appears at that indentation, and OptionSpec /
# ExampleSpec use different field names entirely.
commands="$(
  grep -oE '^[[:space:]]+name: "[a-z-]+",' "${REPO_ROOT}/${HELP_FILE}" \
    | sed -E 's/.*name: "([a-z-]+)",/\1/' \
    | sort -u
)"

# Section headings of the form:  ## `glomeris <name> ...`
# `<name>` is the first word after `glomeris`, which is why the trailing
# flag/argument text in those headings does not need to be parsed.
documented="$(
  grep -oE '^## `glomeris [a-z-]+' "${REPO_ROOT}/${DOC_FILE}" \
    | sed -E 's/^## `glomeris ([a-z-]+)/\1/' \
    | sort -u
)"

if [[ -z "$commands" ]]; then
  echo "FAIL: extracted no command names from ${HELP_FILE} — this check would pass vacuously."
  exit 1
fi
if [[ -z "$documented" ]]; then
  echo "FAIL: extracted no documented commands from ${DOC_FILE} — this check would pass vacuously."
  exit 1
fi

undocumented="$(comm -23 <(echo "$commands") <(echo "$documented"))"
unknown="$(comm -13 <(echo "$commands") <(echo "$documented"))"

status=0

if [[ -n "$undocumented" ]]; then
  echo "FAIL: these commands ship but have no section in ${DOC_FILE}:"
  while IFS= read -r name; do echo "  - glomeris ${name}"; done <<<"$undocumented"
  status=1
fi

if [[ -n "$unknown" ]]; then
  echo "FAIL: ${DOC_FILE} documents these, but they are not in ${HELP_FILE}'s COMMANDS table:"
  while IFS= read -r name; do echo "  - glomeris ${name}"; done <<<"$unknown"
  status=1
fi

if [[ "$status" -eq 0 ]]; then
  count="$(echo "$commands" | wc -l | tr -d ' ')"
  echo "PASS: all ${count} commands in ${HELP_FILE} have a section in ${DOC_FILE}."
fi

exit "$status"
