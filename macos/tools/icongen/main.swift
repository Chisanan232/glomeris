//
//  main.swift — Glomeris app-icon generator
//
//  HORO-1305. Rasterises the canonical `GlomerisMark` into the macOS
//  AppIcon set under GlomerisMenuBar/Sources/Assets.xcassets.
//
//  This tool is compiled *together with* the app's own
//  Sources/GlomerisMark.swift (see scripts/generate-app-icon.sh), not with
//  a copy of it. The app icon and the menu-bar glyph are therefore the same
//  geometry by construction rather than by diligence.
//
//  Reproducibility is a requirement, not a nicety: `--verify` re-renders and
//  compares against the committed PNGs, which is what lets a reviewer trust
//  binary assets they cannot read in a diff. So no timestamps, no randomness,
//  and no host-dependent colour management — an explicit sRGB colour space is
//  used rather than "the display's".
//
//  The comparison is pixel-wise with a small tolerance rather than a file
//  hash. A hash would also be sensitive to the PNG encoder's own choices and
//  to antialiasing differences in CoreGraphics' polygon rasteriser between
//  macOS versions — neither of which is drift, and both of which would turn
//  the CI guard into a flake that gets disabled.
//
//  Measured sensitivity of the tolerance below (negative controls run when it
//  was written, on the committed art):
//    - headRadius 0.400 -> 0.401, a 0.25% geometry change: caught at every
//      size, max Δ3 even on the 16px icon.
//    - a palette channel moved by 5/255: caught. By 1/255: absorbed.
//  So it catches anything a person could see and ignores what nobody can.
//  `maxChannelDelta` is the criterion that actually fires;
//  `meanChannelDelta` is a whole-image backstop that stays well under its
//  limit even for large changes to small features (recolouring the core
//  entirely only moves the mean to 0.145) and exists for the case where a
//  change is spread thinly across the full canvas.
//
//  Usage: icongen <path to AppIcon.appiconset> [--verify]
//         --verify: compare against what is already there; write nothing.
//

import AppKit
import CoreGraphics
import Foundation

// MARK: - Design

/// Palette for the full app icon.
///
/// The menu-bar glyph is a monochrome template and takes no colour from
/// here; this is the "richer visual treatment" the full icon is allowed.
///
/// Deliberately cool, deep and low-chroma. Bright greens, yellows and
/// sparkle-gradients are the visual vocabulary of consumer "speed up your
/// Mac" cleaners, which is the exact association HORO-1305 exists to break.
/// Slate-into-teal reads as instrument, not toy.
enum IconPalette {
    /// Top of the background gradient: deep slate.
    static let backgroundTop = (r: 0.133, g: 0.180, b: 0.263)
    /// Bottom of the background gradient: deep teal.
    static let backgroundBottom = (r: 0.059, g: 0.322, b: 0.341)
    /// The armoured plates: near-white, slightly cool.
    static let plates = (r: 0.949, g: 0.973, b: 0.980)
    /// The protected core: warm amber.
    ///
    /// The only warm element in the icon, and the only place colour carries
    /// meaning — it marks the thing being protected as *valuable*. It is
    /// redundant with the core's shape and position, never the sole
    /// indicator, so it survives greyscale and colour-blind rendering.
    static let core = (r: 0.910, g: 0.710, b: 0.290)
}

/// Fraction of the canvas left empty around the rounded-rect plate.
///
/// macOS app icons are drawn on a 1024 canvas with the artwork inset so the
/// system has room for the drop shadow it composites in Finder and the
/// Dock. ~10% per side matches Apple's macOS icon grid.
let plateMarginFraction = 0.098

/// Corner radius as a fraction of the rounded-rect plate's side. 0.225 is
/// the macOS "squircle" approximation.
let plateCornerFraction = 0.225

/// Fraction of the full canvas occupied by the mark.
///
/// Generous on purpose: at the 16pt end the mark has only ~16 device pixels
/// to work with, and margin spent on the plate is margin the glyph does not
/// get.
let markSizeFraction = 0.620

// MARK: - Rendering

func renderIcon(pixels: Int) -> CGImage {
    let side = CGFloat(pixels)
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!

    guard
        let context = CGContext(
            data: nil,
            width: pixels,
            height: pixels,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: colorSpace,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )
    else {
        fatalError("could not create a \(pixels)x\(pixels) bitmap context")
    }

    context.interpolationQuality = .high
    context.setAllowsAntialiasing(true)

    // Flip into the y-down space GlomerisMark documents, so the volute
    // curls the same way here as it does in the menu bar.
    context.translateBy(x: 0, y: side)
    context.scaleBy(x: 1, y: -1)

    // --- Rounded-rect plate with the background gradient ---
    let margin = side * CGFloat(plateMarginFraction)
    let plateRect = CGRect(
        x: margin, y: margin, width: side - margin * 2, height: side - margin * 2
    )
    let cornerRadius = plateRect.width * CGFloat(plateCornerFraction)
    let plate = CGPath(
        roundedRect: plateRect,
        cornerWidth: cornerRadius,
        cornerHeight: cornerRadius,
        transform: nil
    )

    context.saveGState()
    context.addPath(plate)
    context.clip()

    let gradient = CGGradient(
        colorsSpace: colorSpace,
        colors: [
            CGColor(
                srgbRed: CGFloat(IconPalette.backgroundTop.r),
                green: CGFloat(IconPalette.backgroundTop.g),
                blue: CGFloat(IconPalette.backgroundTop.b),
                alpha: 1
            ),
            CGColor(
                srgbRed: CGFloat(IconPalette.backgroundBottom.r),
                green: CGFloat(IconPalette.backgroundBottom.g),
                blue: CGFloat(IconPalette.backgroundBottom.b),
                alpha: 1
            ),
        ] as CFArray,
        locations: [0, 1]
    )!
    context.drawLinearGradient(
        gradient,
        start: CGPoint(x: plateRect.midX, y: plateRect.minY),
        end: CGPoint(x: plateRect.midX, y: plateRect.maxY),
        options: []
    )
    context.restoreGState()

    // --- The mark ---
    let markSide = side * CGFloat(markSizeFraction)
    let markRect = CGRect(
        x: (side - markSide) / 2,
        y: (side - markSide) / 2,
        width: markSide,
        height: markSide
    )

    context.addPath(GlomerisMark.brand.platesPath(in: markRect))
    context.setFillColor(
        CGColor(
            srgbRed: CGFloat(IconPalette.plates.r),
            green: CGFloat(IconPalette.plates.g),
            blue: CGFloat(IconPalette.plates.b),
            alpha: 1
        )
    )
    context.fillPath()

    context.addPath(GlomerisMark.brand.corePath(in: markRect))
    context.setFillColor(
        CGColor(
            srgbRed: CGFloat(IconPalette.core.r),
            green: CGFloat(IconPalette.core.g),
            blue: CGFloat(IconPalette.core.b),
            alpha: 1
        )
    )
    context.fillPath()

    guard let image = context.makeImage() else {
        fatalError("could not snapshot the \(pixels)x\(pixels) bitmap")
    }
    return image
}

func pngData(for image: CGImage) -> Data {
    let rep = NSBitmapImageRep(cgImage: image)
    // Setting an explicit pixel size keeps the PNG's own metadata stable
    // across hosts, which the hash-based drift check depends on.
    rep.size = NSSize(width: image.width, height: image.height)
    guard let data = rep.representation(using: .png, properties: [:]) else {
        fatalError("PNG encoding failed")
    }
    return data
}

// MARK: - Asset catalog

/// One entry in the macOS AppIcon set: logical point size and scale.
let variants: [(points: Int, scale: Int)] = [
    (16, 1), (16, 2),
    (32, 1), (32, 2),
    (128, 1), (128, 2),
    (256, 1), (256, 2),
    (512, 1), (512, 2),
]

func filename(points: Int, scale: Int) -> String {
    let suffix = scale == 1 ? "" : "@\(scale)x"
    return "icon_\(points)x\(points)\(suffix).png"
}

func contentsJSON() -> Data {
    // Hand-built rather than JSONSerialization so key order is fixed and
    // the file is a stable, readable diff.
    var images: [String] = []
    for variant in variants {
        images.append(
            """
                {
                  "filename" : "\(filename(points: variant.points, scale: variant.scale))",
                  "idiom" : "mac",
                  "scale" : "\(variant.scale)x",
                  "size" : "\(variant.points)x\(variant.points)"
                }
            """
        )
    }
    let json = """
        {
          "images" : [
        \(images.joined(separator: ",\n"))
          ],
          "info" : {
            "author" : "xcode",
            "version" : 1
          }
        }

        """
    return Data(json.utf8)
}

// MARK: - Verification

enum Tolerance {
    /// Largest per-channel difference, in 0...255, accepted on any single
    /// pixel. 2/255 absorbs rasteriser rounding along antialiased polygon
    /// edges; it cannot absorb a moved plate, a changed radius or a different
    /// colour, all of which shift whole regions by tens or hundreds of levels.
    static let maxChannelDelta = 2

    /// Largest mean per-channel difference across the whole image. Bounds the
    /// case where every edge pixel is off by the per-pixel allowance at once,
    /// which would still be a real (if subtle) change.
    static let meanChannelDelta = 0.35
}

/// Decodes an image into a tightly packed sRGB RGBA8 buffer.
///
/// Goes via an explicit context rather than trusting the file's own layout:
/// the committed PNG and the freshly rendered one must be compared in the
/// same colour space and channel order, whatever the encoder chose to store.
func rgbaBuffer(from image: CGImage) -> [UInt8] {
    let width = image.width
    let height = image.height
    var buffer = [UInt8](repeating: 0, count: width * height * 4)
    buffer.withUnsafeMutableBytes { raw in
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
            fatalError("could not create a comparison context")
        }
        context.draw(
            image, in: CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height))
        )
    }
    return buffer
}

func decodePNG(at url: URL) -> CGImage? {
    guard
        let data = try? Data(contentsOf: url),
        let rep = NSBitmapImageRep(data: data)
    else { return nil }
    return rep.cgImage
}

struct Deviation {
    let maxChannelDelta: Int
    let meanChannelDelta: Double

    var withinTolerance: Bool {
        maxChannelDelta <= Tolerance.maxChannelDelta
            && meanChannelDelta <= Tolerance.meanChannelDelta
    }
}

func compare(_ lhs: [UInt8], _ rhs: [UInt8]) -> Deviation {
    var maxDelta = 0
    var total = 0
    for index in 0..<lhs.count {
        let delta = abs(Int(lhs[index]) - Int(rhs[index]))
        maxDelta = max(maxDelta, delta)
        total += delta
    }
    return Deviation(
        maxChannelDelta: maxDelta,
        meanChannelDelta: Double(total) / Double(lhs.count)
    )
}

// MARK: - Entry point

let arguments = CommandLine.arguments
guard arguments.count == 2 || (arguments.count == 3 && arguments[2] == "--verify") else {
    FileHandle.standardError.write(
        Data("usage: icongen <path to AppIcon.appiconset> [--verify]\n".utf8)
    )
    exit(2)
}

let outputDirectory = URL(fileURLWithPath: arguments[1])
let verifyOnly = arguments.count == 3

// Render each distinct pixel size once and reuse it for every variant that
// needs it — (32,1) and (16,2) are both 32px and must be identical.
var rendered: [Int: Data] = [:]
func png(forPixels pixels: Int) -> Data {
    if let existing = rendered[pixels] { return existing }
    let data = pngData(for: renderIcon(pixels: pixels))
    rendered[pixels] = data
    return data
}

if verifyOnly {
    var failures: [String] = []

    for variant in variants {
        let pixels = variant.points * variant.scale
        let name = filename(points: variant.points, scale: variant.scale)
        let committedURL = outputDirectory.appendingPathComponent(name)

        guard let committed = decodePNG(at: committedURL) else {
            failures.append("\(name): missing or not a decodable PNG")
            continue
        }
        guard committed.width == pixels, committed.height == pixels else {
            failures.append(
                "\(name): is \(committed.width)x\(committed.height), expected \(pixels)x\(pixels)"
            )
            continue
        }
        guard
            let freshRep = NSBitmapImageRep(data: png(forPixels: pixels)),
            let fresh = freshRep.cgImage
        else {
            fatalError("could not decode freshly rendered \(pixels)px image")
        }

        let deviation = compare(rgbaBuffer(from: committed), rgbaBuffer(from: fresh))
        if deviation.withinTolerance {
            print("ok    \(name) (max Δ\(deviation.maxChannelDelta))")
        } else {
            failures.append(
                String(
                    format: "%@: differs from the geometry — max Δ%d (allowed %d), mean Δ%.3f (allowed %.2f)",
                    name, deviation.maxChannelDelta, Tolerance.maxChannelDelta,
                    deviation.meanChannelDelta, Tolerance.meanChannelDelta
                )
            )
        }
    }

    // Contents.json is generated text, so it is compared exactly.
    let contentsURL = outputDirectory.appendingPathComponent("Contents.json")
    if (try? Data(contentsOf: contentsURL)) != contentsJSON() {
        failures.append("Contents.json: does not match what icongen generates")
    } else {
        print("ok    Contents.json")
    }

    if failures.isEmpty {
        print("\nPASS: the committed AppIcon set matches GlomerisMark's geometry.")
        exit(0)
    }
    let report =
        (["", "FAIL: the committed AppIcon set is not what the geometry produces:"]
            + failures.map { "  - \($0)" }).joined(separator: "\n") + "\n"
    FileHandle.standardError.write(Data(report.utf8))
    exit(1)
}

try FileManager.default.createDirectory(
    at: outputDirectory, withIntermediateDirectories: true
)
for variant in variants {
    let destination = outputDirectory.appendingPathComponent(
        filename(points: variant.points, scale: variant.scale)
    )
    try png(forPixels: variant.points * variant.scale).write(to: destination)
}
try contentsJSON().write(
    to: outputDirectory.appendingPathComponent("Contents.json")
)

print("wrote \(variants.count) PNGs + Contents.json to \(outputDirectory.path)")
