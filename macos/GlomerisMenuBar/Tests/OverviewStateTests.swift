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
    //
    // These drive the real `runDetect()` / `runLlmPlan()` against a pinned
    // fixture binary, rather than performing the production assignments in the
    // test body and asserting they happened. That distinction matters: an
    // earlier version of these tests did the latter, and would have passed
    // against a `runDetect` that cleared the candidate list on failure.

    /// AC6. The fix must not have made the state immovable — an explicit
    /// Refresh replaces a scan outright rather than merging into it.
    @MainActor
    func testAnExplicitRefreshReplacesTheScanRatherThanAddingToIt() async throws {
        let scan = ScanState()
        scan.candidates = [candidate(resourceId: "/tmp/old/target")]
        scan.lastScannedAt = Date(timeIntervalSince1970: 1_700_000_000)

        let card = CandidatesSectionView(
            scan: scan,
            client: try Self.fixtureClient(stdout: Self.detectReportJSON(resourceId: "/tmp/new/node_modules"))
        )
        await card.runDetect()

        XCTAssertEqual(
            scan.candidates.map(\.resourceId), ["/tmp/new/node_modules"],
            "a successful detect must replace the list, not append to it"
        )
        XCTAssertNotEqual(
            scan.lastScannedAt, Date(timeIntervalSince1970: 1_700_000_000),
            "the scan timestamp must move with the scan"
        )
        XCTAssertNil(scan.lastErrorMessage)
        XCTAssertFalse(scan.isScanning, "the flag must be cleared however the fetch ends")
        XCTAssertNil(scan.progressStatusText, "a finished scan shows no progress line")
    }

    /// A failed Refresh is additive: it reports the failure and leaves the
    /// earlier scan usable. Anything else is the founder's symptom arriving by
    /// a second route — an empty overview the user did not ask for.
    ///
    /// The fixture exits non-zero here, which is a real `GlomerisClient` error
    /// rather than a decode failure, so the `catch` branch is the one under test.
    @MainActor
    func testAFailedRefreshLeavesTheEarlierScanInPlace() async throws {
        let scan = ScanState()
        let earlier = [candidate(resourceId: "/tmp/a/node_modules")]
        scan.candidates = earlier
        let scannedAt = Date(timeIntervalSince1970: 1_700_000_000)
        scan.lastScannedAt = scannedAt

        let card = CandidatesSectionView(
            scan: scan,
            client: try Self.fixtureClient(stdout: "not json at all", exitCode: 3)
        )
        await card.runDetect()

        XCTAssertEqual(scan.candidates, earlier, "a failure must not empty the overview")
        XCTAssertEqual(scan.lastScannedAt, scannedAt, "a failure must not restamp the scan")
        XCTAssertNotNil(scan.lastErrorMessage, "and it must say so")
        XCTAssertFalse(scan.isScanning)
    }

    /// AC7, the same shape for an explicit Ask. `llm-plan` is read with
    /// `runRaw`, so the fixture's stdout is interpreted by
    /// `AiPlanInterpretation` exactly as the real CLI's would be.
    @MainActor
    func testAnExplicitAskReplacesThePlanRatherThanAddingToIt() async throws {
        let plan = PlanState()
        plan.outcome = .plan(planReport(resourceIds: ["/tmp/old/target"]))
        plan.lastPlannedAt = Date(timeIntervalSince1970: 1_700_000_000)

        // Not handed to the client: `runLlmPlan` calls
        // `withEnvironment(settingsStore.childEnvironment())`, which REPLACES
        // the child environment wholesale, so anything pinned on the client
        // would be discarded. `childEnvironment()` is built on top of this
        // process's environment, which is therefore where the fixture's
        // instructions have to go.
        Self.setFixtureEnvironment(stdout: Self.planReportJSON(resourceId: "/tmp/new/node_modules"))
        defer { Self.clearFixtureEnvironment() }

        let card = AiPlanSectionView(
            plan: plan,
            client: GlomerisClient(executableURL: try Self.fixtureBinary()),
            settingsStore: Self.settingsStoreThatTouchesNoKeychain()
        )
        await card.runLlmPlan()

        XCTAssertEqual(
            planItems(plan.outcome).map(\.resourceId), ["/tmp/new/node_modules"],
            "a successful ask must replace the plan"
        )
        XCTAssertNotEqual(plan.lastPlannedAt, Date(timeIntervalSince1970: 1_700_000_000))
        XCTAssertFalse(plan.isPlanning)
        XCTAssertNil(plan.planTask, "the handle must be released so Stop cannot cancel a finished run")
    }

    /// A failed Ask is additive — it must not take an earlier usable plan with
    /// it. This is the one the user pays for twice if it is wrong: the plan is
    /// gone and the only way back is another billable request.
    @MainActor
    func testAFailedAskLeavesTheEarlierPlanInPlace() async throws {
        let plan = PlanState()
        let earlier = planReport(resourceIds: ["/tmp/a/node_modules"])
        plan.outcome = .plan(earlier)
        let plannedAt = Date(timeIntervalSince1970: 1_700_000_000)
        plan.lastPlannedAt = plannedAt

        let card = AiPlanSectionView(
            plan: plan,
            // A pinned path that does not exist: the spawn itself fails, which
            // is the "could not reach the provider" class of failure.
            client: GlomerisClient(executableURL: URL(fileURLWithPath: "/no/such/glomeris-binary")),
            settingsStore: Self.settingsStoreThatTouchesNoKeychain()
        )
        await card.runLlmPlan()

        XCTAssertEqual(plan.outcome, .plan(earlier), "a failed ask must not discard a paid-for plan")
        XCTAssertEqual(plan.lastPlannedAt, plannedAt, "nor claim the old plan is current")
        XCTAssertNotNil(plan.lastErrorMessage)
        XCTAssertFalse(plan.isPlanning)
    }

    /// Both fetches must stay `@MainActor`: every assignment they make is to a
    /// `@Published` property of an object a live view observes, so an off-main
    /// write would publish a SwiftUI-observed change from a background thread.
    ///
    /// Asserted as a source fact, which is an uncomfortable shape for a test and
    /// is chosen deliberately. The behavioural version — subscribe to
    /// `objectWillChange`, drive the fetch, count publishes seen off the main
    /// thread — was written first and then discarded, because it was measured to
    /// pass with the annotation removed: in Swift 5 language mode an unstructured
    /// `Task {}` inherits its creating context and a `nonisolated async` callee
    /// stays on it, so there is no off-main write to observe on this target
    /// today. A guard that cannot fail is not a guard. Under Swift 6 semantics
    /// the callee would resume on the cooperative pool, and this assertion is
    /// what stops the annotation being deleted as decoration before then.
    func testBothFetchesStayOnTheMainActor() throws {
        for (file, function) in [
            ("CandidatesSectionView.swift", "func runDetect() async"),
            ("AiPlanSectionView.swift", "func runLlmPlan() async"),
        ] {
            let source = try strippedOfComments(readSource(file))
            guard let declaration = source.range(of: function) else {
                return XCTFail("\(file) no longer declares \(function)")
            }
            // The annotation must be the token immediately before it, so an
            // `@MainActor` somewhere else in the file cannot satisfy this.
            let preceding = source[source.startIndex..<declaration.lowerBound]
                .trimmingCharacters(in: .whitespacesAndNewlines)
            XCTAssertTrue(
                preceding.hasSuffix("@MainActor"),
                "\(function) publishes to a store a live view observes and must be main-actor"
            )
        }
    }

    // MARK: - Structure: why this cannot come back

    /// The state is owned by the popover root — above every drill-down, because
    /// that root is also where `navigation` is declared.
    func testTheScanAndThePlanAreOwnedByThePopoverRootAndNotByTheCards() throws {
        let shell = try strippedOfComments(readSource("GlomerisPopoverView.swift"))
        XCTAssertTrue(
            shell.contains("@StateObject private var scan = ScanState()"),
            "the scan must be created once, by the view above the drill-down"
        )
        XCTAssertTrue(
            shell.contains("@StateObject private var plan = PlanState()"),
            "the plan must be created once, by the view above the drill-down"
        )
        XCTAssertTrue(
            shell.contains("@State private var navigation = CandidateDetailNavigation()"),
            "ownership is only 'above navigation' while navigation is declared here"
        )

        // And the cards must not have taken ownership back.
        for file in ["CandidatesSectionView.swift", "AiPlanSectionView.swift"] {
            let source = try strippedOfComments(readSource(file))
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
        let candidates = try strippedOfComments(readSource("CandidatesSectionView.swift"))
        XCTAssertTrue(candidates.contains("scan: ScanState,"), "scan must be a required parameter")
        XCTAssertFalse(
            candidates.contains("scan: ScanState = ScanState()"),
            "a defaulted store is a silent route back to an empty overview"
        )

        let aiPlan = try strippedOfComments(readSource("AiPlanSectionView.swift"))
        XCTAssertTrue(aiPlan.contains("plan: PlanState,"), "plan must be a required parameter")
        XCTAssertFalse(
            aiPlan.contains("plan: PlanState = PlanState()"),
            "a defaulted store is a silent route back to an empty plan"
        )
    }

    /// The state must NOT be hoisted onto the `App`, and this is not a style
    /// preference — it was measured.
    ///
    /// An observable object on the scene makes every publish re-evaluate
    /// `App.body`, and `App.body` constructs the `Settings` tabs eagerly,
    /// whether or not a Settings window exists. At the time this was measured,
    /// `AiProviderPreferencesView.init` seeded its status from
    /// `GlomerisLlmSettingsStore.status()`, which read the keychain synchronously.
    /// A Refresh publishes on every progress line, so the scene-level version
    /// fired a burst of main-thread `SecItemCopyMatching` calls per scan; on a
    /// bundle whose code identity the keychain ACL did not recognise, one blocked
    /// behind a `SecurityAgent` prompt and the menu-bar item vanished mid-scan —
    /// the app became unreachable, with nothing on screen to explain it. Strictly
    /// worse than the bug being fixed.
    ///
    /// HORO-1368 has since fixed the far end: the view seeds from UserDefaults
    /// only, and every keychain touch happens on a background queue. That removes
    /// the *consequence* measured above, not the reason for this guard — scene
    /// state still means `App.body` and the whole `Settings` tab tree
    /// re-evaluate on every publish, for a window that is usually not open.
    ///
    /// This test deliberately does NOT assert on `AiProviderPreferencesView`'s or
    /// `GlomerisLlmSettingsStore`'s internals to prove the chain exists. Pinning
    /// another file's private implementation from here is what would have made
    /// HORO-1368's repair come back and edit a test about scene state, and a
    /// guard that fires on the repair is worse than no guard. That it did not
    /// have to is the evidence the split was drawn in the right place.
    func testTheAppSceneHoldsNoObservableStateBecauseItsBodyBuildsTheSettingsTabs() throws {
        let app = try strippedOfComments(readSource("GlomerisMenuBarApp.swift"))
        XCTAssertFalse(
            app.contains("@StateObject"),
            "a scene-level store re-evaluates App.body, which reads the keychain"
        )
        XCTAssertFalse(app.contains("@State"), "same reason: any observable scene state does it")

        // The one fact this file can honestly own: the Settings tabs really are
        // constructed by `App.body`, so a scene-level publish reaches them.
        XCTAssertTrue(
            app.contains("AiProviderPreferencesView()"),
            "if this pane moves, re-check whether the keychain read is still on this path"
        )

        // And the popover must not be given an identity the scene can change.
        // `.id()` on the content view is the one modifier that would let a
        // scene-level re-evaluation discard the popover root's `@StateObject`s
        // — the exact ownership this fix depends on — without any of the
        // clearing patterns the other guards look for.
        XCTAssertFalse(
            app.contains(".id("),
            "an identity on the popover would let the scene throw the stores away"
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

        // The shell is in this list too, and it is the important one: it is what
        // owns the stores now, so a trigger there could clear a completed scan
        // for the whole app rather than for one card. Leaving it out was how the
        // first version of this guard managed to cover only the views that
        // cannot do the damage.
        for file in [
            "GlomerisPopoverView.swift",
            "CandidatesSectionView.swift",
            "AiPlanSectionView.swift",
        ] {
            let source = try strippedOfComments(readSource(file))
            // Each of these is a route to "something other than a button
            // decided the state should change". `.onAppear` is the one worth
            // spelling out: it is how a view-lifecycle event — which a
            // drill-down and a back press both produce — would get to write the
            // stores again, which is this ticket's defect with a different
            // trigger.
            for trigger in [".onChange(", ".onReceive(", ".onAppear", ".onDisappear", "Timer", "NotificationCenter"] {
                XCTAssertFalse(
                    source.contains(trigger),
                    "\(file) contains \(trigger): the state must change only from an explicit button"
                )
            }
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
    /// Only the two cards, not the shell: the shell is what owns the stores, so
    /// constructing one here would build a second pair of stores unrelated to
    /// the ones under test — and, through its defaulted `ProjectRootsStore`,
    /// open the real `UserDefaults` suite to do it. The result is deliberately
    /// discarded; the assertion is always about the stores afterwards.
    private func buildOverview(scan: ScanState, plan: PlanState) {
        _ = CandidatesSectionView(scan: scan)
        _ = AiPlanSectionView(plan: plan)
    }

    // MARK: - The fixture binary
    //
    // The same compiled Mach-O helper `GlomerisClientTests` builds, under the
    // same revision-suffixed name on purpose: an unsuffixed or differently-named
    // copy would be built once by whichever suite ran first and then never
    // rebuilt when the `.c` changed, which is precisely the staleness the suffix
    // exists to prevent.
    //
    // It is handed its script through `GLOMERIS_FIXTURE_STDOUT` /
    // `GLOMERIS_FIXTURE_EXIT` rather than through argv, because here the
    // arguments are chosen by the production code under test (`runDetect` builds
    // `["detect", "--json", "--progress-json"] + roots`), not by the test.

    private static let fixtureStdoutVariable = "GLOMERIS_FIXTURE_STDOUT"
    private static let fixtureExitVariable = "GLOMERIS_FIXTURE_EXIT"

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

    /// A client pinned to the fixture, with the fixture's script in the
    /// client's own environment. Good for `runDetect`, which spawns with
    /// whatever environment its client carries.
    private static func fixtureClient(stdout: String, exitCode: Int32 = 0) throws -> GlomerisClient {
        GlomerisClient(
            executableURL: try fixtureBinary(),
            // A complete environment, as `GlomerisClient` requires — and a
            // deliberately tiny one. The fixture is spawned by absolute path
            // and reads nothing else, so there is nothing here to inherit.
            environment: [
                fixtureStdoutVariable: stdout,
                fixtureExitVariable: String(exitCode),
            ]
        )
    }

    /// For `runLlmPlan` only, which replaces the child environment wholesale
    /// from `GlomerisLlmSettingsStore.childEnvironment()` — so the script has to
    /// be in this process's environment for `childEnvironment()` to carry it
    /// through. Always paired with `clearFixtureEnvironment()` in a `defer`.
    private static func setFixtureEnvironment(stdout: String, exitCode: Int32 = 0) {
        setenv(fixtureStdoutVariable, stdout, 1)
        setenv(fixtureExitVariable, String(exitCode), 1)
    }

    private static func clearFixtureEnvironment() {
        unsetenv(fixtureStdoutVariable)
        unsetenv(fixtureExitVariable)
    }

    /// A settings store that reads no keychain and writes no shared defaults.
    ///
    /// Both halves matter. `childEnvironment()` reads the API key from the
    /// credential store at spawn time, and an `InMemoryCredentialStore` keeps
    /// that off the real keychain — no `SecItemCopyMatching`, so no
    /// authorisation prompt can appear in the middle of a test run. The
    /// throwaway storage keeps the endpoint and model out of the app's own
    /// `UserDefaults`, which on this machine belongs to a running app — and,
    /// being in memory, out of any preference file (HORO-1486).
    private static func settingsStoreThatTouchesNoKeychain() -> GlomerisLlmSettingsStore {
        GlomerisLlmSettingsStore(
            defaults: TestUserDefaults.inMemory(),
            credentials: InMemoryCredentialStore()
        )
    }

    // MARK: - Fixture payloads
    //
    // Hand-written rather than built by encoding the DTOs: these have to be
    // what the CLI's `Serialize` output looks like on the wire, and a round trip
    // through the app's own `Decodable` mirrors would prove only that the
    // mirrors agree with themselves. The key spelling is snake_case for the same
    // reason (`GlomerisDtos.swift`'s `CodingKeys`).

    private static func candidateJSON(
        resourceId: String,
        policyLabel: String = "AUTO_SAFE",
        executable: Bool = true
    ) -> String {
        """
        {
          "resource_id": "\(resourceId)",
          "kind": "node_modules",
          "reclaimable_bytes": 1048576,
          "reclaimable_human": "1.0 MB",
          "reclaimable_bytes_is_lower_bound": false,
          "impact_tier": "normal",
          "policy_label": "\(policyLabel)",
          "reasons": ["regenerable"],
          "executable": \(executable),
          "offered_actions": [
            {"action_id": "node.clean.node_modules", "requires_confirmation": false}
          ],
          "refusal_reason": null
        }
        """
    }

    private static func detectReportJSON(resourceId: String) -> String {
        """
        {"candidates": [\(candidateJSON(resourceId: resourceId))]}
        """
    }

    private static func planReportJSON(resourceId: String) -> String {
        """
        {
          "items": [
            {
              "resource_id": "\(resourceId)",
              "policy_label": "AUTO_SAFE",
              "requested_action_id": "node.clean.node_modules",
              "priority": 1,
              "model_reason": "regenerable dependencies",
              "explain": null,
              "skip_reason": null,
              "candidate": \(candidateJSON(resourceId: resourceId)),
              "completeness": "complete",
              "confidence": "high"
            }
          ],
          "dropped_unknown_resource": 0,
          "dropped_unknown_action": 0,
          "provider_error": null
        }
        """
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
