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
cd "$REPO_ROOT"

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

if [[ ! -d "$WORKFLOW_DIR" ]]; then
  echo "FAIL: ${WORKFLOW_DIR} not found — run this from a full checkout."
  exit 1
fi

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "FAIL: not inside a git checkout, so the manifest set cannot be discovered."
  echo "Guessing at it would report a pass over whatever happened to be on disk."
  exit 1
fi

# ---------------------------------------------------------------------------
# 1. Collect every action reference.
# ---------------------------------------------------------------------------
# A step's `uses:` line, with or without the list dash. A commented-out one is
# not matched, because a `#` cannot appear before the key.
USES_RE='^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*'

workflows=()
while IFS= read -r found; do
  workflows+=("$found")
done < <(
  find "$WORKFLOW_DIR" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) \
    | LC_ALL=C sort
)

if [[ ${#workflows[@]} -eq 0 ]]; then
  echo "FAIL: found no workflow files under ${WORKFLOW_DIR}."
  echo "Passing by comparing nothing is the one outcome this guard must never produce."
  exit 1
fi

# The value up to the first whitespace or comment, which is the `owner/repo@ref`
# form. Local (`./path`) and container (`docker://`) references have no version
# to compare and are dropped below.
references="$(
  grep -hE "$USES_RE" "${workflows[@]}" \
    | sed -E "s|${USES_RE}||" \
    | sed -E 's|[[:space:]#].*$||' \
    | grep -E '^[^./][^[:space:]]*@[^[:space:]@]+$' \
    | LC_ALL=C sort -u \
    || true
)"

if [[ -z "$references" ]]; then
  echo "FAIL: matched no versioned 'uses:' references across ${#workflows[@]} workflow file(s)."
  echo "Every job in this repository checks out with an action, so zero means the pattern"
  echo "above stopped matching — not that the workflows have no dependencies."
  exit 1
fi

reference_count="$(printf '%s\n' "$references" | wc -l | tr -d '[:space:]')"
action_count="$(printf '%s\n' "$references" | awk -F'@' '{print $1}' | LC_ALL=C sort -u | wc -l | tr -d '[:space:]')"

# ---------------------------------------------------------------------------
# 2. No action referenced at two versions.
# ---------------------------------------------------------------------------
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
  exit 1
fi

# ---------------------------------------------------------------------------
# 3. The configuration exists, watches the actions, and is not neutered.
# ---------------------------------------------------------------------------
if [[ ! -f "$CONFIG" ]]; then
  echo "FAIL: ${CONFIG} does not exist, so nothing proposes updates for the"
  echo "${action_count} action(s) above."
  echo "Dependabot alerts are a repository setting and may also be off; this file is the"
  echo "part that lives in the repository, and it is the part a checkout can verify."
  exit 1
fi

# Uncommented key lines only. A `#` before the key means it is prose about the
# key, which is how the header of that file explains itself.
config_keys="$(grep -E '^[[:space:]]*(-[[:space:]]+)?[a-z-]+:' "$CONFIG" || true)"

if ! printf '%s\n' "$config_keys" | grep -qE "package-ecosystem:[[:space:]]*[\"']?github-actions[\"']?[[:space:]]*$"; then
  echo "FAIL: ${CONFIG} does not declare the 'github-actions' ecosystem."
  echo "Without it the ${action_count} action(s) this repository runs are watched by nothing,"
  echo "which is the state HORO-1497 was filed about."
  exit 1
fi

if printf '%s\n' "$config_keys" | grep -qE 'target-branch:'; then
  echo "FAIL: ${CONFIG} sets 'target-branch:'."
  echo ""
  echo "That key silently disables security updates for the default branch. It does not"
  echo "appear in the repository's settings UI, it produces no warning, and a configuration"
  echo "carrying it is indistinguishable from a working one until someone checks whether a"
  echo "known advisory ever opened a pull request."
  exit 1
fi

if ! printf '%s\n' "$config_keys" | grep -qE '^[[:space:]]*version:[[:space:]]*2[[:space:]]*$'; then
  echo "FAIL: ${CONFIG} does not declare 'version: 2'."
  echo "GitHub rejects the whole file without it, and a rejected configuration watches"
  echo "nothing while still being present in the repository."
  exit 1
fi

watched_ecosystems="$(
  printf '%s\n' "$config_keys" \
    | grep -E 'package-ecosystem:' \
    | sed -E 's|.*package-ecosystem:[[:space:]]*||; s|["'"'"']||g; s|[[:space:]]*$||' \
    | LC_ALL=C sort -u
)"

# ---------------------------------------------------------------------------
# 4. Every ecosystem with a manifest is watched, or excused by name.
# ---------------------------------------------------------------------------
is_watched() {
  printf '%s\n' "$watched_ecosystems" | grep -qxF "$1"
}

is_excused() {
  local wanted="$1" entry
  for entry in "${DELIBERATELY_UNWATCHED[@]}"; do
    [[ "${entry%%|*}" == "$wanted" ]] && return 0
  done
  return 1
}

# An exclusion cannot outlive the manifest it was written for.
for entry in "${DELIBERATELY_UNWATCHED[@]}"; do
  excused_manifest="${entry#*|}"
  if [[ ! -e "$excused_manifest" ]]; then
    echo "FAIL: '${entry%%|*}' is listed in DELIBERATELY_UNWATCHED in this script, but"
    echo "${excused_manifest} no longer exists."
    echo "Remove the entry — a stale exclusion is an exemption nobody is looking at."
    exit 1
  fi
done

unaccounted=()
present_count=0
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
  exit 1
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
exit 0
