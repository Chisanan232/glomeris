//
//  PlanApplicationTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1366. Two properties matter here and neither is "the enum has five
//  cases".
//
//  The first is that a batch cannot promote anything. AC3 says AI ordering must
//  not make a non-executable or PROTECTED item executable, so the central
//  fixture below is one plan holding all five situations at once — AUTO_SAFE,
//  ASK, PROTECTED, structurally non-executable, and stale — with the PROTECTED
//  item ranked first by the model, which is the arrangement that would catch a
//  batch path trusting the provider's order. AC10 asks for exactly that mix.
//
//  The second is that the result cannot lie. AC8 says a partial batch is never
//  reported as a success, so the headline is asserted for every shape of
//  partial: some cleaned, none cleaned, stopped early, and empty.
//
//  The refusal strings are Rust's own, transcribed from `executable_fields`
//  (`src/reporting/dto.rs`) and `executor::structural_refusal`
//  (`src/executor/mod.rs`) — the same set `CandidateActionabilityTests` uses,
//  because pass-through is the assertion in both files.
//

import XCTest

final class PlanApplicationTests: XCTestCase {
    // MARK: - Fixtures

    private static let protectedRefusal = "PROTECTED: protected_credential_material"

    private static let structuralRefusal =
        "refusing to run brew: this step has no scoped_path, so its identity cannot be "
        + "revalidated before mutation — an unscoped mutating action is never executed "
        + "regardless of policy class"

    private func explainReport(
        resourceId: String,
        kind: String = "cargo_target",
        policyLabel: String = "AUTO_SAFE",
        reclaimableBytes: UInt64? = 1_048_576,
        reclaimableHuman: String? = "1.0 MB",
        reclaimableBytesIsLowerBound: Bool = false,
        fingerprintToken: String? = "opaque-token",
        executable: Bool,
        offeredActions: [OfferedActionDto] = [],
        refusalReason: String? = nil
    ) -> ExplainReportDto {
        ExplainReportDto(
            resourceId: resourceId,
            kind: kind,
            detector: "cargo",
            sources: ["Cargo.toml", "target/"],
            logicalBytes: 2_097_152,
            logicalHuman: "2.0 MB",
            reclaimableBytes: reclaimableBytes,
            reclaimableHuman: reclaimableHuman,
            reclaimableBytesIsLowerBound: reclaimableBytesIsLowerBound,
            completeness: "complete",
            confidence: "high",
            activeUseSignals: [],
            regenerability: "regenerable",
            policyLabel: policyLabel,
            reasons: ["regenerable by cargo build"],
            nativeCleanupAvailable: true,
            nativeCleanupActionId: "cargo.clean.target_dir",
            fingerprintToken: fingerprintToken,
            executable: executable,
            offeredActions: offeredActions,
            refusalReason: refusalReason
        )
    }

    /// The AC10 mix, in the order a provider returned it: the PROTECTED item
    /// ranked first, the ASK item second, and the resource Glomeris is actually
    /// willing to clean unattended ranked last.
    private func mixedPreview() -> PlanApplicationPreview {
        PlanApplicationPreview(steps: [
            // 1. PROTECTED — ranked first by the model.
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/.aws/credentials",
                kind: "credential_material",
                policyLabel: "PROTECTED",
                executable: false,
                refusalReason: Self.protectedRefusal
            )),
            // 2. ASK.
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/Library/Caches/pip",
                kind: "pip_cache",
                policyLabel: "ASK",
                reclaimableBytes: 4_194_304,
                reclaimableHuman: "4.0 MB",
                fingerprintToken: "ask-token",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "pip.purge.cache", requiresConfirmation: true)]
            )),
            // 3. Structurally non-executable: AUTO_SAFE, and refused anyway.
            PlanApplicationStep(explain: explainReport(
                resourceId: "/opt/homebrew/var/cache",
                kind: "homebrew_cache",
                policyLabel: "AUTO_SAFE",
                executable: false,
                refusalReason: Self.structuralRefusal
            )),
            // 4. Stale: `explain` would not describe it.
            PlanApplicationStep(
                staleResourceId: "/Users/x/gone/target",
                kind: "cargo_target",
                policyLabel: "AUTO_SAFE",
                reason: "explain: no such resource is currently detected"
            ),
            // 5. AUTO_SAFE, runnable — last in the model's ordering.
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/proj/target",
                reclaimableBytes: 8_388_608,
                reclaimableHuman: "8.0 MB",
                fingerprintToken: "safe-token",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
            )),
        ])
    }

    private func executeReportJson(
        outcome: String,
        actualBytes: String = "null",
        actualHuman: String = "null",
        failureMessage: String = "null",
        abortReason: String = "null"
    ) -> Data {
        Data("""
        {
          "action_id": "cargo.clean.target_dir",
          "resource_id": "/Users/x/proj/target",
          "outcome": "\(outcome)",
          "failure_message": \(failureMessage),
          "abort_reason": \(abortReason),
          "expected_reclaimed_bytes": 8388608,
          "actual_reclaimed_bytes": \(actualBytes),
          "expected_reclaimed_human": "8.0 MB",
          "actual_reclaimed_human": \(actualHuman)
        }
        """.utf8)
    }

    private func refusalJson(reason: String, message: String) -> Data {
        Data("""
        {"reason": "\(reason)", "message": "\(message)"}
        """.utf8)
    }

    // MARK: - AC10 / AC2: every situation in one plan is classified separately

    func testMixedPlanClassifiesAllFiveSituationsDistinctly() {
        let steps = mixedPreview().steps
        XCTAssertEqual(steps.count, 5)
        XCTAssertEqual(steps[0].disposition, .skippedRefused(reason: Self.protectedRefusal))
        XCTAssertEqual(steps[1].disposition, .willRunAfterConfirming)
        XCTAssertEqual(steps[2].disposition, .skippedRefused(reason: Self.structuralRefusal))
        XCTAssertEqual(
            steps[3].disposition,
            .skippedStale(reason: "explain: no such resource is currently detected")
        )
        XCTAssertEqual(steps[4].disposition, .willRun)
    }

    /// PROTECTED and structurally-non-executable are both `skippedRefused`,
    /// which is correct — they are skipped for the same reason, that the CLI
    /// will not run them — but a user has to be able to tell them apart, and
    /// only the reason text can do that. `offered_actions` is `[]` in both.
    func testTwoRefusalsForDifferentCausesRemainDistinguishable() {
        let steps = mixedPreview().steps
        XCTAssertNotEqual(steps[0].disposition.skipReason, steps[2].disposition.skipReason)
        XCTAssertEqual(steps[0].disposition.skipReason, Self.protectedRefusal)
        XCTAssertEqual(steps[2].disposition.skipReason, Self.structuralRefusal)
    }

    // MARK: - AC3: the model's ordering promotes nothing

    /// The provider ranked the PROTECTED item first and the AUTO_SAFE one
    /// last. Nothing about that ordering reaches the classification: the
    /// runnable set is the AUTO_SAFE item alone, and the first-ranked item is
    /// not in it.
    func testModelRankingProtectedFirstDoesNotMakeItRunnable() {
        let preview = mixedPreview()
        XCTAssertEqual(preview.runnableSteps.map(\.resourceId), ["/Users/x/proj/target"])
        XCTAssertEqual(preview.confirmableSteps.map(\.resourceId), ["/Users/x/Library/Caches/pip"])
        XCTAssertEqual(
            preview.skippedSteps.map(\.resourceId),
            ["/Users/x/.aws/credentials", "/opt/homebrew/var/cache", "/Users/x/gone/target"]
        )
        XCTAssertFalse(preview.steps[0].disposition.isRunnable)
    }

    /// The structural guarantee that backs the one above: a step this type
    /// describes as skipped carries nothing `execute` could be invoked with.
    /// `buildExecuteArguments` requires an action id, and `--confirm-ask`
    /// requires a token, so a caller that ignored `disposition` entirely
    /// still could not run any of these three.
    func testEverySkippedStepCarriesNoActionIdAndNoFingerprintToken() {
        for step in mixedPreview().skippedSteps {
            XCTAssertNil(step.actionId, "skipped step \(step.resourceId) carried an action id")
            XCTAssertNil(step.fingerprintToken, "skipped step \(step.resourceId) carried a token")
        }
    }

    /// And the converse, since a runnable step that lost its token would
    /// silently become an unconfirmable one: both runnable dispositions carry
    /// the action id and the token from their own `explain` report.
    func testRunnableStepsCarryTheirOwnActionIdAndToken() {
        let preview = mixedPreview()
        XCTAssertEqual(preview.confirmableSteps.first?.actionId, "pip.purge.cache")
        XCTAssertEqual(preview.confirmableSteps.first?.fingerprintToken, "ask-token")
        XCTAssertEqual(preview.runnableSteps.first?.actionId, "cargo.clean.target_dir")
        XCTAssertEqual(preview.runnableSteps.first?.fingerprintToken, "safe-token")
    }

    /// A step is built from a *fresh* explain report, so `executable: false`
    /// decides it even when the policy label reads as permissive — the same
    /// contradiction `CandidateDetailViewTests` pins for the Clean button.
    func testAutoSafeLabelDoesNotOverrideExecutableFalse() {
        let step = PlanApplicationStep(explain: explainReport(
            resourceId: "/opt/homebrew/var/cache",
            policyLabel: "AUTO_SAFE",
            executable: false,
            refusalReason: Self.structuralRefusal
        ))
        XCTAssertFalse(step.disposition.isRunnable)
    }

    /// And the reverse contradiction: a PROTECTED-sounding label on a report
    /// whose `executable` is `true` must not be re-refused here. Rust owns
    /// that decision; this type reads the field it published.
    func testProtectedLabelDoesNotOverrideExecutableTrue() {
        let step = PlanApplicationStep(explain: explainReport(
            resourceId: "/Users/x/proj/target",
            policyLabel: "PROTECTED",
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir", requiresConfirmation: false)]
        ))
        XCTAssertEqual(step.disposition, .willRun)
    }

    /// `executable: false` with no reason at all should not happen — every
    /// branch of `executable_fields` returns one — but it must still be a
    /// skip with words on it rather than a silent one.
    func testNonExecutableWithoutAReasonIsStillSkippedWithWording() {
        let step = PlanApplicationStep(explain: explainReport(
            resourceId: "/Users/x/proj/target",
            executable: false,
            refusalReason: nil
        ))
        XCTAssertFalse(step.disposition.isRunnable)
        XCTAssertEqual(step.disposition.skipReason, CandidateActionability.refusedWithoutStatedReason.sentence)
    }

    // MARK: - AC5: an ASK item runs only when the user opts in

    func testConfirmableStepIsExcludedUnlessTheUserOptsIn() {
        let preview = mixedPreview()
        XCTAssertEqual(
            preview.stepsToAttempt(includingConfirmable: false).map(\.resourceId),
            ["/Users/x/proj/target"]
        )
        XCTAssertEqual(
            preview.stepsToAttempt(includingConfirmable: true).map(\.resourceId),
            ["/Users/x/Library/Caches/pip", "/Users/x/proj/target"]
        )
    }

    /// The opt-in widens the batch by exactly the ASK items and nothing else.
    /// A refused or stale step must not become attemptable because the user
    /// agreed to confirm things.
    func testOptingInToConfirmationDoesNotAdmitRefusedOrStaleSteps() {
        let attempted = Set(mixedPreview().stepsToAttempt(includingConfirmable: true).map(\.resourceId))
        XCTAssertFalse(attempted.contains("/Users/x/.aws/credentials"))
        XCTAssertFalse(attempted.contains("/opt/homebrew/var/cache"))
        XCTAssertFalse(attempted.contains("/Users/x/gone/target"))
    }

    // MARK: - AC7 / AC1: when a batch may be offered at all

    func testPlanWithNoRunnableItemsOffersNothingToApply() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/.aws/credentials",
                policyLabel: "PROTECTED",
                executable: false,
                refusalReason: Self.protectedRefusal
            )),
            PlanApplicationStep(
                staleResourceId: "/Users/x/gone/target",
                kind: "cargo_target",
                policyLabel: "AUTO_SAFE",
                reason: "explain: no such resource is currently detected"
            ),
        ])
        XCTAssertFalse(preview.hasAnythingToApply)
    }

    func testEmptyPlanOffersNothingToApply() {
        XCTAssertFalse(PlanApplicationPreview(steps: []).hasAnythingToApply)
    }

    /// AC1 says "executable/confirmable", so a plan whose only actionable
    /// items ask first is still a plan the user can apply — they just have to
    /// say so explicitly.
    func testPlanOfOnlyConfirmableItemsCanStillBeApplied() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/Library/Caches/pip",
                policyLabel: "ASK",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "pip.purge.cache", requiresConfirmation: true)]
            )),
        ])
        XCTAssertTrue(preview.hasAnythingToApply)
        XCTAssertTrue(preview.runnableSteps.isEmpty)
        XCTAssertTrue(preview.stepsToAttempt(includingConfirmable: false).isEmpty)
    }

    // MARK: - The estimate covers what will be attempted, and says when it is partial

    func testEstimateCountsOnlyTheStepsThatWillBeAttempted() {
        let preview = mixedPreview()
        let withoutAsk = preview.reclaimableEstimate(includingConfirmable: false)
        XCTAssertEqual(withoutAsk.bytes, 8_388_608)
        XCTAssertFalse(withoutAsk.isLowerBound)

        let withAsk = preview.reclaimableEstimate(includingConfirmable: true)
        XCTAssertEqual(withAsk.bytes, 8_388_608 + 4_194_304)
        XCTAssertFalse(withAsk.isLowerBound)
    }

    func testAnAttemptedStepWithNoReportedSizeMakesTheTotalALowerBound() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/proj/target",
                reclaimableBytes: 1024,
                reclaimableHuman: "1.0 KB",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: false)]
            )),
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/other/target",
                reclaimableBytes: nil,
                reclaimableHuman: nil,
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "b", requiresConfirmation: false)]
            )),
        ])
        let estimate = preview.reclaimableEstimate(includingConfirmable: false)
        XCTAssertEqual(estimate.bytes, 1024)
        XCTAssertTrue(estimate.isLowerBound)
    }

    /// Rust's own lower-bound flag propagates, so a total assembled from an
    /// estimate that was already a floor is not presented as exact.
    func testRustsLowerBoundFlagPropagatesToTheTotal() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/proj/target",
                reclaimableBytes: 1024,
                reclaimableHuman: "1.0 KB",
                reclaimableBytesIsLowerBound: true,
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: false)]
            )),
        ])
        XCTAssertTrue(preview.reclaimableEstimate(includingConfirmable: false).isLowerBound)
        XCTAssertEqual(preview.steps[0].reclaimableText, "at least 1.0 KB")
    }

    func testEstimateIsNilWhenNoAttemptedStepReportedASize() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/proj/target",
                reclaimableBytes: nil,
                reclaimableHuman: nil,
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: false)]
            )),
        ])
        let estimate = preview.reclaimableEstimate(includingConfirmable: false)
        XCTAssertNil(estimate.bytes)
        XCTAssertTrue(estimate.isLowerBound)
    }

    func testStepWithNoReportedSizeSaysSoRatherThanShowingZero() {
        let step = PlanApplicationStep(explain: explainReport(
            resourceId: "/Users/x/proj/target",
            reclaimableBytes: nil,
            reclaimableHuman: nil,
            executable: true,
            offeredActions: [OfferedActionDto(actionId: "a", requiresConfirmation: false)]
        ))
        XCTAssertEqual(step.reclaimableText, "size unknown")
    }

    // MARK: - Per-item status is read from Rust's own discriminants

    func testSucceededOutcomeBecomesCleanedWithTheMeasuredSize() {
        let status = PlanApplicationItemStatus.from(
            exitCode: 0,
            stdout: executeReportJson(outcome: "succeeded", actualBytes: "8388608", actualHuman: "\"8.0 MB\""),
            stderrText: ""
        )
        XCTAssertEqual(status, .cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB"))
        XCTAssertTrue(status.didClean)
        XCTAssertFalse(status.haltsBatch)
    }

    func testAbortedByRevalidationIsItsOwnStatusAndNotAFailure() {
        let status = PlanApplicationItemStatus.from(
            exitCode: 4,
            stdout: executeReportJson(
                outcome: "aborted_by_revalidation",
                abortReason: "\"ResourceIdentityChanged\""
            ),
            stderrText: ""
        )
        guard case .abortedByRevalidation(let message) = status else {
            return XCTFail("expected abortedByRevalidation, got \(status)")
        }
        XCTAssertTrue(message.contains("ResourceIdentityChanged"), message)
        XCTAssertFalse(status.didClean)
        XCTAssertFalse(status.haltsBatch)
    }

    func testFailedOutcomeCarriesRustsFailureMessage() {
        let status = PlanApplicationItemStatus.from(
            exitCode: 1,
            stdout: executeReportJson(outcome: "failed", failureMessage: "\"permission denied\""),
            stderrText: ""
        )
        guard case .failed(let message) = status else {
            return XCTFail("expected failed, got \(status)")
        }
        XCTAssertTrue(message.contains("permission denied"), message)
    }

    func testRefusalCarriesRustsReasonTokenAndMessage() {
        let status = PlanApplicationItemStatus.from(
            exitCode: 3,
            stdout: refusalJson(reason: "protected", message: "this resource is protected and is never cleaned"),
            stderrText: ""
        )
        XCTAssertEqual(
            status,
            .refused(reason: "protected", message: "this resource is protected and is never cleaned")
        )
        XCTAssertFalse(status.haltsBatch)
    }

    /// The one refusal that stops the batch, because the lock belongs to a
    /// different invocation and every remaining item would refuse identically.
    func testABusyRefusalHaltsTheBatchAndNoOtherRefusalDoes() {
        let busy = PlanApplicationItemStatus.from(
            exitCode: 75,
            stdout: refusalJson(reason: "busy", message: "another glomeris execution is in progress"),
            stderrText: ""
        )
        XCTAssertTrue(busy.haltsBatch)

        for reason in ["protected", "ask_no_consent", "ask_consent_mismatch", "auto_safe_contract_violation",
                       "resource_not_found", "action_not_found", "action_mismatch"] {
            let status = PlanApplicationItemStatus.from(
                exitCode: 3,
                stdout: refusalJson(reason: reason, message: "refused"),
                stderrText: ""
            )
            XCTAssertFalse(status.haltsBatch, "\(reason) must not halt the batch")
        }
    }

    /// Fail-closed: an outcome this path never requests, and one nobody has
    /// written yet, both count as "nothing was cleaned". Counting an
    /// uninterpretable outcome as a success is the failure mode AC8 exists to
    /// prevent.
    func testDryRunAndUnrecognizedOutcomesAreNotCountedAsCleaned() {
        for outcome in ["dry_run", "some_future_outcome"] {
            let status = PlanApplicationItemStatus.from(
                exitCode: 0,
                stdout: executeReportJson(outcome: outcome),
                stderrText: ""
            )
            XCTAssertFalse(status.didClean, "\(outcome) must not report as cleaned")
        }
    }

    func testUnparseableOutputIsAFailureAndNotACleanedItem() {
        let status = PlanApplicationItemStatus.from(
            exitCode: 2,
            stdout: Data("not json".utf8),
            stderrText: "unexpected argument"
        )
        XCTAssertFalse(status.didClean)
        guard case .failed(let message) = status else {
            return XCTFail("expected failed, got \(status)")
        }
        XCTAssertTrue(message.contains("unexpected argument"), message)
    }

    /// The batch and the single-item Clean must describe the same invocation
    /// identically — they share `describeExecuteOutcome`, and this pins that
    /// they still do.
    func testStatusWordingMatchesTheSingleItemCleanPath() {
        let stdout = executeReportJson(outcome: "failed", failureMessage: "\"permission denied\"")
        let batch = PlanApplicationItemStatus.from(exitCode: 1, stdout: stdout, stderrText: "")
        let single = describeExecuteOutcome(exitCode: 1, stdout: stdout, stderrText: "")
        guard case .message(let singleText) = single else {
            return XCTFail("expected a message from the single-item path")
        }
        XCTAssertEqual(batch.message, singleText)
    }

    // MARK: - AC8: the aggregate cannot report a partial batch as a success

    private func result(
        _ statuses: [PlanApplicationItemStatus],
        stoppedEarlyReason: String? = nil
    ) -> PlanApplicationResult {
        PlanApplicationResult(
            items: statuses.enumerated().map { index, status in
                PlanApplicationItemResult(
                    resourceId: "/resource/\(index)",
                    kind: "cargo_target",
                    status: status
                )
            },
            stoppedEarlyReason: stoppedEarlyReason
        )
    }

    func testAllCleanedIsACompleteSuccess() {
        let outcome = result([
            .cleaned(reclaimedBytes: 1024, human: "1.0 KB"),
            .cleaned(reclaimedBytes: 2048, human: "2.0 KB"),
        ])
        XCTAssertTrue(outcome.isCompleteSuccess)
        XCTAssertEqual(outcome.cleanedCount, 2)
        XCTAssertEqual(outcome.notCleanedCount, 0)
        XCTAssertEqual(outcome.headline, "Cleaned 2 items.")
    }

    func testOneRefusalStopsTheBatchBeingASuccessAndIsNamedInTheHeadline() {
        let outcome = result([
            .cleaned(reclaimedBytes: 1024, human: "1.0 KB"),
            .refused(reason: "ask_no_consent", message: "this action requires confirmation"),
        ])
        XCTAssertFalse(outcome.isCompleteSuccess)
        XCTAssertEqual(outcome.cleanedCount, 1)
        XCTAssertEqual(outcome.notCleanedCount, 1)
        XCTAssertEqual(outcome.headline, "Cleaned 1 of 2 items — 1 did not run.")
    }

    func testOneFailureStopsTheBatchBeingASuccess() {
        let outcome = result([
            .cleaned(reclaimedBytes: 1024, human: "1.0 KB"),
            .failed(message: "Execution failed: permission denied"),
        ])
        XCTAssertFalse(outcome.isCompleteSuccess)
        XCTAssertTrue(outcome.headline.contains("1 did not run"), outcome.headline)
    }

    func testOneAbortStopsTheBatchBeingASuccess() {
        let outcome = result([
            .cleaned(reclaimedBytes: 1024, human: "1.0 KB"),
            .abortedByRevalidation(message: "Aborted before making any changes (revalidation): changed"),
        ])
        XCTAssertFalse(outcome.isCompleteSuccess)
    }

    /// A batch every item of which refused is the case most at risk of being
    /// reported as "done": nothing errored, so a naive implementation shows a
    /// success. It must say nothing was cleaned.
    func testNothingCleanedSaysSoExplicitly() {
        let outcome = result([
            .refused(reason: "protected", message: "protected"),
            .notAttempted(reason: "skipped"),
        ])
        XCTAssertFalse(outcome.isCompleteSuccess)
        XCTAssertEqual(outcome.cleanedCount, 0)
        XCTAssertEqual(outcome.headline, "Nothing was cleaned — 2 items did not run.")
        XCTAssertNil(outcome.reclaimedText)
    }

    func testASingleItemThatDidNotRunIsSingular() {
        XCTAssertEqual(
            result([.notAttempted(reason: "skipped")]).headline,
            "Nothing was cleaned — 1 item did not run."
        )
    }

    func testAnEmptyBatchIsNotASuccess() {
        let outcome = result([])
        XCTAssertFalse(outcome.isCompleteSuccess)
        XCTAssertEqual(outcome.headline, "Nothing was run.")
    }

    /// Stopping early is not a failure of any individual item, so the counts
    /// alone would read as a clean success. The stop has to spoil it.
    func testStoppingEarlyStopsTheBatchBeingACompleteSuccess() {
        let outcome = result(
            [.cleaned(reclaimedBytes: 1024, human: "1.0 KB")],
            stoppedEarlyReason: "another glomeris execution is in progress"
        )
        XCTAssertEqual(outcome.notCleanedCount, 0)
        XCTAssertFalse(outcome.isCompleteSuccess)
        XCTAssertEqual(outcome.stoppedEarlyReason, "another glomeris execution is in progress")
    }

    // MARK: - The reclaimed total is measured, never estimated

    func testReclaimedTotalSumsOnlyItemsThatActuallyCleaned() {
        let outcome = result([
            .cleaned(reclaimedBytes: 1024, human: "1.0 KB"),
            .cleaned(reclaimedBytes: 2048, human: "2.0 KB"),
            .failed(message: "Execution failed: permission denied"),
        ])
        XCTAssertEqual(outcome.reclaimedBytes, 3072)
        XCTAssertFalse(outcome.reclaimedIsIncomplete)
        // HORO-1452: was "3072 bytes". The sum is rendered by the same
        // convention as the per-item strings it was summed from, so a reader
        // can check the arithmetic — 1.0 KB + 2.0 KB = 3.0 KB.
        XCTAssertEqual(outcome.reclaimedText, "3.0 KB")
    }

    /// One cleaned row quotes Rust's rendering rather than re-rendering its
    /// byte count with a different convention.
    func testASingleCleanedItemQuotesRustsOwnHumanString() {
        XCTAssertEqual(
            result([.cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB")]).reclaimedText,
            "8.0 MB"
        )
    }

    func testACleanedItemWithNoMeasuredSizeMakesTheTotalALowerBound() {
        let outcome = result([
            .cleaned(reclaimedBytes: 1024, human: "1.0 KB"),
            .cleaned(reclaimedBytes: nil, human: "unknown"),
        ])
        XCTAssertTrue(outcome.reclaimedIsIncomplete)
        // HORO-1452: was "at least 1024 bytes". The floor wording is unchanged;
        // only the figure inside it is now rendered like every other figure.
        XCTAssertEqual(outcome.reclaimedText, "at least 1.0 KB")
    }

    func testNoMeasuredSizeAtAllReportsNoTotalRatherThanZero() {
        let outcome = result([.cleaned(reclaimedBytes: nil, human: "unknown"), .cleaned(reclaimedBytes: nil, human: "unknown")])
        XCTAssertNil(outcome.reclaimedBytes)
        XCTAssertNil(outcome.reclaimedText)
    }

    /// The commonest successful batch there is, and it used to read "Reclaimed
    /// unknown."
    ///
    /// `executor::execute_plan` measures `actual_reclaimed_bytes` only for a
    /// plan made entirely of `DeletePath` steps; a `RunTool` step makes it
    /// `Unavailable`. `cargo.clean.target_dir` is a `RunTool`, so the release
    /// binary returns `"actual_reclaimed_bytes": null` on every successful cargo
    /// cleanup, and `humanByteCount` renders that as the literal `"unknown"`.
    /// One such row must therefore say nothing about space, exactly as two
    /// already did.
    func testASingleCleanedRowWithNoMeasuredSizeSaysNothingAboutSpace() {
        let outcome = result([.cleaned(reclaimedBytes: nil, human: "unknown")])
        XCTAssertEqual(outcome.cleanedCount, 1)
        XCTAssertEqual(outcome.headline, "Cleaned 1 item.")
        XCTAssertNil(
            outcome.reclaimedText,
            "a row whose size was never measured must not be reported as 'unknown' reclaimed"
        )
    }

    /// The counterpart: a single row that *did* measure still quotes Rust's own
    /// rendering rather than the raw count, so the fix above did not silence the
    /// case it was never about.
    func testASingleCleanedRowWithAMeasuredSizeStillQuotesRustsOwnString() {
        let outcome = result([.cleaned(reclaimedBytes: 2_621_478, human: "2.5 MB")])
        XCTAssertEqual(outcome.reclaimedText, "2.5 MB")
    }

    // MARK: - Steps and results carry no policy judgment of their own

    /// Both types render their policy label and kind through
    /// `GlomerisVocabulary`, which is a lookup — the same route every other
    /// row in the panel uses. Asserted so a future edit that starts branching
    /// on the label here is caught by a test as well as by
    /// `check-no-policy-label-branching.sh`.
    func testStepsAndResultsRenderLabelsThroughTheSharedVocabulary() {
        let step = mixedPreview().steps[0]
        XCTAssertEqual(step.safetyTerm, GlomerisVocabulary.safety("PROTECTED"))
        XCTAssertEqual(step.kindTerm, GlomerisVocabulary.kind("credential_material"))

        let item = PlanApplicationItemResult(
            resourceId: "/Users/x/proj/target",
            kind: "cargo_target",
            status: .cleaned(reclaimedBytes: 1024, human: "1.0 KB")
        )
        XCTAssertEqual(item.kindTerm, GlomerisVocabulary.kind("cargo_target"))
    }

    /// A step's sentence is the shared actionability wording, so the preview
    /// says the same thing about a resource that the candidates list and the
    /// detail view do.
    func testStepSentenceIsTheSharedActionabilityWording() {
        let steps = mixedPreview().steps
        XCTAssertEqual(steps[0].actionability.sentence, Self.protectedRefusal)
        XCTAssertEqual(steps[1].actionability, .asksFirstThenCleans)
        XCTAssertEqual(steps[4].actionability, .readyToClean)
    }

    // MARK: - AC1: whether an entry point is offered at all

    /// The gate is read from the suggestion set's own actionability, before any
    /// re-check has happened, so the button can appear at the same moment the
    /// rows do. It is deliberately optimistic in one direction only: a plan may
    /// offer the entry point and then preview as entirely unrunnable once
    /// `explain` has been re-run, and that is the honest outcome — the review
    /// step is where the user learns it, and `hasAnythingToApply` is what gates
    /// the destructive control.
    func testEntryPointIsOfferedWhenAnySuggestionLooksActionable() {
        XCTAssertTrue(PlanApplicationPreview.snapshotSuggestsAnApplicableItem([
            planItem(resourceId: "/Users/x/.aws/credentials", policyLabel: "PROTECTED",
                     executable: false, refusalReason: Self.protectedRefusal),
            planItem(resourceId: "/Users/x/proj/target", policyLabel: "AUTO_SAFE",
                     executable: true, requiresConfirmation: false),
        ]))
    }

    /// AC7, at the entry point. A plan of refusals must not present a
    /// destructive-sounding action at all — not a disabled one, since a dimmed
    /// Apply still asserts that applying this plan is a thing that could happen.
    func testEntryPointIsNotOfferedForAPlanOfRefusalsOnly() {
        XCTAssertFalse(PlanApplicationPreview.snapshotSuggestsAnApplicableItem([
            planItem(resourceId: "/Users/x/.aws/credentials", policyLabel: "PROTECTED",
                     executable: false, refusalReason: Self.protectedRefusal),
            planItem(resourceId: "/opt/homebrew/var/cache", policyLabel: "AUTO_SAFE",
                     executable: false, refusalReason: Self.structuralRefusal),
            // No stated reason at all: still not something to offer an Apply for.
            planItem(resourceId: "/Users/x/other", policyLabel: "AUTO_SAFE",
                     executable: false, refusalReason: nil),
        ]))
    }

    func testEntryPointIsNotOfferedForAnEmptyPlan() {
        XCTAssertFalse(PlanApplicationPreview.snapshotSuggestsAnApplicableItem([]))
    }

    /// An ASK-only plan is applicable — the user can opt in — so the entry
    /// point appears. The same property `testPlanOfOnlyConfirmableItemsCanStillBeApplied`
    /// asserts one step further down.
    func testEntryPointIsOfferedForAConfirmableOnlySuggestion() {
        XCTAssertTrue(PlanApplicationPreview.snapshotSuggestsAnApplicableItem([
            planItem(resourceId: "/Users/x/Library/Caches/pip", policyLabel: "ASK",
                     executable: true, requiresConfirmation: true),
        ]))
    }

    /// AC3 at the gate as well as in the preview: an AUTO_SAFE label on a
    /// resource Rust will not execute does not make the plan applicable.
    func testAnAutoSafeLabelOnANonExecutableSuggestionDoesNotOfferTheEntryPoint() {
        XCTAssertFalse(PlanApplicationPreview.snapshotSuggestsAnApplicableItem([
            planItem(resourceId: "/opt/homebrew/var/cache", policyLabel: "AUTO_SAFE",
                     executable: false, refusalReason: Self.structuralRefusal),
        ]))
    }

    // MARK: - Positions, not resource ids

    /// A provider may name the same resource twice. `attemptedIndices` answers
    /// in positions so a duplicated id cannot collapse two outcomes into one or
    /// attribute the first item's result to the second.
    func testDuplicatedResourceIdsRemainTwoSeparateSteps() {
        let duplicated = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/proj/target",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            )),
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/proj/target",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            )),
        ])
        XCTAssertEqual(duplicated.attemptedIndices(includingConfirmable: false), [0, 1])

        // The first cleaned, the second found nothing left to clean. Both
        // outcomes survive, attached to the right position.
        let result = PlanApplicationResult.assemble(
            preview: duplicated,
            includingConfirmable: false,
            statusesByStepIndex: [
                0: .cleaned(reclaimedBytes: 1_048_576, human: "1.0 MB"),
                1: .failed(message: "the directory no longer exists"),
            ],
            stoppedEarlyReason: nil
        )
        XCTAssertEqual(result.items.count, 2)
        XCTAssertTrue(result.items[0].status.didClean)
        XCTAssertFalse(result.items[1].status.didClean)
        XCTAssertFalse(result.isCompleteSuccess)
    }

    // MARK: - A resource the plan names twice
    //
    // Two layers, and both are wanted. `attemptedIndices` answers in positions
    // so that a preview holding a duplicate is still described correctly — that
    // is the structural property just above, and it stays true. But a preview
    // that *holds* a duplicate is a preview that double-counts its estimate and
    // attempts the second copy against what the first deleted, so the sweep
    // collapses the plan first. The tests below pin the collapse; the tests
    // above pin what the types do if one ever gets through anyway.

    /// The estimate AC2 shows and the headline AC8 shows are the two places a
    /// duplicate is visible to the user, so both are asserted against the
    /// collapsed list rather than only the count.
    func testARepeatedResourceIsRecheckedAndAttemptedOnce() {
        let twice = [
            planItem(resourceId: "/Users/x/proj/target", policyLabel: "AUTO_SAFE", executable: true),
            planItem(resourceId: "/Users/x/proj/target", policyLabel: "AUTO_SAFE", executable: true),
        ]
        let collapsed = PlanApplicationPreview.deduplicatedByResource(twice)
        XCTAssertEqual(collapsed.map(\.candidate.resourceId), ["/Users/x/proj/target"])

        // What the preview would then say, built from the collapsed list: one
        // resource's size, once.
        let preview = PlanApplicationPreview(steps: collapsed.map { item in
            PlanApplicationStep(explain: explainReport(
                resourceId: item.candidate.resourceId,
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            ))
        })
        XCTAssertEqual(preview.attemptedIndices(includingConfirmable: false), [0])
        XCTAssertEqual(
            preview.reclaimableEstimate(includingConfirmable: false).bytes,
            1_048_576,
            "a resource named twice must not promise twice its size"
        )

        let result = PlanApplicationResult.assemble(
            preview: preview,
            includingConfirmable: false,
            statusesByStepIndex: [0: .cleaned(reclaimedBytes: 1_048_576, human: "1.0 MB")],
            stoppedEarlyReason: nil
        )
        XCTAssertEqual(result.headline, "Cleaned 1 item.")
        XCTAssertTrue(
            result.isCompleteSuccess,
            "a batch that did everything the user asked must not be marked incomplete by a "
                + "duplicate the provider introduced"
        )
    }

    /// The first mention wins, because the plan's order is the model's stated
    /// priority and dropping the earlier of two identical picks would re-rank
    /// it. Asserted through a field that differs between the two copies.
    func testTheFirstMentionOfARepeatedResourceIsTheOneKept() {
        let collapsed = PlanApplicationPreview.deduplicatedByResource([
            planItem(resourceId: "/Users/x/proj/target", policyLabel: "AUTO_SAFE",
                     executable: true, requiresConfirmation: false),
            planItem(resourceId: "/Users/x/proj/target", policyLabel: "ASK",
                     executable: true, requiresConfirmation: true),
        ])
        XCTAssertEqual(collapsed.count, 1)
        XCTAssertEqual(collapsed[0].policyLabel, "AUTO_SAFE")
    }

    /// Distinct resources are left exactly as the model ordered them — the
    /// collapse must not become a re-sort or a filter.
    func testDistinctResourcesAreLeftInTheModelsOrder() {
        let items = [
            planItem(resourceId: "/Users/x/b/target", policyLabel: "AUTO_SAFE", executable: true),
            planItem(resourceId: "/Users/x/a/target", policyLabel: "AUTO_SAFE", executable: true),
            planItem(resourceId: "/Users/x/b/target", policyLabel: "AUTO_SAFE", executable: true),
            planItem(resourceId: "/Users/x/c/target", policyLabel: "AUTO_SAFE", executable: true),
        ]
        XCTAssertEqual(
            PlanApplicationPreview.deduplicatedByResource(items).map(\.candidate.resourceId),
            ["/Users/x/b/target", "/Users/x/a/target", "/Users/x/c/target"]
        )
    }

    /// A refused duplicate collapses too. It cannot be executed either way, but
    /// two identical PROTECTED rows in a preview read as two resources Glomeris
    /// declined when there is one.
    func testARepeatedRefusedResourceAlsoCollapses() {
        let collapsed = PlanApplicationPreview.deduplicatedByResource([
            planItem(resourceId: "/Users/x/.aws/credentials", policyLabel: "PROTECTED",
                     executable: false, refusalReason: Self.protectedRefusal),
            planItem(resourceId: "/Users/x/.aws/credentials", policyLabel: "PROTECTED",
                     executable: false, refusalReason: Self.protectedRefusal),
        ])
        XCTAssertEqual(collapsed.count, 1)
    }

    func testAnEmptyPlanCollapsesToNothing() {
        XCTAssertTrue(PlanApplicationPreview.deduplicatedByResource([]).isEmpty)
    }

    func testAttemptedIndicesArePositionsInTheModelsOrder() {
        let preview = mixedPreview()
        // Position 4 only: the runnable AUTO_SAFE item the model ranked last.
        XCTAssertEqual(preview.attemptedIndices(includingConfirmable: false), [4])
        // Opting in adds position 1 — and in plan order, not appended.
        XCTAssertEqual(preview.attemptedIndices(includingConfirmable: true), [1, 4])
    }

    // MARK: - AC8: the result accounts for every previewed step

    /// The heart of AC8's "it never reports the whole plan successful when one
    /// or more items were refused". The result is assembled over every step in
    /// the preview, not over the attempted subset, so a batch that cleaned its
    /// one runnable item out of five still says so as one of five.
    func testAssembleReportsEverySkippedStepAlongsideTheExecutedOne() {
        let preview = mixedPreview()
        let result = PlanApplicationResult.assemble(
            preview: preview,
            includingConfirmable: false,
            statusesByStepIndex: [4: .cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB")],
            stoppedEarlyReason: nil
        )

        XCTAssertEqual(result.items.count, 5)
        XCTAssertEqual(result.cleanedCount, 1)
        XCTAssertEqual(result.notCleanedCount, 4)
        XCTAssertFalse(result.isCompleteSuccess)
        XCTAssertEqual(result.headline, "Cleaned 1 of 5 items — 4 did not run.")

        // Order is the preview's order, which is the model's order.
        XCTAssertEqual(result.items.map(\.resourceId), preview.steps.map(\.resourceId))
    }

    /// AC6 in the result as well as in the preview: each skipped row carries
    /// the CLI's own sentence, so PROTECTED, a structural refusal and a stale
    /// resource remain three different explanations after the run rather than
    /// one shared "skipped".
    func testEverySkippedStepKeepsItsOwnReasonInTheResult() {
        let result = PlanApplicationResult.assemble(
            preview: mixedPreview(),
            includingConfirmable: false,
            statusesByStepIndex: [4: .cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB")],
            stoppedEarlyReason: nil
        )
        XCTAssertEqual(result.items[0].status, .notAttempted(reason: Self.protectedRefusal))
        XCTAssertEqual(result.items[2].status, .notAttempted(reason: Self.structuralRefusal))
        XCTAssertEqual(
            result.items[3].status,
            .notAttempted(reason: "explain: no such resource is currently detected")
        )

        let messages = Set([0, 2, 3].map { result.items[$0].status.message })
        XCTAssertEqual(messages.count, 3, "three distinct causes must read as three distinct rows")
    }

    /// AC5's consequence. An ASK item the user did not opt in to is not
    /// "refused" and not "failed" — it is unapplied because they did not
    /// authorise it, and the row says which.
    func testAConfirmableStepTheUserDidNotIncludeSaysWhyItWasNotApplied() {
        let result = PlanApplicationResult.assemble(
            preview: mixedPreview(),
            includingConfirmable: false,
            statusesByStepIndex: [4: .cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB")],
            stoppedEarlyReason: nil
        )
        XCTAssertEqual(
            result.items[1].status,
            .notAttempted(
                reason: "Not applied — you did not include the items that ask for confirmation."
            )
        )
        // And it does not read as a refusal by Glomeris, which would be a lie
        // about who declined.
        XCTAssertFalse(result.items[1].status.message.contains("PROTECTED"))
    }

    /// An item that was in the batch and never reached says that, rather than
    /// borrowing the reason of the item that stopped the batch.
    func testAnItemTheBatchNeverReachedIsDistinguishedFromARefusal() {
        let preview = mixedPreview()
        let result = PlanApplicationResult.assemble(
            preview: preview,
            includingConfirmable: true,
            // Position 1 refused with `busy`; position 4 was authorised and
            // never reached.
            statusesByStepIndex: [
                1: .refused(reason: "busy", message: "another Glomeris invocation is running"),
            ],
            stoppedEarlyReason: "another Glomeris invocation is running"
        )
        XCTAssertEqual(
            result.items[4].status,
            .notAttempted(reason: "Not attempted — the batch stopped before reaching this item.")
        )
        XCTAssertFalse(result.isCompleteSuccess)
        XCTAssertEqual(result.stoppedEarlyReason, "another Glomeris invocation is running")
        // The refusal's own words are still on its own row, not moved onto the
        // row that never ran.
        XCTAssertTrue(result.items[1].status.message.contains("another Glomeris invocation"))
    }

    /// An all-skipped plan, applied anyway (nothing to attempt): no row claims
    /// success and the aggregate does not either.
    func testAssemblingWithNoStatusesAtAllClaimsNothing() {
        let result = PlanApplicationResult.assemble(
            preview: mixedPreview(),
            includingConfirmable: false,
            statusesByStepIndex: [:],
            stoppedEarlyReason: nil
        )
        XCTAssertEqual(result.cleanedCount, 0)
        XCTAssertFalse(result.isCompleteSuccess)
        XCTAssertNil(result.reclaimedBytes)
        XCTAssertEqual(result.headline, "Nothing was cleaned — 5 items did not run.")
    }

    /// The one case that is a complete success: every previewed step ran and
    /// cleaned. `includingConfirmable` is true here because otherwise the ASK
    /// step would be unapplied and the batch — correctly — would not be complete.
    func testAPlanWhereEveryStepCleanedIsACompleteSuccess() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/a", fingerprintToken: "a", executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            )),
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/b", fingerprintToken: "b", executable: true,
                offeredActions: [OfferedActionDto(actionId: "pip.purge.cache",
                                                  requiresConfirmation: true)]
            )),
        ])
        let result = PlanApplicationResult.assemble(
            preview: preview,
            includingConfirmable: true,
            statusesByStepIndex: [
                0: .cleaned(reclaimedBytes: 1_000, human: "1.0 KB"),
                1: .cleaned(reclaimedBytes: 2_000, human: "2.0 KB"),
            ],
            stoppedEarlyReason: nil
        )
        XCTAssertTrue(result.isCompleteSuccess)
        XCTAssertEqual(result.cleanedCount, 2)
        XCTAssertEqual(result.reclaimedBytes, 3_000)
        XCTAssertFalse(result.reclaimedIsIncomplete)
    }

    // MARK: - Rendering the pre-run estimate
    //
    // What the estimate counts is asserted above; these cover only how the
    // preview words it.

    /// A single step quotes Rust's own rendering rather than re-formatting it,
    /// so the preview's figure matches the one the detail view shows for the
    /// same resource.
    func testASingleStepEstimateQuotesTheCliRendering() {
        XCTAssertEqual(
            mixedPreview().reclaimableEstimateText(includingConfirmable: false),
            "8.0 MB"
        )
    }

    /// HORO-1452, the finding itself: this used to be "12582912 bytes", sitting
    /// between a list of humanised per-item sizes above it and a humanised
    /// result line after the run.
    ///
    /// A sum is not something any CLI invocation produces — a batch is N
    /// single-item `execute` calls — so it has to be rendered here. What HORO-1312
    /// objected to was a *disagreeing* rendering, and `GlomerisByteFormat` is a
    /// port of Rust's rule under a fixture both languages assert against. So the
    /// sum is now rendered, by the producer's convention, and 4.0 MB + 8.0 MB
    /// reads as the 12.0 MB it is.
    func testASummedEstimateIsRenderedByTheSharedConvention() {
        XCTAssertEqual(
            mixedPreview().reclaimableEstimateText(includingConfirmable: true),
            "12.0 MB"
        )
    }

    /// A total assembled over an unmeasured step is worded as a floor, so the
    /// preview never states a figure it cannot stand behind.
    func testASummedEstimateOverAnUnmeasuredStepIsWordedAsAFloor() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/a", reclaimableBytes: 2_048, reclaimableHuman: "2.0 KB",
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            )),
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/b", reclaimableBytes: nil, reclaimableHuman: nil,
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            )),
        ])
        XCTAssertEqual(
            preview.reclaimableEstimateText(includingConfirmable: false),
            "at least 2.0 KB"
        )
    }

    /// No attempted step reported a size: the preview says nothing about space
    /// rather than showing a confident zero.
    func testNoMeasuredStepMeansNoEstimateSentenceAtAll() {
        let preview = PlanApplicationPreview(steps: [
            PlanApplicationStep(explain: explainReport(
                resourceId: "/Users/x/a", reclaimableBytes: nil, reclaimableHuman: nil,
                executable: true,
                offeredActions: [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                                  requiresConfirmation: false)]
            )),
        ])
        XCTAssertNil(preview.reclaimableEstimateText(includingConfirmable: false))
    }

    // MARK: - Fixtures for the entry-point gate

    /// A plan item as `llm-plan --json` reports one. Only the candidate's
    /// actionability triple matters to `snapshotSuggestsAnApplicableItem`; the
    /// rest is filled in so the DTO is a real one rather than a stub of the
    /// shape the gate happens to read.
    private func planItem(
        resourceId: String,
        policyLabel: String,
        executable: Bool,
        requiresConfirmation: Bool = false,
        refusalReason: String? = nil
    ) -> LlmPlanItemReportDto {
        let offeredActions = executable
            ? [OfferedActionDto(actionId: "cargo.clean.target_dir",
                                requiresConfirmation: requiresConfirmation)]
            : []
        return LlmPlanItemReportDto(
            resourceId: resourceId,
            policyLabel: policyLabel,
            requestedActionId: offeredActions.first?.actionId,
            priority: 1,
            modelReason: "the model's own words, which decide nothing here",
            explain: nil,
            skipReason: nil,
            candidate: DetectCandidateReportDto(
                resourceId: resourceId,
                kind: "cargo_target",
                reclaimableBytes: 1_048_576,
                reclaimableHuman: "1.0 MB",
                reclaimableBytesIsLowerBound: false,
                impactTier: nil,
                policyLabel: policyLabel,
                reasons: ["regenerable by cargo build"],
                executable: executable,
                offeredActions: offeredActions,
                refusalReason: refusalReason
            ),
            completeness: "complete",
            confidence: "high"
        )
    }
}
