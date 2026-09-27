#!/usr/bin/env bash
#
# check-app-cli-invocations-match-cli-contract.sh
#
# HORO-1501. The menu-bar app drives the Rust CLI by building argument vectors
# and spawning it. Nothing compared those vectors against the arguments the CLI
# actually accepts, and the cost of that gap was a shipped defect: the Status
# card ran
#
#     glomeris status --json --project-root <path>
#
# for every user who had configured a project root. `status` accepts no project
# root — it reports disk figures and runs no detectors — and since HORO-1322 it
# exits 2 on an unrecognised argument, so the card showed a CLI error instead of
# the disk and monitor figures it exists to show. With no roots configured the
# same code worked, which is why it passed review, tests and manual use.
#
# The Swift tests cannot close that gap on their own. They assert the app's
# table of root-scoped commands against a list retyped into the test file, so
# the one drift they cannot catch is the important one: the table and the CLI
# disagreeing. This script is the mechanical half.
#
# WHAT IS CHECKED
# ---------------
# 1. The app's table against the CLI's own. `glomeris::cli::extract_project_roots`
#    is the sole reader of `--project-root` in src/main.rs, so the command
#    handlers that call it ARE the commands that accept the flag. Set difference
#    both ways:
#      - a command in `GlomerisCliProjectRootScope.rootScopedCommands` whose Rust
#        handler does not call it => the app attaches a flag the CLI rejects.
#        This is exactly HORO-1501, caught at the table instead of in a view.
#      - a Rust handler that does call it and is not in the Swift table => either
#        a command the app should now scope, or one of the three deliberate
#        omissions declared below. A new one fails, so the decision is taken
#        rather than defaulted.
#
# 2. One formatter, and it consults the table. The literal `--project-root` may
#    appear in exactly one executable line of the Swift sources: inside
#    `ProjectRootsStore.projectRootArguments(forCommand:)`, whose body must
#    consult `GlomerisCliProjectRootScope`. Any other occurrence is a call site
#    formatting the flag by hand, which is the defect with the table bypassed.
#
# 3. Root-scoped invocations still get their roots. An argument vector whose
#    leading token names a root-scoped command must route through `scoped(...)`
#    or `projectRootArguments(forCommand:)`. Losing the roots is the quiet
#    inverse of HORO-1501: `detect` would silently stop searching the folders
#    the user configured, and every report would still look healthy.
#
# 4. Every `projectRootArguments(forCommand: "X")` names a command in the table.
#    A typo there returns an empty array, so the roots vanish and nothing fails.
#
# 5. The `status` invocation specifically: it must go through the table and must
#    carry no project-root argument of any kind. This is the regression pin for
#    the reported defect, in the file the defect was in.
#
# COVERAGE — deliberately partial, and it says so in the PASS line
# ----------------------------------------------------------------
# This checks the project-root contract, not every flag of every command. The
# general form — extract each argv literal, extract each command's accepted flags
# from the Rust handler, diff — is a larger job than this ticket, and a half-built
# version of it would produce false failures on the flags whose handling is
# genuinely per-command (`--target`, `--kinds`, `--confirm-ask`). What is checked
# is the one flag with a shared producer on both sides, which is what makes the
# comparison honest rather than approximate.
#
# Swift `//` comments are ignored; `/* */` blocks are not recognised and none of
# these files use them.
#
# Exit 0 = the app's invocations match the CLI's project-root contract.
# Exit 1 = drift, or an extraction that came back empty — which would otherwise
# be a vacuous pass, and is the way this guard would be lost.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  echo "usage: $(basename "$0") [--self-test]"
  echo ""
  echo "  (no arguments)  Checks this repository."
  echo "  --self-test     Runs every rule against throwaway fixtures, proving each"
  echo "                  one fires. See the block comment above run_self_test."
}

self_test=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --self-test)
      self_test=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1"
      usage
      exit 2
      ;;
  esac
done

# `<rust handler fn>;;<cli command>`
#
# The handler is the function in src/main.rs that owns the subcommand; the
# command is the token the app would put first in its argument vector. Both
# halves are needed: the Rust side is discovered by finding which of these
# functions calls `extract_project_roots`, and the CLI token is what the Swift
# table is written in.
#
# A handler that calls `extract_project_roots` and is NOT listed here fails the
# run rather than being skipped — an unmapped handler is a command whose
# project-root support nobody has considered from the app's side.
HANDLER_COMMANDS=(
  'run_detect_command;;detect'
  'run_explain_command;;explain'
  'run_clean_command;;clean'
  'run_llm_plan_command;;llm-plan'
  'run_execute_command;;execute'
  'run_free_command;;free'
  'run_status_command;;status'
  'run_llm_check_command;;llm-check'
  'run_history_command;;history'
  'run_emergency_command;;emergency'
  'run_autopilot_command;;autopilot'
  'run_daemon_command;;daemon'
  'run_actions_command;;actions'
  # Subcommand handlers. `autopilot run` accepts project roots where its three
  # siblings reject extra arguments outright, so the group's own handler is not a
  # useful granularity here.
  'autopilot_run;;autopilot run'
  'autopilot_show;;autopilot show'
  'autopilot_enable;;autopilot enable'
  'autopilot_revoke;;autopilot revoke'
  'daemon_status;;daemon status'
  'actions_history;;actions history'
  'actions_list;;actions list'
)

# Commands the CLI accepts project roots on that the app deliberately does not
# scope. Each one is a decision, so each one is listed with its reason and is
# required to still be root-accepting on the Rust side — a stale entry here would
# silently excuse a real mismatch later.
#
#   clean             — the app never invokes it at all.
#   autopilot run     — the stored grant's own kinds and limits decide what it
#                       may consider; narrowing that with the roots would change
#                       a grant the user wrote down.
#
# `free` was listed here on the same grounds as `clean` until HORO-1506 gave the
# app a Recovery card that invokes it. It is now scoped, which is the outcome
# rule 1 is written to force: an omission stops being a decision the moment the
# app starts making the call.
DECLARED_OMISSIONS=(
  'clean'
  'autopilot run'
)

# --- rules -------------------------------------------------------------------
#
# Factored out so the self-test can run them against fixtures rather than
# against a second copy of the logic. A guard whose self-test exercises a
# paraphrase of the rules proves nothing about the rules.
assess() {
  local root="$1"
  local problems=0

  local rust_main="${root}/src/main.rs"
  local sources="${root}/macos/GlomerisMenuBar/Sources"
  local scope_file="${sources}/GlomerisCliProjectRootScope.swift"
  local store_file="${sources}/ProjectRootsStore.swift"
  local status_file="${sources}/StatusHealthSectionView.swift"

  local missing=0 path
  for path in "$rust_main" "$scope_file" "$store_file" "$status_file"; do
    if [[ ! -f "$path" ]]; then
      echo "FAIL: ${path#"$root"/} is missing, so the project-root contract cannot be compared."
      missing=1
    fi
  done
  if [[ ! -d "$sources" ]]; then
    echo "FAIL: ${sources#"$root"/} is missing, so there are no invocations to check."
    missing=1
  fi
  if [[ "$missing" -ne 0 ]]; then
    return 1
  fi

  # Every executable Swift line in the app sources, as "<file>:<line>:<text>".
  # Comments are dropped here rather than at each rule, because every rule below
  # is about code and three of them look for a string that this ticket wrote a
  # great many comments about.
  local swift_code
  # The line number is matched explicitly rather than looking for the first
  # `//` after the colon: several of these files contain `https://` in code, and
  # a looser pattern drops those lines as if they were comments.
  swift_code="$(
    find "$sources" -name '*.swift' -type f -print0 \
      | xargs -0 grep -Hn '' \
      | grep -vE ':[0-9]+:[[:space:]]*//' \
      || true
  )"
  if [[ -z "$swift_code" ]]; then
    echo "FAIL: no Swift code found under ${sources#"$root"/}."
    return 1
  fi

  # --- 1. the app's table against the CLI's ----------------------------------

  # The table, from `static let rootScopedCommands: Set<String> = [...]`.
  local swift_table
  swift_table="$(
    awk '
      index($0, "rootScopedCommands") > 0 { collecting = 1 }
      collecting {
        while (match($0, /"[a-z-]+"/)) {
          token = substr($0, RSTART + 1, RLENGTH - 2)
          print token
          $0 = substr($0, RSTART + RLENGTH)
        }
        if (index($0, "]") > 0) exit
      }
    ' "$scope_file" | sort -u || true
  )"
  if [[ -z "$swift_table" ]]; then
    echo "FAIL: extracted no commands from rootScopedCommands in ${scope_file#"$root"/}."
    echo "      The declaration was renamed or reshaped; fix the extraction here rather than"
    echo "      assuming the app and the CLI agree."
    return 1
  fi

  # The Rust side: which command handlers read `--project-root`. Brace counting
  # is not needed — `extract_project_roots` is called at the top of each handler
  # and handlers are top-level `fn`s — but the nearest preceding `fn` line is
  # tracked rather than assumed, so a helper introduced between two handlers
  # cannot silently attribute its call to the handler above it.
  local rust_handlers
  rust_handlers="$(
    awk '
      /^(pub )?fn [a-z_]+/ {
        line = $0
        sub(/^(pub )?fn /, "", line)
        sub(/[(<].*$/, "", line)
        current = line
      }
      index($0, "extract_project_roots(") > 0 && current != "" { print current }
    ' "$rust_main" | sort -u || true
  )"
  if [[ -z "$rust_handlers" ]]; then
    echo "FAIL: found no call to extract_project_roots() in ${rust_main#"$root"/}."
    echo "      Either the CLI stopped accepting --project-root, or this extraction broke."
    echo "      Reporting agreement from an empty set is the vacuous pass this guard avoids."
    return 1
  fi

  local rust_commands="" handler mapped
  while IFS= read -r handler; do
    [[ -z "$handler" ]] && continue
    mapped=""
    local entry
    for entry in "${HANDLER_COMMANDS[@]}"; do
      if [[ "${entry%%;;*}" == "$handler" ]]; then
        mapped="${entry#*;;}"
        break
      fi
    done
    if [[ -z "$mapped" ]]; then
      echo "VIOLATION: ${handler}() in src/main.rs reads --project-root and this guard has no"
      echo "    CLI command mapped to it. Add it to HANDLER_COMMANDS, and decide whether the"
      echo "    app should scope that command to the configured roots."
      problems=$((problems + 1))
      continue
    fi
    rust_commands+="${mapped}"$'\n'
  done <<< "$rust_handlers"

  rust_commands="$(printf '%s' "$rust_commands" | grep -v '^$' | sort -u || true)"
  if [[ -z "$rust_commands" ]]; then
    echo "FAIL: no Rust handler that reads --project-root could be mapped to a CLI command."
    return 1
  fi

  local omitted
  omitted="$(printf '%s\n' "${DECLARED_OMISSIONS[@]}" | sort -u)"

  # The app attaches roots to a command the CLI does not accept them on. This is
  # the HORO-1501 class, at the table.
  local rejected
  rejected="$(comm -23 <(echo "$swift_table") <(echo "$rust_commands") || true)"
  if [[ -n "$rejected" ]]; then
    echo "VIOLATION: GlomerisCliProjectRootScope scopes commands the CLI rejects --project-root on:"
    while IFS= read -r command; do echo "    ${command}"; done <<< "$rejected"
    echo "    No handler for these calls extract_project_roots(), so the CLI exits 2 on the flag"
    echo "    and every user with a configured root gets an error instead of a result."
    problems=$((problems + 1))
  fi

  # The CLI accepts roots somewhere the app neither scopes nor declares.
  local unaccounted
  unaccounted="$(comm -13 <(echo "$swift_table") <(echo "$rust_commands") | comm -23 - <(echo "$omitted") || true)"
  if [[ -n "$unaccounted" ]]; then
    echo "VIOLATION: the CLI accepts --project-root on commands the app neither scopes nor declares:"
    while IFS= read -r command; do echo "    ${command}"; done <<< "$unaccounted"
    echo "    Add it to rootScopedCommands, or to DECLARED_OMISSIONS in this script with the"
    echo "    reason it gets no roots. Silence would leave a user's configured folders ignored."
    problems=$((problems + 1))
  fi

  # A declared omission that is no longer root-accepting is a stale excuse.
  local stale
  stale="$(comm -23 <(echo "$omitted") <(echo "$rust_commands") || true)"
  if [[ -n "$stale" ]]; then
    echo "VIOLATION: DECLARED_OMISSIONS names commands the CLI no longer accepts --project-root on:"
    while IFS= read -r command; do echo "    ${command}"; done <<< "$stale"
    echo "    Remove them. A stale entry excuses a mismatch this guard should report."
    problems=$((problems + 1))
  fi

  if [[ "$problems" -eq 0 ]]; then
    local table_count rust_count
    table_count="$(echo "$swift_table" | wc -l | tr -d ' ')"
    rust_count="$(echo "$rust_commands" | wc -l | tr -d ' ')"
    echo "ok: ${table_count} scoped command(s) are a subset of the ${rust_count} the CLI accepts,"
    echo "    and the difference is exactly the ${#DECLARED_OMISSIONS[@]} declared omission(s)."
  fi

  # --- 2. one formatter, and it consults the table ---------------------------

  local flag_lines
  flag_lines="$(echo "$swift_code" | grep -F -- '"--project-root"' || true)"
  local stray
  stray="$(echo "$flag_lines" | grep -v '/ProjectRootsStore.swift:' || true)"
  if [[ -n "$stray" ]]; then
    echo "VIOLATION: --project-root is formatted outside ProjectRootsStore:"
    while IFS= read -r line; do echo "    ${line#"$root"/}"; done <<< "$stray"
    echo "    A call site that writes the flag itself has bypassed the one table that knows"
    echo "    which commands accept it, which is how the Status card came to send it to status."
    problems=$((problems + 1))
  fi

  local formatter_count
  formatter_count="$(echo "$flag_lines" | grep -c '/ProjectRootsStore.swift:' || true)"
  if [[ "$formatter_count" -ne 1 ]]; then
    echo "VIOLATION: expected exactly one line formatting --project-root in ProjectRootsStore.swift,"
    echo "    found ${formatter_count}. Zero means nothing formats the flag and every 'no roots"
    echo "    attached' assertion below is vacuous; more than one means two formatters to keep"
    echo "    in step."
    problems=$((problems + 1))
  fi

  # The formatter's own function must consult the table. Extracted by brace
  # depth from the declaration, so a second function added below it cannot
  # satisfy this on the first one's behalf.
  local formatter_body
  formatter_body="$(
    awk '
      index($0, "func projectRootArguments(forCommand") > 0 { started = 1 }
      started {
        print
        n = gsub(/\{/, "{"); depth += n
        n = gsub(/\}/, "}"); depth -= n
        if (depth <= 0 && index($0, "}") > 0) exit
      }
    ' "$store_file" || true
  )"
  if [[ -z "$formatter_body" ]]; then
    echo "VIOLATION: ProjectRootsStore has no projectRootArguments(forCommand:) — the only"
    echo "    function permitted to format --project-root, and the one every call site is"
    echo "    required below to route through."
    problems=$((problems + 1))
  elif ! grep -qF 'GlomerisCliProjectRootScope' <<< "$formatter_body"; then
    echo "VIOLATION: projectRootArguments(forCommand:) does not consult GlomerisCliProjectRootScope."
    echo "    It takes a command name and must use it; formatting the roots for whatever it is"
    echo "    handed is the unconditional property HORO-1501 removed, with an argument added."
    problems=$((problems + 1))
  elif ! grep -qF -- '"--project-root"' <<< "$formatter_body"; then
    echo "VIOLATION: projectRootArguments(forCommand:) no longer formats --project-root, so the"
    echo "    flag is being produced somewhere this guard does not know about."
    problems=$((problems + 1))
  fi

  # --- 3. root-scoped invocations still get their roots ----------------------
  #
  # An argv literal is recognised by its leading token: `["detect", ...`. The
  # roots may be attached on the same line — `scoped(["detect", ...])` — or by a
  # builder the vector is passed to, so a window of following lines is searched
  # for the store's two entry points before a site is called bare.
  local scoped_sites=0 command
  while IFS= read -r command; do
    [[ -z "$command" ]] && continue
    local sites site
    sites="$(echo "$swift_code" | grep -E "\[[[:space:]]*\"${command}\"" || true)"
    [[ -z "$sites" ]] && continue
    while IFS= read -r site; do
      [[ -z "$site" ]] && continue
      local file line_no
      file="${site%%:*}"
      line_no="$(echo "${site#"$file":}" | cut -d: -f1)"
      local window
      window="$(sed -n "${line_no},$((line_no + 8))p" "$file" || true)"
      if grep -qE 'scoped\(|projectRootArguments|projectRootsArguments' <<< "$window"; then
        scoped_sites=$((scoped_sites + 1))
        continue
      fi
      # A literal that is a fixture or an expectation rather than an invocation
      # has no store in reach; those live in Tests, which is not scanned. In
      # Sources a bare root-scoped vector is a call site that lost its roots.
      echo "VIOLATION: ${file#"$root"/}:${line_no} builds a \`${command}\` vector with no project"
      echo "    roots in reach. ${command} is root-scoped, so the user's configured folders would"
      echo "    be silently ignored here — the quiet inverse of HORO-1501."
      problems=$((problems + 1))
    done <<< "$sites"
  done <<< "$swift_table"

  if [[ "$scoped_sites" -eq 0 ]]; then
    echo "VIOLATION: found no invocation of any root-scoped command in the app sources."
    echo "    Either the extraction broke or the app no longer runs detect/explain/llm-plan/"
    echo "    execute at all. Either way this guard is checking nothing."
    problems=$((problems + 1))
  fi

  # --- 4. every forCommand: literal names a command in the table -------------

  local named_commands
  # `forCommand:` alone, not `projectRootArguments(forCommand:`: one of the two
  # call sites wraps its argument onto the following line, and a pattern that
  # required the call and its label on one line would silently skip it.
  named_commands="$(
    echo "$swift_code" \
      | grep -oE 'forCommand: "[a-z-]+"' \
      | grep -oE '"[a-z-]+"' \
      | tr -d '"' \
      | sort -u \
      || true
  )"
  if [[ -z "$named_commands" ]]; then
    echo "VIOLATION: no call site names a command through projectRootArguments(forCommand:)."
    echo "    The two mid-vector sites — execute and the llm-plan payload preview — use it, so"
    echo "    an empty extraction means it broke or those sites stopped passing roots."
    problems=$((problems + 1))
  else
    local not_in_table
    not_in_table="$(comm -23 <(echo "$named_commands") <(echo "$swift_table") || true)"
    if [[ -n "$not_in_table" ]]; then
      echo "VIOLATION: projectRootArguments(forCommand:) is called with commands the table does"
      echo "    not scope:"
      while IFS= read -r command; do echo "    ${command}"; done <<< "$not_in_table"
      echo "    That call returns an empty array, so the roots vanish and nothing fails. A typo"
      echo "    here reads exactly like working code."
      problems=$((problems + 1))
    fi
  fi

  # --- 5. the status invocation, pinned in the file the defect was in --------

  local status_sites
  status_sites="$(grep -n '\[[[:space:]]*"status"' "$status_file" | grep -v '^[0-9]*:[[:space:]]*//' || true)"
  if [[ -z "$status_sites" ]]; then
    echo "VIOLATION: no \`status\` argument vector found in StatusHealthSectionView.swift, so the"
    echo "    regression this guard exists for cannot be pinned. If the invocation moved, move"
    echo "    this rule with it."
    problems=$((problems + 1))
  else
    local status_line
    while IFS= read -r status_line; do
      [[ -z "$status_line" ]] && continue
      if grep -qE -- '--project-root|commandLineArguments|projectRootArguments' <<< "$status_line"; then
        echo "VIOLATION: StatusHealthSectionView.swift:${status_line%%:*} attaches project roots to"
        echo "    \`status\`. That is HORO-1501 exactly: the command accepts no project root and"
        echo "    exits 2 when given one, so the card fails for every user who configured a root."
        problems=$((problems + 1))
      fi
      if ! grep -qF 'scoped(' <<< "$status_line"; then
        echo "VIOLATION: StatusHealthSectionView.swift:${status_line%%:*} builds the \`status\` vector"
        echo "    without going through ProjectRootsStore.scoped(). The vector is the same either"
        echo "    way; routing it through the table is what keeps the next edit to this line from"
        echo "    having to remember that status takes no roots."
        problems=$((problems + 1))
      fi
    done <<< "$status_sites"
  fi

  [[ "$problems" -eq 0 ]]
}

# --- self-test ---------------------------------------------------------------
#
# Every VIOLATION above, fired against a disposable copy of this repository's
# own files. The reason this exists is the reason the guard exists: a check on
# invocation shape is easy to write in a way that passes on everything, and the
# defect it is meant to catch appears once every few months. A failing case here
# is the only evidence that a rule fires at all.
run_self_test() {
  local work
  work="$(mktemp -d)"
  # shellcheck disable=SC2064  # $work must expand now, not at trap time.
  trap "rm -rf '$work'" RETURN

  local cases_run=0 cases_failed=0

  # A copy of the real sources under $work/<name>, ready to be mutated.
  fixture() {
    local name="$1"
    local dir="${work}/${name}"
    mkdir -p "${dir}/src" "${dir}/macos/GlomerisMenuBar"
    cp "${REPO_ROOT}/src/main.rs" "${dir}/src/main.rs"
    cp -R "${REPO_ROOT}/macos/GlomerisMenuBar/Sources" "${dir}/macos/GlomerisMenuBar/Sources"
    printf '%s' "$dir"
  }

  quoted() {
    local line
    while IFS= read -r line; do
      printf '      %s\n' "$line"
    done <<< "$1"
  }

  # $1 = description, $2 = expected exit, $3 = fixture dir, $4 = phrase the
  # output must contain on one line.
  expect() {
    local what="$1" want="$2" dir="$3" phrase="$4"
    cases_run=$((cases_run + 1))
    local out status=0
    out="$(assess "$dir" 2>&1)" || status=1
    if [[ "$status" -ne "$want" ]]; then
      echo "  SELF-TEST FAIL: ${what}: expected exit ${want}, got ${status}"
      quoted "$out"
      cases_failed=$((cases_failed + 1))
      return 0
    fi
    if ! grep -qF "$phrase" <<< "$out"; then
      echo "  SELF-TEST FAIL: ${what}: exit ${status} was right but the output does not say why."
      echo "      expected to contain: ${phrase}"
      quoted "$out"
      cases_failed=$((cases_failed + 1))
      return 0
    fi
    echo "  ok: ${what}"
  }

  local dir scope store status_view candidates

  # An unmutated copy passes. Without this case every rule below could be
  # firing on the copy itself rather than on the mutation.
  dir="$(fixture pristine)"
  expect "an unmutated copy of the sources passes" 0 "$dir" \
    "declared omission(s)."

  # 1. The reported defect, reintroduced at the table: `status` scoped.
  dir="$(fixture table-scopes-status)"
  scope="${dir}/macos/GlomerisMenuBar/Sources/GlomerisCliProjectRootScope.swift"
  perl -0pi -e 's/(static let rootScopedCommands: Set<String> = \[\n)/$1        "status",\n/' "$scope"
  expect "scoping a command the CLI rejects the flag on fails" 1 "$dir" \
    "scopes commands the CLI rejects --project-root on"

  # 1b. A root-accepting CLI command that is neither scoped nor declared.
  dir="$(fixture undeclared-rust-command)"
  perl -0pi -e 's/^fn run_history_command\(args: &\[String\]\) \{\n/fn run_history_command(args: &[String]) {\n    let (_roots, _rest) = match glomeris::cli::extract_project_roots(args) { Ok(v) => v, Err(_) => return };\n/m' \
    "${dir}/src/main.rs"
  expect "a newly root-accepting CLI command must be scoped or declared" 1 "$dir" \
    "neither scopes nor declares"

  # 2. A call site formatting the flag itself.
  dir="$(fixture hand-written-flag)"
  status_view="${dir}/macos/GlomerisMenuBar/Sources/StatusHealthSectionView.swift"
  perl -0pi -e 's/projectRootsStore\.scoped\(\["status", "--json"\]\)/["status", "--json", "--project-root", "\/tmp"]/' "$status_view"
  expect "a hand-written --project-root outside the store fails" 1 "$dir" \
    "formatted outside ProjectRootsStore"

  # 2b. The formatter stops consulting the table — the exact shape of the
  # property this ticket removed, with an unused argument.
  dir="$(fixture formatter-ignores-table)"
  store="${dir}/macos/GlomerisMenuBar/Sources/ProjectRootsStore.swift"
  perl -0pi -e 's/guard GlomerisCliProjectRootScope\.accepts\(command: command\) else \{ return \[\] \}//' "$store"
  expect "a formatter that ignores the table fails" 1 "$dir" \
    "does not consult GlomerisCliProjectRootScope"

  # 3. A root-scoped invocation that loses its roots.
  dir="$(fixture detect-loses-roots)"
  candidates="${dir}/macos/GlomerisMenuBar/Sources/CandidatesSectionView.swift"
  perl -0pi -e 's/projectRootsStore\.scoped\(\["detect", "--json", "--progress-json"\]\)/["detect", "--json", "--progress-json"]/' "$candidates"
  expect "a detect invocation with no roots in reach fails" 1 "$dir" \
    "is root-scoped, so the user's configured folders would"

  # 4. A typo in a forCommand: literal, which silently yields no roots.
  dir="$(fixture forcommand-typo)"
  perl -0pi -e 's/forCommand: "execute"/forCommand: "exceute"/' \
    "${dir}/macos/GlomerisMenuBar/Sources/CandidateDetailView.swift"
  expect "a mistyped forCommand: literal fails" 1 "$dir" \
    "is called with commands the table does"

  # 5. The status invocation bypassing the table, vector otherwise correct.
  dir="$(fixture status-bypasses-table)"
  perl -0pi -e 's/projectRootsStore\.scoped\(\["status", "--json"\]\)/["status", "--json"]/' \
    "${dir}/macos/GlomerisMenuBar/Sources/StatusHealthSectionView.swift"
  expect "the status vector must go through the table" 1 "$dir" \
    "without going through ProjectRootsStore.scoped()"

  # 5b. The reported defect, reintroduced at the call site through the escape
  # hatch — the roots attached to `status` while the table still says it takes
  # none. Asks for `detect`'s roots so that only this rule fires.
  dir="$(fixture status-smuggles-roots)"
  perl -0pi -e 's/projectRootsStore\.scoped\(\["status", "--json"\]\)/projectRootsStore.scoped(["status", "--json"] + projectRootsStore.projectRootArguments(forCommand: "detect"))/' \
    "${dir}/macos/GlomerisMenuBar/Sources/StatusHealthSectionView.swift"
  expect "roots smuggled onto status through the mid-vector accessor fails" 1 "$dir" \
    "attaches project roots to"

  # Extraction failures must fail, not pass quietly. Each of these is a way a
  # future refactor breaks this guard's reach rather than its subject.
  dir="$(fixture renamed-table)"
  perl -0pi -e 's/rootScopedCommands/commandsTakingProjectRoots/g' \
    "${dir}/macos/GlomerisMenuBar/Sources/GlomerisCliProjectRootScope.swift" \
    "${dir}/macos/GlomerisMenuBar/Sources/ProjectRootsStore.swift"
  expect "a renamed table fails rather than reporting agreement" 1 "$dir" \
    "extracted no commands from rootScopedCommands"

  dir="$(fixture no-rust-roots)"
  perl -0pi -e 's/extract_project_roots\(/extract_project_roots_renamed(/g' "${dir}/src/main.rs"
  expect "no extract_project_roots() in the CLI fails" 1 "$dir" \
    "found no call to extract_project_roots()"

  dir="$(fixture missing-store)"
  rm "${dir}/macos/GlomerisMenuBar/Sources/ProjectRootsStore.swift"
  expect "a missing ProjectRootsStore.swift fails" 1 "$dir" \
    "ProjectRootsStore.swift is missing"

  if [[ "$cases_run" -lt 12 ]]; then
    echo "FAIL: the self-test ran only ${cases_run} case(s); it is supposed to cover 12."
    echo "Cases were removed or a fixture failed to build. Either way this is not a pass."
    return 1
  fi

  if [[ "$cases_failed" -gt 0 ]]; then
    echo "FAIL: ${cases_failed} of ${cases_run} self-test case(s) did not behave as specified."
    return 1
  fi

  echo "PASS: ${cases_run} self-test case(s) — every rule fires as specified."
  return 0
}

# --- run ---------------------------------------------------------------------

if [[ "$self_test" -eq 1 ]]; then
  echo "Self-test: every rule, against disposable copies of this repository's sources."
  run_self_test
  exit $?
fi

echo "Checking the menu-bar app's CLI invocations against the CLI's project-root contract."
echo ""

if assess "$REPO_ROOT"; then
  echo ""
  echo "PASS: the app's project-root scoping matches the CLI, one formatter produces the flag,"
  echo "and every root-scoped invocation still reaches it."
  echo "Not checked here (see this script's header): flags other than --project-root, which have"
  echo "no shared producer to diff against."
  exit 0
fi

echo ""
echo "FAIL: the app's CLI invocations do not match the CLI's project-root contract."
echo ""
echo "The menu-bar app is a thin presenter over this CLI: which commands accept the configured"
echo "project roots is the CLI's contract, not a decision to take per view. HORO-1501 is what"
echo "taking it per view cost — one card, broken for exactly the users of the feature."
exit 1
