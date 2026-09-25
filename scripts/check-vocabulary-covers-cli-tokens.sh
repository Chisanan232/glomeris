#!/usr/bin/env bash
#
# check-vocabulary-covers-cli-tokens.sh
#
# HORO-1306. GlomerisMenuBar translates the Rust CLI's JSON tokens into
# plain language in Sources/GlomerisVocabulary.swift. The Swift unit tests
# cover every token they know about, but they know about them because the
# lists were transcribed by hand — so the one drift they cannot catch is
# the important one: Rust grows a variant, the Swift table does not, and
# the GUI silently renders the unrecognised fallback for a real state.
#
# That is not a hypothetical. The doc comments in both files claimed
# `ReasonCode` had 19 values when it has 18.
#
# This script compares the two sides mechanically. For each vocabulary
# below it extracts the token set from the Rust function that is the sole
# producer of those strings, extracts the `case "..."` labels from the
# corresponding Swift lookup, and requires the two sets to be equal in
# both directions:
#
#   - a token in Rust but not Swift  -> the GUI would show the fallback
#     for a state the CLI really emits
#   - a token in Swift but not Rust  -> dead wording, or a typo in a case
#     label that silently routes a live token to the fallback instead
#
# COVERAGE — deliberately partial, and it says so in the PASS line
# ----------------------------------------------------------------
# Eleven of the twelve vocabularies are checked. The last one cannot be,
# honestly, because it has no single canonical producer to diff against:
# `ExecuteReport::outcome` is built from string literals at its call sites
# in src/cli/mod.rs rather than from one `as_str`-style match. Grepping
# those literals repo-wide also picks up test assertions and unrelated
# strings, so a set-equality check built on it would produce false
# failures and, worse, invite someone to loosen it until it passed. It
# stays covered by the transcribed Swift tests only, and this script
# reports 11/12 rather than printing a bare PASS that reads as "all twelve
# verified".
#
# Two vocabularies have been moved OUT of that list by giving them a
# producer, which is the fix whenever this list is uncomfortable — not
# loosening the extraction.
#
# `AuditRecord::source` was the first. It did not stay merely unchecked:
# HORO-1310 added `autopilot_auto_safe` and `autopilot_preauthorized_ask`
# at new call sites, and nothing here could have told anyone whether the
# GUI had learned words for them. HORO-1312 made
# `ActionSource::as_str` in src/monitor/persistence.rs the sole producer,
# so it is diffed like the rest.
#
# `ExecuteRefusalReport::reason` was the second, and the one where silence
# cost the most: an unrecognised refusal token renders as "The CLI refused
# for a reason this app has no wording for" at exactly the moment a user is
# being told they may not delete something. HORO-1327 made
# `RefusalReason::as_str` in src/reporting/dto.rs the sole producer. Note
# that the vocabulary is eight tokens, not the seven that mirror
# `ExecuteResolution`: `busy` is emitted before that enum exists at all,
# from the execution lock, and is a variant of `RefusalReason` for exactly
# that reason. `"busy"` also appears as a temp-lock filename in
# src/executor/lock.rs, which is why a repo-wide grep was never a
# substitute for a producer here.
#
# Exit 0 = the eleven checked vocabularies match. Exit 1 = drift, or an
# extraction that came back empty (which would otherwise be a vacuous
# pass).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SWIFT_FILE="macos/GlomerisMenuBar/Sources/GlomerisVocabulary.swift"
abs_swift="${REPO_ROOT}/${SWIFT_FILE}"

if [[ ! -f "$abs_swift" ]]; then
  echo "FAIL: ${SWIFT_FILE} is missing, so there is nothing to compare the CLI tokens against."
  exit 1
fi

# "<swift lookup>;;<rust file>;;<rust fn>;;<what the tokens are>"
#
# The Rust function named here must be the only place those strings are
# produced; if that stops being true the check becomes misleading rather
# than merely incomplete.
VOCABULARIES=(
  'pressure;;src/monitor/pressure.rs;;as_str;;PressureState'
  'safety;;src/reporting/policy_label.rs;;as_str;;PolicyLabel'
  'completeness;;src/reporting/dto.rs;;completeness_tag;;Completeness'
  'confidence;;src/reporting/dto.rs;;confidence_tag;;Confidence'
  'regenerability;;src/reporting/dto.rs;;regenerability_tag;;Regenerability'
  'reason;;src/policy/class.rs;;as_str;;ReasonCode'
  'kind;;src/evidence/model.rs;;tag;;ResourceKind'
  # HORO-1307. Note the Swift side takes `String?` rather than `String`,
  # which is why the anchor below stops at `_ token:` instead of spelling
  # out the parameter type.
  'impactTier;;src/reporting/impact.rs;;as_str;;StorageImpactTier'
  # HORO-1309. `llm_check_outcome` exists precisely so these five strings
  # have one producer to diff against: they were originally inline in
  # `build_llm_check_report`, where `"ok"` came from a function call rather
  # than a match arm and so would have been invisible here.
  'llmCheckOutcome;;src/actions/llm.rs;;llm_check_outcome;;LlmCheckOutcome'
  # HORO-1312. `AuditRecord::source` stays a `String` on the read side, so
  # that a future version's new source value cannot make `read_audit_tail`
  # drop a user's history — but every writer now goes through this enum,
  # which is what makes the set diffable at all.
  'actionSource;;src/monitor/persistence.rs;;as_str;;ActionSource'
  # HORO-1327. `ExecuteRefusalReport::reason` is typed as this enum, and its
  # `Serialize` impl goes through `as_str`, so the JSON token cannot be
  # produced anywhere else. Eight tokens: the seven that mirror
  # `ExecuteResolution`'s non-`Executed` variants, plus `busy` from the
  # execution lock (see this script's header).
  'refusal;;src/reporting/dto.rs;;as_str;;RefusalReason'
)

# Print the body of a function, from its `fn <name>` line to the line
# where brace depth returns to zero. Brace counting rather than an indent
# heuristic, because `as_str` appears in several impl blocks and a later
# one must not be picked up by accident.
function_body() {
  local file="$1" fn_name="$2" lang="$3"
  # A literal substring, matched with index() rather than a regex. The
  # anchors necessarily contain `(`, and escaping it survives neither
  # bash's quoting nor awk's -v string-escape processing intact — the
  # paren arrives unbalanced and the pattern fails to compile. Nothing
  # here needs regex power anyway.
  #
  # The trailing `(` is load-bearing: it keeps `fn as_str(` from matching
  # the test function `fn as_str_is_distinct_and_non_empty_...()` that
  # sits further down src/policy/class.rs.
  local anchor
  if [[ "$lang" == "rust" ]]; then
    anchor="fn ${fn_name}("
  else
    # Stops at the parameter NAME, not its type: `impactTier` takes a
    # `String?`, so an anchor ending in `String)` silently matched nothing
    # for it — and a non-matching anchor here produces an empty extraction,
    # which this script reports as a failure rather than a pass.
    anchor="static func ${fn_name}(_ token:"
  fi

  awk -v anchor="$anchor" '
    BEGIN { started = 0; depth = 0 }
    !started && index($0, anchor) > 0 { started = 1 }
    started {
      print
      n = gsub(/\{/, "{"); depth += n
      n = gsub(/\}/, "}"); depth -= n
      if (depth <= 0 && index($0, "}") > 0) exit
    }
  ' "$file"
}

failures=0
checked=0

for entry in "${VOCABULARIES[@]}"; do
  swift_fn="${entry%%;;*}"
  rest="${entry#*;;}"
  rust_file="${rest%%;;*}"
  rest="${rest#*;;}"
  rust_fn="${rest%%;;*}"
  rest="${rest#*;;}"
  rust_type="${rest%%;;*}"

  abs_rust="${REPO_ROOT}/${rust_file}"
  if [[ ! -f "$abs_rust" ]]; then
    echo "FAIL: ${rust_file} is missing, so ${rust_type} tokens cannot be verified."
    failures=$((failures + 1))
    continue
  fi

  # `|| true` so that "grep matched nothing" does not trip pipefail and
  # abort the script here: an empty extraction is a condition this script
  # must *report*, and under `set -e` the assignment failing would exit
  # with a bare non-zero and none of the explanation below. That is how a
  # renamed Rust producer first showed up — correctly red, but silent
  # about why, which is the kind of failure someone deletes rather than
  # fixes. The `-z` checks immediately after catch emptiness whatever its
  # cause, so nothing is lost by tolerating the non-zero exit.
  rust_tokens="$(
    function_body "$abs_rust" "$rust_fn" rust \
      | grep -oE '=>[[:space:]]*"[a-zA-Z0-9_]+"' \
      | grep -oE '"[a-zA-Z0-9_]+"' \
      | tr -d '"' \
      | sort -u \
      || true
  )"

  swift_tokens="$(
    function_body "$abs_swift" "$swift_fn" swift \
      | grep -E '^[[:space:]]*case[[:space:]]+"' \
      | grep -oE '"[a-zA-Z0-9_]+"' \
      | tr -d '"' \
      | sort -u \
      || true
  )"

  # An empty side means the anchor stopped matching — a renamed function,
  # a reformatted match arm. Reporting that as "no drift" is exactly the
  # vacuous pass this script exists to avoid.
  if [[ -z "$rust_tokens" ]]; then
    echo "FAIL: extracted no tokens from ${rust_file} ${rust_fn}() — the anchor no longer matches."
    echo "      Fix the extraction in this script; do not assume the sets agree."
    failures=$((failures + 1))
    continue
  fi
  if [[ -z "$swift_tokens" ]]; then
    echo "FAIL: extracted no case labels from ${SWIFT_FILE} ${swift_fn}() — the anchor no longer matches."
    failures=$((failures + 1))
    continue
  fi

  missing_in_swift="$(comm -23 <(echo "$rust_tokens") <(echo "$swift_tokens") || true)"
  missing_in_rust="$(comm -13 <(echo "$rust_tokens") <(echo "$swift_tokens") || true)"

  if [[ -n "$missing_in_swift" ]]; then
    echo "VIOLATION: ${rust_type} emits tokens ${SWIFT_FILE} has no wording for:"
    while IFS= read -r token; do echo "    ${token}"; done <<<"$missing_in_swift"
    echo "    The GUI would render the unrecognised fallback for a state the CLI really emits."
    failures=$((failures + 1))
  fi

  if [[ -n "$missing_in_rust" ]]; then
    echo "VIOLATION: ${swift_fn}() has wording for tokens ${rust_type} never emits:"
    while IFS= read -r token; do echo "    ${token}"; done <<<"$missing_in_rust"
    echo "    Either dead copy, or a typo in a case label that routes a live token to the fallback."
    failures=$((failures + 1))
  fi

  if [[ -z "$missing_in_swift" && -z "$missing_in_rust" ]]; then
    count="$(echo "$rust_tokens" | wc -l | tr -d ' ')"
    echo "ok: ${rust_type} (${count} tokens) == ${swift_fn}()"
  fi

  checked=$((checked + 1))
done

if [[ "$failures" -gt 0 ]]; then
  echo ""
  echo "FAIL: ${failures} vocabulary mismatch(es) between the Rust CLI and ${SWIFT_FILE}."
  echo "The menu-bar app is a thin client over the CLI's JSON: when the CLI learns a new state,"
  echo "the GUI must learn a word for it, or it will show a question mark for something real."
  exit 1
fi

echo ""
echo "PASS: ${checked} of 12 vocabularies verified against their Rust producer."
echo "Not verified here (no single canonical producer to diff — see this script's header):"
echo "  outcome — covered by the transcribed Swift tests only."
exit 0
