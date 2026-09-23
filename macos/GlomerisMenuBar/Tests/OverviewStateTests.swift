//
//  OverviewStateTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1365: regression tests at the state/view-model boundary, for the
//  founder report "open one suggested candidate, press back, and the scan and
//  the AI plan are gone".
//
//  What these tests can and cannot prove
//  -------------------------------------
//  They prove the two things the bug was actually made of: that a completed
//  scan and a completed plan are reachable only by the Refresh and Ask
//  buttons, and that building, rebuilding or discarding the views that draw
//  them — which is what a drill-down and a back press do — cannot disturb
//  them. Constructing a `View` struct is exactly the operation SwiftUI
//  performs every time it re-evaluates a body, so "construct the card again
//  and check the store" is the real mechanism under test, not a stand-in.
//
//  They cannot prove SwiftUI's own lifecycle: whether a `MenuBarExtra(.window)`
//  panel tears its content down on dismissal, and whether a future
//  re-presentation would unmount the cards, are facts about the framework that
//  no unit test in this target can exercise. That is why AC9 also requires a
//  mechanical run against the real built app, and why the fix does not rely on
//  the cards staying mounted in the first place.
//

import SwiftUI
import XCTest

final class OverviewStateTests: XCTestCase {
    // MARK: - AC1–AC4: a drill-down and a return change nothing

    /// The founder's exact sequence, at the boundary these tests can reach:
    /// record the candidate set and the plan, drill into one candidate, come
    /// back, and compare. The views are rebuilt around the stores on the way
    /// in and on the way out, because that is what the shell does.
    func testADrillDownAndReturnLeavesTheScanAndThePlanExactlyAsTheyWere() {
        let scan = ScanState()
        let plan = PlanState()

        let recordedCandidates = [
            candidate(resourceId: "/tmp/a/node_modules"),
            candidate(resourceId: "/tmp/b/target", policyLabel: "ASK"),
            candidate(resourceId: "/tmp/c/.terraform", policyLabel: "PROTECTED"),
        ]
        let scannedAt = Date(timeIntervalSince1970: 1_700_000_000)
        scan.candidates = recordedCandidates
        scan.lastScannedAt = scannedAt

        let recordedPlan = planReport(resourceIds: ["/tmp/b/target", "/tmp/a/node_modules"])
        let plannedAt = Date(timeIntervalSince1970: 1_700_000_060)
        plan.outcome = .plan(recordedPlan)
        plan.lastPlannedAt = plannedAt

        var navigation = CandidateDetailNavigation()
        buildOverview(scan: scan, plan: plan)

        navigation.open("/tmp/b/target")
        XCTAssertTrue(navigation.isShowingDetail)
        buildOverview(scan: scan, plan: plan)

        navigation.back()
        XCTAssertFalse(navigation.isShowingDetail)
        buildOverview(scan: scan, plan: plan)

        // AC3: the candidate set is identity-equivalent at the product-model
        // level — same resources, same count, same order.
        XCTAssertEqual(
            scan.candidates.map(\.resourceId),
            recordedCandidates.map(\.resourceId),
            "returning from a detail must not change which resources are listed"
        )
        XCTAssertEqual(scan.candidates, recordedCandidates)
        XCTAssertEqual(scan.lastScannedAt, scannedAt, "the scan timestamp is part of the state")

        // AC4: plan count, order and metadata unchanged.
        XCTAssertEqual(plan.outcome, .plan(recordedPlan))
        XCTAssertEqual(
            planItems(plan.outcome).map(\.resourceId),
            ["/tmp/b/target", "/tmp/a/node_modules"],
            "the model's own ordering must survive navigation"
        )
        XCTAssertEqual(plan.lastPlannedAt, plannedAt, "the requested-at stamp is plan metadata")

        // And the overview must not have fallen back to either empty state.
        XCTAssertFalse(scan.candidates.isEmpty, "this is the 'No scan yet' regression")
        XCTAssertNotNil(plan.outcome, "this is the 'No plan yet' regression")
    }

    /// AC5: repeated detail→back across several candidates neither clears nor
    /// recomputes either state. The founder hit this on the *second*
    /// candidate, so once is not enough of a test.
    func testRepeatedDrillDownsAcrossSeveralCandidatesNeverClearEitherState() {
        let scan = ScanState()
        let plan = PlanState()
        let recorded = (0..<5).map { candidate(resourceId: "/tmp/p\($0)/node_modules") }
        scan.candidates = recorded
        scan.lastScannedAt = Date(timeIntervalSince1970: 1_700_000_000)
        let recordedPlan = planReport(resourceIds: recorded.map(\.resourceId))
        plan.outcome = .plan(recordedPlan)

        var navigation = CandidateDetailNavigation()
        for resourceId in recorded.map(\.resourceId) {
            navigation.open(resourceId)
            buildOverview(scan: scan, plan: plan)
            navigation.back()
            buildOverview(scan: scan, plan: plan)

            XCTAssertEqual(scan.candidates, recorded, "cleared while visiting \(resourceId)")
            XCTAssertEqual(plan.outcome, .plan(recordedPlan), "plan lost while visiting \(resourceId)")
        }

        XCTAssertEqual(scan.candidates.count, 5)
        XCTAssertEqual(planItems(plan.outcome).count, 5)
    }

    /// The view controls are state too. A filter or an order that silently
    /// resets on a drill-down changes which candidates are on screen when the
    /// user comes back, which is the same defect in a quieter form.
    func testTheFilterAndOrderAlsoSurviveADrillDownAndReturn() {
        let scan = ScanState()
        scan.candidates = [candidate()]
        scan.sortOrder = .path
        scan.safetyFilter = .autoSafe

        var navigation = CandidateDetailNavigation()
        navigation.open("/tmp/example/target")
        buildOverview(scan: scan, plan: PlanState())
        navigation.back()
        buildOverview(scan: scan, plan: PlanState())

        XCTAssertEqual(scan.sortOrder, .path)
        XCTAssertEqual(scan.safetyFilter, .autoSafe)
    }

    // MARK: - AC6/AC7: the user can still replace either result

    /// The fix must not have made the state immovable — an explicit Refresh
    /// replaces a scan outright rather than merging into it.
    func testAnExplicitRefreshReplacesTheScanRatherThanAddingToIt() {
        let scan = ScanState()
        scan.candidates = [candidate(resourceId: "/tmp/old/target")]
        scan.lastScannedAt = Date(timeIntervalSince1970: 1_700_000_000)

        // What `runDetect()` does on success, in the same order.
        scan.candidates = [candidate(resourceId: "/tmp/new/node_modules")]
        let rescannedAt = Date(timeIntervalSince1970: 1_700_009_999)
        scan.lastScannedAt = rescannedAt

        XCTAssertEqual(scan.candidates.map(\.resourceId), ["/tmp/new/node_modules"])
        XCTAssertEqual(scan.lastScannedAt, rescannedAt)
    }

    /// The same for an explicit Ask.
    func testAnExplicitAskReplacesThePlanRatherThanAddingToIt() {
        let plan = PlanState()
        plan.outcome = .plan(planReport(resourceIds: ["/tmp/old/target"]))

        plan.outcome = .plan(planReport(resourceIds: ["/tmp/new/node_modules"]))
        let askedAt = Date(timeIntervalSince1970: 1_700_009_999)
        plan.lastPlannedAt = askedAt

        XCTAssertEqual(planItems(plan.outcome).map(\.resourceId), ["/tmp/new/node_modules"])
        XCTAssertEqual(plan.lastPlannedAt, askedAt)
    }

    /// A failed Ask is additive — it must not take an earlier usable plan with
    /// it, which is the rule the card's own error rendering already follows.
    func testAFailedAskLeavesTheEarlierPlanInPlace() {
        let plan = PlanState()
        let earlier = planReport(resourceIds: ["/tmp/a/node_modules"])
        plan.outcome = .plan(earlier)
        plan.lastPlannedAt = Date(timeIntervalSince1970: 1_700_000_000)

        // What `runLlmPlan()` does in its `catch`: set the message, touch
        // neither the outcome nor the stamp.
        plan.lastErrorMessage = "could not reach the provider"

        XCTAssertEqual(plan.outcome, .plan(earlier))
        XCTAssertEqual(plan.lastPlannedAt, Date(timeIntervalSince1970: 1_700_000_000))
    }

    // MARK: - Structure: why this cannot come back

    /// The state is owned by the scene, above every view that can be built,
    /// hidden, removed or re-presented.
    func testTheScanAndThePlanAreOwnedByTheAppSceneAndNotByAnyView() throws {
        let app = try readSource("GlomerisMenuBarApp.swift")
        XCTAssertTrue(
            app.contains("@StateObject private var scan = ScanState()"),
            "the scan must be created once, by the scene"
        )
        XCTAssertTrue(
            app.contains("@StateObject private var plan = PlanState()"),
            "the plan must be created once, by the scene"
        )

        // And the cards must not have taken ownership back.
        for file in ["CandidatesSectionView.swift", "AiPlanSectionView.swift"] {
            let source = try readSource(file)
            XCTAssertFalse(
                source.contains("@State private var candidates"),
                "\(file) must not own the scan result"
            )
            XCTAssertFalse(
                source.contains("@State private var outcome"),
                "\(file) must not own the plan result"
            )
            XCTAssertFalse(
                source.contains("@StateObject"),
                "\(file) observes the stores; creating one here would make a private copy"
            )
        }
    }

    /// The convenience that would quietly reintroduce the bug: a defaulted
    /// store parameter. `CandidatesSectionView()` with no argument would build
    /// a fresh, empty `ScanState` and render "No scan yet" over a scan that had
    /// in fact completed — the same user-visible defect, arriving through an
    /// init rather than through a re-presentation.
    func testNeitherCardCanBeGivenAThrowawayStoreByOmission() throws {
        let candidates = try readSource("CandidatesSectionView.swift")
        XCTAssertTrue(candidates.contains("scan: ScanState,"), "scan must be a required parameter")
        XCTAssertFalse(
            candidates.contains("scan: ScanState = ScanState()"),
            "a defaulted store is a silent route back to an empty overview"
        )

        let aiPlan = try readSource("AiPlanSectionView.swift")
        XCTAssertTrue(aiPlan.contains("plan: PlanState,"), "plan must be a required parameter")
        XCTAssertFalse(
            aiPlan.contains("plan: PlanState = PlanState()"),
            "a defaulted store is a silent route back to an empty plan"
        )
    }

    /// Navigation cannot reach the state, because it holds nothing but the
    /// resource id it is looking at. This is the property the fix turns on: no
    /// amount of opening and closing details has anything to clear.
    func testNavigationCarriesNothingButTheResourceIdItIsShowing() {
        let mirror = Mirror(reflecting: CandidateDetailNavigation())
        XCTAssertEqual(
            mirror.children.compactMap(\.label),
            ["resourceId"],
            "navigation must not gain a reference to the scan or the plan"
        )
    }

    /// AC8: a project-roots change keeps its existing documented behaviour —
    /// the roots are read when the next `detect` or `llm-plan` is spawned, so a
    /// change takes effect on the next explicit Refresh or Ask and invalidates
    /// nothing by itself. Wiring an observer for it here would hand a
    /// non-navigation event the power this ticket just took away from
    /// navigation, so the stores deliberately know nothing about roots.
    func testTheStoresAreNotWiredToProjectRootsAtAll() throws {
        // Comments stripped: the file's header explains at length why it is
        // NOT wired to the roots, and naming the type in order to say so must
        // not read as wiring.
        let source = try strippedOfComments(readSource("OverviewState.swift"))
        XCTAssertFalse(
            source.contains("ProjectRootsStore"),
            "invalidation on a roots change would be a new behaviour, not this fix"
        )
        XCTAssertFalse(source.contains("UserDefaults"), "these stores are not persistence")

        for file in ["CandidatesSectionView.swift", "AiPlanSectionView.swift"] {
            let card = try readSource(file)
            XCTAssertFalse(
                card.contains(".onChange("),
                "\(file) must have no state-clearing trigger outside its own button"
            )
            XCTAssertFalse(card.contains(".onReceive("), "\(file) must not react to a publisher")
        }
    }

    /// The scan and the plan are written from exactly one place each, and both
    /// are button actions. Anything else that could write them is a second
    /// route to the founder's symptom.
    func testEachStoreIsWrittenOnlyFromItsOwnCardsOneRequest() throws {
        let candidates = try strippedOfComments(readSource("CandidatesSectionView.swift"))
        XCTAssertEqual(
            occurrences(of: "scan.candidates = ", in: candidates), 1,
            "the candidate list must be assigned in exactly one place"
        )
        XCTAssertTrue(candidates.contains("Task { await runDetect() }"))

        let aiPlan = try strippedOfComments(readSource("AiPlanSectionView.swift"))
        XCTAssertEqual(
            occurrences(of: "plan.outcome = ", in: aiPlan), 1,
            "the plan must be assigned in exactly one place"
        )
        XCTAssertTrue(aiPlan.contains("planTask = Task { await runLlmPlan() }"))
    }

    // MARK: - Helpers

    /// Builds the views that draw the two stores, then throws them away —
    /// which is what SwiftUI does on every body re-evaluation, and what a
    /// drill-down and a back press do to the overview.
    ///
    /// The result is deliberately discarded: the assertion is always about the
    /// stores afterwards, never about the views.
    private func buildOverview(scan: ScanState, plan: PlanState) {
        _ = GlomerisPopoverView(scan: scan, plan: plan)
        _ = CandidatesSectionView(scan: scan)
        _ = AiPlanSectionView(plan: plan)
    }

    private func candidate(
        resourceId: String = "/tmp/example/target",
        policyLabel: String = "AUTO_SAFE"
    ) -> DetectCandidateReportDto {
        DetectCandidateReportDto(
            resourceId: resourceId,
            kind: "node_modules",
            reclaimableBytes: 1_048_576,
            reclaimableHuman: "1.0 MB",
            reclaimableBytesIsLowerBound: false,
            impactTier: nil,
            policyLabel: policyLabel,
            reasons: ["regenerable"],
            executable: policyLabel == "AUTO_SAFE",
            offeredActions: [],
            refusalReason: nil
        )
    }

    private func planReport(resourceIds: [String]) -> LlmPlanReportDto {
        LlmPlanReportDto(
            items: resourceIds.enumerated().map { index, resourceId in
                LlmPlanItemReportDto(
                    resourceId: resourceId,
                    policyLabel: "AUTO_SAFE",
                    requestedActionId: "node.clean.node_modules",
                    priority: UInt32(index + 1),
                    modelReason: "regenerable dependencies",
                    explain: nil,
                    skipReason: nil,
                    candidate: candidate(resourceId: resourceId),
                    completeness: "complete",
                    confidence: "high"
                )
            },
            droppedUnknownResource: 0,
            droppedUnknownAction: 0,
            providerError: nil
        )
    }

    private func planItems(_ outcome: AiPlanOutcome?) -> [LlmPlanItemReportDto] {
        guard case .plan(let report) = outcome else { return [] }
        return report.items
    }

    private func occurrences(of needle: String, in haystack: String) -> Int {
        haystack.components(separatedBy: needle).count - 1
    }

    /// Whole-line comments only, so a `//` inside a string literal is left
    /// alone — the same rule the other source-reading suites in this target use.
    private func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    private func readSource(_ name: String) throws -> String {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Sources")
        return try String(contentsOf: sources.appendingPathComponent(name), encoding: .utf8)
    }
}
