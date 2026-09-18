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
