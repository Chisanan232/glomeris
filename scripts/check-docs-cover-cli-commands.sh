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
#   - a subcommand declared in COMMANDS that its own command's section never
#     mentions -> a verb ships with nowhere to look it up. Added by HORO-1485,
#     which gave each verb its own safety label: a verb is now a documented
#     surface in its own right, and `daemon run` writing two files while the
#     book described only `install`/`uninstall`/`status` is the shape of
#     omission this catches.
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

# The `name:` field of every CommandSpec, matched at exactly the indentation a
# CommandSpec field sits at. Indentation is load-bearing here: HORO-1485 gave
# each command a `subcommands:` list whose SubcommandSpecs have a `name:` field
# too, four levels in, so `[[:space:]]+` began reading `install`, `uninstall`,
# `show` and the rest as top-level commands and demanding a book section for
# each. OptionSpec and ExampleSpec use different field names entirely and were
# never a problem.
#
#     pub const COMMANDS: &[CommandSpec] = &[
#         CommandSpec {
#             name: "daemon",              <- 8 spaces, a command
#             subcommands: &[
#                 SubcommandSpec {
#                     name: "install",     <- 16 spaces, a verb
commands="$(
  grep -oE '^ {8}name: "[a-z-]+",' "${REPO_ROOT}/${HELP_FILE}" \
    | sed -E 's/.*name: "([a-z-]+)",/\1/' \
    | sort -u \
    || true
)"

# Section headings of the form:  ## `glomeris <name> ...`
# `<name>` is the first word after `glomeris`, which is why the trailing
# flag/argument text in those headings does not need to be parsed.
documented="$(
  grep -oE '^## `glomeris [a-z-]+' "${REPO_ROOT}/${DOC_FILE}" \
    | sed -E 's/^## `glomeris ([a-z-]+)/\1/' \
    | sort -u \
    || true
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

# Every declared subcommand, as `command<TAB>verb` pairs, read from the same
# indentation the extraction above relies on: an 8-space `name:` opens a
# command, and each 16-space `name:` under it is one of its verbs.
pairs="$(
  awk '
    /^ {8}name: "[a-z-]+",$/ {
      match($0, /"[a-z-]+"/)
      command = substr($0, RSTART + 1, RLENGTH - 2)
      next
    }
    /^ {16}name: "[a-z-]+",$/ {
      if (command == "") next
      match($0, /"[a-z-]+"/)
      printf "%s\t%s\n", command, substr($0, RSTART + 1, RLENGTH - 2)
    }
  ' "${REPO_ROOT}/${HELP_FILE}"
)"

# Each command's section runs from its own heading to the next `## ` heading.
# A verb counts as documented if it appears in that section as a whole word,
# which is as strict as this check can be without asserting on prose: what is
# required is somewhere to look the verb up, not a particular sentence.
verb_count=0
while IFS=$'\t' read -r command verb; do
  [[ -n "$command" ]] || continue
  verb_count=$((verb_count + 1))
  section="$(
    awk -v cmd="$command" '
      $0 ~ "^## `glomeris " cmd "( |`)" { inside = 1; next }
      inside && /^## / { exit }
      inside { print }
    ' "${REPO_ROOT}/${DOC_FILE}"
  )"
  if [[ -z "$section" ]]; then
    echo "FAIL: ${DOC_FILE} has no section body for 'glomeris ${command}', so its subcommands cannot be documented in it."
    status=1
    continue
  fi
  if ! grep -qE "(^|[^a-z-])${verb}([^a-z-]|\$)" <<<"$section"; then
    echo "FAIL: 'glomeris ${command} ${verb}' ships but the '${command}' section of ${DOC_FILE} never mentions '${verb}'."
    status=1
  fi
done <<<"$pairs"

if [[ "$verb_count" -eq 0 ]]; then
  echo "FAIL: extracted no subcommands from ${HELP_FILE} — the CLI has verbs, so this half of the check would pass vacuously."
  status=1
fi

if [[ "$status" -eq 0 ]]; then
  count="$(echo "$commands" | wc -l | tr -d ' ')"
  echo "PASS: all ${count} commands in ${HELP_FILE} have a section in ${DOC_FILE}, and all ${verb_count} declared subcommands are mentioned in theirs."
fi

exit "$status"
