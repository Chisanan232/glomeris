//
//  StatusHealthSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1062: proves the ticket's core AC — launch-agent-loaded and
//  heartbeat freshness are rendered as two independently distinguishable
//  facts, never collapsed into one combined "healthy" indicator — against
//  DaemonHealthViewModel, the pure mapping step StatusHealthSectionView
//  renders from. Testing the view-model directly (rather than the
//  SwiftUI view tree) is what makes "independently distinguishable"
//  mechanically checkable: two DaemonStatusReportDto inputs that disagree
//  on both axes must produce two view-model outputs that disagree on
//  both axes independently, not just "look different overall".
//

import XCTest

final class StatusHealthSectionViewTests: XCTestCase {
    private func daemonReport(loaded: Bool, heartbeatAgeSecs: UInt64?) -> DaemonStatusReportDto {
        DaemonStatusReportDto(
            plistInstalled: loaded,
            plistPath: "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist",
            loaded: loaded,
            heartbeatAgeSecs: heartbeatAgeSecs
        )
    }

    /// Core AC: loaded=true+fresh-heartbeat vs. loaded=false+no-heartbeat
    /// must differ on the loaded-text axis AND the heartbeat-text axis
    /// independently — not merely differ overall, which a single
    /// collapsed "Healthy"/"Unhealthy" field would already satisfy.
    func testLoadedAndHeartbeatAreIndependentlyDistinguishable() {
        let healthy = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 8))
        let neverRan = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: nil))

        XCTAssertNotEqual(healthy.loadedText, neverRan.loadedText)
        XCTAssertNotEqual(healthy.heartbeatText, neverRan.heartbeatText)

        // Cross-check: a report that is loaded but has never sent a
        // heartbeat must show "loaded" distinctly from a report that is
        // both loaded and has a fresh heartbeat, proving the two facts
        // are not derived from each other.
        let loadedButNoHeartbeat = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: nil))
        XCTAssertEqual(loadedButNoHeartbeat.loadedText, healthy.loadedText)
        XCTAssertNotEqual(loadedButNoHeartbeat.heartbeatText, healthy.heartbeatText)
    }

    func testLoadedTrueRendersYes() {
        let viewModel = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 8))
        XCTAssertEqual(viewModel.loadedText, "Loaded: Yes")
        XCTAssertEqual(viewModel.heartbeatText, "Last heartbeat: 8s ago")
    }

    func testLoadedFalseRendersNo() {
        let viewModel = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: nil))
        XCTAssertEqual(viewModel.loadedText, "Loaded: No")
        XCTAssertEqual(viewModel.heartbeatText, "Last heartbeat: no heartbeat recorded")
    }

    /// The "daemon not installed / never ran" case: `heartbeatAgeSecs`
    /// is `nil`, which must render as an explicit message, not a crash
    /// or a garbage "0s ago"/empty string.
    func testNilHeartbeatAgeRendersNoHeartbeatRecordedNotGarbage() {
        let viewModel = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: nil))
        XCTAssertEqual(viewModel.heartbeatText, "Last heartbeat: no heartbeat recorded")
        XCTAssertFalse(viewModel.heartbeatText.contains("0s"))
    }

    func testHeartbeatAgeFormatsMinutesAndHours() {
        let minutes = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 125))
        XCTAssertEqual(minutes.heartbeatText, "Last heartbeat: 2m ago")

        let hours = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 7_300))
        XCTAssertEqual(hours.heartbeatText, "Last heartbeat: 2h ago")
    }

    // MARK: - HORO-1297: a failing fetch must stay visible

    /// The regression proper. `status` and `daemon status` are dispatched
    /// concurrently and each reports into the shared error state. When the
    /// two shared one slot, a daemon failure was overwritten by the
    /// status success that landed after it — so the panel showed a healthy
    /// disk line and *no* indication that daemon health had failed to
    /// decode at all. Ordering the writes the unlucky way must still leave
    /// the daemon message outstanding.
    func testDaemonFailureIsNotErasedByAConcurrentStatusSuccess() {
        var errors = SectionFetchErrors()

        errors.daemon = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed("typeMismatch(...)"),
            subject: "daemon status"
        )
        // The status fetch finishes second and succeeds — it clears only
        // its own slot.
        errors.status = nil

        XCTAssertEqual(errors.messages.count, 1)
        XCTAssertTrue(errors.messages[0].hasPrefix("daemon status:"))
    }

    /// Symmetry: the same must hold with the roles reversed, so this is a
    /// property of the type rather than an artifact of one ordering.
    func testStatusFailureIsNotErasedByAConcurrentDaemonSuccess() {
        var errors = SectionFetchErrors()
        errors.status = SectionFetchErrors.shortMessage(
            GlomerisClientError.executionFailed("no such file"),
            subject: "status"
        )
        errors.daemon = nil

        XCTAssertEqual(errors.messages.count, 1)
        XCTAssertTrue(errors.messages[0].hasPrefix("status:"))
    }

    func testBothFailuresAreRenderedNotJustTheLastOne() {
        var errors = SectionFetchErrors()
        errors.status = SectionFetchErrors.shortMessage(
            GlomerisClientError.unexpectedExitCode(9),
            subject: "status"
        )
        errors.daemon = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed("dataCorrupted(...)"),
            subject: "daemon status"
        )

        XCTAssertEqual(errors.messages.count, 2)
    }

    func testNoErrorsRendersNothing() {
        XCTAssertTrue(SectionFetchErrors().messages.isEmpty)
    }

    /// A recovered fetch clears its own message, so a transient failure
    /// does not stick around forever.
    func testSucceedingAgainClearsThatFetchsMessage() {
        var errors = SectionFetchErrors()
        errors.daemon = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed("x"),
            subject: "daemon status"
        )
        XCTAssertFalse(errors.messages.isEmpty)

        errors.daemon = nil
        XCTAssertTrue(errors.messages.isEmpty)
    }

    /// "Visibly" means legible. A multi-line `DecodingError` dump in a
    /// 260pt popover is technically visible and practically useless; the
    /// message must be one line that names the subcommand and says the
    /// output was not the expected JSON.
    func testDecodeFailureMessageIsOneActionableLineNotADecodingErrorDump() {
        let message = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed(
                "typeMismatch(Swift.Bool, Swift.DecodingError.Context(codingPath: [], debugDescription: \"…\"))"
            ),
            subject: "daemon status"
        )

        XCTAssertEqual(message, "daemon status: the CLI's output was not the expected JSON.")
        XCTAssertFalse(message.contains("\n"))
        XCTAssertFalse(message.contains("typeMismatch"))
        XCTAssertFalse(message.contains("codingPath"))
    }

    func testExecutionFailureSurfacesTheCliStderrVerbatim() {
        let message = SectionFetchErrors.shortMessage(
            GlomerisClientError.executionFailed("glomeris daemon status: HOME is not set\n"),
            subject: "daemon status"
        )
        XCTAssertEqual(message, "daemon status: glomeris daemon status: HOME is not set")
    }

    /// A spawn failure with nothing on stderr must still say something —
    /// never render as a bare "daemon status: " with an empty tail.
    func testEmptyStderrStillProducesAMeaningfulSentence() {
        let message = SectionFetchErrors.shortMessage(
            GlomerisClientError.executionFailed("   \n"),
            subject: "daemon status"
        )
        XCTAssertEqual(message, "daemon status: the CLI could not be run.")
    }

    /// A non-`GlomerisClientError` (anything thrown from outside the
    /// client's own typed set) must not fall through to an empty string.
    func testUnknownErrorTypeStillProducesAMessage() {
        struct Weird: Error {}
        let message = SectionFetchErrors.shortMessage(Weird(), subject: "status")
        XCTAssertTrue(message.hasPrefix("status: failed —"))
        XCTAssertGreaterThan(message.count, "status: failed —".count)
    }
}
