//
//  SpokenLabelTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1451. `SpokenLabel` is the one place this app decides where one spoken
//  clause ends and the next begins, so this file holds the rule itself and the
//  five row files hold only "this row's clauses go through it, in this order".
//
//  Two things are asserted, and the second matters more than the first:
//
//  1. The rule. A clause with no sentence-ending mark gets exactly one; a
//     clause that has one keeps exactly the one it has; nothing else moves.
//  2. That it never rewrites a refusal. Every shape the CLI can put in
//     `refusal_reason`, `skip_reason` or an outcome message appears in the
//     composed label byte for byte, with a period appended and not one other
//     character changed. A composer that "tidied" Rust's words would breach the
//     same rule `CandidateActionability` is under — this target may not restate
//     policy, and it may not paraphrase policy's own explanation of itself
//     either.
//
//  The fixtures below are Rust's literal strings, not paraphrases of them, and
//  `testTheseFixturesAreTheWordsRustActuallyEmits` reads the Rust sources to
//  keep that true. Without it this file could pass while asserting termination
//  of strings the product never produces.
//

import XCTest

final class SpokenLabelTests: XCTestCase {
    // MARK: - Fixtures: what the CLI actually says

    /// `executable_fields`, `src/reporting/dto.rs` — the reason a PROTECTED
    /// resource is never executable. `{reason}` is a `PolicyReason` token.
    private static let protectedRefusal = "PROTECTED: protected_credential_material"

    /// `executable_fields` — no action is registered for the kind at all.
    private static let noActionForKind = "no registered cleanup action for this resource kind"

    /// `executable_fields` — `Action::plan` returned an `ActionError`, rendered
    /// with the planner's own `Display`. Ends in a colon when that error renders
    /// empty, which is why a colon is not treated as a sentence ending.
    private static let cannotBePlanned =
        "cargo.clean.target_dir cannot be planned for this resource: "
        + "Cargo.toml is missing from /Users/dev/proj"

    /// `executor::structural_refusal` — the empty-plan refusal, verbatim across
    /// its Rust line continuation.
    private static let emptyPlanRefusal =
        "refusing to execute an empty plan: a plan with zero steps would otherwise fall "
        + "through to a false ExecutionOutcome::Succeeded report despite mutating nothing"

    /// `executor::structural_refusal` — the multi-step refusal. The one shape
    /// that ends in a closing parenthesis, so the period has to land after it.
    private static let multiStepRefusal =
        "refusing to execute a 2-step plan: multi-step plans are not yet supported "
        + "(partial-deletion byte accounting would be discarded on a later-step failure)"

    /// `executor::unscoped_run_tool_refusal`. The fail-closed rule's own words:
    /// this is the string a user is most likely to quote back in a bug report,
    /// so it is the one least allowed to be reworded.
    private static let unscopedRefusal =
        "refusing to run brew: this step has no scoped_path, so its identity cannot be "
        + "revalidated before mutation — an unscoped mutating action is never executed "
        + "regardless of policy class"

    /// `executor::decoy_scoped_path_refusal`.
    private static let decoyRefusal =
        "refusing to run brew: scoped_path /Users/dev/elsewhere does not appear in this "
        + "step's own args — the executor cannot confirm what this command will actually mutate"

    /// `src/cli/mod.rs` — the planner's `skip_reason` for a protected resource.
    /// Carries an em dash, which is not a sentence ending.
    private static let protectedSkipReason =
        "PROTECTED — no cleanup action is ever rendered for this resource"

    /// `src/cli/mod.rs` — the planner's other two `skip_reason` shapes.
    private static let noActionForActionId = "no registered action for this action id"
    private static let noActionForResourceKind = "no registered action for this resource kind"

    /// Every one of them. A refusal is the clause a listener can least afford
    /// to have run into the next one, so the rule is checked against all of
    /// them rather than against a representative.
    private static var everyCliRefusal: [String] {
        [
            protectedRefusal,
            noActionForKind,
            cannotBePlanned,
            emptyPlanRefusal,
            multiStepRefusal,
            unscopedRefusal,
            decoyRefusal,
            protectedSkipReason,
            noActionForActionId,
            noActionForResourceKind,
        ]
    }

    // MARK: - The rule

    /// The defect in one assertion: the CLI's fragments carry no full stop, so
    /// without this they run into whatever is spoken next.
    func testAnUnterminatedClauseGetsExactlyOnePeriod() {
        XCTAssertEqual(SpokenLabel.terminated("Safety: Protected"), "Safety: Protected.")
        XCTAssertEqual(SpokenLabel.terminated(Self.protectedRefusal), Self.protectedRefusal + ".")
    }

    /// The same defect in the other direction — `ApplyPlanView` used to join
    /// with `". "`, and the two permitted cleanup sentences already end in one.
    func testAnAlreadyTerminatedClauseIsUntouched() {
        for terminated in [
            "Glomeris will ask you to confirm this before anything runs.",
            "Glomeris is willing to run this. Open the row to review it and clean.",
            "Is that really what you meant?",
            "Careful!",
        ] {
            XCTAssertEqual(SpokenLabel.terminated(terminated), terminated)
        }
    }

    /// The punctuation-bearing case that actually occurs: a provider returns a
    /// quoted sentence, and `"…on the disk."` is terminated even though its
    /// last character is not a full stop.
    func testATerminatorInsideClosingPunctuationCounts() {
        for quoted in [
            "The model said \"nothing is using it.\"",
            "The model said 'nothing is using it.'",
            "The model said \u{201C}nothing is using it.\u{201D}",
            "It is stale (the lock file is gone.)",
            "It is stale [see the note above.]",
        ] {
            XCTAssertEqual(
                SpokenLabel.terminated(quoted), quoted,
                "a mark sitting behind closing punctuation is still a mark"
            )
        }
    }

    /// The converse, and the reason the closers are looked *past* rather than
    /// treated as terminators themselves: a parenthesis at the end of an
    /// unterminated clause does not end the sentence, and the period belongs
    /// after it rather than inside it.
    func testAClosingBracketOverUnterminatedTextStillGetsItsPeriod() {
        XCTAssertEqual(
            SpokenLabel.terminated(Self.multiStepRefusal),
            Self.multiStepRefusal + ".",
            "the period goes after the closing parenthesis, not inside it"
        )
        XCTAssertTrue(SpokenLabel.terminated(Self.multiStepRefusal).hasSuffix("failure)."))
    }

    /// A colon is not a sentence ending. `"<action> cannot be planned for this
    /// resource: <error>"` ends in one exactly when Rust's error rendered empty,
    /// and that clause genuinely is unfinished.
    func testAColonIsNotASentenceEnding() {
        let truncated = "cargo.clean.target_dir cannot be planned for this resource:"
        XCTAssertEqual(SpokenLabel.terminated(truncated), truncated + ".")
    }

    /// Documented rather than discovered: an ellipsis is a trailing mark, not a
    /// full stop, and this treats it as unterminated. No field composed here
    /// can end in one today — the only ellipses in this app are in its own
    /// loading copy, which never reaches a row label — so the alternative would
    /// be a rule nothing exercises.
    func testAnEllipsisIsTreatedAsUnterminated() {
        XCTAssertEqual(SpokenLabel.terminated("Still measuring\u{2026}"), "Still measuring\u{2026}.")
    }

    // MARK: - Composition

    func testNilAndBlankClausesAreDroppedRatherThanSpoken() {
        XCTAssertEqual(
            SpokenLabel.compose(["Kind: node_modules", nil, "", "   ", "\n\t", "Path: /tmp/x"]),
            "Kind: node_modules. Path: /tmp/x.",
            "a dropped clause must leave no residue — not a bare period, not a double space"
        )
        XCTAssertEqual(SpokenLabel.compose([nil, "", "  "]), "")
    }

    /// One space between clauses, none at either end. A listener hears a
    /// doubled space as nothing at all, but it is the visible trace of a join
    /// rule that has lost track of what it already added.
    func testClausesAreJoinedBySingleSpacesWithNoStrayWhitespace() {
        let composed = SpokenLabel.compose([
            "  Rust build output  ",
            "Safety: Protected\n",
            "\tPath: /Users/dev/proj/target",
        ])
        XCTAssertEqual(composed, "Rust build output. Safety: Protected. Path: /Users/dev/proj/target.")
        XCTAssertFalse(composed.contains("  "), composed)
        XCTAssertEqual(composed, composed.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// A whole row-shaped label, for every refusal the CLI can produce, checked
    /// against the string written out by hand. This is the property the five row
    /// labels rely on and the one the defect broke at three of the five, and it
    /// is asserted as an equality rather than as a search for "." so that a
    /// clause gaining or losing a mark cannot pass.
    func testARowShapedLabelIsPunctuatedTheSameWayForEveryRefusal() throws {
        for refusal in Self.everyCliRefusal {
            let composed = SpokenLabel.compose([
                "Rust build output",
                "Safety: Protected",
                SpokenLabel.clause(CandidateActionability.axis, refusal),
                "Path: cargo_target_dir:/Users/dev/proj/target",
            ])
            XCTAssertEqual(
                composed,
                "Rust build output. Safety: Protected. Cleanup: \(refusal). "
                    + "Path: cargo_target_dir:/Users/dev/proj/target.",
                "the row's punctuation depends on which refusal it carries"
            )
            XCTAssertFalse(composed.contains(".."), composed)
        }
    }

    // MARK: - It never rewrites a refusal

    /// The invariant that matters most here. For every shape the CLI can
    /// produce, the composed clause is the refusal plus one period and nothing
    /// else — asserted by reconstruction, not by `contains`, so a composer that
    /// also normalised a dash or collapsed a double space would fail.
    func testEveryCliRefusalSurvivesCompositionByteForByte() {
        for refusal in Self.everyCliRefusal {
            let composed = SpokenLabel.compose([refusal])
            XCTAssertEqual(composed, refusal + ".", "the refusal's own words were altered")
            XCTAssertEqual(
                composed.count, refusal.count + 1,
                "exactly one character may be added"
            )
        }
    }

    /// The em dash in `unscoped_run_tool_refusal` and the `::` in the empty-plan
    /// refusal are the characters a well-meaning "tidy the punctuation" change
    /// would most likely touch, so they are named explicitly rather than left
    /// to the length check above.
    func testTheFailClosedRefusalKeepsItsOwnPunctuation() {
        let composed = SpokenLabel.compose([
            SpokenLabel.clause(CandidateActionability.axis, Self.unscopedRefusal),
        ])
        XCTAssertEqual(composed, "Cleanup: " + Self.unscopedRefusal + ".")
        XCTAssertTrue(composed.contains("\u{2014} an unscoped mutating action"), composed)
        XCTAssertTrue(
            SpokenLabel.compose([Self.emptyPlanRefusal])
                .contains("ExecutionOutcome::Succeeded"),
            "a refusal naming a Rust type must keep naming it"
        )
    }

    /// A long reason is spoken in full. VoiceOver users can interrupt an
    /// utterance; they cannot recover a clause the app decided to shorten, and
    /// a truncated refusal is a refusal whose reason has been withheld.
    func testALongReasonIsNeitherTruncatedNorReflowed() {
        let long = "cargo.clean.target_dir cannot be planned for this resource: "
            + String(repeating: "the manifest names a workspace member that is missing, ", count: 40)
            + "and the planner therefore has nothing to delete"
        XCTAssertGreaterThan(long.count, 2000)

        let composed = SpokenLabel.compose(["Rust build output", long, "Path: /tmp/x"])
        XCTAssertTrue(composed.contains(long), "the reason was altered somewhere in its middle")
        XCTAssertEqual(
            composed,
            "Rust build output. " + long + ". Path: /tmp/x.",
            "a long clause is composed exactly like a short one"
        )
    }

    /// Interior punctuation and interior newlines are the clause's own content.
    /// Only the two ends are the composer's business.
    func testInteriorPunctuationAndNewlinesArePassedThrough() {
        let messy = "it failed; then it failed again \u{2014} \"twice\", in fact\nand the log says so"
        XCTAssertEqual(SpokenLabel.compose([messy]), messy + ".")
    }

    // MARK: - The axis helper

    func testTheAxisHelperPrefixesTheAxisAndDropsAbsentText() {
        XCTAssertEqual(SpokenLabel.clause("Safety", "Protected"), "Safety: Protected")
        XCTAssertNil(SpokenLabel.clause("Safety", nil))
        XCTAssertNil(SpokenLabel.clause("Safety", ""))
        XCTAssertNil(
            SpokenLabel.clause("Safety", "   "),
            "an axis with nothing after it is worse than no axis: it announces a fact and "
                + "then withholds it"
        )
    }

    // MARK: - Anti-vacuity

    /// Everything above would also pass against a composer that joined with
    /// `". "` unconditionally, or against one that joined with a bare space, if
    /// the fixtures happened to be shaped conveniently. These two assertions
    /// fail against both, so a regression to either implementation is caught
    /// here rather than only in the live accessibility tree.
    func testTheCompositionIsNeitherABareSpaceJoinNorAnUnconditionalPeriodJoin() {
        let clauses = ["Rust build output", Self.protectedRefusal, "Path: /tmp/x"]

        let bareSpaceJoin = clauses.joined(separator: " ")
        let unconditionalPeriodJoin = clauses.joined(separator: ". ") + "."
        let composed = SpokenLabel.compose(clauses)

        XCTAssertNotEqual(composed, bareSpaceJoin, "this is the HORO-1451 defect")
        XCTAssertEqual(composed, unconditionalPeriodJoin, "no clause here was already terminated")

        // …and with an already-terminated clause the two diverge the other way,
        // which is the ApplyPlanView half of the same defect.
        let withSentence = ["Rust build output", CandidateActionability.readyToClean.sentence, "Path: /tmp/x"]
        XCTAssertNotEqual(
            SpokenLabel.compose(withSentence),
            withSentence.joined(separator: ". ") + ".",
            "a period was added to a clause that already had one"
        )
        XCTAssertFalse(SpokenLabel.compose(withSentence).contains(".."), "doubled mark")
    }

    /// Keeps this file honest. The fixtures claim to be the CLI's own words; if
    /// a Rust literal is reworded and this file is not, the termination rule
    /// above would be asserted against strings the product no longer emits.
    ///
    /// Only the single-line literals are checked — Rust wraps the three longer
    /// refusals across a `\` continuation, so their text does not appear
    /// contiguously in the source and a grep for it would fail on a file that
    /// is perfectly correct.
    func testTheseFixturesAreTheWordsRustActuallyEmits() throws {
        let repoRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root

        let sources = try ["src/reporting/dto.rs", "src/cli/mod.rs"]
            .map { try String(contentsOf: repoRoot.appendingPathComponent($0), encoding: .utf8) }
            .joined(separator: "\n")

        for literal in [
            Self.noActionForKind,
            Self.protectedSkipReason,
            Self.noActionForActionId,
            Self.noActionForResourceKind,
        ] {
            XCTAssertTrue(
                sources.contains(literal),
                "no Rust source emits \u{201C}\(literal)\u{201D} any more"
            )
        }

        // The two format shapes, checked as their literal halves.
        XCTAssertTrue(sources.contains("PROTECTED: {reason}"), "the PROTECTED refusal shape moved")
        XCTAssertTrue(
            sources.contains("cannot be planned for this resource: {e}"),
            "the un-plannable refusal shape moved"
        )
    }
}
