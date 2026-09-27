//
//  PressureNotificationTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1508 AC 1, 2, 4 and 6, as values: what the banner says, what its buttons
//  are, which token comes back from a press, and the two rules whose failure is
//  invisible on screen.
//
//  The measured finding this file exists to protect: re-posting a request whose
//  identifier has already been delivered updates the existing notification *in
//  place, with no new banner*. So an identifier keyed on the episode id alone
//  would make the reminder after a snooze show nothing at all — and nothing at all
//  is exactly what a working snooze looks like, right up until the moment it
//  should have spoken.
//
//  Delivery itself is not asserted here: `UNUserNotificationCenter.current()`
//  traps in a process with no app bundle. Everything that decides *what* is
//  delivered is a pure function, and those are asserted directly.
//

import UserNotifications
import XCTest

final class PressureNotificationTests: XCTestCase {

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

    private func banner(notificationsRaised: UInt32 = 0) throws -> PressureBanner {
        PressureBanner(
            key: PressureBannerKey(episodeId: 1, notificationsRaised: notificationsRaised),
            report: try report()
        )
    }

    // MARK: - The identifier

    /// The rule that cannot be seen failing. Two banners for one episode must have
    /// two identifiers, or the second one silently updates the first instead of
    /// appearing.
    func testTheReminderForAnEpisodeGetsItsOwnRequestIdentifier() throws {
        let first = PressureNotificationContent.requestIdentifier(
            for: PressureBannerKey(episodeId: 1, notificationsRaised: 0)
        )
        let reminder = PressureNotificationContent.requestIdentifier(
            for: PressureBannerKey(episodeId: 1, notificationsRaised: 1)
        )
        XCTAssertNotEqual(
            first, reminder,
            "the post-snooze reminder re-uses a delivered identifier, so macOS will update the "
                + "old banner in place and the user will see nothing"
        )
    }

    /// The other half: the *same* banner re-posted keeps its identifier, so a
    /// hovering disk updates one notification rather than stacking them.
    func testTheSameBannerKeepsOneIdentifier() {
        let key = PressureBannerKey(episodeId: 7, notificationsRaised: 2)
        XCTAssertEqual(
            PressureNotificationContent.requestIdentifier(for: key),
            PressureNotificationContent.requestIdentifier(for: key)
        )
    }

    /// Distinct episodes are distinct notifications even at the same raised count.
    func testDifferentEpisodesGetDifferentIdentifiers() {
        XCTAssertNotEqual(
            PressureNotificationContent.requestIdentifier(
                for: PressureBannerKey(episodeId: 1, notificationsRaised: 0)
            ),
            PressureNotificationContent.requestIdentifier(
                for: PressureBannerKey(episodeId: 2, notificationsRaised: 0)
            )
        )
    }

    /// One episode's banners are one conversation, so the thread identifier is the
    /// episode's and does not move when a reminder is raised.
    func testAnEpisodesBannersShareOneThread() throws {
        XCTAssertEqual(
            PressureNotificationContent.threadIdentifier(episodeId: 1),
            PressureNotificationContent.threadIdentifier(episodeId: 1)
        )
        XCTAssertNotEqual(
            PressureNotificationContent.threadIdentifier(episodeId: 1),
            PressureNotificationContent.threadIdentifier(episodeId: 2)
        )
    }

    // MARK: - What it says

    func testTheTitleStatesCurrentUsage() throws {
        XCTAssertEqual(PressureNotificationContent.title(for: try banner()), "Disk is 91.0% used")
    }

    /// The threshold and the goal are two sentences' worth of distinct meaning, and
    /// both appear. A banner naming one percentage would leave the reader to guess
    /// which of the two it was.
    func testTheBodyNamesTheThresholdAndTheGoalAsDifferentThings() throws {
        let body = PressureNotificationContent.body(for: try banner())
        XCTAssertTrue(body.contains("41.9 GB free"), body)
        XCTAssertTrue(body.contains("alerted at 85% used"), body)
        XCTAssertTrue(body.contains("recovery goal is 60% used (40% free)"), body)
    }

    /// Crossing a threshold must not start destructive work, and the banner has to
    /// say so where it will be read in a hurry.
    func testTheBodySaysNothingHasBeenDeleted() throws {
        XCTAssertTrue(
            PressureNotificationContent.body(for: try banner()).contains("Nothing has been deleted")
        )
    }

    // MARK: - The buttons

    /// The buttons are the CLI's published answers, in its order, with this app's
    /// wording for each — and the identifier is the token itself, so there is no
    /// second mapping to fall out of step.
    func testTheButtonsAreTheClisAnswersWithTheSharedWording() throws {
        let published = try report().responses
        let actions = PressureNotificationContent.actions(responseTokens: published)

        XCTAssertEqual(actions.map(\.identifier), published)
        XCTAssertEqual(
            actions.map(\.title),
            published.map { GlomerisVocabulary.episodeResponse($0).title }
        )
        // And the wording HORO-1508 asks for by name: no "Skip", because it would
        // be an equally good label for two answers that do different things.
        XCTAssertEqual(actions.map(\.title), ["Review & recover", "Remind me later", "Ignore this alert"])
    }

    /// No action is styled as destructive, because none of them removes anything.
    /// `.destructive` on a notification button is a promise about consequences.
    func testNoButtonIsStyledAsDestructive() throws {
        for action in PressureNotificationContent.actions(responseTokens: try report().responses) {
            XCTAssertFalse(
                action.options.contains(.destructive),
                "\(action.identifier) is styled as destructive, but no answer to an alert deletes "
                    + "anything"
            )
        }
    }

    /// Only the answer that opens a window brings the app forward. A "later" button
    /// that stole focus would defeat its own purpose.
    func testOnlyTheAnswerThatOpensAWindowRunsInTheForeground() throws {
        for action in PressureNotificationContent.actions(responseTokens: try report().responses) {
            XCTAssertEqual(
                action.options.contains(.foreground),
                RecoveryDeepLink.opensRecovery(action.identifier),
                "\(action.identifier) has the wrong foreground behaviour"
            )
        }
    }

    /// A token this app has no wording for still becomes a button. The CLI would
    /// accept it, so dropping it would hide an answer the user is entitled to —
    /// and the vocabulary's fallback gives it honest wording rather than none.
    func testAnUnfamiliarAnswerStillGetsAButton() throws {
        let actions = PressureNotificationContent.actions(
            responseTokens: ["review_and_recover", "escalate_to_founder"]
        )
        XCTAssertEqual(actions.map(\.identifier), ["review_and_recover", "escalate_to_founder"])
        XCTAssertFalse(try XCTUnwrap(actions.last).title.isEmpty)
    }

    func testTheCategoryCarriesExactlyThosePublishedActions() throws {
        let published = try report().responses
        let category = PressureNotificationContent.category(responseTokens: published)
        XCTAssertEqual(category.identifier, PressureNotificationContent.categoryIdentifier)
        XCTAssertEqual(category.actions.map(\.identifier), published)
    }

    // MARK: - The answer coming back

    func testAPressedButtonComesBackAsItsToken() throws {
        let published = try report().responses
        for token in published {
            XCTAssertEqual(
                PressureNotificationContent.answer(
                    forActionIdentifier: token, published: published),
                token
            )
        }
    }

    /// Tapping the banner body means "show me", so it becomes the answer that opens
    /// Recovery — the one answer that deletes nothing and decides nothing.
    func testTappingTheBannerBodyMeansReviewAndRecover() throws {
        let published = try report().responses
        XCTAssertEqual(
            PressureNotificationContent.answer(
                forActionIdentifier: UNNotificationDefaultActionIdentifier,
                published: published
            ),
            RecoveryDeepLink.reviewToken
        )
    }

    /// AC 4's neighbour, and the one worth being strict about: dismissing a banner
    /// is *not* an answer. Recording it as "ignore this alert" would close an
    /// episode the user never answered and put a choice in their mouth.
    func testDismissingTheBannerIsNotAnAnswer() throws {
        XCTAssertNil(
            PressureNotificationContent.answer(
                forActionIdentifier: UNNotificationDismissActionIdentifier,
                published: try report().responses
            )
        )
    }

    /// An identifier that was never offered cannot become an answer, whatever it
    /// says. This is the same fail-closed shape as the rest of the app: the set
    /// that was published is the set that can come back.
    func testAnIdentifierThatWasNeverOfferedIsNotAnAnswer() {
        XCTAssertNil(
            PressureNotificationContent.answer(
                forActionIdentifier: "ignore_episode",
                published: ["review_and_recover", "remind_later"]
            )
        )
    }

    /// And if the CLI never published the answer that opens Recovery, tapping the
    /// body answers nothing rather than sending a token that would be refused.
    func testTappingTheBodyAnswersNothingWhenTheCliDidNotPublishThatAnswer() {
        XCTAssertNil(
            PressureNotificationContent.answer(
                forActionIdentifier: UNNotificationDefaultActionIdentifier,
                published: ["remind_later", "ignore_episode"]
            )
        )
    }

    // MARK: - The request

    func testTheRequestCarriesTheCategoryTheActionsAreRegisteredUnder() throws {
        let request = PressureNotificationContent.request(for: try banner())
        XCTAssertEqual(
            request.content.categoryIdentifier, PressureNotificationContent.categoryIdentifier)
        XCTAssertEqual(
            request.identifier,
            PressureNotificationContent.requestIdentifier(
                for: PressureBannerKey(episodeId: 1, notificationsRaised: 0))
        )
        // Nothing to schedule: the condition is already true.
        XCTAssertNil(request.trigger)
        // Silent. A disk filling up does not need to interrupt a call, and a
        // feature that does gets switched off.
        XCTAssertNil(request.content.sound)
    }
}
