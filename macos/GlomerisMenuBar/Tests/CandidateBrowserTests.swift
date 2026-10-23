//
//  CandidateBrowserTests.swift
//  GlomerisMenuBarTests
//
//  HORO-2173: the rules behind the candidates window, over the result-set sizes
//  the founder asked for, including a result set far past the 220 observed.
//

import XCTest

final class CandidateBrowserTests: XCTestCase {
    private func candidate(_ i: Int, label: String, id: String? = nil, kind: String = "docker_volume")
        -> DetectCandidateReportDto
    {
        DetectCandidateReportDto(
            resourceId: id ?? "res-\(i)",
            kind: kind,
            reclaimableBytes: 1_024,
            reclaimableHuman: "1 KB",
            reclaimableBytesIsLowerBound: false,
            impactTier: nil,
            policyLabel: label,
            reasons: [],
            executable: false,
            offeredActions: [],
            refusalReason: "refusal-\(i)"
        )
    }

    private func scan(_ n: Int) -> [DetectCandidateReportDto] {
        let labels = ["PROTECTED", "ASK", "UNKNOWN_INCOMPLETE", "AUTO_SAFE"]
        return (0..<n).map { candidate($0, label: labels[$0 % labels.count]) }
    }

    func testRowsKeepEveryCandidateAtEveryScale() {
        for n in [0, 1, 5, 25, 100, 220, 1_000, 5_000] {
            let all = scan(n)
            let rows = CandidateBrowserRows.rows(all, filter: .all, order: .recommended, search: "")
            XCTAssertEqual(rows, all, "n=\(n): the window must show the complete scan, unchanged")
        }
    }

    func testFilterKeepsOnlyTheChosenClassAndNeverInventsRows() {
        let all = scan(220)
        let protected = CandidateBrowserRows.rows(all, filter: .protected, order: .recommended, search: "")
        XCTAssertEqual(protected.count, 55)
        XCTAssertTrue(protected.allSatisfy { $0.policyLabel == "PROTECTED" })
    }

    func testSearchMatchesPathAndKindCaseInsensitivelyAndTrims() {
        let items = [
            candidate(0, label: "ASK", id: "/Users/dev/Projects/Alpha/target", kind: "cargo_target_dir"),
            candidate(1, label: "ASK", id: "docker://volume/abc", kind: "docker_volume"),
        ]
        func ids(_ q: String) -> [String] {
            CandidateBrowserRows.rows(items, filter: .all, order: .recommended, search: q).map(\.resourceId)
        }
        XCTAssertEqual(ids("alpha"), ["/Users/dev/Projects/Alpha/target"])
        XCTAssertEqual(ids("  DOCKER_VOLUME "), ["docker://volume/abc"])
        XCTAssertEqual(ids(""), items.map(\.resourceId))
        XCTAssertEqual(ids("   "), items.map(\.resourceId))
        XCTAssertEqual(ids("nope"), [])
    }

    func testSearchHidesRowsFromTheListNotFromTheScan() {
        let all = scan(50)
        let rows = CandidateBrowserRows.rows(all, filter: .all, order: .recommended, search: "res-4")
        XCTAssertLessThan(rows.count, all.count)
        XCTAssertEqual(all.count, 50)
        XCTAssertTrue(CandidateListWindow.safetySummary(of: all).hasPrefix("50 found"))
    }

    func testLongIdentifiersSurviveShapingUnchanged() {
        let long = "/Users/dev/" + String(repeating: "very-long-segment/", count: 40) + "target"
        let items = [candidate(0, label: "ASK", id: long)]
        XCTAssertEqual(
            CandidateBrowserRows.rows(items, filter: .all, order: .path, search: "very-long").first?.resourceId, long)
    }

    // MARK: - Selection

    func testNoSelection() {
        XCTAssertEqual(CandidateSelection.reconcile(selected: nil, shownIds: ["a"]), .none)
    }

    func testSelectionKeptWhenStillShown() {
        XCTAssertEqual(CandidateSelection.reconcile(selected: "b", shownIds: ["a", "b"]), .selected("b"))
    }

    func testVanishedSelectionIsNeverReplacedByANeighbour() {
        XCTAssertEqual(CandidateSelection.reconcile(selected: "b", shownIds: ["a", "c"]), .unavailable("b"))
        XCTAssertEqual(CandidateSelection.reconcile(selected: "b", shownIds: []), .unavailable("b"))
    }

    func testDuplicateIdsDoNotBreakReconcile() {
        XCTAssertEqual(CandidateSelection.reconcile(selected: "a", shownIds: ["a", "a"]), .selected("a"))
    }

    func testSelectionHiddenByAFilterIsUnavailableNotSwitched() {
        let all = scan(8)
        let selected = all[0].resourceId  // PROTECTED
        let shown = CandidateBrowserRows.rows(all, filter: .ask, order: .recommended, search: "").map(\.resourceId)
        XCTAssertEqual(CandidateSelection.reconcile(selected: selected, shownIds: shown), .unavailable(selected))
    }

    // MARK: - Performance at 5,000 rows

    func testShapingSearchAndReconcileStayFastAtFiveThousandRows() {
        let all = scan(5_000)
        measure {
            let rows = CandidateBrowserRows.rows(all, filter: .all, order: .path, search: "res-4")
            let ids = rows.map(\.resourceId)
            _ = CandidateSelection.reconcile(selected: "res-4999", shownIds: ids)
            _ = CandidateListWindow.safetySummary(of: all)
        }
    }
}
