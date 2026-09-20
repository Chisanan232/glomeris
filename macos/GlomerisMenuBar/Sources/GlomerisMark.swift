//
//  GlomerisMark.swift
//  GlomerisMenuBar
//
//  HORO-1305: the canonical Glomeris mark, defined once as vector geometry
//  and shared by every surface that draws the brand — the menu-bar status
//  item, the Finder / System Settings / Login Items app icon, and the
//  PROTECTED state badge.
//
//  WHY GEOMETRY IN CODE RATHER THAN HAND-AUTHORED ART
//  --------------------------------------------------
//  Three properties fall out of it that a committed binary image cannot
//  offer:
//
//  1. No drift. The menu-bar glyph and the 1024pt app icon are literally
//     the same `Path`. There is no second copy of the identity to forget to
//     update, and no way for the two to diverge.
//  2. Reviewable. The identity arrives in a pull request as readable
//     numbers a human can reason about, not an opaque blob whose only
//     review is "looks fine to me". The PNGs under Assets.xcassets are
//     *derived*: `scripts/generate-app-icon.sh` regenerates them from this
//     file, and CI re-runs it and compares hashes, so the committed art is
//     provably what this code produces.
//  3. Testable. Legibility at menu-bar size is mostly a set of measurable
//     properties — plate count, stroke weight, gap width, staying inside
//     the bounds, actually depositing ink. GlomerisMarkTests asserts them.
//     It cannot assert that the result is *handsome*; that is a founder
//     dogfood question (HORO-1313).
//
//  WHAT THE MARK MEANS
//  -------------------
//  Glomeris marginata is a pill millipede: when threatened it rolls into a
//  sealed armoured ball, and it is otherwise a segmented animal that moves
//  deliberately, one segment at a time. Both halves of that are the
//  product's thesis, so both are in the mark:
//
//    - A **volute of discrete armoured plates**, not a continuous swoosh.
//      Countable segments read as selective, segment-by-segment reclamation
//      and as discrete evidence. A swoosh, a broom, a sparkle or a trash
//      can would all read as "toy cleaner", which is precisely the
//      impression this ticket exists to avoid.
//    - The volute **encloses a solid core**: protection. The core is never
//      one of the plates that can be reclaimed.
//    - At `curl == 0` the volute has a visible opening — it is a posture,
//      not a padlock, so the product does not read as "everything is
//      forbidden". At `curl == 1` it closes into the sealed ball, which is
//      the established "rolled up" metaphor reserved for PROTECTED.
//
//  THIN-CLIENT RULE
//  ----------------
//  Pure presentation geometry. No policy semantics live here: this type
//  does not know what PROTECTED *means*, it only knows how to draw a
//  tighter curl when a caller asks for one. See GlomerisMenuBarApp.swift's
//  standing project rule.
//

import CoreGraphics
import SwiftUI

/// The Glomeris identity, as resolution-independent geometry normalised
/// into whatever rect it is asked to draw into.
///
/// Drawn in a y-down coordinate space (CoreGraphics/SwiftUI `Path` in a
/// view rect), so angles increase clockwise on screen.
struct GlomerisMark: Equatable {
    /// How tightly the animal is rolled up.
    ///
    /// `0` is the open, walking volute used for the neutral brand (menu
    /// bar, app icon). `1` is the sealed defensive ball, reserved for
    /// PROTECTED. Values between the two exist so a caller can animate
    /// between postures; they are clamped on use.
    var curl: Double

    /// Neutral brand posture. Slightly curled rather than fully open, so
    /// the silhouette is compact enough to stay legible in a 16pt menu bar
    /// without the tail reading as a detached speck.
    static let brand = GlomerisMark(curl: 0.35)

    /// The PROTECTED posture: rolled into a sealed ball.
    static let sealed = GlomerisMark(curl: 1.0)

    // MARK: - Geometry constants
    //
    // All lengths are fractions of the smaller side of the target rect, so
    // the mark is identical at 16pt and 1024pt. The values are tuned for
    // the worst case — a 16×16 monochrome template on a busy menu bar —
    // because anything legible there is legible everywhere else.

    /// Number of armoured plates. Six is the most that still leaves gaps
    /// wide enough to read as separate segments at 16pt; more turns into a
    /// grey smear, fewer stops reading as "segmented animal".
    static let plateCount = 6

    /// Radius of the plate band's centreline at the head (outermost) end.
    static let headRadius = 0.400

    /// Radius of the plate band's centreline at the tail (innermost) end.
    static let tailRadius = 0.185

    /// Band thickness at the head. The head plate is the thickest — an
    /// animal's armour is heaviest at the front, and it gives the mark a
    /// clear visual entry point.
    static let headThickness = 0.130

    /// Band thickness at the tail.
    static let tailThickness = 0.070

    /// Radius of the protected core.
    static let coreRadius = 0.078

    /// Angle the head plate starts at, in degrees, clockwise from the
    /// positive x axis in a y-down space. -50° puts the head at the upper
    /// right, which is where a left-to-right reader's eye enters a glyph.
    static let startAngleDegrees = -50.0

    /// Total angular sweep of the volute when fully open (`curl == 0`).
    /// Less than 360° so the opening is visible.
    static let openSweepDegrees = 300.0

    /// Total angular sweep when sealed (`curl == 1`).
    static let sealedSweepDegrees = 356.0

    /// Angular gap between plates when fully open.
    static let openGapDegrees = 9.0

    /// Angular gap when sealed. Still non-zero: the plates must stay
    /// countable even in the sealed posture, or the PROTECTED badge
    /// degenerates into a plain disc that reads as a button.
    static let sealedGapDegrees = 3.5

    /// Points sampled along each plate's outer and inner edge. Enough that
    /// the facets are invisible at 1024pt.
    static let samplesPerPlate = 14

    // MARK: - Derived values

    /// `curl` clamped into its documented range.
    var clampedCurl: Double { min(max(curl, 0), 1) }

    var sweepDegrees: Double {
        Self.openSweepDegrees
            + (Self.sealedSweepDegrees - Self.openSweepDegrees) * clampedCurl
    }

    var gapDegrees: Double {
        Self.openGapDegrees
            + (Self.sealedGapDegrees - Self.openGapDegrees) * clampedCurl
    }

    /// Angular width of a single plate.
    var plateSpanDegrees: Double {
        (sweepDegrees - gapDegrees * Double(Self.plateCount)) / Double(Self.plateCount)
    }

    // MARK: - Path construction

    /// The outline of every armoured plate, as one path.
    ///
    /// Each plate is an annular sector whose centreline radius and
    /// thickness both taper from head to tail, so it is built as a sampled
    /// polygon — out along the outer edge, back along the inner edge —
    /// rather than with `addArc`, which cannot taper.
    func platesPath(in rect: CGRect) -> CGPath {
        let path = CGMutablePath()
        let metrics = Metrics(rect: rect)

        for plate in 0..<Self.plateCount {
            let startFraction = Double(plate) / Double(Self.plateCount)
            let plateStart = Self.startAngleDegrees
                + Double(plate) * (plateSpanDegrees + gapDegrees)

            var outer: [CGPoint] = []
            var inner: [CGPoint] = []
            outer.reserveCapacity(Self.samplesPerPlate)
            inner.reserveCapacity(Self.samplesPerPlate)

            for sample in 0..<Self.samplesPerPlate {
                let withinPlate = Double(sample) / Double(Self.samplesPerPlate - 1)
                let angle = plateStart + withinPlate * plateSpanDegrees
                // Taper across the whole volute, not within one plate, so
                // consecutive plates line up into a continuous spiral.
                let alongVolute = startFraction
                    + withinPlate / Double(Self.plateCount)

                let radius = Self.lerp(Self.headRadius, Self.tailRadius, alongVolute)
                let thickness = Self.lerp(
                    Self.headThickness, Self.tailThickness, alongVolute
                )

                outer.append(metrics.point(angle: angle, radius: radius + thickness / 2))
                inner.append(metrics.point(angle: angle, radius: radius - thickness / 2))
            }

            path.addLines(between: outer + inner.reversed())
            path.closeSubpath()
        }

        return path
    }

    /// The protected core at the centre of the volute.
    func corePath(in rect: CGRect) -> CGPath {
        let metrics = Metrics(rect: rect)
        let radius = Self.coreRadius * metrics.side
        return CGPath(
            ellipseIn: CGRect(
                x: metrics.center.x - radius,
                y: metrics.center.y - radius,
                width: radius * 2,
                height: radius * 2
            ),
            transform: nil
        )
    }

    /// Plates and core as a single path, which is what every monochrome
    /// surface (menu bar, template image, `Shape`) draws.
    func combinedPath(in rect: CGRect) -> CGPath {
        let path = CGMutablePath()
        path.addPath(platesPath(in: rect))
        path.addPath(corePath(in: rect))
        return path
    }

    // MARK: - Helpers

    private static func lerp(_ from: Double, _ to: Double, _ t: Double) -> Double {
        from + (to - from) * min(max(t, 0), 1)
    }

    /// Maps the normalised polar space onto a concrete rect. Uses the
    /// smaller side so the mark never distorts in a non-square rect, and
    /// centres it.
    private struct Metrics {
        let center: CGPoint
        let side: CGFloat

        init(rect: CGRect) {
            side = min(rect.width, rect.height)
            center = CGPoint(x: rect.midX, y: rect.midY)
        }

        func point(angle degrees: Double, radius: Double) -> CGPoint {
            let radians = degrees * .pi / 180
            return CGPoint(
                x: center.x + CGFloat(cos(radians) * radius) * side,
                y: center.y + CGFloat(sin(radians) * radius) * side
            )
        }
    }
}

// MARK: - SwiftUI

/// The Glomeris mark as a SwiftUI `Shape`, so it can be filled with
/// `Color.primary` and inherit light/dark adaptation for free.
struct GlomerisMarkShape: Shape, Equatable {
    var mark: GlomerisMark = .brand

    func path(in rect: CGRect) -> Path {
        Path(mark.combinedPath(in: rect))
    }
}
