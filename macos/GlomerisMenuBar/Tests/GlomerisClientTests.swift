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

/// Collects results produced on a non-test thread, so the hang test below
/// can run its workload off the cooperative pool and still assert on what
/// happened. `@unchecked Sendable` with an explicit lock rather than an
/// `actor`, because the assertions run synchronously after
/// `wait(for:timeout:)` returns and must not need `await` — an `await` in
/// the assertion would put the test back on the very pool whose starvation
/// it is trying to detect.
private final class InvocationLog: @unchecked Sendable {
    private let lock = NSLock()
    private var recordedExitCodes: [Int32] = []
    private var recordedFailures: [String] = []

    func record(exitCode: Int32) {
        lock.lock()
        recordedExitCodes.append(exitCode)
        lock.unlock()
    }

    func record(failure: String) {
        lock.lock()
        recordedFailures.append(failure)
        lock.unlock()
    }

    var exitCodes: [Int32] {
        lock.lock()
        defer { lock.unlock() }
        return recordedExitCodes
    }

    var failures: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recordedFailures
    }
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

    /// HORO-1304's deterministic guard, in the same mechanical style as the
    /// no-shell check above — and for the same reason: the property is
    /// absolute, and a source assertion holds on every run, whereas the bug
    /// it prevents only surfaced in about one run in six.
    ///
    /// `runRaw` is `async`, so its body executes on a Swift-concurrency
    /// cooperative thread. `Process.waitUntilExit()` blocks the calling
    /// thread, and with no `terminationHandler` installed Foundation reaps
    /// the child via the *launching* thread's run loop — which a cooperative
    /// thread never runs again. The wait then never ends: the child has long
    /// since exited, and nothing will ever tell this thread so. Awaiting
    /// `ExitStatusRelay`, fed by a handler installed before `run()`, both
    /// suspends instead of blocking and puts the reaping on a dispatch queue
    /// rather than a run loop.
    ///
    /// Asserted on the source rather than via a probe because there is no
    /// "safe" occurrence to allow: any `waitUntilExit` reachable from an
    /// `async` context reintroduces the defect, and a reviewer adding one
    /// deserves to be told exactly that rather than to be handed a flake.
    ///
    /// Comment lines are stripped before scanning, so the rule applies to
    /// code and the source stays free to name the banned call while
    /// explaining why it is banned — which the fix's own comment does, and
    /// which a naive whole-file `contains` check turned into a self-inflicted
    /// failure.
    func testSourceNeverBlocksAThreadWaitingForProcessExit() throws {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/GlomerisClient.swift")
        let wholeFile = try String(contentsOf: sourceURL, encoding: .utf8)
        let source = wholeFile
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")

        XCTAssertFalse(
            source.contains("waitUntilExit"),
            """
            GlomerisClient must never call Process.waitUntilExit(): it blocks \
            the calling thread, which in an async function is a Swift \
            concurrency cooperative thread, and it then never returns \
            (HORO-1304). Await ExitStatusRelay instead.
            """
        )
        XCTAssertTrue(
            source.contains("terminationHandler"),
            """
            The exit status must be delivered by a terminationHandler \
            installed before Process.run(); without one, Foundation reaps the \
            child through the launching thread's run loop, which a \
            cooperative thread never runs (HORO-1304).
            """
        )
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

    // MARK: - Executable resolution (HORO-1295)

    /// A directory containing a copy of the fixture helper named exactly
    /// `glomeris`, so a locator pointed at it resolves a genuinely spawnable
    /// binary using the real filesystem predicate. Returns the directory and
    /// a cleanup closure.
    private func installDirectoryContainingFixture() throws -> (String, () -> Void) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-resolve-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try FileManager.default.copyItem(
            at: Self.fixtureURL,
            to: directory.appendingPathComponent("glomeris")
        )
        return (directory.path, { try? FileManager.default.removeItem(at: directory) })
    }

    /// The whole point of the resolution change: the binary the locator picks
    /// is the binary that actually gets spawned. Asserted by round-tripping
    /// output through it rather than by reading a property back, so a client
    /// that resolved correctly but spawned something else would still fail.
    func testResolvedBinaryIsTheOneSpawned() async throws {
        let (directory, cleanup) = try installDirectoryContainingFixture()
        defer { cleanup() }

        let client = GlomerisClient(
            locator: GlomerisExecutableLocator(
                bundledExecutableURL: nil,
                pathVariable: nil,
                knownInstallDirectories: [directory]
            )
        )

        let result = try await client.run(
            ["0", "{\"value\":7}", ""],
            outputType: ValueOutput.self,
            progressType: EmptyProgress.self
        )

        XCTAssertEqual(result.output.value, 7)
    }

    /// Nothing installed anywhere must surface as its own error naming the
    /// searched locations — not as `executionFailed` with whatever text
    /// Foundation produces for a path the app invented.
    func testUnresolvableBinaryMapsToExecutableNotFound() async throws {
        let client = GlomerisClient(
            locator: GlomerisExecutableLocator(
                bundledExecutableURL: nil,
                pathVariable: "/usr/bin:/bin:/usr/sbin:/sbin",
                knownInstallDirectories: ["/no/such/prefix/bin"],
                isExecutableFile: { _ in false }
            )
        )

        do {
            _ = try await client.run([], outputType: ValueOutput.self, progressType: EmptyProgress.self)
            XCTFail("expected GlomerisClientError.executableNotFound")
        } catch GlomerisClientError.executableNotFound(let searched) {
            XCTAssertEqual(searched, ["the app bundle", "PATH", "/no/such/prefix/bin"])
        }
    }

    /// A pinned URL must bypass resolution even when it does not exist.
    /// Without this, the suite's missing-binary tests above would quietly
    /// start finding a real CLI on any host that has one installed, and
    /// would assert nothing.
    func testPinnedExecutableBypassesResolution() async throws {
        let (directory, cleanup) = try installDirectoryContainingFixture()
        defer { cleanup() }

        // Proves the fallback was available and still not taken: the same
        // directory resolves fine for a client that is not pinned.
        let locator = GlomerisExecutableLocator(
            bundledExecutableURL: nil,
            pathVariable: nil,
            knownInstallDirectories: [directory]
        )
        XCTAssertNotNil(locator.locate())

        let client = GlomerisClient(executableURL: URL(fileURLWithPath: "/no/such/glomeris-binary"))
        do {
            _ = try await client.run([], outputType: ValueOutput.self, progressType: EmptyProgress.self)
            XCTFail("expected GlomerisClientError.executionFailed")
        } catch GlomerisClientError.executionFailed {
            // expected — the pin was honoured, not replaced by a fallback
        }
    }

    /// Resolution happens per invocation, so a CLI installed while the app is
    /// already running is picked up at the next poll instead of after a
    /// restart. The client is built before the binary exists here, which is
    /// exactly the sequence a user performs.
    func testResolutionHappensPerInvocationNotAtConstruction() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-late-install-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        let client = GlomerisClient(
            locator: GlomerisExecutableLocator(
                bundledExecutableURL: nil,
                pathVariable: nil,
                knownInstallDirectories: [directory.path]
            )
        )

        do {
            _ = try await client.run([], outputType: ValueOutput.self, progressType: EmptyProgress.self)
            XCTFail("expected GlomerisClientError.executableNotFound before the CLI is installed")
        } catch GlomerisClientError.executableNotFound {
            // expected
        }

        try FileManager.default.copyItem(
            at: Self.fixtureURL,
            to: directory.appendingPathComponent("glomeris")
        )

        let result = try await client.run(
            ["0", "{\"value\":11}", ""],
            outputType: ValueOutput.self,
            progressType: EmptyProgress.self
        )
        XCTAssertEqual(result.output.value, 11)
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

    // MARK: - HORO-1297: the exact schema mismatch that broke the panel

    /// The client half of HORO-1297, pinned end to end.
    ///
    /// `daemon status --json` used to print `launchctl list`'s property
    /// dictionary ahead of its own report, because the Rust side built that
    /// child with `.status()` (inheriting stdout) instead of `.output()`.
    /// The Rust fix is pinned by `tests/daemon_status_stdout_purity.rs`; this
    /// test pins the *other* end — that such stdout is rejected as
    /// `outputDecodingFailed` and never silently coerced into a plausible
    /// `DaemonStatusReportDto`. The two together are what make the defect
    /// unable to return undetected: even if some future subcommand leaks a
    /// child's stdout again, the client refuses it loudly.
    ///
    /// The payload is the real observed shape — tab-indented, `=`-separated,
    /// `};`-terminated — followed by a *valid* report. A lenient decoder that
    /// scanned for the first `{` would find the launchctl dictionary; one that
    /// scanned for the last would find the real report and appear to work.
    /// Neither is acceptable: the contract is one JSON document on stdout.
    func testLaunchctlPollutedStdoutIsRejectedNotSalvaged() async throws {
        let polluted = """
        {
        \t"Label" = "com.glomeris.monitor";
        \t"LastExitStatus" = 0;
        \t"PID" = 4242;
        };
        {"plist_installed":true,"plist_path":"/Users/dev/Library/LaunchAgents/com.glomeris.monitor.plist","loaded":true,"heartbeat_age_secs":8}
        """
        do {
            _ = try await fixtureClient().run(
                ["0", polluted, ""],
                outputType: DaemonStatusReportDto.self,
                progressType: EmptyProgress.self
            )
            XCTFail("expected GlomerisClientError.outputDecodingFailed")
        } catch GlomerisClientError.outputDecodingFailed {
            // expected
        }
    }

    /// The control for the test above: the *fixed* CLI's stdout — the report
    /// alone — must decode, and every field must survive the round trip. A
    /// rejection test alone would also pass if the client rejected
    /// everything.
    func testCleanDaemonStatusStdoutDecodesWithEveryFieldIntact() async throws {
        let clean = #"{"plist_installed":true,"plist_path":"/Users/dev/Library/LaunchAgents/com.glomeris.monitor.plist","loaded":true,"heartbeat_age_secs":8}"#
        let result = try await fixtureClient().run(
            ["0", clean, ""],
            outputType: DaemonStatusReportDto.self,
            progressType: EmptyProgress.self
        )

        XCTAssertTrue(result.output.plistInstalled)
        XCTAssertTrue(result.output.loaded)
        XCTAssertEqual(result.output.heartbeatAgeSecs, 8)
        XCTAssertEqual(
            result.output.plistPath,
            "/Users/dev/Library/LaunchAgents/com.glomeris.monitor.plist"
        )
    }

    /// The never-polled state, which the CLI emits as an explicit `null`.
    /// `heartbeatAgeSecs` must decode as `nil` rather than failing — a
    /// decode failure here would show the panel HORO-1297's red error line
    /// for what is in fact a perfectly ordinary state.
    func testNullHeartbeatAgeDecodesAsNilRatherThanFailing() async throws {
        let clean = #"{"plist_installed":true,"plist_path":"/Users/dev/x.plist","loaded":false,"heartbeat_age_secs":null}"#
        let result = try await fixtureClient().run(
            ["0", clean, ""],
            outputType: DaemonStatusReportDto.self,
            progressType: EmptyProgress.self
        )

        XCTAssertNil(result.output.heartbeatAgeSecs)
        XCTAssertFalse(result.output.loaded)
        XCTAssertTrue(result.output.plistInstalled)
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

    // MARK: - HORO-1304: waiting for exit must never block a thread

    /// Concurrent-invocation coverage: many `runRaw` calls in flight at once
    /// must all return, each with its own child's exit code.
    ///
    /// Scope, stated honestly because it is easy to assume otherwise: this
    /// test does **not** reproduce HORO-1304. It was written to, and
    /// A/B-tested against the blocking implementation three ways — 60
    /// sequential spawns, then 72 concurrent ones, then the same with no
    /// `terminationHandler` installed at all. All three passed while the bug
    /// was present. The hang needs whatever ordering the full class produces
    /// (it surfaced in about one run in six, in a different test each time),
    /// and no synthetic workload found here reproduces it on demand. The
    /// deterministic guard for the defect is
    /// `testSourceNeverBlocksAThreadWaitingForProcessExit` above; the
    /// mechanism that replaced the block is pinned by the `ExitStatusRelay`
    /// tests below.
    ///
    /// What this test does add is genuine: the app fires its popover fetches
    /// concurrently, nothing else here exercises more than one invocation at
    /// a time, and it verifies exit codes are not cross-wired between
    /// simultaneous children. It also runs the workload on `Task.detached`
    /// and waits on an `XCTestExpectation`, so the test thread is a real
    /// thread on a run loop: if a future regression does wedge the
    /// cooperative pool, this fails on a deadline instead of wedging
    /// `xcodebuild` until the CI job's own limit — which is exactly why the
    /// original hang read as an infrastructure timeout rather than a test
    /// failure.
    func testConcurrentRunRawInvocationsAllReturnWithoutBlockingTheCooperativePool() {
        // Deliberately wider than the cooperative pool on any machine this
        // runs on: the pool is sized to the active core count, so exceeding
        // it is what turns "a blocked thread" into "no thread left to make
        // progress".
        let concurrency = max(24, ProcessInfo.processInfo.activeProcessorCount * 2)
        let rounds = 3
        let expected = concurrency * rounds
        let log = InvocationLog()
        let client = fixtureClient()
        let finished = expectation(description: "\(expected) concurrent runRaw invocations return")

        Task.detached {
            for round in 0..<rounds {
                await withTaskGroup(of: Void.self) { group in
                    for slot in 0..<concurrency {
                        group.addTask {
                            let exitCode = Int32(slot % 7)
                            do {
                                let result = try await client.runRaw(
                                    [String(exitCode), #"{"value": 1}"#, "{\"percent\": 3}\n"],
                                    progressType: PercentProgress.self
                                )
                                log.record(exitCode: result.exitCode)
                            } catch {
                                log.record(failure: "round \(round) slot \(slot) threw: \(error)")
                            }
                        }
                    }
                }
            }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 90)

        XCTAssertEqual(log.failures, [])
        XCTAssertEqual(
            log.exitCodes.count,
            expected,
            "every concurrent invocation must return, and report its own child's exit code"
        )
        XCTAssertEqual(
            log.exitCodes.sorted(),
            (0..<expected).map { Int32(($0 % concurrency) % 7) }.sorted(),
            "exit codes must not be cross-wired between concurrent invocations"
        )
    }

    /// The exit status is now delivered by a `terminationHandler` installed
    /// *before* `Process.run()`, because Foundation will not invoke a handler
    /// attached to an already-terminated process. That creates two possible
    /// orderings, and `ExitStatusRelay` exists to make both safe. This is
    /// the one that used to be a race in every hand-rolled version of this
    /// pattern: the child is fast, exits before anybody awaits, and the
    /// status must already be sitting in the relay.
    func testExitStatusRelayDeliversAStatusThatArrivedBeforeAnyoneWaited() async {
        let relay = ExitStatusRelay()
        relay.complete(5)

        let status = await relay.wait()

        XCTAssertEqual(status, 5)
    }

    /// The other ordering: somebody is already suspended when the child
    /// exits. This is the path that must resume the continuation rather
    /// than leave it parked forever — i.e. the actual fix for the hang.
    func testExitStatusRelayDeliversAStatusThatArrivesAfterTheWaitBegins() async {
        let relay = ExitStatusRelay()
        Task.detached {
            try? await Task.sleep(nanoseconds: 20_000_000)
            relay.complete(7)
        }

        let status = await relay.wait()

        XCTAssertEqual(status, 7)
    }

    /// Resuming a `CheckedContinuation` twice traps. The relay is one-shot
    /// so that a duplicate termination callback — or a `wait` racing a
    /// second `complete` — degrades to "first status wins" instead of
    /// crashing the app.
    func testExitStatusRelayIsOneShotSoARepeatedCompletionCannotTrap() async {
        let relay = ExitStatusRelay()
        relay.complete(3)
        relay.complete(9)

        let first = await relay.wait()
        // Waiting again is also safe: the status is retained, not consumed.
        let second = await relay.wait()

        XCTAssertEqual(first, 3, "the first completion wins")
        XCTAssertEqual(second, 3)
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
