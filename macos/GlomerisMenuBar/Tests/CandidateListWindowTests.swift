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
            executable: id % 3 == 0,
            offeredActions: [],
            refusalReason: id % 3 == 0 ? nil : "refusal-\(id)"
        )
    }

    private func scan(_ n: Int) -> [DetectCandidateReportDto] {
        let labels = ["PROTECTED", "ASK", "UNKNOWN_INCOMPLETE", "AUTO_SAFE"]
        return (0..<n).map { candidate($0, label: labels[$0 % labels.count]) }
    }

    func testSmallListIsShownWhole() {
        let items = scan(4)
        let w = CandidateListWindow.window(items, limit: CandidateListWindow.previewLimit)
        XCTAssertEqual(w.shown, items)
        XCTAssertEqual(w.hiddenCount, 0)
    }

    func testLargeListIsBoundedAndKeepsOrder() {
        let items = scan(219)
        let w = CandidateListWindow.window(items, limit: CandidateListWindow.previewLimit)
        XCTAssertEqual(w.shown.count, 5)
        XCTAssertEqual(w.hiddenCount, 214)
        XCTAssertEqual(w.shown, Array(items.prefix(5)), "windowing must keep the CLI's order")
    }

    func testNonPositiveLimitFallsBackToTheInitialLimit() {
        XCTAssertEqual(CandidateListWindow.window(scan(100), limit: 0).shown.count, 5)
    }



    func testSummaryCountsTheCompleteScanBySafetyClass() {
        let text = CandidateListWindow.safetySummary(of: scan(219))
        // 219 = 55 protected + 55 ask + 55 incomplete + 54 safe (labels cycle).
        XCTAssertEqual(
            text,
            "219 found: 54 safe to reclaim, 55 ask first, 55 protected, 55 not enough evidence")
    }

    func testSummaryOmitsEmptyClassesAndHandlesEmptyScan() {
        let onlyProtected = (0..<3).map { candidate($0, label: "PROTECTED") }
        XCTAssertEqual(CandidateListWindow.safetySummary(of: onlyProtected), "3 found: 3 protected")
        XCTAssertEqual(CandidateListWindow.safetySummary(of: []), "0 found")
    }

    func testUnknownLabelsAreCountedAsOtherNotDropped() {
        let mixed = scan(4) + [candidate(99, label: "SOMETHING_NEW")]
        XCTAssertTrue(CandidateListWindow.safetySummary(of: mixed).hasSuffix(", 1 other"))
    }

    func testBoundariesAtTheInitialLimit() {
        XCTAssertEqual(CandidateListWindow.window(scan(5), limit: 5).hiddenCount, 0)
        XCTAssertEqual(CandidateListWindow.window(scan(6), limit: 5).hiddenCount, 1)
        XCTAssertEqual(CandidateListWindow.window(scan(0), limit: 5).shown.count, 0)
    }

    /// The load-bearing call site: the summary must count the whole scan, never
    /// the filtered or windowed slice, and appears only past the initial limit.
    func testViewSummarisesTheCompleteScanOnly() throws {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/CandidatesSectionView.swift")
        let source = try String(contentsOf: url, encoding: .utf8)
        XCTAssertTrue(source.contains("CandidateListWindow.safetySummary(of: scan.candidates)"))
        XCTAssertTrue(source.contains("CandidateListWindow.showsViewAll(candidateCount: scan.candidates.count)"))
        XCTAssertFalse(source.contains("Show more"), "the popover list no longer grows")
    }

    func testViewAllIsOfferedOnlyPastThePreview() {
        for (n, expected) in [(0, false), (1, false), (5, false), (6, true), (219, true), (5_000, true)] {
            XCTAssertEqual(CandidateListWindow.showsViewAll(candidateCount: n), expected, "n=\(n)")
        }
    }

    func testWindowingDoesNotChangeAnyRowsClassificationOrRefusal() {
        let items = scan(100)
        let shown = CandidateListWindow.window(items, limit: 5).shown
        for (a, b) in zip(shown, items) {
            XCTAssertEqual(a.policyLabel, b.policyLabel)
            XCTAssertEqual(a.executable, b.executable)
            XCTAssertEqual(a.refusalReason, b.refusalReason)
        }
        XCTAssertTrue(shown.contains { $0.executable } && shown.contains { !$0.executable })
    }

    /// The new file holds exactly one Button (View all; the sanctioned third
    /// control next to the two pinned in CandidatesSectionView) and no cleanup or
    /// execute path, so the list stays a read-only surface.
    func testViewAllControlIsNotACleanupPath() throws {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/CandidateListWindow.swift")
        let source = try String(contentsOf: url, encoding: .utf8)
            .split(separator: "\n").filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
        XCTAssertEqual(
            (source.components(separatedBy: "Button(").count - 1) + (source.components(separatedBy: "Button {").count - 1), 1)
        for forbidden in ["execute", "\"Clean\"", "executable", ".disabled("] {
            XCTAssertFalse(source.contains(forbidden), "unexpected \(forbidden)")
        }
    }
}
