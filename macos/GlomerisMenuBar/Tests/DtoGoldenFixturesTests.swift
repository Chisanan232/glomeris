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
}
