//
//  DiskCapacityViewModelTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1506. The Disk space card's acceptance criteria, stated as assertions
//  on values rather than on a rendered view.
//
//  What the ticket asks for is mechanical: the top card must state current usage
//  as a percentage, that percentage must sit on the same axis as the alert
//  threshold and the recovery goal, the absolute bytes must remain visible, and
//  VoiceOver must hear meaning rather than a naked number. Every one of those is
//  a property of `DiskCapacityViewModel`, which is why the card's three
//  previously-inline values were lifted into it — the old arrangement built the
//  figure, the bytes line and the bar's spoken value in three places, and
//  nothing but a person with a screen reader could observe that they disagreed.
//

import XCTest

final class DiskCapacityViewModelTests: XCTestCase {
    private func statusReport(
        usedPercent: Double,
        freeHuman: String = "26.8 GB",
        totalHuman: String = "460.4 GB",
        pressureState: String = "critical"
    ) -> StatusReportDto {
        StatusReportDto(
            totalBytes: 494_384_795_648,
            freeBytes: 28_776_534_016,
            usedPercent: usedPercent,
            freeHuman: freeHuman,
            totalHuman: totalHuman,
            pressureState: pressureState
        )
    }

    // MARK: - The ticket's core AC

    /// The clarification in one test: the card states current usage as an
    /// explicit percentage, and it says which axis that percentage is on.
    ///
    /// Before this ticket the card had no percentage anywhere a human could read
    /// — a badge, a free/total byte pair, and a bar whose only percentage was
    /// its progress value.
    func testTheCardStatesCurrentUsageAsAnExplicitPercentage() {
        let capacity = DiskCapacityViewModel(statusReport(usedPercent: 94.2))

        XCTAssertEqual(capacity.figureText, "94.2% used")
        XCTAssertTrue(
            capacity.figureText.contains("used"),
            "a bare percentage does not say whether it means used or free"
        )
    }

    /// The bytes stay. They answer a different question from the percentage
    /// ("how much room is left" versus "how full is it"), and replacing one with
    /// the other would trade the mental conversion this ticket removes for the
    /// opposite one.
    func testTheAbsoluteBytesRemainVisibleAsSecondaryContext() {
        let capacity = DiskCapacityViewModel(
            statusReport(usedPercent: 94.2, freeHuman: "26.8 GB", totalHuman: "460.4 GB")
        )

        XCTAssertEqual(capacity.bytesText, "26.8 GB free of 460.4 GB")
        XCTAssertNotEqual(
            capacity.bytesText,
            capacity.figureText,
            "the two lines are two facts; neither may stand in for the other"
        )
    }

    /// The CLI's own byte strings, verbatim. This target does not re-render a
    /// size it was handed a rendering for — that is HORO-1312's rule, and the
    /// reason `GlomerisByteFormat` exists only for figures the CLI cannot render.
    func testTheByteStringsAreTheClisOwnAndAreNotReformatted() {
        let capacity = DiskCapacityViewModel(
            statusReport(usedPercent: 50, freeHuman: "1023 B", totalHuman: "16384.0 PB")
        )

        XCTAssertEqual(capacity.bytesText, "1023 B free of 16384.0 PB")
    }

    // MARK: - One axis, one rule

    /// The figure, the spoken figure and the bar's spoken value are all the same
    /// percentage. This is the property the old card violated: it spoke
    /// `Int(fraction * 100)` from the bar while the Recovery card rendered the
    /// same field to one decimal place, so one measurement was announced two
    /// ways.
    func testTheFigureTheSpokenFigureAndTheBarAllStateTheSamePercentage() {
        for usedPercent in [0.0, 26.8, 72.046, 89.96, 94.2, 100.0] {
            let capacity = DiskCapacityViewModel(statusReport(usedPercent: usedPercent))
            let canonical = GlomerisUsedPercent.text(usedPercent)
            let digits = canonical.replacingOccurrences(of: "% used", with: "")

            XCTAssertEqual(capacity.figureText, canonical)
            XCTAssertEqual(capacity.spokenFigure, "\(digits) percent used")
            XCTAssertTrue(
                capacity.spokenBarValue.hasPrefix("\(digits) percent used"),
                "the bar announced \(capacity.spokenBarValue) beside a card reading \(canonical)"
            )
        }
    }

    /// The rule is the shared one, so the card inherits the threshold invariant
    /// rather than restating it: a figure on this card that reads as at-or-past
    /// the user's alert threshold means the CLI's comparison had crossed it.
    ///
    /// 89.96 against a threshold of 90 is the reading the ticket cites. The card
    /// must show 89.9, because the CLI decided `89.96 >= 90.0` was false and did
    /// not notify.
    func testTheCardCannotClaimAThresholdTheCliDidNotCross() {
        let capacity = DiskCapacityViewModel(statusReport(usedPercent: 89.96))

        XCTAssertEqual(capacity.figureText, "89.9% used")
        XCTAssertFalse(
            capacity.figureText.hasPrefix("90"),
            "the card would state the alert threshold while the daemon stayed silent"
        )
    }

    // MARK: - Accessibility

    /// VoiceOver must hear meaning, not a naked number. The bar's old value was
    /// "72 percent" — no axis, and a precision of its own.
    func testVoiceOverHearsTheAxisAndTheAbsoluteContext() {
        let capacity = DiskCapacityViewModel(statusReport(usedPercent: 94.2))

        XCTAssertEqual(capacity.spokenFigure, "94.2 percent used")
        XCTAssertEqual(capacity.spokenBarValue, "94.2 percent used. 26.8 GB free of 460.4 GB.")
        XCTAssertFalse(
            capacity.spokenBarValue.contains("%"),
            "a spoken string is read, not parsed"
        )
    }

    /// The bar is one accessibility element, so a listener who steps onto it
    /// alone still gets both facts — `SpokenLabel` terminates each clause, as it
    /// does for every composed label in this target (HORO-1451).
    func testTheBarsSpokenValueIsOneTerminatedSentencePerFact() {
        let capacity = DiskCapacityViewModel(statusReport(usedPercent: 72.0))
        let sentences = capacity.spokenBarValue
            .components(separatedBy: ". ")
            .filter { !$0.isEmpty }

        XCTAssertEqual(sentences.count, 2, capacity.spokenBarValue)
        XCTAssertTrue(capacity.spokenBarValue.hasSuffix("."), capacity.spokenBarValue)
    }

    // MARK: - The bar's fill

    func testTheBarFillTracksTheMeasurementAndStaysInRange() {
        for (usedPercent, expected) in [(0.0, 0.0), (50.0, 0.5), (94.2, 0.942), (100.0, 1.0)] {
            let capacity = DiskCapacityViewModel(statusReport(usedPercent: usedPercent))
            XCTAssertEqual(capacity.barFraction, expected, accuracy: 1e-12)
        }
    }

    /// A reading outside 0...100 cannot come from a filesystem observation —
    /// `FsUsage::used_percent` saturates — but it arrives here as JSON. The bar
    /// clamps rather than overflowing its track.
    func testAnOutOfRangeReadingClampsTheBarRatherThanOverflowingIt() {
        XCTAssertEqual(DiskCapacityViewModel(statusReport(usedPercent: -5)).barFraction, 0)
        XCTAssertEqual(DiskCapacityViewModel(statusReport(usedPercent: 140)).barFraction, 1)
        XCTAssertEqual(DiskCapacityViewModel(statusReport(usedPercent: -5)).figureText, "0.0% used")
        XCTAssertEqual(
            DiskCapacityViewModel(statusReport(usedPercent: 140)).figureText,
            "100.0% used"
        )
    }

    /// `min`/`max` do not filter `NaN` — every comparison against it is false,
    /// so it passes straight through a clamp — and `ProgressView(value: .nan)`
    /// is not a rendering to discover at the founder's desk. An empty bar is
    /// also the honest shape, because the figure beside it says the usage is
    /// unavailable rather than naming a number.
    func testANonFiniteReadingGivesAnEmptyBarAndAnHonestFigure() {
        for bogus in [Double.nan, .infinity, -.infinity] {
            let capacity = DiskCapacityViewModel(statusReport(usedPercent: bogus))
            XCTAssertEqual(capacity.barFraction, 0, "\(bogus) reached the bar")
            XCTAssertTrue(capacity.barFraction.isFinite, "\(bogus) reached the bar")
            XCTAssertEqual(capacity.figureText, GlomerisUsedPercent.unavailableText)
            XCTAssertEqual(capacity.spokenFigure, GlomerisUsedPercent.unavailableText)
            XCTAssertFalse(
                capacity.figureText.contains("0.0"),
                "an unmeasured disk must not read as an empty one"
            )
        }
    }

    // MARK: - The card renders from the view model, not from its own arithmetic

    /// The view model is only worth having if the card actually uses it. These
    /// are the two specific regressions the extraction exists to prevent: a
    /// local `%.1f` percentage, and the bar's old whole-number spoken value.
    ///
    /// Asserted against the source because the card's body is a SwiftUI
    /// `@ViewBuilder` with nothing to read back — the same reason
    /// `GlomerisByteFormatTests` reads its own implementation to keep
    /// `ByteCountFormatter` out.
    func testTheCardDoesNotRenderAPercentageOfItsOwn() throws {
        let source = try String(
            contentsOf: URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()
                .deletingLastPathComponent()
                .appendingPathComponent("Sources/StatusHealthSectionView.swift"),
            encoding: .utf8
        )
        let code = source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("///") }
            .joined(separator: "\n")

        XCTAssertFalse(
            code.contains("%.1f"),
            "the card must render its percentage through GlomerisUsedPercent, not a local format"
        )
        XCTAssertFalse(
            code.contains("Int(fraction"),
            "the bar's spoken value must not truncate the percentage a second time"
        )
        XCTAssertTrue(
            code.contains("DiskCapacityViewModel(statusReport)"),
            "the disk card no longer renders from the view model these tests assert on"
        )
    }
}
