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

        let detectOccurrences = source.components(separatedBy: "\"detect\"").count - 1
        XCTAssertEqual(detectOccurrences, 1, "detect must be constructed in exactly one place")
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

    private func candidate(isLowerBound: Bool) -> DetectCandidateReportDto {
        DetectCandidateReportDto(
            resourceId: "/tmp/example/target",
            kind: "cargo_target",
            reclaimableBytes: 1_048_576,
            reclaimableHuman: "1.0 MB",
            reclaimableBytesIsLowerBound: isLowerBound,
            policyLabel: "AUTO_SAFE",
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

    // MARK: - Helpers

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }
}
