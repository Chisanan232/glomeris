//
//  GlomerisQAFixtureCliTests.swift
//  GlomerisMenuBarTests
//
//  HORO-2176: the DEV-QA synthetic-candidate stand-in, checked against the
//  app's real DTOs and for the property it exists to have — no authority.
//

import XCTest

final class GlomerisQAFixtureCliTests: XCTestCase {
    private static let script = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .appendingPathComponent("QAFixtures/glomeris-qa-fixture-cli.py")

    private struct Run {
        let status: Int32
        let stdout: Data
        let stderr: String
    }

    private func run(_ args: [String], count: Int = 0, stateDir: URL? = nil) throws -> Run {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["python3", "-I", Self.script.path] + args
        var env = ["PATH": "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin", "GLOMERIS_QA_COUNT": String(count)]
        if let stateDir { env["GLOMERIS_QA_STATE_DIR"] = stateDir.path }
        process.environment = env
        let out = Pipe(), err = Pipe()
        process.standardOutput = out
        process.standardError = err
        try process.run()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        let errData = err.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return Run(status: process.terminationStatus, stdout: data, stderr: String(decoding: errData, as: UTF8.self))
    }

    private func tempDir() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("qa-fixture-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return dir
    }

    private func detect(_ count: Int, stateDir: URL? = nil) throws -> DetectReportDto {
        let result = try run(["detect", "--json"], count: count, stateDir: stateDir)
        XCTAssertEqual(result.status, 0, result.stderr)
        return try JSONDecoder().decode(DetectReportDto.self, from: result.stdout)
    }

    // MARK: - The seven required scales

    func testEveryRequiredScaleDecodesThroughTheRealDtoWithStableUniqueIds() throws {
        for n in [0, 1, 5, 25, 220, 1_000, 5_000] {
            let report = try detect(n)
            XCTAssertEqual(report.candidates.count, n, "n=\(n)")
            let ids = report.candidates.map(\.resourceId)
            XCTAssertEqual(Set(ids).count, n, "n=\(n): ids must be unique")
            XCTAssertTrue(ids.allSatisfy { $0.hasPrefix("devqa-synthetic:") }, "n=\(n): every id is visibly synthetic")
            // Stable: a second run is byte-for-byte the same list.
            XCTAssertEqual(try detect(n).candidates, report.candidates, "n=\(n): not deterministic")
        }
    }

    func testEveryClassAppearsAndTheMixIsPredictable() throws {
        let report = try detect(220)
        func count(_ label: String) -> Int { report.candidates.filter { $0.policyLabel == label }.count }
        XCTAssertEqual(count("AUTO_SAFE"), 22)
        XCTAssertEqual(count("ASK"), 44)
        XCTAssertEqual(count("PROTECTED"), 88)
        XCTAssertEqual(count("UNKNOWN_INCOMPLETE"), 66)
        XCTAssertEqual(
            CandidateListWindow.safetySummary(of: report.candidates),
            "220 found: 22 safe to reclaim, 44 ask first, 88 protected, 66 not enough evidence")
    }

    func testProtectedAndIncompleteRowsAreNeverOfferedAnAction() throws {
        for candidate in try detect(1_000).candidates
        where candidate.policyLabel == "PROTECTED" || candidate.policyLabel == "UNKNOWN_INCOMPLETE" {
            XCTAssertFalse(candidate.executable, candidate.resourceId)
            XCTAssertTrue(candidate.offeredActions.isEmpty, candidate.resourceId)
            XCTAssertNotNil(candidate.refusalReason, candidate.resourceId)
        }
    }

    func testLongIdentifiersExistAtScale() throws {
        let longest = try detect(220).candidates.map(\.resourceId.count).max() ?? 0
        XCTAssertGreaterThan(longest, 200)
    }

    // MARK: - explain

    func testExplainDecodesForEveryClassAndMatchesDetect() throws {
        let report = try detect(10)
        var seen = Set<String>()
        for candidate in report.candidates {
            let result = try run(["--project-root", "/ignored", "explain", candidate.resourceId, "--json"], count: 10)
            XCTAssertEqual(result.status, 0, result.stderr)
            let explain = try JSONDecoder().decode(ExplainReportDto.self, from: result.stdout)
            XCTAssertEqual(explain.policyLabel, candidate.policyLabel)
            XCTAssertEqual(explain.executable, candidate.executable)
            seen.insert(explain.policyLabel)
        }
        XCTAssertEqual(seen, ["AUTO_SAFE", "ASK", "PROTECTED", "UNKNOWN_INCOMPLETE"])
    }

    func testExplainOfAnUnknownResourceFails() throws {
        let result = try run(["explain", "devqa-synthetic:nothing", "--json"], count: 5)
        XCTAssertEqual(result.status, 1)
    }

    // MARK: - Dropping an item (the "No longer shown" scenario)

    func testDroppedIdsDisappearFromDetectAndExplain() throws {
        let dir = try tempDir()
        let before = try detect(25, stateDir: dir)
        let victim = before.candidates[1].resourceId
        try victim.write(to: dir.appendingPathComponent("dropped.txt"), atomically: true, encoding: .utf8)
        let after = try detect(25, stateDir: dir)
        XCTAssertEqual(after.candidates.count, 24)
        XCTAssertFalse(after.candidates.contains { $0.resourceId == victim })
        XCTAssertEqual(try run(["explain", victim, "--json"], count: 25, stateDir: dir).status, 1)
    }

    // MARK: - No authority

    func testEveryCommandThatCouldActIsRefusedAndRecorded() throws {
        let dir = try tempDir()
        for command in ["execute", "apply", "sweep", "autopilot", "resource", "policy", "settings", "recovery", "llm-plan"] {
            let result = try run([command, "devqa-synthetic:docker_image:00000", "--yes"], count: 5, stateDir: dir)
            XCTAssertEqual(result.status, 64, "\(command) must be refused")
        }
        let log = try String(contentsOf: dir.appendingPathComponent("invocations.log"), encoding: .utf8)
        XCTAssertEqual(log.split(separator: "\n").filter { $0.hasPrefix("REFUSED ") }.count, 9)
    }

    func testTheOnlyFileItEverWritesIsItsOwnInvocationLog() throws {
        let dir = try tempDir()
        _ = try detect(5, stateDir: dir)
        _ = try run(["explain", "devqa-synthetic:docker_image:00000"], count: 5, stateDir: dir)
        _ = try run(["execute", "x"], count: 5, stateDir: dir)
        let names = try FileManager.default.contentsOfDirectory(atPath: dir.path)
        XCTAssertEqual(names, ["invocations.log"])
    }

    func testScriptImportsNothingThatCanTouchTheMachine() throws {
        let source = try String(contentsOf: Self.script, encoding: .utf8)
        let code = source.split(separator: "\n").filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("#") }
            .joined(separator: "\n")
        for forbidden in ["subprocess", "shutil", "os.remove", "os.unlink", "os.rmdir", "os.rename", "rmtree",
                          "socket", "urllib", "os.system", "os.walk", "os.scandir", "os.listdir", "glob"] {
            XCTAssertFalse(code.contains(forbidden), "fixture CLI must not use \(forbidden)")
        }
    }

    // MARK: - The app-side seam

    func testSeamAcceptsOnlyAnAbsolutePathToAnExecutable() {
        let key = GlomerisQAFixtures.cliEnvironmentKey
        XCTAssertNil(GlomerisQAFixtures.fixtureCLI(environment: [:]))
        XCTAssertNil(GlomerisQAFixtures.fixtureCLI(environment: [key: "relative/cli"]))
        XCTAssertNil(GlomerisQAFixtures.fixtureCLI(environment: [key: "/no/such/executable"]))
        XCTAssertNil(GlomerisQAFixtures.fixtureCLI(environment: [key: "/etc/hosts"]), "a non-executable file is ignored")
        XCTAssertEqual(
            GlomerisQAFixtures.fixtureCLI(environment: [key: Self.script.path])?.path, Self.script.path)
    }

    func testTheRealClientRoutesEveryCommandToTheStandInWhenTheSeamIsSet() async throws {
        let dir = try tempDir()
        let saved = ProcessInfo.processInfo.environment
        setenv(GlomerisQAFixtures.cliEnvironmentKey, Self.script.path, 1)
        setenv("GLOMERIS_QA_COUNT", "7", 1)
        setenv("GLOMERIS_QA_STATE_DIR", dir.path, 1)
        addTeardownBlock {
            for key in [GlomerisQAFixtures.cliEnvironmentKey, "GLOMERIS_QA_COUNT", "GLOMERIS_QA_STATE_DIR"] {
                if let value = saved[key] { setenv(key, value, 1) } else { unsetenv(key) }
            }
        }
        // Unpinned: the production resolution path, exactly as the views use it.
        let result = try await GlomerisClient().run(
            ["detect", "--json"], outputType: DetectReportDto.self, progressType: ProgressEventDto.self)
        XCTAssertEqual(result.output.candidates.count, 7)
        do {
            _ = try await GlomerisClient().runRaw(["execute", "x"], progressType: ProgressEventDto.self)
        } catch {}
        let log = try String(contentsOf: dir.appendingPathComponent("invocations.log"), encoding: .utf8)
        XCTAssertEqual(log, "detect\nREFUSED execute\n")
    }
}
