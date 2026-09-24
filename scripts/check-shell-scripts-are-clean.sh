#!/usr/bin/env bash
#
# check-shell-scripts-are-clean.sh
#
# HORO-1473: the mechanical guards in this directory enforce policy separation,
# keychain handling, state namespacing, icon and project drift, version
# agreement, vocabulary parity, documentation coverage and workflow validity —
# and nothing checked the shell they are written in.
#
# HORO-1472 added workflow validation, and that guard does run shellcheck, but
# only over the shell written inline in `run:` blocks. Every one of these scripts
# is invoked from a workflow as a single line:
#
#     run: bash scripts/check-something.sh
#
# which is all shellcheck ever saw. The several hundred lines inside each script
# were opaque to it, so the guards protecting everything else were the least
# checked shell in the repository.
#
# That matters more here than in ordinary scripts, because the bug class a shell
# linter catches is the one that makes a guard pass while proving nothing. An
# expansion that word-splits, a variable read before assignment, a pipeline whose
# status is taken from the wrong stage — each turns a check into a no-op that
# reports PASS. Two guards here have already been found doing exactly that for
# other reasons (HORO-1466's unanchored greps, and an extraction that scanned
# line-number-prefixed output), so this is a demonstrated failure mode rather
# than a hypothetical one.
#
# WHAT IS CHECKED
# ---------------
# 1. shellcheck over every shell script in the repository, at the version pinned
#    in `.github/.actionlint-version`. Findings of any severity are failures:
#    `--severity=style` is passed explicitly rather than relied on as the
#    default, because this guard has no severity threshold — a threshold is a
#    decision about which findings to stop reading.
#
# 2. Every `disable=` directive has a written reason. A directive is the same
#    decision as a severity threshold taken one line at a time, and it is the way
#    this guard gets quietly neutered later, so an unexplained one fails. The
#    rule is mechanical and does not judge the reason: either the codes are
#    followed by a `#` and some text on the same line, or the line above is an
#    ordinary comment. Both forms are accepted because both are already in use
#    here — the first draft of this check accepted only the second and reported
#    two false positives against `check-app-version-matches-crate.sh`, whose
#    directives explain themselves on the line they are on.
#
# The file set is discovered rather than listed, so a new script is covered the
# moment it is written. Discovery is `git ls-files`, plus untracked-but-not-
# ignored files so a script counts before it is first committed, plus a shebang
# scan so a shell script without a `.sh` suffix cannot escape by being named
# something else.
#
# WHAT THAT COVERS, PRECISELY
# ---------------------------
# Scripts whose shebang names `sh` or `bash`. A `zsh` script would not be
# covered and must not be: shellcheck does not support that dialect. A `dash`
# shebang would also not be matched by the scan; there are none, and adding one
# should come with widening the scan and passing `-s dash`.
#
# Build output is excluded for free rather than by a prune list, because
# discovery goes through git and anything generated here is gitignored.
#
# THE SHELLCHECK BINARY IS REQUIRED, NOT OPTIONAL
# -----------------------------------------------
# Absence is a hard failure rather than a skip, for the reason HORO-1472
# measured on this repository: actionlint delegates to that binary and, when it
# is missing, does not warn, error or mention it — it exits 0 having checked no
# shell at all, reporting 0 findings where it otherwise reports 6. A guard that
# quietly loses its coverage depending on the machine it runs on is worse than no
# guard, because it reports a pass. So this script refuses to run without the
# binary, and refuses to run with a version other than the pinned one: a version
# difference means the guard cannot tell a real finding from a ruleset change.
#
# Exit 0 = every shell script is clean. Exit 1 = a finding, an unexplained
# directive, or a precondition that would have made this guard pass by checking
# nothing.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Shared with `check-workflows-are-valid.sh`, which pins the same binary for
# actionlint's integration. One file, so the two uses cannot drift apart. The
# name is the one its first consumer gave it.
VERSION_FILE=".github/.actionlint-version"

if [[ ! -f "$VERSION_FILE" ]]; then
  echo "FAIL: ${VERSION_FILE} not found — run this from a full checkout."
  exit 1
fi

# `<key>=<value>` lines only; everything else in the pin file is prose.
pinned() {
  grep -E "^$1=" "$VERSION_FILE" | head -1 | cut -d= -f2
}

shellcheck_version="$(pinned shellcheck_version)"
if [[ -z "$shellcheck_version" ]]; then
  echo "FAIL: no 'shellcheck_version=' line in ${VERSION_FILE}."
  echo "This guard compares the installed binary against that file; without it there is"
  echo "nothing to compare and the check would pass on any version."
  exit 1
fi

# ---------------------------------------------------------------------------
# The linter is present, and at the pinned version.
# ---------------------------------------------------------------------------
if ! command -v shellcheck >/dev/null 2>&1; then
  echo "FAIL: shellcheck not on PATH."
  echo "This guard does not skip when its linter is missing. A guard that reports a pass"
  echo "it did not earn is worse than one that is absent — see this script's header for"
  echo "the measurement that motivated the rule."
  echo "Install the pinned version ${shellcheck_version} — see ${VERSION_FILE}:"
  echo "  brew install shellcheck   # then check 'shellcheck --version'"
  exit 1
fi

# `shellcheck --version` prints a 'version: 0.11.0' line.
shellcheck_actual="$(shellcheck --version 2>/dev/null | grep -E '^version:' | head -1 | awk '{print $2}')"
if [[ "$shellcheck_actual" != "$shellcheck_version" ]]; then
  echo "FAIL: shellcheck version mismatch — this guard cannot tell a real finding from a ruleset difference."
  echo "  pinned (${VERSION_FILE}): ${shellcheck_version}"
  echo "  on PATH:                  ${shellcheck_actual:-unknown}"
  echo ""
  echo "Install ${shellcheck_version}, or — if the upgrade is intentional — bump ${VERSION_FILE}"
  echo "(version AND sha256) and fix whatever the new ruleset finds in the same commit."
  exit 1
fi

# ---------------------------------------------------------------------------
# Discover the scripts.
# ---------------------------------------------------------------------------
if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "FAIL: not inside a git checkout, so the file set cannot be discovered."
  echo "Linting a guessed subset would report a pass over whatever it happened to find."
  exit 1
fi

by_suffix() {
  git ls-files -- '*.sh' '*.bash'
  git ls-files --others --exclude-standard -- '*.sh' '*.bash'
}

# Shell scripts that do not advertise themselves with a suffix. `git grep -I`
# skips binaries; the shebang has to be on the first line, which also drops the
# documentation and fixtures that merely quote one.
by_shebang() {
  local candidate
  while IFS= read -r candidate; do
    [[ -f "$candidate" ]] || continue
    if head -1 "$candidate" | grep -qE '^#!.*[/ ](ba)?sh([[:space:]]|$)'; then
      echo "$candidate"
    fi
  done < <(
    git grep -I -l -E '^#!.*[/ ](ba)?sh([[:space:]]|$)' -- ':!*.sh' ':!*.bash' 2>/dev/null || true
  )
}

all_scripts=()
while IFS= read -r found; do
  all_scripts+=("$found")
done < <( { by_suffix; by_shebang; } | LC_ALL=C sort -u )

if [[ ${#all_scripts[@]} -eq 0 ]]; then
  echo "FAIL: discovered no shell scripts at all."
  echo "Passing by linting nothing is the one outcome this guard must never produce."
  exit 1
fi

# The guard scripts are the reason this file exists, so an empty `scripts/` share
# means discovery broke rather than that the repository has no shell in it.
in_scripts_dir=0
for script in "${all_scripts[@]}"; do
  [[ "$script" == scripts/* ]] && in_scripts_dir=$((in_scripts_dir + 1))
done
if [[ "$in_scripts_dir" -eq 0 ]]; then
  echo "FAIL: discovered ${#all_scripts[@]} shell script(s), none of them under scripts/."
  echo "The guard scripts are what this check exists for; finding none of them means the"
  echo "discovery above is broken, not that they are clean."
  exit 1
fi

# ---------------------------------------------------------------------------
# Every suppression is explained.
# ---------------------------------------------------------------------------
# Matching on the directive keyword rather than on the tool's name at the start
# of a line, because a comment line that begins with that name is itself read as
# a directive addressed to the tool, and an English sentence is not a valid one.
DIRECTIVE_RE='^[[:space:]]*#[[:space:]]*shellcheck[[:space:]]+disable='

# A reason on the directive line itself: the codes, then a `#`, then something.
TRAILING_REASON_RE='disable=[^[:space:]]+[[:space:]]+#[[:space:]]*[^[:space:]]'

unexplained=()
while IFS= read -r hit; do
  [[ -n "$hit" ]] || continue
  hit_file="${hit%%:*}"
  hit_rest="${hit#*:}"
  hit_line="${hit_rest%%:*}"
  hit_text="${hit_rest#*:}"

  [[ "$hit_text" =~ $TRAILING_REASON_RE ]] && continue

  if [[ "$hit_line" -le 1 ]]; then
    unexplained+=("${hit_file}:${hit_line} (no reason on the line, nothing above it)")
    continue
  fi

  previous="$(sed -n "$((hit_line - 1))p" "$hit_file")"
  if [[ ! "$previous" =~ ^[[:space:]]*# ]]; then
    unexplained+=("${hit_file}:${hit_line} (no reason on the line, and the line above is not a comment)")
  elif [[ "$previous" =~ ^#! ]]; then
    # The shebang matches "starts with #" and explains nothing. Measured: without
    # this branch, a file-wide directive pasted directly under the shebang — the
    # most far-reaching placement there is — was accepted as explained by the
    # shebang above it.
    unexplained+=("${hit_file}:${hit_line} (no reason on the line, and the line above is the shebang)")
  elif [[ "$previous" =~ $DIRECTIVE_RE ]]; then
    unexplained+=("${hit_file}:${hit_line} (no reason on the line, and the line above is another directive)")
  fi
done < <(grep -n -E "$DIRECTIVE_RE" "${all_scripts[@]}" /dev/null || true)

if [[ ${#unexplained[@]} -gt 0 ]]; then
  echo "FAIL: ${#unexplained[@]} suppression(s) with no written reason:"
  printf '  %s\n' "${unexplained[@]}"
  echo ""
  echo "Put the reason in a comment immediately above the directive. A suppression is the"
  echo "same decision as a severity threshold taken one line at a time, and an unexplained"
  echo "one is how this guard stops finding anything."
  exit 1
fi

# ---------------------------------------------------------------------------
# Lint.
# ---------------------------------------------------------------------------
echo "shellcheck ${shellcheck_version}, no severity threshold:"
printf '  %s\n' "${all_scripts[@]}"
echo ""

rc=0
shellcheck --severity=style --color=never "${all_scripts[@]}" || rc=$?

# Exit status 1 means findings and 2 means a file could not be processed. Those
# must not be reported as the same thing: a linter that could not read a script
# has not found it to be clean. (This comment does not open with the tool's name
# on purpose — such a line is parsed as a directive addressed to it, which is
# what the guard caught in this file's own first two drafts.)
if [[ "$rc" -gt 1 ]]; then
  echo ""
  echo "FAIL: shellcheck itself failed with exit status ${rc}."
  echo "That is not a clean result — at least one script was not read. Fix the invocation."
  exit 1
fi

if [[ "$rc" -ne 0 ]]; then
  echo ""
  echo "FAIL: shellcheck reported problems in the scripts above."
  echo ""
  echo "Fix them rather than suppressing them. These scripts are the repository's guards:"
  echo "a quoting bug in one does not make it fail, it makes it pass over something it was"
  echo "supposed to catch."
  echo ""
  echo "Reproduce locally with:"
  echo "  bash scripts/check-shell-scripts-are-clean.sh"
  exit 1
fi

echo "PASS: ${#all_scripts[@]} shell script(s) clean at every severity, ${in_scripts_dir} of them under scripts/."
exit 0
