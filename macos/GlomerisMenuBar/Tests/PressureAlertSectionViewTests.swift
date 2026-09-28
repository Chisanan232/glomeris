//
//  PressureAlertSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1508 AC 6, as values: what the in-app alert card shows, when it shows at
//  all, and what a screen reader is told.
//
//  These are the claims that cannot be checked by looking at the card. Whether it
//  appears for a user who already dismissed the banner is a decision about a
//  *different* field than the banner reads, and getting it wrong leaves a person
//  with an over-full disk and no route to the three answers. What VoiceOver says is
//  invisible on screen by definition. And a background reader that has stopped
//  looks exactly like a healthy disk unless something insists on saying otherwise.
//

import XCTest

final class PressureAlertSectionViewTests: XCTestCase {

    // MARK: - Fixtures

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .deletingLastPathComponent()  // macos
            .deletingLastPathComponent()  // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func report(_ name: String = "pressure_status_report.json") throws
        -> PressureStatusReportDto
    {
        try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
        )
    }

    /// A reported run that reclaimed 139.7 GB, for the clause that depends on what an
    /// automatic run did (HORO-1510).
    private func runReport() throws -> RecoveryRunReportDto {
        try JSONDecoder().decode(
            RecoveryRunReportDto.self,
            from: try Data(
                contentsOf: Self.fixturesDir.appendingPathComponent("recovery_run_report.json")
            )
        )
    }

    // MARK: - When the card appears

    func testAnOpenEpisodeShowsTheCard() throws {
        XCTAssertTrue(
            PressureAlertPresentation.isShown(report: try report(), errorMessage: nil)
        )
    }

    /// The load-bearing one. This fixture is an episode the user already answered
    /// with "Remind me later": still open, still over the threshold, and
    /// `notification_due` false. Keying the card on the banner's field instead of
    /// the episode's existence would hide the only remaining route to the three
    /// answers from exactly the person who asked to be reminded.
    func testAnEpisodeWithNoBannerOwedStillShowsTheCard() throws {
        let snoozed = try report("pressure_status_report_snoozed.json")

        XCTAssertFalse(snoozed.notificationDue, "fixture no longer exercises this case")
        XCTAssertTrue(PressureAlertPresentation.isShown(report: snoozed, errorMessage: nil))
    }

    func testAHealthyDiskShowsNothing() throws {
        XCTAssertFalse(
            PressureAlertPresentation.isShown(
                report: try report("pressure_status_report_no_episode.json"),
                errorMessage: nil
            )
        )
    }

    /// A reader that has stopped must not read as a disk that is fine. There is no
    /// report at all in this case — the first poll failed — so the card has to be
    /// driven by the error on its own.
    func testAFailedReadShowsTheCardWithNoReport() {
        XCTAssertTrue(
            PressureAlertPresentation.isShown(report: nil, errorMessage: "glomeris not found")
        )
    }

    func testNothingAtAllShowsNothing() {
        XCTAssertFalse(PressureAlertPresentation.isShown(report: nil, errorMessage: nil))
    }

    // MARK: - The figures

    /// The app renders one percentage one way. A card that said "91%" next to a
    /// Recovery card saying "91.0% used" would read as two different measurements.
    func testTheHeadlineMatchesTheAppsOwnPercentRendering() throws {
        let dto = try report()

        XCTAssertEqual(PressureAlertPresentation.headline(report: dto), "91.0% used")
        XCTAssertTrue(
            PressureAlertPresentation.headline(report: dto).hasSuffix(" used"),
            "a bare percentage does not say whether it means used or free"
        )
    }

    // MARK: - What a listener hears

    /// The three figures reach a screen reader as three separately named clauses.
    /// The campaign's accessibility rule names this case directly, and it is the
    /// one place a sighted reader cannot check by looking: told a single
    /// percentage, a listener has been told the wrong thing about when Glomeris
    /// speaks versus where a run stops.
    func testTheSpokenStateTellsTheThreeFiguresApart() throws {
        let dto = try report()
        let spoken = PressureAlertPresentation.spokenState(report: dto, automaticRun: nil)

        XCTAssertTrue(spoken.contains("Disk now: 91.0% used, 41.9 GB free."), spoken)
        XCTAssertTrue(spoken.contains("Alert threshold: 85% used."), spoken)
        XCTAssertTrue(spoken.contains("Recovery goal: 60% used (40% free)."), spoken)
    }

    /// And it ends by saying nothing has happened yet. Someone who has just been
    /// told their disk is 91% full by a program that deletes files should hear that
    /// before they decide anything.
    func testTheSpokenStateSaysNothingHasBeenDeleted() throws {
        XCTAssertTrue(
            PressureAlertPresentation.spokenState(report: try report(), automaticRun: nil)
                .hasSuffix("Nothing has been deleted.")
        )
    }

    /// And on an opted-in Mac it accounts for the run instead, because this card can
    /// appear *after* a bounded run that did not reach the goal. A listener told that
    /// nothing had been deleted would have been read a sentence the app knows to be
    /// false (HORO-1510) — and a listener is the one reader who cannot check.
    func testTheSpokenStateAccountsForAnAutomaticRunRatherThanDenyingIt() throws {
        let spoken = PressureAlertPresentation.spokenState(
            report: try report(),
            automaticRun: .ran(try runReport())
        )

        XCTAssertFalse(spoken.contains("Nothing has been deleted"), spoken)
        XCTAssertTrue(spoken.hasSuffix("Autopilot already reclaimed 139.7 GB."), spoken)
        // The figures are still there, and still first: the account is the last clause
        // so a listener reaches the three numbers without waiting through it.
        XCTAssertTrue(spoken.hasPrefix("Disk now: 91.0% used, 41.9 GB free."), spoken)
    }

    /// AC 2's in-the-moment half, spoken. The mode's own `summary` answers a label, so
    /// heard on its own — "You will be asked first" with no axis — it could be about
    /// anything on the card. It is read with the axis attached, the way every other row
    /// in this app is.
    func testTheModeIsSpokenWithItsAxisAttached() {
        for mode in [
            AutopilotUnpromptedMode.askedFirst, .startsOnPressure, .dormantWhileRevoked,
        ] {
            let spoken = PressureAlertPresentation.spokenMode(mode)

            XCTAssertEqual(
                spoken,
                SpokenLabel.compose([
                    SpokenLabel.clause(PressureAlertPresentation.unpromptedModeLabel, mode.summary)
                ])
            )
            XCTAssertTrue(spoken.contains(mode.summary), spoken)
            XCTAssertTrue(
                spoken.hasPrefix(PressureAlertPresentation.unpromptedModeLabel),
                spoken
            )
        }
    }

    /// The three modes are told apart by what is *said*, not only by glyph and colour
    /// (campaign §14). A listener who hears the same sentence whichever mode is in
    /// force has not been told which one it is.
    func testTheThreeModesAreSpokenDistinctly() {
        let spoken = [
            AutopilotUnpromptedMode.askedFirst, .startsOnPressure, .dormantWhileRevoked,
        ].map(PressureAlertPresentation.spokenMode)

        XCTAssertEqual(Set(spoken).count, 3, "\(spoken)")
    }

    /// Composed through the shared composer, so every spoken row in the app
    /// terminates its clauses the same way. Asserted against `SpokenLabel` itself
    /// rather than against a sentence retyped here.
    func testTheSpokenStateIsComposedTheWayEveryOtherRowIs() throws {
        let dto = try report()

        XCTAssertEqual(
            PressureAlertPresentation.spokenState(report: dto, automaticRun: nil),
            SpokenLabel.compose([
                SpokenLabel.clause(
                    "Disk now",
                    "\(PressureAlertPresentation.headline(report: dto)), "
                        + "\(dto.current.freeHuman) free"
                ),
                SpokenLabel.clause("Alert threshold", dto.notifyAtDescription),
                SpokenLabel.clause("Recovery goal", dto.defaultGoal.description),
                UnpromptedRecoveryAccount.nothingDeleted,
            ])
        )
    }

    // MARK: - The answers

    /// The buttons are explained the same way the notification's are. Two sets of
    /// wording for one answer is how a user comes to believe "Ignore this alert"
    /// means two different things depending on where they pressed it.
    func testTheAnswerHintsComeFromTheSharedVocabulary() throws {
        for token in try report().responses {
            XCTAssertEqual(
                PressureAlertPresentation.answerHint(token),
                GlomerisVocabulary.episodeResponse(token).explanation
            )
            XCTAssertFalse(
                PressureAlertPresentation.answerHint(token).isEmpty,
                "\(token) has no explanation, so its button would have an empty hint"
            )
        }
    }

    /// An answer this app has never heard of still gets a hint rather than an empty
    /// one — the same fail-soft shape the notification's buttons have, so a CLI that
    /// grows a fourth answer produces a labelled button rather than a blank row.
    func testAnUnfamiliarAnswerStillGetsAHint() {
        XCTAssertFalse(PressureAlertPresentation.answerHint("defer_to_tuesday").isEmpty)
    }

    /// The card names itself as an alert about pressure, not as an action. Its
    /// title is the first thing read in the panel's reading order, and the panel
    /// already has a "Recovery goal" card underneath it.
    func testTheCardTitleIsDistinctFromTheRecoveryCards() {
        XCTAssertNotEqual(
            PressureAlertPresentation.cardTitle, RecoverySectionView.cardTitle
        )
    }
}
