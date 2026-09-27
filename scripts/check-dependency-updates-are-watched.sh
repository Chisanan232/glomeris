#!/usr/bin/env bash
#
# check-dependency-updates-are-watched.sh
#
# HORO-1497: eight third-party GitHub Actions run in this repository's CI, and
# nothing in the repository watched any of them. Dependabot alerts are disabled
# on the repository, there was no `.github/dependabot.yml`, and every reference
# was a floating tag — so a withdrawn or abandoned action would have been
# discovered when a job started failing, at whatever moment that happened to be.
#
# The same gap had already produced a visible symptom before anyone went looking
# for it: `actions/checkout` was referenced as `@v4` eighteen times in the three
# hand-written workflows (sixteen in `ci.yml`, one in `docs.yml`, one in
# `macos-app-release.yml`) and as `@v6` six times in the generated `release.yml`.
# Two majors of one action, in one repository, decided by which workflow fired.
# Nothing reported that, because nothing was comparing the references to each
# other.
#
# WHAT IS CHECKED
# ---------------
# 1. No action is referenced at two different versions. This is the defect above,
#    stated as a property. It is the cheap half and it is the half that catches
#    the realistic mistake: someone bumps a pin in the workflow they are editing
#    and the other three keep the old one.
#
# 2. `.github/dependabot.yml` exists, declares the `github-actions` ecosystem,
#    and does not carry a `target-branch:` key. The last of those is not
#    housekeeping. `target-branch:` silently turns off security updates for the
#    default branch, it is invisible in the repository's settings UI, and a
#    configuration that has it reads exactly like one that is working.
#
# 3. Every dependency ecosystem this repository actually has a manifest for is
#    either watched by that file or named in DELIBERATELY_UNWATCHED below with a
#    reason. Adding a manifest for something nobody has thought about — a
#    `Package.swift`, a `Dockerfile`, a `package.json` — fails this guard rather
#    than quietly widening the unwatched surface.
#
# WHAT THIS GUARD CANNOT DO, STATED RATHER THAN IMPLIED
# ----------------------------------------------------
# It does not validate `dependabot.yml` against Dependabot's schema. Only GitHub
# can do that, and it reports the result on the repository's Dependabot page
# after the file reaches the default branch. So this guard proves the file says
# the right things, not that GitHub accepted it. Check that page once after this
# lands.
#
# It also cannot make a mutable reference watchable. `dtolnay/rust-toolchain` is
# pinned to `@stable`, which is a branch and not a version, so Dependabot has no
# version to compare and will never propose an update for it. That is left alone
# deliberately — that action is designed to be used that way, and pinning it to a
# tag would change which Rust toolchain CI installs — but it means one of the
# eight stays unwatched after this guard passes. The honest coverage figure is
# seven of eight, and this file is where that is written down.
#
# Exit 0 = the references agree and the ecosystems with manifests are accounted
# for. Exit 1 = a split reference, a missing or neutered configuration, or an
# ecosystem nobody has decided about.

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

WORKFLOW_DIR=".github/workflows"
CONFIG=".github/dependabot.yml"
# Ecosystems with a manifest in this repository that are deliberately not given
# to Dependabot. Each entry is `<ecosystem>|<manifest that must still exist>`,
# so an entry cannot outlive the thing it excuses: if the manifest goes away the
# exclusion is stale and this guard says so.
#
# cargo: `deny.toml` configures an `[advisories]` section and the `deny` job runs
# cargo-deny on every pull request, so the security question is already answered
# by something that fails the build rather than opening a pull request. Adding
# cargo version updates here would be a maintenance-cadence preference, and the
# cost is a steady stream of pull requests on a 98-package lock file.
DELIBERATELY_UNWATCHED=(
  "cargo|Cargo.toml"
)

# `<ecosystem>|<git pathspec that indicates the ecosystem is present>`. Not
# exhaustive over everything Dependabot supports — exhaustive over what could
# plausibly appear in this repository, which is what the check is for.
ECOSYSTEM_MANIFESTS=(
  "cargo|Cargo.toml"
  "swift|Package.swift"
  "npm|package.json"
  "pip|requirements.txt"
  "pip|pyproject.toml"
  "docker|Dockerfile"
  "gomod|go.mod"
  "bundler|Gemfile"
  "composer|composer.json"
  "nuget|*.csproj"
  "terraform|*.tf"
  "gitsubmodule|.gitmodules"
)

# A step's `uses:` line, with or without the list dash. A commented-out one is
# not matched, because a `#` cannot appear before the key.
USES_RE='^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*'

# The versioned `owner/repo@ref` references in the files named as arguments.
# Local (`./path`) and container (`docker://`) references have no version to
# compare and are dropped.
action_references_in() {
  grep -hE "$USES_RE" "$@" \
    | sed -E "s|${USES_RE}||" \
    | sed -E 's|[[:space:]#].*$||' \
    | grep -E '^[^./][^[:space:]]*@[^[:space:]@]+$' \
    | LC_ALL=C sort -u \
    || true
}

# --- rules -------------------------------------------------------------------
#
# Factored out so the self-test can run them against fixtures rather than
# against a second copy of the logic. A guard whose self-test exercises a
# paraphrase of the rules proves nothing about the rules.
#
# Runs in a subshell so that `cd` and the local variables cannot leak into the
# next fixture. Returns 0 when the repository at $1 satisfies every rule.
assess() (
  local root="$1"
  cd "$root" || {
    echo "FAIL: ${root} is not a directory that can be entered."
    return 1
  }

  if [[ ! -d "$WORKFLOW_DIR" ]]; then
    echo "FAIL: ${WORKFLOW_DIR} not found — run this from a full checkout."
    return 1
  fi

  if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "FAIL: not inside a git checkout, so the manifest set cannot be discovered."
    echo "Guessing at it would report a pass over whatever happened to be on disk."
    return 1
  fi

  # -------------------------------------------------------------------------
  # 1. Collect every action reference.
  # -------------------------------------------------------------------------
  local workflows=()
  local found
  while IFS= read -r found; do
    workflows+=("$found")
  done < <(
    find "$WORKFLOW_DIR" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) \
      | LC_ALL=C sort
  )

  if [[ ${#workflows[@]} -eq 0 ]]; then
    echo "FAIL: found no workflow files under ${WORKFLOW_DIR}."
    echo "Passing by comparing nothing is the one outcome this guard must never produce."
    return 1
  fi

  local references
  references="$(action_references_in "${workflows[@]}")"

  if [[ -z "$references" ]]; then
    echo "FAIL: matched no versioned 'uses:' references across ${#workflows[@]} workflow file(s)."
    echo "Every job in this repository checks out with an action, so zero means the pattern"
    echo "above stopped matching — not that the workflows have no dependencies."
    return 1
  fi

  local reference_count action_count
  reference_count="$(printf '%s\n' "$references" | wc -l | tr -d '[:space:]')"
  action_count="$(printf '%s\n' "$references" | awk -F'@' '{print $1}' | LC_ALL=C sort -u | wc -l | tr -d '[:space:]')"

  # -------------------------------------------------------------------------
  # 2. No action referenced at two versions.
  # -------------------------------------------------------------------------
  local split
  split="$(
    printf '%s\n' "$references" \
      | awk -F'@' '{ count[$1]++; seen[$1] = seen[$1] " @" $2 }
                   END { for (name in count) if (count[name] > 1) printf "%s%s\n", name, seen[name] }' \
      | LC_ALL=C sort
  )"

  if [[ -n "$split" ]]; then
    echo "FAIL: an action is referenced at more than one version:"
    printf '  %s\n' "$split"
    echo ""
    echo "Pick one and use it everywhere. Two majors of the same action means CI behaves"
    echo "differently depending on which workflow fired, and the older pin is the one that"
    echo "breaks first — at a moment nobody chose."
    echo ""
    echo "Find them with:"
    echo "  grep -rn 'uses:' ${WORKFLOW_DIR}"
    return 1
  fi

  # -------------------------------------------------------------------------
  # 3. The configuration exists, watches the actions, and is not neutered.
  # -------------------------------------------------------------------------
  if [[ ! -f "$CONFIG" ]]; then
    echo "FAIL: ${CONFIG} does not exist, so nothing proposes updates for the"
    echo "${action_count} action(s) above."
    echo "Dependabot alerts are a repository setting and may also be off; this file is the"
    echo "part that lives in the repository, and it is the part a checkout can verify."
    return 1
  fi

  # Uncommented key lines only. A `#` before the key means it is prose about the
  # key, which is how the header of that file explains itself.
  local config_keys
  config_keys="$(grep -E '^[[:space:]]*(-[[:space:]]+)?[a-z-]+:' "$CONFIG" || true)"

  if ! printf '%s\n' "$config_keys" | grep -qE "package-ecosystem:[[:space:]]*[\"']?github-actions[\"']?[[:space:]]*$"; then
    echo "FAIL: ${CONFIG} does not declare the 'github-actions' ecosystem."
    echo "Without it the ${action_count} action(s) this repository runs are watched by nothing,"
    echo "which is the state HORO-1497 was filed about."
    return 1
  fi

  if printf '%s\n' "$config_keys" | grep -qE 'target-branch:'; then
    echo "FAIL: ${CONFIG} sets 'target-branch:'."
    echo ""
    echo "That key silently disables security updates for the default branch. It does not"
    echo "appear in the repository's settings UI, it produces no warning, and a configuration"
    echo "carrying it is indistinguishable from a working one until someone checks whether a"
    echo "known advisory ever opened a pull request."
    return 1
  fi

  if ! printf '%s\n' "$config_keys" | grep -qE '^[[:space:]]*version:[[:space:]]*2[[:space:]]*$'; then
    echo "FAIL: ${CONFIG} does not declare 'version: 2'."
    echo "GitHub rejects the whole file without it, and a rejected configuration watches"
    echo "nothing while still being present in the repository."
    return 1
  fi

  local watched_ecosystems
  watched_ecosystems="$(
    printf '%s\n' "$config_keys" \
      | grep -E 'package-ecosystem:' \
      | sed -E 's|.*package-ecosystem:[[:space:]]*||; s|["'"'"']||g; s|[[:space:]]*$||' \
      | LC_ALL=C sort -u
  )"

  # -------------------------------------------------------------------------
  # 4. Every ecosystem with a manifest is watched, or excused by name.
  # -------------------------------------------------------------------------
  local entry
  is_watched() {
    printf '%s\n' "$watched_ecosystems" | grep -qxF "$1"
  }

  is_excused() {
    local wanted="$1" candidate
    for candidate in "${DELIBERATELY_UNWATCHED[@]}"; do
      [[ "${candidate%%|*}" == "$wanted" ]] && return 0
    done
    return 1
  }

  # An exclusion cannot outlive the manifest it was written for.
  local excused_manifest
  for entry in "${DELIBERATELY_UNWATCHED[@]}"; do
    excused_manifest="${entry#*|}"
    if [[ ! -e "$excused_manifest" ]]; then
      echo "FAIL: '${entry%%|*}' is listed in DELIBERATELY_UNWATCHED in this script, but"
      echo "${excused_manifest} no longer exists."
      echo "Remove the entry — a stale exclusion is an exemption nobody is looking at."
      return 1
    fi
  done

  local unaccounted=()
  local present_count=0
  local ecosystem pathspec
  for entry in "${ECOSYSTEM_MANIFESTS[@]}"; do
    ecosystem="${entry%%|*}"
    pathspec="${entry#*|}"

    found="$(git ls-files -- "$pathspec" | head -1)"
    [[ -n "$found" ]] || continue
    present_count=$((present_count + 1))

    if is_watched "$ecosystem" || is_excused "$ecosystem"; then
      continue
    fi
    unaccounted+=("${ecosystem} (found ${found})")
  done

  if [[ ${#unaccounted[@]} -gt 0 ]]; then
    echo "FAIL: ${#unaccounted[@]} dependency ecosystem(s) have a manifest here and are"
    echo "neither watched by ${CONFIG} nor excused in this script:"
    printf '  %s\n' "${unaccounted[@]}"
    echo ""
    echo "Either add an 'updates:' entry for it, or add it to DELIBERATELY_UNWATCHED above"
    echo "with the reason. Doing neither leaves a dependency surface that nothing looks at"
    echo "and nobody decided to ignore."
    return 1
  fi

  echo "Action references across ${#workflows[@]} workflow file(s):"
  printf '%s\n' "$references" | sed 's|^|  |'
  echo ""
  echo "Ecosystems watched by ${CONFIG}:"
  printf '%s\n' "$watched_ecosystems" | sed 's|^|  |'
  if [[ ${#DELIBERATELY_UNWATCHED[@]} -gt 0 ]]; then
    echo ""
    echo "Deliberately unwatched, with the reason in this script's DELIBERATELY_UNWATCHED:"
    for entry in "${DELIBERATELY_UNWATCHED[@]}"; do
      echo "  ${entry%%|*} (${entry#*|})"
    done
  fi
  echo ""
  echo "PASS: ${reference_count} reference(s) to ${action_count} action(s), none split across versions; ${present_count} ecosystem(s) with a manifest, all accounted for."
  return 0
)

# --- self-test ---------------------------------------------------------------
#
# Every rule above, fired against a disposable copy of this repository's own
# configuration. The reason this exists is the shape of the rules: all of them
# are negative assertions over file text, and a negative assertion whose
# extraction stops matching reports agreement between two empty sets. This
# repository has already paid for that once — a blank-line-intolerant regex gave
# a clean bill of health over 135 workflow files — and the defect each rule here
# is written for appears once every few months, so a failing case in this
# function is the only evidence that the rule fires at all.
run_self_test() {
  local work
  work="$(mktemp -d)"
  # shellcheck disable=SC2064  # $work must expand now, not at trap time.
  trap "rm -rf '$work'" RETURN

  local cases_run=0 cases_failed=0

  # A copy of the real configuration under $work/<name>, ready to be mutated.
  # `git init` and `git add` and no commit: the ecosystem discovery reads the
  # index, so a fixture needs one, and committing would need an identity this
  # script has no business choosing.
  fixture() {
    local name="$1"
    local dir="${work}/${name}"
    mkdir -p "${dir}/.github"
    cp -R "${REPO_ROOT}/.github/workflows" "${dir}/.github/workflows"
    cp "${REPO_ROOT}/.github/dependabot.yml" "${dir}/.github/dependabot.yml"
    cp "${REPO_ROOT}/Cargo.toml" "${dir}/Cargo.toml"
    if [[ "${2:-}" != "--no-git" ]]; then
      git -C "$dir" -c init.defaultBranch=main init -q
      git -C "$dir" add -A
    fi
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

  local dir

  # An unmutated copy passes. Without this case every rule below could be firing
  # on the copy itself rather than on the mutation.
  dir="$(fixture pristine)"
  expect "an unmutated copy of the configuration passes" 0 "$dir" \
    "none split across versions"

  # 1. The HORO-1497 defect: one action at two versions.
  dir="$(fixture split-version)"
  perl -0pi -e 's/actions\/checkout\@v6/actions\/checkout\@v4/' \
    "${dir}/.github/workflows/docs.yml"
  expect "an action referenced at two versions fails" 1 "$dir" \
    "referenced at more than one version"

  # 2. No configuration at all — the state HORO-1497 was filed about.
  dir="$(fixture no-config)"
  rm "${dir}/.github/dependabot.yml"
  expect "a missing dependabot.yml fails" 1 "$dir" \
    "does not exist, so nothing proposes updates"

  # 2b. A configuration that watches something else.
  dir="$(fixture wrong-ecosystem)"
  perl -0pi -e 's/package-ecosystem: "github-actions"/package-ecosystem: "npm"/' \
    "${dir}/.github/dependabot.yml"
  expect "a configuration that does not watch the actions fails" 1 "$dir" \
    "does not declare the 'github-actions' ecosystem"

  # 2c. The silent kill switch. This is the case worth the whole function: a
  # configuration carrying it reads exactly like a working one.
  dir="$(fixture target-branch)"
  perl -0pi -e 's/^(\s*)(schedule:)/$1target-branch: "main"\n$1$2/m' \
    "${dir}/.github/dependabot.yml"
  expect "target-branch: fails" 1 "$dir" \
    "silently disables security updates"

  # 2d. Without `version: 2` GitHub rejects the whole file, and a rejected
  # configuration watches nothing while still being present.
  dir="$(fixture no-version-key)"
  perl -0pi -e 's/^version: 2$//m' "${dir}/.github/dependabot.yml"
  expect "a configuration without version: 2 fails" 1 "$dir" \
    "does not declare 'version: 2'"

  # 3. A manifest for an ecosystem nobody has decided about.
  dir="$(fixture unwatched-ecosystem)"
  printf '{}\n' > "${dir}/package.json"
  git -C "$dir" add -A
  expect "a new manifest for an unwatched ecosystem fails" 1 "$dir" \
    "neither watched by"

  # 3b. An exclusion that outlived the manifest it excuses.
  dir="$(fixture stale-exclusion)"
  rm "${dir}/Cargo.toml"
  git -C "$dir" add -A
  expect "a DELIBERATELY_UNWATCHED entry whose manifest is gone fails" 1 "$dir" \
    "no longer exists"

  # Extraction failures must fail, not pass quietly. Each of these is a way this
  # guard loses its reach rather than its subject.
  dir="$(fixture no-workflow-dir)"
  rm -rf "${dir}/.github/workflows"
  expect "a missing workflow directory fails" 1 "$dir" \
    "not found — run this from a full checkout"

  dir="$(fixture empty-workflow-dir)"
  rm -f "${dir}/.github/workflows/"*
  expect "a workflow directory with no workflows in it fails" 1 "$dir" \
    "found no workflow files"

  dir="$(fixture no-uses-lines)"
  perl -0pi -e 's/uses:/used:/g' "${dir}/.github/workflows/"*
  expect "workflows with no matchable uses: lines fail" 1 "$dir" \
    "matched no versioned 'uses:' references"

  dir="$(fixture not-a-checkout --no-git)"
  expect "a tree that is not a git checkout fails" 1 "$dir" \
    "not inside a git checkout"

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
  echo "Self-test: every rule, against disposable copies of this repository's configuration."
  run_self_test
  exit $?
fi

if assess "$REPO_ROOT"; then
  exit 0
fi

exit 1
