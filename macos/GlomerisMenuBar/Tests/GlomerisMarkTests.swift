//
//  GlomerisMarkTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1305: the measurable half of "is this identity any good".
//
//  What these tests can and cannot do is worth stating plainly, because the
//  distinction is the whole reason the mark is geometry in code rather than a
//  hand-drawn asset. They assert the properties that decide whether the mark
//  survives a 16pt monochrome menu bar and whether it still says what it is
//  meant to say: the plates stay countable, the gaps stay open, the volute
//  stays inside its box, the core stays protected, and the sealed posture is
//  visibly more closed than the open one.
//
//  They cannot assert that it is handsome. That is a founder dogfood
//  judgement (HORO-1313), and no assertion here should be read as a claim
//  about it.
//

import CoreGraphics
import XCTest

final class GlomerisMarkTests: XCTestCase {

    /// The rect every test draws into unless it is specifically about scale.
    /// Deliberately not square in one test below, to check the mark does not
    /// distort.
    private let box = CGRect(x: 0, y: 0, width: 100, height: 100)

    // MARK: - Helpers

    /// Every subpath of a `CGPath`, as its own point list.
    ///
    /// Used instead of just counting elements because the property under test
    /// is "six separate plates", and a single path containing six closed
    /// subpaths is exactly how that is represented.
    private func subpaths(of path: CGPath) -> [[CGPoint]] {
        var result: [[CGPoint]] = []
        var current: [CGPoint] = []
        path.applyWithBlock { element in
            switch element.pointee.type {
            case .moveToPoint:
                if !current.isEmpty { result.append(current) }
                current = [element.pointee.points[0]]
            case .addLineToPoint:
                current.append(element.pointee.points[0])
            case .addQuadCurveToPoint:
                current.append(element.pointee.points[1])
            case .addCurveToPoint:
                current.append(element.pointee.points[2])
            case .closeSubpath:
                break
            @unknown default:
                break
            }
        }
        if !current.isEmpty { result.append(current) }
        return result
    }

    private func distance(_ a: CGPoint, _ b: CGPoint) -> Double {
        Double(hypot(a.x - b.x, a.y - b.y))
    }

    // MARK: - Segmentation

    /// The mark must read as a segmented animal, which means the plates have
    /// to be separate closed shapes rather than one merged arc. If they ever
    /// merge, the identity stops saying "selective, segment-by-segment
    /// reclamation" and starts saying "swoosh".
    func testPlatesAreSixSeparateShapes() {
        let plates = subpaths(of: GlomerisMark.brand.platesPath(in: box))
        XCTAssertEqual(
            plates.count,
            GlomerisMark.plateCount,
            "the volute must be \(GlomerisMark.plateCount) discrete plates"
        )
    }

    /// Countability is not just a matter of drawing separate shapes — the gap
    /// between them has to survive rasterisation at menu-bar size. At 16pt
    /// the gap between consecutive plates must be at least half a pixel, or
    /// antialiasing bridges them into a continuous smear.
    func testGapsBetweenPlatesSurviveSixteenPointRendering() {
        let mark = GlomerisMark.brand
        let small = CGRect(x: 0, y: 0, width: 16, height: 16)
        let plates = subpaths(of: mark.platesPath(in: small))
        XCTAssertEqual(plates.count, GlomerisMark.plateCount)

        // Each plate is [outer edge..., inner edge reversed...]. The last
        // point of one plate's outer edge and the first point of the next
        // plate's outer edge are the two sides of a gap.
        let samples = GlomerisMark.samplesPerPlate
        for index in 0..<(plates.count - 1) {
            let gapStart = plates[index][samples - 1]
            let gapEnd = plates[index + 1][0]
            let width = distance(gapStart, gapEnd)
            XCTAssertGreaterThan(
                width,
                0.5,
                """
                the gap after plate \(index) is \(width)pt at 16pt, which \
                antialiasing will close — the plates stop being countable
                """
            )
        }
    }

    // MARK: - Bounds

    /// Nothing may escape the rect, at any curl. A status item clips to its
    /// box, and an app icon that overflows its rounded-rect plate looks like
    /// a rendering bug.
    func testMarkStaysInsideItsRectAtEveryCurl() {
        for curl in [0.0, 0.35, 0.5, 1.0] {
            let path = GlomerisMark(curl: curl).combinedPath(in: box)
            XCTAssertTrue(
                box.insetBy(dx: -0.001, dy: -0.001).contains(path.boundingBox),
                "curl \(curl) overflows: \(path.boundingBox) is outside \(box)"
            )
        }
    }

    /// `curl` is documented as clamped, and a caller animating past the ends
    /// (a spring overshoot, say) must not produce different geometry from the
    /// endpoint.
    func testCurlIsClampedRatherThanExtrapolated() {
        XCTAssertEqual(
            GlomerisMark(curl: -3).combinedPath(in: box).boundingBox,
            GlomerisMark(curl: 0).combinedPath(in: box).boundingBox
        )
        XCTAssertEqual(
            GlomerisMark(curl: 9).combinedPath(in: box).boundingBox,
            GlomerisMark(curl: 1).combinedPath(in: box).boundingBox
        )
    }

    /// The mark is radial, so it must use the smaller side of a non-square
    /// rect and stay circular rather than stretching into an ellipse.
    func testMarkDoesNotDistortInANonSquareRect() {
        let wide = CGRect(x: 0, y: 0, width: 200, height: 80)
        let bounds = GlomerisMark.brand.combinedPath(in: wide).boundingBox
        XCTAssertEqual(
            Double(bounds.width),
            Double(bounds.height),
            accuracy: 0.5,
            "the mark stretched to fill a non-square rect"
        )
        XCTAssertTrue(wide.insetBy(dx: -0.001, dy: -0.001).contains(bounds))
    }

    /// Scale invariance is the property that lets one definition serve a 16pt
    /// status item and a 1024pt app icon. Geometry at one size must be the
    /// same shape as geometry at another, modulo the scale factor.
    func testGeometryIsScaleInvariant() {
        let small = GlomerisMark.brand.combinedPath(
            in: CGRect(x: 0, y: 0, width: 10, height: 10)
        ).boundingBox
        let large = GlomerisMark.brand.combinedPath(
            in: CGRect(x: 0, y: 0, width: 1000, height: 1000)
        ).boundingBox
        XCTAssertEqual(Double(large.width / small.width), 100, accuracy: 0.01)
        XCTAssertEqual(Double(large.height / small.height), 100, accuracy: 0.01)
    }

    // MARK: - The protected core

    /// The core is the thing being protected. It must exist, be inside the
    /// volute, and never touch the plates — if it merges with the innermost
    /// plate it reads as part of the reclaimable body.
    func testCoreIsPresentCentredAndClearOfThePlates() {
        let mark = GlomerisMark.brand
        let core = mark.corePath(in: box).boundingBox
        XCTAssertFalse(core.isEmpty, "the protected core is missing")

        XCTAssertEqual(Double(core.midX), Double(box.midX), accuracy: 0.001)
        XCTAssertEqual(Double(core.midY), Double(box.midY), accuracy: 0.001)

        // The innermost plate edge sits at tailRadius - tailThickness/2.
        let innermostPlateEdge =
            GlomerisMark.tailRadius - GlomerisMark.tailThickness / 2
        XCTAssertLessThan(
            GlomerisMark.coreRadius,
            innermostPlateEdge,
            "the core touches or overlaps the tail plate instead of sitting inside it"
        )
    }

    /// At 16pt the core must still be at least a pixel across, or the mark
    /// loses its centre and the volute reads as an empty spiral.
    func testCoreIsVisibleAtSixteenPoints() {
        let core = GlomerisMark.brand
            .corePath(in: CGRect(x: 0, y: 0, width: 16, height: 16))
            .boundingBox
        XCTAssertGreaterThan(
            Double(core.width), 1.0, "the core vanishes at 16pt (\(core.width)pt across)"
        )
    }

    // MARK: - Posture semantics

    /// `.sealed` is the PROTECTED metaphor and must be visibly more closed
    /// than the neutral brand posture — both in how far the volute wraps and
    /// in how tight the gaps are. This is what makes the two postures
    /// distinguishable at a glance rather than a subtle shift nobody reads.
    func testSealedPostureIsMoreClosedThanTheBrandPosture() {
        XCTAssertGreaterThan(
            GlomerisMark.sealed.sweepDegrees,
            GlomerisMark.brand.sweepDegrees,
            "the sealed posture must wrap further round than the brand posture"
        )
        XCTAssertLessThan(
            GlomerisMark.sealed.gapDegrees,
            GlomerisMark.brand.gapDegrees,
            "the sealed posture must close its gaps"
        )
    }

    /// Even sealed, the plates stay countable. A fully closed disc would read
    /// as a plain button, losing both the animal and the segmentation that
    /// carries the product's meaning.
    func testSealedPostureStillHasCountablePlates() {
        XCTAssertGreaterThan(
            GlomerisMark.sealedGapDegrees, 0,
            "a zero gap turns the PROTECTED badge into a featureless disc"
        )
        XCTAssertEqual(
            subpaths(of: GlomerisMark.sealed.platesPath(in: box)).count,
            GlomerisMark.plateCount
        )
        XCTAssertLessThan(
            GlomerisMark.sealed.sweepDegrees, 360,
            "the volute must not overlap itself"
        )
    }

    /// The open posture must actually look open: an opening of a few degrees
    /// is not legible as "not a padlock".
    func testOpenPostureHasAVisibleOpening() {
        let opening = 360 - GlomerisMark(curl: 0).sweepDegrees
        XCTAssertGreaterThan(
            opening, 30, "the open volute's \(opening)° opening is too small to read"
        )
    }

    /// Plate width must stay positive for every curl, including the ones an
    /// animation passes through. If the gaps ever consumed the whole sweep,
    /// `plateSpanDegrees` would go negative and the plates would invert into
    /// self-intersecting polygons.
    func testPlateSpanStaysPositiveAcrossTheWholeCurlRange() {
        for step in 0...20 {
            let curl = Double(step) / 20
            XCTAssertGreaterThan(
                GlomerisMark(curl: curl).plateSpanDegrees,
                0,
                "plate span collapses at curl \(curl)"
            )
        }
    }

    // MARK: - Taper

    /// The armour is heaviest at the head and lightest at the tail. This is
    /// what gives the glyph a reading direction instead of looking like a
    /// uniform gear, and it is the one structural property most likely to be
    /// lost to a careless edit of the constants.
    func testArmourTapersFromHeadToTail() {
        XCTAssertGreaterThan(GlomerisMark.headRadius, GlomerisMark.tailRadius)
        XCTAssertGreaterThan(GlomerisMark.headThickness, GlomerisMark.tailThickness)
    }

    /// The taper must not go so far that the tail plate falls below a pixel
    /// at menu-bar size, which would leave the volute looking broken off.
    func testTailPlateIsThickerThanAPixelAtSixteenPoints() {
        XCTAssertGreaterThan(
            GlomerisMark.tailThickness * 16,
            1.0,
            "the tail plate is sub-pixel at 16pt and will disappear"
        )
    }

    /// The outermost extent must fit inside the unit box the constants are
    /// expressed in, which is what `testMarkStaysInsideItsRect` then observes
    /// concretely. Asserted on the constants too so a bad edit names its own
    /// cause.
    func testOutermostExtentFitsTheUnitBox() {
        let extent = GlomerisMark.headRadius + GlomerisMark.headThickness / 2
        XCTAssertLessThanOrEqual(
            extent, 0.5, "head plate extends beyond the mark's box (\(extent))"
        )
    }

    // MARK: - Composition

    /// The combined path is what every monochrome surface draws, so it must
    /// be the union of both parts. A missing core or missing plates here
    /// would ship a half-drawn mark even though both sub-paths test fine.
    func testCombinedPathContainsBothPlatesAndCore() {
        let mark = GlomerisMark.brand
        let combined = subpaths(of: mark.combinedPath(in: box)).count
        let plates = subpaths(of: mark.platesPath(in: box)).count
        let core = subpaths(of: mark.corePath(in: box)).count
        XCTAssertEqual(combined, plates + core)
    }

    func testShapeWrapperDrawsTheSameGeometryAsTheMark() {
        let shape = GlomerisMarkShape(mark: .brand)
        XCTAssertEqual(
            shape.path(in: box).boundingRect,
            GlomerisMark.brand.combinedPath(in: box).boundingBox
        )
    }

    /// An empty rect must not crash or produce garbage — SwiftUI lays views
    /// out at zero size during the first pass.
    func testEmptyRectProducesNoGeometryRatherThanCrashing() {
        let path = GlomerisMark.brand.combinedPath(in: .zero)
        XCTAssertTrue(path.boundingBox.isEmpty || path.boundingBox.width == 0)
    }
}
