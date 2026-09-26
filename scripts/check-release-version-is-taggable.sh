#!/usr/bin/env bash
#
# check-release-version-is-taggable.sh
#
# HORO-1483. Answers one question: can the version this workspace declares
# still be released?
#
# WHY THE QUESTION NEEDED ASKING
# ------------------------------
# HORO-1328 is the state this guard is named after. `Cargo.toml` declared
# 0.2.0, the tag v0.2.0 already existed, and no MVP 2.0 tag was possible from
# that state whatever number was chosen. It had been true for 611 commits when
# it was found, and it was found by someone attempting the release.
#
# Fourteen mechanical guards ran on every one of those commits and none could
# see it. Two come close and both stop short by design:
#
#   * check-app-version-matches-crate.sh asserts the app bundle reports the
#     crate version. It says nothing about whether that version is releasable,
#     and its header is explicit that it leans on cargo-dist for crate == tag.
#   * cargo-dist does check a pushed tag against the declared version — at
#     release time, for the tag you managed to push. A version that COLLIDES
#     with an existing tag never reaches that check, because `git push` refuses
#     the duplicate ref first. The collision is the case that cannot be caught
#     downstream.
#
# WHAT THIS DELIBERATELY DOES NOT FAIL ON
# ---------------------------------------
# `version == newest tag` is NOT a failure here, and that is a decision rather
# than an oversight.
#
# For a cargo-dist repository the version is bumped as part of the release
# change itself, so sitting at the last released number between releases is the
# ordinary state. A guard that failed on it would be red on main from the
# moment of every release until someone bumped, and the pressure that creates
# is the bad part: the fastest way to clear a red build is to invent a version
# number, and the version number is a product-identity decision (0.3.0 versus
# 1.0.0 is HORO-1328's open question, not a semver deduction).
#
# So equality is REPORTED, with the commit distance, on every CI run. The
# reporting is the fix for the invisibility; the hard failures below are
# reserved for states that are wrong under any release model.
#
# MODES
# -----
#   (no arguments)      Always-true checks, plus a report of the current state.
#                       This is what runs on every push and pull request.
#
#   --for-release       Adds: a tag naming the declared version must not
#                       already exist. This is the HORO-1328 state, and in a
#                       release it is fatal rather than informational.
#
#   --tag <name>        Adds --for-release, and additionally requires <name> to
#                       be exactly "v<declared version>".
#
#   --self-test         Runs the release-mode rules against throwaway git
#                       repositories under a temporary directory and asserts
#                       each one fires. Runs in CI so that the release-mode
#                       paths cannot rot unnoticed between releases, which for
#                       this repository has so far been a span of months.
#
# Exit 0 = pass. Exit 1 = a state that blocks a release, or a check that could
# not be performed. Reading no version, or finding no tags to compare against
# in a repository that has them, is a failure and not a pass.

set -euo pipefail

usage() {
  echo "usage: $(basename "$0") [--for-release | --tag <name> | --self-test]"
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MANIFEST="Cargo.toml"

# --- argument parsing -------------------------------------------------------

for_release=0
self_test=0
tag_name=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --for-release)
      for_release=1
      shift
      ;;
    --tag)
      if [[ $# -lt 2 || -z "${2:-}" ]]; then
        echo "FAIL: --tag needs a tag name."
        usage
        exit 1
      fi
      tag_name="$2"
      for_release=1
      shift 2
      ;;
    --self-test)
      self_test=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "FAIL: unknown argument: $1"
      usage
      exit 1
      ;;
  esac
done

# --- reading the declared version -------------------------------------------
#
# The workspace version, from the `[workspace.package]` or `[package]` table's
# first `version =` line. `|| true` so that a manifest without one reaches the
# emptiness check below instead of ending the script silently (HORO-1479).
declared_version_of() {
  grep -m1 -E '^version[[:space:]]*=' "$1" \
    | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/' || true
}

# `sort -V` decides the ordering question this guard is about, so it is worth
# stating why it is trusted: it is the same comparison `git tag --sort=version`
# uses, and the self-test below pins the one case that matters (0.10.0 is newer
# than 0.9.0, which a lexical sort gets backwards).
newer_of() {
  printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1
}

# --- the checks, over one repository ----------------------------------------
#
# Factored out so the self-test can run them against fixtures rather than
# against a second copy of the logic. A guard whose self-test exercises a
# reimplementation proves only that the reimplementation works.
#
# $1 = repository root, $2 = 1 if release mode, $3 = tag name or empty.
# Prints its findings. Returns 0 if releasable, 1 otherwise.
assess() {
  local root="$1" release_mode="$2" want_tag="$3"
  local manifest="${root}/${MANIFEST}"
  local problems=0

  if [[ ! -f "$manifest" ]]; then
    echo "FAIL: no ${MANIFEST} at ${root}."
    echo "This guard reads the declared version from that file; without it there is nothing to check."
    return 1
  fi

  local version
  version="$(declared_version_of "$manifest")"
  if [[ -z "$version" ]]; then
    echo "FAIL: no version declared in ${MANIFEST}."
    echo "Either the manifest moved or the version key was renamed. Update this guard — do not"
    echo "delete it, because an unreadable version is indistinguishable here from a releasable one."
    return 1
  fi

  local tags newest distance
  tags="$(git -C "$root" tag --list 'v*' --sort=version:refname || true)"

  if [[ -z "$tags" ]]; then
    # Not a pass in any mode, including the default one.
    #
    # This guard's entire content is a comparison against the existing tags, so
    # a checkout without them has checked nothing — and "nothing to compare
    # against" printed as a warning above an exit 0 is precisely the vacuous
    # green that every other guard in this directory refuses. `git clone`
    # fetches tags, `actions/checkout` does not unless asked, and that asymmetry
    # is the realistic way this check would go quiet.
    #
    # This costs a genuinely untagged repository a red build, which glomeris
    # stopped being at v0.1.0 and cannot become again.
    echo "FAIL: no v* tags in this checkout, so there is nothing to compare ${version} against."
    echo "This repository has released tags, so an empty list means the checkout is shallow or was"
    echo "made without them: 'git fetch --tags' locally, or 'fetch-depth: 0' on the checkout step."
    echo "Passing here would report a clean bill of health for a comparison that never happened."
    return 1
  fi

  newest="$(printf '%s\n' "$tags" | tail -1 | sed 's/^v//' || true)"
  if [[ -z "$newest" ]]; then
    echo "FAIL: could not read a version out of the newest tag."
    echo "  tags seen: $(tr '\n' ' ' <<< "$tags" || true)"
    return 1
  fi

  echo "declared version: ${version}"
  echo "newest tag:       v${newest}"
  echo "tags compared:    $(tr '\n' ' ' <<< "$tags" || true)"

  # --- AC 2: a declared downgrade is wrong under every release model --------
  if [[ "$version" != "$newest" ]] && [[ "$(newer_of "$version" "$newest")" == "$newest" ]]; then
    echo ""
    echo "FAIL: the declared version ${version} is OLDER than the newest tag v${newest}."
    echo "Nothing can be released from here and nothing legitimate produces this state: it means a"
    echo "version bump was reverted, or a tag was cut from a branch whose version was behind."
    problems=$((problems + 1))
  fi

  # --- the HORO-1328 state -------------------------------------------------
  local collision=0
  if printf '%s\n' "$tags" | grep -qxF "v${version}"; then
    collision=1
  fi

  if [[ "$collision" -eq 1 ]]; then
    local head_tagged=0
    if git -C "$root" describe --exact-match --match "v${version}" HEAD >/dev/null 2>&1; then
      head_tagged=1
    fi

    if [[ "$head_tagged" -eq 1 ]]; then
      echo ""
      echo "note: HEAD is v${version} itself, so the declared version and the tag agree."
    else
      distance="$(git -C "$root" rev-list --count "v${version}..HEAD" 2>/dev/null || true)"
      echo ""
      if [[ "$release_mode" -eq 1 ]]; then
        echo "FAIL: v${version} already exists and points somewhere other than HEAD."
        echo "No release is possible from this state whatever tag name is chosen: v${version} cannot"
        echo "be reused, and any other tag names a version this workspace does not declare, which"
        echo "cargo-dist resolves from the manifest rather than from the ref. Bump the version."
        problems=$((problems + 1))
      else
        echo "note: v${version} already exists, ${distance:-?} commit(s) behind HEAD."
        echo "  Not a failure here: bumping in the release change is this repository's model, so"
        echo "  sitting at the last released number between releases is expected. It IS a release"
        echo "  blocker — 'check-release-version-is-taggable.sh --for-release' says so, and that is"
        echo "  what runs on a tag push. Printed on every run because the state is otherwise"
        echo "  invisible: HORO-1328 sat here for 611 commits."
      fi
    fi
  fi

  # --- AC 3: a supplied tag must name the declared version -----------------
  if [[ -n "$want_tag" ]] && [[ "$want_tag" != "v${version}" ]]; then
    echo ""
    echo "FAIL: tag ${want_tag} does not name the declared version ${version}."
    echo "cargo-dist resolves the release from the workspace version, so a tag that disagrees with"
    echo "the manifest is not a release it can plan. Expected v${version}."
    problems=$((problems + 1))
  fi

  [[ "$problems" -eq 0 ]]
}

# --- self-test --------------------------------------------------------------
#
# Every hard failure above, fired against a disposable repository. This exists
# because the release-mode branches run only on a tag push, and for this
# repository that has meant twice, months apart. A guard whose interesting half
# is exercised twice a year is a guard nobody knows is broken.
run_self_test() {
  local work
  work="$(mktemp -d)"
  # shellcheck disable=SC2064  # $work must expand now, not at trap time.
  trap "rm -rf '$work'" RETURN

  local cases_run=0 cases_failed=0

  # Build a fixture repository: $1 = name, $2 = declared version, rest = tags
  # to create, each on its own commit.
  fixture() {
    local name="$1" version="$2"
    shift 2
    local dir="${work}/${name}"
    mkdir -p "$dir"
    git -C "$dir" init -q
    git -C "$dir" config user.email fixture@example.invalid
    git -C "$dir" config user.name Fixture
    printf 'version = "%s"\n' "$version" > "${dir}/${MANIFEST}"
    git -C "$dir" add "$MANIFEST"
    git -C "$dir" commit -qm "initial"
    local t
    for t in "$@"; do
      git -C "$dir" tag "$t"
      # A later commit so HEAD is not the tag, which is the state that matters.
      git -C "$dir" commit -q --allow-empty -m "after ${t}"
    done
    printf '%s' "$dir"
  }

  # Quote a failing case's output so it is readable under the case that failed.
  quoted() {
    local line
    while IFS= read -r line; do
      printf '      %s\n' "$line"
    done <<< "$1"
  }

  # $1 = description, $2 = expected exit (0 pass / 1 fail), $3 = fixture dir,
  # $4 = release mode, $5 = tag name, $6 = a phrase the output must contain.
  #
  # The phrase is matched against the output as a whole, so it has to sit on one
  # line of it. A phrase straddling a wrapped sentence never matches and reads
  # as the assertion failing, which is how this comment came to be here.
  expect() {
    local what="$1" want="$2" dir="$3" mode="$4" tag="$5" phrase="$6"
    cases_run=$((cases_run + 1))
    local out status=0
    out="$(assess "$dir" "$mode" "$tag" 2>&1)" || status=1
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

  dir="$(fixture collision 0.2.0 v0.1.0 v0.2.0)"
  expect "a released version is fatal in release mode" 1 "$dir" 1 "" \
    "already exists and points somewhere other than HEAD"
  expect "the same state is only a note by default" 0 "$dir" 0 "" \
    "Not a failure here"

  dir="$(fixture trailing 0.1.0 v0.1.0 v0.2.0)"
  expect "a declared version behind the newest tag fails either way" 1 "$dir" 0 "" \
    "is OLDER than the newest tag"

  dir="$(fixture ahead 0.3.0 v0.1.0 v0.2.0)"
  expect "a bumped version is releasable" 0 "$dir" 1 "" \
    "newest tag:       v0.2.0"
  expect "a tag that disagrees with the manifest fails" 1 "$dir" 1 "v0.2.1" \
    "does not name the declared version"
  expect "the matching tag passes" 0 "$dir" 1 "v0.3.0" \
    "declared version: 0.3.0"

  # Version ordering, not lexical ordering. `sort -V` is the whole basis of the
  # trailing check, and a lexical comparison gets exactly this backwards.
  dir="$(fixture ordering 0.9.0 v0.9.0 v0.10.0)"
  expect "0.10.0 is newer than 0.9.0" 1 "$dir" 0 "" \
    "is OLDER than the newest tag v0.10.0"

  # Unreadable input must fail rather than pass.
  mkdir -p "${work}/nomanifest"
  git -C "${work}/nomanifest" init -q
  expect "a missing manifest fails" 1 "${work}/nomanifest" 0 "" \
    "no ${MANIFEST} at"

  mkdir -p "${work}/noversion"
  git -C "${work}/noversion" init -q
  git -C "${work}/noversion" config user.email fixture@example.invalid
  git -C "${work}/noversion" config user.name Fixture
  echo '[package]' > "${work}/noversion/${MANIFEST}"
  git -C "${work}/noversion" add "$MANIFEST"
  git -C "${work}/noversion" commit -qm initial
  expect "a manifest with no version fails" 1 "${work}/noversion" 0 "" \
    "no version declared in"

  # A tagless checkout fails in BOTH modes. Asserted in both because the
  # difference between the modes is the whole design of this guard, and "it
  # passes quietly on the cheap path" is how the property would be lost.
  dir="$(fixture untagged 0.1.0)"
  expect "a tagless checkout fails by default rather than reporting clean" 1 "$dir" 0 "" \
    "nothing to compare 0.1.0 against"
  expect "a tagless checkout fails in release mode too" 1 "$dir" 1 "" \
    "nothing to compare 0.1.0 against"

  # A self-test that asserted nothing would be the vacuous green this whole
  # file exists to avoid, so the count is checked rather than assumed.
  if [[ "$cases_run" -lt 11 ]]; then
    echo "FAIL: the self-test ran only ${cases_run} case(s); it is supposed to cover 11."
    echo "Cases were removed or a fixture failed to build. Either way this is not a pass."
    return 1
  fi

  if [[ "$cases_failed" -gt 0 ]]; then
    echo "FAIL: ${cases_failed} of ${cases_run} self-test case(s) did not behave as specified."
    return 1
  fi

  echo "PASS: ${cases_run} self-test case(s) — every release-mode rule fires as specified."
  return 0
}

# --- run --------------------------------------------------------------------

if [[ "$self_test" -eq 1 ]]; then
  echo "Self-test: the release-mode rules, against disposable fixtures."
  run_self_test
  exit $?
fi

if [[ "$for_release" -eq 1 ]]; then
  echo "Release mode${tag_name:+ for tag ${tag_name}}."
else
  echo "Default mode: hard failures only for states that are wrong under any release model."
fi
echo ""

if assess "$REPO_ROOT" "$for_release" "$tag_name"; then
  echo ""
  if [[ "$for_release" -eq 1 ]]; then
    echo "PASS: this workspace can be released."
  else
    echo "PASS: the declared version is not behind any existing tag."
  fi
  exit 0
fi

echo ""
echo "FAIL: this workspace cannot be released as it stands."
echo ""
echo "The version number itself is a product decision, not something to pick to clear this"
echo "check. See HORO-1328. Bump the workspace version, let cargo-dist regenerate what it owns,"
echo "and do not hand-edit the generated release workflow."
exit 1
