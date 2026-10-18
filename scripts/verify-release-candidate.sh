#!/usr/bin/env bash
#
# verify-release-candidate.sh
#
# HORO-1921. Answers one question about a built `glomeris` CLI: is this the
# artifact a user following the install instructions would get, and does it
# work, safely, from nothing? Run it on a release candidate BEFORE anything is
# tagged or published, and again on the published artifact afterwards.
#
# It exists because HORO-1313's clean-install check found the opposite:
# the published v0.2.0 tarball predates the CLI contract the menu-bar app is
# built against (no `autopilot`, `pressure`, `settings`, `actions`), and nothing
# in CI could see that, because CI builds from source and never installs the
# artifact that was actually published.
#
# WHAT IT CHECKS
# --------------
# 1. Identity. With --tarball, the SHA-256 in the sibling .sha256 file matches
#    the bytes, and the archive extracts into a fresh directory. It then reports
#    the binary's own SHA-256, architecture(s) and code-signature summary, so a
#    human sees exactly which bytes were tested.
# 2. Version. `--version` runs; with --expect-version the output must contain it.
# 3. Contract. In a brand-new, empty HOME with a scrubbed environment, every CLI
#    surface the menu-bar app reads exits 0 and prints valid JSON, and every
#    top-level command's --help exits 0.
# 4. Safe defaults. A fresh install has Autopilot disabled, nothing allowed, and
#    nothing that starts unprompted — a release must never ship unattended
#    cleanup switched on.
# 5. Strictness. An unknown argument to a read-only command is refused (exit 2)
#    rather than silently ignored.
# 6. Isolation. The candidate only ever touches the throwaway HOME. This script
#    never runs `daemon install`, `autopilot enable`, `execute`, `free` or any
#    other command that writes outside Glomeris's own state.
#
# --self-test builds a stub CLI and proves the checks can fail: a stub that
# lacks a surface, one with Autopilot enabled, one that ignores unknown
# arguments, and one with a bad checksum must each be rejected, and a good stub
# must pass.
#
# Exit 0 = every check passed. Exit 1 = a failure, printed. Exit 2 = usage.

set -euo pipefail

usage() {
  echo "usage: $0 (--binary PATH | --tarball PATH) [--expect-version X.Y.Z]"
  echo "       $0 --self-test"
}

BINARY=""
TARBALL=""
EXPECT_VERSION=""

READ_ONLY_JSON=(
  "status --json"
  "settings show --json"
  "autopilot show --json"
  "pressure show --json"
  "actions history --json"
  "history --json"
  "daemon status --json"
  "resource inspect"
  "external-context --json"
)
TOP_LEVEL=(status scan detect check-project-root explain clean llm-plan llm-check resource
  execute free emergency autopilot history actions workflow-profile daemon pressure
  settings external-context)

failures=0
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}
ok() { echo "ok:   $1"; }

# run_cli <binary> <home> <args...>: scrubbed environment, throwaway HOME.
run_cli() {
  local bin="$1" home="$2"
  shift 2
  env -i HOME="$home" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$bin" "$@"
}

is_json() {
  python3 -I -c 'import json,sys; json.load(open(sys.argv[1]))' "$1" 2>/dev/null
}

verify_binary() {
  local bin="$1" home out
  home="$(mktemp -d "${TMPDIR:-/tmp}/glomeris-rc-home.XXXXXX")"
  out="$(mktemp -d "${TMPDIR:-/tmp}/glomeris-rc-out.XXXXXX")"
  # Single-quoted so the paths expand when the trap fires, not now.
  trap 'rm -rf "$home" "$out"' RETURN

  local sha arch
  sha="$(shasum -a 256 "$bin" | cut -d' ' -f1)"
  arch="$(lipo -archs "$bin" 2>/dev/null || echo unknown)"
  echo "binary: $bin"
  echo "sha256: $sha"
  echo "arch:   $arch"
  echo "signature: $(codesign -dv "$bin" 2>&1 | grep -E 'Signature|flags=' | tr '\n' ' ' || true)"

  # 2. Version.
  local version
  if version="$(run_cli "$bin" "$home" --version 2>&1)"; then
    ok "--version: $version"
    if [[ -n "$EXPECT_VERSION" && "$version" != *"$EXPECT_VERSION"* ]]; then
      fail "--version does not contain the expected ${EXPECT_VERSION}: $version"
    fi
  else
    fail "--version is not supported by this binary"
  fi

  # 3a. Contract: every surface the app reads.
  local args
  for args in "${READ_ONLY_JSON[@]}"; do
    # shellcheck disable=SC2086 # deliberate word splitting of a fixed argument list
    if run_cli "$bin" "$home" $args >"$out/stdout" 2>"$out/stderr"; then
      if is_json "$out/stdout"; then
        ok "contract: glomeris $args"
      else
        fail "contract: glomeris $args printed invalid JSON"
      fi
    else
      fail "contract: glomeris $args exited non-zero ($(head -c 120 "$out/stderr" | tr '\n' ' '))"
    fi
  done

  # 3b. Every top-level command documents itself.
  local c
  for c in "${TOP_LEVEL[@]}"; do
    if ! run_cli "$bin" "$home" "$c" --help >/dev/null 2>&1; then
      fail "help: glomeris $c --help does not exit 0"
    fi
  done

  # 4. Safe defaults.
  if run_cli "$bin" "$home" autopilot show --json >"$out/autopilot" 2>/dev/null && is_json "$out/autopilot"; then
    if python3 -I - "$out/autopilot" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
bad = []
if d.get("enabled") is not False: bad.append("enabled")
if d.get("allowed_kinds") not in ([], None): bad.append("allowed_kinds")
if d.get("starts_unprompted") is not False: bad.append("starts_unprompted")
sys.exit(1 if bad else 0)
PY
    then
      ok "defaults: Autopilot is disabled, allows nothing, starts nothing unprompted"
    else
      fail "defaults: a fresh install has Autopilot switched on or armed"
    fi
  fi

  # 5. Strictness.
  local rc=0
  run_cli "$bin" "$home" status --glomeris-rc-unknown-flag >/dev/null 2>&1 || rc=$?
  if [[ "$rc" -eq 2 ]]; then
    ok "strictness: an unknown argument is refused (exit 2)"
  else
    fail "strictness: 'status' with an unknown argument exited $rc, expected 2"
  fi
}

verify_tarball() {
  local tarball="$1" sumfile dir expected actual
  sumfile="${tarball}.sha256"
  if [[ ! -f "$tarball" ]]; then
    echo "FAIL: no such tarball: $tarball"
    return 1
  fi
  if [[ ! -f "$sumfile" ]]; then
    echo "FAIL: no checksum file beside the tarball: $sumfile"
    return 1
  fi
  expected="$(cut -d' ' -f1 "$sumfile")"
  actual="$(shasum -a 256 "$tarball" | cut -d' ' -f1)"
  if [[ "$expected" != "$actual" ]]; then
    echo "FAIL: checksum mismatch for $tarball"
    return 1
  fi
  ok "checksum: $tarball matches ${sumfile##*/}"
  dir="$(mktemp -d "${TMPDIR:-/tmp}/glomeris-rc-extract.XXXXXX")"
  trap 'rm -rf "$dir"' RETURN
  tar -xJf "$tarball" -C "$dir"
  BINARY="$(find "$dir" -type f -name glomeris -perm -u+x | head -1)"
  if [[ -z "$BINARY" ]]; then
    echo "FAIL: the archive contains no executable named glomeris"
    return 1
  fi
  verify_binary "$BINARY"
}

self_test() {
  local work fails=0
  work="$(mktemp -d "${TMPDIR:-/tmp}/glomeris-rc-selftest.XXXXXX")"
  trap 'rm -rf "$work"' RETURN

  # make_stub <name> <autopilot-enabled> <strict> <missing-surface>
  make_stub() {
    local name="$1" enabled="$2" strict="$3" missing="$4"
    cat >"$work/$name" <<STUB
#!/bin/sh
case "\$*" in
  --version) echo "glomeris 9.9.9"; exit 0 ;;
  *--help) exit 0 ;;
  "$missing") echo "unknown command" >&2; exit 2 ;;
  "autopilot show --json") echo '{"enabled": $enabled, "allowed_kinds": [], "starts_unprompted": false}'; exit 0 ;;
  "resource inspect") echo '[]'; exit 0 ;;
  status\ --glomeris-rc-unknown-flag) [ "$strict" = yes ] && exit 2 || exit 0 ;;
  *--json) echo '{}'; exit 0 ;;
esac
exit 0
STUB
    chmod +x "$work/$name"
  }
  make_stub good false yes "<none>"
  make_stub armed true yes "<none>"
  make_stub lenient false no "<none>"
  make_stub nopressure false yes "pressure show --json"

  expect() { # name, wanted-exit
    local rc=0
    "$0" --binary "$work/$1" >/dev/null 2>&1 || rc=$?
    if [[ "$rc" -ne "$2" ]]; then
      echo "SELF-TEST FAIL: $1 exited $rc, expected $2"
      fails=$((fails + 1))
    fi
  }
  expect good 0
  expect armed 1
  expect lenient 1
  expect nopressure 1

  # A bad checksum must be rejected before anything is extracted.
  echo "not really a tarball" >"$work/fake.tar.xz"
  echo "0000000000000000000000000000000000000000000000000000000000000000  fake.tar.xz" >"$work/fake.tar.xz.sha256"
  local rc=0
  "$0" --tarball "$work/fake.tar.xz" >/dev/null 2>&1 || rc=$?
  if [[ "$rc" -ne 1 ]]; then
    echo "SELF-TEST FAIL: a bad checksum exited $rc, expected 1"
    fails=$((fails + 1))
  fi

  if [[ "$fails" -gt 0 ]]; then
    echo "FAIL: ${fails} self-test failure(s)."
    exit 1
  fi
  echo "PASS: self-test — a good stub passes; an armed Autopilot, a lenient CLI, a missing surface and a bad checksum are each rejected."
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BINARY="${2:?--binary needs a path}"; shift 2 ;;
    --tarball) TARBALL="${2:?--tarball needs a path}"; shift 2 ;;
    --expect-version) EXPECT_VERSION="${2:?--expect-version needs a value}"; shift 2 ;;
    --self-test) self_test ;;
    -h | --help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

if [[ -n "$TARBALL" ]]; then
  verify_tarball "$TARBALL" || failures=$((failures + 1))
elif [[ -n "$BINARY" ]]; then
  verify_binary "$BINARY"
else
  usage
  exit 2
fi

if [[ "$failures" -gt 0 ]]; then
  echo ""
  echo "FAIL: ${failures} check(s) failed. This artifact is not a releasable Glomeris."
  exit 1
fi
echo ""
echo "PASS: identity, version, contract, safe defaults and strictness hold in an empty HOME."
exit 0
