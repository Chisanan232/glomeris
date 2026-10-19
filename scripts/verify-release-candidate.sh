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
#    the bytes; the archive listing is vetted (no absolute path, no `..`, no
#    symlink or hard link, exactly one file named glomeris) BEFORE anything is
#    extracted; then it extracts into a fresh directory. It reports the
#    binary's own SHA-256, architecture(s) and code-signature summary, so a
#    human sees exactly which bytes were tested.
# 2. Version. `--version` runs; with --expect-version the output must contain
#    that version as a whole number sequence (0.2.0 does not match 10.2.0).
# 3. Contract. In a brand-new, empty HOME with a scrubbed environment, every CLI
#    surface the menu-bar app reads exits 0, prints valid JSON AND carries the
#    keys the app decodes; and every top-level command's --help exits 0.
# 4. Safe defaults. A fresh install has Autopilot disabled, nothing allowed, and
#    nothing that starts unprompted — a release must never ship unattended
#    cleanup switched on.
# 5. Strictness. An unknown argument to a read-only command is refused (exit 2)
#    rather than silently ignored.
# 6. Containment. The candidate runs with HOME pointing at a throwaway
#    directory, a scrubbed environment and, where `sandbox-exec` exists, with
#    network access denied. This script never runs `daemon install`,
#    `autopilot enable`, `execute`, `free` or any other command that writes
#    outside Glomeris's own state. One read leaks through by design:
#    `daemon status` asks the real per-user launchd whether the monitor label
#    is loaded (read-only), so its "loaded" value describes the machine, not the
#    throwaway HOME. This is containment for the real CLI, not a sandbox for a
#    hostile binary; do not run an untrusted binary with it.
#
# Temporary directories are removed by ONE script-level EXIT trap over a global
# list. (Per-function RETURN traps replace each other and run again in the
# caller's scope; that bug made a good tarball fail under bash 5.)
#
# --self-test builds stub CLIs and tarballs and proves the checks can fail: a
# stub lacking a surface, lacking required keys, with Autopilot enabled,
# ignoring unknown arguments, a bad checksum, a hostile archive (`..`, absolute
# path, symlink, two binaries), and a version that only matches as a substring
# must each be rejected, and a good stub and a good tarball must pass.
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
CLEANUP_DIRS=()

# `${arr[@]+...}` keeps an empty array valid under `set -u` on bash 3.2. The
# trap body is inline rather than a function, so the linter sees every call.
d=""
trap 'for d in ${CLEANUP_DIRS[@]+"${CLEANUP_DIRS[@]}"}; do rm -rf "$d"; done' EXIT

# Sets LAST_TEMP rather than printing the path: a command substitution would run
# this in a subshell, and the registration in CLEANUP_DIRS would be lost with it.
LAST_TEMP=""
make_temp_dir() {
  LAST_TEMP="$(mktemp -d "${TMPDIR:-/tmp}/glomeris-rc.XXXXXX")"
  CLEANUP_DIRS+=("$LAST_TEMP")
}

# Each surface the app reads, with the JSON keys it must carry. An empty key
# list means only "valid JSON" is required (the surface is a list).
READ_ONLY_JSON=(
  "status --json|pressure_state"
  "settings show --json|notify_at_used_percent default_goal"
  "autopilot show --json|enabled allowed_kinds starts_unprompted"
  "pressure show --json|current"
  "actions history --json|"
  "history --json|"
  "daemon status --json|plist_installed loaded"
  "resource inspect|"
  "external-context --json|"
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

NETWORK_DENY='(version 1)(allow default)(deny network*)'

# run_cli <binary> <home> <args...>: scrubbed environment, throwaway HOME,
# network denied where the platform can do it.
run_cli() {
  local bin="$1" home="$2"
  shift 2
  if command -v sandbox-exec >/dev/null 2>&1; then
    env -i HOME="$home" PATH=/usr/bin:/bin:/usr/sbin:/sbin \
      sandbox-exec -p "$NETWORK_DENY" "$bin" "$@"
  else
    env -i HOME="$home" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$bin" "$@"
  fi
}

# has_keys <json-file> <space-separated keys>: valid JSON object with every key.
has_keys() {
  python3 -I - "$1" "$2" <<'PY'
import json, sys
try:
    data = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
keys = sys.argv[2].split()
if keys and not (isinstance(data, dict) and all(k in data for k in keys)):
    sys.exit(1)
PY
}

# version_matches <output> <expected>: whole-number-sequence match.
version_matches() {
  local pattern="${2//./\\.}"
  grep -Eq "(^|[^0-9.])${pattern}([^0-9.]|\$)" <<<"$1"
}

verify_binary() {
  local bin="$1" home out
  make_temp_dir
  home="$LAST_TEMP"
  make_temp_dir
  out="$LAST_TEMP"

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
    if [[ -n "$EXPECT_VERSION" ]] && ! version_matches "$version" "$EXPECT_VERSION"; then
      fail "--version does not carry the expected ${EXPECT_VERSION}: $version"
    fi
  else
    fail "--version is not supported by this binary"
  fi

  # 3a. Contract: every surface the app reads, with the keys it decodes.
  local entry args keys
  for entry in "${READ_ONLY_JSON[@]}"; do
    args="${entry%%|*}"
    keys="${entry#*|}"
    # shellcheck disable=SC2086 # deliberate word splitting of a fixed argument list
    if run_cli "$bin" "$home" $args >"$out/stdout" 2>"$out/stderr"; then
      if has_keys "$out/stdout" "$keys"; then
        ok "contract: glomeris $args"
      else
        fail "contract: glomeris $args printed invalid JSON or lacks: ${keys:-valid JSON}"
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
  if run_cli "$bin" "$home" autopilot show --json >"$out/autopilot" 2>/dev/null; then
    if python3 -I - "$out/autopilot" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
ok = (
    isinstance(d, dict)
    and d.get("enabled") is False
    and isinstance(d.get("allowed_kinds"), list)
    and d.get("allowed_kinds") == []
    and d.get("starts_unprompted") is False
)
sys.exit(0 if ok else 1)
PY
    then
      ok "defaults: Autopilot is disabled, allows nothing, starts nothing unprompted"
    else
      fail "defaults: a fresh install has Autopilot switched on, armed, or not reporting its state"
    fi
  else
    fail "defaults: glomeris autopilot show --json did not run"
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

# vet_archive <tarball>: refuse anything that could write outside the extract
# directory or smuggle a second binary, before a single byte is extracted.
vet_archive() {
  local listing verbose count
  listing="$(tar -tJf "$1")"
  verbose="$(tar -tvJf "$1")"
  if grep -Eq '^/|(^|/)\.\.(/|$)' <<<"$listing"; then
    echo "FAIL: the archive has an absolute path or a '..' component"
    return 1
  fi
  if grep -Eq '^[lh]' <<<"$verbose"; then
    echo "FAIL: the archive contains a symlink or hard link"
    return 1
  fi
  count="$(grep -Ec '(^|/)glomeris$' <<<"$listing" || true)"
  if [[ "$count" -ne 1 ]]; then
    echo "FAIL: the archive must contain exactly one file named glomeris, found $count"
    return 1
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
  expected="$(head -1 "$sumfile" | cut -d' ' -f1)"
  actual="$(shasum -a 256 "$tarball" | cut -d' ' -f1)"
  if [[ "$expected" != "$actual" ]]; then
    echo "FAIL: checksum mismatch for $tarball"
    return 1
  fi
  ok "checksum: $tarball matches ${sumfile##*/}"
  vet_archive "$tarball" || return 1
  make_temp_dir
  dir="$LAST_TEMP"
  tar -xJf "$tarball" -C "$dir"
  BINARY="$(find "$dir" -type f -name glomeris -perm -u+x | head -1 || true)"
  if [[ -z "$BINARY" ]]; then
    echo "FAIL: the archive contains no executable named glomeris"
    return 1
  fi
  verify_binary "$BINARY"
}

self_test() {
  local work fails=0 rc before after name
  make_temp_dir
  work="$LAST_TEMP"

  # make_stub <name> <autopilot-enabled> <strict> <missing-surface> [settings-json]
  make_stub() {
    local sname="$1" enabled="$2" strict="$3" missing="$4" settings="${5:-}"
    [[ -n "$settings" ]] || settings='{"notify_at_used_percent": 75, "default_goal": {}}'
    cat >"$work/$sname" <<STUB
#!/bin/sh
case "\$*" in
  --version) echo "glomeris 0.2.0"; exit 0 ;;
  *--help) exit 0 ;;
  "$missing") echo "unknown command" >&2; exit 2 ;;
  "autopilot show --json") echo '{"enabled": $enabled, "allowed_kinds": [], "starts_unprompted": false}'; exit 0 ;;
  "settings show --json") echo '$settings'; exit 0 ;;
  "status --json") echo '{"pressure_state": "OK"}'; exit 0 ;;
  "pressure show --json") echo '{"current": {}}'; exit 0 ;;
  "daemon status --json") echo '{"plist_installed": false, "loaded": false}'; exit 0 ;;
  "resource inspect") echo '[]'; exit 0 ;;
  status\ --glomeris-rc-unknown-flag) [ "$strict" = yes ] && exit 2 || exit 0 ;;
  *--json) echo '{}'; exit 0 ;;
esac
exit 0
STUB
    chmod +x "$work/$sname"
  }
  make_stub good false yes "<none>"
  make_stub armed true yes "<none>"
  make_stub lenient false no "<none>"
  make_stub nopressure false yes "pressure show --json"
  make_stub nokeys false yes "<none>" '{}'

  expect_binary() { # name, wanted-exit, [extra args...]
    local bname="$1" want="$2" got=0
    shift 2
    "$0" --binary "$work/$bname" "$@" >/dev/null 2>&1 || got=$?
    if [[ "$got" -ne "$want" ]]; then
      echo "SELF-TEST FAIL: $bname $* exited $got, expected $want"
      fails=$((fails + 1))
    fi
  }
  expect_binary good 0
  expect_binary good 0 --expect-version 0.2.0
  expect_binary good 1 --expect-version 2.0
  expect_binary good 1 --expect-version 10.2.0
  expect_binary armed 1
  expect_binary lenient 1
  expect_binary nopressure 1
  expect_binary nokeys 1

  # Tarballs: build real archives so the vetting and the cleanup path run.
  make_tarball() { # name, kind (good|symlink|second)
    local tname="$1" kind="$2" stage="$work/stage-$1"
    mkdir -p "$stage/pkg"
    cp "$work/good" "$stage/pkg/glomeris"
    case "$kind" in
      good) ;;
      symlink) ln -s /etc/passwd "$stage/pkg/link" ;;
      second) mkdir -p "$stage/other" && cp "$stage/pkg/glomeris" "$stage/other/glomeris" ;;
    esac
    (cd "$stage" && tar -cJf "$work/$tname.tar.xz" .)
    (cd "$work" && shasum -a 256 "$tname.tar.xz" >"$work/$tname.tar.xz.sha256")
  }
  make_tarball goodtar good
  make_tarball linktar symlink
  make_tarball twotar second

  expect_tarball() { # name, wanted-exit
    local got=0
    "$0" --tarball "$work/$1.tar.xz" >/dev/null 2>&1 || got=$?
    if [[ "$got" -ne "$2" ]]; then
      echo "SELF-TEST FAIL: tarball $1 exited $got, expected $2"
      fails=$((fails + 1))
    fi
  }
  expect_tarball goodtar 0
  expect_tarball linktar 1
  expect_tarball twotar 1

  # `..` and absolute-path members, written with a tool that does not refuse them.
  python3 -I - "$work" <<'PY'
import io, sys, tarfile
work = sys.argv[1]
for name, member in (("dottar", "../escape/glomeris"), ("abstar", "/tmp/glomeris-rc-abs/glomeris")):
    with tarfile.open(f"{work}/{name}.tar.xz", "w:xz") as t:
        data = b"#!/bin/sh\nexit 0\n"
        info = tarfile.TarInfo(member)
        info.size = len(data)
        info.mode = 0o755
        t.addfile(info, io.BytesIO(data))
PY
  for name in dottar abstar; do
    (cd "$work" && shasum -a 256 "$name.tar.xz" >"$name.tar.xz.sha256")
    expect_tarball "$name" 1
  done

  # A bad checksum must be rejected before anything is extracted.
  cp "$work/goodtar.tar.xz" "$work/badsum.tar.xz"
  echo "0000000000000000000000000000000000000000000000000000000000000000  badsum.tar.xz" >"$work/badsum.tar.xz.sha256"
  expect_tarball badsum 1

  # Nothing may be left behind by a tarball run.
  before="$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'glomeris-rc.*' 2>/dev/null | wc -l | tr -d ' ')"
  rc=0
  "$0" --tarball "$work/goodtar.tar.xz" >/dev/null 2>&1 || rc=$?
  after="$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'glomeris-rc.*' 2>/dev/null | wc -l | tr -d ' ')"
  if [[ "$rc" -ne 0 || "$before" != "$after" ]]; then
    echo "SELF-TEST FAIL: a tarball run exited $rc and changed the temp-dir count from $before to $after"
    fails=$((fails + 1))
  fi

  if [[ "$fails" -gt 0 ]]; then
    echo "FAIL: ${fails} self-test failure(s)."
    exit 1
  fi
  echo "PASS: self-test — good stub and good tarball pass; armed Autopilot, lenient CLI, missing surface, missing keys, substring-only version, bad checksum, symlink, '..', absolute-path and two-binary archives are each rejected; no temp directory leaks."
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
