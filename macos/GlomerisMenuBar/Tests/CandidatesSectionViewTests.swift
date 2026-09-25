//
//  CandidatesSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1063: candidates list — cached snapshot + Refresh + live progress,
//  never a timer.
//

import XCTest

/// Collects the events a `@Sendable` progress callback reports, so the live
/// progress test can assert on them afterwards without mutating a captured
/// `var` from inside the callback.
///
/// The callback parameter on `GlomerisClient.run` is `@Sendable`, which is the
/// client's statement that it may invoke the closure from a context other than
/// the caller's — and `spawnAndDrain` does. Appending straight into a local
/// `var` therefore had no declared ordering against the test thread that reads
/// the array once the `await` returns; it passed, which is what an
/// unsynchronised access usually does. Swift 6 rejects it outright
/// (HORO-1478), and this was the only site in the target that had it.
///
/// `@unchecked Sendable` with an explicit lock rather than an `actor`, for the
/// same reason `InvocationLog` in `GlomerisClientTests` is one: the assertions
/// run synchronously after the `await` and must not themselves need an
/// `await`.
private final class ProgressEventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [ProgressEventDto] = []

    func record(_ event: ProgressEventDto) {
        lock.lock()
        recorded.append(event)
        lock.unlock()
    }

    var events: [ProgressEventDto] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }
}

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
        // Comment-stripped since HORO-1365: the function's doc comment now
        // quotes this very call shape while explaining what its `@MainActor`
        // annotation guarantees, and prose about a call site is not one.
        let source = try Self.strippedOfComments(Self.readSource("CandidatesSectionView.swift"))
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
            // Versioned, so this really is the binary GlomerisClientTests
            // builds. It used to be the unsuffixed name, which meant this test
            // silently kept its own never-rebuilt copy — the exact staleness
            // the revision suffix exists to prevent (HORO-1365).
            .appendingPathComponent("glomeris-client-fixture-helper-v4")
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
        let seen = ProgressEventLog()
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
                seen.record(event)
            }
        )

        let seenEvents = seen.events
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
        impactTier: String? = nil,
        executable: Bool = true,
        offeredActions: [OfferedActionDto] = [],
        refusalReason: String? = nil
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
            executable: executable,
            offeredActions: offeredActions,
            refusalReason: refusalReason
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

    // MARK: - HORO-1323: telling non-deletable rows apart from the overview
    //
    // AC 1 of the ticket is that a user can identify which candidates cannot
    // be cleaned without pressing Clean on each one. That makes this an
    // overview-level property, not a detail-sheet one.

    /// The badge is the visible half of AC 1.
    func testNonExecutableRowCarriesAnActionabilityBadge() {
        let row = CandidateRowViewModel(
            candidate(
                executable: false,
                refusalReason: "no registered cleanup action for this resource kind"
            )
        )
        XCTAssertEqual(row.actionabilityTerm?.title, "Cannot be cleaned")
        XCTAssertEqual(row.actionabilityTerm?.tone, .guarded)
    }

    /// And it appears only where it says something. Following `impactTier`'s
    /// precedent: a chip on every row is noise, and on a cleanable row the
    /// safety badge already reads "Safe to reclaim" or "Asks first".
    func testExecutableRowsCarryNoActionabilityBadge() {
        XCTAssertNil(
            CandidateRowViewModel(
                candidate(
                    executable: true,
                    offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
                )
            ).actionabilityTerm
        )
        XCTAssertNil(
            CandidateRowViewModel(
                candidate(
                    policyLabel: "ASK",
                    executable: true,
                    offeredActions: [OfferedActionDto(actionId: "node.clean.node_modules", requiresConfirmation: true)]
                )
            ).actionabilityTerm
        )
    }

    /// `executor::structural_refusal`'s own words, verbatim from
    /// `src/executor/mod.rs`. One constant rather than a literal per test: this
    /// string was transcribed with a hyphen where Rust has an em dash (U+2014),
    /// and the correction had to be applied in three separate files. A fixture
    /// that claims to be verbatim should exist once.
    private static let structuralRefusal =
        "refusing to run brew: this step has no scoped_path, so its identity cannot be "
        + "revalidated before mutation — an unscoped mutating action is never executed "
        + "regardless of policy class"

    /// The case HORO-1358 made real, and the reason this is a separate axis
    /// rather than a restatement of the safety badge: the Homebrew cache is
    /// classified AUTO_SAFE and can still never be executed. A row that reads
    /// only "Safe to reclaim" is, for that resource, actively misleading.
    func testAnAutoSafeRowCanStillBeMarkedNonExecutable() {
        let row = CandidateRowViewModel(
            candidate(
                kind: "homebrew_cache",
                policyLabel: "AUTO_SAFE",
                executable: false,
                refusalReason: Self.structuralRefusal
            )
        )
        XCTAssertNotNil(row.actionabilityTerm)
        XCTAssertNotEqual(
            row.actionabilityTerm, row.safetyTerm,
            "the two badges are different axes and must not collapse into one"
        )
        XCTAssertEqual(row.safetyTerm.token, "AUTO_SAFE")
    }

    /// The badge is not the only carrier: a PROTECTED row and a structurally
    /// refused row share the badge title, so the reason has to come from the
    /// CLI's own words.
    func testRowExplanationIsTheCLIsOwnRefusalText() {
        let refusal = "PROTECTED: protected_credential_material"
        let row = CandidateRowViewModel(
            candidate(policyLabel: "PROTECTED", executable: false, refusalReason: refusal)
        )
        XCTAssertEqual(row.actionabilityTerm?.explanation, refusal)
    }

    /// Unlike the chip, the spoken label carries the actionability state on
    /// every row. A sighted user compares rows and reads absence as "this one
    /// is fine"; a VoiceOver user hears one row at a time with nothing to
    /// compare it against, so absence conveys nothing to them.
    func testAccessibilityLabelAlwaysNamesTheActionabilityState() {
        let refusal = "no registered cleanup action for this resource kind"
        let refused = CandidateRowViewModel(candidate(executable: false, refusalReason: refusal))
        let cleanable = CandidateRowViewModel(
            candidate(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
            )
        )
        let asks = CandidateRowViewModel(
            candidate(
                policyLabel: "ASK",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "node.clean.node_modules", requiresConfirmation: true)]
            )
        )

        XCTAssertTrue(refused.accessibilityLabel.contains(refusal), refused.accessibilityLabel)
        XCTAssertTrue(refused.accessibilityLabel.contains("Cleanup:"), refused.accessibilityLabel)
        XCTAssertTrue(cleanable.accessibilityLabel.contains("Cleanup:"), cleanable.accessibilityLabel)
        // Allowed, needs-confirmation and refused are three states the ticket
        // says must not collapse — including for a listener.
        XCTAssertNotEqual(cleanable.accessibilityLabel, asks.accessibilityLabel)
        XCTAssertNotEqual(cleanable.accessibilityLabel, refused.accessibilityLabel)
    }

    /// The path stays last in the spoken label. It is the longest and least
    /// scannable part, and the state has to arrive before a listener decides
    /// whether to keep listening.
    func testActionabilityIsSpokenBeforeThePath() throws {
        let label = CandidateRowViewModel(
            candidate(executable: false, refusalReason: "no registered cleanup action for this resource kind")
        ).accessibilityLabel

        let cleanup = try XCTUnwrap(label.range(of: "Cleanup:"), label)
        let path = try XCTUnwrap(label.range(of: "Path:"), label)
        XCTAssertTrue(cleanup.lowerBound < path.lowerBound, label)
    }

    /// Every clause in the label ends in a period so VoiceOver pauses between
    /// them. The CLI's refusal strings carry no trailing period of their own,
    /// so the refused cases are the ones that need supplying — otherwise the
    /// reason runs straight into "Path:" as one breathless clause.
    ///
    /// Checks all five states, since only two of them come from the CLI and it
    /// is the CLI's that lack the period.
    func testEveryActionabilityClauseIsTerminatedBeforeThePath() throws {
        let cases: [(String, DetectCandidateReportDto)] = [
            ("refused, no action", candidate(
                executable: false,
                refusalReason: "no registered cleanup action for this resource kind"
            )),
            ("refused, structural", candidate(
                executable: false,
                refusalReason: Self.structuralRefusal
            )),
            ("refused, protected", candidate(
                policyLabel: "PROTECTED",
                executable: false,
                refusalReason: "PROTECTED: protected_infra_state"
            )),
            ("refused, no reason given", candidate(executable: false, refusalReason: nil)),
            ("ready", candidate(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: false)]
            )),
            ("asks first", candidate(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: true)]
            )),
        ]

        for (name, dto) in cases {
            let label = CandidateRowViewModel(dto).accessibilityLabel
            let cleanup = try XCTUnwrap(label.range(of: "Cleanup:"), label)
            let path = try XCTUnwrap(label.range(of: " Path:"), label)
            let clause = String(label[cleanup.upperBound..<path.lowerBound])
            XCTAssertTrue(
                clause.hasSuffix("."),
                "\(name): the actionability clause runs into the path — \(clause)"
            )
            XCTAssertFalse(clause.hasSuffix(".."), "\(name): doubled period — \(clause)")
        }
    }

    /// HORO-1451. The two tests above check the cleanup clause's ending; this
    /// one checks the whole label, because this row's termination rule moved out
    /// to `SpokenLabel` and "the clause still ends in a period" would also hold
    /// of a composer that had quietly changed something else — a dropped axis, a
    /// doubled space where the impact tier used to be, a reordered clause.
    ///
    /// Written out by hand rather than assembled from `row`'s own terms: a test
    /// that rebuilt the label the way the implementation does would agree with
    /// any implementation.
    func testTheWholeRowLabelIsSpokenAsSentences() {
        let refused = CandidateRowViewModel(
            candidate(
                kind: "node_modules",
                human: "310 MB",
                policyLabel: "PROTECTED",
                resourceId: "/Users/dev/proj/node_modules",
                impactTier: "large",
                executable: false,
                refusalReason: "PROTECTED: protected_user_documents"
            )
        )
        XCTAssertEqual(
            refused.accessibilityLabel,
            "node_modules. Safety: Protected. Storage impact: 310 MB. Biggest wins. "
                + "Cleanup: PROTECTED: protected_user_documents. "
                + "Path: /Users/dev/proj/node_modules."
        )

        // The same row without the optional tier, and with a cleanup sentence
        // that already ends in a period — the two ways the label can change
        // shape, neither of which may alter its punctuation.
        let cleanable = CandidateRowViewModel(
            candidate(
                kind: "cargo_target_dir",
                human: "2.0 GB",
                resourceId: "/Users/dev/proj/target",
                executable: true,
                offeredActions: [
                    OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false),
                ]
            )
        )
        XCTAssertEqual(
            cleanable.accessibilityLabel,
            "Rust build output. Safety: Safe to reclaim. Storage impact: 2.0 GB. "
                + "Cleanup: Glomeris is willing to run this. Open the row to review it and clean. "
                + "Path: /Users/dev/proj/target."
        )
        XCTAssertFalse(cleanable.accessibilityLabel.contains(".."), cleanable.accessibilityLabel)
    }

    /// The row renders the badge but must not gain an enablement decision from
    /// it. This file's only `.disabled(...)` is the Refresh button's, on
    /// `scan.isScanning`, and it stays the only one: nothing in the candidate
    /// list gates on actionability, because the overview's job here is to say
    /// what is true, and the one control that acts on it lives in the detail
    /// sheet where `executable` is read directly.
    func testTheRowBadgeAddsNoEnablementDecision() throws {
        let source = Self.strippedOfComments(try Self.readSource("CandidatesSectionView.swift"))
        XCTAssertTrue(source.contains("GlomerisBadgeView(term: actionabilityTerm)"))

        let disabledModifiers = source.components(separatedBy: ".disabled(").dropFirst()
        XCTAssertEqual(disabledModifiers.count, 1, "a new .disabled appeared in the candidate list")
        XCTAssertTrue(disabledModifiers.first?.hasPrefix("scan.isScanning)") == true)

        // And the list never branches on the wording. The previous version of
        // this checked for `actionability.sentence ==` and `actionabilityTerm ==`,
        // spellings nobody would write; these are the ones that would actually
        // appear if someone rebuilt a decision here.
        for branch in [
            "actionability ==", "actionabilityTerm ==", "switch row.actionability",
            "switch actionability", ".readyToClean", ".asksFirstThenCleans", ".refused",
        ] {
            XCTAssertFalse(
                source.contains(branch),
                "\(branch): the candidate list is branching on actionability wording"
            )
        }
    }

    // MARK: - HORO-1363: the row stays actionable, not merely readable

    /// A candidate row must keep the `AXButton` role and the `AXPress` action
    /// that `Button` gives it.
    ///
    /// `.accessibilityElement(children: .ignore)` was on this row until
    /// HORO-1363 and looked harmless — the row already had an explicit label,
    /// so "ignore the children" read as a tidy-up. What it actually does is
    /// substitute a plain container element for the button, and the live
    /// accessibility tree showed the consequence: the row came back as
    /// `AXUnknown` with an empty actions array. VoiceOver could read every
    /// fact on the row and could not open it, so the evidence behind a
    /// deletion was reachable only by sighted click.
    ///
    /// Asserted as an absence inside a window on `rowView` alone, because the
    /// modifier is correct elsewhere in this app (badge groups, history rows)
    /// and a file-wide ban would be wrong.
    func testTheCandidateRowRemainsAPressableButton() throws {
        let body = try Self.rowViewBody(in: Self.strippedOfComments(try Self.readSource("CandidatesSectionView.swift")))

        XCTAssertTrue(
            body.contains("Button {"),
            "the row must stay a Button — that is where AXPress comes from"
        )
        XCTAssertFalse(
            Self.ignoresItsChildren(body),
            "HORO-1363: .accessibilityElement(children: .ignore) is back on the candidate row, "
                + "which costs it the AXButton role and the AXPress action"
        )

        // Positive controls, so the test cannot pass because the row lost its
        // accessibility treatment altogether: the label the modifier was
        // supposed to be helping install is installed without it, and the hint
        // still describes what pressing does.
        XCTAssertTrue(body.contains(".accessibilityLabel(row.accessibilityLabel)"))
        XCTAssertTrue(body.contains(".accessibilityHint("))

        // And the window really is a window: `runDetect` is the next thing in
        // the file after `rowView`, so its absence proves the search above did
        // not quietly scan the rest of the source.
        XCTAssertFalse(body.contains("func runDetect"), "the rowView window over-ran its function")
    }

    /// Anti-vacuity for the test above. Splices the modifier back in at the
    /// place it used to sit and asserts the same predicate then trips, so a
    /// future rename of `rowView` or of the modifier cannot turn that guard
    /// into an assertion about nothing.
    func testThePressableRowGuardWouldCatchTheModifierReturning() throws {
        let body = try Self.rowViewBody(in: Self.strippedOfComments(try Self.readSource("CandidatesSectionView.swift")))
        let regressed = body.replacingOccurrences(
            of: ".buttonStyle(.plain)",
            with: ".buttonStyle(.plain)\n        .accessibilityElement(children: .ignore)"
        )
        XCTAssertNotEqual(regressed, body, "no .buttonStyle(.plain) to splice onto — the row changed shape")
        XCTAssertTrue(
            Self.ignoresItsChildren(regressed),
            "the guard would not notice the modifier returning"
        )
    }

    // MARK: - Helpers

    /// `rowView`'s body, so an accessibility assertion cannot be satisfied —
    /// or broken — by a modifier on some other view in the same file.
    ///
    /// The closing brace is matched at the function's own indentation, which
    /// no brace inside the body shares.
    private static func rowViewBody(in source: String) throws -> String {
        let signature = try XCTUnwrap(
            source.range(of: "private func rowView("),
            "no `private func rowView(` — renamed, or no longer a function"
        )
        let rest = source[signature.upperBound...]
        let end = try XCTUnwrap(rest.range(of: "\n    }\n"), "could not find the end of rowView")
        return String(rest[..<end.upperBound])
    }

    private static func ignoresItsChildren(_ body: String) -> Bool {
        body.contains(".accessibilityElement(children: .ignore)")
    }

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }

    /// Strips whole-line `//` comments, matching
    /// `CandidateDetailViewTests.strippedOfComments` — see that doc comment for
    /// why a conservative line filter is preferred over a real comment parser.
    ///
    /// Added in HORO-1365 because it was needed: the view's doc comment now
    /// quotes its own call site (`Task { await runDetect() }`) while explaining
    /// what the `@MainActor` annotation does and does not guarantee, and
    /// `testRunDetectIsOnlyInvokedFromTheRefreshButtonAction` counted that
    /// sentence as a third call site. A guard that a truthful comment can break
    /// pressures the next person to write a less truthful comment.
    private static func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }
}
