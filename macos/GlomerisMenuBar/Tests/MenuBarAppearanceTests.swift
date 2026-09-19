//
//  MenuBarAppearanceTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1294: the menu-bar item's SF Symbol must actually resolve on the
//  OS the app is running on.
//
//  This is the regression test for a defect that shipped silently.
//  `MenuBarExtra`'s automatic style renders the icon only — the title is
//  accessibility text, not visible chrome — so a `systemImage` name that
//  fails to resolve yields an empty, zero-width status item. The app still
//  launches, stays alive and registers in the Aqua session; it simply has
//  no reachable entrypoint, and emits no error, no crash and no log
//  output. Nothing but a human looking at the menu bar caught it.
//
//  The assertion deliberately goes through MenuBarAppearance.systemImageName
//  rather than repeating the string literal: a test with its own copy of
//  the name would keep passing while the app shipped a different, broken
//  one.
//

import AppKit
import XCTest

// The test target compiles MenuBarAppearance.swift directly as one of its
// own sources (see project.yml), matching ProjectRootsStoreTests's pattern
// — GlomerisMenuBar is an `LSUIElement` app target with no importable
// framework product.

final class MenuBarAppearanceTests: XCTestCase {
    /// The actual regression guard: whatever `systemImageName` currently
    /// holds must be a real SF Symbol here and now.
    func testSystemImageNameResolvesOnThisOS() {
        let image = NSImage(
            systemSymbolName: MenuBarAppearance.systemImageName,
            accessibilityDescription: nil
        )
        XCTAssertNotNil(
            image,
            """
            SF Symbol "\(MenuBarAppearance.systemImageName)" does not resolve on \
            this OS, so MenuBarExtra will render an empty, invisible menu-bar \
            item and the app will have no reachable entrypoint. Pick a symbol \
            available at or below the target's deployment floor.
            """
        )
    }

    /// Guards the specific dead name, so a revert or a bad merge that
    /// reintroduces it fails loudly with a named cause rather than only
    /// tripping the generic resolution assertion above.
    func testSystemImageNameIsNotTheNonexistentGearshapeBadge() {
        XCTAssertNotEqual(
            MenuBarAppearance.systemImageName,
            "externaldrive.badge.gearshape",
            "This name has never existed in any SF Symbols release — see HORO-1294."
        )
    }

    /// The title is accessibility text for the status item, so an empty
    /// one would leave the item unlabelled for VoiceOver even when the
    /// icon renders.
    func testTitleIsNonEmpty() {
        XCTAssertFalse(MenuBarAppearance.title.isEmpty)
    }
}
