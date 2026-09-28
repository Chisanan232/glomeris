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

    /// HORO-1484: per-detector health crosses the language boundary, and the
    /// two zero-candidate detectors in the fixture are deliberately one
    /// `tool_absent` and one `failed` — a mirror that collapses them (the
    /// defect this ticket fixes, which lived on the Rust side) still decodes
    /// both without throwing, so the assertion has to be on the distinction
    /// rather than on decoding succeeding.
    func testDecodesPerDetectorHealthAndSeparatesAFailureFromAnAbsentTool() throws {
        let dto = try decodeFixture("detect_report.json", as: DetectReportDto.self)

        XCTAssertEqual(dto.detectors.count, 4)
        XCTAssertEqual(
            dto.detectors.map(\.detector),
            ["cargo_target_dir", "docker_build_cache", "node_modules", "project_roots"],
            "order is the registry's and is the only stable identity a row has"
        )

        let absent = try XCTUnwrap(dto.detectors.first { $0.detector == "node_modules" })
        let failed = try XCTUnwrap(dto.detectors.first { $0.detector == "project_roots" })

        // Identical counts. Everything below has to come from `status`.
        XCTAssertEqual(absent.candidatesFound, 0)
        XCTAssertEqual(failed.candidatesFound, 0)

        XCTAssertEqual(absent.status, "tool_absent")
        XCTAssertFalse(absent.didFail)
        XCTAssertNil(absent.reason, "an absent tool has nothing to explain")

        XCTAssertEqual(failed.status, "failed")
        XCTAssertTrue(failed.didFail)
        XCTAssertEqual(failed.reason, "permission denied reading /Users/dev/private")

        XCTAssertEqual(dto.failedDetectors.map(\.detector), ["project_roots"])
        XCTAssertFalse(
            dto.discoveryComplete,
            "one detector failed, so this report is not a complete account of the disk"
        )
    }

    /// The anti-vacuity partner to the test above: with every detector
    /// answering, `discovery_complete` is `true` and `failedDetectors` is empty
    /// — so a mirror that hardwired either one would fail here rather than
    /// quietly passing both tests.
    func testDetectReportReportsCompleteDiscoveryWhenNoDetectorFailed() throws {
        let json = """
        {"candidates":[],"discovery_complete":true,
        "detectors":[{"detector":"cargo_target_dir","status":"found","candidates_found":2},
        {"detector":"docker_images","status":"tool_absent","candidates_found":0}]}
        """
        let dto = try JSONDecoder().decode(DetectReportDto.self, from: Data(json.utf8))

        XCTAssertTrue(dto.discoveryComplete)
        XCTAssertTrue(dto.failedDetectors.isEmpty)
        XCTAssertEqual(dto.detectors.count, 2)
        XCTAssertEqual(dto.detectors[0].candidatesFound, 2)
    }

    /// An older `glomeris` on `PATH` predates `detectors`/`discovery_complete`.
    /// The absent-key default is `true` — "assume complete" — and that is the
    /// less safe of the two defaults, so it is chosen deliberately rather than
    /// by accident: a binary that never had the concept also never had a way to
    /// report a failed detector, so defaulting to `false` would put a permanent
    /// "this list may be incomplete" caveat on every scan from an older CLI,
    /// where it would say nothing and be ignored. The honest mitigation is the
    /// CLI-identity surface (HORO-1466), not a caveat that is always on.
    func testDetectReportStillDecodesWithoutTheDetectorHealthKeys() throws {
        let json = """
        {"candidates":[{"resource_id":"a","kind":"cargo_target","reclaimable_bytes":1,
        "reclaimable_human":"1 B","reclaimable_bytes_is_lower_bound":false,
        "policy_label":"AUTO_SAFE","reasons":[],"executable":true,
        "offered_actions":[],"refusal_reason":null}]}
        """
        let dto = try JSONDecoder().decode(DetectReportDto.self, from: Data(json.utf8))

        XCTAssertEqual(dto.candidates.count, 1)
        XCTAssertTrue(dto.detectors.isEmpty)
        XCTAssertTrue(dto.discoveryComplete)
        XCTAssertTrue(dto.failedDetectors.isEmpty)
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

        // HORO-1312. 2 GiB is "2.0 GB" because `human_bytes` is 1024-based
        // with decimal-style labels. This app formatted the measured figure
        // itself and got "2.15 GB" for the same number — so the two rows of
        // one panel disagreed. Both strings asserted, because the estimate
        // and the result agreeing is the whole point.
        XCTAssertEqual(dto.expectedReclaimedHuman, "2.0 GB")
        XCTAssertEqual(dto.actualReclaimedHuman, "2.0 GB")
        XCTAssertEqual(dto.expectedReclaimedHuman, dto.actualReclaimedHuman)
    }

    func testDecodesExecuteReportAbortedByRevalidation() throws {
        let dto = try decodeFixture("execute_report_aborted.json", as: ExecuteReportDto.self)

        XCTAssertEqual(dto.outcome, "aborted_by_revalidation")
        XCTAssertEqual(dto.abortReason, "ResourceIdentityChanged")
        XCTAssertNil(dto.actualReclaimedBytes)
        XCTAssertEqual(dto.expectedReclaimedBytes, 2_147_483_648)

        // Null, not "0 B". An abort deleted nothing, which is a different
        // claim from having freed zero bytes successfully.
        XCTAssertNil(dto.actualReclaimedHuman)
        XCTAssertEqual(dto.expectedReclaimedHuman, "2.0 GB")
    }

    /// HORO-1312. The `*_human` keys are new, and the binary on `PATH` is not
    /// necessarily the one this app was built beside. An older `glomeris`
    /// omits them, and the decode has to survive that with the rest of the
    /// report intact — a non-optional field here would have blanked the whole
    /// result panel over a missing display string.
    func testDecodesExecuteReportFromAnOlderBinaryWithNoHumanFields() throws {
        let json = """
        {
          "action_id": "cargo.clean.target_dir",
          "resource_id": "cargo_target_dir:/Users/dev/proj/target",
          "outcome": "succeeded",
          "failure_message": null,
          "abort_reason": null,
          "expected_reclaimed_bytes": 2147483648,
          "actual_reclaimed_bytes": 2147483648
        }
        """
        let dto = try JSONDecoder().decode(ExecuteReportDto.self, from: Data(json.utf8))

        XCTAssertEqual(dto.outcome, "succeeded")
        XCTAssertEqual(dto.actualReclaimedBytes, 2_147_483_648)
        XCTAssertNil(dto.actualReclaimedHuman)
        XCTAssertNil(dto.expectedReclaimedHuman)
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
        // Two different sentences, deliberately: the item-level skip reason
        // says why no action was rendered at all, the candidate's own refusal
        // reason names the policy reason code. The card shows both.
        XCTAssertEqual(
            protected.skipReason,
            "PROTECTED — no cleanup action is ever rendered for this resource"
        )
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

        XCTAssertEqual(dto.events.count, 3)

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

        // HORO-1310. An Autopilot run writes the same record shape as the
        // interactive paths, with one difference this app has to survive:
        // `source` names an authority nobody typed. The decode must not be
        // all-or-nothing about it — a client that failed here would blank
        // the whole history panel because one row came from Autopilot.
        let byAutopilot = dto.events[2]
        XCTAssertEqual(byAutopilot.actionId, "node.clean.node_modules")
        XCTAssertEqual(byAutopilot.policyLabel, "AUTO_SAFE")
        XCTAssertEqual(byAutopilot.outcome, "succeeded")
        XCTAssertEqual(byAutopilot.actualReclaimedBytes, 524_288_000)
        XCTAssertEqual(byAutopilot.actualReclaimedHuman, "500.0 MB")
        XCTAssertEqual(byAutopilot.source, "autopilot_auto_safe")

        // `AUTO_SAFE` on a row a model ranked first is still policy's own
        // verdict: the plan reorders candidates and never reaches
        // `classify`. Pinned here so a later reader of this fixture cannot
        // mistake the two axes for one.
        XCTAssertEqual(byAutopilot.policyLabel, dto.events[0].policyLabel)
    }

    /// `model_rank` is asserted on the raw JSON, not on the Swift model,
    /// because `ActionHistoryEventReportDto` deliberately has no property
    /// for it — this app renders no Autopilot surface yet (HORO-1310), and
    /// a mirror silently dropping a key proves nothing about what Rust
    /// emits. What matters here is that the number in the log is the same
    /// 1-based one `glomeris autopilot run` printed as `[AI rank 1]`.
    func testAutopilotHistoryEventCarriesAOneBasedModelRank() throws {
        let data = try loadFixture("action_history_report.json")
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        let events = try XCTUnwrap(json["events"] as? [[String: Any]])
        XCTAssertEqual(events.count, 3)

        XCTAssertTrue(events[0]["model_rank"] is NSNull)
        XCTAssertTrue(events[1]["model_rank"] is NSNull)
        XCTAssertEqual(events[2]["model_rank"] as? Int, 1)
    }

    // MARK: - LlmCheckReport (HORO-1309)

    func testDecodesLlmCheckReportOk() throws {
        let dto = try decodeFixture("llm_check_report_ok.json", as: LlmCheckReportDto.self)

        XCTAssertEqual(dto.outcome, "ok")
        XCTAssertEqual(dto.model, "gpt-4o-mini")
        XCTAssertEqual(dto.endpointPath, "/v1/chat/completions")
        XCTAssertNil(dto.error)
        XCTAssertEqual(dto.responseExcerpt, "ok")
    }

    func testDecodesLlmCheckReportRejected() throws {
        let dto = try decodeFixture("llm_check_report_rejected.json", as: LlmCheckReportDto.self)

        XCTAssertEqual(dto.outcome, "rejected")
        XCTAssertEqual(dto.endpointPath, "/v1/chat/completions")
        XCTAssertNil(dto.responseExcerpt)
        let error = try XCTUnwrap(dto.error)
        XCTAssertTrue(error.contains("HTTP 401"))
        XCTAssertTrue(error.contains("request_id=req_abc123"))
    }

    /// The `llm-check` report carries the request *path* and nothing else about
    /// the address. Asserted on the raw JSON, not on the Swift model, for the
    /// same reason as `testLlmPlanItemsCarryNoFingerprintToken`: a mirror
    /// without a property proves only that this file ignores the key.
    ///
    /// Both halves matter. No `base_url`/`api_key` key means the app cannot
    /// render a credential even by accident; no `://` anywhere means a user who
    /// pasted a key into a query string — which some gateways accept — did not
    /// have it copied into a report a UI shows and a log keeps.
    func testLlmCheckReportsCarryNoHostAndNoCredential() throws {
        for name in ["llm_check_report_ok.json", "llm_check_report_rejected.json"] {
            let data = try loadFixture(name)
            let text = try XCTUnwrap(String(data: data, encoding: .utf8))

            XCTAssertFalse(text.contains("base_url"), "\(name) names a base URL")
            XCTAssertFalse(text.contains("api_key"), "\(name) names an API key")
            XCTAssertFalse(text.contains("://"), "\(name) carries a scheme and host")
        }
    }

    // MARK: - LlmPayloadReport (HORO-1298, surfaced by HORO-1309)

    func testDecodesLlmPayloadReport() throws {
        let dto = try decodeFixture("llm_payload_report.json", as: LlmPayloadReportDto.self)

        XCTAssertTrue(dto.systemPrompt.hasPrefix("You are a storage cleanup ranking assistant."))
        XCTAssertTrue(dto.userPrompt.hasPrefix("[{"))
        XCTAssertEqual(dto.resourceAliases.count, 2)

        let first = dto.resourceAliases[0]
        XCTAssertEqual(first.wireResourceId, "resource_1")
        XCTAssertEqual(first.localResourceId, "cargo_target_dir:/Users/dev/proj/target")
        // `Identifiable` by the wire id — what `ForEach` in the privacy
        // preview's alias table keys on.
        XCTAssertEqual(first.id, "resource_1")
        XCTAssertEqual(dto.resourceAliases[1].wireResourceId, "resource_2")
    }

    /// The claim the privacy preview makes on screen, asserted on the report it
    /// makes it from: the two prompt fields are the outbound bytes, and no real
    /// resource id — every one of which is an absolute path — appears in them.
    ///
    /// This is the Swift half of the same assertion in
    /// `tests/dto_golden_fixtures.rs`. Worth having on both sides: Rust's
    /// version pins what the producer emits, and this one pins that the mirror
    /// the GUI renders from still separates the two lists.
    func testLlmPayloadPromptsContainNoRealResourceId() throws {
        let dto = try decodeFixture("llm_payload_report.json", as: LlmPayloadReportDto.self)

        for alias in dto.resourceAliases {
            XCTAssertFalse(
                dto.systemPrompt.contains(alias.localResourceId),
                "\(alias.localResourceId) is in the system prompt"
            )
            XCTAssertFalse(
                dto.userPrompt.contains(alias.localResourceId),
                "\(alias.localResourceId) is in the user prompt"
            )
            XCTAssertTrue(
                dto.userPrompt.contains(alias.wireResourceId),
                "\(alias.wireResourceId) should be what was sent in its place"
            )
        }
    }

    // MARK: - AutopilotEnvelopeReport

    func testDecodesAutopilotEnvelopeReport() throws {
        let dto = try decodeFixture("autopilot_envelope_report.json", as: AutopilotEnvelopeDto.self)

        XCTAssertTrue(dto.enabled)
        XCTAssertEqual(dto.allowedKinds, ["node_modules", "cargo_target_dir"])
        XCTAssertEqual(dto.maxActions, 2)
        XCTAssertEqual(dto.maxBytes, 2_147_483_648)
        XCTAssertEqual(dto.maxBytesHuman, "2.0 GB")
        XCTAssertEqual(dto.maxDurationSecs, 120)
        XCTAssertEqual(dto.minPressure, "PRESSURED")
        XCTAssertEqual(dto.storedAt, "/Users/dev/Library/Application Support/Glomeris/autopilot.conf")

        XCTAssertEqual(dto.askPreauthorizations.count, 1)
        let consent = try XCTUnwrap(dto.askPreauthorizations.first)
        XCTAssertEqual(consent.kind, "node_modules")
        XCTAssertEqual(consent.reason, "rebuild_cost_high")
        // What `ForEach` keys on, and why it is the pair rather than the kind:
        // two consents can name the same kind.
        XCTAssertEqual(consent.id, "node_modules:rebuild_cost_high")

        XCTAssertEqual(dto.ceilings.maxActions, 25)
        XCTAssertEqual(dto.ceilings.maxBytes, 68_719_476_736)
        XCTAssertEqual(dto.ceilings.maxBytesHuman, "64.0 GB")
        XCTAssertEqual(dto.ceilings.maxDurationSecs, 900)

        XCTAssertEqual(dto.allowlistableKinds.count, 8)
        XCTAssertEqual(dto.neverAllowlistableKinds, ["unknown"])
        XCTAssertEqual(dto.preauthorizableReasons, ["rebuild_cost_high"])
        XCTAssertEqual(dto.neverPreauthorizableReasons.count, 14)
        XCTAssertEqual(dto.pressureStates, ["HEALTHY", "WARN", "PRESSURED", "CRITICAL", "EMERGENCY"])
        XCTAssertEqual(dto.neverExecutableLabels, ["PROTECTED", "UNKNOWN_INCOMPLETE"])
        XCTAssertEqual(dto.aiAuthority.count, 2)
    }

    /// The screen's whole reason for carrying the refusal lists: a control it
    /// builds from `allowlistableKinds` must never be able to offer something
    /// the CLI refuses. Asserted on the decoded report rather than on the view,
    /// because this is a property of the data the view is built from.
    func testAutopilotRefusedKindsAreNeverOffered() throws {
        let dto = try decodeFixture("autopilot_envelope_report.json", as: AutopilotEnvelopeDto.self)

        for refused in dto.neverAllowlistableKinds {
            XCTAssertFalse(
                dto.allowlistableKinds.contains(refused),
                "\(refused) is offered as allowlistable despite being refused"
            )
            XCTAssertFalse(
                dto.allowedKinds.contains(refused),
                "\(refused) is reported as granted despite being refused"
            )
        }
        for refused in dto.neverPreauthorizableReasons {
            XCTAssertFalse(
                dto.preauthorizableReasons.contains(refused),
                "\(refused) is offered as pre-authorizable despite being refused"
            )
        }
    }

    // MARK: - Recovery goal (HORO-1506)

    func testDecodesRecoveryPreviewReport() throws {
        let dto = try decodeFixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)

        // Both axes arrive named, and the sentence the GUI shows is Rust's.
        XCTAssertEqual(dto.goal.usedPercent, 60.0)
        XCTAssertEqual(dto.goal.freePercent, 40.0)
        XCTAssertEqual(dto.goal.description, "60% used (40% free)")

        XCTAssertEqual(dto.current.usedPercent, 88.0)
        XCTAssertEqual(dto.current.freeBytes, 60_000_000_000)
        XCTAssertEqual(dto.current.freeHuman, "55.9 GB")
        XCTAssertEqual(dto.current.pressureState, "PRESSURED")

        XCTAssertEqual(dto.requiredFreeBytes, 200_000_000_000)
        XCTAssertEqual(dto.requiredFreeHuman, "186.3 GB")
        XCTAssertEqual(dto.bytesNeeded, 140_000_000_000)
        XCTAssertEqual(dto.bytesNeededHuman, "130.4 GB")

        XCTAssertEqual(dto.opportunity.actionableNowCount, 1)
        XCTAssertEqual(dto.opportunity.actionableNowBytes, 2_147_483_648)
        XCTAssertEqual(dto.opportunity.actionableNowHuman, "2.0 GB")
        XCTAssertEqual(dto.opportunity.requiresConfirmationCount, 1)
        XCTAssertEqual(dto.opportunity.requiresConfirmationBytes, 5_368_709_120)
        XCTAssertEqual(dto.opportunity.requiresConfirmationHuman, "5.0 GB")
        XCTAssertEqual(dto.opportunity.notExecutableCount, 2)
        XCTAssertEqual(dto.opportunity.protectedCount, 1)
        XCTAssertTrue(dto.opportunity.isLowerBound)

        XCTAssertFalse(dto.goalAppearsReachable)
        XCTAssertFalse(dto.discoveryComplete)
        XCTAssertEqual(dto.candidates.count, 4)
        XCTAssertEqual(dto.detectors.count, 5)

        // The computed property the card uses instead of re-deriving failure
        // from `discoveryComplete`, which cannot name who failed.
        XCTAssertEqual(dto.failedDetectors.map(\.detector), ["homebrew_cache"])
    }

    /// The already-met goal, which is the state the card must not offer a
    /// Recover button for.
    ///
    /// `bytesNeeded` decoding as a real `0` rather than a missing value is the
    /// load-bearing part: it is what lets the app tell "already there" from "not
    /// measured", and the caveat is the only thing that explains the refusal a
    /// real run would produce. Both are asserted here because both are things
    /// the app reads rather than computes.
    func testDecodesRecoveryPreviewReportForAGoalAlreadyMet() throws {
        let dto = try decodeFixture(
            "recovery_preview_report_goal_already_met.json",
            as: RecoveryPreviewReportDto.self
        )

        XCTAssertEqual(dto.goal.description, "95% used (5% free)")
        XCTAssertEqual(dto.current.usedPercent, 88.0)
        XCTAssertEqual(dto.bytesNeeded, 0)
        XCTAssertEqual(dto.bytesNeededHuman, "0 B")
        XCTAssertTrue(
            dto.goalAppearsReachable,
            "a goal with nothing left to reach is trivially reachable, and the flag says so"
        )
        XCTAssertTrue(
            dto.caveats.contains {
                $0.contains("already satisfied") && $0.contains("refused")
            },
            "the preview must say a real run would be refused: \(dto.caveats)"
        )
    }

    /// The preview exists to stop a goal being sold as achievable on the
    /// strength of bytes no run can take. Asserted on the decoded report
    /// because it is a property of the numbers, not of any view: the
    /// opportunity totals must account for the executable candidates only,
    /// and a goal needing more than the whole opportunity must not claim to
    /// be reachable.
    func testRecoveryPreviewOpportunityExcludesWhatNoRunCanReclaim() throws {
        let dto = try decodeFixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)

        let executableBytes = dto.candidates
            .filter(\.executable)
            .reduce(UInt64(0)) { $0 + ($1.reclaimableBytes ?? 0) }
        let offered = dto.opportunity.actionableNowBytes + dto.opportunity.requiresConfirmationBytes
        XCTAssertEqual(
            offered, executableBytes,
            "the opportunity totals must sum the executable candidates and nothing else"
        )

        let protectedBytes = dto.candidates
            .filter { $0.policyLabel == "PROTECTED" }
            .reduce(UInt64(0)) { $0 + ($1.reclaimableBytes ?? 0) }
        XCTAssertGreaterThan(protectedBytes, 0, "the fixture must contain protected bytes to exclude")
        XCTAssertLessThan(
            offered, executableBytes + protectedBytes,
            "protected bytes leaked into the reported opportunity"
        )

        XCTAssertGreaterThan(dto.bytesNeeded, offered)
        XCTAssertFalse(
            dto.goalAppearsReachable,
            "a goal needing more than the entire opportunity must not be reported as reachable"
        )
    }

    func testDecodesRecoveryRunReport() throws {
        let dto = try decodeFixture("recovery_run_report.json", as: RecoveryRunReportDto.self)

        let goal = try XCTUnwrap(dto.goal)
        XCTAssertEqual(goal.usedPercent, 60.0)
        XCTAssertEqual(goal.description, "60% used (40% free)")
        XCTAssertEqual(dto.target, "40% free")

        XCTAssertEqual(dto.stopReason, "target_reached")
        XCTAssertEqual(
            dto.stopReasonDetail,
            "The recovery goal was reached: re-measured free space satisfies it."
        )
        XCTAssertNil(dto.error)

        XCTAssertEqual(dto.iterationsRun, 3)
        XCTAssertEqual(dto.actionsExecuted, 4)
        XCTAssertEqual(dto.actionsDeclinedOrSkipped, 2)

        // Measured, not estimated: the before/after pair is what the card shows
        // beside the goal, and Rust renders the human strings so they cannot
        // disagree with a 1000-based ByteCountFormatter.
        XCTAssertEqual(dto.bytesFreedMeasured, 150_000_000_000)
        XCTAssertEqual(dto.bytesFreedMeasuredHuman, "139.7 GB")
        XCTAssertEqual(dto.startedFreeBytes, 60_000_000_000)
        XCTAssertEqual(dto.startedFreeHuman, "55.9 GB")
        XCTAssertEqual(dto.finalFreeBytes, 210_000_000_000)
        XCTAssertEqual(dto.finalFreeHuman, "195.6 GB")
        XCTAssertEqual(dto.finalFreeBytes - dto.startedFreeBytes, dto.bytesFreedMeasured)

        XCTAssertTrue(dto.targetMet)
        XCTAssertTrue(dto.detectorFailures.isEmpty)
        XCTAssertTrue(dto.discoveryComplete)
        XCTAssertTrue(dto.caveats.isEmpty)

        // A run that reached its goal stopped on purpose, with candidates it
        // never got to. Zeros here would read as "we looked and nothing is
        // left" — a claim it never made — so the key is absent and the app has
        // nothing to show (HORO-1509).
        XCTAssertNil(dto.remaining, "only a safe_exhausted run may state what is left behind")
    }

    /// Two absences are this fixture's whole point. A raw `--target` floor is
    /// not a used-axis goal, so `goal` is absent rather than null — the app
    /// must render the free-axis `target` string in that case instead of
    /// inventing a used figure — and a clean stop carries no `error` key.
    func testDecodesRecoveryRunReportForARawTargetWithNoGoalAndNoError() throws {
        let dto = try decodeFixture("recovery_run_report_raw_target.json", as: RecoveryRunReportDto.self)

        XCTAssertNil(dto.goal, "a free-space floor must not be decoded as a used-axis goal")
        XCTAssertNil(dto.error)
        XCTAssertEqual(dto.target, "232.8 GB free")

        XCTAssertEqual(dto.stopReason, "safe_exhausted")
        XCTAssertFalse(dto.targetMet)
        XCTAssertEqual(dto.iterationsRun, 2)
        XCTAssertEqual(dto.actionsExecuted, 1)
        XCTAssertEqual(dto.actionsDeclinedOrSkipped, 3)
        XCTAssertEqual(dto.bytesFreedMeasured, 2_147_483_648)
        XCTAssertEqual(dto.bytesFreedMeasuredHuman, "2.0 GB")
        XCTAssertEqual(dto.detectorFailures, ["homebrew_cache: brew --cache exited 1"])
        XCTAssertFalse(dto.discoveryComplete)

        // HORO-1509: "no safe candidate remained" is only actionable with what
        // *is* still there, so this fixture carries the breakdown its
        // counterpart above omits. Three different counts on purpose: each is a
        // different next step — say yes, wait for a tool to finish, or nothing
        // at all — so a decoder that transposed the keys would tell a user to
        // confirm something protected, and must fail here instead.
        let remaining = try XCTUnwrap(
            dto.remaining,
            "a safe_exhausted run must say what it left behind"
        )
        XCTAssertEqual(remaining.requiresConfirmationCount, 2)
        XCTAssertEqual(remaining.protectedCount, 1)
        XCTAssertEqual(remaining.notExecutableCount, 3)
    }

    /// A stop short of the goal must be explained, and the explanation is
    /// rendered verbatim in a SwiftUI `Text`. Mirrors
    /// `run_caveats_carry_no_terminal_formatting` on the Rust side, from the
    /// end that actually displays the strings: column padding shows as a gap
    /// mid-sentence, and a bullet or `note:` prefix is terminal formatting the
    /// app would have to strip — which is parsing prose.
    func testRecoveryRunCaveatsAreSentencesRatherThanTerminalLines() throws {
        let dto = try decodeFixture("recovery_run_report_raw_target.json", as: RecoveryRunReportDto.self)

        XCTAssertFalse(dto.caveats.isEmpty, "a run that stopped short must say why the picture is partial")
        for caveat in dto.caveats {
            XCTAssertFalse(caveat.contains("  "), "column padding in a rendered caveat: \(caveat)")
            XCTAssertFalse(caveat.hasPrefix("-"), "a bullet in a rendered caveat: \(caveat)")
            XCTAssertFalse(caveat.hasPrefix("note:"), "a terminal prefix in a rendered caveat: \(caveat)")
            XCTAssertTrue(caveat.hasSuffix("."), "a caveat must be a sentence: \(caveat)")
        }
        XCTAssertTrue(
            dto.caveats.contains { $0.contains("no safe candidate remained") },
            "the exhausted stop must be stated as a limit of discovery, not as completion"
        )
        // The failed detector names belong to `detectorFailures`; repeating
        // them inside a caveat would make the card show them twice.
        for failure in dto.detectorFailures {
            XCTAssertFalse(
                dto.caveats.contains { $0.contains(failure) },
                "\(failure) is duplicated into a caveat"
            )
        }
    }

    func testDecodesRecoveryGoalRejectionReport() throws {
        let dto = try decodeFixture(
            "recovery_goal_rejection_report.json",
            as: RecoveryGoalRejectionReportDto.self
        )

        XCTAssertEqual(dto.reason, "not_an_improvement")
        XCTAssertEqual(
            dto.message,
            "recovery goal 95% used is not an improvement on the current 88.0% used: "
                + "choose a goal below current usage"
        )
        // Both figures, so the card can say what was asked for *and* what it
        // was compared against without re-reading status itself.
        XCTAssertEqual(dto.goalUsedPercent, 95.0)
        XCTAssertEqual(dto.currentUsedPercent, 88.0)
    }

    /// A rejection that never got as far as measuring anything omits both
    /// figures rather than sending zeros, which would read as "0% used".
    func testDecodesRecoveryGoalRejectionReportWithNoFigures() throws {
        let dto = try decodeFixture(
            "recovery_goal_rejection_report_not_finite.json",
            as: RecoveryGoalRejectionReportDto.self
        )

        XCTAssertEqual(dto.reason, "not_finite")
        XCTAssertEqual(dto.message, "recovery goal must be a finite percentage of disk used")
        XCTAssertNil(dto.goalUsedPercent)
        XCTAssertNil(dto.currentUsedPercent)
    }

    // MARK: - Settings (HORO-1507)

    func testDecodesRecoverySettingsReport() throws {
        let dto = try decodeFixture(
            "recovery_settings_report.json",
            as: RecoverySettingsReportDto.self
        )

        // The two numbers arrive under two names and mean two things. This
        // assertion is the Swift half of that contract: swapping the fields in
        // the mirror would make one read 60 and the other 85.
        XCTAssertEqual(dto.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(dto.notifyAtDescription, "85% used")
        XCTAssertEqual(dto.defaultGoal.usedPercent, 60.0)
        XCTAssertEqual(dto.defaultGoal.freePercent, 40.0)
        XCTAssertEqual(dto.defaultGoal.description, "60% used (40% free)")

        XCTAssertEqual(dto.bounds.notifyAtMinimumUsedPercent, 1.0)
        XCTAssertEqual(dto.bounds.notifyAtMaximumUsedPercent, 99.0)
        XCTAssertEqual(dto.bounds.goalMinimumUsedPercent, 0.0)
        XCTAssertEqual(dto.bounds.goalMaximumUsedPercent, 100.0)

        XCTAssertEqual(
            dto.storedAt,
            "/Users/dev/Library/Application Support/Glomeris/settings.conf"
        )
        XCTAssertTrue(dto.loadedFromFile)
    }

    /// Nothing stored yet: the path is absent and the flag says so.
    ///
    /// The flag is the assertion that matters. A pane that inferred "not
    /// configured" from a missing path would say it to anyone whose `$HOME`
    /// could not be resolved, and would say the opposite to someone who had
    /// never opened the app.
    func testDecodesRecoverySettingsReportForTheBuiltInDefaults() throws {
        let dto = try decodeFixture(
            "recovery_settings_report_defaults.json",
            as: RecoverySettingsReportDto.self
        )

        XCTAssertEqual(dto.notifyAtUsedPercent, 75.0)
        XCTAssertEqual(dto.defaultGoal.usedPercent, 70.0)
        XCTAssertNil(dto.storedAt)
        XCTAssertFalse(dto.loadedFromFile)
    }

    /// The cross-field refusal, which is the one that carries both figures.
    func testDecodesSettingsRejectionReport() throws {
        let dto = try decodeFixture(
            "settings_rejection_report.json",
            as: SettingsRejectionReportDto.self
        )

        XCTAssertEqual(dto.reason, "goal_not_below_notify_threshold")
        XCTAssertEqual(
            dto.message,
            "default recovery goal 90% used is not below the alert threshold of 85% used: "
                + "recovery would aim at a disk no emptier than the one that raised the alert"
        )
        XCTAssertEqual(dto.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(dto.goalUsedPercent, 90.0)
    }

    // MARK: - Pressure episodes (HORO-1508)

    /// The state the notifier acts on: threshold crossed, banner owed, nothing
    /// answered yet.
    ///
    /// The four optionals are all absent here, and that is the half of the
    /// contract a mis-mapped `CodingKey` passes silently — a key that never
    /// matches decodes an optional as `nil` without complaint, so a fixture
    /// where everything is `nil` anyway cannot catch it. The sibling test below
    /// populates all four for that reason.
    func testDecodesPressureStatusReport() throws {
        let dto = try decodeFixture(
            "pressure_status_report.json",
            as: PressureStatusReportDto.self
        )

        // Four percentages, four names. Swapping any two in the mirror shows a
        // user the wrong number for the right label.
        XCTAssertEqual(dto.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(dto.notifyAtDescription, "85% used")
        XCTAssertEqual(dto.clearAtUsedPercent, 82.0)
        XCTAssertEqual(dto.current.usedPercent, 91.0)
        XCTAssertEqual(dto.defaultGoal.usedPercent, 60.0)
        XCTAssertEqual(dto.defaultGoal.description, "60% used (40% free)")

        XCTAssertEqual(dto.snoozeSecs, 7200)
        XCTAssertTrue(dto.thresholdCrossed)
        XCTAssertTrue(dto.notificationDue)
        XCTAssertEqual(
            dto.statePath,
            "/Users/dev/Library/Application Support/Glomeris/pressure-episode.json"
        )

        let episode = try XCTUnwrap(dto.episode)
        XCTAssertEqual(episode.episodeId, 1)
        XCTAssertEqual(episode.id, episode.episodeId)
        XCTAssertEqual(episode.openedUsedPercent, 91.0)
        XCTAssertEqual(episode.peakUsedPercent, 91.0)
        XCTAssertEqual(episode.latestUsedPercent, 91.0)
        XCTAssertEqual(episode.latestFreeBytes, 45_000_000_000)
        XCTAssertEqual(episode.latestFreeHuman, "41.9 GB")
        XCTAssertEqual(episode.latestUnixSecs, 1_700_000_000)
        XCTAssertTrue(episode.notificationDue)
        XCTAssertEqual(episode.notificationsRaised, 0)

        // Absent, not zero and not "none": the user has not answered, which is
        // a different thing from having chosen to do nothing.
        XCTAssertNil(episode.lastNotifiedUnixSecs)
        XCTAssertNil(episode.response)
        XCTAssertNil(episode.respondedUnixSecs)
        XCTAssertNil(episode.snoozedUntilUnixSecs)
        XCTAssertFalse(episode.isSnoozed)
    }

    /// The same episode raised, snoozed, and measured again on a disk that had
    /// recovered somewhat.
    ///
    /// Two things are asserted that the test above cannot assert. All four
    /// optionals carry values, so each `CodingKey` is proven to match a real
    /// key. And `current.usedPercent` (88) is deliberately not
    /// `episode.peakUsedPercent` (94): a surface reading one for the other would
    /// tell the user their disk is fuller than the one they are deciding about.
    func testDecodesPressureStatusReportForASnoozedEpisode() throws {
        let dto = try decodeFixture(
            "pressure_status_report_snoozed.json",
            as: PressureStatusReportDto.self
        )

        XCTAssertEqual(dto.current.usedPercent, 88.0)
        XCTAssertEqual(dto.current.pressureState, "PRESSURED")
        XCTAssertTrue(dto.thresholdCrossed, "88% is still above the 85% threshold")
        XCTAssertFalse(dto.notificationDue, "but a snooze is running, so nothing is owed")

        let episode = try XCTUnwrap(dto.episode)
        XCTAssertEqual(episode.peakUsedPercent, 94.0)
        XCTAssertEqual(episode.latestUsedPercent, 88.0)
        XCTAssertEqual(episode.latestFreeHuman, "55.9 GB")
        XCTAssertEqual(episode.notificationsRaised, 1)
        XCTAssertEqual(episode.lastNotifiedUnixSecs, 1_700_000_060)
        XCTAssertEqual(episode.response, "remind_later")
        XCTAssertEqual(episode.respondedUnixSecs, 1_700_000_120)
        XCTAssertEqual(episode.snoozedUntilUnixSecs, 1_700_007_320)
        XCTAssertTrue(episode.isSnoozed)
        XCTAssertFalse(episode.notificationDue)

        // The app renders the answer through the vocabulary rather than
        // switching on the tag, so the token it decoded must be one the
        // vocabulary knows. An unrecognised one here would reach the user as a
        // question mark on a pressure banner.
        XCTAssertEqual(
            GlomerisVocabulary.episodeResponse(try XCTUnwrap(episode.response)).title,
            "Remind me later"
        )
    }

    /// A quiet disk.
    ///
    /// Both absences are asserted, because both could be misread. No episode
    /// means there is nothing to show, not that nothing is being watched — the
    /// threshold and the answer set are still reported. And a missing state path
    /// means `$HOME` could not be resolved, not that monitoring is off.
    func testDecodesPressureStatusReportWithNoEpisode() throws {
        let dto = try decodeFixture(
            "pressure_status_report_no_episode.json",
            as: PressureStatusReportDto.self
        )

        XCTAssertNil(dto.episode)
        XCTAssertNil(dto.statePath)
        XCTAssertFalse(dto.thresholdCrossed)
        XCTAssertFalse(dto.notificationDue)
        XCTAssertEqual(dto.current.usedPercent, 60.0)
        XCTAssertEqual(dto.current.pressureState, "HEALTHY")

        // Still reported with no episode in sight, which is how a pane says
        // what it is watching for before anything has happened.
        XCTAssertEqual(dto.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(dto.responses, ["review_and_recover", "remind_later", "ignore_episode"])
    }

    /// The answer set is read from the CLI, not known locally — so every token
    /// it publishes must already have wording, or a banner ships with a button
    /// labelled with a question mark.
    func testEveryPublishedAnswerHasWording() throws {
        let dto = try decodeFixture(
            "pressure_status_report.json",
            as: PressureStatusReportDto.self
        )

        XCTAssertFalse(dto.responses.isEmpty)
        for token in dto.responses {
            let term = GlomerisVocabulary.episodeResponse(token)
            XCTAssertEqual(term.token, token)
            XCTAssertFalse(
                term.title.contains("Unrecognised"),
                "no wording for published answer \(token)"
            )
        }
    }

    /// A refused acknowledgement — the one an app reaches by doing the right
    /// thing twice, when two polls race to raise the same banner.
    func testDecodesPressureRejectionReport() throws {
        let dto = try decodeFixture(
            "pressure_rejection_report.json",
            as: PressureRejectionReportDto.self
        )

        XCTAssertEqual(dto.reason, "no_notification_due")
        XCTAssertEqual(
            dto.message,
            "no pressure notification is owed "
                + "(there is no open episode, or its notification was already raised)"
        )

        // Worded as news rather than as a failure: nothing went wrong, the disk
        // simply recovered before the button was pressed.
        let term = GlomerisVocabulary.episodeRejection(dto.reason)
        XCTAssertEqual(term.title, "Nothing was owed")
        XCTAssertEqual(term.tone, .neutral)
    }
}
