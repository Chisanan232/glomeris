//
//  RecoveryPreferencesTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1507. Covers the parts of the recovery settings pane that are not
//  SwiftUI: the exit-code contract, the pair the form composes, and the
//  sentences said about the result.
//
//  The claims worth pinning here are not "the form works". They are:
//
//    * a refused pair is never read as a usage error, and a usage error is never
//      read as a refused pair — both arrive on exit 2, and only the stream they
//      come out of separates them;
//    * the two percentages are never sent as each other. This is the failure the
//      whole pane is shaped to prevent, so the assertion is on which flag each
//      number lands behind, not on the vector's length;
//    * `set` always states both numbers, because `settings set` validates the
//      pair in one call and a one-field change can be refused for the field the
//      user did not touch;
//    * a draft can never compose a value the CLI's own reported bounds refuse,
//      including when the value it was *loaded* from is already outside them;
//    * an unreadable-but-successful save never reads as "nothing was saved" —
//      that one is backwards in the expensive direction, because the user would
//      believe their old values were still in force;
//    * spoken state distinguishes the threshold from the target.
//
//  Every draft and refusal test starts from the same `tests/fixtures/dto/`
//  fixtures the Rust side asserts `settings show|set --json` serializes to, so a
//  change to either report's shape reaches this file rather than being absorbed
//  by a hand-written literal.
//

import XCTest

final class RecoveryPreferencesTests: XCTestCase {

    // MARK: - Fixtures

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .deletingLastPathComponent()  // macos
            .deletingLastPathComponent()  // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func fixtureData(_ name: String) throws -> Data {
        try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
    }

    /// The configured pair: threshold 85% used, goal 60% used, bounds 1…99 and
    /// 0…100.
    private func report() throws -> RecoverySettingsReportDto {
        try JSONDecoder().decode(
            RecoverySettingsReportDto.self,
            from: try fixtureData("recovery_settings_report.json")
        )
    }

    private func defaultsReport() throws -> RecoverySettingsReportDto {
        try JSONDecoder().decode(
            RecoverySettingsReportDto.self,
            from: try fixtureData("recovery_settings_report_defaults.json")
        )
    }

    private func rejection() throws -> SettingsRejectionReportDto {
        try JSONDecoder().decode(
            SettingsRejectionReportDto.self,
            from: try fixtureData("settings_rejection_report.json")
        )
    }

    private func draft() throws -> RecoverySettingsDraft {
        RecoverySettingsDraft.from(try report())
    }

    // MARK: - Interpretation

    func testExitZeroWithAReportIsTheSettings() throws {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 0,
            stdout: try fixtureData("recovery_settings_report.json"),
            stderr: Data()
        )
        guard case .settings(let decoded) = outcome else {
            return XCTFail("expected .settings, got \(outcome)")
        }
        XCTAssertEqual(decoded.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(decoded.defaultGoal.usedPercent, 60.0)
    }

    /// The case that separates this pane from the Autopilot one: exit 2 carrying
    /// a structured refusal on stdout.
    ///
    /// `src/main.rs` prints the rejection report and *then* exits 2. Reading that
    /// as a prose failure would throw away the reason token, which is the only
    /// thing that can tell the user which of the two controls to move.
    func testExitTwoWithARejectionReportIsARefusal() throws {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 2,
            stdout: try fixtureData("settings_rejection_report.json"),
            stderr: Data()
        )
        guard case .refused(let decoded) = outcome else {
            return XCTFail("expected .refused, got \(outcome)")
        }
        XCTAssertEqual(decoded.reason, "goal_not_below_notify_threshold")
        XCTAssertEqual(decoded.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(decoded.goalUsedPercent, 90.0)
    }

    /// Same exit code, different stream, opposite meaning. A version skew is this
    /// app's problem; a refused pair is the user's to fix.
    func testExitTwoWithAUsageErrorIsNotARefusal() {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 2,
            stdout: Data("usage: glomeris settings …".utf8),
            stderr: Data("glomeris settings: unrecognized argument '--goal'".utf8)
        )
        guard case .usageError(let detail) = outcome else {
            return XCTFail("expected .usageError, got \(outcome)")
        }
        // The CLI's own prefix is stripped so the sentence reads as a sentence.
        XCTAssertEqual(detail, "unrecognized argument '--goal'")
    }

    /// A refused *argument* — a number out of range reported as prose rather than
    /// as a report, which is what `settings set` without `--json` does. Not a
    /// version skew, so not a usage error.
    func testExitTwoWithoutTheSkewMarkerIsAPlainFailure() {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: Data("glomeris settings: set needs --notify-at-used-percent".utf8)
        )
        XCTAssertEqual(outcome, .failed("set needs --notify-at-used-percent"))
    }

    func testExitOneIsAStoreFailureWithItsOwnSentence() {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 1,
            stdout: Data(),
            stderr: Data("glomeris settings: failed to write your settings: disk full".utf8)
        )
        XCTAssertEqual(outcome, .failed("failed to write your settings: disk full"))
    }

    func testExitZeroWithUnreadableStdoutIsMalformed() {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 0,
            stdout: Data("notify me at: 75% used".utf8),
            stderr: Data()
        )
        XCTAssertEqual(outcome, .malformedOutput)
    }

    /// A report on stdout beats the exit code, so a non-zero exit cannot throw
    /// away the only structured account of what is now stored.
    func testAReportOnStdoutWinsOverANonZeroExit() throws {
        let outcome = RecoverySettingsInterpretation.interpret(
            exitCode: 1,
            stdout: try fixtureData("recovery_settings_report.json"),
            stderr: Data("glomeris settings: something went wrong".utf8)
        )
        guard case .settings = outcome else {
            return XCTFail("expected .settings, got \(outcome)")
        }
    }

    /// The two reports must stay decode-disjoint. If they ever converge, the
    /// order in `interpret` is what decides, and this is the assertion that would
    /// fail first.
    func testASettingsReportIsNotAlsoDecodableAsARejection() throws {
        XCTAssertNil(
            try? JSONDecoder().decode(
                SettingsRejectionReportDto.self,
                from: try fixtureData("recovery_settings_report.json")
            ))
        XCTAssertNil(
            try? JSONDecoder().decode(
                RecoverySettingsReportDto.self,
                from: try fixtureData("settings_rejection_report.json")
            ))
    }

    /// Prose on stderr with an empty stream still produces a sentence. Silence is
    /// the one outcome a settings pane must not present.
    func testEveryFailurePathStillSaysSomething() {
        for code: Int32 in [1, 2, 7] {
            let outcome = RecoverySettingsInterpretation.interpret(
                exitCode: code, stdout: Data(), stderr: Data())
            guard case .failed(let detail) = outcome else {
                return XCTFail("exit \(code): expected .failed, got \(outcome)")
            }
            XCTAssertFalse(detail.isEmpty, "exit \(code) produced an empty sentence")
        }
    }

    // MARK: - Which write happened

    /// The judgement that is backwards in the expensive direction if it is
    /// wrong: exit 0 with unreadable stdout means the file WAS written.
    func testOnlyTheOutcomesThatReachedTheFileCountAsWritten() throws {
        XCTAssertFalse(RecoverySettingsOutcome.settings(try report()).wroteNothing)
        XCTAssertFalse(RecoverySettingsOutcome.malformedOutput.wroteNothing)

        XCTAssertTrue(RecoverySettingsOutcome.refused(try rejection()).wroteNothing)
        XCTAssertTrue(RecoverySettingsOutcome.usageError("skew").wroteNothing)
        XCTAssertTrue(RecoverySettingsOutcome.failed("disk full").wroteNothing)
    }

    // MARK: - The command

    /// Both flags, always. `settings set` validates the pair in one call
    /// precisely so a destination like (75, 70) → (60, 55) is reachable; sending
    /// one flag would have the user refused for a field they are in the middle of
    /// replacing.
    func testSetAlwaysStatesBothNumbers() {
        let arguments = RecoverySettingsCommands.set(
            notifyAtUsedPercent: 60, goalUsedPercent: 55)
        XCTAssertTrue(arguments.contains("--notify-at-used-percent"))
        XCTAssertTrue(arguments.contains("--default-goal-used-percent"))
    }

    /// The assertion this pane exists for. Each number lands behind its own flag,
    /// and neither is reachable from the other's.
    func testTheThresholdAndTheGoalNeverSwapFlags() {
        let arguments = RecoverySettingsCommands.set(
            notifyAtUsedPercent: 85, goalUsedPercent: 60)
        XCTAssertEqual(value(of: "--notify-at-used-percent", in: arguments), "85")
        XCTAssertEqual(value(of: "--default-goal-used-percent", in: arguments), "60")
    }

    /// The flag spellings are `src/main.rs`'s, and a `--json` is present so the
    /// result comes back as a report rather than as prose this app would have to
    /// parse.
    func testTheCommandIsTheOneTheCliParses() {
        let arguments = RecoverySettingsCommands.set(
            notifyAtUsedPercent: 85, goalUsedPercent: 60)
        XCTAssertEqual(arguments.prefix(2), ["settings", "set"])
        XCTAssertTrue(arguments.contains("--json"))
        XCTAssertEqual(RecoverySettingsCommands.show, ["settings", "show", "--json"])
    }

    /// Nothing this pane runs names a path or carries a credential. It configures
    /// two numbers; a filesystem argument here would be a scope it has no
    /// business having.
    func testNoCommandNamesAPathOrACredential() throws {
        let vectors = [
            RecoverySettingsCommands.show,
            try draft().commandArguments,
            RecoverySettingsCommands.set(notifyAtUsedPercent: 1, goalUsedPercent: 0),
        ]
        for arguments in vectors {
            for argument in arguments {
                XCTAssertFalse(argument.hasPrefix("/"), "\(arguments) names a path")
                XCTAssertFalse(argument.contains("--root"), "\(arguments) scopes a root")
                XCTAssertFalse(argument.contains("key"), "\(arguments) mentions a key")
                XCTAssertFalse(argument.contains("token"), "\(arguments) mentions a token")
            }
        }
    }

    /// No `%`, no exponent. `settings set` parses with `f64::parse` after
    /// trimming a trailing percent sign, and `"9e1"` would parse to 90 while
    /// reading as nothing at all.
    func testTheFiguresAreSentAsPlainDigits() {
        for percent in [0.0, 1.0, 62.5, 99.0, 100.0] {
            let argument = RecoverySettingsFigure.argument(percent)
            XCTAssertFalse(argument.contains("%"), argument)
            XCTAssertFalse(argument.lowercased().contains("e"), argument)
            XCTAssertEqual(Double(argument), percent, argument)
        }
    }

    /// A whole percentage is not shown as `70.0`, and a fractional one is not
    /// rounded — rounding it on screen would make the pane disagree with
    /// `settings show` about what is stored.
    func testWholePercentagesLoseTheDecimalAndFractionalOnesKeepIt() {
        XCTAssertEqual(RecoverySettingsFigure.text(70.0), "70")
        XCTAssertEqual(RecoverySettingsFigure.text(62.5), "62.5")
    }

    // MARK: - The draft

    func testTheDraftStartsFromWhatIsStored() throws {
        let draft = try draft()
        XCTAssertEqual(draft.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(draft.goalUsedPercent, 60.0)
    }

    /// Loaded verbatim, not snapped to the stepper's step. Showing 60 for a
    /// stored 62.5 would make merely opening the pane and pressing Save change a
    /// number the user never touched.
    func testAFractionalStoredValueIsNotSnapped() throws {
        var draft = try draft()
        draft.goalUsedPercent = 62.5
        XCTAssertEqual(draft.clampedGoalUsedPercent, 62.5)
        XCTAssertEqual(value(of: "--default-goal-used-percent", in: draft.commandArguments), "62.5")
    }

    /// The steppers are bounded by what the CLI reported, not by constants in the
    /// app. A pane with its own idea of the limits eventually offers a value the
    /// CLI refuses, and the refusal arrives after Save.
    func testTheStepperRangesAreTheReportedBounds() throws {
        let draft = try draft()
        XCTAssertEqual(draft.notifyAtRange, 1.0...99.0)
        XCTAssertEqual(draft.goalRange, 0.0...100.0)
    }

    /// A value already outside the bounds — an older CLI's settings file, or a
    /// hand-edited one — is clamped rather than forwarded. Sending it back
    /// unchanged would ask this CLI to accept what it refuses.
    func testAValueLoadedFromOutsideTheBoundsIsClampedBeforeItIsSent() throws {
        var draft = try draft()
        draft.notifyAtUsedPercent = 150
        draft.goalUsedPercent = -20
        XCTAssertEqual(draft.clampedNotifyAtUsedPercent, 99.0)
        XCTAssertEqual(draft.clampedGoalUsedPercent, 0.0)
        XCTAssertEqual(value(of: "--notify-at-used-percent", in: draft.commandArguments), "99")
        XCTAssertEqual(value(of: "--default-goal-used-percent", in: draft.commandArguments), "0")
    }

    /// The one value that would reach `argv` as the literal text "nan". It cannot
    /// arrive from a report — JSON has no NaN — but a stepper binding is a
    /// `Double`, and `min`/`max` pass NaN through because it compares false
    /// against everything.
    func testANonFiniteValueCannotReachTheCommandLine() throws {
        for bad in [Double.nan, .infinity, -.infinity] {
            var draft = try draft()
            draft.notifyAtUsedPercent = bad
            draft.goalUsedPercent = bad
            XCTAssertTrue(draft.clampedNotifyAtUsedPercent.isFinite, "\(bad)")
            XCTAssertTrue(draft.clampedGoalUsedPercent.isFinite, "\(bad)")
            for argument in draft.commandArguments {
                XCTAssertFalse(argument.lowercased().contains("nan"), argument)
                XCTAssertFalse(argument.lowercased().contains("inf"), argument)
            }
        }
    }

    /// A reversed range would trap `Stepper(value:in:)` rather than render it, so
    /// a CLI reporting a minimum above its maximum degrades to offering no
    /// choice instead of crashing the pane.
    func testAReversedReportedBoundDoesNotProduceAnInvalidRange() throws {
        let decoded = try JSONDecoder().decode(
            RecoverySettingsBoundsReportDto.self,
            from: Data(
                """
                {
                  "notify_at_minimum_used_percent": 99.0,
                  "notify_at_maximum_used_percent": 1.0,
                  "goal_minimum_used_percent": 100.0,
                  "goal_maximum_used_percent": 0.0
                }
                """.utf8)
        )
        let draft = RecoverySettingsDraft(
            notifyAtUsedPercent: 50, goalUsedPercent: 40, bounds: decoded)
        XCTAssertLessThanOrEqual(draft.notifyAtRange.lowerBound, draft.notifyAtRange.upperBound)
        XCTAssertLessThanOrEqual(draft.goalRange.lowerBound, draft.goalRange.upperBound)
    }

    /// Compared against the report, not tracked as a dirty flag, so moving a
    /// stepper away and back again correctly reads as no change.
    func testSavingIsOfferedOnlyWhenSomethingActuallyDiffers() throws {
        let report = try report()
        var draft = RecoverySettingsDraft.from(report)
        XCTAssertFalse(draft.differs(from: report))

        draft.goalUsedPercent = 55
        XCTAssertTrue(draft.differs(from: report))

        draft.goalUsedPercent = report.defaultGoal.usedPercent
        XCTAssertFalse(draft.differs(from: report))
    }

    /// The pane does not pre-judge the pair. The cross-field rule is not a bound
    /// on either number, so a goal at or above the threshold is composable here
    /// and refused by the validator — which is the only arrangement in which the
    /// pane and the validator cannot disagree about the rule.
    func testAGoalAboveTheThresholdIsComposableAndLeftToTheCliToRefuse() throws {
        var draft = try draft()
        draft.goalUsedPercent = 90  // threshold is 85
        XCTAssertEqual(draft.clampedGoalUsedPercent, 90.0)
        XCTAssertEqual(value(of: "--default-goal-used-percent", in: draft.commandArguments), "90")
    }

    // MARK: - What is said about the result

    func testASuccessfulSaveSaysTheValuesAreInForce() throws {
        let message = RecoverySettingsSaveWording.message(.settings(try report()))
        XCTAssertEqual(message?.kind, .success)
    }

    /// A refusal is rendered as a card with its reason and the CLI's own
    /// sentence, so there is deliberately no line for it here. Two wordings for
    /// one refusal is how a pane tells a user two different things about one
    /// event.
    func testARefusalHasNoLineBecauseItHasACard() throws {
        XCTAssertNil(RecoverySettingsSaveWording.message(.refused(try rejection())))
    }

    /// The expensive-direction assertion. Exit 0 means `save_settings` returned
    /// `Ok`, so this one must never read as "nothing was saved".
    func testAnUnreadableButSuccessfulSaveDoesNotClaimNothingWasSaved() {
        guard let message = RecoverySettingsSaveWording.message(.malformedOutput) else {
            return XCTFail("a malformed save must still say something")
        }
        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("saved these values"), message.title)
        XCTAssertFalse(message.title.contains("Nothing was saved"), message.title)
    }

    /// Both of the paths where nothing reached the file say so, in those words.
    func testTheFailurePathsSayNothingWasSaved() {
        for outcome in [
            RecoverySettingsOutcome.usageError("unrecognized argument '--goal'"),
            RecoverySettingsOutcome.failed("failed to write your settings: disk full"),
        ] {
            guard let message = RecoverySettingsSaveWording.message(outcome) else {
                return XCTFail("\(outcome) must say something")
            }
            XCTAssertEqual(message.kind, .failure)
            XCTAssertTrue(
                message.title.lowercased().contains("nothing was saved"), message.title)
        }
    }

    // MARK: - Wording

    /// Neither label may show a percentage without its axis in the same string,
    /// and neither may be confusable with the other.
    func testEveryLabelNamesItsAxisAndItsPurpose() {
        let threshold = RecoveryPreferencesWording.thresholdLabel(85)
        let goal = RecoveryPreferencesWording.goalLabel(60)

        XCTAssertTrue(threshold.contains("85% used"), threshold)
        XCTAssertTrue(goal.contains("60% used"), goal)
        XCTAssertNotEqual(threshold, goal)
        // The two verbs are what a reader distinguishes them by when both
        // percentages happen to be similar.
        XCTAssertTrue(threshold.contains("Say something"), threshold)
        XCTAssertTrue(goal.contains("Recover down to"), goal)
    }

    /// The threshold is a prompt, never a cleanup. Said in the pane's own words
    /// because the campaign's default is that crossing it must not start
    /// anything destructive, and a user cannot infer a default from silence.
    func testTheThresholdIsDescribedAsAPromptNotACleanup() {
        let text = RecoveryPreferencesWording.thresholdExplanation
        XCTAssertTrue(text.contains("never a cleanup"), text)
        XCTAssertTrue(text.contains("nothing is deleted"), text.lowercased())
    }

    /// The pane states that the goal has to be below the threshold — as an
    /// explanation, not as a control limit. The two are different: one teaches
    /// the rule, the other would reimplement it.
    func testThePaneExplainsTheRuleItDoesNotEnforce() {
        XCTAssertTrue(
            RecoveryPreferencesWording.twoDifferentNumbers.contains("below the threshold"),
            RecoveryPreferencesWording.twoDifferentNumbers
        )
    }

    /// The bounds note quotes the reported limits rather than restating numbers
    /// this file knows.
    func testTheBoundsNoteQuotesTheReportedLimits() throws {
        let note = RecoveryPreferencesWording.boundsNote(try draft())
        XCTAssertTrue(note.contains("1%"), note)
        XCTAssertTrue(note.contains("99%"), note)
        XCTAssertTrue(note.contains("100%"), note)
    }

    /// Built-in defaults are said to be built-in. "75% used" looks identical
    /// whether the user chose it or the product did, and only one of those is a
    /// decision anybody made.
    func testUnstoredDefaultsAreNotPresentedAsAChoice() throws {
        XCTAssertFalse(try defaultsReport().loadedFromFile)
        XCTAssertTrue(
            RecoveryPreferencesWording.storedDefaults.contains("built-in defaults"),
            RecoveryPreferencesWording.storedDefaults
        )
        XCTAssertNotEqual(
            RecoveryPreferencesWording.storedDefaults, RecoveryPreferencesWording.storedChosen)
    }

    // MARK: - Accessibility

    /// Spoken state distinguishes the two numbers. A value read as a bare "85"
    /// leaves the axis to be inferred from a label heard some moments earlier,
    /// and there are two axes here a listener must not merge.
    func testSpokenValuesNameTheirAxisAndSpellOutThePercent() {
        XCTAssertEqual(RecoverySettingsFigure.spokenValue(85), "85 percent used")
        XCTAssertEqual(RecoverySettingsFigure.spokenValue(62.5), "62.5 percent used")
        XCTAssertFalse(RecoverySettingsFigure.spokenValue(85).contains("%"))
    }

    /// The refusal is three `Text` views in one accessibility element, so the
    /// sentence boundaries layout gives a sighted user have to be stated. Checked
    /// against `SpokenLabel` itself rather than against a copy of its rule.
    func testTheSpokenRefusalStatesItsAxisAndBothSentences() throws {
        let rejection = try rejection()
        let spoken = RecoverySettingsRefusalLabel.spoken(rejection)
        let term = GlomerisVocabulary.settingsRejection(rejection.reason)

        XCTAssertEqual(
            spoken,
            SpokenLabel.compose([
                SpokenLabel.clause(term.axis, term.title),
                term.explanation,
                rejection.message,
            ])
        )
        // The axis, so a listener knows which kind of refusal arrived.
        XCTAssertTrue(spoken.hasPrefix(term.axis), spoken)
        // The CLI's own account, verbatim — including both figures, which is what
        // makes this refusal actionable rather than merely reported.
        XCTAssertTrue(spoken.contains(rejection.message), spoken)
        XCTAssertTrue(spoken.contains("90% used"), spoken)
        XCTAssertTrue(spoken.contains("85% used"), spoken)
    }

    /// Every refusal the CLI can report has wording that names which number was
    /// refused. The set of tokens is the vocabulary guard's business; what is
    /// asserted here is that none of them arrives as the unrecognised fallback,
    /// and that each explanation says nothing was saved.
    func testEveryKnownRefusalTokenHasWordingThatSaysNothingWasSaved() {
        let tokens = [
            "notify_threshold_not_finite",
            "notify_threshold_out_of_range",
            "goal_not_finite",
            "goal_out_of_range",
            "goal_not_below_notify_threshold",
            "goal_refused",
        ]
        for token in tokens {
            let term = GlomerisVocabulary.settingsRejection(token)
            XCTAssertEqual(term.token, token)
            XCTAssertTrue(
                term.explanation.contains("Nothing was saved"),
                "\(token): \(term.explanation)"
            )
            XCTAssertEqual(term.axis, GlomerisVocabulary.settingsRejectionAxis, token)
        }
    }

    // MARK: - Helpers

    private func value(of flag: String, in arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else {
            return nil
        }
        return arguments[index + 1]
    }
}
