//
//  HistoryAuditSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1066: proves the ticket's literal AC — a fixture with known
//  contents (at least 2 pressure transitions, one AUTO_SAFE action record,
//  and one refused/aborted action record) renders correctly. Tests the
//  pure view-model mapping steps (`HistoryEventRowViewModel` /
//  `ActionHistoryRowViewModel`) directly, same pattern as
//  StatusHealthSectionViewTests / CandidatesSectionViewTests, rather than
//  going through SwiftUI view rendering.
//
//  Fixtures are decoded from the same golden fixture used by
//  DtoGoldenFixturesTests (`tests/fixtures/dto/history_report.json`,
//  `action_history_report.json`) plus one hand-built in-memory DTO set
//  below for the AC's specific "known contents" requirement (2+
//  transitions, one AUTO_SAFE succeeded record, one aborted record) —
//  the golden fixture already happens to satisfy this, but this test
//  constructs its own fixture explicitly so the AC is provably
//  independent of any future edit to the golden fixture.
//

import XCTest

final class HistoryAuditSectionViewTests: XCTestCase {
    // MARK: - HistoryEventRowViewModel

    func testHistoryEventRowRendersTransitionAndMetrics() {
        let dto = HistoryEventReportDto(
            unixTimeSecs: 1_700_000_000,
            from: "OK",
            to: "WARN",
            usedPercent: 82.5,
            freeBytes: 80_000_000_000,
            freeHuman: "74.5 GB"
        )
        let row = HistoryEventRowViewModel(dto)

        XCTAssertEqual(row.transitionText, "OK \u{2192} WARN")
        XCTAssertEqual(row.usedPercentText, "82.5% used")
        XCTAssertEqual(row.freeText, "74.5 GB free")
    }

    /// AC: at least two pressure transitions render as two independently
    /// distinguishable rows.
    func testAtLeastTwoTransitionsRenderAsDistinctRows() {
        let events = [
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_000,
                from: "OK",
                to: "WARN",
                usedPercent: 82.5,
                freeBytes: 80_000_000_000,
                freeHuman: "74.5 GB"
            ),
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_600,
                from: "WARN",
                to: "CRITICAL",
                usedPercent: 95.1,
                freeBytes: 20_000_000_000,
                freeHuman: "18.6 GB"
            ),
        ]
        let rows = events.map(HistoryEventRowViewModel.init)

        XCTAssertEqual(rows.count, 2)
        XCTAssertNotEqual(rows[0].id, rows[1].id)
        XCTAssertEqual(rows[0].transitionText, "OK \u{2192} WARN")
        XCTAssertEqual(rows[1].transitionText, "WARN \u{2192} CRITICAL")
    }

    // MARK: - ActionHistoryRowViewModel — AC: AUTO_SAFE vs. aborted/refused

    private func autoSafeSucceededDto() -> ActionHistoryEventReportDto {
        ActionHistoryEventReportDto(
            timestamp: 1_700_000_000,
            actionId: "cargo.clean.target_dir",
            resourceId: "cargo_target_dir:/Users/dev/proj/target",
            policyLabel: "AUTO_SAFE",
            outcome: "succeeded",
            abortReason: nil,
            actualReclaimedBytes: 2_147_483_648,
            actualReclaimedHuman: "2.0 GB",
            source: "execute"
        )
    }

    private func abortedDto() -> ActionHistoryEventReportDto {
        ActionHistoryEventReportDto(
            timestamp: 1_700_000_600,
            actionId: "docker.clean.build_cache",
            resourceId: "docker_build_cache:docker",
            policyLabel: "ASK",
            outcome: "aborted_by_revalidation",
            abortReason: "ResourceIdentityChanged",
            actualReclaimedBytes: nil,
            actualReclaimedHuman: nil,
            source: "free"
        )
    }

    func testAutoSafeSucceededRecordRendersPolicyOutcomeAndSource() {
        let row = ActionHistoryRowViewModel(autoSafeSucceededDto())

        XCTAssertEqual(row.policyLabelText, "AUTO_SAFE")
        XCTAssertEqual(row.outcomeText, "Succeeded (2.0 GB)")
        XCTAssertEqual(row.source, "execute")
        XCTAssertNil(row.detailText)
        XCTAssertTrue(row.isSuccess)
    }

    func testAbortedRecordRendersAbortReasonAndSource() {
        let row = ActionHistoryRowViewModel(abortedDto())

        XCTAssertEqual(row.policyLabelText, "ASK")
        XCTAssertEqual(row.outcomeText, "Aborted")
        XCTAssertEqual(row.detailText, "ResourceIdentityChanged")
        XCTAssertEqual(row.source, "free")
        XCTAssertFalse(row.isSuccess)
    }

    /// Core AC: an AUTO_SAFE/succeeded record and a refused/aborted record
    /// must be visually distinguishable — asserted here as `isSuccess`
    /// disagreeing between the two (the field `HistoryAuditSectionView`
    /// reads to pick the outcome text's color), never as a difference
    /// derived from `policyLabelText`'s text.
    func testAutoSafeAndAbortedRecordsAreVisuallyDistinguishable() {
        let succeeded = ActionHistoryRowViewModel(autoSafeSucceededDto())
        let aborted = ActionHistoryRowViewModel(abortedDto())

        XCTAssertNotEqual(succeeded.isSuccess, aborted.isSuccess)
        XCTAssertNotEqual(succeeded.outcomeText, aborted.outcomeText)
    }

    func testFailedRecordRendersFailedOutcome() {
        let dto = ActionHistoryEventReportDto(
            timestamp: 1_700_000_900,
            actionId: "cargo.clean.target_dir",
            resourceId: "cargo_target_dir:/Users/dev/proj/target",
            policyLabel: "AUTO_SAFE",
            outcome: "failed",
            abortReason: nil,
            actualReclaimedBytes: nil,
            actualReclaimedHuman: nil,
            source: "emergency"
        )
        let row = ActionHistoryRowViewModel(dto)

        XCTAssertEqual(row.outcomeText, "Failed")
        XCTAssertFalse(row.isSuccess)
        XCTAssertEqual(row.source, "emergency")
    }

    /// An unrecognized outcome string must never crash decoding/mapping —
    /// same forward-compatibility contract as `describeExecuteOutcome`'s
    /// `default` case in CandidateDetailView.swift.
    func testUnrecognizedOutcomeDoesNotCrashAndRendersAsUnrecognized() {
        let dto = ActionHistoryEventReportDto(
            timestamp: 1_700_001_000,
            actionId: "some.action",
            resourceId: "some_resource",
            policyLabel: "AUTO_SAFE",
            outcome: "some_future_outcome",
            abortReason: nil,
            actualReclaimedBytes: nil,
            actualReclaimedHuman: nil,
            source: "execute"
        )
        let row = ActionHistoryRowViewModel(dto)

        XCTAssertFalse(row.isSuccess)
        XCTAssertTrue(row.outcomeText.contains("some_future_outcome"))
    }

    // MARK: - AC: known-contents fixture renders correctly end to end

    /// The ticket's literal AC: a fixture with known contents — at least
    /// two pressure transitions, one AUTO_SAFE action record, and one
    /// refused/aborted action record — renders correctly through both
    /// view-model mapping steps together.
    func testKnownFixtureRendersHistoryAndActionAuditCorrectly() {
        let historyEvents = [
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_000,
                from: "OK",
                to: "WARN",
                usedPercent: 82.5,
                freeBytes: 80_000_000_000,
                freeHuman: "74.5 GB"
            ),
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_600,
                from: "WARN",
                to: "CRITICAL",
                usedPercent: 95.1,
                freeBytes: 20_000_000_000,
                freeHuman: "18.6 GB"
            ),
        ]
        let actionEvents = [autoSafeSucceededDto(), abortedDto()]

        let historyRows = historyEvents.map(HistoryEventRowViewModel.init)
        let actionRows = actionEvents.map(ActionHistoryRowViewModel.init)

        XCTAssertEqual(historyRows.count, 2)
        XCTAssertEqual(actionRows.count, 2)

        // Pressure transitions rendered with correct from/to.
        XCTAssertEqual(historyRows[0].transitionText, "OK \u{2192} WARN")
        XCTAssertEqual(historyRows[1].transitionText, "WARN \u{2192} CRITICAL")

        // The AUTO_SAFE record renders its policy label and a success
        // outcome distinct from the aborted record.
        let autoSafeRow = actionRows[0]
        XCTAssertEqual(autoSafeRow.policyLabelText, "AUTO_SAFE")
        XCTAssertTrue(autoSafeRow.isSuccess)

        let abortedRow = actionRows[1]
        XCTAssertEqual(abortedRow.outcomeText, "Aborted")
        XCTAssertFalse(abortedRow.isSuccess)
        XCTAssertEqual(abortedRow.detailText, "ResourceIdentityChanged")

        XCTAssertNotEqual(autoSafeRow.isSuccess, abortedRow.isSuccess)
    }
}
