//
//  CandidateListWindowTests.swift
//  GlomerisMenuBarTests
//
//  HORO-2157: the bounded candidate list is presentation only. These tests pin
//  that windowing never drops, reorders or reclassifies a row, that the safety
//  summary counts the complete scan, and that the new control is not a
//  cleanup path.
//

import XCTest

final class CandidateListWindowTests: XCTestCase {
    private func candidate(_ id: Int, label: String) -> DetectCandidateReportDto {
        DetectCandidateReportDto(
            resourceId: "res-\(id)",
            kind: "docker_volume",
            reclaimableBytes: 1_024,
            reclaimableHuman: "1 KB",
            reclaimableBytesIsLowerBound: false,
            impactTier: nil,
            policyLabel: label,
            reasons: [],
            executable: false,
            offeredActions: [],
            refusalReason: "no registered cleanup action for this resource kind"
        )
    }

    private func scan(_ n: Int) -> [DetectCandidateReportDto] {
        let labels = ["PROTECTED", "ASK", "UNKNOWN_INCOMPLETE", "AUTO_SAFE"]
        return (0..<n).map { candidate($0, label: labels[$0 % labels.count]) }
    }

    func testSmallListIsShownWhole() {
        let items = scan(10)
        let w = CandidateListWindow.window(items, limit: CandidateListWindow.initialLimit)
        XCTAssertEqual(w.shown, items)
        XCTAssertEqual(w.hiddenCount, 0)
    }

    func testLargeListIsBoundedAndKeepsOrder() {
        let items = scan(219)
        let w = CandidateListWindow.window(items, limit: CandidateListWindow.initialLimit)
        XCTAssertEqual(w.shown.count, 25)
        XCTAssertEqual(w.hiddenCount, 194)
        XCTAssertEqual(w.shown, Array(items.prefix(25)), "windowing must keep the CLI's order")
    }

    func testNonPositiveLimitFallsBackToTheInitialLimit() {
        XCTAssertEqual(CandidateListWindow.window(scan(100), limit: 0).shown.count, 25)
    }

    func testShowMoreGrowsByStepAndNeverPastTheTotal() {
        XCTAssertEqual(CandidateListWindow.expanded(from: 25, total: 219), 50)
        XCTAssertEqual(CandidateListWindow.expanded(from: 200, total: 219), 219)
        XCTAssertEqual(CandidateListWindow.expanded(from: 25, total: 30), 30)
    }

    func testRepeatedShowMoreEventuallyShowsEveryRow() {
        let items = scan(219)
        var limit = CandidateListWindow.initialLimit
        for _ in 0..<20 { limit = CandidateListWindow.expanded(from: limit, total: items.count) }
        let w = CandidateListWindow.window(items, limit: limit)
        XCTAssertEqual(w.shown, items)
        XCTAssertEqual(w.hiddenCount, 0)
    }

    func testSummaryCountsTheCompleteScanBySafetyClass() {
        let text = CandidateListWindow.safetySummary(of: scan(219))
        XCTAssertTrue(text.hasPrefix("219 found: "), text)
        XCTAssertTrue(text.contains("protected"), text)
        XCTAssertTrue(text.contains("not enough evidence"), text)
        XCTAssertTrue(text.contains("ask first"), text)
        XCTAssertTrue(text.contains("safe to reclaim"), text)
    }

    func testSummaryOmitsEmptyClassesAndHandlesEmptyScan() {
        let onlyProtected = (0..<3).map { candidate($0, label: "PROTECTED") }
        XCTAssertEqual(CandidateListWindow.safetySummary(of: onlyProtected), "3 found: 3 protected")
        XCTAssertEqual(CandidateListWindow.safetySummary(of: []), "0 found")
    }

    func testWindowingDoesNotChangeAnyRowsClassificationOrRefusal() {
        let items = scan(100)
        let shown = CandidateListWindow.window(items, limit: 25).shown
        for (a, b) in zip(shown, items) {
            XCTAssertEqual(a.policyLabel, b.policyLabel)
            XCTAssertEqual(a.executable, b.executable)
            XCTAssertEqual(a.refusalReason, b.refusalReason)
        }
    }

    /// The new file holds exactly one Button (Show more), and no cleanup,
    /// execute or policy-label branching, so the list stays a read-only surface.
    func testShowMoreControlIsNotACleanupPath() throws {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/CandidateListWindow.swift")
        let source = try String(contentsOf: url, encoding: .utf8)
            .split(separator: "\n").filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
        XCTAssertEqual(source.components(separatedBy: "Button(").count - 1, 1)
        for forbidden in ["execute", "\"Clean\"", "executable", ".disabled("] {
            XCTAssertFalse(source.contains(forbidden), "unexpected \(forbidden)")
        }
    }
}
