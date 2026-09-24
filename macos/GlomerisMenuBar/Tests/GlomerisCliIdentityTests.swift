//
//  GlomerisCliIdentityTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1466: which `glomeris` the app resolved, and whether it is the one the
//  app was built against.
//
//  Two things these tests deliberately do NOT do.
//
//  They do not install a binary or depend on the host having one. Every input
//  is injected — the locator's filesystem predicate, the declared hash, and the
//  hashing function — for the same reason GlomerisExecutableLocatorTests does
//  it: whether this suite passes must not depend on whether the machine running
//  it happens to have a `glomeris` in `/opt/homebrew/bin`, which is a member of
//  the very class of bug being fixed here.
//
//  And they do not assert on wording. `GlomerisVocabularyTests` owns that. What
//  is asserted here is the comparison rule and the shape of the result: that
//  there is no way to hold a resolved path without something to say about its
//  bytes, and that "this app declares no expectation" stays distinguishable
//  from "the bytes disagree".
//

import XCTest

final class GlomerisCliIdentityTests: XCTestCase {
    /// A plausible SHA-256, and a different one. Both are 64 hex characters
    /// because a shorter stand-in would let a length assumption pass here and
    /// fail against real output.
    private let hashA = String(repeating: "a1b2c3d4", count: 8)
    private let hashB = String(repeating: "f0e1d2c3", count: 8)

    private let located = URL(fileURLWithPath: "/opt/homebrew/bin/glomeris")

    // MARK: - The comparison rule

    func testMatchesWhenTheResolvedBytesHashToTheDeclaredValue() {
        XCTAssertEqual(
            GlomerisCliIdentity.expectation(
                for: .sha256(hashA),
                declaredExpectedHash: hashA
            ),
            .matches
        )
    }

    /// The reported defect, reduced to its rule: this is the case that a
    /// version comparison cannot detect, because both binaries answer `0.2.0`.
    func testDiffersWhenTheResolvedBytesHashToSomethingElse() {
        XCTAssertEqual(
            GlomerisCliIdentity.expectation(
                for: .sha256(hashA),
                declaredExpectedHash: hashB
            ),
            .differs(expected: hashB)
        )
    }

    /// Hex case is a property of whatever produced the string — `shasum`
    /// lowercases, PlistBuddy round-trips verbatim, and a human editing
    /// Info.plist may paste either — so it must not decide the verdict.
    func testHashComparisonIgnoresHexCaseAndSurroundingWhitespace() {
        XCTAssertEqual(
            GlomerisCliIdentity.expectation(
                for: .sha256(hashA.lowercased()),
                declaredExpectedHash: "  \(hashA.uppercased())\n"
            ),
            .matches
        )
    }

    func testNotDeclaredWhenTheAppRecordsNoExpectedHash() {
        XCTAssertEqual(
            GlomerisCliIdentity.expectation(
                for: .sha256(hashA),
                declaredExpectedHash: nil
            ),
            .notDeclared
        )
    }

    /// A blank declared value is absence, not a mismatch against "".
    ///
    /// This is the case an Info.plist build-setting substitution produces when
    /// the setting is unset: expansion fails silently and leaves an empty
    /// string. Reading that as "expected nothing, therefore differs" would make
    /// every developer build shout that its CLI is the wrong one, and a warning
    /// that is always on is a warning nobody reads — which would cost exactly
    /// the signal this ticket adds.
    func testBlankDeclaredHashCountsAsAbsentRatherThanAsAMismatch() {
        for blank in ["", "   ", "\n", "\t "] {
            XCTAssertEqual(
                GlomerisCliIdentity.expectation(
                    for: .sha256(hashA),
                    declaredExpectedHash: blank
                ),
                .notDeclared,
                "a declared hash of \(blank.debugDescription) is an absent one"
            )
        }
    }

    /// Unreadable bytes with an expectation on file is its own state. Folding
    /// it into `differs` would report a mismatch that was never measured, and
    /// folding it into `matches` would claim a check that never ran.
    func testNotComparableWhenTheBytesCouldNotBeReadButAHashIsDeclared() {
        XCTAssertEqual(
            GlomerisCliIdentity.expectation(
                for: .unreadable("its contents could not be read"),
                declaredExpectedHash: hashA
            ),
            .notComparable
        )
    }

    /// Precedence between the two "cannot say" states: with nothing declared,
    /// unreadable bytes are still `notDeclared`, because there was nothing to
    /// compare against regardless of whether the read succeeded. The two states
    /// answer different questions — one about this app, one about the binary.
    func testUnreadableBytesWithNoDeclaredHashIsNotDeclared() {
        XCTAssertEqual(
            GlomerisCliIdentity.expectation(
                for: .unreadable("its contents could not be read"),
                declaredExpectedHash: nil
            ),
            .notDeclared
        )
    }

    // MARK: - The expectation cannot contradict the content

    /// `expectation` is derived in `init`, so no caller can assemble an
    /// identity that reports agreement next to bytes that disagree.
    func testInitDerivesTheExpectationFromTheContentItWasGiven() {
        let identity = GlomerisCliIdentity(
            location: GlomerisExecutableLocation(url: located, source: .bundled),
            content: .sha256(hashA),
            declaredExpectedHash: hashB
        )

        XCTAssertEqual(identity.expectation, .differs(expected: hashB))
        XCTAssertEqual(identity.content, .sha256(hashA))
    }

    // MARK: - Abbreviation

    /// The row shows 12 hex characters; the full value stays available in the
    /// tooltip and the accessibility label, which the view renders from the
    /// whole hash rather than from this.
    func testShortDescriptionAbbreviatesAHashAndNamesUnreadability() {
        XCTAssertEqual(GlomerisCliContent.sha256(hashA).shortDescription, "a1b2c3d4a1b2")
        XCTAssertEqual(GlomerisCliContent.sha256(hashA).shortDescription.count, 12)
        XCTAssertEqual(GlomerisCliContent.unreadable("whatever").shortDescription, "unreadable")
    }

    // MARK: - Hashing real bytes

    /// Against a known vector, so this proves SHA-256 and not merely "some
    /// stable digest": the empty file's SHA-256 is a published constant.
    func testSha256OfFileMatchesTheKnownEmptyFileVector() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-empty-\(UUID().uuidString)")
        try Data().write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }

        XCTAssertEqual(
            GlomerisCliIdentity.sha256OfFile(url),
            .sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        )
    }

    /// Larger than the 1 MiB read chunk, so the chunked loop is proven to
    /// accumulate across reads rather than hashing only the first block. A
    /// single-block implementation would pass every other test here.
    func testSha256OfFileHashesTheWholeFileAcrossReadChunks() throws {
        let chunk = 1 << 20
        var bytes = Data(repeating: 0x41, count: chunk)
        bytes.append(Data(repeating: 0x42, count: 7))

        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-large-\(UUID().uuidString)")
        try bytes.write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }

        let firstBlockOnly = Data(repeating: 0x41, count: chunk)
        let firstBlockURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-first-\(UUID().uuidString)")
        try firstBlockOnly.write(to: firstBlockURL)
        defer { try? FileManager.default.removeItem(at: firstBlockURL) }

        let whole = GlomerisCliIdentity.sha256OfFile(url)
        XCTAssertNotEqual(
            whole,
            GlomerisCliIdentity.sha256OfFile(firstBlockURL),
            "the trailing bytes past the first read chunk must change the hash"
        )
        if case .sha256(let hash) = whole {
            XCTAssertEqual(hash.count, 64)
        } else {
            XCTFail("a readable file must hash, got \(whole)")
        }
    }

    func testSha256OfFileReportsUnreadableRatherThanReturningNothing() {
        let missing = URL(fileURLWithPath: "/nonexistent/glomeris-\(UUID().uuidString)")

        guard case .unreadable(let reason) = GlomerisCliIdentity.sha256OfFile(missing) else {
            return XCTFail("a file that cannot be opened must report why, not a hash")
        }
        XCTAssertFalse(reason.isEmpty, "the reason is rendered into a sentence, so it must exist")
    }

    // MARK: - The probe

    private func probe(
        executable: Set<String>,
        path: String? = nil,
        declaredExpectedHash: String? = nil,
        content: @escaping (URL) -> GlomerisCliContent = { _ in
            .sha256(String(repeating: "0", count: 64))
        }
    ) -> GlomerisCliIdentityProbe {
        GlomerisCliIdentityProbe(
            locator: GlomerisExecutableLocator(
                bundledExecutableURL: nil,
                pathVariable: path,
                isExecutableFile: { executable.contains($0.path) }
            ),
            declaredExpectedHash: declaredExpectedHash,
            contentOf: content
        )
    }

    func testProbeIdentifiesTheBinaryTheLocatorResolved() {
        let outcome = probe(
            executable: ["/opt/homebrew/bin/glomeris"],
            declaredExpectedHash: hashA,
            content: { _ in .sha256(self.hashA) }
        ).probe()

        guard case .identified(let identity) = outcome else {
            return XCTFail("expected an identified binary, got \(outcome)")
        }
        XCTAssertEqual(identity.location.url.path, "/opt/homebrew/bin/glomeris")
        XCTAssertEqual(identity.location.source, .knownInstallDirectory("/opt/homebrew/bin"))
        XCTAssertEqual(identity.content, .sha256(hashA))
        XCTAssertEqual(identity.expectation, .matches)
    }

    /// The hash is taken from the file the locator actually chose, not from
    /// some other candidate. Asserted by having the injected hasher answer
    /// per-path: a probe that hashed the wrong file would return `hashB`.
    func testProbeHashesTheResolvedPathAndNotAnotherCandidate() {
        let outcome = probe(
            executable: ["/opt/homebrew/bin/glomeris", "/usr/local/bin/glomeris"],
            declaredExpectedHash: hashA,
            content: { url in
                url.path == "/opt/homebrew/bin/glomeris" ? .sha256(self.hashA) : .sha256(self.hashB)
            }
        ).probe()

        guard case .identified(let identity) = outcome else {
            return XCTFail("expected an identified binary, got \(outcome)")
        }
        XCTAssertEqual(identity.content, .sha256(hashA))
        XCTAssertEqual(identity.expectation, .matches)
    }

    /// Nothing found names where it looked, from the locator's own fields — the
    /// same list `GlomerisClientError.executableNotFound` carries, so one
    /// install problem reads the same in both places.
    func testProbeReportsWhereItLookedWhenNothingWasFound() {
        let outcome = probe(executable: [], path: "/usr/bin:/bin").probe()

        guard case .notFound(let searched) = outcome else {
            return XCTFail("expected notFound, got \(outcome)")
        }
        XCTAssertTrue(searched.contains("the app bundle"))
        XCTAssertTrue(searched.contains("PATH"))
        XCTAssertTrue(searched.contains("/opt/homebrew/bin"))
        XCTAssertFalse(searched.isEmpty)
    }

    /// A build that stamps no expected hash says so, rather than claiming the
    /// resolved binary is wrong. This is every locally built app.
    func testProbeWithNoDeclaredHashReportsNotDeclaredAndStillNamesTheBinary() {
        let outcome = probe(
            executable: ["/usr/local/bin/glomeris"],
            declaredExpectedHash: nil,
            content: { _ in .sha256(self.hashA) }
        ).probe()

        guard case .identified(let identity) = outcome else {
            return XCTFail("expected an identified binary, got \(outcome)")
        }
        XCTAssertEqual(identity.expectation, .notDeclared)
        // The point of the state: the path is still reported. "Cannot compare"
        // must not degrade into "cannot say anything".
        XCTAssertEqual(identity.location.url.path, "/usr/local/bin/glomeris")
        XCTAssertEqual(identity.content, .sha256(hashA))
    }

    func testProbeOffMainThreadReturnsTheSameOutcomeAsTheSynchronousProbe() async {
        let subject = probe(
            executable: ["/opt/homebrew/bin/glomeris"],
            declaredExpectedHash: hashB,
            content: { _ in .sha256(self.hashA) }
        )

        // Awaited into a local first: XCTAssertEqual takes autoclosures, which
        // cannot contain an `async` call.
        let offMainThread = await subject.probeOffMainThread()
        XCTAssertEqual(offMainThread, subject.probe())
    }

    // MARK: - The structural property, independent of any rendering site

    /// The reason this is a type and not a convention: there is no value in
    /// `GlomerisCliIdentityOutcome` that carries a location without carrying
    /// content, so "showed a path and no identity" is unrepresentable rather
    /// than merely discouraged. Asserted by exhausting the enum — a new case
    /// carrying a bare location stops compiling here.
    func testNoOutcomeCarriesALocationWithoutContent() {
        let outcomes: [GlomerisCliIdentityOutcome] = [
            .notFound(searched: ["the app bundle"]),
            .identified(
                GlomerisCliIdentity(
                    location: GlomerisExecutableLocation(url: located, source: .bundled),
                    content: .sha256(hashA),
                    declaredExpectedHash: nil
                )
            ),
        ]

        for outcome in outcomes {
            switch outcome {
            case .notFound(let searched):
                XCTAssertFalse(searched.isEmpty)
            case .identified(let identity):
                // Reaching the location necessarily means having the content.
                XCTAssertFalse(identity.location.url.path.isEmpty)
                XCTAssertFalse(identity.content.shortDescription.isEmpty)
            }
        }
    }
}
