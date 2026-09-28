//
//  RecoverySectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1506. The recovery card's wording and its exit-code mapping, asserted
//  against the same golden fixtures `tests/dto_golden_fixtures.rs` builds from
//  the CLI's own report builders. What a user is told about a run that has
//  already deleted things is the thing worth pinning, and every sentence in this
//  file's subject is derived from one report.
//
//  The two properties the whole file exists for:
//
//    1. A stop is worded as success only when the CLI measured the goal as met.
//       `RecoveryRunSummary` gets `targetMet` from the report, which Rust sets
//       from the re-read free space rather than the sum of what was deleted — so
//       the test for a run that stopped short asserts the *absence* of a success
//       message as well as the presence of the right badge.
//    2. The three reclaimable figures stay three figures. A card that summed
//       confirmation-gated space into actionable space would show a user space
//       they have not authorised as space they are about to get back.
//
//  And one that is about a defect rather than wording: a busy execution lock and
//  a refused goal have the same required JSON fields, so decode order cannot
//  tell them apart. `testBusyExecutionLockIsNotReportedAsARefusedGoal` is the
//  regression pin, and it fails if the exit-code guard is removed.
//

import XCTest

final class RecoverySectionViewTests: XCTestCase {
    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func fixtureData(_ name: String) throws -> Data {
        try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
    }

    private func fixture<T: Decodable>(_ name: String, as type: T.Type) throws -> T {
        try JSONDecoder().decode(T.self, from: try fixtureData(name))
    }

    // MARK: - The goal control

    /// The axis is in the label, which is the ticket's central requirement: a
    /// bare "Target: 50%" says nothing about whether it means used or free.
    func testGoalLabelNamesTheUsedAxis() {
        XCTAssertEqual(RecoverySectionView.goalLabel(usedPercent: 70), "Get down to 70% used")
        XCTAssertTrue(RecoverySectionView.goalLabel(usedPercent: 5).contains("used"))
        XCTAssertFalse(
            RecoverySectionView.goalLabel(usedPercent: 70).contains("free"),
            "one axis per label — naming both invites the reader to pick"
        )
    }

    /// The spoken value carries the axis too, and spells the unit out.
    ///
    /// A stepper read as a bare "70" is the unlabeled percentage control the
    /// campaign's accessibility rule names, even when a sighted user can see the
    /// word beside it — VoiceOver announces the value on every increment and the
    /// label far less often.
    func testTheSpokenGoalValueNamesTheUsedAxisInWords() {
        XCTAssertEqual(
            RecoverySectionView.goalAccessibilityValue(usedPercent: 70),
            "70 percent used"
        )
        XCTAssertFalse(
            RecoverySectionView.goalAccessibilityValue(usedPercent: 70).contains("%"),
            "a spoken % is at the mercy of the voice; the word is not"
        )
        XCTAssertFalse(
            RecoverySectionView.goalAccessibilityValue(usedPercent: 70).contains("free"),
            "one axis here as well, for the same reason the label has one"
        )
    }

    /// The control's range excludes both ends, and each exclusion has a reason:
    /// 0% used asks Glomeris to empty the disk, and 100% used can never be an
    /// improvement on anything, so a goal there could only ever be refused.
    func testGoalRangeExcludesTheTwoUselessEnds() {
        XCTAssertGreaterThan(RecoverySectionView.goalRange.lowerBound, 0)
        XCTAssertLessThan(RecoverySectionView.goalRange.upperBound, 100)
        XCTAssertEqual(
            RecoverySectionView.goalRange.lowerBound % RecoverySectionView.goalStep, 0,
            "the bounds must be reachable by stepping, or the control cannot reach its own end"
        )
        XCTAssertEqual(RecoverySectionView.goalRange.upperBound % RecoverySectionView.goalStep, 0)
    }

    /// The starting point is in the range the control can express, and below the
    /// point the CLI's own default thresholds start warning — see the property's
    /// documentation in OverviewState.swift.
    func testDefaultGoalIsReachableByTheControl() {
        let state = RecoveryState()
        XCTAssertTrue(RecoverySectionView.goalRange.contains(state.goalUsedPercent))
        XCTAssertEqual(state.goalUsedPercent % RecoverySectionView.goalStep, 0)
    }

    // MARK: - Seeding from the stored default (HORO-1507)

    /// A whole stored percentage arrives unchanged. This is the ordinary case and
    /// the one that would be quietly wrong if the rounding below were applied
    /// unconditionally.
    func testAWholeStoredDefaultIsTakenAsItIs() {
        XCTAssertEqual(RecoverySectionView.storedDefaultGoal(usedPercent: 60), 60)
        XCTAssertEqual(RecoverySectionView.storedDefaultGoal(usedPercent: 70), 70)
    }

    /// Not snapped to `goalStep`. A stored 62 presents as 62, because showing 60
    /// for it would have the card describe a run aiming somewhere the user did
    /// not ask for — and the control can still step from there.
    func testAStoredDefaultOffTheStepGridIsNotSnappedToIt() {
        XCTAssertEqual(RecoverySectionView.storedDefaultGoal(usedPercent: 62), 62)
        XCTAssertNotEqual(
            RecoverySectionView.storedDefaultGoal(usedPercent: 62)! % RecoverySectionView.goalStep,
            0,
            "a value off the grid proves the snap is absent; if this ever passes trivially the "
                + "step changed and this test is no longer testing anything"
        )
    }

    /// Rounded up, toward the disk staying fuller. Something has to give on a
    /// whole-percentage control, and rounding down would aim at a slightly
    /// emptier disk than the stored number — which means deleting marginally more
    /// than was asked for.
    func testAFractionalStoredDefaultRoundsTowardLessDeletion() {
        XCTAssertEqual(RecoverySectionView.storedDefaultGoal(usedPercent: 62.5), 63)
        XCTAssertEqual(RecoverySectionView.storedDefaultGoal(usedPercent: 62.1), 63)
        XCTAssertEqual(RecoverySectionView.storedDefaultGoal(usedPercent: 62.9), 63)
    }

    /// Clamped into what the control can express. `settings` will store a goal of
    /// 2% used quite happily — its range is 0…100 — so this is the card admitting
    /// its own limit rather than correcting the setting.
    func testAStoredDefaultOutsideTheControlsRangeIsClampedToIt() {
        XCTAssertEqual(
            RecoverySectionView.storedDefaultGoal(usedPercent: 2),
            RecoverySectionView.goalRange.lowerBound
        )
        XCTAssertEqual(
            RecoverySectionView.storedDefaultGoal(usedPercent: 99),
            RecoverySectionView.goalRange.upperBound
        )
        XCTAssertEqual(
            RecoverySectionView.storedDefaultGoal(usedPercent: 0),
            RecoverySectionView.goalRange.lowerBound
        )
        XCTAssertEqual(
            RecoverySectionView.storedDefaultGoal(usedPercent: 100),
            RecoverySectionView.goalRange.upperBound
        )
    }

    /// `Int(exactly:)` traps on a non-finite `Double`, and there is no whole
    /// percentage that means "not a number". It cannot arrive from JSON, which is
    /// why the answer is `nil` rather than a number the card would then show.
    func testANonFiniteStoredDefaultHasNoAnswer() {
        XCTAssertNil(RecoverySectionView.storedDefaultGoal(usedPercent: .nan))
        XCTAssertNil(RecoverySectionView.storedDefaultGoal(usedPercent: .infinity))
        XCTAssertNil(RecoverySectionView.storedDefaultGoal(usedPercent: -.infinity))
    }

    /// Every representable answer is one the control can reach, so the seed can
    /// never put a value into a `Stepper(value:in:)` that its own range excludes.
    func testEverySeededGoalIsInsideTheControlsRange() {
        for tenths in stride(from: -50, through: 1500, by: 7) {
            let percent = Double(tenths) / 10
            guard let seeded = RecoverySectionView.storedDefaultGoal(usedPercent: percent) else {
                return XCTFail("\(percent) produced no answer")
            }
            XCTAssertTrue(
                RecoverySectionView.goalRange.contains(seeded),
                "\(percent) seeded \(seeded), outside \(RecoverySectionView.goalRange)"
            )
        }
    }

    /// The seed is a default, so it applies to a card the user has not touched.
    func testTheStoredDefaultSeedsAnUntouchedGoal() {
        let state = RecoveryState()
        XCTAssertTrue(state.adoptStoredDefaultGoal(55))
        XCTAssertEqual(state.goalUsedPercent, 55)
    }

    /// And never overrides a goal the user set. Without this the seed would
    /// re-apply on every popover appearance, and a goal chosen two minutes ago
    /// would revert between one look at the panel and the next.
    func testTheStoredDefaultNeverOverridesAGoalTheUserChose() {
        let state = RecoveryState()
        state.goalUsedPercent = 40
        state.noteUserChoseGoal()

        XCTAssertFalse(state.adoptStoredDefaultGoal(55))
        XCTAssertEqual(state.goalUsedPercent, 40)
    }

    /// Adoption does not trip the flag. A store that treated its own seed as a
    /// user choice would decline the next one for no reason — including the one
    /// that follows the user changing the setting and reopening the panel.
    func testAdoptingTheStoredDefaultIsNotAUserChoice() {
        let state = RecoveryState()
        XCTAssertTrue(state.adoptStoredDefaultGoal(55))
        XCTAssertFalse(state.hasUserChosenGoal)
        XCTAssertTrue(state.adoptStoredDefaultGoal(50), "a later report must still be adoptable")
        XCTAssertEqual(state.goalUsedPercent, 50)
    }

    /// Re-seeding the same number reports no change, so a caller does not clear a
    /// pre-flight that is still measured against the goal it was taken under.
    func testReSeedingTheSameGoalReportsNoChange() {
        let state = RecoveryState()
        XCTAssertTrue(state.adoptStoredDefaultGoal(55))
        XCTAssertFalse(state.adoptStoredDefaultGoal(55))
        XCTAssertEqual(state.goalUsedPercent, 55)
    }

    // MARK: - Arguments

    /// One axis on the wire. `--goal-used-percent` is what the CLI validates
    /// against live usage; `--target` is the raw free-space floor, and this app
    /// has no call site for it.
    func testArgumentsSendTheGoalOnTheUsedAxisOnly() {
        let store = ProjectRootsStore(defaults: TestUserDefaults.inMemory())

        let preview = RecoverySectionView.previewArguments(
            goalUsedPercent: 60,
            projectRootsStore: store
        )
        XCTAssertEqual(preview.first, "free")
        XCTAssertTrue(preview.contains("--goal-used-percent"))
        XCTAssertTrue(preview.contains("60"))
        XCTAssertFalse(preview.contains("--target"))
        XCTAssertTrue(preview.contains("--dry-run"))
        XCTAssertTrue(preview.contains("--json"))

        let run = RecoverySectionView.runArguments(goalUsedPercent: 60, projectRootsStore: store)
        XCTAssertEqual(run.first, "free")
        XCTAssertTrue(run.contains("--goal-used-percent"))
        XCTAssertFalse(run.contains("--target"))
        XCTAssertFalse(
            run.contains("--dry-run"),
            "the run is the one that actually reclaims — a --dry-run here would reclaim nothing "
                + "while the card reported a finished run"
        )
    }

    /// `--progress-json` is asked for only where the CLI will accept it. A real
    /// run emits no progress events, and `free` exits 2 on the flag outside
    /// `--dry-run` rather than accepting it and streaming nothing — so asking
    /// would fail every run.
    func testProgressEventsAreRequestedOnlyForThePreflight() {
        let store = ProjectRootsStore(defaults: TestUserDefaults.inMemory())

        XCTAssertTrue(
            RecoverySectionView.previewArguments(goalUsedPercent: 60, projectRootsStore: store)
                .contains("--progress-json")
        )
        XCTAssertFalse(
            RecoverySectionView.runArguments(goalUsedPercent: 60, projectRootsStore: store)
                .contains("--progress-json")
        )
    }

    /// HORO-1501's inverse: `free` discovers before it reclaims, so an
    /// invocation that lost the configured roots would quietly search the wrong
    /// places and still report success. Asserted through the store rather than
    /// by grepping for the flag, so the builders are proven to route through it.
    func testBothInvocationsCarryTheConfiguredProjectRoots() {
        let defaults = TestUserDefaults.inMemory()
        let store = ProjectRootsStore(defaults: defaults)
        store.addRoot("/Users/dev/proj")

        for arguments in [
            RecoverySectionView.previewArguments(goalUsedPercent: 60, projectRootsStore: store),
            RecoverySectionView.runArguments(goalUsedPercent: 60, projectRootsStore: store),
        ] {
            XCTAssertTrue(arguments.contains("--project-root"), "\(arguments)")
            XCTAssertTrue(arguments.contains("/Users/dev/proj"), "\(arguments)")
        }
    }

    // MARK: - The pre-flight summary

    func testPreviewSummaryNamesBothAxesFromTheReportsOwnSentence() throws {
        let dto = try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        let summary = RecoveryPreviewSummary(dto)

        // Verbatim. Reassembling it from the two percentages is how a GUI comes
        // to disagree with the terminal about the same goal.
        XCTAssertEqual(summary.goalText, "60% used (40% free)")
        // One decimal, matching `print_recovery_preview_report`'s
        // "{:.1}% used" — so the card and `glomeris free --dry-run` cannot
        // report the same reading differently.
        XCTAssertEqual(summary.currentText, "88.0% used — 55.9 GB free of 465.7 GB")
        XCTAssertEqual(summary.pressureTerm.token, "PRESSURED")
        XCTAssertEqual(summary.stillNeededText, "130.4 GB more free space needed")
        XCTAssertFalse(summary.goalIsAlreadyMet)
    }

    /// The three-figure rule. Each bucket is rendered on its own, and the test
    /// asserts the absence of the total as well as the presence of the parts:
    /// 2.0 GB + 5.0 GB is 7.0 GB, and a card showing that would be telling the
    /// user a confirmation-gated 5 GB is already theirs.
    func testPreviewSummaryKeepsTheThreeOpportunityFiguresApart() throws {
        let dto = try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        let summary = RecoveryPreviewSummary(dto)

        XCTAssertEqual(summary.actionableNowText, "≥ 2.0 GB across 1 item")
        XCTAssertEqual(summary.requiresConfirmationText, "≥ 5.0 GB across 1 item")
        XCTAssertEqual(summary.notExecutableText, "2 items no run can act on, 1 of them protected")

        let everything = [
            summary.actionableNowText,
            summary.requiresConfirmationText ?? "",
            summary.notExecutableText ?? "",
            summary.reachabilityText,
        ].joined(separator: " ")
        XCTAssertFalse(
            everything.contains("7.0 GB"),
            "the actionable and confirmation-gated figures must not be summed: \(everything)"
        )
    }

    /// `isLowerBound` is one flag for the whole opportunity, so the `≥` marker
    /// appears on both byte figures. A figure that is really a floor shown
    /// without it would state a measurement Glomeris does not have.
    func testLowerBoundEstimatesAreMarkedOnEveryByteFigure() throws {
        let dto = try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        XCTAssertTrue(dto.opportunity.isLowerBound, "the fixture must be the lower-bound case")

        let summary = RecoveryPreviewSummary(dto)
        XCTAssertTrue(summary.actionableNowText.hasPrefix("≥ "))
        XCTAssertTrue(summary.requiresConfirmationText?.hasPrefix("≥ ") == true)
    }

    /// Rust's caveats are shown as they are. They are the report's defence
    /// against being misread — estimates are not measurements, an incomplete
    /// search is not a complete one — and a client that edited them would be
    /// deciding which of its own numbers were misleading.
    func testPreviewSummaryCarriesTheCaveatsVerbatimAndInOrder() throws {
        let dto = try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        XCTAssertFalse(dto.caveats.isEmpty, "the fixture must carry caveats to pass through")
        XCTAssertEqual(RecoveryPreviewSummary(dto).caveats, dto.caveats)
    }

    /// Reachability is a hint in both directions. This fixture's goal needs more
    /// than the whole opportunity, and the sentence still has to leave room for
    /// a lower-bound estimate being wrong the useful way.
    func testUnreachableGoalIsWordedAsAnEstimateRatherThanAVerdict() throws {
        let dto = try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        XCTAssertFalse(dto.goalAppearsReachable, "the fixture must be the unreachable case")

        let text = RecoveryPreviewSummary(dto).reachabilityText
        XCTAssertTrue(text.contains("estimate"), text)
        XCTAssertTrue(
            text.contains("safely can"),
            "an unreachable estimate must not read as 'do not bother': \(text)"
        )
    }

    /// The already-met goal. `stillNeededText` goes to `nil` rather than
    /// reporting "0 B more needed", because zero-more-needed is a figure and
    /// being already there is a situation — and it is the situation the card
    /// disables its Recover button for.
    func testAnAlreadyMetGoalReportsNoShortfallAndIsFlagged() throws {
        let dto = try fixture(
            "recovery_preview_report_goal_already_met.json",
            as: RecoveryPreviewReportDto.self
        )
        let summary = RecoveryPreviewSummary(dto)

        XCTAssertTrue(summary.goalIsAlreadyMet)
        XCTAssertNil(summary.stillNeededText)
        XCTAssertTrue(
            summary.caveats.contains { $0.contains("refused") },
            "the reason the button is disabled has to be on screen: \(summary.caveats)"
        )
    }

    // MARK: - The run summary

    func testReachedGoalIsTheOnlyOutcomeWordedAsSuccess() throws {
        let dto = try fixture("recovery_run_report.json", as: RecoveryRunReportDto.self)
        XCTAssertTrue(dto.targetMet, "the fixture must be the goal-reached case")

        let summary = RecoveryRunSummary(dto)
        XCTAssertEqual(summary.stopReasonTerm.token, "target_reached")
        XCTAssertEqual(summary.outcomeMessage?.kind, .success)
        // Measured, and the figure quoted is the measured one.
        XCTAssertEqual(summary.reclaimedText, "139.7 GB reclaimed")
        XCTAssertEqual(summary.freeSpaceText, "55.9 GB free → 195.6 GB free")
        XCTAssertEqual(summary.effortText, "3 rounds · 4 actions run · 2 left alone")
        XCTAssertEqual(summary.goalText, "60% used (40% free)")
    }

    /// The campaign's "do NOT collapse every termination into 'Done'", as an
    /// assertion. A run that stopped for want of safe candidates gets its own
    /// badge and the CLI's own explanation, and gets no success message — which
    /// is asserted as an absence, because a success sentence is precisely what a
    /// well-meaning simplification would add here.
    func testRunThatStoppedShortIsNotWordedAsDone() throws {
        let dto = try fixture("recovery_run_report_raw_target.json", as: RecoveryRunReportDto.self)
        XCTAssertFalse(dto.targetMet, "the fixture must be the stopped-short case")
        XCTAssertNil(dto.error, "and it must not be the error case either")

        let summary = RecoveryRunSummary(dto)
        XCTAssertEqual(summary.stopReasonTerm.token, "safe_exhausted")
        XCTAssertNil(
            summary.outcomeMessage,
            "a run that did not reach the goal has no success or failure message to show"
        )
        // The badge carries the outcome, and it does so in words rather than by
        // colour alone — which is also the accessibility requirement.
        XCTAssertEqual(summary.stopReasonTerm.title, "Nothing safe left to take")
        XCTAssertTrue(
            summary.stopReasonDetail.contains("no safe candidate remained"),
            summary.stopReasonDetail
        )
        // What it did manage is still reported, measured.
        XCTAssertEqual(summary.reclaimedText, "2.0 GB reclaimed")
        XCTAssertEqual(summary.effortText, "2 rounds · 1 action run · 3 left alone")
    }

    /// The stop-reason badge's explanation has to name what is left rather than
    /// implying it is gone, which is `safe_exhausted`'s whole ambiguity: the
    /// remaining candidates still exist, they are simply not this run's to take.
    func testSafeExhaustedExplanationAccountsForWhatRemains() throws {
        let dto = try fixture("recovery_run_report_raw_target.json", as: RecoveryRunReportDto.self)
        let explanation = RecoveryRunSummary(dto).stopReasonTerm.explanation

        XCTAssertTrue(explanation.contains("confirmation"), explanation)
        XCTAssertTrue(explanation.contains("protected"), explanation)
        XCTAssertTrue(explanation.contains("has not gone anywhere"), explanation)
    }

    /// A raw `--target` run has no used-axis goal at all, and its `target`
    /// string names the free-space axis itself. Neither is rewritten, which is
    /// what keeps a free-space figure from being read as a usage one.
    func testARawTargetRunShowsTheFreeSpaceAxisItNamed() throws {
        let dto = try fixture("recovery_run_report_raw_target.json", as: RecoveryRunReportDto.self)
        XCTAssertNil(dto.goal, "the fixture must be the raw-target case")

        let summary = RecoveryRunSummary(dto)
        XCTAssertEqual(summary.goalText, "232.8 GB free")
        XCTAssertTrue(
            summary.goalText.contains("free"),
            "a free-space floor must say so, or it reads as a usage figure"
        )
    }

    // MARK: - Exit codes

    func testExitZeroWithARunReportFinishesTheRun() throws {
        let phase = RecoverySectionView.phase(
            forRunExitCode: 0,
            stdout: try fixtureData("recovery_run_report.json")
        )
        guard case .finished(let report) = phase else {
            return XCTFail("expected a finished run, got \(String(describing: phase))")
        }
        XCTAssertTrue(report.targetMet)
    }

    func testExitTwoWithARejectionReportsARefusedGoal() throws {
        let phase = RecoverySectionView.phase(
            forRunExitCode: 2,
            stdout: try fixtureData("recovery_goal_rejection_report.json")
        )
        guard case .refused(let rejection) = phase else {
            return XCTFail("expected a refused goal, got \(String(describing: phase))")
        }
        XCTAssertEqual(rejection.reason, "not_an_improvement")
        // The CLI's own sentence, shown as it is: this app does not re-explain a
        // judgment it did not make.
        XCTAssertTrue(rejection.message.contains("choose a goal below current usage"))
    }

    /// The regression pin.
    ///
    /// `RecoveryGoalRejectionReportDto`'s percentages are optional, so its
    /// required fields are `reason` and `message` — which is the whole of
    /// `ExecuteRefusalReportDto`. The execution lock's `busy` refusal therefore
    /// decodes cleanly as a goal rejection, and a card that tried the two DTOs in
    /// order would tell a user their goal had been declined when another run
    /// simply held the lock. Deleting the `exitCode == 2` guard in `phase` makes
    /// this test fail.
    func testBusyExecutionLockIsNotReportedAsARefusedGoal() throws {
        let busy = Data((#"{"reason":"busy","message":"another glomeris execution is "#
            + #"already in progress (execution lock busy) — try again shortly"}"#).utf8)

        // First: the shapes really do overlap, so the guard is load-bearing
        // rather than defensive. Without this assertion the test below could
        // pass for the wrong reason — a decode that failed anyway.
        XCTAssertNoThrow(
            try JSONDecoder().decode(RecoveryGoalRejectionReportDto.self, from: busy),
            "if this throws, the two reports no longer overlap and this pin is testing nothing"
        )

        XCTAssertNil(
            RecoverySectionView.phase(forRunExitCode: 75, stdout: busy),
            "a held execution lock is not a refused goal"
        )
    }

    /// And the user is told what the lock said, not what its exit code was.
    func testBusyExecutionLockIsReportedInTheCliOwnWords() {
        let busy = Data(#"{"reason":"busy","message":"another glomeris execution is already in progress"}"#.utf8)

        let message = RecoverySectionView.unreadableOutcomeMessage(
            subject: "free",
            exitCode: 75,
            stdout: busy,
            stderr: Data()
        )
        XCTAssertEqual(message, "free: another glomeris execution is already in progress")
        XCTAssertFalse(message.contains("75"), message)
    }

    /// A held lock is the one unreadable outcome where the pre-flight may stay on
    /// screen, because it is the one where nothing ran.
    func testAHeldLockKeepsThePreflightAndAnythingElseDiscardsIt() throws {
        let preview = try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        let busy = Data(#"{"reason":"busy","message":"another glomeris execution is already in progress"}"#.utf8)

        guard case .reviewing = RecoverySectionView.unreadableOutcomePhase(
            exitCode: 75,
            stdout: busy,
            startedFrom: preview
        ) else {
            return XCTFail("a refused lock ran nothing, so the pre-flight is still current")
        }

        // Anything else may have deleted an unknown amount, which makes every
        // figure in the pre-flight a claim about a volume that no longer exists.
        XCTAssertEqual(
            RecoverySectionView.unreadableOutcomePhase(
                exitCode: 1,
                stdout: Data(),
                startedFrom: preview
            ),
            .idle,
            "a run that may have deleted things must not leave stale figures under a Recover button"
        )
    }

    /// A usage error puts prose on stderr and nothing on stdout. The message has
    /// to carry that prose, because it is the only thing that says what was
    /// wrong.
    func testAStderrOnlyFailureReportsWhatTheCliSaid() {
        let message = RecoverySectionView.unreadableOutcomeMessage(
            subject: "free",
            exitCode: 2,
            stdout: Data(),
            stderr: Data("glomeris free: unrecognized argument '--nope'\n".utf8)
        )
        XCTAssertEqual(message, "free: glomeris free: unrecognized argument '--nope'")
    }

    /// Both streams empty is the honest floor: a number, and no cause invented
    /// for it.
    func testASilentFailureNamesOnlyTheExitCode() {
        XCTAssertEqual(
            RecoverySectionView.unreadableOutcomeMessage(
                subject: "free",
                exitCode: 1,
                stdout: Data(),
                stderr: Data()
            ),
            "free: did not complete (exit 1)."
        )
    }

    /// The literal this file's phase decision rests on has to be the token the
    /// vocabulary knows, or `unreadableOutcomePhase` would silently stop
    /// recognising a held lock while every other surface still worded it
    /// correctly.
    func testTheBusyTokenMatchesTheVocabularyLabel() {
        XCTAssertEqual(
            GlomerisVocabulary.refusal(RecoverySectionView.executionLockBusyReason).token,
            RecoverySectionView.executionLockBusyReason,
            "an unrecognised token comes back with the fallback wording, not its own"
        )
    }

    // MARK: - The run guard

    /// The compare-and-set, for the reason `PlanState.beginApplyingBatch()`
    /// documents: two concurrent runs would take turns losing to an exclusive
    /// execution lock, and the second would be told `busy` with us as the cause.
    func testOnlyOneRunCanClaimTheRightToRun() {
        let state = RecoveryState()
        XCTAssertFalse(state.isRecovering)
        XCTAssertTrue(state.beginRecovering())
        XCTAssertFalse(state.beginRecovering(), "a second run must be refused, not queued")
        state.endRecovering()
        XCTAssertTrue(state.beginRecovering(), "and the right is reclaimable once the run ends")
    }

    /// Changing the goal clears the pre-flight, because every figure in one is
    /// measured against the goal that produced it.
    func testChangingTheGoalClearsTheProgressItWasMeasuredAgainst() throws {
        let state = RecoveryState()
        state.phase = .reviewing(
            try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        )
        state.progressStatusText = "searching…"
        state.lastErrorMessage = "something"

        state.resetGoalProgress()

        XCTAssertEqual(state.phase, .idle)
        XCTAssertNil(state.progressStatusText)
        XCTAssertNil(state.lastErrorMessage)
    }

    /// And it does not stop a run, because nothing can: the absent task handle is
    /// the safety property, not an omission. A reset that cleared `isRecovering`
    /// would let a second child start while the first was still deleting.
    func testResettingProgressDoesNotReleaseARunningRecovery() {
        let state = RecoveryState()
        XCTAssertTrue(state.beginRecovering())

        state.resetGoalProgress()

        XCTAssertTrue(state.isRecovering, "clearing display state must not release the run guard")
        XCTAssertFalse(state.beginRecovering())
    }

    // MARK: - The live progress stream (HORO-1509)

    /// Decoding one line per phase, with the JSON written out here rather than
    /// read from a fixture.
    ///
    /// That is deliberate and it is the opposite of this file's rule for
    /// reports. A report DTO is pinned against `tests/fixtures/dto/`, which the
    /// Rust builders produce, so a renamed field turns both sides red at once.
    /// The progress stream has no such fixture — it is NDJSON on stderr, not a
    /// report document — so the literals here *are* the contract, and they were
    /// written by reading `RecoveryProgressEvent`'s `#[serde]` attributes in
    /// `src/reporting/dto.rs`. The Rust side pins its own half:
    /// `every_progress_phase_is_distinctly_named_and_carries_its_iteration`
    /// asserts the seven phase tokens and their shape.
    private func progressEvent(_ json: String) throws -> RecoveryProgressEventDto {
        try JSONDecoder().decode(RecoveryProgressEventDto.self, from: Data(json.utf8))
    }

    func testEveryProgressPhaseDecodesAndCarriesItsIteration() throws {
        let lines = [
            #"{"phase":"measured","iteration":1,"total_bytes":500,"free_bytes":100,"#
                + #""used_percent":80.0,"free_human":"100 B","bytes_freed_so_far":0,"#
                + #""bytes_freed_so_far_human":"0 B"}"#,
            #"{"phase":"discovering","iteration":2}"#,
            #"{"phase":"discovered","iteration":3,"candidates":4,"detectors_failed":1}"#,
            #"{"phase":"revalidating","iteration":4}"#,
            #"{"phase":"action_started","iteration":5,"resource":"cargo:/p/target","#
                + #""action":"cargo_clean","policy_label":"AUTO_SAFE"}"#,
            #"{"phase":"action_finished","iteration":6,"resource":"cargo:/p/target","#
                + #""action":"cargo_clean","outcome":"succeeded","bytes_freed_so_far":9,"#
                + #""bytes_freed_so_far_human":"9 B"}"#,
            #"{"phase":"stop_requested","iteration":7}"#,
        ]

        let decoded = try lines.map { try progressEvent($0) }
        XCTAssertEqual(
            decoded.map(\.iteration), [1, 2, 3, 4, 5, 6, 7],
            "every variant carries the pass that produced it, so a display can "
                + "always say which round it is on"
        )
    }

    func testAMeasuredLineCarriesTheUsageItReadAndTheBytesAlreadyFreed() throws {
        let event = try progressEvent(
            #"{"phase":"measured","iteration":2,"total_bytes":500,"free_bytes":120,"#
                + #""used_percent":76.0,"free_human":"120 B","bytes_freed_so_far":20,"#
                + #""bytes_freed_so_far_human":"20 B"}"#
        )
        guard case .measured(let payload) = event else {
            return XCTFail("expected a measured event, got \(event)")
        }
        XCTAssertEqual(payload.totalBytes, 500)
        XCTAssertEqual(payload.freeBytes, 120)
        XCTAssertEqual(payload.usedPercent, 76.0, accuracy: 0.001)
        XCTAssertEqual(payload.freeHuman, "120 B")
        // Re-measured free space, not a sum of estimates (campaign §9). This is
        // the one figure in the stream a progress display may show as progress.
        XCTAssertEqual(payload.bytesFreedSoFar, 20)
        XCTAssertEqual(payload.bytesFreedSoFarHuman, "20 B")
    }

    /// Rust omits a byte count it could not measure rather than serializing
    /// zero, and `nil` here has to keep meaning "nobody measured this". A
    /// decoder that defaulted either field to `0` would let the card print
    /// "0 B reclaimed" for an action whose size is simply unknown.
    func testAnUnmeasuredByteCountDecodesAsAbsentRatherThanZero() throws {
        let started = try progressEvent(
            #"{"phase":"action_started","iteration":1,"resource":"brew:cache","#
                + #""action":"brew_cleanup","policy_label":"ASK"}"#
        )
        guard case .actionStarted(let startedPayload) = started else {
            return XCTFail("expected an action_started event, got \(started)")
        }
        XCTAssertNil(startedPayload.estimatedBytes)
        XCTAssertNil(startedPayload.estimatedHuman)
        XCTAssertEqual(startedPayload.policyLabel, "ASK")

        let finished = try progressEvent(
            #"{"phase":"action_finished","iteration":1,"resource":"brew:cache","#
                + #""action":"brew_cleanup","outcome":"failed","bytes_freed_so_far":0,"#
                + #""bytes_freed_so_far_human":"0 B"}"#
        )
        guard case .actionFinished(let finishedPayload) = finished else {
            return XCTFail("expected an action_finished event, got \(finished)")
        }
        XCTAssertNil(
            finishedPayload.reclaimedBytes,
            "an action whose reclaim could not be measured must not report a measurement"
        )
        XCTAssertNil(finishedPayload.reclaimedHuman)
        // The cumulative figure is always present, because it is always measured.
        XCTAssertEqual(finishedPayload.bytesFreedSoFar, 0)
    }

    /// The estimate is decoded into a differently-named field from the
    /// measurement, so no call site can reach for one and get the other.
    func testAnEstimateAndAMeasurementAreSeparatelyNamedFields() throws {
        let started = try progressEvent(
            #"{"phase":"action_started","iteration":1,"resource":"cargo:/p/target","#
                + #""action":"cargo_clean","policy_label":"AUTO_SAFE","#
                + #""estimated_bytes":2048,"estimated_human":"2.0 KB"}"#
        )
        guard case .actionStarted(let payload) = started else {
            return XCTFail("expected an action_started event, got \(started)")
        }
        XCTAssertEqual(payload.estimatedBytes, 2048)
        XCTAssertEqual(payload.estimatedHuman, "2.0 KB")

        let finished = try progressEvent(
            #"{"phase":"action_finished","iteration":1,"resource":"cargo:/p/target","#
                + #""action":"cargo_clean","outcome":"succeeded","reclaimed_bytes":1024,"#
                + #""reclaimed_human":"1.0 KB","bytes_freed_so_far":1024,"#
                + #""bytes_freed_so_far_human":"1.0 KB"}"#
        )
        guard case .actionFinished(let payload) = finished else {
            return XCTFail("expected an action_finished event, got \(finished)")
        }
        // An estimate of 2 KB and a measured reclaim of 1 KB, on purpose: a
        // decoder that read the estimate as the result would pass with equal
        // values and fail here.
        XCTAssertEqual(payload.reclaimedBytes, 1024)
        XCTAssertEqual(payload.outcome, "succeeded")
    }

    /// An unknown phase throws, and `GlomerisClient` skips a line that throws —
    /// so a CLI newer than this app costs one missed status update rather than a
    /// failed run. Asserted here because the tolerance depends on the throw.
    func testAnUnknownProgressPhaseIsRejectedRatherThanGuessedAt() {
        XCTAssertThrowsError(try progressEvent(#"{"phase":"reticulating","iteration":1}"#))
    }

    /// The two NDJSON streams share the `phase` key and nothing else, which is
    /// why they are separate types. Decoding either line as the other's type has
    /// to fail rather than half-succeed: `detect`'s scan and a run that deletes
    /// things are not interchangeable, and a display that took one for the other
    /// would report a mutation as a search.
    func testTheDiscoveryAndRecoveryStreamsCannotBeDecodedAsEachOther() {
        let discovery = #"{"phase":"detector_started","detector":"cargo_target_dir"}"#
        let recovery = #"{"phase":"discovering","iteration":1}"#

        XCTAssertThrowsError(
            try JSONDecoder().decode(RecoveryProgressEventDto.self, from: Data(discovery.utf8))
        )
        XCTAssertThrowsError(
            try JSONDecoder().decode(ProgressEventDto.self, from: Data(recovery.utf8))
        )
    }
}
