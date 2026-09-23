//
//  CandidateActionabilityTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1323. The interesting property of `CandidateActionability` is not
//  that it maps four field combinations to four cases — it is that a
//  refusal's wording is the CLI's own, and that two refusals the CLI words
//  differently stay distinguishable afterwards. That is the thing the ticket
//  asks for and the thing a well-meaning refactor would break, by reaching
//  for a tidy Swift-side sentence per policy class.
//
//  So the fixtures below use the exact strings `executable_fields`
//  (`src/reporting/dto.rs`) produces, transcribed from the Rust source and
//  cross-checked against a live `detect --json` run. If Rust's wording
//  changes these tests keep passing, which is correct: they assert
//  pass-through, not a particular sentence.
//

import AppKit
import XCTest

final class CandidateActionabilityTests: XCTestCase {
    // MARK: - Fixtures: the four `refusal_reason` shapes Rust can produce
    //
    // `executable_fields` has exactly four early returns, in this order.

    /// `PolicyClass::Protected` — the product's central safety promise, and
    /// per the ticket the one a user most needs to hear.
    private static let protectedRefusal = "PROTECTED: protected_credential_material"

    /// No action registered for the resource kind.
    private static let noActionRefusal = "no registered cleanup action for this resource kind"

    /// The planner itself declined (e.g. a cargo target dir whose Cargo.toml
    /// has since vanished).
    private static let plannerRefusal =
        "cargo.clean.target_dir cannot be planned for this resource: manifest no longer exists"

    /// `executor::structural_refusal`'s own words — the Homebrew case
    /// HORO-1358 exposed, quoted from that ticket's comment on HORO-1323.
    private static let structuralRefusal =
        "refusing to run brew: this step has no scoped_path, so its identity cannot be "
        + "revalidated before mutation — an unscoped mutating action is never executed "
        + "regardless of policy class"

    private func refused(_ reason: String) -> CandidateActionability {
        CandidateActionability(executable: false, requiresConfirmation: false, refusalReason: reason)
    }

    // MARK: - The two permitted cases

    func testExecutableWithoutConfirmationIsReadyToClean() {
        let actionability = CandidateActionability(
            executable: true,
            requiresConfirmation: false,
            refusalReason: nil
        )
        XCTAssertEqual(actionability, .readyToClean)
    }

    func testExecutableWithConfirmationAsksFirst() {
        let actionability = CandidateActionability(
            executable: true,
            requiresConfirmation: true,
            refusalReason: nil
        )
        XCTAssertEqual(actionability, .asksFirstThenCleans)
    }

    /// The two permitted cases are the ticket's "needs confirmation" versus
    /// "allowed" distinction, and collapsing them is one of the four
    /// collapses it names. Asserted on the sentences rather than the cases,
    /// because the sentence is what a user actually receives.
    func testPermittedCasesDoNotShareASentence() {
        XCTAssertNotEqual(
            CandidateActionability(executable: true, requiresConfirmation: false, refusalReason: nil).sentence,
            CandidateActionability(executable: true, requiresConfirmation: true, refusalReason: nil).sentence
        )
    }

    /// A refusal reason arriving alongside `executable: true` must not turn a
    /// permitted resource into a refused one. Rust returns `None` there, so
    /// this is defence against a newer CLI that starts explaining something
    /// about a resource it is still willing to clean — the reason is not the
    /// gating field and must not behave like one.
    func testExecutableWinsOverAStrayRefusalReason() {
        let actionability = CandidateActionability(
            executable: true,
            requiresConfirmation: false,
            refusalReason: Self.protectedRefusal
        )
        XCTAssertEqual(actionability, .readyToClean)
        XCTAssertNil(actionability.term)
    }

    // MARK: - Refusals are passed through verbatim

    func testProtectedRefusalIsTheCLIsOwnText() {
        XCTAssertEqual(refused(Self.protectedRefusal).sentence, Self.protectedRefusal)
    }

    func testStructuralRefusalIsTheCLIsOwnText() {
        XCTAssertEqual(refused(Self.structuralRefusal).sentence, Self.structuralRefusal)
    }

    /// The property the whole design rests on: all four shapes survive
    /// intact, so they remain four distinct messages rather than one generic
    /// "cannot be cleaned". A Swift-side reconstruction per policy class
    /// would fail this the moment two shapes mapped to the same sentence.
    func testAllFourRefusalShapesStayDistinct() {
        let shapes = [
            Self.protectedRefusal,
            Self.noActionRefusal,
            Self.plannerRefusal,
            Self.structuralRefusal,
        ]
        let sentences = shapes.map { refused($0).sentence }
        XCTAssertEqual(sentences, shapes)
        XCTAssertEqual(Set(sentences).count, shapes.count)
    }

    /// The specific confusion HORO-1358 created and this ticket must not
    /// recreate: an AUTO_SAFE resource refused for a structural reason, and a
    /// PROTECTED one refused by policy, are both non-executable with an empty
    /// `offered_actions`. Nothing but the text tells them apart.
    func testProtectedAndStructuralRefusalsAreNotConflated() {
        let policy = CandidateActionability(
            executable: false,
            offeredActions: [],
            refusalReason: Self.protectedRefusal
        )
        let structural = CandidateActionability(
            executable: false,
            offeredActions: [],
            refusalReason: Self.structuralRefusal
        )
        XCTAssertNotEqual(policy, structural)
        XCTAssertNotEqual(policy.sentence, structural.sentence)
        XCTAssertNotEqual(policy.term?.explanation, structural.term?.explanation)
    }

    /// Both refusals must reach the reader, so neither may be silently
    /// swallowed by a `nil` badge.
    func testEveryRefusalShapeProducesABadge() {
        for reason in [Self.protectedRefusal, Self.noActionRefusal, Self.plannerRefusal, Self.structuralRefusal] {
            let term = refused(reason).term
            XCTAssertNotNil(term, "no badge for: \(reason)")
            XCTAssertEqual(term?.explanation, reason)
        }
    }

    // MARK: - The reasonless fallback

    func testMissingReasonFallsBackToItsOwnWording() {
        let actionability = CandidateActionability(
            executable: false,
            requiresConfirmation: false,
            refusalReason: nil
        )
        XCTAssertEqual(actionability, .refusedWithoutStatedReason)
        XCTAssertFalse(actionability.sentence.isEmpty)
    }

    /// An empty string is a reason the CLI did not actually give, and
    /// rendering it would leave a "Cannot be cleaned" badge whose
    /// explanation is blank.
    func testEmptyReasonIsTreatedAsNoReason() {
        XCTAssertEqual(
            CandidateActionability(executable: false, requiresConfirmation: false, refusalReason: ""),
            .refusedWithoutStatedReason
        )
    }

    /// The fallback wording must never appear in place of a reason the CLI
    /// did give — that would be the ticket's defect with extra steps.
    func testFallbackWordingNeverReplacesARealReason() {
        let fallback = CandidateActionability.refusedWithoutStatedReason.sentence
        for reason in [Self.protectedRefusal, Self.noActionRefusal, Self.plannerRefusal, Self.structuralRefusal] {
            XCTAssertNotEqual(refused(reason).sentence, fallback)
        }
    }

    // MARK: - The badge

    func testPermittedCasesCarryNoBadge() {
        XCTAssertNil(CandidateActionability.readyToClean.term)
        XCTAssertNil(CandidateActionability.asksFirstThenCleans.term)
    }

    /// A refusal is Glomeris working correctly, so it is never toned as an
    /// error. The same rule `GlomerisVocabulary.safety("PROTECTED")` follows,
    /// asserted here too because this badge sits beside that one.
    func testRefusalBadgeIsGuardedAndNeverCritical() {
        let term = refused(Self.protectedRefusal).term
        XCTAssertEqual(term?.tone, .guarded)
        XCTAssertNotEqual(term?.tone, .critical)
        XCTAssertNotEqual(term?.tone, .warning)
    }

    /// Colour is never the sole carrier of state: the badge has a symbol and
    /// a plain-language title as well, so it survives greyscale and
    /// VoiceOver.
    func testRefusalBadgeCarriesASymbolAndATitle() {
        let term = refused(Self.noActionRefusal).term
        XCTAssertEqual(term?.symbolName, "slash.circle.fill")
        XCTAssertEqual(term?.title, "Cannot be cleaned")
        XCTAssertEqual(term?.axis, "Cleanup")
    }

    /// The badge's VoiceOver label is what a screen-reader user hears on a
    /// non-deletable row, and it has to carry the reason, not just the state.
    func testRefusalBadgeAccessibilityLabelCarriesTheReason() {
        let term = refused(Self.structuralRefusal).term
        XCTAssertTrue(term?.accessibilityLabel.contains(Self.structuralRefusal) == true)
        XCTAssertTrue(term?.accessibilityLabel.contains("Cleanup") == true)
    }

    // MARK: - The offered-actions form agrees with the boolean form

    func testOfferedActionsFormReadsRequiresConfirmation() {
        XCTAssertEqual(
            CandidateActionability(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: true)],
                refusalReason: nil
            ),
            .asksFirstThenCleans
        )
        XCTAssertEqual(
            CandidateActionability(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: false)],
                refusalReason: nil
            ),
            .readyToClean
        )
    }

    /// `first`, not `contains` — the wording must describe the action that
    /// will actually run.
    ///
    /// Only `offeredActions.first` can execute: `CandidateDetailViewModel`
    /// takes `actionId` from it, gates the confirmation alert on its
    /// `requiresConfirmation`, and passes its id to `execute`. So for a
    /// hypothetical two-action resource where the first does not ask and the
    /// second does, "asks first" would be a promise the button then breaks —
    /// the overview would say the deletion needs confirming and pressing Clean
    /// would delete immediately, unconfirmed. Not reachable from Rust today
    /// (at most one action is offered); pinned because the failure direction
    /// is a silently skipped consent step on the one control that deletes
    /// files.
    func testConfirmationDescribesTheActionThatWouldActuallyRun() {
        XCTAssertEqual(
            CandidateActionability(
                executable: true,
                offeredActions: [
                    OfferedActionDto(actionId: "a", requiresConfirmation: false),
                    OfferedActionDto(actionId: "b", requiresConfirmation: true),
                ],
                refusalReason: nil
            ),
            .readyToClean
        )
        XCTAssertEqual(
            CandidateActionability(
                executable: true,
                offeredActions: [
                    OfferedActionDto(actionId: "a", requiresConfirmation: true),
                    OfferedActionDto(actionId: "b", requiresConfirmation: false),
                ],
                refusalReason: nil
            ),
            .asksFirstThenCleans
        )
    }

    /// The two initialisers must not disagree. The array form is what the
    /// candidates list and the AI plan card use; the boolean form is what the
    /// detail sheet uses, having already reduced the array itself. If those
    /// reductions ever diverge, the overview and the button describe different
    /// behaviour for the same resource — which is precisely the defect class
    /// this ticket is about, reintroduced one layer down.
    func testBothInitialisersAgreeOnTheSameOfferedActions() {
        let shapes: [[OfferedActionDto]] = [
            [],
            [OfferedActionDto(actionId: "a", requiresConfirmation: false)],
            [OfferedActionDto(actionId: "a", requiresConfirmation: true)],
            [
                OfferedActionDto(actionId: "a", requiresConfirmation: false),
                OfferedActionDto(actionId: "b", requiresConfirmation: true),
            ],
            [
                OfferedActionDto(actionId: "a", requiresConfirmation: true),
                OfferedActionDto(actionId: "b", requiresConfirmation: false),
            ],
        ]

        for actions in shapes {
            // The reduction the detail sheet performs, spelled out here so the
            // two are compared rather than assumed equal.
            let asTheDetailSheetReducesIt = CandidateActionability(
                executable: true,
                requiresConfirmation: actions.first?.requiresConfirmation ?? false,
                refusalReason: nil
            )
            let asTheOverviewReducesIt = CandidateActionability(
                executable: true,
                offeredActions: actions,
                refusalReason: nil
            )
            XCTAssertEqual(
                asTheOverviewReducesIt, asTheDetailSheetReducesIt,
                "the overview and the Clean button disagree for \(actions.map(\.actionId))"
            )
        }
    }

    func testNoOfferedActionsWithExecutableTrueDoesNotAskFirst() {
        // Not a state Rust produces — `executable: true` always comes with
        // one offered action. Pinned anyway because the reduction to a
        // boolean must not invent a confirmation requirement out of an empty
        // array, which is the direction that would break `--confirm-ask`.
        XCTAssertEqual(
            CandidateActionability(executable: true, offeredActions: [], refusalReason: nil),
            .readyToClean
        )
    }

    // MARK: - The badge term is held to the vocabulary's own rules

    // This is the first `GlomerisTerm` built outside `GlomerisVocabulary`, so
    // it is invisible to `GlomerisVocabularyTests`' sweeps — those enumerate
    // `allAxes`, which is a list of vocabulary lookups. The sweeps exist
    // because of failure modes that are silent (a mistyped SF Symbol renders
    // as nothing at all; two axes sharing a name make a label ambiguous), so a
    // term outside them is a term with no guard. The four tests below apply
    // the same rules to this one.

    private var badgeTerms: [GlomerisTerm] {
        [
            CandidateActionability.refused(reason: "PROTECTED: protected_infra_state"),
            .refusedWithoutStatedReason,
        ].compactMap(\.term)
    }

    /// A mistyped SF Symbol name is not a compile error — it renders as
    /// nothing, and this badge is the one non-text marker distinguishing a
    /// refused row on the overview, so it failing silently would take the
    /// ticket's whole visual signal with it.
    func testTheBadgeSymbolIsARealSFSymbol() throws {
        XCTAssertFalse(badgeTerms.isEmpty)
        for term in badgeTerms {
            let name = try XCTUnwrap(term.symbolName, "a rendered badge needs a symbol")
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not a real SF Symbol, so the badge would render as nothing at all"
            )
        }
    }

    /// `testEachAxisHasADistinctName` holds the vocabulary's thirteen axes to
    /// this; a fourteenth axis introduced elsewhere has to meet it too, or two
    /// clauses of the same accessibility label would carry the same prefix.
    func testTheAxisNameCollidesWithNoVocabularyAxis() {
        let vocabularyAxes = [
            GlomerisVocabulary.pressureAxis,
            GlomerisVocabulary.safetyAxis,
            GlomerisVocabulary.completenessAxis,
            GlomerisVocabulary.confidenceAxis,
            GlomerisVocabulary.regenerabilityAxis,
            GlomerisVocabulary.impactAxis,
            GlomerisVocabulary.impactTierAxis,
            GlomerisVocabulary.monitorAxis,
            GlomerisVocabulary.outcomeAxis,
            GlomerisVocabulary.refusalAxis,
            GlomerisVocabulary.sourceAxis,
            GlomerisVocabulary.kindAxis,
            GlomerisVocabulary.reasonAxis,
            GlomerisVocabulary.llmCheckAxis,
        ]
        XCTAssertFalse(
            vocabularyAxes.contains(CandidateActionability.axis),
            "\(CandidateActionability.axis) already names a vocabulary axis"
        )
    }

    /// The badge shares a row with up to three others in a 340pt panel, so the
    /// same chip-width rule the vocabulary axes are held to applies, and the
    /// title must not leak the raw field it came from.
    func testTheBadgeWordingIsChipSizedAndLeaksNoTags() {
        for term in badgeTerms {
            XCTAssertLessThanOrEqual(term.title.count, 34, "\(term.title) is too long for a chip")
            XCTAssertFalse(term.explanation.isEmpty)
            XCTAssertTrue(term.accessibilityLabel.contains(":"))
            XCTAssertFalse(term.title.contains("_"), "the title leaks a raw tag")
            XCTAssertFalse(term.title.lowercased().contains("executable"))
        }
    }

    /// The symbol has to be distinguishable from the safety badge sitting
    /// beside it, or the two axes differ by colour alone — which is exactly
    /// what makes the AUTO_SAFE-but-refused Homebrew row unreadable in
    /// greyscale.
    func testTheBadgeSymbolDiffersFromEverySafetySymbol() throws {
        let safetySymbols = Set(
            ["AUTO_SAFE", "ASK", "PROTECTED", "UNKNOWN_INCOMPLETE"]
                .compactMap { GlomerisVocabulary.safety($0).symbolName }
        )
        for term in badgeTerms {
            let name = try XCTUnwrap(term.symbolName)
            XCTAssertFalse(
                safetySymbols.contains(name),
                "\(name) is also a safety symbol, so the two badges differ by colour alone"
            )
        }
    }

    // MARK: - This type cannot gate anything

    /// The structural argument from the file header, as a test rather than a
    /// comment: `CandidateActionability` exposes no boolean, so there is
    /// nothing here for a `.disabled(...)` to read. The Clean button's
    /// enablement stays a direct field read of `executable`.
    ///
    /// Greps the source because that is what the claim is about — the shape
    /// of the type's surface, not the value of one instance.
    func testTypeExposesNoBooleanAndCannotBuildAControl() {
        let source = Self.readSource("CandidateActionability.swift")
        let code = source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        XCTAssertFalse(code.contains("import SwiftUI"))
        XCTAssertFalse(code.contains("import AppKit"))
        XCTAssertFalse(code.contains("Button"))
        XCTAssertFalse(code.contains(".disabled("))
        XCTAssertFalse(code.contains("GlomerisClient"))
        // No `-> Bool` computed property or function: every accessor returns
        // wording. `var isX: Bool` and `func isX() -> Bool` both contain
        // "Bool", so one absent substring covers both shapes.
        XCTAssertFalse(code.contains("Bool {"))
        // And no policy label, by name or by text.
        XCTAssertFalse(code.contains("policyLabel"))
        XCTAssertFalse(code.contains("policy_label"))
        XCTAssertFalse(code.contains("\"PROTECTED"))
        XCTAssertFalse(code.contains("\"AUTO_SAFE"))
        XCTAssertFalse(code.contains("\"ASK"))
        // The refusal text is never inspected, only carried.
        XCTAssertFalse(code.contains("hasPrefix"))
        XCTAssertFalse(code.contains("contains(\""))
    }

    private static func readSource(_ fileName: String) -> String {
        let thisFile = URL(fileURLWithPath: #filePath)
        let sourceFile = thisFile
            .deletingLastPathComponent()      // Tests/
            .deletingLastPathComponent()      // GlomerisMenuBar/
            .appendingPathComponent("Sources")
            .appendingPathComponent(fileName)
        guard let source = try? String(contentsOf: sourceFile, encoding: .utf8) else {
            XCTFail("could not read \(fileName)")
            return ""
        }
        return source
    }
}
