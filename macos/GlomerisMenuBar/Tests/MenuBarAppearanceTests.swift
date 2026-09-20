//
//  MenuBarAppearanceTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1294 (origin): the menu-bar item must actually draw something.
//
//  This is the regression test for a defect that shipped silently.
//  `MenuBarExtra` renders the icon only — the title is accessibility text,
//  not visible chrome — so an icon that fails to draw yields an empty,
//  zero-width status item. The app still launches, stays alive and registers
//  in the Aqua session; it simply has no reachable entrypoint, and emits no
//  error, no crash and no log output. Nothing but a human looking at the
//  menu bar caught it.
//
//  HORO-1305 replaced the stock `externaldrive` SF Symbol with the Glomeris
//  mark drawn from `GlomerisMark`, and the guard got stronger as a result.
//  The old test could only ask whether a symbol *name resolved* — it could
//  not distinguish a rendered glyph from a blank one, so it never actually
//  tested the property HORO-1294 violated. A locally drawn image can be
//  rasterised in-process and inspected, so these tests assert the direct
//  thing: opaque pixels exist, at the sizes macOS will ask for.
//

import AppKit
import XCTest

// The test target compiles MenuBarAppearance.swift and GlomerisMark.swift
// directly as its own sources (see project.yml), matching
// ProjectRootsStoreTests's pattern — GlomerisMenuBar is an `LSUIElement` app
// target with no importable framework product.

final class MenuBarAppearanceTests: XCTestCase {

    // MARK: - Helpers

    /// Rasterises an `NSImage` at its own size and returns the fraction of
    /// pixels with meaningful alpha.
    ///
    /// Drawn into an explicit sRGB context rather than asking the image for a
    /// representation, so the result does not depend on the host's display or
    /// on what backing store AppKit happened to choose.
    private func inkCoverage(of image: NSImage) -> Double {
        let width = Int(image.size.width.rounded())
        let height = Int(image.size.height.rounded())
        XCTAssertGreaterThan(width, 0, "image has zero width")
        XCTAssertGreaterThan(height, 0, "image has zero height")

        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        pixels.withUnsafeMutableBytes { raw in
            guard
                let context = CGContext(
                    data: raw.baseAddress,
                    width: width,
                    height: height,
                    bitsPerComponent: 8,
                    bytesPerRow: width * 4,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                )
            else {
                XCTFail("could not create a \(width)x\(height) inspection context")
                return
            }
            let previous = NSGraphicsContext.current
            NSGraphicsContext.current = NSGraphicsContext(
                cgContext: context, flipped: false
            )
            image.draw(
                in: NSRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height))
            )
            NSGraphicsContext.current = previous
        }

        // Count alpha only. A template image's colour is whatever the system
        // decides at composite time, so coverage is the only appearance-
        // independent measure of "did anything get drawn".
        var opaque = 0
        for index in stride(from: 3, to: pixels.count, by: 4) where pixels[index] > 8 {
            opaque += 1
        }
        return Double(opaque) / Double(width * height)
    }

    // MARK: - The HORO-1294 regression guard

    /// The actual guard: the menu-bar image deposits ink.
    ///
    /// The lower bound is what makes this meaningful. A blank image and an
    /// image with one stray antialiased pixel both "exist"; neither is a
    /// visible menu-bar item. 6% of the 18x18 box is roughly 19 pixels — far
    /// more than rounding noise, and far below the mark's measured 0.287, so
    /// the bound leaves the design room to change without becoming a pin on
    /// its current coverage.
    ///
    /// Verified to bite: stubbing the draw handler out reports 0.0 here and
    /// fails with this message, which is precisely the state HORO-1294
    /// shipped.
    func testMenuBarImageActuallyDrawsInk() {
        let coverage = inkCoverage(of: MenuBarAppearance.menuBarImage())
        XCTAssertGreaterThan(
            coverage,
            0.06,
            """
            The menu-bar image is blank or near-blank (\(coverage) of pixels \
            inked). MenuBarExtra renders the icon only, so this ships an \
            invisible, zero-width status item with no reachable entrypoint and \
            no error of any kind — see HORO-1294.
            """
        )
    }

    /// Ink at every size macOS may ask for, not just the default.
    ///
    /// A shape can be legible at 18pt and collapse to nothing at 11pt if a
    /// dimension rounds to zero, and the menu bar's height is not a constant
    /// across displays and accessibility settings.
    ///
    /// Measured coverage across this range is 0.248–0.364 (it rises at the
    /// small end, where antialiasing spreads each edge over proportionally
    /// more of the box), so the 0.04 floor is roughly a sixfold margin.
    func testMenuBarImageDrawsInkAtEverySizeTheBarMightRequest() {
        for pointSize in [11, 14, 16, 18, 22, 32] as [CGFloat] {
            let coverage = inkCoverage(
                of: MenuBarAppearance.menuBarImage(pointSize: pointSize)
            )
            XCTAssertGreaterThan(
                coverage,
                0.04,
                "the mark collapses to nothing at \(pointSize)pt (coverage \(coverage))"
            )
        }
    }

    /// The mark must not fill its box either: a solid blob would be
    /// indistinguishable from a rendering failure that paints the whole rect,
    /// and it would read as a button rather than a glyph.
    func testMenuBarImageIsAGlyphNotABlock() {
        let coverage = inkCoverage(of: MenuBarAppearance.menuBarImage())
        XCTAssertLessThan(
            coverage, 0.85, "the mark covers nearly its whole box (\(coverage))"
        )
    }

    // MARK: - Template and accessibility contract

    /// Template images are recoloured by the system. Without this the glyph
    /// keeps the flat black it is drawn in, which is invisible in a dark menu
    /// bar and wrong while the item is highlighted.
    func testMenuBarImageIsATemplateImage() {
        XCTAssertTrue(
            MenuBarAppearance.menuBarImage().isTemplate,
            """
            A non-template menu-bar image keeps its literal colour, so it \
            disappears against a dark menu bar and does not invert when the \
            item is highlighted.
            """
        )
    }

    /// With a custom image rather than a `systemImage` name, the image's own
    /// accessibility description is what VoiceOver reads.
    func testMenuBarImageCarriesAnAccessibilityDescription() {
        XCTAssertEqual(
            MenuBarAppearance.menuBarImage().accessibilityDescription,
            MenuBarAppearance.title
        )
    }

    func testMenuBarImageHonoursTheRequestedSize() {
        let image = MenuBarAppearance.menuBarImage(pointSize: 18)
        XCTAssertEqual(image.size.width, 18)
        XCTAssertEqual(image.size.height, 18)
    }

    /// The title is accessibility text for the status item, so an empty one
    /// would leave the item unlabelled for VoiceOver even when the icon
    /// renders.
    func testTitleIsNonEmpty() {
        XCTAssertFalse(MenuBarAppearance.title.isEmpty)
    }

    /// 18pt is the conventional macOS menu-bar glyph size. Pinned because a
    /// larger value crops against the bar and a much smaller one reads as a
    /// speck; if this is changed deliberately, change it here too.
    func testDefaultPointSizeSuitsTheMenuBar() {
        XCTAssertGreaterThanOrEqual(MenuBarAppearance.pointSize, 14)
        XCTAssertLessThanOrEqual(MenuBarAppearance.pointSize, 20)
    }
}
