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
            GlomerisStateMessage.failure("x").symbolName,
        ].compactMap { $0 }

        XCTAssertEqual(symbols.count, 2, "both non-loading states need a glyph")
        for name in symbols {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not a real SF Symbol, so it would render as nothing at all"
            )
        }
    }

    /// Three states, three presentations. If any two became
    /// indistinguishable the section would be telling the user the wrong
    /// thing about its own condition.
    func testTheThreeStatesArePairwiseDistinguishable() {
        let states = [
            GlomerisStateMessage.loading("Checking…"),
            GlomerisStateMessage.empty("Nothing worth reclaiming"),
            GlomerisStateMessage.failure("detect failed"),
        ]
        let fingerprints = states.map { "\($0.kind)|\($0.symbolName ?? "spinner")|\($0.tone)" }
        XCTAssertEqual(
            Set(fingerprints).count,
            states.count,
            "two of the three empty states present identically: \(fingerprints)"
        )
    }
}
