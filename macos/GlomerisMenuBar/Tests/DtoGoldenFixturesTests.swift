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

        // HORO-1307: the new `impact_tier` key really reaches this mirror,
        // and the fixture deliberately pairs the axes the opposite way round
        // from the intuitive one — the *bigger* candidate is the one Glomeris
        // will not touch. Anything that reads size as safety fails here.
        XCTAssertEqual(executable.impactTier, "notable")
        XCTAssertEqual(refused.impactTier, "large")
        XCTAssertGreaterThan(refused.reclaimableBytes ?? 0, executable.reclaimableBytes ?? 0)
    }

    /// An older `glomeris` on `PATH` predates `impact_tier`. Losing an
    /// emphasis hint is acceptable; losing the whole candidates list because
    /// one optional key is absent is not.
    func testDetectReportStillDecodesWithoutTheImpactTierKey() throws {
        let json = """
        {"candidates":[{"resource_id":"a","kind":"cargo_target","reclaimable_bytes":1,
        "reclaimable_human":"1 B","reclaimable_bytes_is_lower_bound":false,
        "policy_label":"AUTO_SAFE","reasons":[],"executable":true,
        "offered_actions":[],"refusal_reason":null}]}
        """
        let dto = try JSONDecoder().decode(DetectReportDto.self, from: Data(json.utf8))

        XCTAssertEqual(dto.candidates.count, 1)
        XCTAssertNil(dto.candidates[0].impactTier)
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

    // MARK: - LlmPlanReport

    /// HORO-1308. Asserts the three shapes the AI Plan card renders, and
    /// asserts them in the awkward pairing the fixture was built with:
    /// the item the model explained best is also the one it was most wrong
    /// about.
    func testDecodesLlmPlanReport() throws {
        let dto = try decodeFixture("llm_plan_report.json", as: LlmPlanReportDto.self)

        XCTAssertEqual(dto.items.count, 3)
        XCTAssertNil(dto.providerError)
        // Surfaced, not swallowed — the model asked for things that do not
        // exist, and a plan that hides that is overstating itself.
        XCTAssertEqual(dto.droppedUnknownResource, 1)
        XCTAssertEqual(dto.droppedUnknownAction, 2)

        let safe = dto.items[0]
        XCTAssertEqual(safe.policyLabel, "AUTO_SAFE")
        XCTAssertEqual(safe.requestedActionId, "cargo.clean.target_dir")
        XCTAssertEqual(safe.priority, 1)
        XCTAssertEqual(safe.modelReason, "Largest build output and nothing is using it.")
        XCTAssertEqual(safe.completeness, "complete")
        XCTAssertEqual(safe.confidence, "high")
        XCTAssertTrue(safe.candidate.executable)
        XCTAssertEqual(safe.candidate.offeredActions.count, 1)
        XCTAssertFalse(safe.candidate.offeredActions[0].requiresConfirmation)
        XCTAssertEqual(safe.candidate.impactTier, "notable")

        // No rationale at all, and its action still needs confirmation. So
        // "the model explained it" can never be read as "this is fine", and
        // an absent rationale can never be read as "nothing to confirm".
        let ask = dto.items[1]
        XCTAssertEqual(ask.policyLabel, "ASK")
        XCTAssertNil(ask.modelReason)
        XCTAssertEqual(ask.completeness, "partial")
        XCTAssertTrue(ask.candidate.executable)
        XCTAssertTrue(ask.candidate.offeredActions[0].requiresConfirmation)

        // The important one: a confident, plausible-sounding recommendation
        // to delete an SSH private key. Every field that gates an action says
        // no, and the model's sentence is still carried — attributed, beside
        // the refusal, never instead of it.
        let protected = dto.items[2]
        XCTAssertEqual(protected.policyLabel, "PROTECTED")
        XCTAssertEqual(protected.modelReason, "looks like a stale build directory")
        XCTAssertNil(protected.requestedActionId)
        XCTAssertNil(protected.explain)
        XCTAssertEqual(protected.skipReason, "PROTECTED: protected_credential_material")
        XCTAssertFalse(protected.candidate.executable)
        XCTAssertTrue(protected.candidate.offeredActions.isEmpty)
        XCTAssertEqual(
            protected.candidate.refusalReason,
            "PROTECTED: protected_credential_material"
        )
        XCTAssertEqual(protected.candidate.reasons, ["protected_credential_material"])
    }

    /// The plan item carries no `fingerprint_token` — deliberately, because
    /// that token is what pins consent for an `ASK` resource, and a plan must
    /// not hand it out. Acting on a suggestion goes through its own `explain`
    /// call first, exactly as acting on a candidates-list row does.
    ///
    /// Asserted on the raw JSON rather than on the Swift model, because the
    /// Swift model not having a property proves only that this mirror ignores
    /// the key; what matters is that Rust never emits it here.
    func testLlmPlanItemsCarryNoFingerprintToken() throws {
        let data = try loadFixture("llm_plan_report.json")
        let text = try XCTUnwrap(String(data: data, encoding: .utf8))
        XCTAssertFalse(text.contains("fingerprint_token"))
    }

    // MARK: - ActionHistoryReport

    func testDecodesActionHistoryReport() throws {
        let dto = try decodeFixture("action_history_report.json", as: ActionHistoryReportDto.self)

        XCTAssertEqual(dto.events.count, 2)

        let succeeded = dto.events[0]
        XCTAssertEqual(succeeded.actionId, "cargo.clean.target_dir")
        XCTAssertEqual(succeeded.policyLabel, "AUTO_SAFE")
        XCTAssertEqual(succeeded.outcome, "succeeded")
        XCTAssertNil(succeeded.abortReason)
        XCTAssertEqual(succeeded.actualReclaimedBytes, 2_147_483_648)
        XCTAssertEqual(succeeded.actualReclaimedHuman, "2.0 GB")
        XCTAssertEqual(succeeded.source, "execute")

        let aborted = dto.events[1]
        XCTAssertEqual(aborted.policyLabel, "ASK")
        XCTAssertEqual(aborted.outcome, "aborted_by_revalidation")
        XCTAssertEqual(aborted.abortReason, "ResourceIdentityChanged")
        XCTAssertNil(aborted.actualReclaimedBytes)
        XCTAssertNil(aborted.actualReclaimedHuman)
        XCTAssertEqual(aborted.source, "free")
    }
}
