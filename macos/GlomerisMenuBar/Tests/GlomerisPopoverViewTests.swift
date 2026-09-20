//
//  GlomerisPopoverViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1306. The popover shell has no view model to test — it is layout —
//  so what is asserted here is its source, the same technique
//  HistoryAuditSectionViewTests uses for the one-error-per-list rule.
//
//  These are not style checks. Each one pins a decision that regressed
//  silently once already or that nothing else in the suite would catch:
//  the width that made every row wrap, the unbounded height, the section
//  order that is also the VoiceOver reading order, and the fact that the
//  panel has a way out of itself at all. `GlomerisPopoverView` is not
//  compiled into this target (it drives the real app's Settings scene and
//  calls `NSApplication.terminate`), which is the other reason these read
//  the file rather than the type.
//

import XCTest

final class GlomerisPopoverViewTests: XCTestCase {
    private var source: String = ""

    override func setUpWithError() throws {
        source = try Self.readSource("GlomerisPopoverView.swift")
    }

    /// Comment-stripped, so a token merely *discussed* in the file header
    /// cannot satisfy or fail an assertion about the code.
    private var code: String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    /// The popover was 260pt wide with a `.title3` header, which is narrower
    /// than a safety badge plus a size — so nearly every row in every
    /// section wrapped. Geometry and type now come from the one place that
    /// documents why, and a future hardcoded number here would put the
    /// popover back out of step with the cards inside it.
    func testShellTakesItsGeometryAndTypeFromTheDesignSystem() {
        XCTAssertTrue(code.contains("GlomerisDesign.popoverWidth"))
        XCTAssertTrue(code.contains("GlomerisDesign.titleFont"))
        XCTAssertFalse(code.contains("width: 260"), "the width that made every row wrap")
        XCTAssertFalse(code.contains(".title3"))
        XCTAssertFalse(
            code.contains(".padding()"),
            "an argument-less .padding() opts out of the shared spacing scale"
        )
    }

    /// A menu-bar popover that grows with its content ends up covering the
    /// screen whose disk it is reporting on — and the history card alone can
    /// hold twenty rows.
    func testTheBodyScrollsWithinABoundedHeight() {
        XCTAssertTrue(code.contains("ScrollView"))
        XCTAssertTrue(
            code.contains("maxHeight: GlomerisDesign.maxBodyHeight"),
            "the scrolling body must be bounded, and bounded by maxHeight so a short popover stays short"
        )
    }

    /// Primary state first, history last — and because this is a plain
    /// `VStack`, source order *is* VoiceOver's reading order. Reordering
    /// these would silently change what a screen-reader user hears first.
    func testSectionsAreInReadingOrderStatusThenCandidatesThenHistory() throws {
        let status = try XCTUnwrap(code.range(of: "StatusHealthSectionView()"))
        let candidates = try XCTUnwrap(code.range(of: "CandidatesSectionView()"))
        let history = try XCTUnwrap(code.range(of: "HistoryAuditSectionView()"))

        XCTAssertLessThan(
            status.lowerBound, candidates.lowerBound,
            "what the disk is doing now must come before what could be reclaimed"
        )
        XCTAssertLessThan(
            candidates.lowerBound, history.lowerBound,
            "what could be reclaimed must come before what has already happened"
        )
    }

    /// Every section is a `GlomerisCard` now, so spacing does the grouping.
    /// The only dividers left are the two structural ones marking where the
    /// fixed header and footer stop and the scrolling body begins — a third
    /// would mean a divider had crept back in between two cards that are
    /// already visually separate.
    func testDividersAreStructuralOnly() {
        XCTAssertEqual(
            code.components(separatedBy: "Divider()").count - 1,
            2,
            "cards are separated by spacing; dividers only bracket the scrolling body"
        )
    }

    /// `MenuBarExtra(.window)` draws no menu, so this panel is the app's
    /// only surface. Without these two it is a dead end: no route to the
    /// project roots that decide what `detect` looks at, and no way to quit
    /// short of Activity Monitor.
    func testThePanelIsNotADeadEnd() {
        XCTAssertTrue(
            code.contains("SettingsLink"),
            "macOS 14+ must use the supported route into the Settings scene"
        )
        XCTAssertTrue(
            code.contains("showPreferencesWindow:"),
            "the deployment target is macOS 13, which has no SettingsLink"
        )
        XCTAssertTrue(code.contains("NSApplication.shared.terminate"))
    }

    /// The mark is drawn as a template image so it follows the label colour
    /// in both appearances. Left as artwork it would stay flat black, which
    /// is invisible against a dark popover — and it is decorative here,
    /// beside a "Glomeris" label that already says the same thing, so it
    /// must not be announced twice.
    func testTheHeaderMarkIsTemplateRenderedAndNotAnnouncedTwice() {
        XCTAssertTrue(code.contains(".renderingMode(.template)"))
        XCTAssertTrue(code.contains(".accessibilityHidden(true)"))
    }

    // MARK: - Helpers

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }
}
