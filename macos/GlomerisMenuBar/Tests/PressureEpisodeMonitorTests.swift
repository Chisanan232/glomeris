//
//  PressureEpisodeMonitorTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1508 AC 7: crossing, recovery below the hysteresis margin, re-crossing,
//  snooze and ignore.
//
//  All five are assertions about values here, not about pixels or about a disk
//  filling up. `PressureBannerPlan.decide` is a pure function of one report and
//  what this session has already put on screen, and `PressureEpisodeMonitor` is
//  driven through `pollOnce()` and `answer(_:)` against a stub CLI, so the
//  sequences that matter — five polls of one owed banner, a banner whose raise
//  failed, a snooze followed by its reminder — are reachable without a daemon and
//  without a notification centre.
//
//  Two claims could not be checked any other way, and they are the reason this
//  file exists rather than a comment:
//
//    * the post-snooze reminder is a *different* banner for the *same* episode.
//      Keyed on the episode id alone it would be suppressed as a duplicate, and
//      "Remind me later" would silently mean "never again" — AC 3 inverted, with
//      nothing on screen to show it;
//    * a raise that failed must not consume the episode's one chance to be shown.
//      `pressure notified` is what says a banner reached the screen, so a raise
//      that did not reach it must leave the record owing one.
//
//  The reports start from the same `tests/fixtures/dto/` fixtures the Rust side
//  asserts `pressure show --json` serializes to. Variants are made by editing
//  that JSON rather than by writing a fresh literal, so a change to the report's
//  shape reaches this file.
//

import XCTest

final class PressureEpisodeMonitorTests: XCTestCase {

    // MARK: - Reports

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

    private func decoded(_ name: String) throws -> PressureStatusReportDto {
        try JSONDecoder().decode(PressureStatusReportDto.self, from: try fixtureData(name))
    }

    /// Threshold 85% used, disk at 91% used, episode 1 open with a banner owed and
    /// none raised yet.
    private func owed() throws -> PressureStatusReportDto {
        try decoded("pressure_status_report.json")
    }

    /// Disk at 60% used, no episode at all — the state after recovery took usage
    /// below the hysteresis margin and the episode closed.
    private func recovered() throws -> PressureStatusReportDto {
        try decoded("pressure_status_report_no_episode.json")
    }

    /// Episode 1, one banner raised, answered "remind later", snooze still
    /// running, so nothing is owed.
    private func snoozed() throws -> PressureStatusReportDto {
        try decoded("pressure_status_report_snoozed.json")
    }

    /// A variant of a fixture, made by editing its JSON.
    ///
    /// The alternative — a hand-written report literal — would stop tracking the
    /// shape Rust serializes the moment a field is added, and would do it
    /// silently. `notificationDue` is written at both levels because the CLI
    /// reports it at both and they are never inconsistent in a real report.
    private func variant(
        of fixture: String = "pressure_status_report.json",
        notificationDue: Bool? = nil,
        episodeId: UInt64? = nil,
        notificationsRaised: UInt32? = nil,
        isSnoozed: Bool? = nil,
        currentUsedPercent: Double? = nil,
        dropEpisode: Bool = false
    ) throws -> PressureStatusReportDto {
        var object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try fixtureData(fixture)) as? [String: Any]
        )
        if let notificationDue {
            object["notification_due"] = notificationDue
        }
        if let currentUsedPercent, var current = object["current"] as? [String: Any] {
            current["used_percent"] = currentUsedPercent
            object["current"] = current
        }
        if var episode = object["episode"] as? [String: Any] {
            if let notificationDue { episode["notification_due"] = notificationDue }
            if let episodeId { episode["episode_id"] = episodeId }
            if let notificationsRaised { episode["notifications_raised"] = notificationsRaised }
            if let isSnoozed { episode["is_snoozed"] = isSnoozed }
            object["episode"] = episode
        }
        if dropEpisode {
            object.removeValue(forKey: "episode")
        }
        return try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try JSONSerialization.data(withJSONObject: object)
        )
    }

    private func rejection() throws -> PressureRejectionReportDto {
        try JSONDecoder().decode(
            PressureRejectionReportDto.self,
            from: try fixtureData("pressure_rejection_report.json")
        )
    }

    // MARK: - The rule, as values

    /// AC 1: a crossing owes one banner.
    func testAnOwedBannerWithNothingRaisedYetIsRaised() throws {
        let decision = PressureBannerPlan.decide(report: try owed(), lastRaised: nil)
        XCTAssertEqual(
            decision,
            .raise(PressureBannerKey(episodeId: 1, notificationsRaised: 0))
        )
    }

    /// AC 5, the half that needs no loop. The CLI's flag and this app's memory are
    /// not simultaneous — the banner goes up, and only then is `pressure notified`
    /// run — so a report still saying a banner is owed must not produce a second
    /// one.
    func testTheSameOwedBannerIsNotRaisedASecondTime() throws {
        let decision = PressureBannerPlan.decide(
            report: try owed(),
            lastRaised: PressureBannerKey(episodeId: 1, notificationsRaised: 0)
        )
        XCTAssertEqual(
            decision,
            .alreadyRaised(PressureBannerKey(episodeId: 1, notificationsRaised: 0))
        )
    }

    /// AC 7, recovery below the hysteresis margin: the episode has closed, so
    /// there is nothing to raise and nothing to remember.
    func testARecoveredDiskOwesNothing() throws {
        XCTAssertEqual(
            PressureBannerPlan.decide(report: try recovered(), lastRaised: nil),
            .nothingOwed
        )
    }

    /// A running snooze owes nothing, and this app does not work that out — Rust
    /// reduced the snooze deadline to `notification_due == false` against its own
    /// clock, which is what keeps a snooze from coming back early on a machine
    /// that slept.
    func testARunningSnoozeOwesNothing() throws {
        let report = try snoozed()
        XCTAssertTrue(report.thresholdCrossed, "the fixture should still be over the threshold")
        XCTAssertEqual(report.episode?.isSnoozed, true)
        XCTAssertEqual(PressureBannerPlan.decide(report: report, lastRaised: nil), .nothingOwed)
    }

    /// AC 3, and the reason `PressureBannerKey` carries `notificationsRaised`.
    ///
    /// When the snooze elapses the daemon makes the *same episode* owe a
    /// notification again. Keyed on the episode id alone this second banner would
    /// be `.alreadyRaised` and never appear, so "Remind me later" would mean
    /// "never again" — the deterministic behaviour AC 3 asks for, silently
    /// inverted.
    func testTheReminderAfterASnoozeIsADifferentBannerForTheSameEpisode() throws {
        let reminder = try variant(
            of: "pressure_status_report_snoozed.json",
            notificationDue: true,
            isSnoozed: false
        )
        XCTAssertEqual(reminder.episode?.episodeId, 1, "still the same episode")
        XCTAssertEqual(reminder.episode?.notificationsRaised, 1)

        let decision = PressureBannerPlan.decide(
            report: reminder,
            lastRaised: PressureBannerKey(episodeId: 1, notificationsRaised: 0)
        )
        XCTAssertEqual(
            decision,
            .raise(PressureBannerKey(episodeId: 1, notificationsRaised: 1)),
            "the post-snooze reminder was suppressed as a duplicate — \"Remind me later\" has "
                + "become \"never again\""
        )
    }

    /// AC 4: "Ignore this pressure event" applies to that event. A later crossing
    /// is a new episode, and a new episode is a new banner — so ignoring one
    /// cannot have silently turned monitoring off.
    func testACrossingAfterAnIgnoredEpisodeStillRaises() throws {
        let fresh = try variant(notificationDue: true, episodeId: 2, notificationsRaised: 0)
        let decision = PressureBannerPlan.decide(
            report: fresh,
            lastRaised: PressureBannerKey(episodeId: 1, notificationsRaised: 0)
        )
        XCTAssertEqual(
            decision,
            .raise(PressureBannerKey(episodeId: 2, notificationsRaised: 0))
        )
    }

    /// The combination `build_pressure_status_report` does not produce, asserted
    /// anyway: there would be no episode to record the answer against, so acting
    /// on it would put buttons on screen that could only be refused.
    func testABannerOwedWithNoEpisodeIsNotRaised() throws {
        let impossible = try variant(notificationDue: true, dropEpisode: true)
        XCTAssertTrue(impossible.notificationDue)
        XCTAssertNil(impossible.episode)
        XCTAssertEqual(
            PressureBannerPlan.decide(report: impossible, lastRaised: nil),
            .nothingOwed
        )
    }

    /// No percentage decides anything here. A disk at 99% used with the threshold
    /// at 85% still owes nothing when Rust says nothing is owed — because the
    /// reason might be a snooze, an answered episode, or a banner already shown,
    /// and a client recomputing "99 >= 85" would get all three wrong.
    func testNoPercentageDecidesWhetherToSpeak() throws {
        let high = try variant(notificationDue: false, currentUsedPercent: 99.0)
        XCTAssertGreaterThan(high.current.usedPercent, high.notifyAtUsedPercent)
        XCTAssertEqual(PressureBannerPlan.decide(report: high, lastRaised: nil), .nothingOwed)
    }

    // MARK: - The banner's content

    /// The two percentages stay two. The campaign's whole point about them is that
    /// a user who reads them as one number has been told the wrong thing: the
    /// threshold is when to speak, the goal is where recovery stops.
    func testTheBannerCarriesTheThresholdAndTheGoalSeparately() throws {
        let report = try owed()
        let banner = PressureBanner(
            key: PressureBannerKey(episodeId: 1, notificationsRaised: 0),
            report: report
        )
        XCTAssertEqual(banner.currentUsedPercent, 91.0)
        XCTAssertEqual(banner.notifyAtDescription, "85% used")
        XCTAssertEqual(banner.goalDescription, "60% used (40% free)")
        XCTAssertNotEqual(
            banner.notifyAtDescription, banner.goalDescription,
            "the alert threshold and the recovery goal must never render as one figure"
        )
        // The CLI's own rendering of free space, not a second formatter's.
        XCTAssertEqual(banner.freeHuman, "41.9 GB")
    }

    /// The buttons are the CLI's answer set. A banner offering anything else would
    /// be refused after the user pressed it.
    func testTheBannersAnswersAreTheOnesTheCliPublished() throws {
        let report = try owed()
        let banner = PressureBanner(
            key: PressureBannerKey(episodeId: 1, notificationsRaised: 0),
            report: report
        )
        XCTAssertEqual(banner.responseTokens, report.responses)
        XCTAssertEqual(banner.responseTokens.count, 3)
    }

    // MARK: - The loop

    @MainActor
    private func makeMonitor(
        showing outcomes: [PressureEpisodeOutcome]
    ) -> (PressureEpisodeMonitor, StubPressureCli, StubBannerRaiser) {
        let cli = StubPressureCli(showOutcomes: outcomes)
        let banners = StubBannerRaiser()
        return (PressureEpisodeMonitor(client: cli, banners: banners), cli, banners)
    }

    /// AC 1: one poll of an owed banner raises it and reports it raised.
    @MainActor
    func testACrossingRaisesOneBannerAndReportsItRaised() async throws {
        let (monitor, cli, banners) = makeMonitor(showing: [.status(try owed())])

        await monitor.pollOnce()

        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertEqual(
            banners.raised.first?.key,
            PressureBannerKey(episodeId: 1, notificationsRaised: 0)
        )
        // Reported back, because `notifications_raised` counts banners the user
        // could have seen rather than banners this app intended.
        XCTAssertEqual(cli.calls, ["show", "notified"])
        XCTAssertEqual(monitor.episode?.episodeId, 1)
        XCTAssertNil(monitor.lastErrorMessage)
    }

    /// AC 5: the storm. Five polls of a record that still says a banner is owed
    /// produce one banner.
    ///
    /// The stub keeps returning the same owed report, which is both the ordinary
    /// lag — the flag is cleared by `pressure notified`, not by raising — and the
    /// worse case where that write failed and the flag stays set for good.
    @MainActor
    func testRepeatedPollsOfOneOwedBannerRaiseItOnce() async throws {
        let (monitor, cli, banners) = makeMonitor(showing: [.status(try owed())])

        for _ in 0..<5 {
            await monitor.pollOnce()
        }

        XCTAssertEqual(
            banners.raised.count, 1,
            "a hovering disk produced \(banners.raised.count) banners from one owed notification"
        )
        XCTAssertEqual(cli.calls.filter { $0 == "notified" }.count, 1)
    }

    /// The other side of that guard, which must not be paid for by losing the
    /// banner: a raise that did not reach the screen leaves the episode owing one,
    /// so the next poll tries again.
    @MainActor
    func testARaiseThatFailedDoesNotConsumeTheEpisodesOneChance() async throws {
        let (monitor, cli, banners) = makeMonitor(showing: [.status(try owed())])
        banners.reachesTheScreen = false

        await monitor.pollOnce()
        await monitor.pollOnce()

        XCTAssertEqual(banners.raised.count, 2, "a failed raise was treated as a shown banner")
        XCTAssertFalse(
            cli.calls.contains("notified"),
            "a banner that never reached the screen was recorded as raised"
        )
    }

    /// AC 7 end to end: crossing, snooze, reminder. Two banners, both for episode
    /// 1, distinguished by the count of banners already raised.
    @MainActor
    func testASnoozedEpisodeRaisesAgainWhenItsReminderComesDue() async throws {
        let reminder = try variant(
            of: "pressure_status_report_snoozed.json",
            notificationDue: true,
            isSnoozed: false
        )
        let (monitor, cli, banners) = makeMonitor(
            showing: [
                .status(try owed()),  // the crossing
                .status(try snoozed()),  // re-read after the answer
                .status(try snoozed()),  // the snooze still running
                .status(reminder),  // and the reminder coming due
            ]
        )

        await monitor.pollOnce()  // crossing → banner
        cli.respondOutcome = .status(try snoozed())
        await monitor.answer("remind_later")  // user snoozes
        await monitor.pollOnce()  // snooze still running → nothing
        await monitor.pollOnce()  // reminder due → banner again

        XCTAssertEqual(banners.raised.count, 2)
        XCTAssertEqual(
            banners.raised.map(\.key),
            [
                PressureBannerKey(episodeId: 1, notificationsRaised: 0),
                PressureBannerKey(episodeId: 1, notificationsRaised: 1),
            ]
        )
        XCTAssertTrue(cli.calls.contains("respond:remind_later"))
    }

    /// AC 7, recovery: usage falls below the hysteresis margin, the episode
    /// closes, and the monitor stops holding one.
    @MainActor
    func testRecoveryBelowTheMarginClearsTheEpisodeAndRaisesNothingFurther() async throws {
        let (monitor, _, banners) = makeMonitor(
            showing: [.status(try owed()), .status(try recovered())]
        )

        await monitor.pollOnce()
        await monitor.pollOnce()

        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertNil(monitor.episode, "the closed episode is still being held")
        XCTAssertEqual(monitor.lastReport?.current.usedPercent, 60.0)
    }

    /// An answer is sent as the token the CLI published, and the record is re-read
    /// at once rather than at the next interval — an in-app surface showing the
    /// pre-answer state for another half minute reads as the answer not having
    /// been taken.
    @MainActor
    func testAnAnswerIsSentAndTheRecordReReadImmediately() async throws {
        let (monitor, cli, _) = makeMonitor(showing: [.status(try snoozed())])
        cli.respondOutcome = .status(try snoozed())

        await monitor.answer("ignore_episode")

        XCTAssertEqual(cli.calls, ["respond:ignore_episode", "show"])
    }

    /// `.nothingToRecord` is good news worded as a refusal: the disk recovered
    /// between the banner appearing and the button being pressed. Reporting it as
    /// a fault would have the user chasing a condition that had already resolved.
    @MainActor
    func testNothingToRecordClearsTheErrorRatherThanSettingOne() async throws {
        let (monitor, cli, _) = makeMonitor(showing: [.malformedOutput])

        await monitor.pollOnce()
        XCTAssertNotNil(monitor.lastErrorMessage, "a skew should have been reported first")

        let refusal = PressureEpisodeOutcome.nothingToRecord(try rejection())
        cli.respondOutcome = refusal
        cli.showOutcomes = [refusal]
        await monitor.answer("review_and_recover")

        XCTAssertNil(monitor.lastErrorMessage)
    }

    /// A background reader that goes quiet is indistinguishable from a disk that
    /// is fine, so every non-answer says something — and a version skew says which
    /// of the two programs is behind.
    @MainActor
    func testAVersionSkewIsReportedRatherThanSwallowed() async throws {
        let (monitor, _, banners) = makeMonitor(
            showing: [.usageError("unknown subcommand 'pressure'")]
        )

        await monitor.pollOnce()

        let message = try XCTUnwrap(monitor.lastErrorMessage)
        XCTAssertTrue(message.contains("unknown subcommand 'pressure'"))
        XCTAssertTrue(banners.raised.isEmpty)
    }

    /// The tokens the notifier builds its buttons from arrive from the report, and
    /// are handed to the raiser before the first banner.
    @MainActor
    func testTheRaiserIsPreparedWithTheClisOwnAnswerSet() async throws {
        let report = try owed()
        let (monitor, _, banners) = makeMonitor(showing: [.status(report)])

        await monitor.pollOnce()

        XCTAssertEqual(banners.prepared, [report.responses])
    }

    /// A button press becomes an answer through the same path a poll does, so the
    /// notification centre's callback cannot reach the CLI by any other route.
    @MainActor
    func testAButtonPressBecomesAnAnswer() async throws {
        let (monitor, cli, banners) = makeMonitor(showing: [.status(try owed())])
        cli.respondOutcome = .status(try snoozed())

        await monitor.pollOnce()
        let answer = try XCTUnwrap(banners.onAnswer)
        answer("remind_later")

        // The hook schedules the answer on the main actor rather than running it
        // inline, so let that task run before reading what was sent.
        for _ in 0..<10 where !cli.calls.contains("respond:remind_later") {
            await Task.yield()
        }
        XCTAssertTrue(cli.calls.contains("respond:remind_later"))
    }

    // MARK: - Review & recover opens Recovery

    @MainActor
    private func makeMonitor(
        showing outcomes: [PressureEpisodeOutcome],
        opening opener: StubRecoveryOpener
    ) -> (PressureEpisodeMonitor, StubPressureCli, StubBannerRaiser) {
        let cli = StubPressureCli(showOutcomes: outcomes)
        let banners = StubBannerRaiser()
        return (
            PressureEpisodeMonitor(client: cli, banners: banners, deepLink: opener), cli, banners
        )
    }

    /// AC 2, and the detail that makes it honest: the surface is handed the disk as
    /// it is *now*, not as it was when the banner went up. A user who took ten
    /// minutes to press the button would otherwise open Recovery on a stale figure
    /// and act on it.
    @MainActor
    func testAnsweringReviewAndRecoverOpensRecoveryOnThePostAnswerReading() async throws {
        let opener = StubRecoveryOpener()
        let (monitor, _, _) = makeMonitor(
            showing: [
                .status(try owed()),  // 91% used, when the banner went up
                .status(try variant(currentUsedPercent: 77.0)),  // and by the time it was pressed
            ],
            opening: opener
        )

        await monitor.pollOnce()
        await monitor.answer("review_and_recover")

        XCTAssertEqual(opener.opened.count, 1)
        let context = try XCTUnwrap(opener.opened.first.flatMap { $0 })
        XCTAssertEqual(
            context.currentUsedPercent, 77.0,
            "Recovery opened on the reading the banner was raised from rather than the one taken "
                + "when the user answered"
        )
        XCTAssertEqual(context.episodeId, 1)
    }

    /// The other two answers adjust when the user is spoken to. Neither opens
    /// anything: a "later" button that took over the screen would be the opposite of
    /// what it says.
    @MainActor
    func testTheAnswersThatAreNotReviewOpenNothing() async throws {
        for token in ["remind_later", "ignore_episode"] {
            let opener = StubRecoveryOpener()
            let (monitor, cli, _) = makeMonitor(
                showing: [.status(try owed())],
                opening: opener
            )

            await monitor.answer(token)

            XCTAssertTrue(cli.calls.contains("respond:" + token))
            XCTAssertTrue(opener.opened.isEmpty, "\(token) opened the Recovery surface")
        }
    }

    /// A refused answer still opens the surface. Both refusals the CLI can give
    /// mean the pressure resolved itself before the button was pressed — which is
    /// news, not an error — and a user who asked to see Recovery should still see
    /// it.
    @MainActor
    func testAReviewAnswerTheRecordRefusedStillOpensRecovery() async throws {
        let opener = StubRecoveryOpener()
        let (monitor, _, _) = makeMonitor(
            showing: [.status(try owed()), .status(try recovered())],
            opening: opener
        )
        // Exit 3: there was no open episode left to answer.

        await monitor.pollOnce()
        await monitor.answer("review_and_recover")

        XCTAssertEqual(opener.opened.count, 1)
        let context = try XCTUnwrap(opener.opened.first.flatMap { $0 })
        // No episode to name, and it does not invent one.
        XCTAssertNil(context.episodeId)
        XCTAssertEqual(context.currentUsedPercent, 60.0)
        XCTAssertNil(monitor.lastErrorMessage)
    }

    /// And with no readable report at all, the surface still opens — without a
    /// header. Pressing "Review & recover" and having *nothing happen*, with no
    /// error anywhere, is the one outcome this deep link exists to prevent; the
    /// Recovery card reads the disk for itself when it appears.
    @MainActor
    func testAReviewAnswerWithNoReadableReportStillOpensRecovery() async throws {
        let opener = StubRecoveryOpener()
        let (monitor, _, _) = makeMonitor(showing: [.malformedOutput], opening: opener)

        await monitor.answer("review_and_recover")

        XCTAssertEqual(opener.opened.count, 1)
        XCTAssertNil(
            try XCTUnwrap(opener.opened.first),
            "a context was invented for a report that could not be read"
        )
    }

    // MARK: - What is not written down

    /// The monitor cannot offer a fourth answer, for the same structural reason
    /// the client cannot: none of the three tokens is spelled in its source, so
    /// there is nowhere for a fourth to be typed. Every token it sends arrived
    /// from `responses`, or from a button built out of it.
    func testTheMonitorSpellsNoAnswerTokenItself() throws {
        let source = try String(
            contentsOf: URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()  // Tests
                .deletingLastPathComponent()  // GlomerisMenuBar
                .appendingPathComponent("Sources/PressureEpisodeMonitor.swift"),
            encoding: .utf8
        )
        for token in ["review_and_recover", "remind_later", "ignore_episode"] {
            XCTAssertFalse(
                source.contains("\"\(token)\""),
                "PressureEpisodeMonitor.swift spells \(token) as a literal — the answers must "
                    + "arrive from the CLI's own `responses`"
            )
        }
    }
}

// MARK: - Doubles

/// A scripted `pressure` CLI.
///
/// `@MainActor` because the monitor is, and because that is where every call
/// actually happens: making the stub main-isolated says so, rather than claiming
/// a thread-safety it does not implement.
///
/// The last scripted `show` outcome repeats. That is what a hovering disk looks
/// like from this app's side — the flag stays set until `pressure notified`
/// succeeds — and it is the sequence AC 5 is about.
@MainActor
private final class StubPressureCli: PressureEpisodeReading {
    var showOutcomes: [PressureEpisodeOutcome]
    var notifiedOutcome: PressureEpisodeOutcome
    var respondOutcome: PressureEpisodeOutcome

    /// Every call in order: `"show"`, `"notified"`, `"respond:<token>"`.
    private(set) var calls: [String] = []

    init(showOutcomes: [PressureEpisodeOutcome]) {
        self.showOutcomes = showOutcomes
        // `pressure notified` and `pressure respond` both exit 0 with a status
        // report on success, so both default to echoing the first scripted report
        // rather than to a failure — otherwise a test about a banner would find an
        // error message on a monitor where nothing had gone wrong, and the
        // assertion that nothing did would be about the stub instead.
        let firstReport = showOutcomes.first { $0.report != nil } ?? .malformedOutput
        notifiedOutcome = firstReport
        respondOutcome = firstReport
    }

    func show() async -> PressureEpisodeOutcome {
        calls.append("show")
        guard let first = showOutcomes.first else { return .malformedOutput }
        if showOutcomes.count > 1 { showOutcomes.removeFirst() }
        return first
    }

    func markNotified() async -> PressureEpisodeOutcome {
        calls.append("notified")
        return notifiedOutcome
    }

    func respond(_ responseToken: String) async -> PressureEpisodeOutcome {
        calls.append("respond:" + responseToken)
        return respondOutcome
    }
}

/// A banner raiser that records instead of notifying.
///
/// `reachesTheScreen` is the one knob that matters: `pressure notified` means a
/// banner the user could have seen, and a test has to be able to say it did not.
@MainActor
private final class StubBannerRaiser: PressureBannerRaising {
    var reachesTheScreen = true
    private(set) var raised: [PressureBanner] = []
    private(set) var prepared: [[String]] = []
    var onAnswer: ((String) -> Void)?

    func prepare(responseTokens: [String]) {
        prepared.append(responseTokens)
    }

    func raise(_ banner: PressureBanner) async -> Bool {
        raised.append(banner)
        return reachesTheScreen
    }
}

/// A Recovery surface that records being asked to open instead of opening.
///
/// The optional element is the point: a `nil` here is a window opened without a
/// pressure header, which is a real and correct outcome, so the record has to be
/// able to tell it apart from not being asked at all.
@MainActor
private final class StubRecoveryOpener: RecoveryDeepLinkOpening {
    private(set) var opened: [RecoveryDeepLinkContext?] = []

    func openRecovery(_ context: RecoveryDeepLinkContext?) {
        opened.append(context)
    }
}
