//
//  GlomerisClientTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1060: GlomerisClient spawn wrapper tests.
//

import XCTest

// The test target compiles GlomerisClient.swift directly as one of its own
// sources (see project.yml) rather than importing the app module, since
// GlomerisMenuBar is an `LSUIElement` app target with no importable
// framework product.

private struct EmptyProgress: Decodable {}

private struct ValueOutput: Decodable {
    let value: Int
}

private struct PercentProgress: Decodable {
    let percent: Int
}

private struct DetectCandidateProbe: Decodable {}

private struct DetectReportProbe: Decodable {
    let candidates: [DetectCandidateProbe]
}

final class GlomerisClientTests: XCTestCase {
    // MARK: - Shared fixture executable

    /// One compiled Mach-O helper (`Fixtures/glomeris_fixture_helper.c`),
    /// built once and reused by every fixture-based test below — never a
    /// fresh file per test, and a compiled binary rather than a
    /// `#!/bin/sh` script. On the machine this was authored on, directly
    /// executing a freshly written `#!/bin/sh` script via
    /// `Process.executableURL` never returned (observed hung for 5+
    /// minutes), while `/bin/sh <script>` and a freshly compiled binary
    /// both ran instantly — root cause not established, but a single
    /// shared compiled fixture sidesteps it. It's still an ordinary
    /// executable spawned via `Process.executableURL` + `arguments`,
    /// exactly like the real `glomeris` binary would be.
    private static let fixtureURL: URL = {
        let binaryURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-client-fixture-helper")
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .appendingPathComponent("Fixtures/glomeris_fixture_helper.c")

        if !FileManager.default.fileExists(atPath: binaryURL.path) {
            let clang = Process()
            clang.executableURL = URL(fileURLWithPath: "/usr/bin/clang")
            clang.arguments = ["-O0", "-o", binaryURL.path, sourceURL.path]
            try? clang.run()
            clang.waitUntilExit()
        }
        return binaryURL
    }()

    private func fixtureClient() -> GlomerisClient {
        GlomerisClient(executableURL: Self.fixtureURL)
    }

    // MARK: - Mechanical no-shell check

    func testSourceContainsNoShellExecution() throws {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/GlomerisClient.swift")
        let source = try String(contentsOf: sourceURL, encoding: .utf8)

        XCTAssertFalse(source.contains("/bin/sh"), "GlomerisClient must never shell out via /bin/sh")
        XCTAssertFalse(source.contains(".launchPath"), "GlomerisClient must never use Process.launchPath")
    }

    // MARK: - Success path

    func testDecodesStdoutJSONAndStderrNDJSONProgress() async throws {
        let result = try await fixtureClient().run(
            ["0", #"{"value": 42}"#, "{\"percent\": 10}\n{\"percent\": 50}\n"],
            outputType: ValueOutput.self,
            progressType: PercentProgress.self
        )

        XCTAssertEqual(result.output.value, 42)
        XCTAssertEqual(result.progressLines.map(\.percent), [10, 50])
    }

    func testNoStderrYieldsEmptyProgressLines() async throws {
        let result = try await fixtureClient().run(
            ["0", #"{"value": 1}"#, ""],
            outputType: ValueOutput.self,
            progressType: EmptyProgress.self
        )

        XCTAssertEqual(result.output.value, 1)
        XCTAssertTrue(result.progressLines.isEmpty)
    }

    // MARK: - Typed exit-code errors

    func testExitCodeOneMapsToExecutionFailed() async throws {
        do {
            _ = try await fixtureClient().run(
                ["1", "", "boom"],
                outputType: ValueOutput.self,
                progressType: EmptyProgress.self
            )
            XCTFail("expected GlomerisClientError.executionFailed")
        } catch GlomerisClientError.executionFailed(let message) {
            XCTAssertEqual(message, "boom")
        }
    }

    func testExitCodeTwoMapsToUsage() async throws {
        do {
            _ = try await fixtureClient().run(
                ["2", "", "usage: fake [--flag]"],
                outputType: ValueOutput.self,
                progressType: EmptyProgress.self
            )
            XCTFail("expected GlomerisClientError.usage")
        } catch GlomerisClientError.usage(let message) {
            XCTAssertEqual(message, "usage: fake [--flag]")
        }
    }

    func testUnrecognizedExitCodeMapsToUnexpectedExitCode() async throws {
        do {
            _ = try await fixtureClient().run(
                ["17", "", ""],
                outputType: ValueOutput.self,
                progressType: EmptyProgress.self
            )
            XCTFail("expected GlomerisClientError.unexpectedExitCode")
        } catch GlomerisClientError.unexpectedExitCode(let code) {
            XCTAssertEqual(code, 17)
        }
    }

    func testMissingBinaryMapsToExecutionFailed() async throws {
        let client = GlomerisClient(executableURL: URL(fileURLWithPath: "/no/such/glomeris-binary"))
        do {
            _ = try await client.run([], outputType: ValueOutput.self, progressType: EmptyProgress.self)
            XCTFail("expected GlomerisClientError.executionFailed")
        } catch GlomerisClientError.executionFailed {
            // expected
        }
    }

    func testMalformedStdoutMapsToOutputDecodingFailed() async throws {
        do {
            _ = try await fixtureClient().run(
                ["0", "not json", ""],
                outputType: ValueOutput.self,
                progressType: EmptyProgress.self
            )
            XCTFail("expected GlomerisClientError.outputDecodingFailed")
        } catch GlomerisClientError.outputDecodingFailed {
            // expected
        }
    }

    // MARK: - runRaw (HORO-1065): no exit-code interpretation at all

    /// `execute`'s JSON body appears on stdout for most non-zero exit
    /// codes too (see `book/src/cli_reference.md`) — `runRaw` must hand
    /// that back rather than throwing it away, unlike `run` above.
    func testRunRawReturnsStdoutAndExitCodeForNonZeroExit() async throws {
        let result = try await fixtureClient().runRaw(
            ["5", #"{"reason": "resource_not_found", "message": "no candidate"}"#, "diagnostic text"],
            progressType: EmptyProgress.self
        )

        XCTAssertEqual(result.exitCode, 5)
        XCTAssertEqual(String(data: result.stdout, encoding: .utf8), #"{"reason": "resource_not_found", "message": "no candidate"}"#)
        XCTAssertEqual(String(data: result.stderr, encoding: .utf8), "diagnostic text")
    }

    func testRunRawNeverThrowsForAnyExitCode() async throws {
        for exitCode in [0, 1, 2, 3, 4, 5, 75, 17] {
            let result = try await fixtureClient().runRaw(
                [String(exitCode), "", ""],
                progressType: EmptyProgress.self
            )
            XCTAssertEqual(result.exitCode, Int32(exitCode))
        }
    }

    func testRunRawStillThrowsWhenBinaryCannotBeSpawned() async throws {
        let client = GlomerisClient(executableURL: URL(fileURLWithPath: "/no/such/glomeris-binary"))
        do {
            _ = try await client.runRaw([], progressType: EmptyProgress.self)
            XCTFail("expected GlomerisClientError.executionFailed")
        } catch GlomerisClientError.executionFailed {
            // expected
        }
    }

    func testRunRawStreamsLiveProgressSameAsRun() async throws {
        let result = try await fixtureClient().runRaw(
            ["0", "", "{\"percent\": 10}\n{\"percent\": 50}\n"],
            progressType: PercentProgress.self,
            onProgress: { _ in }
        )

        XCTAssertEqual(result.exitCode, 0)
        XCTAssertEqual(result.progressLines.map(\.percent), [10, 50])
    }

    // MARK: - Real binary integration

    /// Spawns the actual `glomeris` binary (built from this same worktree
    /// via `cargo build`) and confirms `glomeris detect --json`'s stdout
    /// decodes as generic JSON through GlomerisClient. Skips rather than
    /// fails if the binary hasn't been built, so this test doesn't force
    /// every environment to carry a Rust toolchain.
    ///
    /// `detect`'s discovery walks a handful of well-known locations under
    /// `$HOME` (Xcode DerivedData, Docker build cache, etc.) regardless of
    /// `--project-root` — on a real developer machine those can be huge,
    /// so this test points `HOME` at an empty scratch directory to keep
    /// the invocation fast and deterministic rather than scanning the
    /// tester's actual home directory.
    func testRealGlomerisDetectJSONDecodes() async throws {
        let repoRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
        let candidateBinaryURLs = [
            repoRoot.appendingPathComponent("target/debug/glomeris"),
            URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".cargo/shared-target/debug/glomeris"),
        ]
        guard let binaryURL = candidateBinaryURLs.first(where: {
            FileManager.default.isExecutableFile(atPath: $0.path)
        }) else {
            throw XCTSkip("glomeris binary not built — run `cargo build` first")
        }

        let scratchHome = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-client-fake-home-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: scratchHome, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: scratchHome) }

        var environment = ProcessInfo.processInfo.environment
        environment["HOME"] = scratchHome.path

        let client = GlomerisClient(executableURL: binaryURL, environment: environment)
        let result = try await client.run(
            ["detect", "--json"],
            outputType: DetectReportProbe.self,
            progressType: EmptyProgress.self
        )

        // The real assertion here is "this decoded at all" — `try await`
        // above already throws if stdout doesn't decode as
        // `DetectReportProbe`. `progressLines` being empty confirms the
        // NDJSON stderr path degrades cleanly with no progress output.
        XCTAssertTrue(result.progressLines.isEmpty)
    }
}
