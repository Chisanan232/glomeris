//
//  RecoveryDeepLinkTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1508 AC 2, as values: the one token this app names, and the figures the
//  Recovery surface is handed when an alert is answered.
//
//  The first test here is the whole reason `RecoveryDeepLink.reviewToken` is
//  allowed to exist. Everywhere else the answers are opaque tokens read from
//  `responses`; this one literal decides behaviour, so it is checked against the
//  set the CLI actually publishes. If Rust renames the answer, this test fails —
//  rather than the button silently becoming inert, which is a failure with no error
//  message anywhere and no way for a user to tell it from a slow window.
//

import XCTest

final class RecoveryDeepLinkTests: XCTestCase {

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

    // MARK: - The one named token

    /// The guard against a silently inert button. The token this app compares
    /// against must be one the CLI offers.
    func testTheTokenThatOpensRecoveryIsOneTheCliPublishes() throws {
        XCTAssertTrue(
            try report().responses.contains(RecoveryDeepLink.reviewToken),
            "no published answer matches RecoveryDeepLink.reviewToken, so \"Review & recover\" "
                + "would open nothing and report nothing — check whether EpisodeResponse::as_str "
                + "was renamed in src/monitor/episode.rs"
        )
    }

    /// And it is the *only* one, so neither of the answers whose point is to leave
    /// the user alone brings a window forward.
    func testOnlyThatOneAnswerOpensRecovery() throws {
        let published = try report().responses
        XCTAssertEqual(
            published.filter(RecoveryDeepLink.opensRecovery),
            [RecoveryDeepLink.reviewToken]
        )
        XCTAssertFalse(RecoveryDeepLink.opensRecovery("remind_later"))
        XCTAssertFalse(RecoveryDeepLink.opensRecovery("ignore_episode"))
    }

    /// A token nobody published cannot open a window, which is the same fail-closed
    /// shape the rest of the pressure path has.
    func testAnUnknownTokenOpensNothing() {
        XCTAssertFalse(RecoveryDeepLink.opensRecovery(""))
        XCTAssertFalse(RecoveryDeepLink.opensRecovery("review"))
        XCTAssertFalse(RecoveryDeepLink.opensRecovery("Review_And_Recover"))
    }

    // MARK: - The context

    /// The two percentages arrive as two separate pieces of wording. That is the
    /// campaign's central product claim about these numbers — one is when to speak,
    /// the other is where a run stops — and a context carrying a single "level"
    /// would make the surface unable to say which it had.
    func testTheContextCarriesTheThresholdAndTheGoalSeparately() throws {
        let context = RecoveryDeepLinkContext(report: try report())

        XCTAssertEqual(context.notifyAtDescription, "85% used")
        XCTAssertEqual(context.goalDescription, "60% used (40% free)")
        XCTAssertNotEqual(context.notifyAtDescription, context.goalDescription)
    }

    func testTheContextCarriesTheReadingTheAlertWasAbout() throws {
        let context = RecoveryDeepLinkContext(report: try report())

        XCTAssertEqual(context.currentUsedPercent, 91.0)
        XCTAssertEqual(context.currentFreeHuman, "41.9 GB")
        XCTAssertEqual(context.episodeId, 1)
    }

    /// The goal reaches the Recovery card's own control through the card's own
    /// conversion, not a second one written here. Two rules for one number would
    /// eventually show a user a goal their setting does not name, on the screen
    /// where it is about to be acted on.
    func testTheGoalIsSeededThroughTheRecoveryCardsOwnConversion() throws {
        let dto = try report()
        let context = RecoveryDeepLinkContext(report: dto)

        XCTAssertEqual(context.goalUsedPercent, 60)
        XCTAssertEqual(
            context.goalUsedPercent,
            RecoverySectionView.storedDefaultGoal(usedPercent: dto.defaultGoal.usedPercent),
            "the deep link converts the goal by a different rule than the card it seeds"
        )
    }

    /// Recovery can be opened with no episode behind it — the disk recovered before
    /// the button was pressed, or the user came in through the app rather than an
    /// alert. The context says so rather than inventing an id.
    func testAContextWithNoEpisodeNamesNone() throws {
        let context = RecoveryDeepLinkContext(
            report: try report("pressure_status_report_no_episode.json")
        )

        XCTAssertNil(context.episodeId)
        // Still fully populated otherwise: the surface has something to show.
        XCTAssertEqual(context.currentUsedPercent, 60.0)
        XCTAssertEqual(context.currentFreeHuman, "186.3 GB")
        XCTAssertFalse(context.notifyAtDescription.isEmpty)
        XCTAssertFalse(context.goalDescription.isEmpty)
    }

    /// Two readings of the same disk are two contexts. Equatable is what lets a
    /// test say "opened on the reading taken when the button was pressed" rather
    /// than "opened", so it has to actually discriminate.
    func testTwoReadingsAreNotTheSameContext() throws {
        var object = try XCTUnwrap(
            try JSONSerialization.jsonObject(
                with: try Data(
                    contentsOf: Self.fixturesDir.appendingPathComponent(
                        "pressure_status_report.json")
                )
            ) as? [String: Any]
        )
        var current = try XCTUnwrap(object["current"] as? [String: Any])
        current["used_percent"] = 77.0
        object["current"] = current
        let later = try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try JSONSerialization.data(withJSONObject: object)
        )

        XCTAssertNotEqual(
            RecoveryDeepLinkContext(report: try report()),
            RecoveryDeepLinkContext(report: later)
        )
    }
}
