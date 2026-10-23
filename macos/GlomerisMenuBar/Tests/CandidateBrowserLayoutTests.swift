//
//  CandidateBrowserLayoutTests.swift
//  GlomerisMenuBarTests
//
//  HORO-2173: behaviour of the rendered views, not source text. The popover
//  card must be the same height for 30, 1,000 and 5,000 candidates, and the
//  candidates window must build over thousands of rows without drawing them all.
//

import AppKit
import SwiftUI
import XCTest

final class CandidateBrowserLayoutTests: XCTestCase {
    private func candidate(_ i: Int) -> DetectCandidateReportDto {
        let labels = ["PROTECTED", "ASK", "UNKNOWN_INCOMPLETE", "AUTO_SAFE"]
        return DetectCandidateReportDto(
            resourceId: "/Users/dev/very/long/path/segment-\(i)/" + String(repeating: "deep/", count: 12) + "target",
            kind: i % 2 == 0 ? "docker_volume" : "cargo_target_dir",
            reclaimableBytes: 1_024,
            reclaimableHuman: "1 KB",
            reclaimableBytesIsLowerBound: false,
            impactTier: nil,
            policyLabel: labels[i % labels.count],
            reasons: [],
            executable: i % 3 == 0,
            offeredActions: [],
            refusalReason: i % 3 == 0 ? nil : "refusal-\(i)"
        )
    }

    @MainActor
    private func scanState(_ n: Int) -> ScanState {
        let state = ScanState()
        state.candidates = (0..<n).map(candidate)
        state.lastScannedAt = Date()
        return state
    }

    @MainActor
    private func cardHeight(_ n: Int) -> CGFloat {
        let host = NSHostingView(
            rootView: CandidatesSectionView(scan: scanState(n)).frame(width: GlomerisDesign.popoverWidth))
        host.layoutSubtreeIfNeeded()
        return host.fittingSize.height
    }

    /// The preview is a fixed five rows, so the card is the same height for any
    /// number of candidates. The one thing that can still vary is the summary
    /// sentence wrapping onto a second line once the counts reach four digits (it
    /// is deliberately not truncated: it carries the Protected and Not-enough-
    /// evidence counts). That is a single caption line, not growth per candidate:
    /// 1,000 and 5,000 are identical, and 30 and 220 are identical.
    @MainActor
    func testPopoverCardHeightDoesNotGrowWithCandidateCount() {
        let thirty = cardHeight(30)
        XCTAssertGreaterThan(thirty, 0)
        XCTAssertEqual(cardHeight(220), thirty, accuracy: 0.5)
        let thousand = cardHeight(1_000)
        XCTAssertEqual(cardHeight(5_000), thousand, accuracy: 0.5)
        XCTAssertEqual(cardHeight(20_000), thousand, accuracy: 20)
        XCTAssertLessThanOrEqual(thousand - thirty, 20, "at most one extra caption line, never per-row growth")
    }

    @MainActor
    func testPopoverCardWithFewerRowsThanThePreviewIsShorter() {
        XCTAssertLessThan(cardHeight(1), cardHeight(30))
        XCTAssertLessThanOrEqual(cardHeight(5), cardHeight(30))
    }

    @MainActor
    func testWindowBuildsOverFiveThousandRowsQuicklyAndBounded() {
        let state = scanState(5_000)
        let start = Date()
        let host = NSHostingView(rootView: CandidateBrowserView(scan: state))
        host.frame = NSRect(x: 0, y: 0, width: 1000, height: 640)
        host.layoutSubtreeIfNeeded()
        // A generous ceiling: this catches eager rendering of every row (which
        // takes many seconds), not ordinary machine-to-machine variation.
        XCTAssertLessThan(Date().timeIntervalSince(start), 10)
    }

    func testViewAllIsSpokenAsAWindowAndPluralisedCorrectly() {
        XCTAssertEqual(CandidateViewAllView.accessibilityLabel(total: 219), "View all 219 candidates in a window")
        XCTAssertEqual(CandidateViewAllView.accessibilityLabel(total: 1), "View all 1 candidate in a window")
    }

    @MainActor
    func testEmptyAndSingleScansHostWithoutTrouble() {
        for n in [0, 1, 5] {
            let host = NSHostingView(rootView: CandidateBrowserView(scan: scanState(n)))
            host.frame = NSRect(x: 0, y: 0, width: 1000, height: 640)
            host.layoutSubtreeIfNeeded()
            XCTAssertGreaterThan(host.fittingSize.width, 0, "n=\(n)")
        }
    }
}
