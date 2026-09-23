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
//  panel has a way out of itself at all. These read the file rather than the
//  type because layout is what they are about; HORO-1365 did add the shell to
//  this target, so it can now also be constructed — see
//  `OverviewStateTests.buildOverview`.
//
//  HORO-1357 adds the presentation-shape invariants. The detail view was
//  reached by `.sheet(item:)` from inside whichever card held the tapped
//  row, and over a non-activating `MenuBarExtra(.window)` panel that never
//  reliably appeared. Nothing in a unit-testable layer could have caught
//  that — it is a fact about which modifier is applied to which view — so
//  it is pinned here, across all three files on the route, in both
//  directions: no sheet anywhere, and the in-panel replacement actually
//  wired up. The `CandidateDetailNavigation` state machine those
//  assertions reference is tested as a type in CandidateDetailViewTests.
//

import XCTest

final class GlomerisPopoverViewTests: XCTestCase {
    private var source: String = ""

    override func setUpWithError() throws {
        source = try Self.readSource("GlomerisPopoverView.swift")
    }

    /// Comment-stripped, so a token merely *discussed* in the file header
    /// cannot satisfy or fail an assertion about the code. This matters more
    /// than usual for the HORO-1357 assertions below, whose subject —
    /// `.sheet` — is named repeatedly in three file headers explaining why it
    /// is gone.
    private var code: String { Self.strippedOfComments(source) }

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
    ///
    /// The candidates card is matched on `CandidatesSectionView(` rather than
    /// `CandidatesSectionView()` since HORO-1357: it now takes arguments. That
    /// is a deliberate loosening of one character, not of the invariant — the
    /// order this test exists to pin is asserted exactly as before.
    func testSectionsAreInReadingOrderStatusThenCandidatesThenHistory() throws {
        let status = try XCTUnwrap(code.range(of: "StatusHealthSectionView()"))
        let candidates = try XCTUnwrap(code.range(of: "CandidatesSectionView("))
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

    // MARK: - HORO-1357: the detail view is not presented over the panel

    /// The defect, stated mechanically. `MenuBarExtra(.window)` is a
    /// non-activating panel; presenting or resizing a sheet over one can order
    /// the panel out, so the detail surface never becomes visible. Every file
    /// on the route to `CandidateDetailView` — the shell, and both cards that
    /// lead to it — must therefore contain no sheet presentation at all.
    ///
    /// Comment-stripped on purpose: all three headers now *discuss* `.sheet`
    /// at length, so an un-stripped grep here would fail while the code is
    /// correct.
    func testNoFileOnTheRouteToTheDetailViewPresentsASheet() throws {
        for fileName in [
            "GlomerisPopoverView.swift",
            "CandidatesSectionView.swift",
            "AiPlanSectionView.swift",
        ] {
            let fileCode = try Self.strippedOfComments(Self.readSource(fileName))
            XCTAssertFalse(
                fileCode.contains(".sheet("),
                "\(fileName) must not present a sheet over a MenuBarExtra(.window) panel — HORO-1357"
            )
        }
    }

    /// Non-vacuity for the assertion above: the token it looks for is one this
    /// helper really does find in code and really does ignore in comments. A
    /// stripping bug would otherwise make the three checks pass over nothing.
    func testTheSheetAssertionWouldActuallyCatchASheet() {
        XCTAssertTrue(Self.strippedOfComments("        .sheet(item: $x) { _ in }").contains(".sheet("))
        XCTAssertFalse(Self.strippedOfComments("        // .sheet(item: $x) is gone").contains(".sheet("))
    }

    /// Where it goes instead: the detail view is a child of this panel, shown
    /// in place of the scrolling body, and it is the shell's own navigation
    /// state that decides which of the two is on screen.
    func testTheDetailViewReplacesTheBodyInsideTheSamePanel() {
        XCTAssertTrue(
            code.contains("CandidateDetailView("),
            "the shell must host the detail view itself, not hand it to a presentation modifier"
        )
        XCTAssertTrue(
            code.contains("navigation.resourceId"),
            "which surface is shown must come from CandidateDetailNavigation"
        )
        XCTAssertTrue(
            code.contains(".id(resourceId)"),
            "tapping a second candidate without going back must rebuild the view, so its explain call re-runs"
        )
    }

    /// Both entry points must report the tap upward rather than presenting
    /// anything themselves — otherwise one of them keeps the defect, which is
    /// how it survived being found once already.
    func testBothCardsReportARowTapUpwardToTheShell() throws {
        for fileName in ["CandidatesSectionView.swift", "AiPlanSectionView.swift"] {
            let fileCode = try Self.strippedOfComments(Self.readSource(fileName))
            XCTAssertTrue(
                fileCode.contains("onOpenDetail("),
                "\(fileName) must report the tapped resource id upward"
            )
            XCTAssertFalse(
                fileCode.contains("CandidateDetailView("),
                "\(fileName) must not construct the detail view — the shell owns it"
            )
        }
        XCTAssertTrue(
            code.contains("navigation.open("),
            "the shell must be what turns a reported tap into navigation state"
        )
    }

    /// The first in-panel drill-down swapped `sections` out for the detail, and
    /// that quietly undid the fix it was part of: the candidates card holds a
    /// completed scan in its own `@State`, so taking it out of the hierarchy
    /// discarded it, and pressing back landed on "No scan yet" with the scan to
    /// run again. The sheet this replaced did not do that — a sheet leaves the
    /// view underneath it in place — so the overview has to stay in the
    /// hierarchy, hidden, rather than be removed from it.
    ///
    /// Asserted on the three modifiers together, because each one covers a
    /// different way a merely-invisible view still misbehaves: it must not be
    /// visible, it must not take the scroll events meant for the detail, and it
    /// must not be read out by a screen reader.
    func testTheOverviewIsHiddenNotRemovedWhileTheDetailIsShown() {
        XCTAssertTrue(
            code.contains("ZStack"),
            "the detail is layered over the overview, not swapped in for it"
        )
        XCTAssertTrue(
            code.contains("opacity(navigation.isShowingDetail ? 0 : 1)"),
            "the overview must stay in the hierarchy, or its scroll position resets"
        )
        XCTAssertTrue(
            code.contains("allowsHitTesting(!navigation.isShowingDetail)"),
            "a hidden scroll view must not take the scroll events meant for the detail"
        )
        XCTAssertTrue(
            code.contains("accessibilityHidden(navigation.isShowingDetail)"),
            "a screen reader must not read out a list the user cannot see"
        )
    }

    /// HORO-1365: the shell hands the two stores down; it does not create
    /// them, and it does not let a card default one into existence.
    ///
    /// This is the assertion that keeps the fix from being undone by a
    /// convenience. `.opacity` above no longer protects the scan and the plan —
    /// ownership does — so if this file ever starts constructing a `ScanState`,
    /// every drill-down goes back to showing "No scan yet" no matter how the
    /// overview is presented.
    func testTheShellOwnsTheStoresAndHandsTheSameOnesDown() {
        XCTAssertTrue(
            code.contains("@StateObject private var scan = ScanState()"),
            "the shell owns the scan: it is above the drill-down and below the scene"
        )
        XCTAssertTrue(
            code.contains("@StateObject private var plan = PlanState()"),
            "the shell owns the plan for the same reason"
        )
        XCTAssertTrue(code.contains("scan: scan,"), "the candidates card must get the real scan")
        XCTAssertTrue(code.contains("plan: plan,"), "the AI Plan card must get the real plan")

        // `@ObservedObject` here would be the bug back: an observed object is
        // re-assigned from the init on every rebuild of this view, so a scan
        // would last exactly as long as whoever constructed it kept it alive.
        for forbidden in ["@ObservedObject", "scan: ScanState", "plan: PlanState"] {
            XCTAssertFalse(
                code.contains(forbidden),
                "\(forbidden) would put the stores' lifetime back in a caller's hands"
            )
        }
    }

    /// A drill-down with no way out of it is the same dead end
    /// `testThePanelIsNotADeadEnd` exists to prevent, one level down: the
    /// sheet this replaced could at least be dismissed with Escape.
    func testThereIsAWayBackOutOfTheDetailView() {
        XCTAssertTrue(code.contains("navigation.back()"))
        XCTAssertTrue(
            code.contains(".keyboardShortcut(.escape, modifiers: [])"),
            "Escape dismissed the sheet this replaced, and is what a user will reach for"
        )
        XCTAssertTrue(
            code.contains("accessibilityLabel(\"Back to all candidates\")"),
            "a chevron glyph alone announces nothing"
        )
    }

    // MARK: - Helpers

    /// Drops whole-line `//` comments only, matching
    /// `CandidateDetailViewTests.strippedOfComments`. A trailing comment on a
    /// line of code survives, which is the conservative direction: it can make
    /// a "must not contain" assertion fail spuriously, never pass falsely.
    private static func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }
}
