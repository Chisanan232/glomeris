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
//  HORO-1306 changed what these rows say, not what they read: both view
//  models now map their raw tokens through `GlomerisVocabulary`, so the
//  assertions below are on the plain-language wording and on the tone, with
//  the raw token still asserted present. The pressure fixtures also use
//  real `PressureState::as_str()` tokens now — they said `OK`, which no
//  Rust variant emits, so the fixture was quietly testing the
//  unrecognised-token path while claiming to be a known-contents fixture.
//  That path is still covered, deliberately, by
//  `testUnrecognisedPressureTokenIsNotPresentedAsFine`.
//

import XCTest

final class HistoryAuditSectionViewTests: XCTestCase {
    // MARK: - HistoryEventRowViewModel

    func testHistoryEventRowRendersTransitionAndMetrics() {
        let dto = HistoryEventReportDto(
            unixTimeSecs: 1_700_000_000,
            from: "HEALTHY",
            to: "WARN",
            usedPercent: 82.5,
            freeBytes: 80_000_000_000,
            freeHuman: "74.5 GB"
        )
        let row = HistoryEventRowViewModel(dto)

        XCTAssertEqual(row.transitionText, "Plenty of room \u{2192} Filling up")
        XCTAssertEqual(row.rawTransitionText, "HEALTHY \u{2192} WARN")
        XCTAssertEqual(row.usedPercentText, "82.5% used")
        XCTAssertEqual(row.freeText, "74.5 GB free")
    }

    /// AC: at least two pressure transitions render as two independently
    /// distinguishable rows.
    func testAtLeastTwoTransitionsRenderAsDistinctRows() {
        let rows = Self.knownHistoryFixture().map(HistoryEventRowViewModel.init)

        XCTAssertEqual(rows.count, 2)
        XCTAssertNotEqual(rows[0].id, rows[1].id)
        XCTAssertEqual(rows[0].rawTransitionText, "HEALTHY \u{2192} WARN")
        XCTAssertEqual(rows[1].rawTransitionText, "WARN \u{2192} CRITICAL")
        XCTAssertNotEqual(rows[0].transitionText, rows[1].transitionText)
    }

    /// The history card and the status card read the same field from the same
    /// Rust enum, so they must not describe the same state in two different
    /// ways — that is exactly the drift HORO-1306 exists to remove.
    func testHistoryNamesAPressureStateTheSameWayTheStatusCardDoes() {
        let row = HistoryEventRowViewModel(
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_000,
                from: "PRESSURED",
                to: "CRITICAL",
                usedPercent: 97.0,
                freeBytes: 1_000_000_000,
                freeHuman: "953 MB"
            )
        )

        XCTAssertEqual(row.fromTerm, GlomerisVocabulary.pressure("PRESSURED"))
        XCTAssertEqual(row.toTerm, GlomerisVocabulary.pressure("CRITICAL"))
    }

    /// A token from a newer CLI must not be dressed up as a state this app
    /// understands, and must never land on a reassuring tone. The raw token
    /// stays visible so it can be reported.
    func testUnrecognisedPressureTokenIsNotPresentedAsFine() {
        let row = HistoryEventRowViewModel(
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_000,
                from: "OK",
                to: "WARN",
                usedPercent: 50.0,
                freeBytes: 1,
                freeHuman: "1 byte"
            )
        )

        XCTAssertEqual(row.fromTerm.tone, .unknown)
        XCTAssertNotEqual(row.fromTerm.tone, .positive)
        XCTAssertEqual(row.fromTerm.token, "OK")
        XCTAssertTrue(row.rawTransitionText.contains("OK"))
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

        XCTAssertEqual(row.safetyTerm.token, "AUTO_SAFE")
        XCTAssertEqual(row.outcomeTerm.token, "succeeded")
        XCTAssertEqual(row.outcomeTerm.tone, .positive)
        XCTAssertEqual(row.reclaimedText, "reclaimed 2.0 GB")
        XCTAssertEqual(row.sourceTerm.token, "execute")
        XCTAssertEqual(row.actionId, "cargo.clean.target_dir")
        XCTAssertNil(row.detailText)
    }

    func testAbortedRecordRendersAbortReasonAndSource() {
        let row = ActionHistoryRowViewModel(abortedDto())

        XCTAssertEqual(row.safetyTerm.token, "ASK")
        XCTAssertEqual(row.outcomeTerm.token, "aborted_by_revalidation")
        XCTAssertEqual(row.detailText, "ResourceIdentityChanged")
        XCTAssertEqual(row.sourceTerm.token, "free")
        XCTAssertNil(row.reclaimedText)
    }

    /// Core AC: an AUTO_SAFE/succeeded record and a refused/aborted record
    /// must be visually distinguishable — asserted as the two rows differing
    /// in outcome wording, symbol AND tone, never as a difference derived
    /// from `policyLabel`'s text.
    func testAutoSafeAndAbortedRecordsAreVisuallyDistinguishable() {
        let succeeded = ActionHistoryRowViewModel(autoSafeSucceededDto())
        let aborted = ActionHistoryRowViewModel(abortedDto())

        XCTAssertNotEqual(succeeded.outcomeTerm.title, aborted.outcomeTerm.title)
        XCTAssertNotEqual(succeeded.outcomeTerm.symbolName, aborted.outcomeTerm.symbolName)
        XCTAssertNotEqual(succeeded.outcomeTerm.tone, aborted.outcomeTerm.tone)
    }

    /// HORO-1306's semantic correction to this card. A revalidation abort is
    /// the safety machinery working: the resource changed between checking
    /// and acting, so nothing was touched. The old rendering painted it the
    /// same red as a genuine failure, which teaches a user that Glomeris
    /// breaks every time it protects them.
    func testARevalidationAbortIsNotPaintedAsAFailure() {
        let aborted = ActionHistoryRowViewModel(abortedDto())
        let failed = ActionHistoryRowViewModel(
            ActionHistoryEventReportDto(
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
        )

        XCTAssertEqual(failed.outcomeTerm.tone, .critical)
        XCTAssertNotEqual(aborted.outcomeTerm.tone, .critical)
        XCTAssertNotEqual(aborted.outcomeTerm.tone, failed.outcomeTerm.tone)
        // …and it is not reassurance either. Nothing was cleaned.
        XCTAssertNotEqual(aborted.outcomeTerm.tone, .positive)
    }

    /// The row's tone comes from `outcome`. Two records with the same
    /// outcome and different policy labels must tone identically — if the
    /// policy label leaked into the presentation, this is where it shows.
    func testTheRowsToneComesFromTheOutcomeNotThePolicyLabel() {
        let base = autoSafeSucceededDto()
        let autoSafe = ActionHistoryRowViewModel(base)
        let protected = ActionHistoryRowViewModel(
            ActionHistoryEventReportDto(
                timestamp: base.timestamp,
                actionId: base.actionId,
                resourceId: base.resourceId,
                policyLabel: "PROTECTED",
                outcome: base.outcome,
                abortReason: base.abortReason,
                actualReclaimedBytes: base.actualReclaimedBytes,
                actualReclaimedHuman: base.actualReclaimedHuman,
                source: base.source
            )
        )

        XCTAssertEqual(autoSafe.outcomeTerm, protected.outcomeTerm)
        XCTAssertNotEqual(autoSafe.safetyTerm, protected.safetyTerm)
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

        XCTAssertEqual(row.outcomeTerm.token, "failed")
        XCTAssertEqual(row.outcomeTerm.tone, .critical)
        XCTAssertEqual(row.sourceTerm.token, "emergency")
        // An emergency-triggered action is worth noticing on sight: it means
        // ordinary recovery was not enough.
        XCTAssertNotEqual(row.sourceTerm.tone, .neutral)
    }

    /// An unrecognized outcome string must never crash decoding/mapping —
    /// same forward-compatibility contract as `describeExecuteOutcome`'s
    /// `default` case in CandidateDetailView.swift — and must not be
    /// presented as a success.
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

        XCTAssertEqual(row.outcomeTerm.tone, .unknown)
        XCTAssertNotEqual(row.outcomeTerm.tone, .positive)
        XCTAssertEqual(row.outcomeTerm.token, "some_future_outcome")
        XCTAssertTrue(row.accessibilityLabel.contains(row.outcomeTerm.title))
    }

    /// The row is one accessibility element, so everything it shows has to be
    /// in one label — including who triggered the action, which is the fact a
    /// user checks when something was cleaned that they did not ask for.
    func testAccessibilityLabelCarriesTheWholeAuditRecord() {
        let row = ActionHistoryRowViewModel(abortedDto())
        let label = row.accessibilityLabel

        XCTAssertTrue(label.contains(row.actionId), label)
        XCTAssertTrue(label.contains(row.outcomeTerm.title), label)
        XCTAssertTrue(label.contains("ResourceIdentityChanged"), label)
        XCTAssertTrue(label.contains(row.sourceTerm.title), label)
        XCTAssertTrue(label.contains(row.safetyTerm.title), label)
        XCTAssertTrue(label.contains(row.resourceId), label)
        XCTAssertTrue(label.contains(GlomerisVocabulary.sourceAxis), label)
    }

    // MARK: - One error per list

    /// Both lists are fetched concurrently and each used to write the same
    /// `lastErrorMessage`, clearing it on success — so a healthy `history`
    /// read erased a failing `actions history` one and the popover reported
    /// an unreadable audit trail as an empty one. Asserted on the source
    /// because the defect is in which state the two fetches share.
    func testEachListOwnsItsOwnErrorMessage() throws {
        let source = try Self.readSource("HistoryAuditSectionView.swift")
        let code = source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        XCTAssertTrue(code.contains("historyErrorMessage"))
        XCTAssertTrue(code.contains("actionHistoryErrorMessage"))
        XCTAssertFalse(
            code.contains("lastErrorMessage"),
            "a single shared error lets one list's success hide the other's failure"
        )
        XCTAssertEqual(
            code.components(separatedBy: "SectionFetchErrors.shortMessage").count - 1,
            2,
            "each fetch must produce its own message"
        )
    }

    // MARK: - AC: known-contents fixture renders correctly end to end

    /// The ticket's literal AC: a fixture with known contents — at least
    /// two pressure transitions, one AUTO_SAFE action record, and one
    /// refused/aborted action record — renders correctly through both
    /// view-model mapping steps together.
    func testKnownFixtureRendersHistoryAndActionAuditCorrectly() {
        let historyRows = Self.knownHistoryFixture().map(HistoryEventRowViewModel.init)
        let actionRows = [autoSafeSucceededDto(), abortedDto()].map(ActionHistoryRowViewModel.init)

        XCTAssertEqual(historyRows.count, 2)
        XCTAssertEqual(actionRows.count, 2)

        // Pressure transitions rendered with correct from/to, in both the
        // plain wording and the CLI's own tokens.
        XCTAssertEqual(historyRows[0].transitionText, "Plenty of room \u{2192} Filling up")
        XCTAssertEqual(historyRows[1].transitionText, "Filling up \u{2192} Critically low")
        XCTAssertEqual(historyRows[0].rawTransitionText, "HEALTHY \u{2192} WARN")
        XCTAssertEqual(historyRows[1].rawTransitionText, "WARN \u{2192} CRITICAL")

        // The AUTO_SAFE record renders its policy label and a success
        // outcome distinct from the aborted record.
        let autoSafeRow = actionRows[0]
        XCTAssertEqual(autoSafeRow.safetyTerm.token, "AUTO_SAFE")
        XCTAssertEqual(autoSafeRow.outcomeTerm.tone, .positive)

        let abortedRow = actionRows[1]
        XCTAssertEqual(abortedRow.outcomeTerm.token, "aborted_by_revalidation")
        XCTAssertNotEqual(abortedRow.outcomeTerm.tone, .positive)
        XCTAssertEqual(abortedRow.detailText, "ResourceIdentityChanged")

        XCTAssertNotEqual(autoSafeRow.outcomeTerm.tone, abortedRow.outcomeTerm.tone)
    }

    // MARK: - Helpers

    private static func knownHistoryFixture() -> [HistoryEventReportDto] {
        [
            HistoryEventReportDto(
                unixTimeSecs: 1_700_000_000,
                from: "HEALTHY",
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
    }

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }
}
