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
}
