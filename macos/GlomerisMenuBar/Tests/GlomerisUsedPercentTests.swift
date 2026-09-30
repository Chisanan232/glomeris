//
//  GlomerisUsedPercentTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1506. The Swift half of the cross-language contract. Rust's
//  `golden_fixture_pins_the_shared_convention` makes
//  `tests/fixtures/used_percent_golden.tsv` Rust's own output by definition;
//  this suite holds `GlomerisUsedPercent` to the same file. Either
//  implementation moving fails in the language that moved, and changing the
//  convention deliberately means editing the fixture first and then following
//  in both languages.
//
//  Modelled on `GlomerisByteFormatTests`, which established the arrangement for
//  byte counts in HORO-1452 — including the anti-vacuity tests, which are the
//  part that makes a golden fixture worth having: a fixture of only
//  already-one-decimal values would look like coverage while proving nothing
//  about the rounding rule this ticket exists for.
//

import XCTest

final class GlomerisUsedPercentTests: XCTestCase {
    private static let repoRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // GlomerisMenuBar
        .deletingLastPathComponent() // macos
        .deletingLastPathComponent() // repo root

    private static let fixtureURL: URL = repoRoot
        .appendingPathComponent("tests/fixtures/used_percent_golden.tsv")

    private struct GoldenCase {
        let measuredText: String
        let measured: Double
        let figure: String
        let sentence: String
    }

    /// `<measured>` / `<figure>` / `<sentence>` triples, comments and blank
    /// lines dropped.
    private static func goldenCases() throws -> [GoldenCase] {
        let text = try String(contentsOf: fixtureURL, encoding: .utf8)
        var cases: [GoldenCase] = []
        for rawLine in text.components(separatedBy: "\n") {
            // Rust's reader does `trim_end()`; matching it keeps a stray CR or
            // trailing space from reading as part of the last field in one
            // language and not the other.
            var line = rawLine
            while let last = line.last, last.isWhitespace { line.removeLast() }
            if line.isEmpty || line.hasPrefix("#") { continue }
            let fields = line.components(separatedBy: "\t")
            guard fields.count == 3 else {
                XCTFail("fixture line is not <measured>\\t<figure>\\t<sentence>: \(line)")
                continue
            }
            // Rust parses this column with `str::parse::<f64>()`, which accepts
            // "nan", "inf" and "-inf". `Double(_:)` accepts the same spellings,
            // which is why the fixture can carry the non-finite rows at all.
            guard let measured = Double(fields[0]) else {
                XCTFail("fixture measurement \(fields[0]) does not parse as Double")
                continue
            }
            cases.append(
                GoldenCase(
                    measuredText: fields[0],
                    measured: measured,
                    figure: fields[1],
                    sentence: fields[2]
                )
            )
        }
        return cases
    }

    // MARK: - The contract

    func testEveryGoldenFixtureValueMatchesRustsRendering() throws {
        let cases = try Self.goldenCases()

        for golden in cases {
            XCTAssertEqual(
                GlomerisUsedPercent.figure(golden.measured),
                golden.figure,
                "figure for \(golden.measuredText) — this Swift port no longer agrees with Rust"
            )
            XCTAssertEqual(
                GlomerisUsedPercent.text(golden.measured),
                golden.sentence,
                "sentence for \(golden.measuredText) — this Swift port no longer agrees with Rust"
            )
        }

        // A fixture that failed to parse, or that someone emptied, would
        // otherwise pass the loop above by asserting nothing at all. Rust's side
        // of the contract asserts the same floor, so the two cannot be narrowed
        // independently.
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
            "no fixture at tests/fixtures/used_percent_golden.tsv — Rust reads that exact path"
        )

        let rustSource = try String(
            contentsOf: Self.repoRoot.appendingPathComponent("src/reporting/used_percent.rs"),
            encoding: .utf8
        )
        XCTAssertTrue(
            rustSource.contains("/tests/fixtures/used_percent_golden.tsv"),
            "Rust no longer reads this fixture, so nothing is pinning Rust's side of the rule"
        )
    }

    /// The spoken form is pinned to the fixture too, without the fixture
    /// carrying a fourth column Rust could not produce: the CLI has no
    /// VoiceOver, so `spoken` is Swift-only, and it is defined as the sentence
    /// with the symbol spelled out. Asserting the derivation rather than a
    /// hand-written list means the spoken string cannot drift from the printed
    /// one even for a value nobody thought to write down.
    func testTheSpokenFormIsTheSentenceWithThePercentSignSpelledOut() throws {
        for golden in try Self.goldenCases() {
            let expected = golden.measured.isFinite
                ? golden.sentence.replacingOccurrences(of: "% used", with: " percent used")
                : GlomerisUsedPercent.unavailableText
            XCTAssertEqual(
                GlomerisUsedPercent.spoken(golden.measured),
                expected,
                "spoken form for \(golden.measuredText)"
            )
        }
    }

    // MARK: - Anti-vacuity: the contract test has to be able to fail

    /// Proves the fixture discriminates the rule this ticket exists for. A
    /// renderer that rounds to nearest — which is what every site did before —
    /// must disagree with the fixture on at least one value, and specifically on
    /// 89.96, the reading that rendered as "90.0% used" while the CLI decided
    /// 89.96 >= 90.0 was false.
    func testARoundToNearestRendererWouldFailTheFixture() throws {
        func roundToNearest(_ measured: Double) -> String {
            guard measured.isFinite else { return GlomerisUsedPercent.unavailableText }
            return String(format: "%.1f%% used", locale: nil, min(max(measured, 0), 100))
        }

        let cases = try Self.goldenCases()
        let disagreements = cases.filter { roundToNearest($0.measured) != $0.sentence }
        XCTAssertGreaterThan(
            disagreements.count,
            0,
            "the fixture cannot tell truncation from rounding to nearest"
        )
        XCTAssertTrue(
            disagreements.contains { $0.measuredText == "89.96" },
            "89.96 is the reading the fixture exists to pin; it no longer discriminates"
        )
    }

    /// The other way the port could silently drift: dropping the decimal place,
    /// which is what the capacity bar's old `Int(fraction * 100)` did. A
    /// whole-number renderer must fail the fixture, or the fixture is not
    /// pinning the precision either.
    func testAWholeNumberRendererWouldFailTheFixture() throws {
        func wholeNumber(_ measured: Double) -> String {
            guard measured.isFinite else { return GlomerisUsedPercent.unavailableText }
            return "\(Int(min(max(measured, 0), 100)))% used"
        }

        let disagreements = try Self.goldenCases().filter { wholeNumber($0.measured) != $0.sentence }
        XCTAssertGreaterThan(
            disagreements.count,
            0,
            "the fixture has no fractional value in it, so it cannot pin the precision"
        )
    }

    // MARK: - Boundaries the fixture cannot enumerate

    /// The invariant the rule exists to provide, over every whole-number
    /// threshold the settings stepper can produce.
    ///
    /// The CLI compares the raw measurement with `>=`. This asserts both
    /// directions that make the screen and that comparison the same statement:
    /// a displayed figure that has reached the threshold means the measurement
    /// had, and a measurement that reached it always displays as having done so.
    /// Rust asserts the identical property; this is the Swift half, because the
    /// figure a user reads before deciding whether to trust a silent app is
    /// rendered here.
    func testDisplayedReachingAWholeThresholdMeansTheDecisionCrossedIt() {
        for threshold in 1...99 {
            let t = Double(threshold)
            for delta in [-0.04, -0.01, -0.0001, 0.0, 0.0001, 0.04] {
                let measured = t + delta
                guard let displayed = GlomerisUsedPercent.displayed(measured) else {
                    return XCTFail("\(measured) is finite and must render")
                }
                XCTAssertEqual(
                    displayed >= t,
                    measured >= t,
                    "threshold \(t), measured \(measured): displayed \(displayed) disagrees "
                        + "with the comparison the CLI makes"
                )
            }
        }
    }

    /// Never above the measurement, for any reading. This is the half of the
    /// invariant that holds for a fractional threshold too — the CLI accepts one
    /// even though the stepper does not offer it — so the display can still
    /// never overstate a crossing.
    func testADisplayedFigureNeverExceedsTheMeasurement() {
        for hundredths in 0...10_000 {
            let measured = Double(hundredths) / 100
            guard let displayed = GlomerisUsedPercent.displayed(measured) else {
                return XCTFail("\(measured) is finite and must render")
            }
            XCTAssertLessThanOrEqual(
                displayed,
                measured + 1e-9,
                "\(displayed) must not exceed the measured \(measured)"
            )
        }
    }

    /// Guards the floating-point trap in the implementation: if `89.9 * 10`
    /// landed on 898.999… this would read "89.8% used" and every figure in the
    /// app would be a tenth low. Rust sweeps the same 1001 values.
    func testAValueAlreadyAtOneDecimalPlaceSurvivesIntact() {
        for tenths in 0...1000 {
            let value = Double(tenths) / 10
            XCTAssertEqual(
                GlomerisUsedPercent.text(value),
                String(format: "%.1f%% used", locale: nil, value),
                "a one-decimal value must render as itself: \(value)"
            )
        }
    }

    func testTheEndsOfTheRangeAreOrdinaryReadings() {
        XCTAssertEqual(GlomerisUsedPercent.text(0), "0.0% used")
        XCTAssertEqual(GlomerisUsedPercent.text(100), "100.0% used")
        XCTAssertEqual(GlomerisUsedPercent.figure(0), "0.0%")
        XCTAssertEqual(GlomerisUsedPercent.figure(100), "100.0%")
        XCTAssertEqual(GlomerisUsedPercent.spoken(0), "0.0 percent used")
        XCTAssertEqual(GlomerisUsedPercent.spoken(100), "100.0 percent used")
    }

    func testOutOfRangeReadingsAreClampedNotRenderedRaw() {
        XCTAssertEqual(GlomerisUsedPercent.text(-0.5), "0.0% used")
        XCTAssertEqual(GlomerisUsedPercent.text(140), "100.0% used")
        XCTAssertEqual(GlomerisUsedPercent.displayed(-0.5), 0)
        XCTAssertEqual(GlomerisUsedPercent.displayed(140), 100)
    }

    /// A `NaN` is not 0% used. The CLI's own `used_percent()` cannot produce
    /// one, but this arrives as JSON, where it can be anything — and a decoded
    /// `nan` silently rendering as an empty disk is the reading a storage tool
    /// least wants to be wrong about.
    func testANonFiniteReadingIsNamedRatherThanTurnedIntoZero() {
        for bogus in [Double.nan, .infinity, -.infinity] {
            XCTAssertNil(GlomerisUsedPercent.displayed(bogus))
            XCTAssertEqual(GlomerisUsedPercent.text(bogus), GlomerisUsedPercent.unavailableText)
            XCTAssertEqual(GlomerisUsedPercent.figure(bogus), GlomerisUsedPercent.unavailableText)
            XCTAssertEqual(GlomerisUsedPercent.spoken(bogus), GlomerisUsedPercent.unavailableText)
        }
    }

    // MARK: - The axis word

    /// The spoken form has to name the axis. A bare "94.2 percent" is the defect
    /// the capacity bar shipped with: it is the only thing a screen-reader user
    /// gets from the bar, and it does not say whether the disk is nearly full or
    /// nearly empty.
    func testEveryRenderingThatCarriesTheAxisSaysWhichAxis() {
        XCTAssertTrue(GlomerisUsedPercent.text(94.2).hasSuffix("% used"))
        XCTAssertTrue(GlomerisUsedPercent.spoken(94.2).hasSuffix(" percent used"))
        XCTAssertFalse(
            GlomerisUsedPercent.spoken(94.2).contains("%"),
            "a spoken string is read, not parsed; the symbol is spelled out"
        )
    }

    /// `locale: nil` is load-bearing, exactly as in `GlomerisByteFormat`: Rust's
    /// `{:.1}` always writes a full stop, and a localised formatter would write
    /// a comma under a European locale, so the two languages would disagree for
    /// a reason no en_US CI runner would surface.
    func testTheDecimalSeparatorIsAlwaysAFullStop() {
        for measured in [26.8, 72.046, 89.96, 94.2] {
            let rendered = GlomerisUsedPercent.text(measured)
            XCTAssertTrue(rendered.contains("."), "\(rendered) has no full stop")
            XCTAssertFalse(rendered.contains(","), "\(rendered) used a localised separator")
        }
    }

    /// Asserted against the source because a type that is merely unused today
    /// can be reached for tomorrow, and either of these would move the decimal
    /// separator away from Rust's.
    func testTheImplementationDoesNotReachForALocalisingFormatter() throws {
        let source = try String(
            contentsOf: Self.repoRoot
                .appendingPathComponent("macos/GlomerisMenuBar/Sources/GlomerisUsedPercent.swift"),
            encoding: .utf8
        )
        let code = source
            .components(separatedBy: "\n")
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        XCTAssertFalse(code.contains("NumberFormatter"), "a localising formatter moves the separator")
        XCTAssertFalse(code.contains("formatted("), "Double.formatted() localises by default")
        XCTAssertTrue(
            code.contains("locale: nil"),
            "the format call must pin the locale, or a European locale writes a comma"
        )
    }
}
