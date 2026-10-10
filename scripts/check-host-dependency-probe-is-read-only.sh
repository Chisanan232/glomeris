#!/usr/bin/env bash
#
# check-host-dependency-probe-is-read-only.sh
#
# HORO-1825 mechanical CI guard: the executable-dependency probe
# (`src/evidence/correlate/host_dependency.rs`) reads a developer's real
# host configuration — Claude Code settings, Codex config, LaunchAgent
# plists — to decide whether a disposable build cache is an installed
# runtime dependency. It must never execute anything it finds there, never
# write to any host config file, and never collect argv or raw command
# text beyond the structural fields the data contract names (ADR-0001 §4.3).
#
# Modelled on check-external-context-is-read-only.sh's approach: constrain
# the module by shape, not only by the tests its fixtures happen to reach.
#
# Checks:
#
#   1. No subprocess execution of anything the probe discovered: no `--version`
#      or `which` invocation, and no `Command::new` naming a variable that
#      could hold a discovered path (`resolved`, `candidate`, `exe`). The two
#      permitted subprocess tools are named explicitly (`lsof`, the injected
#      `plutil_bin`) and nothing else.
#   2. `plutil` is invoked with `-o -` wherever it is invoked at all — omitting
#      it rewrites the plist in place, which this read-only probe must never
#      risk.
#   3. The module's production code does not write to the filesystem
#      (`fs::write`, `fs::remove`, `fs::rename`, `File::create`, `OpenOptions`).
#      `#[cfg(test)]` fixture setup is exempt.
#   4. The module's production code does not print (a raw command string or a
#      parsed env value reaching stdout/stderr/a debug format is exactly the
#      HORO-1822 incident's privacy failure, reproduced).
#   5. No ambient `$HOME`/`std::env::var("HOME")` read inside the probe's
#      per-call logic — only `HostDependencyRoots::from_env`, which is the one
#      sanctioned production entrypoint, may read it.
#   6. Non-vacuity: the probe, its roots type, and the `plutil -convert json`
#      invocation must still be present.
#
# Exit 0 = pass. Exit 1 = fail, with file:line detail on stdout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODULE_FILE="src/evidence/correlate/host_dependency.rs"
abs_module="${REPO_ROOT}/${MODULE_FILE}"

if [[ ! -f "$abs_module" ]]; then
  echo "FAIL: ${MODULE_FILE} is missing."
  echo "HORO-1825's executable-dependency probe lives there. If it moved, update MODULE_FILE in this script."
  exit 1
fi

strip_comments() {
  grep -vE '^[0-9]+:[[:space:]]*(//|\*|/\*)' || true
}

# Numbered production lines only — everything before the first #[cfg(test)].
production_lines() {
  awk '/^#\[cfg\(test\)\]/ { exit } { print NR ":" $0 }' "$1"
}

require_compilable() {
  local pattern="$1"
  local compile_error
  compile_error="$(grep -E -- "$pattern" /dev/null 2>&1 || true)"
  if [[ -n "$compile_error" ]]; then
    echo "FAIL: forbidden-pattern regex does not compile: ${pattern}"
    echo "    grep said: ${compile_error}"
    exit 1
  fi
}

violations=0

# ---------------------------------------------------------------------------
# Check 1: no execution of a discovered path, and no subprocess tool beyond
# the two sanctioned ones.
# ---------------------------------------------------------------------------

FORBIDDEN_EXEC=(
  '--version;;probes a discovered binary'"'"'s version, which this probe must never do (never execute anything it finds)'
  '\bwhich\b.*Command::new|Command::new\("which"\);;shells out to which to confirm a binary runs, instead of a pure filesystem/PATH check'
)

for entry in "${FORBIDDEN_EXEC[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"
  require_compilable "$pattern"
  while IFS= read -r match; do
    [[ -n "$match" ]] || continue
    echo "VIOLATION: ${MODULE_FILE}:${match%%:*}: ${why}"
    echo "    ${match#*:}"
    violations=$((violations + 1))
  done < <(production_lines "$abs_module" | grep -E ":.*${pattern}" | strip_comments)
done

# Only `lsof` and `self.roots.plutil_bin` / `plutil_bin` may be named as a
# `Command::new` target in this module.
while IFS= read -r match; do
  [[ -n "$match" ]] || continue
  line="${match#*:}"
  if echo "$line" | grep -qE 'Command::new\("lsof"\)|Command::new\(lsof_bin\)|Command::new\(plutil_bin\)'; then
    continue
  fi
  echo "VIOLATION: ${MODULE_FILE}:${match%%:*}: spawns a subprocess target other than lsof/plutil_bin"
  echo "    ${line}"
  violations=$((violations + 1))
done < <(production_lines "$abs_module" | grep -E ':.*Command::new\(' | strip_comments)

# ---------------------------------------------------------------------------
# Check 2: every plutil invocation passes -o -.
# ---------------------------------------------------------------------------

plutil_calls="$(production_lines "$abs_module" | grep -c 'Command::new(plutil_bin)' || true)"
o_dash_calls="$(production_lines "$abs_module" | grep -c '"-o"' || true)"
if [[ "$plutil_calls" -gt 0 && "$o_dash_calls" -eq 0 ]]; then
  echo "VIOLATION: ${MODULE_FILE}: plutil is invoked but no '-o' flag is built anywhere — omitting '-o -' rewrites the plist in place."
  violations=$((violations + 1))
fi

# ---------------------------------------------------------------------------
# Checks 3 and 4: production code neither writes nor prints.
# ---------------------------------------------------------------------------

FORBIDDEN_IN_PRODUCTION=(
  'println!;;prints, and this module reads structural locations out of a developer'"'"'s real hook/daemon config'
  'eprintln!;;prints to stderr, same risk as println!'
  '\bprint!;;prints'
  '\beprint!;;prints to stderr'
  'dbg!;;debug-prints, which is how a command string or env value reaches a CI log'
  'fs::write;;writes a file — this probe reports and refuses, it never mutates host config (ADR-0001 §3.8)'
  'fs::remove;;removes a path'
  'fs::rename;;renames a path'
  'File::create;;creates a file'
  'OpenOptions;;opens a file for something other than reading'
)

for entry in "${FORBIDDEN_IN_PRODUCTION[@]}"; do
  pattern="${entry%%;;*}"
  why="${entry#*;;}"
  require_compilable "$pattern"
  while IFS= read -r match; do
    [[ -n "$match" ]] || continue
    echo "VIOLATION: ${MODULE_FILE}:${match%%:*}: ${why}"
    echo "    ${match#*:}"
    violations=$((violations + 1))
  done < <(production_lines "$abs_module" | grep -E ":.*${pattern}" | strip_comments)
done

# ---------------------------------------------------------------------------
# Check 5: no ambient $HOME read outside HostDependencyRoots::from_env.
# ---------------------------------------------------------------------------

from_env_body="$(awk '
  /pub fn from_env/ { inside = 1 }
  inside { print; if (/^    \}/) exit }
' "$abs_module" || true)"

while IFS= read -r match; do
  [[ -n "$match" ]] || continue
  lineno="${match%%:*}"
  line="${match#*:}"
  if echo "$from_env_body" | grep -qF "$line"; then
    continue
  fi
  echo "VIOLATION: ${MODULE_FILE}:${lineno}: reads the real environment outside HostDependencyRoots::from_env — roots must be an injected constructor argument everywhere else"
  echo "    ${line}"
  violations=$((violations + 1))
done < <(production_lines "$abs_module" | grep -E ':.*std::env::var(_os)?\(' | strip_comments)

# ---------------------------------------------------------------------------
# Check 6: non-vacuity.
# ---------------------------------------------------------------------------

REQUIRED=(
  "${MODULE_FILE};;pub struct HostDependencyRoots;;the injected-roots type is gone, so check 5 is asserting about nothing"
  "${MODULE_FILE};;pub trait HostDependencyProbe;;the probe trait is gone"
  "${MODULE_FILE};;-convert;;the plutil invocation is gone, so checks 1 and 2 are vacuous"
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
  echo "FAIL: found ${violations} line(s) breaking HORO-1825's read-only host-dependency boundary."
  echo "The probe reads a developer's real hook/daemon/LaunchAgent config as supporting evidence."
  echo "It must never execute anything it finds, never write to host config, and never print a"
  echo "discovered command string or env value. See ADR-0001 §4.3/§6 and the header of"
  echo "${MODULE_FILE}."
  exit 1
fi

echo "PASS: ${MODULE_FILE} names no execution of a discovered path, and only lsof/plutil as subprocess targets."
echo "PASS: every plutil invocation passes -o -."
echo "PASS: the module's production code neither prints nor writes to the filesystem."
echo "PASS: no ambient \$HOME read outside HostDependencyRoots::from_env."
exit 0
