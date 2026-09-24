//
//  GlomerisDesignSystemTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1306. The design system's comments make several claims that are
//  easy to break silently later — that the spacing scale really is a
//  hierarchy, that PROTECTED is not painted like a failure, that "nothing
//  to clean" is not dressed as an error. These turn those claims into
//  assertions.
//
//  SwiftUI views are not rendered here. What is asserted is the part that
//  can be: the token relationships, the tone mapping, and the
//  GlomerisStateMessage model the three empty presentations are built
//  from.
//

import AppKit
import SwiftUI
import XCTest

final class GlomerisDesignSystemTests: XCTestCase {
    // MARK: - Tokens

    /// The spacing scale has to actually be a hierarchy, or the grouping it
    /// is supposed to express does nothing. Cards must be further apart
    /// from each other than their own rows are, and rows further apart than
    /// items sitting on one line — otherwise the popover reads as one
    /// undifferentiated list again, which is the specific problem this
    /// ticket set out to fix.
    func testSpacingScaleIsActuallyAHierarchy() {
        XCTAssertGreaterThan(
            GlomerisDesign.sectionSpacing,
            GlomerisDesign.rowSpacing,
            "cards must be further apart than the rows inside them, or the cards do not group anything"
        )
        XCTAssertGreaterThan(
            GlomerisDesign.rowSpacing,
            GlomerisDesign.inlineSpacing,
            "rows must be further apart than items within a row"
        )
        XCTAssertGreaterThan(
            GlomerisDesign.outerPadding,
            GlomerisDesign.cardPadding - 1,
            "the popover's own margin should not be tighter than a card's interior"
        )
    }

    /// A popover narrower than its own label column plus a value is a
    /// layout that can only wrap. The detail rows reserve a fixed label
    /// column, so the width has to leave a usable remainder next to it.
    func testPopoverIsWideEnoughForItsOwnDetailRows() {
        let remainder = GlomerisDesign.popoverWidth
            - (2 * GlomerisDesign.outerPadding)
            - (2 * GlomerisDesign.cardPadding)
            - GlomerisDesign.detailLabelWidth
        XCTAssertGreaterThan(
            remainder,
            120,
            """
            only \(remainder)pt is left for values next to the \
            \(GlomerisDesign.detailLabelWidth)pt label column — sizes and badges would wrap
            """
        )
    }

    /// The other end of the same argument, and the one HORO-1367 left
    /// unguarded when it widened the panel: a menu-bar popover is a column
    /// hanging off a menu-bar item, and a wide enough one stops reading as
    /// that and starts crowding the item it belongs to.
    ///
    /// Expressed against the narrowest display the app is expected to run on
    /// rather than as a bare number, because that is the reasoning the width
    /// was actually chosen by — `popoverWidth`'s own note claims the panel is
    /// a third of a 1280pt display, and nothing else in the suite would notice
    /// if a later widening made that false.
    func testThePopoverStaysAColumnAndNotAWindow() {
        let narrowestSupportedDisplayWidth: CGFloat = 1280
        XCTAssertLessThanOrEqual(
            GlomerisDesign.popoverWidth * 3,
            narrowestSupportedDisplayWidth,
            """
            \(GlomerisDesign.popoverWidth)pt is more than a third of a \
            \(narrowestSupportedDisplayWidth)pt display — wide enough to crowd the menu-bar \
            item it hangs from
            """
        )
    }

    /// A menu-bar popover that can grow without limit stops being a glance
    /// and starts covering the screen whose disk it is reporting on.
    func testBodyHeightIsBounded() {
        XCTAssertGreaterThan(GlomerisDesign.maxBodyHeight, 200, "too short to show a candidate list")
        XCTAssertLessThan(
            GlomerisDesign.maxBodyHeight,
            800,
            "a popover this tall covers the screen it is reporting on"
        )
    }

    /// HORO-1367. A ceiling alone never bound the panel — a `ScrollView`
    /// accepts any height it is offered, so it states no preference of its
    /// own and `MenuBarExtra(.window)` fell back to its own default. The
    /// floor is what states the preference, so it has to be a real floor:
    /// below the ceiling (or the range inverts) and tall enough to be worth
    /// opening at.
    func testTheOpeningHeightIsAFloorBelowTheCeiling() {
        XCTAssertLessThan(
            GlomerisDesign.minBodyHeight,
            GlomerisDesign.maxBodyHeight,
            "the floor is at or above the ceiling, so the range is not a range"
        )
        XCTAssertGreaterThan(
            GlomerisDesign.minBodyHeight,
            GlomerisDesign.floorBodyHeight,
            "the comfortable opening height is no better than the small-display fallback"
        )
        XCTAssertGreaterThan(
            GlomerisDesign.minBodyHeight,
            300,
            "a floor this low does not open onto more than one card, which is the point of having one"
        )
    }

    /// The comfortable range is what the panel opens at on the displays this
    /// app actually runs on. Asserted against real usable heights rather than
    /// pixels on screen, so this stays true regardless of how SwiftUI lays
    /// the panel out.
    ///
    /// The heights are `visibleFrame` heights — menu bar and Dock already
    /// deducted — for a 13" MacBook Air (1470x956 scaled, ~918 usable), a
    /// 16" MacBook Pro (~1079 usable) and a 27" external display (~1379).
    func testEveryLaptopSizedDisplayGetsTheFullComfortableRange() {
        for usableHeight in [918.0, 1079.0, 1379.0] as [CGFloat] {
            let limits = GlomerisDesign.bodyHeightLimits(visibleScreenHeight: usableHeight)
            XCTAssertEqual(
                limits.max,
                GlomerisDesign.maxBodyHeight,
                "a \(usableHeight)pt display has room for the full ceiling but was given \(limits.max)pt"
            )
            XCTAssertEqual(
                limits.min,
                GlomerisDesign.minBodyHeight,
                "a \(usableHeight)pt display was not offered the comfortable opening height"
            )
        }
    }

    /// AC6: a display too short for the comfortable range must get the height
    /// back, not have the panel run off the bottom of the screen. The panel
    /// still has to fit *with* its own header and footer, which is what the
    /// chrome allowance is for.
    ///
    /// Swept across the whole band where the panel is between the two
    /// regimes, rather than tested at one height. At any single height in
    /// this band `max` happens to equal `height - panelChromeAllowance`
    /// exactly, so a lone chrome-fit assertion there is an identity and
    /// discriminates nothing; the same assertion across the band does
    /// discriminate, because raising the floor or dropping the allowance term
    /// breaks it at the bottom of the band while leaving the top intact.
    func testEveryShortDisplayGetsAShorterPanelRatherThanOneOffTheScreen() {
        let crossover = GlomerisDesign.floorBodyHeight + GlomerisDesign.panelChromeAllowance
        let fullRange = GlomerisDesign.maxBodyHeight + GlomerisDesign.panelChromeAllowance

        for usableHeight in stride(from: crossover, through: fullRange, by: 20) {
            let limits = GlomerisDesign.bodyHeightLimits(visibleScreenHeight: usableHeight)

            XCTAssertLessThanOrEqual(
                limits.max + GlomerisDesign.panelChromeAllowance,
                usableHeight,
                "body + chrome is \(limits.max + GlomerisDesign.panelChromeAllowance)pt on a "
                    + "\(usableHeight)pt display — the footer would be off-screen"
            )
            XCTAssertLessThanOrEqual(limits.min, limits.max, "the range inverted at \(usableHeight)pt")
        }

        // And the ceiling really does come down, rather than the band being
        // vacuously satisfied by a range that never moves.
        XCTAssertLessThan(
            GlomerisDesign.bodyHeightLimits(visibleScreenHeight: 600).max,
            GlomerisDesign.maxBodyHeight,
            "the ceiling did not come down on a display too short for it"
        )
    }

    /// The one input range where the "gives the height back" promise stops
    /// holding, pinned so it stays a documented exception rather than
    /// becoming a surprise.
    ///
    /// Below `floorBodyHeight + panelChromeAllowance` the floor wins and the
    /// panel asks for more than the display has. That is deliberate — a body
    /// thinner than the floor is not a reading surface — and no display a Mac
    /// can drive comes close. What this asserts is that the crossover is
    /// exactly where the design system says it is, so the claim and the
    /// arithmetic cannot drift apart.
    func testTheHeightGivenBackStopsAtTheFloorAndNoLower() {
        let crossover = GlomerisDesign.floorBodyHeight + GlomerisDesign.panelChromeAllowance

        for usableHeight in [-1000.0, 0.0, 100.0, crossover - 1] as [CGFloat] {
            let limits = GlomerisDesign.bodyHeightLimits(visibleScreenHeight: usableHeight)
            XCTAssertEqual(
                limits.max,
                GlomerisDesign.floorBodyHeight,
                "below the crossover the body must sit on the floor, not below it"
            )
            XCTAssertEqual(limits.min, limits.max, "at the floor there is no range left to offer")
        }

        XCTAssertGreaterThan(
            GlomerisDesign.bodyHeightLimits(visibleScreenHeight: crossover).max,
            GlomerisDesign.floorBodyHeight - 1,
            "at the crossover the body should be exactly the floor and not less"
        )
        XCTAssertLessThan(
            crossover,
            500,
            "the exception must stay confined to displays no Mac can drive — \(crossover)pt is "
                + "getting close to a real one"
        )
    }

    /// Total, not just correct on the inputs that were thought of. A display
    /// that has not been configured yet reports a zero height, and
    /// `NSScreen.main` can be nil — both arrive here as 0 or less, and a
    /// negative or inverted range is a layout constraint SwiftUI will trap
    /// on.
    func testNoDisplayHeightCanProduceAnUnusableRange() {
        for usableHeight in [-1000.0, -1.0, 0.0, 1.0, 100.0, 139.0, 140.0, 141.0, 5000.0] as [CGFloat] {
            let limits = GlomerisDesign.bodyHeightLimits(visibleScreenHeight: usableHeight)
            XCTAssertGreaterThan(
                limits.min,
                0,
                "a \(usableHeight)pt display produced a non-positive minimum height"
            )
            XCTAssertLessThanOrEqual(
                limits.min,
                limits.max,
                "a \(usableHeight)pt display inverted the range: \(limits)"
            )
            XCTAssertGreaterThanOrEqual(
                limits.max,
                GlomerisDesign.floorBodyHeight,
                "a \(usableHeight)pt display shrank the body below the point of opening it"
            )
            XCTAssertLessThanOrEqual(
                limits.max,
                GlomerisDesign.maxBodyHeight,
                "a \(usableHeight)pt display was offered more than the ceiling"
            )
        }
    }

    // MARK: - Tone

    /// Two tones that paint identically are one tone, and the second one is
    /// a state the user cannot see.
    func testEveryToneIsVisuallyDistinct() {
        let names = GlomerisTone.allCases.map { $0.style.name }
        XCTAssertEqual(
            Set(names).count,
            names.count,
            "two tones share a style, so one of them renders as the other: \(names)"
        )
        XCTAssertEqual(names.count, GlomerisTone.allCases.count)
    }

    /// PROTECTED is the policy working, not a fault. If `guarded` were
    /// painted as `critical`, every correct refusal would look like a bug
    /// report — and the user would have nothing to fix.
    func testGuardedIsNotPaintedLikeAFailure() {
        XCTAssertNotEqual(GlomerisTone.guarded.style, GlomerisTone.critical.style)
        XCTAssertNotEqual(GlomerisTone.guarded.style, GlomerisTone.warning.style)
        XCTAssertEqual(GlomerisVocabulary.safety("PROTECTED").tone.style.name, "guarded")
    }

    /// An evidence gap is neither reassurance nor an error: nothing is
    /// known yet. It must not borrow either presentation.
    func testUnknownBorrowsNeitherReassuranceNorAlarm() {
        XCTAssertNotEqual(GlomerisTone.unknown.style, GlomerisTone.positive.style)
        XCTAssertNotEqual(GlomerisTone.unknown.style, GlomerisTone.critical.style)
        XCTAssertEqual(
            GlomerisVocabulary.safety("UNKNOWN_INCOMPLETE").tone.style.name,
            "unknown"
        )
    }

    /// The design-system half of the three-axes rule. The vocabulary tests
    /// assert storage impact carries the neutral *tone*; this asserts the
    /// neutral tone is not then painted as a safety signal on the way to
    /// the screen.
    func testStorageImpactIsPaintedNeutrallyAtEverySize() {
        for size in ["0 bytes", "9.9 MB", "47 GB", "3.1 TB"] {
            for isLowerBound in [true, false] {
                let term = GlomerisVocabulary.storageImpact(human: size, isLowerBound: isLowerBound)
                XCTAssertEqual(
                    term.tone.style.name,
                    "neutral",
                    "\(size) is painted \(term.tone.style.name) — size must not imply safety"
                )
                XCTAssertNotEqual(term.tone.style, GlomerisTone.positive.style)
                XCTAssertNotEqual(term.tone.style, GlomerisTone.critical.style)
            }
        }
    }

    // MARK: - Loading / empty / error

    /// A slow first `detect` on a cold cache must not read as a broken
    /// install. Loading carries no error tone and no warning glyph — it
    /// gets a spinner, which is why its symbol name is deliberately nil.
    func testLoadingDoesNotLookLikeAFailure() {
        let loading = GlomerisStateMessage.loading("Checking disk space…")
        XCTAssertEqual(loading.kind, .loading)
        XCTAssertNotEqual(loading.tone, .critical)
        XCTAssertNil(loading.symbolName, "loading renders a spinner, not a glyph")
        XCTAssertEqual(loading.title, "Checking disk space…", "the subject must survive verbatim")
    }

    /// "Nothing worth reclaiming" is good news about the machine, not a
    /// failure of the app. Dressing it in an error presentation trains the
    /// user to dismiss the panel.
    func testEmptyIsNotDressedAsAnError() {
        let empty = GlomerisStateMessage.empty(
            "Nothing worth reclaiming",
            detail: "Your disk is in good shape."
        )
        XCTAssertEqual(empty.kind, .empty)
        XCTAssertNotEqual(empty.tone, .critical)
        XCTAssertNotEqual(
            empty.symbolName,
            GlomerisStateMessage.failure("x").symbolName,
            "empty must not reuse the failure glyph"
        )
        XCTAssertEqual(empty.detail, "Your disk is in good shape.")
    }

    /// "Not scanned yet" and "scanned, found nothing" are different facts,
    /// and only the second one is a clean bill of health. The candidates
    /// section deliberately never scans on appear, so the first thing a user
    /// ever sees there is the un-looked state — under a checkmark it would
    /// read as "your disk is fine" before anything had been measured.
    func testNotLookedYetDoesNotClaimAnAllClear() {
        let notLooked = GlomerisStateMessage.notLookedYet(
            "No scan yet",
            detail: "Refresh to look for space you can reclaim."
        )
        let foundNothing = GlomerisStateMessage.empty("Nothing worth reclaiming")

        XCTAssertNotEqual(
            notLooked.symbolName,
            foundNothing.symbolName,
            "not having looked must not borrow the glyph that means 'all clear'"
        )
        XCTAssertNotEqual(notLooked.symbolName, "checkmark.circle")

        // Neither a failure nor reassurance: the user has nothing to fix,
        // they just have not pressed Refresh.
        XCTAssertEqual(notLooked.kind, .empty)
        XCTAssertEqual(notLooked.tone, .neutral)
        XCTAssertNotEqual(notLooked.symbolName, GlomerisStateMessage.failure("x").symbolName)
        XCTAssertEqual(notLooked.detail, "Refresh to look for space you can reclaim.")
    }

    /// A filter hiding every row is the user's own doing, not a fact about
    /// their disk — so it may borrow neither the checkmark that means "all
    /// clear" (things WERE found) nor the magnifying glass that means "nothing
    /// has been scanned" (something WAS scanned). The second would be the
    /// worse mistake: it invites a pointless rescan instead of pointing at the
    /// filter.
    func testFilteredOutBorrowsNeitherAllClearNorNotLookedYet() {
        let filtered = GlomerisStateMessage.filteredOut(
            "No candidates match this filter",
            detail: "Glomeris found 3 candidates, but none are protected."
        )

        XCTAssertNotEqual(filtered.symbolName, GlomerisStateMessage.empty("x").symbolName)
        XCTAssertNotEqual(filtered.symbolName, GlomerisStateMessage.notLookedYet("x").symbolName)
        XCTAssertNotEqual(filtered.symbolName, GlomerisStateMessage.nothingRecorded("x").symbolName)
        XCTAssertNotEqual(filtered.symbolName, GlomerisStateMessage.failure("x").symbolName)
        XCTAssertNotEqual(filtered.symbolName, "checkmark.circle")

        XCTAssertEqual(filtered.kind, .empty)
        XCTAssertEqual(filtered.tone, .neutral, "filtering is not a problem, it is just not a finding")
        XCTAssertEqual(filtered.detail, "Glomeris found 3 candidates, but none are protected.")
    }

    /// Failure is the only one of the three allowed to read as a problem —
    /// and it must say what failed. An empty list shown instead would imply
    /// "nothing to clean", which is the opposite of the truth.
    func testFailureIsTheOnlyAlarmingState() {
        let failure = GlomerisStateMessage.failure("detect: the CLI could not be run.")
        XCTAssertEqual(failure.kind, .failure)
        XCTAssertEqual(failure.tone, .critical)
        XCTAssertNotNil(failure.symbolName)

        XCTAssertNotEqual(GlomerisStateMessage.loading("x").tone, .critical)
        XCTAssertNotEqual(GlomerisStateMessage.empty("x").tone, .critical)
    }

    /// `SectionFetchErrors.shortMessage` already names the subcommand and
    /// summarises the failure in one actionable sentence — including, for a
    /// missing binary, the list of places actually searched. Re-wording or
    /// truncating it here would throw away the only part the user can act
    /// on.
    func testFailureMessagePassesThroughUnchanged() {
        let message = "status: the glomeris CLI was not found. Looked in: "
            + "/opt/homebrew/bin/glomeris, /usr/local/bin/glomeris."
        XCTAssertEqual(GlomerisStateMessage.failure(message).title, message)
    }

    /// The same silent failure mode as the vocabulary's symbols: a mistyped
    /// SF Symbol name compiles and renders as nothing at all, so the empty
    /// and failure states would show as a bare sentence with an invisible
    /// gap where the glyph should be.
    func testStateMessageSymbolsResolveToRealSFSymbols() {
        let symbols = [
            GlomerisStateMessage.empty("x").symbolName,
            GlomerisStateMessage.notLookedYet("x").symbolName,
            GlomerisStateMessage.nothingRecorded("x").symbolName,
            GlomerisStateMessage.filteredOut("x").symbolName,
            GlomerisStateMessage.success("x").symbolName,
            GlomerisStateMessage.failure("x").symbolName,
        ].compactMap { $0 }

        XCTAssertEqual(symbols.count, 6, "every non-loading state needs a glyph")
        for name in symbols {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not a real SF Symbol, so it would render as nothing at all"
            )
        }
    }

    /// "I cleaned it and reclaimed 1.2 GB" and "there is nothing here" are
    /// different claims. The first is the answer to something the user just
    /// pressed a button for, so it is stated in full contrast rather than
    /// greyed out as context, and it gets its own glyph.
    func testSuccessIsNotTheSameStateAsEmpty() {
        let success = GlomerisStateMessage.success("Cleaned — reclaimed 1.2 GB.")
        let empty = GlomerisStateMessage.empty("Nothing worth reclaiming")

        XCTAssertEqual(success.kind, .success)
        XCTAssertEqual(success.tone, .positive)
        XCTAssertNotEqual(success.symbolName, empty.symbolName)
        XCTAssertNotEqual(success.tone, empty.tone)
        XCTAssertNotEqual(success.titleColor, empty.titleColor)
        XCTAssertNotEqual(success.tone, .critical, "a completed cleanup must not read as a problem")
        XCTAssertEqual(success.title, "Cleaned — reclaimed 1.2 GB.", "the CLI's wording must survive verbatim")
    }

    /// An empty log is not a clean bill of health either. An empty pressure
    /// history could mean the disk has been steady, or it could mean nothing
    /// has been watching it — and the popover cannot tell those apart from
    /// the report it is handed, so it must not pick the flattering reading.
    func testNothingRecordedDoesNotClaimAnAllClear() {
        let nothingRecorded = GlomerisStateMessage.nothingRecorded(
            "No changes recorded yet",
            detail: "A line is logged here each time free space crosses a threshold."
        )

        XCTAssertNotEqual(
            nothingRecorded.symbolName,
            GlomerisStateMessage.empty("x").symbolName,
            "an empty log must not borrow the glyph that means 'all clear'"
        )
        // It is also not the same claim as "nobody has looked": the log HAS
        // been read, and it is genuinely empty.
        XCTAssertNotEqual(
            nothingRecorded.symbolName,
            GlomerisStateMessage.notLookedYet("x").symbolName
        )
        XCTAssertEqual(nothingRecorded.kind, .empty)
        XCTAssertEqual(nothingRecorded.tone, .neutral)
        XCTAssertNotEqual(nothingRecorded.symbolName, GlomerisStateMessage.failure("x").symbolName)
    }

    // MARK: - Appearance and legibility, across every source file

    /// Dynamic Type only works if the type scale asks for a text *style*.
    /// `.system(size: 11)` is 11pt at every accessibility setting, so a user
    /// who has turned text up gets a popover that ignores them — and one
    /// hardcoded size is enough to make the panel inconsistent, because
    /// everything around it does scale.
    ///
    /// Scanned across the whole target rather than asserted on
    /// `GlomerisDesign`, because the failure mode is a section view reaching
    /// past the tokens, not the tokens themselves being wrong.
    func testNoViewHardcodesAFixedFontSize() throws {
        for (name, code) in try Self.sourceFiles() {
            XCTAssertFalse(
                code.contains(".system(size:"),
                "\(name) sets a fixed point size, which does not follow Dynamic Type"
            )
            XCTAssertFalse(
                code.contains("Font.custom("),
                "\(name) uses a custom font at a fixed size"
            )
        }
    }

    /// Light and dark appearance are free as long as nothing names a literal
    /// colour: the semantic `Color`s, `.secondary`/`.tertiary` and the
    /// materials all resolve per appearance. A hardcoded RGB — or a forced
    /// `preferredColorScheme` — is how a panel ends up unreadable in one of
    /// the two, which is exactly the class of defect nobody catches without
    /// looking at the screen in both.
    ///
    /// `NSColor.` is rewritten before scanning so the one legitimate literal
    /// is not caught by the SwiftUI rule: `MenuBarAppearance` fills the mark
    /// flat black on purpose, because it is a *template* image and the system
    /// recolours it.
    func testNoViewHardcodesAnAppearanceSpecificColour() throws {
        let forbidden = [
            "Color(red:",
            "Color(white:",
            "Color(.sRGB",
            "Color.black",
            "Color.white",
            "preferredColorScheme(",
        ]
        for (name, code) in try Self.sourceFiles() {
            let swiftUIOnly = code.replacingOccurrences(of: "NSColor.", with: "appKitLiteral.")
            for token in forbidden {
                XCTAssertFalse(
                    swiftUIOnly.contains(token),
                    "\(name) contains \(token) — it will not adapt to light/dark appearance"
                )
            }
        }
    }

    /// Four kinds, six presentations. If any two became indistinguishable
    /// the section would be telling the user the wrong thing about its own
    /// condition.
    func testTheFourStatesArePairwiseDistinguishable() {
        let states = [
            GlomerisStateMessage.loading("Checking…"),
            GlomerisStateMessage.empty("Nothing worth reclaiming"),
            GlomerisStateMessage.notLookedYet("No scan yet"),
            GlomerisStateMessage.nothingRecorded("No changes recorded yet"),
            GlomerisStateMessage.success("Cleaned — reclaimed 1.2 GB."),
            GlomerisStateMessage.failure("detect failed"),
        ]
        let fingerprints = states.map { "\($0.kind)|\($0.symbolName ?? "spinner")|\($0.tone)" }
        XCTAssertEqual(
            Set(fingerprints).count,
            states.count,
            "two states present identically: \(fingerprints)"
        )
    }

    /// A control whose entire content is a glyph has no name unless one is
    /// given to it (HORO-1325).
    ///
    /// This is the same class of defect as the two scans above — invisible on
    /// screen, and only findable by reading — and it shipped for real: the
    /// project-roots pane's remove button was an `Image(systemName:)` and
    /// nothing else, so VoiceOver could say what glyph was there and not what
    /// pressing it would do, or to which of several paths.
    ///
    /// Only the `} label: {` form is examined, because that is the form a
    /// control takes when its label is not already text. `Button("Add")` names
    /// itself and needs nothing.
    ///
    /// `accessibilityHidden(` counts as satisfying the rule: an image that is
    /// genuinely decorative — because something adjacent already says the same
    /// thing — is correctly removed from the tree rather than named twice. What
    /// is never acceptable is neither.
    func testNoIconOnlyControlShipsWithoutAName() throws {
        var iconOnlyControlsExamined = 0

        for (name, code) in try Self.sourceFiles() {
            let lines = code.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)

            // Line numbers below are positions in the comment-stripped text,
            // not in the file — `sourceFiles()` drops comment lines, so they
            // are an ordinal ("the second such control in this file"), not
            // somewhere to jump to.
            for (index, line) in lines.enumerated() where line.contains("} label: {") {
                // Brace-count from the end of the `label: {` to find where the
                // closure actually closes, rather than guessing a window: a
                // fixed window can spill into the next view and find a `Text(`
                // that belongs to something else, which would silently excuse
                // the very control this is meant to catch.
                var depth = 1
                var closingLine: Int?
                for candidate in (index + 1)..<lines.count {
                    depth += lines[candidate].filter { $0 == "{" }.count
                    depth -= lines[candidate].filter { $0 == "}" }.count
                    if depth <= 0 {
                        closingLine = candidate
                        break
                    }
                }
                let end = try XCTUnwrap(
                    closingLine,
                    "\(name): the label closure opened at line \(index + 1) never closes — the "
                        + "brace scan is wrong, so this guard is not checking anything"
                )

                let body = lines[(index + 1)...end].joined(separator: "\n")
                guard body.contains("Image("), !body.contains("Text(") else { continue }
                iconOnlyControlsExamined += 1

                // The name may sit inside the closure, on the image, or after
                // the closure as a modifier on the control. Both are correct
                // and both appear in this target.
                let trailing = lines[end..<min(end + 8, lines.count)].joined(separator: "\n")
                let named = (body + trailing).contains(".accessibilityLabel(")
                    || (body + trailing).contains(".accessibilityHidden(")

                XCTAssertTrue(
                    named,
                    "\(name): the control whose label closure opens at line \(index + 1) is a "
                        + "glyph and nothing else, and carries no accessibilityLabel — VoiceOver "
                        + "has no way to say what it does"
                )
            }
        }

        // Without this the whole test passes by finding nothing, which is
        // exactly how a scan-shaped guard goes quietly dead.
        XCTAssertGreaterThanOrEqual(
            iconOnlyControlsExamined,
            3,
            "the icon-only control scan matched \(iconOnlyControlsExamined) controls — it used to "
                + "match more, so the pattern it looks for has probably changed"
        )
    }

    // MARK: - Helpers

    /// Every Swift file in the app target, comment-stripped, as
    /// (file name, code). Comments are removed so that documenting a
    /// forbidden construct — as the two scans above both do — cannot fail
    /// the scan it is explaining.
    private static func sourceFiles() throws -> [(String, String)] {
        let sourcesURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources")
        let names = try FileManager.default
            .contentsOfDirectory(atPath: sourcesURL.path)
            .filter { $0.hasSuffix(".swift") }
            .sorted()

        XCTAssertGreaterThan(names.count, 5, "the source scan found almost nothing — wrong path?")

        return try names.map { name in
            let code = try String(contentsOf: sourcesURL.appendingPathComponent(name), encoding: .utf8)
                .split(separator: "\n", omittingEmptySubsequences: false)
                .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
                .joined(separator: "\n")
            return (name, code)
        }
    }
}
