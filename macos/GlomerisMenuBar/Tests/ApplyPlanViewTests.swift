//
//  ApplyPlanViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1366. `PlanApplicationTests` covers what the vocabulary decides; this
//  suite covers what the surface DOES — the two CLI call sites driven against
//  the fixture binary, and the structural properties that keep a batch from
//  being a new kind of authority.
//
//  Some assertions here read the file's source text. That is deliberate and it
//  is the same technique `AiPlanSectionViewTests` uses for the same class of
//  property: "no cancellable task can reach `execute`" and "no provider
//  credential is in a batch child's environment" are facts about which code
//  exists, and a behavioural test can only ever sample the paths it happens to
//  take. A grep catches the line that has not been written yet.
//

// No `@testable import GlomerisMenuBar`. This test target does not import the
// app module — `project.yml` compiles the individual `Sources/*.swift` files it
// needs directly into the test bundle, so every type here is already in this
// module. An `@testable import` resolves locally only when a previous app-target
// build has left a module artefact in DerivedData, and fails on a clean
// checkout; CI caught exactly that. `Combine` is for `$applyPhase`.
import Combine
import SwiftUI
import XCTest

final class ApplyPlanViewTests: XCTestCase {

    // MARK: - Fixtures

    private static let protectedRefusal = "PROTECTED: protected_credential_material"

    /// A `glomeris explain --json` body. The fixture binary echoes this
    /// verbatim, so the production code under test does the decoding.
    private static func explainJson(
        resourceId: String = "/Users/x/proj/target",
        kind: String = "cargo_target",
        policyLabel: String = "AUTO_SAFE",
        executable: Bool = true,
        requiresConfirmation: Bool = false,
        refusalReason: String? = nil
    ) -> String {
        let offered = executable
            ? #"[{"action_id": "cargo.clean.target_dir", "requires_confirmation": \#(requiresConfirmation)}]"#
            : "[]"
        let refusal = refusalReason.map { "\"\($0)\"" } ?? "null"
        return """
        {
          "resource_id": "\(resourceId)",
          "kind": "\(kind)",
          "detector": "cargo",
          "sources": ["Cargo.toml"],
          "logical_bytes": 2097152,
          "logical_human": "2.0 MB",
          "reclaimable_bytes": 8388608,
          "reclaimable_human": "8.0 MB",
          "reclaimable_bytes_is_lower_bound": false,
          "completeness": "complete",
          "confidence": "high",
          "active_use_signals": [],
          "regenerability": "regenerable",
          "policy_label": "\(policyLabel)",
          "reasons": ["regenerable by cargo build"],
          "native_cleanup_available": true,
          "native_cleanup_action_id": "cargo.clean.target_dir",
          "fingerprint_token": "opaque-token",
          "executable": \(executable),
          "offered_actions": \(offered),
          "refusal_reason": \(refusal)
        }
        """
    }

    /// `outcome` carries Rust's own discriminant — `succeeded`, not `cleaned`.
    /// The distinction matters: this app's word for the result is "cleaned",
    /// the CLI's is "succeeded", and a fixture using the app's word would pass
    /// while the real CLI's report fell through to `.failed`.
    private static func executeJson(
        outcome: String = "succeeded",
        actualBytes: String = "8388608",
        actualHuman: String = "\"8.0 MB\""
    ) -> String {
        """
        {
          "action_id": "cargo.clean.target_dir",
          "resource_id": "/Users/x/proj/target",
          "outcome": "\(outcome)",
          "failure_message": null,
          "abort_reason": null,
          "expected_reclaimed_bytes": 8388608,
          "actual_reclaimed_bytes": \(actualBytes),
          "expected_reclaimed_human": "8.0 MB",
          "actual_reclaimed_human": \(actualHuman)
        }
        """
    }

    private static func refusalJson(reason: String, message: String) -> String {
        #"{"reason": "\#(reason)", "message": "\#(message)"}"#
    }

    private func planItem(
        resourceId: String = "/Users/x/proj/target",
        policyLabel: String = "AUTO_SAFE",
        executable: Bool = true,
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
            modelReason: "the model's own words",
            explain: nil,
            skipReason: nil,
            candidate: DetectCandidateReportDto(
                resourceId: resourceId,
                kind: "cargo_target",
                reclaimableBytes: 8_388_608,
                reclaimableHuman: "8.0 MB",
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

    /// A step built without going near a process, for driving `applyPlan`
    /// directly.
    private func step(
        resourceId: String,
        executable: Bool = true,
        requiresConfirmation: Bool = false,
        refusalReason: String? = nil
    ) -> PlanApplicationStep {
        PlanApplicationStep(explain: try! JSONDecoder().decode(
            ExplainReportDto.self,
            from: Data(Self.explainJson(
                resourceId: resourceId,
                executable: executable,
                requiresConfirmation: requiresConfirmation,
                refusalReason: refusalReason
            ).utf8)
        ))
    }

    @MainActor
    private func makeView(
        plan: PlanState,
        items: [LlmPlanItemReportDto],
        stdout: String,
        exitCode: Int32 = 0,
        argvLogPath: String? = nil,
        lingerMs: Int? = nil,
        projectRoots: [String] = []
    ) throws -> ApplyPlanView {
        let defaults = Self.throwawayDefaults()
        let rootsStore = ProjectRootsStore(defaults: defaults)
        for root in projectRoots { rootsStore.addRoot(root) }
        return ApplyPlanView(
            plan: plan,
            items: items,
            client: try Self.fixtureClient(
                stdout: stdout,
                exitCode: exitCode,
                argvLogPath: argvLogPath,
                lingerMs: lingerMs
            ),
            projectRootsStore: rootsStore
        )
    }

    // MARK: - The `explain` sweep

    /// The happy path end to end: a plan item goes in, a real `explain` body
    /// comes back from a real child process, and the phase lands on a preview
    /// whose one step Glomeris is willing to run.
    @MainActor
    func testPreparingAPreviewReChecksTheSuggestionAndLandsOnReviewing() async throws {
        let plan = PlanState()
        let view = try makeView(plan: plan, items: [planItem()], stdout: Self.explainJson())

        await view.preparePreview()

        guard case .reviewing(let preview) = plan.applyPhase else {
            return XCTFail("expected .reviewing, got \(plan.applyPhase)")
        }
        XCTAssertEqual(preview.steps.count, 1)
        XCTAssertEqual(preview.steps[0].disposition, .willRun)
        XCTAssertEqual(preview.steps[0].actionId, "cargo.clean.target_dir")
        XCTAssertNil(plan.applyProgressText, "the progress line is cleared when the sweep ends")
    }

    /// AC4's first half, and the reason the sweep exists at all: the preview is
    /// built from what the CLI says NOW, not from what the plan claimed. Here
    /// the plan says the resource is AUTO_SAFE and executable and `explain`
    /// says it is PROTECTED — and the preview follows `explain`.
    @MainActor
    func testTheReCheckOverridesWhatThePlanClaimed() async throws {
        let plan = PlanState()
        let view = try makeView(
            plan: plan,
            items: [planItem(policyLabel: "AUTO_SAFE", executable: true)],
            stdout: Self.explainJson(
                policyLabel: "PROTECTED",
                executable: false,
                refusalReason: Self.protectedRefusal
            )
        )

        await view.preparePreview()

        guard case .reviewing(let preview) = plan.applyPhase else {
            return XCTFail("expected .reviewing, got \(plan.applyPhase)")
        }
        XCTAssertEqual(preview.steps[0].disposition, .skippedRefused(reason: Self.protectedRefusal))
        XCTAssertFalse(preview.hasAnythingToApply)
        XCTAssertNil(preview.steps[0].actionId)
        XCTAssertNil(preview.steps[0].fingerprintToken)
    }

    /// A resource the CLI can no longer describe becomes a stale step carrying
    /// the CLI's own message, not an error that abandons the whole preview: a
    /// plan holding one deleted resource still previews, and still applies the
    /// rest. AC6's "stale / no longer valid after revalidation".
    @MainActor
    func testAnUndescribableResourceBecomesAStaleStepRatherThanAFailedPreview() async throws {
        let plan = PlanState()
        let view = try makeView(
            plan: plan,
            items: [planItem()],
            stdout: "not json at all",
            exitCode: 3
        )

        await view.preparePreview()

        guard case .reviewing(let preview) = plan.applyPhase else {
            return XCTFail("expected .reviewing, got \(plan.applyPhase)")
        }
        XCTAssertEqual(preview.steps.count, 1)
        guard case .skippedStale(let reason) = preview.steps[0].disposition else {
            return XCTFail("expected .skippedStale, got \(preview.steps[0].disposition)")
        }
        XCTAssertTrue(reason.hasPrefix("explain:"), "the reason names the call that failed: \(reason)")
        XCTAssertTrue(
            preview.steps.allSatisfy { !$0.disposition.isRunnable },
            "a step Glomeris could not describe is never runnable"
        )
    }

    /// The opt-in is off at the start of every preview, whatever it was before.
    /// An ASK item is one Glomeris will not touch without being told to, and a
    /// control remembering "yes" from a previous plan would carry that
    /// instruction to resources the user never saw.
    @MainActor
    func testPreparingAPreviewAlwaysResetsTheConfirmationOptIn() async throws {
        let plan = PlanState()
        plan.applyIncludesConfirmable = true
        let view = try makeView(plan: plan, items: [planItem()], stdout: Self.explainJson())

        await view.preparePreview()

        XCTAssertFalse(plan.applyIncludesConfirmable)
    }

    /// Stopping the sweep abandons it rather than presenting a preview the user
    /// did not ask for and could not tell was partial.
    ///
    /// # Why this waits before cancelling
    ///
    /// This test used to create the task and cancel it immediately. Both the
    /// test and `preparePreview()` are `@MainActor`, so the task body could not
    /// begin until the test suspended at `await task.value` — by which point the
    /// cancellation had already landed and the loop broke on its first
    /// `Task.isCancelled` check, before any child was spawned. The `.idle`
    /// assertion held, but nothing else did: `applyProgressText` was never set,
    /// so asserting it nil proved nothing, and the branch that actually matters
    /// — `SectionFetchErrors.shortMessage` returning `nil` for a child killed
    /// mid-call, which is what abandons the sweep — was never executed by any
    /// test in the suite.
    ///
    /// So the fixture lingers, and this waits for the argv log to prove a child
    /// really started before cancelling. The log is the proof: the fixture
    /// writes it before producing any output, and a `SIGTERM` afterwards cannot
    /// unwrite it.
    @MainActor
    func testCancellingTheSweepMidFlightAbandonsItWithNoPreview() async throws {
        let plan = PlanState()
        let logPath = argvLogPath()
        let view = try makeView(
            plan: plan,
            items: [planItem(), planItem(resourceId: "/Users/x/other")],
            stdout: Self.explainJson(),
            argvLogPath: logPath,
            lingerMs: 3_000
        )

        let task = Task { await view.preparePreview() }
        plan.preparePreviewTask = task

        var waited = 0
        while ((try? recordedInvocations(at: logPath).count) ?? 0) == 0, waited < 150 {
            try await Task.sleep(nanoseconds: 20_000_000)
            waited += 1
        }
        let inFlight = try recordedInvocations(at: logPath)
        XCTAssertEqual(inFlight.count, 1, "a child must be in flight for this to be a mid-flight cancel")
        XCTAssertEqual(inFlight[0].first, "explain", "the sweep's only call is explain")
        XCTAssertNotNil(plan.applyProgressText, "the sweep had reached its first item")

        task.cancel()
        await task.value

        XCTAssertEqual(plan.applyPhase, .idle, "an abandoned sweep presents no preview")
        XCTAssertNil(plan.applyProgressText, "the progress line goes with the sweep")
        XCTAssertEqual(
            try recordedInvocations(at: logPath).count,
            1,
            "cancelling stops the sweep rather than letting it walk the rest of the plan"
        )
    }

    // MARK: - The `execute` loop

    @MainActor
    func testApplyingARunnableStepReportsItAsCleaned() async throws {
        let plan = PlanState()
        let preview = PlanApplicationPreview(steps: [step(resourceId: "/Users/x/proj/target")])
        let view = try makeView(plan: plan, items: [], stdout: Self.executeJson())

        await view.applyPlan(preview)

        guard case .finished(let result) = plan.applyPhase else {
            return XCTFail("expected .finished, got \(plan.applyPhase)")
        }
        XCTAssertEqual(result.cleanedCount, 1)
        XCTAssertTrue(result.isCompleteSuccess)
        XCTAssertEqual(result.reclaimedBytes, 8_388_608)
        XCTAssertNil(result.stoppedEarlyReason)
        XCTAssertNil(plan.applyProgressText)
        XCTAssertTrue(plan.applyCompletedItems.isEmpty, "the live rows fold into the result")
    }

    /// AC7 at the last possible moment. A preview whose steps are all refused
    /// runs nothing at all — no child process is spawned — and does not leave
    /// the surface claiming a batch happened.
    @MainActor
    func testApplyingAPreviewWithNothingRunnableRunsNothing() async throws {
        let plan = PlanState()
        plan.applyPhase = .reviewing(PlanApplicationPreview(steps: []))
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/.aws/credentials",
                 executable: false,
                 refusalReason: Self.protectedRefusal),
        ])
        // A fixture that would report a successful deletion if it were ever
        // spawned, so this test fails loudly rather than vacuously.
        let view = try makeView(plan: plan, items: [], stdout: Self.executeJson())

        await view.applyPlan(preview)

        guard case .reviewing = plan.applyPhase else {
            return XCTFail("the phase must not advance, got \(plan.applyPhase)")
        }
    }

    /// AC5. The ASK step is not attempted while the opt-in is off, and the same
    /// preview does attempt it once the user has opted in — so the control is
    /// load-bearing rather than decorative.
    @MainActor
    func testAConfirmableStepIsOnlyRunAfterTheUserOptsIn() async throws {
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/Library/Caches/pip", requiresConfirmation: true),
        ])

        let withoutOptIn = PlanState()
        withoutOptIn.applyPhase = .reviewing(preview)
        let refusing = try makeView(plan: withoutOptIn, items: [], stdout: Self.executeJson())
        await refusing.applyPlan(preview)
        guard case .reviewing = withoutOptIn.applyPhase else {
            return XCTFail("nothing was authorised, so nothing should have run")
        }

        let withOptIn = PlanState()
        withOptIn.applyIncludesConfirmable = true
        let running = try makeView(plan: withOptIn, items: [], stdout: Self.executeJson())
        await running.applyPlan(preview)
        guard case .finished(let result) = withOptIn.applyPhase else {
            return XCTFail("expected .finished, got \(withOptIn.applyPhase)")
        }
        XCTAssertEqual(result.cleanedCount, 1)
    }

    /// The lock. A `busy` refusal means a DIFFERENT invocation holds the
    /// execution lock, so every remaining item would refuse identically — the
    /// batch stops, names why, and the unreached item says it was never
    /// attempted rather than borrowing the refusal.
    @MainActor
    func testABusyRefusalStopsTheBatchAndTheUnreachedStepSaysSo() async throws {
        let plan = PlanState()
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/a"),
            step(resourceId: "/Users/x/b"),
        ])
        let view = try makeView(
            plan: plan,
            items: [],
            stdout: Self.refusalJson(reason: "busy", message: "another Glomeris invocation is running"),
            exitCode: 3
        )

        await view.applyPlan(preview)

        guard case .finished(let result) = plan.applyPhase else {
            return XCTFail("expected .finished, got \(plan.applyPhase)")
        }
        XCTAssertNotNil(result.stoppedEarlyReason)
        XCTAssertFalse(result.isCompleteSuccess)
        XCTAssertEqual(result.items.count, 2)
        XCTAssertEqual(
            result.items[1].status,
            .notAttempted(reason: "Not attempted — the batch stopped before reaching this item.")
        )
    }

    /// Every other refusal continues, which is the existing deterministic
    /// execution policy: one resource being refused says nothing about the
    /// next, and stopping would silently narrow a batch the user authorised.
    /// Both steps are attempted here, and both are honestly reported as not
    /// cleaned.
    @MainActor
    func testANonBusyRefusalDoesNotStopTheBatch() async throws {
        let plan = PlanState()
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/a"),
            step(resourceId: "/Users/x/b"),
        ])
        let view = try makeView(
            plan: plan,
            items: [],
            stdout: Self.refusalJson(
                reason: "resource_identity_changed",
                message: "the resource changed between planning and execution"
            ),
            exitCode: 3
        )

        await view.applyPlan(preview)

        guard case .finished(let result) = plan.applyPhase else {
            return XCTFail("expected .finished, got \(plan.applyPhase)")
        }
        XCTAssertNil(result.stoppedEarlyReason)
        XCTAssertEqual(result.cleanedCount, 0)
        XCTAssertEqual(result.notCleanedCount, 2)
        XCTAssertFalse(result.isCompleteSuccess)
        for item in result.items {
            XCTAssertTrue(
                item.status.message.contains("changed between planning and execution"),
                "each row keeps the CLI's own words: \(item.status.message)"
            )
        }
    }

    /// AC8 through the real loop rather than through `assemble` alone: a batch
    /// where nothing succeeded does not report success, and the headline says
    /// how many did not run.
    @MainActor
    func testAFailedBatchNeverReportsItselfSuccessful() async throws {
        let plan = PlanState()
        let preview = PlanApplicationPreview(steps: [step(resourceId: "/Users/x/a")])
        let view = try makeView(plan: plan, items: [], stdout: "not json", exitCode: 1)

        await view.applyPlan(preview)

        guard case .finished(let result) = plan.applyPhase else {
            return XCTFail("expected .finished, got \(plan.applyPhase)")
        }
        XCTAssertFalse(result.isCompleteSuccess)
        XCTAssertEqual(result.cleanedCount, 0)
        XCTAssertEqual(result.headline, "Nothing was cleaned — 1 item did not run.")
        XCTAssertNil(result.reclaimedText, "no measured bytes means no reclaimed figure at all")
    }

    /// The batch is `.applying` while it runs and only then `.finished`, so the
    /// surface can never show a live run beside a finished result.
    @MainActor
    func testThePhaseIsApplyingWhileTheLoopRuns() async throws {
        let plan = PlanState()
        let preview = PlanApplicationPreview(steps: [step(resourceId: "/Users/x/a")])
        let view = try makeView(plan: plan, items: [], stdout: Self.executeJson())

        var sawApplying = false
        let observer = plan.$applyPhase.sink { phase in
            if case .applying = phase { sawApplying = true }
        }
        defer { observer.cancel() }

        await view.applyPlan(preview)

        XCTAssertTrue(sawApplying)
        guard case .finished = plan.applyPhase else {
            return XCTFail("expected .finished, got \(plan.applyPhase)")
        }
    }

    // MARK: - What the batch actually asks the CLI for
    //
    // Everything above drives the real binary and then reads the state the
    // surface ended in — which is the right way round for an outcome, and blind
    // to the request. The fixture answers before it looks at `argv`, so a batch
    // that sent the wrong arguments would still get the stdout the test supplied
    // and still land in the phase the test expects.
    //
    // That blindness matters for one argument in particular. `--confirm-ask` is
    // the whole of AC5 at the wire: drop it and
    // `testAConfirmableStepIsOnlyRunAfterTheUserOptsIn` passes unchanged, while
    // an opted-in batch asks Rust to clean an ASK resource with no consent
    // recorded. (Rust refuses — `ask_no_consent`, exit 3, verified against the
    // release binary — so the product would not delete anything it should not.
    // But the app would be attempting it, and the refusal would arrive as a
    // per-item failure the user cannot act on.) The mirror case matters as much:
    // an AUTO_SAFE step must NOT carry a consent flag, or the batch would be
    // claiming consent nobody gave.
    //
    // So these tests read `GLOMERIS_FIXTURE_ARGV_LOG` and assert the argument
    // arrays themselves. They are the only tests in this suite that can fail on
    // a request rather than on a response.

    /// An AUTO_SAFE step: the `execute` the single-item Clean would make, and
    /// neither consent flag.
    @MainActor
    func testAnAutoSafeStepIsExecutedWithNoConsentFlags() async throws {
        let plan = PlanState()
        let logPath = argvLogPath()
        let preview = PlanApplicationPreview(steps: [step(resourceId: "/Users/x/proj/target")])
        let view = try makeView(
            plan: plan, items: [], stdout: Self.executeJson(), argvLogPath: logPath
        )

        await view.applyPlan(preview)

        let invocations = try recordedInvocations(at: logPath)
        XCTAssertEqual(invocations.count, 1, "one authorised step, one child")
        let argv = invocations[0]
        XCTAssertEqual(argv.first, "execute")
        XCTAssertTrue(argv.contains("--action-id"))
        XCTAssertEqual(argv.value(after: "--action-id"), "cargo.clean.target_dir")
        XCTAssertEqual(argv.value(after: "--resource-id"), "/Users/x/proj/target")
        XCTAssertTrue(argv.contains("--json"))
        XCTAssertTrue(argv.contains("--progress-json"))
        XCTAssertFalse(
            argv.contains("--confirm-ask"),
            "an AUTO_SAFE step must not claim a confirmation nobody gave: \(argv)"
        )
        XCTAssertFalse(
            argv.contains("--observed-fingerprint"),
            "the token is only sent with the consent it pairs with: \(argv)"
        )
    }

    /// AC5 at the wire. An opted-in ASK step carries `--confirm-ask` AND
    /// `--observed-fingerprint` with the token from its own `explain` report —
    /// `execute` requires the pair, and either one alone is a usage error
    /// (exit 2, no JSON body, verified against the release binary).
    @MainActor
    func testAnOptedInConfirmableStepSendsConsentAndTheTokenTogether() async throws {
        let plan = PlanState()
        plan.applyIncludesConfirmable = true
        let logPath = argvLogPath()
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/Library/Caches/pip", requiresConfirmation: true),
        ])
        let view = try makeView(
            plan: plan, items: [], stdout: Self.executeJson(), argvLogPath: logPath
        )

        await view.applyPlan(preview)

        let invocations = try recordedInvocations(at: logPath)
        XCTAssertEqual(invocations.count, 1)
        let argv = invocations[0]
        XCTAssertTrue(argv.contains("--confirm-ask"), "AC5 at the wire: \(argv)")
        XCTAssertEqual(
            argv.value(after: "--observed-fingerprint"), "opaque-token",
            "the token is the one `explain` reported for this resource: \(argv)"
        )
    }

    /// The step that asks first and the step that does not, in one batch: the
    /// consent flag goes to exactly one of them. A batch-wide flag would pass
    /// both single-step tests above and fail this one.
    @MainActor
    func testConsentIsSentPerStepAndNotForTheWholeBatch() async throws {
        let plan = PlanState()
        plan.applyIncludesConfirmable = true
        let logPath = argvLogPath()
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/Library/Caches/pip", requiresConfirmation: true),
            step(resourceId: "/Users/x/proj/target"),
        ])
        let view = try makeView(
            plan: plan, items: [], stdout: Self.executeJson(), argvLogPath: logPath
        )

        await view.applyPlan(preview)

        let invocations = try recordedInvocations(at: logPath)
        XCTAssertEqual(invocations.count, 2, "two authorised steps, two children, in plan order")
        XCTAssertEqual(invocations[0].value(after: "--resource-id"), "/Users/x/Library/Caches/pip")
        XCTAssertTrue(invocations[0].contains("--confirm-ask"))
        XCTAssertEqual(invocations[1].value(after: "--resource-id"), "/Users/x/proj/target")
        XCTAssertFalse(
            invocations[1].contains("--confirm-ask"),
            "the AUTO_SAFE step must not inherit the ASK step's consent: \(invocations[1])"
        )
    }

    /// A refused step is not executed with a made-up action id, and not executed
    /// at all: no child process is spawned for it. The absence of a log file is
    /// the assertion — nothing ran, so nothing wrote one.
    @MainActor
    func testARefusedStepSpawnsNoChildAtAll() async throws {
        let plan = PlanState()
        plan.applyPhase = .reviewing(PlanApplicationPreview(steps: []))
        let logPath = argvLogPath()
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/.aws/credentials",
                 executable: false,
                 refusalReason: Self.protectedRefusal),
        ])
        let view = try makeView(
            plan: plan, items: [], stdout: Self.executeJson(), argvLogPath: logPath
        )

        await view.applyPlan(preview)

        XCTAssertFalse(
            FileManager.default.fileExists(atPath: logPath),
            "a PROTECTED resource must not reach a child process"
        )
    }

    /// The configured project roots reach both calls. They scope what Glomeris
    /// will look at, so a batch that dropped them would be re-checking and
    /// mutating against a different resource set than the plan was made from.
    @MainActor
    func testProjectRootsAreForwardedToBothTheReCheckAndTheExecution() async throws {
        let sweepLog = argvLogPath()
        let sweepPlan = PlanState()
        let sweep = try makeView(
            plan: sweepPlan, items: [planItem()], stdout: Self.explainJson(),
            argvLogPath: sweepLog, projectRoots: ["/Users/x/proj"]
        )
        await sweep.preparePreview()
        let explainArgv = try recordedInvocations(at: sweepLog)
        XCTAssertEqual(explainArgv.count, 1)
        XCTAssertEqual(explainArgv[0].first, "explain")
        XCTAssertEqual(explainArgv[0].value(after: "--project-root"), "/Users/x/proj")

        let executeLog = argvLogPath()
        let executePlan = PlanState()
        let execute = try makeView(
            plan: executePlan, items: [], stdout: Self.executeJson(),
            argvLogPath: executeLog, projectRoots: ["/Users/x/proj"]
        )
        await execute.applyPlan(PlanApplicationPreview(steps: [step(resourceId: "/Users/x/proj/target")]))
        let executeArgv = try recordedInvocations(at: executeLog)
        XCTAssertEqual(executeArgv.count, 1)
        XCTAssertEqual(executeArgv[0].value(after: "--project-root"), "/Users/x/proj")
    }

    /// The collapse, at the wire. A resource the plan names twice is re-checked
    /// once, so the preview holds one step rather than two identical ones —
    /// which is what keeps the estimate and the result honest. The unit-level
    /// properties are in `PlanApplicationTests`; this is the assertion that the
    /// sweep actually uses them.
    @MainActor
    func testARepeatedResourceIsReCheckedOnlyOnce() async throws {
        let plan = PlanState()
        let logPath = argvLogPath()
        let view = try makeView(
            plan: plan,
            items: [planItem(), planItem(), planItem(resourceId: "/Users/x/other")],
            stdout: Self.explainJson(),
            argvLogPath: logPath
        )

        await view.preparePreview()

        let invocations = try recordedInvocations(at: logPath)
        XCTAssertEqual(
            invocations.count, 2,
            "three plan rows naming two resources is two `explain` calls: \(invocations)"
        )
        guard case .reviewing(let preview) = plan.applyPhase else {
            return XCTFail("expected .reviewing, got \(plan.applyPhase)")
        }
        XCTAssertEqual(preview.steps.count, 2)
    }

    // MARK: - Wording

    func testTheSummaryNamesWhatWillRunAndWhatWillNot() {
        let preview = PlanApplicationPreview(steps: [
            step(resourceId: "/Users/x/a"),
            step(resourceId: "/Users/x/ask", requiresConfirmation: true),
            step(resourceId: "/Users/x/no", executable: false, refusalReason: Self.protectedRefusal),
        ])
        XCTAssertEqual(
            ApplyPlanView.summaryText(preview, includingConfirmable: false),
            "1 item will run, 1 asks for confirmation and is not included, 1 will be skipped."
        )
        XCTAssertEqual(
            ApplyPlanView.summaryText(preview, includingConfirmable: true),
            "2 items will run, 1 will be skipped."
        )
    }

    func testTheApplyButtonNamesHowManyItemsItWillRun() {
        XCTAssertEqual(ApplyPlanView.applyButtonTitle(1), "Apply 1 item")
        XCTAssertEqual(ApplyPlanView.applyButtonTitle(4), "Apply 4 items")
    }

    /// The opt-in says what the items it authorises have in common, so a user
    /// reading only the control still knows these are the ones Glomeris would
    /// otherwise ask about individually.
    func testTheOptInNamesWhatItAuthorises() {
        XCTAssertEqual(
            ApplyPlanView.confirmableToggleTitle(1),
            "Also apply the 1 item that asks for confirmation"
        )
        XCTAssertEqual(
            ApplyPlanView.confirmableToggleTitle(3),
            "Also apply the 3 items that ask for confirmation"
        )
    }

    /// A measured total is appended to the honest count, never shown instead of
    /// it. A headline reading only "Reclaimed 8.0 MB" would be true and
    /// misleading in the same breath when four other items were refused.
    func testTheResultHeadlineKeepsTheCountWhenItAlsoHasAFigure() {
        let result = PlanApplicationResult(
            items: [
                PlanApplicationItemResult(
                    resourceId: "/Users/x/a",
                    kind: "cargo_target",
                    status: .cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB")
                ),
                PlanApplicationItemResult(
                    resourceId: "/Users/x/b",
                    kind: "credential_material",
                    status: .notAttempted(reason: Self.protectedRefusal)
                ),
            ],
            stoppedEarlyReason: nil
        )
        let headline = ApplyPlanView.resultHeadline(result)
        XCTAssertTrue(headline.contains("Cleaned 1 of 2 items"), headline)
        XCTAssertTrue(headline.contains("1 did not run"), headline)
        XCTAssertTrue(headline.contains("8.0 MB"), headline)
    }

    func testAResultWithNoMeasuredBytesShowsOnlyTheCount() {
        let result = PlanApplicationResult(
            items: [
                PlanApplicationItemResult(
                    resourceId: "/Users/x/a",
                    kind: "cargo_target",
                    status: .failed(message: "the directory could not be removed")
                ),
            ],
            stoppedEarlyReason: nil
        )
        XCTAssertEqual(ApplyPlanView.resultHeadline(result), result.headline)
    }

    // MARK: - Accessibility

    /// Every row is one element with one spoken sentence, because a VoiceOver
    /// user swiping through a preview needs the resource, its size, its safety
    /// class and whether it will run as one utterance rather than four
    /// unlabelled fragments. Same shape as the candidates list's rows.
    func testAStepIsSpokenAsOneSentenceCarryingItsSafetyAndActionability() {
        let refused = step(
            resourceId: "/Users/x/.aws/credentials",
            executable: false,
            refusalReason: Self.protectedRefusal
        )
        let spoken = ApplyPlanView.stepAccessibilityLabel(refused, group: "Will be skipped")

        XCTAssertTrue(spoken.hasPrefix("Will be skipped:"), spoken)
        XCTAssertTrue(spoken.contains(GlomerisVocabulary.impactAxis), spoken)
        XCTAssertTrue(spoken.contains(GlomerisVocabulary.safetyAxis), spoken)
        XCTAssertTrue(spoken.contains(CandidateActionability.axis), spoken)
        // The reason is spoken, not only shown — HORO-1323's rule.
        XCTAssertTrue(spoken.contains(Self.protectedRefusal), spoken)
        XCTAssertTrue(spoken.contains("/Users/x/.aws/credentials"), spoken)
    }

    /// A result row speaks its outcome on the shared axis, so "did this run"
    /// is answerable without sight and without the surrounding layout.
    func testAResultRowIsSpokenWithItsOutcomeAxis() {
        let item = PlanApplicationItemResult(
            resourceId: "/Users/x/proj/target",
            kind: "cargo_target",
            status: .cleaned(reclaimedBytes: 8_388_608, human: "8.0 MB")
        )
        let spoken = ApplyPlanView.resultAccessibilityLabel(item)
        XCTAssertTrue(spoken.contains(GlomerisVocabulary.outcomeAxis), spoken)
        XCTAssertTrue(spoken.contains("/Users/x/proj/target"), spoken)
    }

    // MARK: - Structural guards
    //
    // Facts about which code exists. See the file header for why these are
    // greps.

    /// `execute` must not be reachable from anything cancellable: a `SIGTERM`
    /// partway through a deletion leaves the filesystem in a state neither this
    /// app nor the audit log could describe. The only stored task handle in this
    /// file is the `explain` sweep's, and `applyTask` does not exist on
    /// `PlanState` at all — so there is nothing for a future Stop button to
    /// reach for.
    func testNoCancellableTaskCanReachTheExecuteLoop() throws {
        let source = try strippedSource()

        XCTAssertEqual(
            occurrences(of: "= Task {", in: source), 1,
            "exactly one task handle is stored, and it is the explain sweep's"
        )
        XCTAssertTrue(source.contains("plan.preparePreviewTask = Task {"))
        XCTAssertFalse(source.contains("applyTask"))

        // `.task {}` is cancelled by SwiftUI on disappear, and this panel
        // disappears whenever focus moves. `.onAppear` and `Timer` would start
        // a batch nobody asked for.
        XCTAssertFalse(source.contains(".task {"))
        XCTAssertFalse(source.contains(".onAppear"))
        XCTAssertFalse(source.contains("Timer("))
        XCTAssertFalse(source.contains("Task.sleep"))
    }

    /// The batch spawns no provider call, so no API key may be in the
    /// environment of any child process it creates. `withEnvironment` and the
    /// settings store are the two ways one could get there, and neither is
    /// named here — which is also why the view's initialiser does not take a
    /// settings store to be threaded through by accident.
    func testNoProviderCredentialCanReachABatchChildProcess() throws {
        let source = try strippedSource()
        XCTAssertFalse(source.contains("withEnvironment"))
        XCTAssertFalse(source.contains("settingsStore"))
        XCTAssertFalse(source.contains("GlomerisLlmSettingsStore"))
        XCTAssertFalse(source.contains("childEnvironment"))
        XCTAssertFalse(source.contains("CredentialStore"))
    }

    /// Every mutation goes through the one pure argument builder the
    /// single-item Clean uses. This file never names the `execute` subcommand
    /// itself, never assembles `--confirm-ask` or `--observed-fingerprint` by
    /// hand, and has exactly one place that turns a step into a command.
    func testEveryMutationGoesThroughTheSharedArgumentBuilder() throws {
        let source = try strippedSource()

        XCTAssertEqual(occurrences(of: "buildExecuteArguments(", in: source), 1)
        XCTAssertFalse(source.contains("\"execute\""))
        XCTAssertFalse(source.contains("--confirm-ask"))
        XCTAssertFalse(source.contains("--observed-fingerprint"))
        XCTAssertFalse(source.contains("--action-id"))
        // No dry-run flag is invented for a preview: `execute` has none, and the
        // preview is built from `explain` precisely because a fake dry run would
        // be a second opinion about what may be deleted.
        XCTAssertFalse(source.contains("--dry-run"))
        // And exactly one place spawns a mutating child.
        XCTAssertEqual(occurrences(of: "client.runRaw(", in: source), 1)
    }

    /// The batch decides nothing about safety. It must not branch on a policy
    /// label, re-derive actionability from a boolean, or reorder the model's
    /// list — `scripts/check-no-policy-label-branching.sh` covers the first
    /// repo-wide, and this pins the rest for the one file that could most
    /// plausibly acquire them.
    func testTheBatchAddsNoJudgmentOfItsOwn() throws {
        let source = try strippedSource()
        XCTAssertFalse(source.contains("AUTO_SAFE"))
        XCTAssertFalse(source.contains("PROTECTED"))
        XCTAssertFalse(source.contains("policyLabel =="))
        XCTAssertFalse(source.contains(".sorted"))
        XCTAssertFalse(source.contains("item.candidate.executable"))
        XCTAssertFalse(source.contains("priority"))
    }

    /// Rows are keyed by position. A provider may name the same resource twice,
    /// and `ForEach` over duplicate identities misrenders — which is why
    /// neither `PlanApplicationStep` nor `PlanApplicationItemResult` is
    /// `Identifiable`.
    func testRowsAreKeyedByPositionRatherThanResourceId() throws {
        let source = try strippedSource()
        XCTAssertEqual(occurrences(of: "id: \\.offset", in: source), 2)
        XCTAssertFalse(source.contains("id: \\.resourceId"))
    }

    /// No modal. Presenting one over a non-activating `MenuBarExtra` window can
    /// order the panel out from under it, which was HORO-1357's whole symptom.
    /// The in-panel review is the consent surface.
    func testTheReviewIsInThePanelAndNotAModal() throws {
        let source = try strippedSource()
        XCTAssertFalse(source.contains(".alert("))
        XCTAssertFalse(source.contains(".sheet("))
        XCTAssertFalse(source.contains(".confirmationDialog("))
        XCTAssertFalse(source.contains("NSAlert"))
    }

    /// HORO-1367 cut the visible caption under "Apply plan…" to one sentence
    /// and then put the other one back, because what it had dropped survived
    /// only in the button's accessibility hint — and a hint is not read aloud
    /// when hints are switched off, and is never shown to a sighted user at
    /// all. Both facts therefore have to be *visible*: that every suggestion
    /// is re-checked before anything is shown, which is the evidence-first
    /// promise of the surface, and that nothing runs without consent.
    ///
    /// Asserted on the whole `Text(…)` literal rather than on the two
    /// sentences separately, because a substring search cannot tell visible
    /// copy from an accessibility string: with two loose `contains` checks
    /// this test would pass just as happily if the caption and the hint were
    /// swapped, which is the arrangement it exists to rule out.
    func testTheApplyEntryPointStatesBothPromisesOnScreenAndInFullToVoiceOver() throws {
        let source = try strippedSource()

        XCTAssertTrue(
            source.contains(
                "Text(\"Re-checks every suggestion first. Nothing runs until you confirm.\")"
            ),
            "both the re-check promise and the consent promise must be on screen, in a Text and "
                + "not only in an accessibility hint"
        )
        XCTAssertTrue(
            source.contains("Re-checks every suggestion against Glomeris"),
            "the hint carries the fuller sentence, since a VoiceOver user gets no second glance"
        )
        XCTAssertTrue(
            source.contains("Nothing is deleted until you confirm."),
            "the hint must carry the consent promise in full"
        )
        // The ellipsis is what tells a sighted user there is a step in
        // between, independently of the caption saying so.
        XCTAssertTrue(
            source.contains("Button(\"Apply plan…\")"),
            "the ellipsis promises the review step"
        )
    }

    // MARK: - Reading this file's own source

    private func strippedSource() throws -> String {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/ApplyPlanView.swift")
        return strippedOfComments(try String(contentsOf: url, encoding: .utf8))
    }

    /// Whole-line comments only, so a `//` inside a string literal is left
    /// alone — the same rule the other source-reading suites in this target use.
    private func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    private func occurrences(of needle: String, in haystack: String) -> Int {
        haystack.components(separatedBy: needle).count - 1
    }

    // MARK: - The fixture binary
    //
    // The same compiled Mach-O helper the other suites build, under the same
    // revision-suffixed name on purpose — a differently-named copy would be
    // built once by whichever suite ran first and then never rebuilt when the
    // `.c` changed.
    //
    // Driven through `GLOMERIS_FIXTURE_STDOUT` / `GLOMERIS_FIXTURE_EXIT` rather
    // than argv, because here the arguments are chosen by the production code
    // under test — which is the point of these tests.

    private static func fixtureBinary() throws -> URL {
        let binaryURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-client-fixture-helper-v4")
        guard !FileManager.default.isExecutableFile(atPath: binaryURL.path) else { return binaryURL }

        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .appendingPathComponent("Fixtures/glomeris_fixture_helper.c")
        let clang = Process()
        clang.executableURL = URL(fileURLWithPath: "/usr/bin/clang")
        clang.arguments = ["-O0", "-o", binaryURL.path, sourceURL.path]
        try clang.run()
        clang.waitUntilExit()
        return binaryURL
    }

    /// A client pinned to the fixture, carrying the fixture's script in its own
    /// environment — which is exactly how the production batch spawns, since it
    /// never calls `withEnvironment`.
    ///
    /// `argvLogPath` turns on `GLOMERIS_FIXTURE_ARGV_LOG`, which is how a test
    /// sees what production actually asked the CLI for. Without it the fixture
    /// answers identically to every argument array, so a test asserting only on
    /// the outcome passes whether or not `--confirm-ask` was sent — see
    /// `recordedInvocations`.
    ///
    /// `lingerMs` makes the child outlive its own output, so a cancellation can
    /// land while a call is genuinely in flight.
    private static func fixtureClient(
        stdout: String,
        exitCode: Int32 = 0,
        argvLogPath: String? = nil,
        lingerMs: Int? = nil
    ) throws -> GlomerisClient {
        var environment = [
            "GLOMERIS_FIXTURE_STDOUT": stdout,
            "GLOMERIS_FIXTURE_EXIT": String(exitCode),
        ]
        if let argvLogPath { environment["GLOMERIS_FIXTURE_ARGV_LOG"] = argvLogPath }
        if let lingerMs { environment["GLOMERIS_FIXTURE_LINGER_MS"] = String(lingerMs) }
        return GlomerisClient(executableURL: try fixtureBinary(), environment: environment)
    }

    /// A fresh path for one test's argv log. Not `temporaryDirectory` plus a
    /// fixed name: the fixture *appends*, so a shared path would accumulate
    /// across tests and across runs.
    private func argvLogPath() -> String {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("glomeris-argv-\(UUID().uuidString).log")
            .path
    }

    /// Each invocation the fixture recorded, as its argument array, in order.
    ///
    /// The fixture writes one argument per line and a blank line between
    /// invocations, and includes `argv[0]` — dropped here, since which binary
    /// ran is `GlomerisClient`'s business and every assertion below is about the
    /// arguments the batch chose.
    private func recordedInvocations(at path: String) throws -> [[String]] {
        let contents = try String(contentsOf: URL(fileURLWithPath: path), encoding: .utf8)
        return contents
            .components(separatedBy: "\n\n")
            .map { $0.split(separator: "\n").map(String.init) }
            .filter { !$0.isEmpty }
            .map { Array($0.dropFirst()) }
    }

    /// Throwaway defaults, so no test reads or writes the app's own suite —
    /// which on this machine belongs to a running app.
    private static func throwawayDefaults() -> UserDefaults {
        UserDefaults(suiteName: "dev.glomeris.tests.applyplan.\(UUID().uuidString)")!
    }
}

private extension Array where Element == String {
    /// The argument immediately after `flag`, or `nil` when the flag is absent
    /// or is the last element.
    ///
    /// Positional rather than a `--flag=value` split, because
    /// `buildExecuteArguments` emits separate elements and `clap` is being asked
    /// for exactly that shape.
    func value(after flag: String) -> String? {
        guard let index = firstIndex(of: flag), index + 1 < count else { return nil }
        return self[index + 1]
    }
}
