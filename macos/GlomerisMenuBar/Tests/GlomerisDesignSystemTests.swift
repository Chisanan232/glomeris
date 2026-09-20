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
