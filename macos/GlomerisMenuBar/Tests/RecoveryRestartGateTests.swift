//
//  RecoveryRestartGateTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1512, verification items 11 and 12: "app/UI restart while no recovery is
//  running" and "stale UI state cannot fabricate a running/recovered status".
//
//  ============================================================================
//  WHY A RESTART IS TESTABLE AT ALL
//  ============================================================================
//  The app has exactly two things that outlive a process — the LLM endpoint and
//  model in `GlomerisLlmSettingsStore`, and the roots in `ProjectRootsStore` —
//  and there is no `@AppStorage` anywhere in the target. Nothing about a
//  recovery run is written down. So a relaunch is not a restore path with a
//  stale branch in it: it is literally a second `RecoveryState()`, and that is
//  what these tests construct. `scripts/check-recovery-progress-is-not-persisted.sh`
//  is the other half — it keeps the premise true, because the day a run's phase
//  is persisted is the day "a fresh panel shows nothing" stops following from a
//  fresh construction.
//
//  ============================================================================
//  WHAT WOULD MAKE THESE TESTS WORTHLESS, AND WHAT IS DONE ABOUT IT
//  ============================================================================
//  A test that asserts a newly built object is empty passes on an object that
//  can never be anything else, and proves nothing about the product. HORO-1512
//  says so in as many words: "Do not accept a test that can pass because the
//  path never executed."
//
//  So every assertion about the relaunched state here is made *beside* a state
//  driven into the condition being denied. The same type, in the same test, is
//  first made to report a run in flight, a round number, re-measured reclaimed
//  bytes and a finished result — and only then is a second one constructed and
//  found empty. The emptiness means something because the fullness was shown to
//  be reachable one line above it.
//

import XCTest

final class RecoveryRestartGateTests: XCTestCase {
    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func fixture<T: Decodable>(_ name: String, as type: T.Type) throws -> T {
        try JSONDecoder().decode(
            T.self,
            from: try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
        )
    }

    /// The progress stream's JSON is written out here for the reason
    /// `RecoverySectionViewTests.progressEvent` gives: the NDJSON on stderr has
    /// no golden fixture, so these literals are the contract, read from
    /// `RecoveryProgressEvent`'s serde attributes in `src/reporting/dto.rs`.
    private func progressEvent(_ json: String) throws -> RecoveryProgressEventDto {
        try JSONDecoder().decode(RecoveryProgressEventDto.self, from: Data(json.utf8))
    }

    private func measuredLine(
        iteration: UInt32, freedSoFar: UInt64, freedSoFarHuman: String
    ) -> String {
        #"{"phase":"measured","iteration":\#(iteration),"total_bytes":500,"#
            + #""free_bytes":100,"used_percent":80.0,"free_human":"100 B","#
            + #""bytes_freed_so_far":\#(freedSoFar),"#
            + #""bytes_freed_so_far_human":"\#(freedSoFarHuman)"}"#
    }

    /// Drives a state into the most convincing wrong thing a relaunch could
    /// show: a run in flight, on round 4, with bytes already measured as gone.
    ///
    /// Returns it so each test can assert the condition really was reached
    /// before denying it of a fresh one.
    private func stateMidRun() throws -> RecoveryState {
        let state = RecoveryState()
        XCTAssertTrue(state.beginRecovering())
        state.phase = .recovering(
            try fixture("recovery_preview_report.json", as: RecoveryPreviewReportDto.self)
        )
        state.absorbRunProgress(try progressEvent(
            measuredLine(iteration: 4, freedSoFar: 4_096, freedSoFarHuman: "4.1 kB")
        ))
        return state
    }

    // MARK: - Item 11: a relaunch while no run is going

    /// The claim: the panel a relaunched app opens says no run is happening and
    /// nothing has been reclaimed — because there is no run and nothing has.
    ///
    /// Made non-vacuous by `before`, which is the same type reporting the
    /// opposite of all four things `after` is checked for. If `RecoveryState`
    /// ever gained a restore path, `before` is the state it would restore from
    /// and this test is where that would show.
    func testARelaunchedPanelReportsNoRunInFlightAndNothingReclaimed() throws {
        let before = try stateMidRun()
        // The precondition, asserted rather than assumed: this is the condition
        // whose absence below is the actual subject of the test.
        XCTAssertTrue(before.isRecovering, "precondition: a run is in flight")
        XCTAssertEqual(before.liveProgress.iteration, 4, "precondition: on round 4")
        XCTAssertEqual(
            before.liveProgress.reclaimedSoFarText, "4.1 kB reclaimed so far",
            "precondition: bytes measured as gone"
        )
        XCTAssertNotNil(before.stopRequest, "precondition: there is a run to stop")

        // What a relaunch produces. Not a reset of `before` — a second object,
        // because a second process is what the user does.
        let after = RecoveryState()

        XCTAssertFalse(
            after.isRecovering,
            "a restarted app must not claim a run it cannot see, stop or account for"
        )
        XCTAssertEqual(after.phase, .idle)
        XCTAssertEqual(
            after.liveProgress, RecoveryLiveProgress(),
            "no round number, no status line and no reclaimed figure survive the process"
        )
        XCTAssertNil(
            after.liveProgress.reclaimedSoFarText,
            "\"reclaimed so far\" describes bytes this process measured; it measured none"
        )
        XCTAssertFalse(after.liveProgress.hasCompletedWork)
        XCTAssertNil(after.stopRequest)
        XCTAssertFalse(after.hasRequestedStop)
        XCTAssertNil(after.progressStatusText)
        XCTAssertNil(after.lastErrorMessage)

        before.endRecovering()
    }

    /// The run guard is a within-process compare-and-set, and a fresh process
    /// holds none of it. That is the honest state of affairs and it is asserted
    /// on purpose: the tempting "fix" — persisting `isRecovering` so a relaunch
    /// knows a run was going — is exactly the write the no-persistence guard
    /// forbids, because a flag left behind by a killed process would lock the
    /// user out of recovery with no run to release it.
    ///
    /// Cross-process exclusivity is not this app's job and never was:
    /// `src/executor/lock.rs` holds an exclusive non-blocking lock, so a second
    /// child is refused `busy` by the CLI. Swift's guard exists to stop one
    /// panel starting two children, and it does that.
    func testTheRunGuardIsNotInheritedAcrossARelaunchAndTheCliHoldsTheRealLock() throws {
        let before = try stateMidRun()
        XCTAssertFalse(
            before.beginRecovering(),
            "precondition: within one process the guard refuses a second run"
        )

        let after = RecoveryState()
        XCTAssertTrue(
            after.beginRecovering(),
            "a relaunched panel starts from nothing; the execution lock in "
                + "src/executor/lock.rs is what refuses a second child"
        )

        after.endRecovering()
        before.endRecovering()
    }

    // MARK: - Item 12: stale state cannot fabricate a run or a recovery

    /// A finished run's report is the most expensive thing in the panel — it
    /// describes bytes that are already gone — and it is also the most
    /// misleading thing a relaunch could show, because it looks like the result
    /// of the session the user is now in.
    ///
    /// `before` holds one. `after` holds no report at all, in any phase.
    func testAFinishedRunsReportIsUnreachableFromARelaunchedPanel() throws {
        let before = RecoveryState()
        let report = try fixture("recovery_run_report.json", as: RecoveryRunReportDto.self)
        before.phase = .finished(report)
        XCTAssertNotNil(
            Self.reportSummary(before.phase),
            "precondition: a result is on screen"
        )

        let after = RecoveryState()
        XCTAssertEqual(after.phase, .idle)
        XCTAssertNil(
            Self.reportSummary(after.phase),
            "no phase a fresh panel can be in carries a report, so there is no run "
                + "for it to attribute to this session"
        )
    }

    /// The idle figures are not zeroes standing in for measurements. A fresh
    /// `RecoveryLiveProgress` has *no* usage reading, *no* reclaimed total and
    /// *no* status line, and its spoken state is empty rather than a sentence
    /// claiming a round zero with nothing reclaimed.
    ///
    /// This is item 12's other half, and the distinction is the campaign's §9:
    /// "0 B reclaimed" is a measurement, and a panel that has measured nothing
    /// must not make it. The driven value beside it shows the same type
    /// producing all four figures once a run has actually reported them.
    func testAFreshProgressValueStatesNothingRatherThanStatingZero() throws {
        var driven = RecoveryLiveProgress()
        driven.absorb(try progressEvent(
            measuredLine(iteration: 2, freedSoFar: 0, freedSoFarHuman: "0 B")
        ))
        // A genuine measured zero, which is the case this test would otherwise be
        // confused with: the run looked, and nothing came back yet. It is shown.
        XCTAssertEqual(driven.iteration, 2, "precondition")
        XCTAssertEqual(driven.reclaimedSoFarText, "0 B reclaimed so far", "precondition")
        XCTAssertEqual(driven.currentUsageText, "80.0% used — 100 B free", "precondition")
        XCTAssertFalse(driven.spokenState.isEmpty, "precondition")

        let fresh = RecoveryLiveProgress()
        XCTAssertEqual(fresh.iteration, 0)
        XCTAssertNil(fresh.statusText)
        XCTAssertNil(
            fresh.currentUsageText,
            "a panel that has not measured the disk must not report a reading"
        )
        XCTAssertNil(
            fresh.reclaimedSoFarText,
            "an unmeasured total stays absent; \"0 B reclaimed\" is a measurement"
        )
        XCTAssertNil(fresh.workDoneText)
        XCTAssertFalse(fresh.isActionInFlight)
        XCTAssertFalse(fresh.stopRequested)
        XCTAssertEqual(
            fresh.spokenState, "",
            "VoiceOver gets nothing rather than a sentence about a run that is not happening"
        )
    }

    /// Dismissing a result and relaunching land in the same place, which is what
    /// makes the idle phase one state rather than two that happen to look alike.
    /// A user who quits mid-run and reopens sees what a user who pressed Done
    /// sees: an empty panel, and a control they can start a real run from.
    func testDismissingAResultAndRelaunchingReachTheSameIdleState() throws {
        let dismissed = RecoveryState()
        dismissed.phase = .finished(
            try fixture("recovery_run_report.json", as: RecoveryRunReportDto.self)
        )
        dismissed.absorbRunProgress(try progressEvent(
            measuredLine(iteration: 3, freedSoFar: 2_048, freedSoFarHuman: "2.0 kB")
        ))
        XCTAssertNotEqual(dismissed.liveProgress, RecoveryLiveProgress(), "precondition")

        dismissed.resetGoalProgress()

        let relaunched = RecoveryState()
        XCTAssertEqual(dismissed.phase, relaunched.phase)
        XCTAssertEqual(dismissed.liveProgress, relaunched.liveProgress)
        XCTAssertEqual(dismissed.isRecovering, relaunched.isRecovering)
        XCTAssertEqual(dismissed.hasRequestedStop, relaunched.hasRequestedStop)
    }

    /// A relaunched panel has nothing to stop, and asking is a no-op rather than
    /// a sentinel file written where no loop is watching. The stop path is
    /// reached through `stopRequest`, which `beginRecovering()` mints — so a
    /// process that never began a run cannot write one.
    func testARelaunchedPanelHasNoStopPathToWriteInto() throws {
        let before = try stateMidRun()
        let armedPath = try XCTUnwrap(before.stopRequest?.url.path)
        XCTAssertNil(before.requestStop(), "precondition: the running panel can stop its run")
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: armedPath),
            "precondition: that request is a real file the loop is watching"
        )

        let after = RecoveryState()
        XCTAssertNil(after.stopRequest)
        XCTAssertNil(after.requestStop(), "nothing to ask, and asking is not an error")
        XCTAssertFalse(
            after.hasRequestedStop,
            "a panel must not report a stop it did not request against a run it does not have"
        )

        before.endRecovering()
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: armedPath),
            "and the run that did have one cleans it up"
        )
    }

    // MARK: -

    /// One field off whatever report the phase carries, or `nil` when the phase
    /// carries none.
    ///
    /// Written as a total function over ``RecoveryPhase`` so that a seventh case
    /// added later has to be classified here rather than falling into a
    /// `default` that would silently report "no report" — which is the exact
    /// wrong answer for a case that carries one.
    private static func reportSummary(_ phase: RecoveryPhase) -> String? {
        switch phase {
        case .idle, .previewing:
            return nil
        case .reviewing(let report), .recovering(let report):
            return report.requiredFreeHuman
        case .refused(let report):
            return report.message
        case .finished(let report):
            return report.stopReason
        }
    }
}
