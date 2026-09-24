//
//  GlomerisByteFormatTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1452. The Swift half of the cross-language contract. Rust's
//  `golden_fixture_pins_the_shared_convention` makes
//  `tests/fixtures/human_bytes_golden.tsv` Rust's own output by definition;
//  this suite holds `GlomerisByteFormat.human` to the same file. Either
//  implementation moving fails in the language that moved, and changing the
//  convention deliberately means editing the fixture first and then following
//  in both languages.
//

import XCTest

final class GlomerisByteFormatTests: XCTestCase {
    private static let repoRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // GlomerisMenuBar
        .deletingLastPathComponent() // macos
        .deletingLastPathComponent() // repo root

    private static let fixtureURL: URL = repoRoot
        .appendingPathComponent("tests/fixtures/human_bytes_golden.tsv")

    /// `<bytes>` / `<rendered>` pairs, comments and blank lines dropped.
    private static func goldenCases() throws -> [(bytes: UInt64, expected: String)] {
        let text = try String(contentsOf: fixtureURL, encoding: .utf8)
        var cases: [(bytes: UInt64, expected: String)] = []
        for rawLine in text.components(separatedBy: "\n") {
            // Rust's reader does `trim_end()`; matching it keeps a stray CR or
            // trailing space from reading as part of the rendered field in one
            // language and not the other.
            var line = rawLine
            while let last = line.last, last.isWhitespace { line.removeLast() }
            if line.isEmpty || line.hasPrefix("#") { continue }
            let fields = line.components(separatedBy: "\t")
            guard fields.count == 2 else {
                XCTFail("fixture line is not <bytes>\\t<rendered>: \(line)")
                continue
            }
            guard let bytes = UInt64(fields[0]) else {
                XCTFail("fixture byte count \(fields[0]) does not parse as UInt64")
                continue
            }
            cases.append((bytes, fields[1]))
        }
        return cases
    }

    // MARK: - The contract

    func testEveryGoldenFixtureValueMatchesRustsRendering() throws {
        let cases = try Self.goldenCases()

        for (bytes, expected) in cases {
            XCTAssertEqual(
                GlomerisByteFormat.human(bytes),
                expected,
                "for \(bytes) bytes — this Swift port no longer agrees with Rust's human_bytes"
            )
        }

        // A fixture that failed to parse, or that someone emptied, would
        // otherwise pass the loop above by asserting nothing at all. Rust's
        // side of the contract asserts the same floor, so the two cannot be
        // narrowed independently.
        XCTAssertGreaterThanOrEqual(
            cases.count,
            16,
            "expected the fixture to still cover at least 16 values, read \(cases.count)"
        )
    }

    /// The fixture is only a contract if it is actually the file both sides
    /// read. This asserts it is where Rust's `CARGO_MANIFEST_DIR`-relative path
    /// resolves to, so a moved or renamed fixture fails loudly here rather than
    /// leaving this suite quietly asserting against a stale copy.
    func testTheFixtureIsTheOneRustReads() throws {
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: Self.fixtureURL.path),
            "no fixture at tests/fixtures/human_bytes_golden.tsv — Rust reads that exact path"
        )

        let rustSource = try String(
            contentsOf: Self.repoRoot.appendingPathComponent("src/reporting/bytes.rs"),
            encoding: .utf8
        )
        XCTAssertTrue(
            rustSource.contains("/tests/fixtures/human_bytes_golden.tsv"),
            "Rust no longer reads this fixture, so nothing is pinning Rust's side of the convention"
        )
    }

    // MARK: - Anti-vacuity: the contract test has to be able to fail

    /// Proves the loop above discriminates. A deliberately wrong renderer —
    /// 1000-based, which is exactly the `ByteCountFormatter` mistake HORO-1312
    /// fixed — must disagree with the fixture on at least one scaled value.
    /// Without this, a fixture of only sub-1024 values would look like coverage.
    func testAThousandBasedRendererWouldFailTheFixture() throws {
        func thousandBased(_ bytes: UInt64) -> String {
            if bytes < 1000 { return "\(bytes) B" }
            var value = Double(bytes)
            var index = 0
            let units = ["B", "KB", "MB", "GB", "TB", "PB"]
            while value >= 1000.0 && index < units.count - 1 {
                value /= 1000.0
                index += 1
            }
            return String(format: "%.1f", locale: nil, value) + " \(units[index])"
        }

        let disagreements = try Self.goldenCases().filter { thousandBased($0.bytes) != $0.expected }
        XCTAssertGreaterThan(
            disagreements.count,
            0,
            "the fixture cannot tell a 1000-based renderer from a 1024-based one"
        )
    }

    /// The other way a port silently drifts: rounding. Half-away-from-zero and
    /// half-to-even differ on an exact tie, and `%.1f` and Rust's `{:.1}` both
    /// round half to even — which is why the fixture pins 1280 bytes at
    /// "1.2 KB". This proves the fixture would catch the other rule.
    func testAHalfAwayFromZeroRendererWouldFailTheFixture() throws {
        func halfAwayFromZero(_ bytes: UInt64) -> String {
            if bytes < 1024 { return "\(bytes) B" }
            var value = Double(bytes)
            var index = 0
            let units = ["B", "KB", "MB", "GB", "TB", "PB"]
            while value >= 1024.0 && index < units.count - 1 {
                value /= 1024.0
                index += 1
            }
            let rounded = (value * 10.0).rounded(.toNearestOrAwayFromZero) / 10.0
            return String(format: "%.1f", locale: nil, rounded) + " \(units[index])"
        }

        let disagreements = try Self.goldenCases().filter { halfAwayFromZero($0.bytes) != $0.expected }
        XCTAssertGreaterThan(
            disagreements.count,
            0,
            "the fixture has no exact tie in it, so it cannot pin the rounding rule"
        )
        XCTAssertTrue(
            disagreements.contains { $0.bytes == 1280 },
            "1280 bytes is the tie the fixture exists to pin; it no longer discriminates"
        )
    }

    // MARK: - Properties the fixture cannot enumerate

    /// Monotonicity over a wide sweep. A formatter can match 16 points and
    /// still be wrong between them — a bigger count must never render as a
    /// smaller unit, and the label must always come from the table.
    func testRenderingIsMonotonicInUnitAcrossASweep() {
        let units = ["B", "KB", "MB", "GB", "TB", "PB"]
        var lastUnitIndex = 0

        // Powers of two from 1 B to 8 EB, plus a value either side of each, so
        // every unit boundary is crossed in both directions.
        for exponent in 0..<63 {
            let power = UInt64(1) << UInt64(exponent)
            for bytes in [power &- 1, power, power &+ 1] where bytes > 0 {
                let rendered = GlomerisByteFormat.human(bytes)
                let unit = String(rendered.split(separator: " ").last ?? "")
                guard let index = units.firstIndex(of: unit) else {
                    XCTFail("\(bytes) bytes rendered with an unknown unit: \(rendered)")
                    continue
                }
                XCTAssertGreaterThanOrEqual(
                    index,
                    lastUnitIndex,
                    "\(bytes) bytes fell back to a smaller unit than a smaller count did"
                )
                lastUnitIndex = max(lastUnitIndex, index)
            }
        }

        XCTAssertEqual(units[lastUnitIndex], "PB", "the sweep never reached the top unit")
    }

    /// Saturation. The loop stops at the last label rather than indexing past
    /// the table, so the largest representable count is a PB figure and not a
    /// crash.
    func testTheLargestRepresentableCountSaturatesAtPetabytes() {
        let rendered = GlomerisByteFormat.human(UInt64.max)
        XCTAssertTrue(rendered.hasSuffix(" PB"), "expected a PB-suffixed value, got \(rendered)")
        XCTAssertEqual(rendered, "16384.0 PB")
    }

    /// The sub-1024 branch is a bare integer, deliberately: a fractional digit
    /// on a whole-byte count is noise, and Rust does the same.
    func testCountsBelowOneKilobyteAreBareIntegers() {
        XCTAssertEqual(GlomerisByteFormat.human(0), "0 B")
        XCTAssertEqual(GlomerisByteFormat.human(1), "1 B")
        XCTAssertEqual(GlomerisByteFormat.human(1023), "1023 B")
        for bytes in stride(from: UInt64(0), to: UInt64(1024), by: 37) {
            XCTAssertEqual(
                GlomerisByteFormat.human(bytes),
                "\(bytes) B",
                "a count below 1024 gained a decimal place"
            )
        }
    }

    /// `String(format:locale:)` with `locale: nil` is load-bearing: the decimal
    /// separator has to be a full stop whatever the user's locale is, because
    /// Rust's `{:.1}` always writes one and a comma would make the two
    /// languages disagree for a reason no en_US CI runner would ever surface.
    func testTheDecimalSeparatorIsAlwaysAFullStop() {
        for bytes: UInt64 in [1280, 1_536, 5_242_880, 1_288_490_189] {
            let rendered = GlomerisByteFormat.human(bytes)
            XCTAssertTrue(rendered.contains("."), "\(rendered) has no full stop")
            XCTAssertFalse(rendered.contains(","), "\(rendered) used a localised separator")
        }
    }

    /// `ByteCountFormatter` is the thing this file exists to avoid; it is
    /// 1000-based and would reintroduce HORO-1312. Asserted against the source
    /// because a type that is merely unused today can be reached for tomorrow.
    func testTheImplementationDoesNotReachForByteCountFormatter() throws {
        let source = try String(
            contentsOf: Self.repoRoot
                .appendingPathComponent("macos/GlomerisMenuBar/Sources/GlomerisByteFormat.swift"),
            encoding: .utf8
        )
        let code = source
            .components(separatedBy: "\n")
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        XCTAssertFalse(
            code.contains("ByteCountFormatter"),
            "HORO-1312: ByteCountFormatter is 1000-based and disagrees with the CLI"
        )
        XCTAssertFalse(
            code.contains("NumberFormatter"),
            "a localising formatter would move the decimal separator away from Rust's"
        )
        XCTAssertTrue(
            code.contains("locale: nil"),
            "the format call must pin the locale, or a European locale writes a comma"
        )
    }
}
