//
//  DtoGoldenFixturesTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1061: decodes the same golden fixture JSON files under
//  `tests/fixtures/dto/` (repo root) that `tests/dto_golden_fixtures.rs`
//  asserts the Rust DTOs serialize to, via the hand-written Swift
//  `Codable` mirrors in `Sources/GlomerisDtos.swift`. Asserting on
//  specific decoded field values (not just "decoding did not throw") is
//  what makes a Swift-model field rename or type change fail this test —
//  see the PR description for the scratch experiment that proved both
//  failure directions (Rust field rename fails the Rust test; Swift
//  model field rename fails this one).
//

import XCTest

final class DtoGoldenFixturesTests: XCTestCase {
    /// `tests/fixtures/dto/` at the repo root. This test target compiles
    /// `Sources/GlomerisDtos.swift` directly (see project.yml), same
    /// reasoning as `GlomerisClientTests`' note about not importing an app
    /// module, so fixtures are located relative to `#filePath` exactly
    /// like `GlomerisClientTests.testRealGlomerisDetectJSONDecodes`'s
    /// `repoRoot` does.
    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func loadFixture(_ name: String) throws -> Data {
        let url = Self.fixturesDir.appendingPathComponent(name)
        return try Data(contentsOf: url)
    }

    private func decodeFixture<T: Decodable>(_ name: String, as type: T.Type) throws -> T {
        let data = try loadFixture(name)
        return try JSONDecoder().decode(T.self, from: data)
    }

    // MARK: - StatusReport

    func testDecodesStatusReport() throws {
        let dto = try decodeFixture("status_report.json", as: StatusReportDto.self)

        XCTAssertEqual(dto.totalBytes, 500_000_000_000)
        XCTAssertEqual(dto.freeBytes, 125_000_000_000)
        XCTAssertEqual(dto.usedPercent, 75.0)
        XCTAssertEqual(dto.freeHuman, "116.4 GB")
        XCTAssertEqual(dto.totalHuman, "465.7 GB")
        XCTAssertEqual(dto.pressureState, "WARN")
    }

    // MARK: - DaemonStatusReport

    func testDecodesDaemonStatusReportWithHeartbeat() throws {
        let dto = try decodeFixture("daemon_status_report.json", as: DaemonStatusReportDto.self)

        XCTAssertTrue(dto.plistInstalled)
        XCTAssertEqual(dto.plistPath, "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist")
        XCTAssertTrue(dto.loaded)
        XCTAssertEqual(dto.heartbeatAgeSecs, 42)
    }

    func testDecodesDaemonStatusReportWithNoHeartbeat() throws {
        let dto = try decodeFixture("daemon_status_report_no_heartbeat.json", as: DaemonStatusReportDto.self)

        XCTAssertFalse(dto.plistInstalled)
        XCTAssertFalse(dto.loaded)
        XCTAssertNil(dto.heartbeatAgeSecs)
    }

    // MARK: - HistoryReport

    func testDecodesHistoryReport() throws {
        let dto = try decodeFixture("history_report.json", as: HistoryReportDto.self)

        XCTAssertEqual(dto.events.count, 2)
        XCTAssertEqual(dto.events[0].from, "OK")
        XCTAssertEqual(dto.events[0].to, "WARN")
        XCTAssertEqual(dto.events[0].unixTimeSecs, 1_700_000_000)
        XCTAssertEqual(dto.events[1].from, "WARN")
        XCTAssertEqual(dto.events[1].to, "CRITICAL")
        XCTAssertEqual(dto.events[1].freeBytes, 20_000_000_000)
    }

    // MARK: - DetectReport

    func testDecodesDetectReportWithExecutableAndOfferedActions() throws {
        let dto = try decodeFixture("detect_report.json", as: DetectReportDto.self)

        XCTAssertEqual(dto.candidates.count, 2)

        let executable = dto.candidates[0]
        XCTAssertEqual(executable.resourceId, "cargo_target_dir:/Users/dev/proj/target")
        XCTAssertEqual(executable.policyLabel, "AUTO_SAFE")
        XCTAssertTrue(executable.executable)
        XCTAssertEqual(executable.offeredActions.count, 1)
        XCTAssertEqual(executable.offeredActions[0].actionId, "cargo.clean.target_dir")
        XCTAssertFalse(executable.offeredActions[0].requiresConfirmation)
        XCTAssertNil(executable.refusalReason)
        XCTAssertFalse(executable.reclaimableBytesIsLowerBound)

        let refused = dto.candidates[1]
        XCTAssertEqual(refused.policyLabel, "UNKNOWN_INCOMPLETE")
        XCTAssertFalse(refused.executable)
        XCTAssertTrue(refused.offeredActions.isEmpty)
        XCTAssertEqual(refused.refusalReason, "no registered cleanup action for this resource kind")
        XCTAssertTrue(refused.reclaimableBytesIsLowerBound)
    }

    // MARK: - ExecuteReport

    func testDecodesExecuteReportSucceeded() throws {
        let dto = try decodeFixture("execute_report_succeeded.json", as: ExecuteReportDto.self)

        XCTAssertEqual(dto.outcome, "succeeded")
        XCTAssertNil(dto.failureMessage)
        XCTAssertNil(dto.abortReason)
        XCTAssertEqual(dto.expectedReclaimedBytes, 2_147_483_648)
        XCTAssertEqual(dto.actualReclaimedBytes, 2_147_483_648)
    }

    func testDecodesExecuteReportAbortedByRevalidation() throws {
        let dto = try decodeFixture("execute_report_aborted.json", as: ExecuteReportDto.self)

        XCTAssertEqual(dto.outcome, "aborted_by_revalidation")
        XCTAssertEqual(dto.abortReason, "ResourceIdentityChanged")
        XCTAssertNil(dto.actualReclaimedBytes)
        XCTAssertEqual(dto.expectedReclaimedBytes, 2_147_483_648)
    }
}
