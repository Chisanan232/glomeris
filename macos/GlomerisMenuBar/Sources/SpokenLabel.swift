//
//  SpokenLabel.swift
//  GlomerisMenuBar
//
//  HORO-1451: the one place this app joins clauses into a spoken label.
//
//  ============================================================================
//  THE DEFECT THIS EXISTS FOR
//  ============================================================================
//  A row in this app is one accessibility element, so everything it shows has
//  to reach a screen-reader user as one string. Five view models built that
//  string, each with its own joining rule, and the rules disagreed:
//
//    * `CandidateRowViewModel` (HORO-1323) appended "." to the cleanup clause
//      when it did not already end in one — correct, and the only correct one.
//    * `AiPlanRowViewModel` appended each clause with a bare space. This is
//      HORO-1451: a refused suggestion carrying both an item-level skip reason
//      and a candidate-level reason code was heard as
//      "…Low confidence. that action does not apply to this kind of resource no
//      registered cleanup action for this resource kind The model's reason…" —
//      three unterminated joins in one row.
//    * `ApplyPlanView.stepAccessibilityLabel` joined with ". ", which is the
//      same bug in the other direction: the two permitted cleanup sentences
//      already end in "." and came out as "…review it and clean.. Path: …".
//    * `ApplyPlanView.resultAccessibilityLabel` interpolated the CLI's outcome
//      message with a bare space, so a refused or failed item ran into "Path:"
//      exactly as the AI plan row did.
//    * `ActionHistoryRowViewModel` appended "." to `abort_reason`
//      unconditionally, doubling it whenever Rust's text ended in one.
//
//  Sighted users see none of this: on screen these clauses are separate `Text`
//  views, and layout supplies the boundary that punctuation has to supply in
//  speech. That is what made a whole class of defect invisible to every check
//  except reading the live accessibility tree.
//
//  ============================================================================
//  WHY A COMPOSER AND NOT FIVE MORE PUNCTUATION FIXES
//  ============================================================================
//  Adding the missing period at the AI plan row would have fixed the reported
//  row and left the other three wrong, which is how the candidates list came
//  to be the only correct one in the first place. The rule is worth exactly one
//  implementation, for the same reason `humanByteCount` is (HORO-1452): with
//  five copies, "do all rows describe a refusal the same way" is unanswerable
//  without reading five functions.
//
//  ============================================================================
//  WHAT THIS IS NOT
//  ============================================================================
//  Not a rewriter. It never reads a clause's meaning, never reorders clauses,
//  never rewords or truncates one, and in particular never inspects a refusal's
//  text — the standing rule that this target may not reconstruct policy from
//  prose (`CandidateActionability.swift`) applies here too, and a composer that
//  pattern-matched Rust's wording would breach it. The only character it can
//  ever add is a single "." at the very end of a clause that has no sentence-
//  ending punctuation of its own. Everything before that character is passed
//  through byte for byte.
//
//  It is also not a source of clauses. Callers pass an ordered list, each
//  element built from the structured field it describes and labelled with that
//  field's existing axis term (`GlomerisTerm.axis`,
//  `CandidateActionability.axis`) — so the boundaries a listener hears are the
//  boundaries the data actually has, rather than punctuation sprinkled until
//  one sample sounded better.
//

import Foundation

/// Joins already-worded clauses into the single string an accessibility
/// element reads out.
enum SpokenLabel {
    /// The characters that end a sentence for this purpose.
    ///
    /// Deliberately three, and deliberately not `:` or `…`. A refusal of the
    /// shape "<action> cannot be planned for this resource: <error>" ends in a
    /// colon only if Rust's error rendered empty, and in that case the clause
    /// genuinely is unterminated and should get its period. An ellipsis is a
    /// trailing mark, not a full stop; none of the clauses composed here can
    /// end in one today, and guessing at it would be a rule nothing tests.
    private static let terminators: Set<Character> = [".", "!", "?"]

    /// Characters that may legitimately sit *after* a terminator, and so must
    /// be looked past before deciding a clause is unterminated.
    ///
    /// This is the punctuation-bearing case that matters in practice: a model
    /// may return a sentence in quotation marks, and `"…on the disk."` is
    /// terminated even though its last character is not.
    private static let closers: Set<Character> = ["\"", "'", "\u{201D}", "\u{2019}", ")", "]"]

    /// One clause per element, in reading order. `nil` and blank elements are
    /// dropped rather than producing an empty sentence — `refusal_reason` can
    /// be `Some("")` in principle, and `model_reason` can be whitespace, and
    /// neither should become a bare ". " a listener has to interpret.
    static func compose(_ clauses: [String?]) -> String {
        clauses
            .compactMap { clause in
                guard let clause else { return nil }
                let trimmed = clause.trimmingCharacters(in: .whitespacesAndNewlines)
                return trimmed.isEmpty ? nil : terminated(trimmed)
            }
            .joined(separator: " ")
    }

    /// `clause` with exactly one sentence-ending mark, adding one only if it
    /// has none. Never removes, replaces or reorders anything.
    static func terminated(_ clause: String) -> String {
        var index = clause.endIndex
        while index > clause.startIndex {
            let previous = clause.index(before: index)
            let character = clause[previous]
            if character.isWhitespace || closers.contains(character) {
                index = previous
                continue
            }
            return terminators.contains(character) ? clause : clause + "."
        }
        // Nothing but whitespace and closing punctuation. `compose` already
        // dropped the blank case, so this is a clause like `")"` — pathological
        // and unreachable from any CLI field, but a period is the honest answer
        // rather than a crash or a silent drop.
        return clause + "."
    }

    /// `"Axis: text"`, or `nil` when there is no text — so an optional CLI
    /// field can be passed straight into `compose` without the caller
    /// repeating the axis-prefix shape at every site.
    static func clause(_ axis: String, _ text: String?) -> String? {
        guard let text, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        return "\(axis): \(text)"
    }
}
