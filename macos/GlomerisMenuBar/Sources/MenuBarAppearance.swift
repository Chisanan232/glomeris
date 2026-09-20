//
//  MenuBarAppearance.swift
//  GlomerisMenuBar
//
//  HORO-1294 (origin): the menu-bar item's image, extracted out of an
//  inline literal in GlomerisMenuBarApp.swift so it can be compiled into
//  the test target and asserted to actually produce a visible glyph.
//
//  Why this needs a test at all: `MenuBarExtra` renders the icon only —
//  the title is accessibility text, not visible chrome. An icon that fails
//  to draw therefore produces an empty, zero-width status item: the app
//  launches, stays alive, registers in the Aqua session, and has no
//  reachable entrypoint whatsoever, while emitting no error, no crash and
//  no log output. Nothing short of a human looking at the menu bar catches
//  it. That is exactly how HORO-1294 shipped.
//
//  HORO-1305 changes what is drawn and, with it, how that class of defect
//  is guarded. The item no longer borrows a stock SF Symbol
//  (`externaldrive`, which said "external disk" rather than anything about
//  selective, policy-constrained reclamation). It draws the canonical
//  Glomeris mark from `GlomerisMark`.
//
//  The guard gets *stronger* as a result, not weaker. A `systemImage` name
//  could only be checked for resolving to something; the previous test
//  could not tell a rendered symbol from a blank one. A locally drawn
//  template image can be rasterised in-process and inspected for actual
//  ink, so `MenuBarAppearanceTests` now asserts the far more direct
//  property that HORO-1294 violated: opaque pixels exist.
//

import AppKit

/// Presentation constants and the rendered artwork for the menu-bar item.
/// Pure presentation — this is a thin client, so no policy, evidence or
/// action semantics belong here (see GlomerisMenuBarApp.swift's standing
/// project rule).
enum MenuBarAppearance {
    /// Accessibility/tooltip title for the menu-bar item.
    ///
    /// With a custom image (rather than a `systemImage` name) this is the
    /// only thing VoiceOver has to announce, so it is applied explicitly as
    /// the image's accessibility description as well as the scene's title.
    static let title = "Glomeris"

    /// Point size of the status-item image.
    ///
    /// 18pt is the conventional macOS menu-bar glyph size: it fills the
    /// 22pt bar with the ~2pt breathing room the system expects, and
    /// matches the optical weight of neighbouring system items.
    static let pointSize: CGFloat = 18

    /// The menu-bar glyph: the Glomeris mark as an AppKit **template**
    /// image.
    ///
    /// Template images are recoloured by the system, which is what makes
    /// the item correct in a light menu bar, a dark menu bar, behind a
    /// tinted desktop wallpaper, and while the item is highlighted —
    /// without this code knowing anything about appearance. That is why the
    /// mark is drawn in flat black and `isTemplate` is set, rather than
    /// picking a colour here.
    ///
    /// Re-rendered per call rather than cached: it is a handful of
    /// arithmetic on six polygons, called once per launch, and a cached
    /// `NSImage` would have to be invalidated on appearance changes.
    static func menuBarImage(pointSize: CGFloat = MenuBarAppearance.pointSize) -> NSImage {
        // `flipped: true` gives the drawing handler a y-down space, which
        // is the convention GlomerisMark documents and the app-icon
        // generator also uses. Without it the mark would curl the opposite
        // way here than it does in the app icon.
        let image = NSImage(
            size: CGSize(width: pointSize, height: pointSize),
            flipped: true
        ) { rect in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            context.addPath(GlomerisMark.brand.combinedPath(in: rect))
            NSColor.black.setFill()
            context.fillPath()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = title
        return image
    }
}
