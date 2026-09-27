//
//  RecoveryDeepLinkWindowTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1508 AC 2, the landing end: what the Recovery window is holding once
//  "Review & recover" has opened it.
//
//  A test may not put a window on screen, which is exactly why the deciding half of
//  that file is a separate object. What is asserted here is the only thing in it
//  that could be wrong quietly: whether the goal the alert named reaches the
//  control, and whether arriving a second time can overwrite a goal the user has
//  already set by hand. The second case is the dangerous one — a number silently
//  replaced on the screen where it is about to be acted on, while the user is
//  looking at the screen.
//

import XCTest

@MainActor
final class RecoveryDeepLinkWindowTests: XCTestCase {

    // MARK: - Fixtures

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .deletingLastPathComponent()  // macos
            .deletingLastPathComponent()  // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func context(_ name: String = "pressure_status_report.json") throws
        -> RecoveryDeepLinkContext
    {
        RecoveryDeepLinkContext(
            report: try JSONDecoder().decode(
                PressureStatusReportDto.self,
                from: try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
            )
        )
    }

    // MARK: - Arriving from an alert

    func testArrivingFromAnAlertKeepsTheReadingItArrivedOn() throws {
        let model = RecoveryDeepLinkWindowModel()

        model.adopt(try context())

        XCTAssertEqual(model.context, try context())
        XCTAssertEqual(model.context?.currentUsedPercent, 91.0)
    }

    /// The goal the alert named becomes the goal the control holds. Without this the
    /// window would open on its own default — 70% used against a configured 60% —
    /// and the first thing the user saw would contradict their own setting.
    func testTheGoalTheAlertNamedSeedsTheControl() throws {
        let model = RecoveryDeepLinkWindowModel()
        XCTAssertNotEqual(
            model.recovery.goalUsedPercent, 60,
            "the default already equals the fixture's goal, so this test cannot observe seeding"
        )

        model.adopt(try context())

        XCTAssertEqual(model.recovery.goalUsedPercent, 60)
    }

    /// Seeded through the deep link's figure, which is itself the Recovery card's
    /// own conversion. Pinned against the context rather than against a literal so
    /// this stays true if the fixture's goal changes.
    func testTheSeededGoalIsTheOneTheContextCarries() throws {
        let model = RecoveryDeepLinkWindowModel()
        let arrived = try context()

        model.adopt(arrived)

        XCTAssertEqual(model.recovery.goalUsedPercent, arrived.goalUsedPercent)
    }

    // MARK: - A goal the user chose is theirs

    /// The one that matters. A second alert can arrive while the user is deciding
    /// what to recover — the daemon polls every 30 seconds — and it must not move
    /// the number they just set. `adoptStoredDefaultGoal` is what declines, and this
    /// asserts the window actually goes through it rather than assigning.
    func testASecondAlertDoesNotOverwriteAGoalTheUserChose() throws {
        let model = RecoveryDeepLinkWindowModel()
        model.adopt(try context())

        model.recovery.goalUsedPercent = 40
        model.recovery.noteUserChoseGoal()
        model.adopt(try context())

        XCTAssertEqual(
            model.recovery.goalUsedPercent, 40,
            "an arriving alert replaced the goal the user set by hand"
        )
    }

    /// But the header still updates, so the window is not left claiming the disk is
    /// where it was at the first alert.
    func testASecondAlertStillUpdatesWhyTheWindowIsOpen() throws {
        let model = RecoveryDeepLinkWindowModel()
        model.adopt(try context())

        model.adopt(try context("pressure_status_report_snoozed.json"))

        XCTAssertEqual(model.context?.currentUsedPercent, 88.0)
        XCTAssertEqual(model.context?.currentFreeHuman, "55.9 GB")
    }

    // MARK: - Arriving with nothing to say

    /// Recovery opens even when the disk could not be read for the answer, because
    /// the alternative is a button that does nothing. The header is dropped rather
    /// than filled with guesses, and the goal is left at whatever it was — this also
    /// covers the branch a context with an unrepresentable goal takes.
    func testArrivingWithNoReportOpensWithNoHeaderAndTouchesNothing() {
        let model = RecoveryDeepLinkWindowModel()
        let before = model.recovery.goalUsedPercent

        model.adopt(nil)

        XCTAssertNil(model.context)
        XCTAssertEqual(model.recovery.goalUsedPercent, before)
    }

    /// And a failed read after a good one clears the header rather than leaving a
    /// stale reading on screen as though it were current.
    func testAFailedReadAfterAGoodOneClearsTheHeader() throws {
        let model = RecoveryDeepLinkWindowModel()
        model.adopt(try context())

        model.adopt(nil)

        XCTAssertNil(model.context)
    }

    // MARK: - One recovery state, kept

    /// The window holds the same state across re-openings. A fresh one each time
    /// would discard the result of a run that had already deleted things — the only
    /// account anywhere in the UI of what went — the moment the next alert arrived.
    func testReopeningKeepsTheSameRecoveryState() throws {
        let model = RecoveryDeepLinkWindowModel()
        let state = model.recovery

        model.adopt(try context())
        model.adopt(try context("pressure_status_report_snoozed.json"))

        XCTAssertTrue(state === model.recovery)
    }
}
