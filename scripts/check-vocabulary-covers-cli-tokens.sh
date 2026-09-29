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
# All but one of the vocabularies are checked. That one cannot be,
# honestly, because it has no single canonical producer to diff against:
# `ExecuteReport::outcome` is built from string literals at its call sites
# in src/cli/mod.rs rather than from one `as_str`-style match. Grepping
# those literals repo-wide also picks up test assertions and unrelated
# strings, so a set-equality check built on it would produce false
# failures and, worse, invite someone to loosen it until it passed. It
# stays covered by the transcribed Swift tests only, and this script
# reports "N of N+1" rather than printing a bare PASS that reads as "all of
# them verified". `TOTAL_VOCABULARIES` below is a written-down number rather
# than a count of the array, and the two are cross-checked at the end: that
# is what stops the PASS line drifting into a claim nobody updated. It had
# drifted once already — the line read "of 14" while fourteen were checked
# and a fifteenth was not, which reads as complete coverage.
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
# Exit 0 = the twenty-two checked vocabularies match. Exit 1 = drift, or an
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
#
# `<rust fn>` may be written `Type::name` when one file holds more than one
# function of that name — see `function_body`.
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
  # HORO-1506. Both of these are the wording a recovery run's outcome is
  # reported in, and both have one producer each by construction:
  # `RecoveryRunReport::stop_reason` is only ever written through
  # `stop_reason_tag`, and `RecoveryGoalRejectionReport::reason` only through
  # `GoalRejection::as_str`. The Swift side keeps the field as a `String` so an
  # older binary's token cannot fail the decode — which is exactly why the sets
  # need diffing here rather than by the type checker.
  'stopReason;;src/reporting/dto.rs;;stop_reason_tag;;StopReason'
  'goalRejection;;src/executor/goal.rs;;as_str;;GoalRejection'
  # HORO-1507. `SettingsRejection::as_str` is the sole producer of the
  # `reason` token in `SettingsRejectionReport`, and the Swift side keeps the
  # field a `String` for the same forward-compatibility reason as the two
  # above. Six tokens, not five: `goal_refused` is a deliberate catch-all for
  # a `GoalRejection` variant that `RecoverySettings` cannot map onto one of
  # its own arms, so it is a token Rust can emit and therefore a token the app
  # must have wording for.
  'settingsRejection;;src/settings/mod.rs;;as_str;;SettingsRejection'
  # HORO-1508. The two halves of the pressure-notification surface: the three
  # answers a banner's buttons send back, and the two refusals an answer can
  # meet. Both are the sole producers of their tokens —
  # `PressureEpisodeReport::response` and `PressureStatusReport::responses` go
  # through `EpisodeResponse::as_str`, and `PressureRejectionReport::reason`
  # through `EpisodeRejection::as_str`.
  #
  # Scoped to their impl blocks, and they are why the scoping exists: both live
  # in src/monitor/episode.rs, so a bare `as_str` anchor would have extracted
  # the first one twice and reported the second as verified while never looking
  # at it.
  #
  # The answers matter here more than most. An unrecognised token in this set
  # is not a question mark on a card — it is a button on a notification whose
  # label the app had to invent, at the moment the user is being asked what to
  # do about a disk that is nearly full.
  'episodeResponse;;src/monitor/episode.rs;;EpisodeResponse::as_str;;EpisodeResponse'
  'episodeRejection;;src/monitor/episode.rs;;EpisodeRejection::as_str;;EpisodeRejection'
  # HORO-1510. The Autopilot gate's own refusals, which reach the wire for the
  # first time as `RecoveryRunReport::envelope_refusal`. A different enum from
  # the `refusal` row above despite the shared name — that one is
  # `reporting::dto::RefusalReason` (why `execute` refused), this one is
  # `autopilot::gate::RefusalReason` (why the envelope would not allow it) —
  # which is why the row is scoped to its impl block and labelled distinctly.
  #
  # This is the vocabulary where the wrong wording does real harm: three of the
  # ten are policy refusing on evidence and the other seven are the envelope
  # running out of authority, and a user shown the fallback cannot tell those
  # apart. "Nothing more can be done here" and "raise the limit you set" are
  # opposite next steps.
  'autopilotRefusal;;src/autopilot/gate.rs;;RefusalReason::as_str;;AutopilotRefusalReason'
  # HORO-1511. The three developer-workspace vocabularies, each with one `tag`
  # producer. Two of the three live in src/workspace/branch.rs, so those rows
  # are scoped to their impl blocks for the same reason the episode rows are:
  # an unscoped `fn tag(` anchor would extract `UpstreamState`'s tokens twice
  # and report `MergedState` as verified without ever reading it.
  #
  # Every one of these sets contains `unknown`, and that is why they are here.
  # `unknown` means a probe could not answer, and Rust deliberately counts it as
  # *possible* work in progress rather than as an absence of it. A token that
  # reached the fallback would be shown as "the CLI reported a state this app has
  # no wording for" — survivable — but a token MISSING from Rust and present here
  # is the dangerous direction on this surface: it means wording exists for a
  # state that cannot occur, next to eight that can, and nobody would notice
  # which of them a real worktree was getting.
  'worktreeActivity;;src/workspace/group.rs;;ActivityState::tag;;ActivityState'
  'worktreeUpstream;;src/workspace/branch.rs;;UpstreamState::tag;;UpstreamState'
  'worktreeMerged;;src/workspace/branch.rs;;MergedState::tag;;MergedState'
  # HORO-1545. Two more from the same file, scoped for the same reason: three
  # `fn tag(` now live in src/workspace/branch.rs and an unscoped anchor would
  # take the first every time.
  #
  # `PatchEquivalence` is the one vocabulary on this surface whose strongest
  # token comes close to sounding like permission — "the same work is already
  # on the other branch" — and it is four tokens precisely so it cannot be read
  # as a boolean. `not_applicable` means there was no question to ask, and
  # wording that let it read as "no" would turn a merged branch into an
  # unintegrated one on the way to the screen. The dangerous direction here is
  # the same as for the three rows above, and worse: a token missing from Rust
  # but present in Swift would mean the app has a sentence for an integration
  # state that cannot happen, sitting beside three that can.
  #
  # `EquivalenceMethod` is two tokens and exists because they are not equally
  # strong — one matched every commit, the other only found the files the same.
  # It has no `unknown`: there is no method when there is no equivalence, and
  # the field is absent rather than tagged in that case.
  'worktreeEquivalence;;src/workspace/branch.rs;;PatchEquivalence::tag;;PatchEquivalence'
  'worktreeEquivalenceMethod;;src/workspace/branch.rs;;EquivalenceMethod::tag;;EquivalenceMethod'
)

# One more than the number of rows above: `outcome` has no producer to diff
# against (see this script's header). Cross-checked against the rows actually
# walked, at the end.
TOTAL_VOCABULARIES=23

# Print the body of a function, from its `fn <name>` line to the line
# where brace depth returns to zero. Brace counting rather than an indent
# heuristic, because `as_str` appears in several impl blocks and a later
# one must not be picked up by accident.
#
# A Rust function may be named `Type::name`, which restricts the search to
# that type's inherent `impl` block. Needed because the anchor takes the
# FIRST match in the file, and src/monitor/episode.rs defines two `as_str` —
# one on `EpisodeResponse` and one on `EpisodeRejection`. Unscoped, the
# second row would have silently re-extracted the first function's tokens
# and then reported a mismatch against wording that was perfectly correct,
# which is worse than not checking it: the obvious way to make that red go
# away is to edit the Swift table.
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
  local anchor impl_anchor=""
  if [[ "$lang" == "rust" ]]; then
    if [[ "$fn_name" == *"::"* ]]; then
      # `impl Type {` — with the brace, so `impl Display for Type {` cannot
      # match, and with the type name, so a trait impl on some other type
      # cannot either.
      impl_anchor="impl ${fn_name%%::*} {"
      anchor="fn ${fn_name##*::}("
    else
      anchor="fn ${fn_name}("
    fi
  else
    # Stops at the parameter NAME, not its type: `impactTier` takes a
    # `String?`, so an anchor ending in `String)` silently matched nothing
    # for it — and a non-matching anchor here produces an empty extraction,
    # which this script reports as a failure rather than a pass.
    anchor="static func ${fn_name}(_ token:"
  fi

  awk -v anchor="$anchor" -v impl_anchor="$impl_anchor" '
    function braces(line,   opened, closed) {
      opened = gsub(/\{/, "{", line)
      closed = gsub(/\}/, "}", line)
      return opened - closed
    }
    # "impl" only when a scope was asked for; otherwise start looking for the
    # function immediately, exactly as before.
    BEGIN { phase = (impl_anchor == "" ? "fn" : "impl"); impl_depth = 0; depth = 0 }
    phase == "impl" {
      if (index($0, impl_anchor) > 0) {
        phase = "fn"
        impl_depth = braces($0)
      }
      next
    }
    phase == "fn" {
      if (index($0, anchor) > 0) {
        phase = "body"
      } else {
        # Leaving the impl block without having found the function prints
        # nothing, which this script reports as a failed extraction rather
        # than as agreement.
        impl_depth += braces($0)
        if (impl_anchor != "" && impl_depth <= 0) exit
        next
      }
    }
    phase == "body" {
      print
      depth += braces($0)
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

if [[ "$((checked + 1))" -ne "$TOTAL_VOCABULARIES" ]]; then
  echo ""
  echo "FAIL: ${checked} vocabularies were checked, but TOTAL_VOCABULARIES says"
  echo "      ${TOTAL_VOCABULARIES} exist and exactly one of them (outcome) is unverifiable."
  echo "      A row was added or removed without updating that number, so the PASS"
  echo "      line below would overstate or understate the coverage."
  exit 1
fi

echo ""
echo "PASS: ${checked} of ${TOTAL_VOCABULARIES} vocabularies verified against their Rust producer."
echo "Not verified here (no single canonical producer to diff — see this script's header):"
echo "  outcome — covered by the transcribed Swift tests only."
exit 0
