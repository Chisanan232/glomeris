//
//  MenuBarAppearance.swift
//  GlomerisMenuBar
//
//  HORO-1294: the menu-bar item's SF Symbol name, extracted out of an
//  inline literal in GlomerisMenuBarApp.swift so it can be compiled into
//  the test target and asserted to actually resolve on the running OS.
//
//  Why this needs a test at all: `MenuBarExtra`'s automatic style renders
//  the icon only — the title is accessibility text, not visible chrome. A
//  `systemImage` name that fails to resolve therefore produces an empty,
//  zero-width status item: the app launches, stays alive, registers in the
//  Aqua session, and has no reachable entrypoint whatsoever, while
//  emitting no error, no crash and no log output. Nothing short of a human
//  looking at the menu bar catches it.
//
//  Any change to `systemImageName` must keep
//  MenuBarAppearanceTests.testSystemImageNameResolvesOnThisOS green.
//

/// Presentation constants for the menu-bar item. Pure presentation — this
/// is a thin client, so no policy, evidence or action semantics belong
/// here (see GlomerisMenuBarApp.swift's standing project rule).
enum MenuBarAppearance {
    /// SF Symbol for the menu-bar item.
    ///
    /// `externaldrive` is available from macOS 11.0, comfortably below this
    /// target's 13.0 deployment floor. It replaced
    /// `externaldrive.badge.gearshape`, which is absent from
    /// CoreGlyphs' `name_availability.plist` and has never existed in any
    /// SF Symbols release — no drive-plus-gear symbol does.
    static let systemImageName = "externaldrive"

    /// Accessibility/tooltip title for the menu-bar item.
    static let title = "Glomeris"
}
