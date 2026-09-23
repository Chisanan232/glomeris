//
//  CandidateDetailViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1064: candidate detail view — the sole host of the Clean button.
//  Proves three things mechanically:
//
//  1. `CandidateDetailViewModel.isCleanEnabled` is a direct field read of
//     `ExplainReportDto.executable`, never an inference from
//     `policyLabel`/`reasons` text — proven with a fixture where
//     `policyLabel` reads like something cleanable ("AUTO_SAFE") while
//     `executable` is `false`, and vice versa.
//  2. `CandidateDetailViewModel.requiresConfirmation` is a direct field
//     read of the matching `OfferedActionDto.requiresConfirmation`,
//     never an inference from `policyLabel` text (e.g. never
//     `policyLabel == "ASK"`) — proven with a fixture where
//     `policyLabel` is a deliberately misleading string that has nothing
//     to do with confirmation, while `requiresConfirmation` is `true`.
//  3. No "Clean" button, and no button of any kind, exists anywhere in
//     CandidatesSectionView.swift's row rendering — CandidateDetailView
//     is the only place `Button("Clean"` appears in the whole target.
//

import XCTest

final class CandidateDetailViewTests: XCTestCase {
    // MARK: - Fixtures

    private func explainReport(
        executable: Bool,
        policyLabel: String = "AUTO_SAFE",
        offeredActions: [OfferedActionDto] = [],
        refusalReason: String? = nil
    ) -> ExplainReportDto {
        ExplainReportDto(
            resourceId: "/tmp/example/target",
            kind: "cargo_target",
            detector: "cargo",
            sources: ["Cargo.toml", "target/"],
            logicalBytes: 2_097_152,
            logicalHuman: "2.0 MB",
            reclaimableBytes: 1_048_576,
            reclaimableHuman: "1.0 MB",
            reclaimableBytesIsLowerBound: false,
            completeness: "complete",
            confidence: "high",
            activeUseSignals: [],
            regenerability: "regenerable",
            policyLabel: policyLabel,
            reasons: ["regenerable by cargo build"],
            nativeCleanupAvailable: true,
            nativeCleanupActionId: "cargo.clean.target_dir",
            fingerprintToken: "opaque-token",
            executable: executable,
            offeredActions: offeredActions,
            refusalReason: refusalReason
        )
    }

    // MARK: - `isCleanEnabled` is a field read of `executable`, never an
    // inference from `policyLabel`.

    func testCleanEnabledWhenExecutableTrueEvenWithMisleadingRefusalContext() {
        // policyLabel deliberately reads as something that sounds
        // unsafe/refused ("PROTECTED"), while `executable` is the
        // authoritative `true`. If the view model were inferring from
        // policyLabel text, this would wrongly disable Clean.
        let report = explainReport(
            executable: true,
            policyLabel: "PROTECTED",
            offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertTrue(viewModel.isCleanEnabled)
    }

    func testCleanDisabledWhenExecutableFalseEvenWithMisleadingSafeLabel() {
        // policyLabel deliberately reads as something that sounds safe
        // to clean ("AUTO_SAFE"), while `executable` is the authoritative
        // `false`. If the view model were inferring from policyLabel
        // text, this would wrongly enable Clean.
        let report = explainReport(
            executable: false,
            policyLabel: "AUTO_SAFE",
            offeredActions: [],
            refusalReason: "no registered cleanup action for this resource kind"
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertFalse(viewModel.isCleanEnabled)
    }

    // MARK: - `requiresConfirmation` is a field read of
    // `offeredActions[].requiresConfirmation`, never an inference from
    // `policyLabel`.

    func testRequiresConfirmationTrueWithMisleadingPolicyLabel() {
        // policyLabel is set to a string that has nothing to do with
        // confirmation semantics at all ("ZZZ_NOT_A_REAL_LABEL"), while
        // the offered action's requiresConfirmation is the authoritative
        // `true`. A naive `policyLabel == "ASK"` check would wrongly
        // report `false` here.
        let report = explainReport(
            executable: true,
            policyLabel: "ZZZ_NOT_A_REAL_LABEL",
            offeredActions: [OfferedActionDto(actionId: "node.clean.node_modules", requiresConfirmation: true)]
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertTrue(viewModel.requiresConfirmation)
    }

    func testRequiresConfirmationFalseWithMisleadingAskLikeLabel() {
        // policyLabel literally reads "ASK", which a naive
        // string-matching implementation would treat as
        // requires-confirmation. The authoritative field says otherwise.
        let report = explainReport(
            executable: true,
            policyLabel: "ASK",
            offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertFalse(viewModel.requiresConfirmation)
    }

    func testRequiresConfirmationFalseWhenNoOfferedActionsExist() {
        let report = explainReport(executable: false, offeredActions: [])
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertFalse(viewModel.requiresConfirmation)
    }

    // MARK: - Logical vs. reclaimable size labeled distinctly

    func testLogicalAndReclaimableSizesAreDistinctText() {
        let report = explainReport(
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "x", requiresConfirmation: false)]
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertEqual(viewModel.logicalSizeText, "2.0 MB")
        XCTAssertEqual(viewModel.reclaimableText, "1.0 MB")
        XCTAssertNotEqual(viewModel.logicalSizeText, viewModel.reclaimableText)
    }

    func testReclaimableLowerBoundMarkerRenders() {
        let report = ExplainReportDto(
            resourceId: "/tmp/example/build",
            kind: "docker_build_cache",
            detector: "docker",
            sources: [],
            logicalBytes: nil,
            logicalHuman: nil,
            reclaimableBytes: 500_000_000,
            reclaimableHuman: "500.0 MB",
            reclaimableBytesIsLowerBound: true,
            completeness: "partial",
            confidence: "medium",
            activeUseSignals: [],
            regenerability: "regenerable",
            policyLabel: "ASK",
            reasons: [],
            nativeCleanupAvailable: false,
            nativeCleanupActionId: nil,
            fingerprintToken: nil,
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "docker.prune.build_cache", requiresConfirmation: true)],
            refusalReason: nil
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertTrue(viewModel.reclaimableText.contains("\u{2265}"))
        XCTAssertEqual(viewModel.logicalSizeText, "unknown")
    }

    // MARK: - ExplainReportDto decoding

    func testExplainReportDtoDecodesFingerprintTokenAndExecutableFields() throws {
        let json = """
        {
          "resource_id": "/tmp/example/target",
          "kind": "cargo_target",
          "detector": "cargo",
          "sources": ["Cargo.toml"],
          "logical_bytes": 2097152,
          "logical_human": "2.0 MB",
          "reclaimable_bytes": 1048576,
          "reclaimable_human": "1.0 MB",
          "reclaimable_bytes_is_lower_bound": false,
          "completeness": "complete",
          "confidence": "high",
          "active_use_signals": [],
          "regenerability": "regenerable",
          "policy_label": "AUTO_SAFE",
          "reasons": ["regenerable by cargo build"],
          "native_cleanup_available": true,
          "native_cleanup_action_id": "cargo.clean.target_dir",
          "fingerprint_token": "opaque-token-abc",
          "executable": true,
          "offered_actions": [{"action_id": "cargo.clean.target_dir", "requires_confirmation": false}],
          "refusal_reason": null
        }
        """
        let dto = try JSONDecoder().decode(ExplainReportDto.self, from: Data(json.utf8))
        XCTAssertEqual(dto.fingerprintToken, "opaque-token-abc")
        XCTAssertTrue(dto.executable)
        XCTAssertEqual(dto.offeredActions.count, 1)
        XCTAssertFalse(dto.offeredActions[0].requiresConfirmation)
        XCTAssertNil(dto.refusalReason)
    }

    func testExplainReportDtoDecodesWithNilFingerprintToken() throws {
        let json = """
        {
          "resource_id": "docker:build-cache",
          "kind": "docker_build_cache",
          "detector": "docker",
          "sources": [],
          "logical_bytes": null,
          "logical_human": null,
          "reclaimable_bytes": null,
          "reclaimable_human": null,
          "reclaimable_bytes_is_lower_bound": false,
          "completeness": "unknown",
          "confidence": "low",
          "active_use_signals": [],
          "regenerability": "unknown",
          "policy_label": "PROTECTED",
          "reasons": ["protected"],
          "native_cleanup_available": false,
          "native_cleanup_action_id": null,
          "fingerprint_token": null,
          "executable": false,
          "offered_actions": [],
          "refusal_reason": "PROTECTED: protected"
        }
        """
        let dto = try JSONDecoder().decode(ExplainReportDto.self, from: Data(json.utf8))
        XCTAssertNil(dto.fingerprintToken)
        XCTAssertFalse(dto.executable)
        XCTAssertTrue(dto.offeredActions.isEmpty)
        XCTAssertEqual(dto.refusalReason, "PROTECTED: protected")
    }

    // MARK: - No Clean button anywhere in the candidates list view

    /// AC: "no Clean button exists anywhere in the list view". Mechanical
    /// grep, in the spirit of CandidatesSectionViewTests' own
    /// timer/detect invariants — checks for the actual SwiftUI button
    /// *construction* (`Button("Clean"`), not the bare word "Clean",
    /// since this file's own doc comments legitimately reference "the
    /// Clean button" as context for why it lives elsewhere (see the
    /// caution about comment/grep collisions this ticket calls out).
    func testNoCleanButtonExistsInCandidatesSectionView() throws {
        let source = try Self.readSource("CandidatesSectionView.swift")
        XCTAssertFalse(source.contains("Button(\"Clean\""), "CandidatesSectionView must never construct a Clean button")
    }

    /// Cross-check: `CandidateDetailView` IS the sole host — it must
    /// contain the Clean button construction.
    func testCandidateDetailViewIsTheSoleHostOfTheCleanButton() throws {
        let detailSource = try Self.readSource("CandidateDetailView.swift")
        XCTAssertTrue(detailSource.contains("Button(\"Clean\")"), "CandidateDetailView must host the Clean button")

        let listSource = try Self.readSource("CandidatesSectionView.swift")
        XCTAssertFalse(listSource.contains("Button(\"Clean\")"))
    }

    // MARK: - Wiring invariants (the view actually reads the view model's
    // fields, not just "the view model's fields are correct in isolation")

    /// The view-model-level tests above prove `isCleanEnabled`/
    /// `requiresConfirmation` are field reads. This proves
    /// `CandidateDetailView` actually gates the button and the
    /// confirmation alert on THOSE two view-model properties — not on
    /// some other property (e.g. `policyLabel`) that a future edit could
    /// swap in without any of the view-model tests noticing.
    func testCleanButtonDisabledModifierReadsIsCleanEnabled() throws {
        let source = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        XCTAssertTrue(
            source.contains(".disabled(!viewModel.isCleanEnabled)"),
            "the Clean button's .disabled(...) must read viewModel.isCleanEnabled directly"
        )
    }

    func testConfirmationAlertGatedOnRequiresConfirmation() throws {
        let source = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        XCTAssertTrue(
            source.contains("if viewModel.requiresConfirmation {"),
            "the confirmation alert must be triggered by reading viewModel.requiresConfirmation directly"
        )
        // Exactly one place ever flips the alert-presented flag to true —
        // the branch above. A second, unconditional call site would mean
        // the alert can show regardless of requiresConfirmation.
        let trueAssignments = source.components(separatedBy: "showConfirmationAlert = true").count - 1
        XCTAssertEqual(trueAssignments, 1)
    }

    /// The strongest single proof of "never inferred from policy_label":
    /// after stripping `//` doc-comment lines (which legitimately
    /// mention the policy-label field as prose — see the caution this
    /// ticket calls out about comment/grep collisions), neither
    /// `CandidateDetailView.swift` nor `CandidateDetailViewModel`'s own
    /// code contains a comparison against the `policyLabel` property.
    func testNoCodeBranchesOnPolicyLabelText() throws {
        let code = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        XCTAssertFalse(code.contains("policyLabel =="), "no code path may branch on policyLabel's text")
        XCTAssertFalse(code.contains("== report.policyLabel"), "no code path may branch on policyLabel's text")
    }

    /// Closes the hole a label-specific check like `Button("Clean"`
    /// leaves open: the candidates list must contain exactly the two
    /// known, non-cleanup `Button` constructions (Refresh, and row-tap
    /// navigation to the detail view) — no more — and it must never
    /// construct an `execute` argument. This makes "no cleanup-triggering
    /// control in the list view" mechanical rather than tied to any one
    /// button label like "Clean".
    func testCandidatesSectionViewHasOnlyTheTwoKnownButtonsAndNeverConstructsExecute() throws {
        let source = try Self.strippedOfComments(Self.readSource("CandidatesSectionView.swift"))
        let buttonConstructions = source.components(separatedBy: "Button(").count - 1
            + source.components(separatedBy: "Button {").count - 1
        XCTAssertEqual(
            buttonConstructions, 2,
            "expected exactly the Refresh button and the row-tap navigation button — any other count means a " +
                "button was added or removed without updating this invariant"
        )
        XCTAssertFalse(source.contains("\"execute\""), "the list view must never construct an execute argument")
    }

    // MARK: - HORO-1065: fingerprint pass-through — never refetched

    /// The core security-sensitive proof for this ticket: the token
    /// `buildExecuteArguments` receives must be exactly the one already
    /// captured on the view model from the ORIGINAL `explain` call, never
    /// a value obtained from a hypothetical second `explain` call. This
    /// is proven with a fixture where a "stale" (already-shown) token
    /// deliberately differs from what a fresh `explain` call would
    /// return — the constructed argument array must contain only the
    /// stale one.
    func testExecuteArgumentsCarryTheStoredFingerprintTokenNeverARefreshedOne() {
        let staleToken = "stale-token-shown-to-user"
        let hypotheticalFreshToken = "fresh-token-a-refetch-would-return"

        let report = explainReport(
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: true)]
        )
        // `explainReport(...)` always sets `fingerprintToken: "opaque-token"`;
        // rebuild with the deliberately distinguishable stale token so this
        // test does not rely on that default coincidentally matching.
        let staleReport = ExplainReportDto(
            resourceId: report.resourceId,
            kind: report.kind,
            detector: report.detector,
            sources: report.sources,
            logicalBytes: report.logicalBytes,
            logicalHuman: report.logicalHuman,
            reclaimableBytes: report.reclaimableBytes,
            reclaimableHuman: report.reclaimableHuman,
            reclaimableBytesIsLowerBound: report.reclaimableBytesIsLowerBound,
            completeness: report.completeness,
            confidence: report.confidence,
            activeUseSignals: report.activeUseSignals,
            regenerability: report.regenerability,
            policyLabel: report.policyLabel,
            reasons: report.reasons,
            nativeCleanupAvailable: report.nativeCleanupAvailable,
            nativeCleanupActionId: report.nativeCleanupActionId,
            fingerprintToken: staleToken,
            executable: report.executable,
            offeredActions: report.offeredActions,
            refusalReason: report.refusalReason
        )
        let viewModel = CandidateDetailViewModel(staleReport)
        XCTAssertEqual(viewModel.fingerprintToken, staleToken)

        let arguments = buildExecuteArguments(
            actionId: viewModel.actionId!,
            resourceId: viewModel.resourceId,
            projectRootsArguments: [],
            requiresConfirmation: viewModel.requiresConfirmation,
            fingerprintToken: viewModel.fingerprintToken
        )

        XCTAssertTrue(arguments.contains(staleToken), "the stored (stale) token must be passed verbatim")
        XCTAssertFalse(
            arguments.contains(hypotheticalFreshToken),
            "a token from a hypothetical fresh explain call must never appear"
        )
    }

    /// Mechanical proof at the source level: `explain` is invoked exactly
    /// once in this whole file (inside `loadExplain()`) — there is no
    /// second call site anywhere, including inside `performClean()`.
    func testExplainIsInvokedExactlyOnceInCandidateDetailView() throws {
        // Occurrences as an error-message subject are excluded (HORO-1295
        // routes this view's failures through `SectionFetchErrors
        // .shortMessage(_:subject:)`, which names the subcommand): naming a
        // subcommand in a message is not invoking it. Every other occurrence
        // still counts, so a second call site constructed any other way trips
        // this just as before.
        let code = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        let explainOccurrences = code.components(separatedBy: "\"explain\"").count - 1
        let explainSubjects = code.components(separatedBy: "subject: \"explain\"").count - 1
        XCTAssertEqual(
            explainOccurrences - explainSubjects,
            1,
            "explain must be called from exactly one place — loadExplain()"
        )
    }

    func testBuildExecuteArgumentsOmitsConfirmationFlagsWhenNotRequired() {
        let arguments = buildExecuteArguments(
            actionId: "cargo.clean.target_dir",
            resourceId: "/tmp/example/target",
            projectRootsArguments: ["--project-root", "/tmp/proj"],
            requiresConfirmation: false,
            fingerprintToken: "opaque-token"
        )

        XCTAssertEqual(
            arguments,
            [
                "execute", "--action-id", "cargo.clean.target_dir",
                "--resource-id", "/tmp/example/target",
                "--project-root", "/tmp/proj",
                "--json", "--progress-json",
            ]
        )
        XCTAssertFalse(arguments.contains("--confirm-ask"))
        XCTAssertFalse(arguments.contains("--observed-fingerprint"))
    }

    func testBuildExecuteArgumentsIncludesConfirmationFlagsTogetherWhenRequired() {
        let arguments = buildExecuteArguments(
            actionId: "node.clean.node_modules",
            resourceId: "/tmp/example/app",
            projectRootsArguments: [],
            requiresConfirmation: true,
            fingerprintToken: "opaque-token-xyz"
        )

        XCTAssertEqual(
            arguments,
            [
                "execute", "--action-id", "node.clean.node_modules",
                "--resource-id", "/tmp/example/app",
                "--json", "--progress-json",
                "--confirm-ask", "--observed-fingerprint", "opaque-token-xyz",
            ]
        )
    }

    func testBuildExecuteArgumentsOmitsConfirmationFlagsWhenTokenMissingEvenIfRequired() {
        // Defensive: requiresConfirmation with no token must never emit a
        // half-formed --confirm-ask with no --observed-fingerprint.
        let arguments = buildExecuteArguments(
            actionId: "cargo.clean.target_dir",
            resourceId: "/tmp/example/target",
            projectRootsArguments: [],
            requiresConfirmation: true,
            fingerprintToken: nil
        )

        XCTAssertFalse(arguments.contains("--confirm-ask"))
        XCTAssertFalse(arguments.contains("--observed-fingerprint"))
    }

    // MARK: - HORO-1065: every distinct execute outcome renders its own
    // specific message (the ticket's core AC)

    private func executeReportJSON(
        outcome: String,
        failureMessage: String? = nil,
        abortReason: String? = nil,
        actualReclaimedBytes: UInt64? = nil
    ) -> Data {
        let failure = failureMessage.map { "\"\($0)\"" } ?? "null"
        let abort = abortReason.map { "\"\($0)\"" } ?? "null"
        let actual = actualReclaimedBytes.map(String.init) ?? "null"
        let json = """
        {
          "action_id": "cargo.clean.target_dir",
          "resource_id": "/tmp/example/target",
          "outcome": "\(outcome)",
          "failure_message": \(failure),
          "abort_reason": \(abort),
          "expected_reclaimed_bytes": 1048576,
          "actual_reclaimed_bytes": \(actual)
        }
        """
        return Data(json.utf8)
    }

    private func refusalReportJSON(reason: String, message: String) -> Data {
        let json = """
        {"reason": "\(reason)", "message": "\(message)"}
        """
        return Data(json.utf8)
    }

    func testDescribeExecuteOutcomeSucceededUsesMeasuredActualBytesNotEstimate() {
        let stdout = executeReportJSON(outcome: "succeeded", actualReclaimedBytes: 987_654)
        let outcome = describeExecuteOutcome(exitCode: 0, stdout: stdout, stderrText: "")
        guard case .succeeded(let actualBytes, let human) = outcome else {
            return XCTFail("expected .succeeded")
        }
        XCTAssertEqual(actualBytes, 987_654)
        XCTAssertFalse(human.isEmpty)
    }

    /// Every one of the ~9+ distinct non-succeeded outcome/refusal values
    /// listed in the ticket must render a message unique among all of
    /// them — this is the mechanical proof.
    func testEveryDistinctExecuteOutcomeRendersAUniqueMessage() {
        let cases: [(String, Int32, Data)] = [
            ("failed", 1, executeReportJSON(outcome: "failed", failureMessage: "disk write error")),
            (
                "aborted_by_revalidation:ResourceIdentityChanged", 4,
                executeReportJSON(outcome: "aborted_by_revalidation", abortReason: "ResourceIdentityChanged")
            ),
            (
                "aborted_by_revalidation:PolicyClassDowngraded", 4,
                executeReportJSON(outcome: "aborted_by_revalidation", abortReason: "PolicyClassDowngraded")
            ),
            (
                "aborted_by_revalidation:PolicyReasonsWidened", 4,
                executeReportJSON(outcome: "aborted_by_revalidation", abortReason: "PolicyReasonsWidened")
            ),
            (
                "aborted_by_revalidation:EvidenceDegraded", 4,
                executeReportJSON(outcome: "aborted_by_revalidation", abortReason: "EvidenceDegraded")
            ),
            (
                "resource_not_found", 5,
                refusalReportJSON(reason: "resource_not_found", message: "no discoverable candidate matches")
            ),
            (
                "action_not_found", 5,
                refusalReportJSON(reason: "action_not_found", message: "no registered action resolves")
            ),
            (
                "action_mismatch", 5,
                refusalReportJSON(reason: "action_mismatch", message: "does not match the action this resource resolves to")
            ),
            ("protected", 3, refusalReportJSON(reason: "protected", message: "refused — this resource is PROTECTED")),
            (
                "ask_no_consent", 3,
                refusalReportJSON(reason: "ask_no_consent", message: "refused — this resource requires confirmation")
            ),
            (
                "ask_consent_mismatch", 3,
                refusalReportJSON(
                    reason: "ask_consent_mismatch",
                    message: "refused — the supplied --observed-fingerprint does not match"
                )
            ),
            (
                "auto_safe_contract_violation", 3,
                refusalReportJSON(reason: "auto_safe_contract_violation", message: "refused — an AUTO_SAFE decision failed")
            ),
            (
                "busy", 75,
                refusalReportJSON(reason: "busy", message: "another glomeris execution is already in progress")
            ),
            ("usage_error_exit_2", 2, Data()),
        ]

        var messages: [String: String] = [:]
        for (label, exitCode, stdout) in cases {
            let outcome = describeExecuteOutcome(
                exitCode: exitCode,
                stdout: stdout,
                stderrText: label == "usage_error_exit_2" ? "glomeris execute: --action-id is required" : ""
            )
            guard case .message(let text) = outcome else {
                return XCTFail("\(label) unexpectedly decoded as .succeeded")
            }
            messages[label] = text
        }

        XCTAssertEqual(messages.count, cases.count, "every case must produce a message")
        let uniqueMessages = Set(messages.values)
        XCTAssertEqual(
            uniqueMessages.count, cases.count,
            "every distinct outcome/refusal must render a UNIQUE message — collisions: \(messages)"
        )
    }

    func testDescribeExecuteOutcomeFallsBackGracefullyOnUnparseableStdout() {
        // A real subprocess can still fail unpredictably (e.g. a token
        // encoding edge case) even though the UI constructs the command
        // correctly — this must never crash, only render a message.
        let outcome = describeExecuteOutcome(
            exitCode: 2,
            stdout: Data("not json at all".utf8),
            stderrText: "glomeris execute: invalid --observed-fingerprint token: bad encoding"
        )
        guard case .message(let text) = outcome else {
            return XCTFail("expected .message")
        }
        XCTAssertTrue(text.contains("2"))
        XCTAssertTrue(text.contains("bad encoding"))
    }

    func testExecuteRefusalReportDtoDecodes() throws {
        let json = """
        {"reason": "protected", "message": "refused — this resource is PROTECTED"}
        """
        let dto = try JSONDecoder().decode(ExecuteRefusalReportDto.self, from: Data(json.utf8))
        XCTAssertEqual(dto.reason, "protected")
        XCTAssertEqual(dto.message, "refused — this resource is PROTECTED")
    }

    func testHumanByteCountFormatsRealBytesAndHandlesNil() {
        XCTAssertEqual(humanByteCount(nil, rendered: nil), "unknown")
        XCTAssertFalse(humanByteCount(987_654, rendered: nil).isEmpty)
        XCTAssertNotEqual(humanByteCount(987_654, rendered: nil), "unknown")
    }

    /// HORO-1312. Rust's string wins whenever there is one, and the
    /// fallback for an older binary is deliberately NOT a locally scaled
    /// figure.
    ///
    /// The bug this replaces was not a crash: `ByteCountFormatter` with
    /// `countStyle = .file` is 1000-based, so 2 GiB measured came back as
    /// "2.15 GB" next to Rust's own 1024-based "2.0 GB" estimate for the
    /// same bytes, in the same panel. Both looked finished; one was wrong.
    /// Asserting on the exact "2.0 GB" here is what pins the convention to
    /// the producer — a client that started scaling bytes again would have
    /// to change this line to do it.
    func testHumanByteCountPrefersTheStringRustRendered() {
        XCTAssertEqual(
            humanByteCount(2_147_483_648, rendered: "2.0 GB"),
            "2.0 GB"
        )

        // Empty is treated as absent: a blank label is not a byte count, and
        // showing nothing at all where a result belongs reads as a failure.
        XCTAssertEqual(
            humanByteCount(2_147_483_648, rendered: ""),
            "2147483648 bytes"
        )

        // No `*_human` key at all — an older `glomeris` on PATH. Unformatted
        // on purpose: it is visibly raw, so it cannot be mistaken for a
        // scaled figure, and it is still exactly the number Rust measured.
        let fallback = humanByteCount(2_147_483_648, rendered: nil)
        XCTAssertEqual(fallback, "2147483648 bytes")
        XCTAssertFalse(fallback.contains("GB"))
    }

    // MARK: - HORO-1306: the sheet leads with the verdict, not the fields

    /// This sheet used to be eleven key/value rows in `explain --json`'s own
    /// field order, so "is this safe to delete" was somewhere in the middle
    /// of them. The cards are now ordered by the questions someone opening
    /// it actually has, in order: may I, is it worth it, on what evidence,
    /// and — last — what did the CLI literally say.
    ///
    /// Asserted at the call site, because that is the order rendered and,
    /// this being a plain `VStack`, also the order VoiceOver reads.
    func testCardsAreOrderedSafetyImpactEvidenceThenRawValues() throws {
        let code = Self.strippedOfComments(try Self.readSource("CandidateDetailView.swift"))
        let order = ["safetyCard(viewModel)", "impactCard(viewModel)", "evidenceCard(viewModel)", "rawValuesCard(viewModel)"]
        let positions = try order.map { try XCTUnwrap(code.range(of: $0)?.lowerBound, "\($0) is not rendered") }

        for (earlier, later) in zip(positions, positions.dropFirst()) {
            XCTAssertLessThan(earlier, later, "cards render out of order: \(order)")
        }
    }

    /// The raw `explain --json` values stay reachable — this is an
    /// evidence-first product and a user has to be able to check the
    /// wording against the CLI — but collapsed, so the sheet does not open
    /// on a wall of enum names again.
    func testRawValuesAreReachableButCollapsedBehindADisclosure() throws {
        let code = Self.strippedOfComments(try Self.readSource("CandidateDetailView.swift"))

        XCTAssertTrue(code.contains("DisclosureGroup"), "the raw values must still be reachable")
        XCTAssertTrue(
            code.contains(".textSelection(.enabled)"),
            "values you are meant to quote in a bug report must be selectable"
        )
    }

    // MARK: - HORO-1357: CandidateDetailNavigation

    /// The panel opens on the overview. Before this ticket the equivalent
    /// state was an optional `DetectCandidateReportDto` driving a
    /// `.sheet(item:)`; the mistake that shipped was not in this state but in
    /// what consumed it, so the type is kept deliberately dumb and the wiring
    /// is asserted separately in GlomerisPopoverViewTests.
    func testNavigationStartsOnTheOverview() {
        let navigation = CandidateDetailNavigation()
        XCTAssertNil(navigation.resourceId)
    }

    func testOpeningACandidateShowsItsDetail() {
        var navigation = CandidateDetailNavigation()
        navigation.open("homebrew_cache")
        XCTAssertEqual(navigation.resourceId, "homebrew_cache")
    }

    /// `isShowingDetail` is what the shell hides the overview by, so it has to
    /// track `resourceId` in both directions rather than being set alongside it.
    func testIsShowingDetailTracksWhetherACandidateIsOpen() {
        var navigation = CandidateDetailNavigation()
        XCTAssertFalse(navigation.isShowingDetail)
        navigation.open("homebrew_cache")
        XCTAssertTrue(navigation.isShowingDetail)
        navigation.back()
        XCTAssertFalse(navigation.isShowingDetail)
    }

    /// One level deep, never a stack: opening a second candidate replaces the
    /// first, so "back" always means the overview and can never strand a user
    /// partway down a history they did not know they were building.
    func testOpeningASecondCandidateReplacesRatherThanStacks() {
        var navigation = CandidateDetailNavigation()
        navigation.open("homebrew_cache")
        navigation.open("cargo_target")
        XCTAssertEqual(navigation.resourceId, "cargo_target")

        navigation.back()
        XCTAssertNil(navigation.resourceId, "one back must reach the overview, not the previously-open candidate")
    }

    /// The back control is only rendered while a detail is open, so today this
    /// cannot happen — but nothing about this type should depend on that
    /// staying true, and a second Escape press is the obvious way to find out.
    func testGoingBackIsIdempotent() {
        var navigation = CandidateDetailNavigation()
        navigation.open("homebrew_cache")
        navigation.back()
        navigation.back()
        XCTAssertNil(navigation.resourceId)
    }

    // MARK: - HORO-1323: what the Clean button says about itself
    //
    // The ticket names three gaps, all on the one control in this app that
    // deletes files: no hint that the action is permanent, no reason when it
    // is disabled, and no value tying it to a resource. Asserted on the view
    // model's strings rather than by rendering, which is why those strings
    // live there — see the MARK in CandidateDetailView.swift.

    /// Gap 1. "Clean" on its own conveys nothing about irreversibility.
    func testCleanHintSaysTheDeletionIsPermanentAndNamesTheAmount() {
        let report = explainReport(
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
        )
        let hint = CandidateDetailViewModel(report).cleanButtonHint(isExecuting: false)
        XCTAssertTrue(hint.contains("Permanently deletes"), hint)
        XCTAssertTrue(hint.contains("cannot be undone"), hint)
        XCTAssertTrue(hint.contains("/tmp/example/target"), hint)
        XCTAssertTrue(hint.contains("1.0 MB"), hint)
    }

    /// Gap 3. Out of context the label is an unqualified "Clean", so the
    /// value has to name what is being cleaned.
    func testCleanValueNamesTheResourceAndTheAmount() {
        let report = explainReport(
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
        )
        let viewModel = CandidateDetailViewModel(report)
        XCTAssertEqual(viewModel.cleanTargetDescription, "\(viewModel.kindTerm.title), 1.0 MB")
        XCTAssertNotEqual(viewModel.cleanTargetDescription, "Clean")
    }

    /// An ASK resource is about to prompt, and the hint says so before the
    /// prompt appears rather than after.
    func testCleanHintMentionsConfirmationWhenTheActionAsksFirst() {
        let report = explainReport(
            executable: true,
            policyLabel: "ASK",
            offeredActions: [OfferedActionDto(actionId: "node.clean.node_modules", requiresConfirmation: true)]
        )
        let hint = CandidateDetailViewModel(report).cleanButtonHint(isExecuting: false)
        XCTAssertTrue(hint.contains("confirm"), hint)
        // Still says what it will eventually do — a confirmation prompt is
        // not a substitute for stating the consequence.
        XCTAssertTrue(hint.contains("cannot be undone"), hint)
    }

    /// Gap 2, and per the ticket the half that matters most: the announced
    /// refusal for a PROTECTED resource must be the CLI's own text, not a
    /// Swift-side reconstruction. The fixture's `policy_label` is deliberately
    /// the misleading "AUTO_SAFE" — anything reading the label to decide what
    /// to announce would get this wrong in the most dangerous direction.
    func testDisabledCleanAnnouncesTheCLIsOwnRefusalText() {
        let refusal = "PROTECTED: protected_credential_material"
        let report = explainReport(
            executable: false,
            policyLabel: "AUTO_SAFE",
            offeredActions: [],
            refusalReason: refusal
        )
        let viewModel = CandidateDetailViewModel(report)
        let hint = viewModel.cleanButtonHint(isExecuting: false)
        XCTAssertTrue(hint.contains(refusal), hint)
        XCTAssertTrue(hint.hasPrefix("Unavailable."), hint)
        // And it does NOT promise a deletion that cannot happen.
        XCTAssertFalse(hint.contains("Permanently deletes"), hint)
    }

    /// The structural refusal HORO-1358 exposed, which reaches the UI for an
    /// AUTO_SAFE resource. Same assertion, different Rust wording, because the
    /// point is pass-through rather than one sentence.
    func testDisabledCleanAnnouncesAStructuralRefusalVerbatim() {
        let refusal = "refusing to run brew: this step has no scoped_path, so its identity cannot be "
            + "revalidated before mutation — an unscoped mutating action is never executed "
            + "regardless of policy class"
        let report = explainReport(
            executable: false,
            policyLabel: "AUTO_SAFE",
            offeredActions: [],
            refusalReason: refusal
        )
        XCTAssertTrue(
            CandidateDetailViewModel(report).cleanButtonHint(isExecuting: false).contains(refusal)
        )
    }

    /// The two stacked `.disabled` conditions are different situations — one
    /// is a property of the resource, the other clears in seconds — and this
    /// is the assertion that they no longer collapse into one silent dimmed
    /// control.
    func testTheTwoDisabledReasonsReadDifferently() {
        let refused = CandidateDetailViewModel(
            explainReport(
                executable: false,
                offeredActions: [],
                refusalReason: "no registered cleanup action for this resource kind"
            )
        )
        let executable = CandidateDetailViewModel(
            explainReport(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
            )
        )

        let refusedHint = refused.cleanButtonHint(isExecuting: false)
        let inFlightHint = executable.cleanButtonHint(isExecuting: true)

        XCTAssertNotEqual(refusedHint, inFlightHint)
        // Both have to say the control is unavailable, or the distinction
        // would be between an explanation and silence rather than between two
        // explanations.
        XCTAssertTrue(refusedHint.contains("Unavailable"))
        XCTAssertTrue(inFlightHint.contains("Unavailable"))
        // And only the permanent one renders as a persistent line.
        XCTAssertNotNil(refused.cleanRefusalTerm)
        XCTAssertNil(executable.cleanRefusalTerm)
    }

    /// The transient reason must NOT borrow `GlomerisVocabulary.refusal("busy")`.
    ///
    /// That wording is "Another run is in progress", which is right for what
    /// it describes — `src/main.rs` emits `reason: "busy"` when a *different*
    /// invocation holds the executor lock. `isExecuting` is this view's own
    /// state, and the run it names is the one whose progress spinner renders
    /// directly beneath the button. Announcing that something else is blocking
    /// you, above your own progress indicator, is the same misreading this
    /// ticket exists to remove.
    func testTheInFlightReasonDoesNotClaimAnotherRunIsBlockingYou() {
        let viewModel = CandidateDetailViewModel(
            explainReport(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
            )
        )
        let hint = viewModel.cleanButtonHint(isExecuting: true)

        let busy = GlomerisVocabulary.refusal("busy")
        XCTAssertFalse(hint.contains(busy.title))
        XCTAssertFalse(hint.contains(busy.explanation))
        XCTAssertFalse(hint.lowercased().contains("another"))
        XCTAssertFalse(hint.lowercased().contains("the other"))
        // It does say why the button is dimmed, and that the state passes.
        XCTAssertEqual(hint, CandidateDetailViewModel.cleanInFlightHint)
        XCTAssertTrue(hint.contains("Unavailable"))
        XCTAssertTrue(hint.contains("this cleanup"))
    }

    /// The in-flight case is deliberately not given a persistent reason line:
    /// the progress row directly below it already says a cleanup is running,
    /// live and with the CLI's own step text, so a second static line would
    /// duplicate it and spend viewport the evidence card needs.
    func testTheInFlightCaseRendersNoReasonLineBecauseTheSpinnerSaysIt() {
        let viewModel = CandidateDetailViewModel(
            explainReport(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
            )
        )
        XCTAssertNil(viewModel.cleanRefusalTerm)
    }

    /// An enabled, idle button has no unavailability reason to give, so
    /// nothing extra is rendered beneath it and the hint stays the
    /// description of the action.
    func testAnAvailableCleanButtonHasNoUnavailableReason() {
        let viewModel = CandidateDetailViewModel(
            explainReport(
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
            )
        )
        XCTAssertNil(viewModel.cleanRefusalTerm)
        XCTAssertEqual(
            viewModel.cleanButtonHint(isExecuting: false),
            viewModel.cleanActionDescription
        )
    }

    /// A refusal outranks the transient reason. They cannot co-occur today
    /// (`performClean` needs an `actionId`, and a refused resource has none),
    /// so this pins the resolution rather than discovering it: the permanent
    /// reason is the useful one, and if the two ever did overlap, saying the
    /// button "becomes available again when the run finishes" about a resource
    /// that will never be cleanable would be actively misleading.
    func testARefusalOutranksTheInFlightReason() {
        let viewModel = CandidateDetailViewModel(
            explainReport(
                executable: false,
                offeredActions: [],
                refusalReason: "no registered cleanup action for this resource kind"
            )
        )
        XCTAssertEqual(viewModel.cleanRefusalTerm?.title, "Cannot be cleaned")
        let hint = viewModel.cleanButtonHint(isExecuting: true)
        XCTAssertTrue(hint.contains("no registered cleanup action for this resource kind"))
        XCTAssertNotEqual(hint, CandidateDetailViewModel.cleanInFlightHint)
    }

    /// The view has to actually attach the three modifiers, and attach the
    /// SAME hint string to both `accessibilityHint` and `help` — they both
    /// land in AXHelp, so two different strings would make the spoken and
    /// hovered explanations depend on modifier order.
    func testCleanButtonAttachesTheHintValueAndTooltip() throws {
        let source = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        XCTAssertTrue(source.contains(".accessibilityValue(viewModel.cleanTargetDescription)"))
        XCTAssertTrue(source.contains(".accessibilityHint(viewModel.cleanButtonHint(isExecuting: isExecuting))"))
        XCTAssertTrue(source.contains(".help(viewModel.cleanButtonHint(isExecuting: isExecuting))"))
    }

    /// The reason must also be visible, not accessible-only: the ticket's
    /// premise is that a dimmed control communicates nothing, and that is as
    /// true for someone who can see it as for someone who cannot.
    /// "Beneath the button" is the whole claim, so this compares positions
    /// rather than merely checking that the call exists somewhere in the file.
    /// An earlier version of this test asserted only presence and would have
    /// passed with the line moved above the Button or buried in the evidence
    /// card — either of which defeats the point of rendering it at all.
    func testTheUnavailableReasonIsRenderedVisiblyBeneathTheButton() throws {
        let source = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        XCTAssertTrue(source.contains("private func cleanUnavailableLine(_ term: GlomerisTerm) -> some View"))

        let cleanButton = try XCTUnwrap(source.range(of: #"Button("Clean")"#))
        let enablement = try XCTUnwrap(source.range(of: ".disabled(isExecuting)"))
        let reasonLine = try XCTUnwrap(source.range(of: "cleanUnavailableLine(refusal)"))
        // `ProgressView()` rather than `if isExecuting {`, which also appears
        // in `cleanButtonHint` above and would match there first.
        let progressRow = try XCTUnwrap(source.range(of: "ProgressView()"))

        XCTAssertLessThan(cleanButton.lowerBound, reasonLine.lowerBound, "the reason renders above the button")
        XCTAssertLessThan(enablement.upperBound, reasonLine.lowerBound, "the reason renders inside the button chain")
        XCTAssertLessThan(
            reasonLine.upperBound, progressRow.lowerBound,
            "the reason must sit between the button and the progress row, not after it"
        )
    }

    /// None of the new wording may become a second opinion about whether the
    /// action is allowed. The enablement assertions above already pin the two
    /// `.disabled` reads; this pins the other direction — no `.disabled` in
    /// this file reads the actionability wording.
    func testEnablementNeverReadsTheActionabilityWording() throws {
        let source = try Self.strippedOfComments(Self.readSource("CandidateDetailView.swift"))
        XCTAssertFalse(source.contains(".disabled(viewModel.actionability"))
        XCTAssertFalse(source.contains(".disabled(actionability"))
        let disabledModifiers = source.components(separatedBy: ".disabled(").count - 1
        XCTAssertEqual(
            disabledModifiers, 2,
            "expected exactly the two known .disabled conditions — !isCleanEnabled and isExecuting"
        )
    }

    // MARK: - Helpers

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }

    /// Strips full-line `//` comments (including doc comments) so
    /// mechanical greps for code patterns aren't tripped up by prose
    /// that legitimately mentions the same words/symbols as context —
    /// this file's own headers do exactly that for "policyLabel" and
    /// "Clean button". Only whole-line comments are stripped (every
    /// comment in this codebase's style starts a line), so this is a
    /// conservative, easy-to-reason-about filter rather than a full
    /// Swift-comment parser.
    private static func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }
}
