//
//  CandidatesSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1063: candidates list — cached snapshot + Refresh + live progress,
//  never a timer.
//

import XCTest

final class CandidatesSectionViewTests: XCTestCase {
    // MARK: - "Never a timer" invariant

    /// Mechanical proof, in the spirit of `GlomerisClientTests
    /// .testSourceContainsNoShellExecution`: `detect` is constructed as a
    /// command-line argument exactly once in this file, that one
    /// construction includes `--progress-json`, and the file contains no
    /// `Timer`, no `Task.sleep`, and no poll-loop mechanism of any kind
    /// (StatusHealthSectionView's `pollTask`/`pollInterval` pattern is
    /// deliberately absent here).
    func testDetectIsOnlyCalledWithProgressJSONAndNeverOnATimer() throws {
        let source = try Self.readSource("CandidatesSectionView.swift")

        // Occurrences as an error-message subject are excluded (HORO-1295
        // routes this view's failures through `SectionFetchErrors
        // .shortMessage(_:subject:)`, which names the subcommand): naming a
        // subcommand in a message is not invoking it. Every other occurrence
        // still counts, so a second call site constructed any other way trips
        // this just as before.
        let detectOccurrences = source.components(separatedBy: "\"detect\"").count - 1
        let detectSubjects = source.components(separatedBy: "subject: \"detect\"").count - 1
        XCTAssertEqual(
            detectOccurrences - detectSubjects,
            1,
            "detect must be constructed in exactly one place"
        )
        XCTAssertTrue(source.contains("--progress-json"), "the one detect invocation must pass --progress-json")

        XCTAssertFalse(source.contains("Timer("), "no code path may re-scan on a Timer")
        XCTAssertFalse(source.contains("Task.sleep"), "no code path may re-scan via a sleep loop")
        XCTAssertFalse(source.contains("pollTask"), "no polling task may drive detect")
        XCTAssertFalse(source.contains(".task {"), "detect must never run from an appear/task auto-trigger")
    }

    /// `runDetect()` (detect's one call site) is reachable only from the
    /// Refresh button's own action closure, not from `body`'s top level or
    /// any lifecycle hook.
    func testRunDetectIsOnlyInvokedFromTheRefreshButtonAction() throws {
        let source = try Self.readSource("CandidatesSectionView.swift")
        // The Refresh button wraps the call in `Task { await runDetect() }`
        // inside its action closure; assert that exact call site exists,
        // and that it's the only call to runDetect() in the file.
        let callSites = source.components(separatedBy: "runDetect()").count - 1
        // One definition (`func runDetect()`) + one call site.
        XCTAssertEqual(callSites, 2)
        XCTAssertTrue(source.contains("Task { await runDetect() }"))
    }

    // MARK: - NDJSON progress decoding

    func testDecodesDetectorStartedLine() throws {
        let json = #"{"phase":"detector_started","detector":"cargo"}"#
        let event = try JSONDecoder().decode(ProgressEventDto.self, from: Data(json.utf8))
        XCTAssertEqual(event, .detectorStarted(detector: "cargo"))
    }

    func testDecodesDetectorFinishedLine() throws {
        let json = #"{"phase":"detector_finished","detector":"cargo","candidates_found":3}"#
        let event = try JSONDecoder().decode(ProgressEventDto.self, from: Data(json.utf8))
        XCTAssertEqual(event, .detectorFinished(detector: "cargo", candidatesFound: 3))
    }

    func testUnknownPhaseFailsToDecodeRatherThanSilentlyDropping() {
        let json = #"{"phase":"something_new","detector":"cargo"}"#
        XCTAssertThrowsError(try JSONDecoder().decode(ProgressEventDto.self, from: Data(json.utf8)))
    }

    func testProgressStatusTextFormatsBothPhases() {
        XCTAssertEqual(
            ProgressStatusText.text(for: .detectorStarted(detector: "npm")),
            "Scanning: npm…"
        )
        XCTAssertEqual(
            ProgressStatusText.text(for: .detectorFinished(detector: "npm", candidatesFound: 2)),
            "Finished npm (2 found)"
        )
    }

    // MARK: - Live-progress callback (GlomerisClient extension)

    /// Proves the extended `GlomerisClient.run(_:onProgress:)` API decodes
    /// and invokes the callback once per NDJSON stderr line, in order —
    /// the mechanism CandidatesSectionView's live progress display is
    /// built on.
    func testGlomerisClientInvokesOnProgressPerNDJSONLine() async throws {
        struct ValueOutput: Decodable { let value: Int }

        let binaryURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-client-fixture-helper")
        // Reuse the same compiled fixture GlomerisClientTests builds; if
        // this test runs before that one has compiled it, build it here.
        if !FileManager.default.isExecutableFile(atPath: binaryURL.path) {
            let sourceURL = URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()
                .appendingPathComponent("Fixtures/glomeris_fixture_helper.c")
            let clang = Process()
            clang.executableURL = URL(fileURLWithPath: "/usr/bin/clang")
            clang.arguments = ["-O0", "-o", binaryURL.path, sourceURL.path]
            try clang.run()
            clang.waitUntilExit()
        }

        let client = GlomerisClient(executableURL: binaryURL)
        var seenEvents: [ProgressEventDto] = []
        let result = try await client.run(
            [
                "0",
                #"{"value": 1}"#,
                "{\"phase\":\"detector_started\",\"detector\":\"cargo\"}\n"
                    + "{\"phase\":\"detector_finished\",\"detector\":\"cargo\",\"candidates_found\":1}\n",
            ],
            outputType: ValueOutput.self,
            progressType: ProgressEventDto.self,
            onProgress: { event in
                seenEvents.append(event)
            }
        )

        XCTAssertEqual(result.output.value, 1)
        XCTAssertEqual(seenEvents, [
            .detectorStarted(detector: "cargo"),
            .detectorFinished(detector: "cargo", candidatesFound: 1),
        ])
        // The callback-observed events and the final collected
        // `progressLines` must agree — a caller that only inspects the
        // final result (like every pre-HORO-1063 call site) still sees
        // everything.
        XCTAssertEqual(result.progressLines, seenEvents)
    }

    // MARK: - Lower-bound marker rendering

    private func candidate(
        isLowerBound: Bool = false,
        kind: String = "cargo_target",
        human: String? = "1.0 MB",
        policyLabel: String = "AUTO_SAFE",
        resourceId: String = "/tmp/example/target",
        impactTier: String? = nil
    ) -> DetectCandidateReportDto {
        DetectCandidateReportDto(
            resourceId: resourceId,
            kind: kind,
            reclaimableBytes: 1_048_576,
            reclaimableHuman: human,
            reclaimableBytesIsLowerBound: isLowerBound,
            impactTier: impactTier,
            policyLabel: policyLabel,
            reasons: ["regenerable by cargo build"],
            executable: true,
            offeredActions: [],
            refusalReason: nil
        )
    }

    func testLowerBoundMarkerRendersWhenTrue() {
        let row = CandidateRowViewModel(candidate(isLowerBound: true))
        XCTAssertTrue(row.reclaimableText.contains("\u{2265}"))
        XCTAssertTrue(row.reclaimableText.contains("1.0 MB"))
    }

    func testLowerBoundMarkerDoesNotRenderWhenFalse() {
        let row = CandidateRowViewModel(candidate(isLowerBound: false))
        XCTAssertFalse(row.reclaimableText.contains("\u{2265}"))
        XCTAssertEqual(row.reclaimableText, "1.0 MB")
    }

    // MARK: - HORO-1306: what a row says

    /// The row's primary line was the raw Rust enum tag. It is now the
    /// plain-language kind, with the tag still available verbatim on the
    /// term for anyone reading `--json` alongside the popover.
    func testRowLeadsWithPlainLanguageNotTheRustEnumTag() {
        let row = CandidateRowViewModel(candidate(isLowerBound: false, kind: "xcode_derived_data"))

        XCTAssertEqual(row.kindTerm.title, "Xcode derived data")
        XCTAssertFalse(row.kindTerm.title.contains("_"), "an enum tag leaked into the primary line")
        XCTAssertEqual(row.kindTerm.token, "xcode_derived_data", "the raw tag must stay available")
    }

    /// The safety verdict is the most important thing about a candidate and
    /// used to be invisible until you opened the detail sheet. It is now on
    /// the row, in words, and it is display copy for a verdict the CLI
    /// reached — the term carries the token it was handed, unchanged.
    func testRowCarriesTheSafetyVerdictInWords() {
        let safe = CandidateRowViewModel(candidate(isLowerBound: false, policyLabel: "AUTO_SAFE"))
        let protected = CandidateRowViewModel(candidate(isLowerBound: false, policyLabel: "PROTECTED"))

        XCTAssertEqual(safe.safetyTerm.token, "AUTO_SAFE")
        XCTAssertEqual(protected.safetyTerm.token, "PROTECTED")
        XCTAssertNotEqual(safe.safetyTerm.title, protected.safetyTerm.title)
        XCTAssertNotEqual(safe.safetyTerm.symbolName, protected.safetyTerm.symbolName)
        XCTAssertFalse(safe.safetyTerm.title.contains("_"))
        XCTAssertFalse(protected.safetyTerm.title.contains("_"))
    }

    /// This ticket's central semantic rule, asserted at the row level where
    /// a user actually compares candidates: size and safety are separate
    /// axes. A large AUTO_SAFE row is the best thing on the list and a small
    /// PROTECTED row is still untouchable, so the impact badge must be
    /// toned identically in both cases while the safety badge differs.
    func testSizeAndSafetyAreSeparateAxesOnTheRow() {
        let bigAndSafe = CandidateRowViewModel(
            candidate(isLowerBound: false, human: "48.2 GB", policyLabel: "AUTO_SAFE")
        )
        let smallAndProtected = CandidateRowViewModel(
            candidate(isLowerBound: false, human: "2 KB", policyLabel: "PROTECTED")
        )

        XCTAssertEqual(
            bigAndSafe.impactTerm.tone,
            smallAndProtected.impactTerm.tone,
            "a size changed its tone, which makes a number look like a safety claim"
        )
        XCTAssertEqual(bigAndSafe.impactTerm.tone, .neutral)
        XCTAssertNotEqual(bigAndSafe.safetyTerm.tone, smallAndProtected.safetyTerm.tone)

        // And the two axes never render as the same chip.
        XCTAssertNotEqual(bigAndSafe.impactTerm.axis, bigAndSafe.safetyTerm.axis)
        XCTAssertNotEqual(bigAndSafe.impactTerm.symbolName, bigAndSafe.safetyTerm.symbolName)
    }

    /// The row is one button, so VoiceOver reads one label. All three facts
    /// plus the path have to be in it — a row that reads only "Rust build
    /// output" tells a screen-reader user nothing they could act on.
    func testAccessibilityLabelCarriesEveryFactOnTheRow() {
        let row = CandidateRowViewModel(
            candidate(
                isLowerBound: true,
                kind: "node_modules",
                human: "310 MB",
                policyLabel: "ASK",
                resourceId: "/Users/dev/proj/node_modules"
            )
        )

        let label = row.accessibilityLabel
        XCTAssertTrue(label.contains(row.kindTerm.title), label)
        XCTAssertTrue(label.contains(row.safetyTerm.title), label)
        XCTAssertTrue(label.contains("310 MB"), label)
        XCTAssertTrue(label.contains("/Users/dev/proj/node_modules"), label)
        // The axis names are what stop the chips reading as a list of bare
        // adjectives with no subject.
        XCTAssertTrue(label.contains(GlomerisVocabulary.safetyAxis), label)
        XCTAssertTrue(label.contains(GlomerisVocabulary.impactAxis), label)
    }

    /// `detect --json` carries no `completeness`/`confidence` per candidate —
    /// those fields exist on the `explain` report only, which is what the
    /// detail sheet shows. So the row must not render an evidence-confidence
    /// badge: there is no evidence for it, and a chip built from something
    /// else would be a classification invented in Swift.
    ///
    /// Asserted on the source because the property is about what the view
    /// may not do, not about what one view-model instance happens to hold.
    func testRowInventsNoEvidenceConfidenceItWasNotGiven() throws {
        let source = try Self.readSource("CandidatesSectionView.swift")
        let code = source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        XCTAssertFalse(
            code.contains("GlomerisVocabulary.confidence"),
            "detect does not emit a per-candidate confidence; a row may not invent one"
        )
        XCTAssertFalse(
            code.contains("GlomerisVocabulary.completeness"),
            "detect does not emit a per-candidate completeness; a row may not invent one"
        )
    }

    // MARK: - Ranking stays Rust's (HORO-1307)

    /// Supersedes HORO-1306's blanket "this file contains no `.sorted`".
    ///
    /// That assertion was a proxy for the real rule — *ranking* is a judgment
    /// about what matters most and belongs in Rust — and it was written while
    /// HORO-1307 was still pending. HORO-1307 delivered that ranking
    /// (`reporting::ranking`, applied in `build_detect_report`), and with it a
    /// legitimate second ordering: an alphabetical index for finding a
    /// resource you already know the path of. That is a lookup aid, not a
    /// claim about importance, so the rule is narrowed rather than dropped:
    /// the default must be Rust's order byte-for-byte, and the only sort
    /// permitted here is the explicitly user-chosen one, keyed on
    /// `resourceId` and nothing else.
    ///
    /// Pinning it to `resourceId` is the part that matters. A sort on
    /// `reclaimableBytes` would be a re-implementation of Rust's ranking and
    /// would silently drift from its tie-break and lower-bound rules — `≥ 5
    /// GB` and an exact 5 GB are not interchangeable there.
    func testTheOnlySortingHereIsTheUserChosenPathIndex() throws {
        let source = try Self.readSource("CandidatesSectionView.swift")
        let code = source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        let sortLines = code
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { $0.contains(".sorted") }
        XCTAssertEqual(sortLines.count, 1, "exactly one sort is permitted in this view")
        XCTAssertTrue(
            sortLines.first?.contains("resourceId") == true,
            "the one permitted sort must be the alphabetical path index"
        )
        XCTAssertFalse(
            sortLines.first?.contains("reclaimableBytes") == true,
            "re-ranking by size in Swift would duplicate and then drift from reporting::ranking"
        )
    }

    /// The default state is Rust's order, so the list a user sees without
    /// touching anything is exactly what `detect` decided.
    ///
    /// HORO-1365 moved these two controls into `ScanState`, which upgraded this
    /// from a source grep to a real assertion: the defaults are now read off a
    /// freshly constructed store rather than matched as text in a declaration.
    func testDefaultOrderIsTheCliRanking() {
        let scan = ScanState()
        XCTAssertEqual(scan.sortOrder, .recommended, "the list must open in the CLI's own order")
        XCTAssertEqual(scan.safetyFilter, .all, "nothing may be hidden until the user asks for it")
    }

    /// Behavioural counterpart to the source checks above: `.recommended`
    /// passes the CLI's sequence through untouched even when that sequence is
    /// neither size- nor path-ordered as far as this layer can tell.
    func testRecommendedOrderPassesTheCliSequenceThroughUntouched() {
        let cliOrder = ["/z/first", "/a/second", "/m/third"]
        let input = cliOrder.map { candidate(resourceId: $0) }

        let shaped = CandidateListShaping.shape(input, filter: .all, order: .recommended)

        XCTAssertEqual(shaped.map(\.resourceId), cliOrder)
    }

    func testPathOrderIsAlphabeticalByResourceId() {
        let input = ["/z/first", "/a/second", "/m/third"].map { candidate(resourceId: $0) }

        let shaped = CandidateListShaping.shape(input, filter: .all, order: .path)

        XCTAssertEqual(shaped.map(\.resourceId), ["/a/second", "/m/third", "/z/first"])
    }

    // MARK: - Safety filter (HORO-1307)

    private var mixedPolicyCandidates: [DetectCandidateReportDto] {
        [
            candidate(policyLabel: "AUTO_SAFE", resourceId: "/a/safe"),
            candidate(policyLabel: "PROTECTED", resourceId: "/b/protected"),
            candidate(policyLabel: "ASK", resourceId: "/c/ask"),
            candidate(policyLabel: "UNKNOWN_INCOMPLETE", resourceId: "/d/unknown"),
        ]
    }

    func testEveryFilterKeepsExactlyItsOwnPolicyClass() {
        let expected: [(CandidateSafetyFilter, [String])] = [
            (.all, ["/a/safe", "/b/protected", "/c/ask", "/d/unknown"]),
            (.autoSafe, ["/a/safe"]),
            (.protected, ["/b/protected"]),
            (.ask, ["/c/ask"]),
            (.unknownIncomplete, ["/d/unknown"]),
        ]

        for (filter, ids) in expected {
            let shaped = CandidateListShaping.shape(
                mixedPolicyCandidates, filter: filter, order: .recommended
            )
            XCTAssertEqual(shaped.map(\.resourceId), ids, "filter \(filter.rawValue)")
        }
    }

    /// Every case must be reachable from the picker and every case must map
    /// to a token the Rust `PolicyLabel` actually emits — otherwise a filter
    /// exists that can only ever produce an empty list.
    func testEveryFilterCaseIsOfferedAndMapsToARealPolicyToken() {
        XCTAssertEqual(CandidateSafetyFilter.allCases.count, 5)
        XCTAssertEqual(CandidateSortOrder.allCases.count, 2)

        let realTokens = ["AUTO_SAFE", "ASK", "PROTECTED", "UNKNOWN_INCOMPLETE"]
        let mapped = CandidateSafetyFilter.allCases.compactMap(\.keptToken)
        XCTAssertEqual(mapped.sorted(), realTokens.sorted())
        XCTAssertNil(CandidateSafetyFilter.all.keptToken, "\"All\" must not filter")
    }

    /// The empty-filter message is built from the filter's name, so that name
    /// has to survive being dropped into a sentence. The picker labels do not:
    /// "Not enough evidence" yields "none are not enough evidence".
    func testEveryFilterNameReadsAsEnglishMidSentence() {
        for filter in CandidateSafetyFilter.allCases {
            let sentence = "Glomeris found 3 candidates, but none are "
                + "\(filter.midSentenceDescription)."
            XCTAssertFalse(
                sentence.contains("are not enough"),
                "\(filter.rawValue) produces a garbled sentence: \(sentence)"
            )
            XCTAssertFalse(filter.midSentenceDescription.isEmpty)
            // A sentence fragment, not a UI label: no capital to start it.
            XCTAssertEqual(
                filter.midSentenceDescription.first,
                filter.midSentenceDescription.first?.lowercased().first
            )
        }
    }

    /// Protection is not the same thing as invisibility: a user must be able
    /// to ask "what here is off-limits?" and get an answer.
    func testProtectedCandidatesAreFilterableRatherThanHidden() {
        let unfiltered = CandidateListShaping.shape(
            mixedPolicyCandidates, filter: .all, order: .recommended
        )
        XCTAssertTrue(unfiltered.contains { $0.policyLabel == "PROTECTED" })

        let onlyProtected = CandidateListShaping.shape(
            mixedPolicyCandidates, filter: .protected, order: .recommended
        )
        XCTAssertEqual(onlyProtected.count, 1)
    }

    /// Filtering must not reorder. Combined with `.recommended`, the kept
    /// subset stays in Rust's relative order.
    func testFilteringPreservesRelativeOrder() {
        let input = [
            candidate(policyLabel: "AUTO_SAFE", resourceId: "/z/big"),
            candidate(policyLabel: "PROTECTED", resourceId: "/m/mid"),
            candidate(policyLabel: "AUTO_SAFE", resourceId: "/a/small"),
        ]

        let shaped = CandidateListShaping.shape(input, filter: .autoSafe, order: .recommended)

        XCTAssertEqual(shaped.map(\.resourceId), ["/z/big", "/a/small"])
    }

    // MARK: - Storage-impact badge (HORO-1307)

    /// The third badge is an emphasis hint and must appear only when there is
    /// something to emphasise. A chip on every single row is noise, and
    /// `"unknown"` would just restate the size badge's own "Size unknown".
    func testImpactBadgeAppearsOnlyForNotableAndLargeCandidates() {
        XCTAssertNotNil(CandidateRowViewModel(candidate(impactTier: "large")).impactTierTerm)
        XCTAssertNotNil(CandidateRowViewModel(candidate(impactTier: "notable")).impactTierTerm)
        XCTAssertNil(CandidateRowViewModel(candidate(impactTier: "normal")).impactTierTerm)
        XCTAssertNil(CandidateRowViewModel(candidate(impactTier: "unknown")).impactTierTerm)
        // An older `glomeris` on PATH omits the field entirely.
        XCTAssertNil(CandidateRowViewModel(candidate(impactTier: nil)).impactTierTerm)
    }

    /// Size and safety are separate axes (HORO-1307 AC 4): a large candidate
    /// may be protected and a small one may be safe, so the impact badge must
    /// carry no safety tone of its own and must not vary with `policyLabel`.
    func testImpactBadgeCarriesNoSafetyMeaning() {
        let largeProtected = CandidateRowViewModel(
            candidate(policyLabel: "PROTECTED", impactTier: "large")
        )
        let largeSafe = CandidateRowViewModel(
            candidate(policyLabel: "AUTO_SAFE", impactTier: "large")
        )

        XCTAssertEqual(largeProtected.impactTierTerm, largeSafe.impactTierTerm)
        XCTAssertEqual(largeProtected.impactTierTerm?.tone, .neutral)
        XCTAssertNotEqual(
            largeProtected.safetyTerm, largeSafe.safetyTerm,
            "the safety badge, not the impact badge, is what differs between these two"
        )
    }

    /// A row is read aloud as size, then safety, then emphasis — so the
    /// emphasis is never the only way to learn a candidate is large.
    func testAccessibilityLabelNamesTheImpactTierWhenPresent() {
        let loud = CandidateRowViewModel(candidate(impactTier: "large"))
        let quiet = CandidateRowViewModel(candidate(impactTier: "normal"))

        XCTAssertTrue(loud.accessibilityLabel.contains("Biggest wins"))
        XCTAssertFalse(quiet.accessibilityLabel.contains("Biggest wins"))
        // The size itself is spoken either way: the badge adds emphasis, it
        // does not carry information nothing else carries.
        XCTAssertTrue(loud.accessibilityLabel.contains("1.0 MB"))
        XCTAssertTrue(quiet.accessibilityLabel.contains("1.0 MB"))
    }

    // MARK: - Helpers

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }
}
