//
//  UnpromptedRecoveryTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1510 AC 2 and AC 8: the optional half of §10 — a bounded recovery run
//  that begins in answer to a disk-pressure alert, with nobody present.
//
//  Everything here is driven through the `UnpromptedRecoveryRunning` seam, and
//  that is not a convenience. A real `free --autopilot --unattended` deletes from
//  the machine this suite runs on, so a test may not spawn one; the seam is how
//  these paths become checkable at all. `StubUnpromptedRecovery` records what it
//  was asked for and deletes nothing.
//
//  The claims worth pinning, in the order they would hurt if they were wrong:
//
//    * the default is unchanged. A monitor with no runner, and a monitor whose
//      grant has not opted in, raise a banner and start nothing — because §6 says
//      crossing a threshold must not begin destructive cleanup, and that is the
//      behaviour every Mac has until somebody says otherwise;
//    * an unreadable grant is not consent;
//    * one attempt per banner-worth of pressure, so a disk that stays full does
//      not become a run every thirty seconds;
//    * `busy` is the one outcome that provably mutated nothing, and so the one
//      that may be retried. Getting that pair the wrong way round is either a
//      lost attempt or an unbounded loop;
//    * a run that could not get the disk under its goal still reaches the user —
//      in the same poll, from a report read *after* the deletions;
//    * the run is never reachable from a cancellable task, because `SIGTERM`
//      partway through a deletion is unreportable.
//

import XCTest

final class UnpromptedRecoveryTests: XCTestCase {

    // MARK: - Reports

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .deletingLastPathComponent()  // macos
            .deletingLastPathComponent()  // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func fixtureData(_ name: String) throws -> Data {
        try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
    }

    /// Threshold 85% used, disk at 91% used, episode 1 with a banner owed.
    private func owed() throws -> PressureStatusReportDto {
        try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try fixtureData("pressure_status_report.json")
        )
    }

    /// The same episode owing its *second* banner — what "Remind me later" looks
    /// like once the snooze has elapsed. A different `PressureBannerKey`, which is
    /// the whole reason that key carries the raised count.
    private func owedAgain() throws -> PressureStatusReportDto {
        var object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try fixtureData("pressure_status_report.json"))
                as? [String: Any]
        )
        var episode = try XCTUnwrap(object["episode"] as? [String: Any])
        episode["notifications_raised"] = 1
        object["episode"] = episode
        return try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try JSONSerialization.data(withJSONObject: object)
        )
    }

    /// Disk at 60% used, no episode — what the volume looks like after a run got
    /// usage below the hysteresis margin and Rust closed the episode.
    private func recovered() throws -> PressureStatusReportDto {
        try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try fixtureData("pressure_status_report_no_episode.json")
        )
    }

    private func runReport() throws -> RecoveryRunReportDto {
        try JSONDecoder().decode(
            RecoveryRunReportDto.self,
            from: try fixtureData("recovery_run_report.json")
        )
    }

    /// The same run having reclaimed nothing: a loop that looked and found nothing
    /// it was allowed to take. Edited from the fixture rather than written out, for
    /// the reason ``grant(startsUnprompted:)`` gives — a change to the report's shape
    /// should reach this file rather than be absorbed by a literal.
    ///
    /// The free-space figures are made to agree with that, because a report claiming
    /// zero bytes reclaimed *and* a disk that gained 139 GB would be testing against
    /// a state Rust cannot produce.
    private func runReportThatTookNothing() throws -> RecoveryRunReportDto {
        var object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try fixtureData("recovery_run_report.json"))
                as? [String: Any]
        )
        object["stop_reason"] = "safe_exhausted"
        object["stop_reason_detail"] = "Nothing left that may be removed without confirmation."
        object["bytes_freed_measured"] = 0
        object["bytes_freed_measured_human"] = "0 B"
        object["final_free_bytes"] = object["started_free_bytes"]
        object["final_free_human"] = object["started_free_human"]
        object["target_met"] = false
        return try JSONDecoder().decode(
            RecoveryRunReportDto.self,
            from: try JSONSerialization.data(withJSONObject: object)
        )
    }

    /// A banner owed for a *different* episode — the disk filled up again next week.
    private func owedInASecondEpisode() throws -> PressureStatusReportDto {
        var object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try fixtureData("pressure_status_report.json"))
                as? [String: Any]
        )
        var episode = try XCTUnwrap(object["episode"] as? [String: Any])
        episode["episode_id"] = 2
        object["episode"] = episode
        return try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try JSONSerialization.data(withJSONObject: object)
        )
    }

    /// The grant, with the two HORO-1510 fields set as asked.
    ///
    /// Built by editing the fixture's JSON rather than by writing a literal, so a
    /// change to the report's shape reaches this file instead of being absorbed.
    private func grant(startsUnprompted: Bool) throws -> AutopilotEnvelopeDto {
        var object = try XCTUnwrap(
            try JSONSerialization.jsonObject(
                with: try fixtureData("autopilot_envelope_report.json")
            ) as? [String: Any]
        )
        object["respond_to_alerts"] = startsUnprompted
        object["starts_unprompted"] = startsUnprompted
        return try JSONDecoder().decode(
            AutopilotEnvelopeDto.self,
            from: try JSONSerialization.data(withJSONObject: object)
        )
    }

    // MARK: - The rule, as values

    private let key = PressureBannerKey(episodeId: 1, notificationsRaised: 0)

    /// §6, and the default every Mac is in: crossing a threshold does not start a
    /// destructive run.
    func testAGrantThatHasNotOptedInAsksInstead() {
        XCTAssertEqual(
            UnpromptedRecoveryPlan.decide(
                owed: key, startsUnprompted: false, lastAttempted: nil
            ),
            .askInstead
        )
    }

    /// An unreadable grant is not consent. The direction matters more than the
    /// case: `nil` here is "the CLI could not be asked", and the only safe reading
    /// of that is the one that deletes nothing.
    func testAnUnreadableGrantIsNotConsent() {
        XCTAssertEqual(
            UnpromptedRecoveryPlan.decide(
                owed: key, startsUnprompted: nil, lastAttempted: nil
            ),
            .askInstead
        )
    }

    func testAGrantThatSaysSoRuns() {
        XCTAssertEqual(
            UnpromptedRecoveryPlan.decide(
                owed: key, startsUnprompted: true, lastAttempted: nil
            ),
            .run(key)
        )
    }

    /// One attempt per banner-worth of pressure. Without this a disk that stayed
    /// full would be a bounded run every poll — each individually inside its
    /// envelope, and collectively a loop nobody authorized.
    func testTheSameOwedBannerIsNotRunTwice() {
        XCTAssertEqual(
            UnpromptedRecoveryPlan.decide(
                owed: key, startsUnprompted: true, lastAttempted: key
            ),
            .alreadyAttempted(key)
        )
    }

    /// The other half of that, and the reason the key is not just the episode id.
    ///
    /// After "Remind me later" elapses the daemon owes a *second* banner for the
    /// same episode. That is a second crossing-worth of attention, and it gets a
    /// second bounded attempt — keyed on the episode alone, one long episode would
    /// get one attempt however many times it came back.
    func testThePostSnoozeReminderGetsItsOwnAttempt() {
        let reminder = PressureBannerKey(episodeId: 1, notificationsRaised: 1)

        XCTAssertEqual(
            UnpromptedRecoveryPlan.decide(
                owed: reminder, startsUnprompted: true, lastAttempted: key
            ),
            .run(reminder)
        )
    }

    // MARK: - What an attempt turned out to be

    /// `busy` is the one outcome that provably mutated nothing — exit 75 is the
    /// execution lock refusing *before* the loop started — so it is the only one
    /// worth trying again. Everything else is spent, `failed` included, because a
    /// run that got far enough to fail may already have deleted something.
    func testOnlyABusyLockLeavesTheAttemptUnspent() throws {
        XCTAssertFalse(UnpromptedRecoveryOutcome.busy("held").consumesTheAttempt)

        XCTAssertTrue(UnpromptedRecoveryOutcome.ran(try runReport()).consumesTheAttempt)
        XCTAssertTrue(UnpromptedRecoveryOutcome.notAuthorized("no").consumesTheAttempt)
        XCTAssertTrue(UnpromptedRecoveryOutcome.failed("boom").consumesTheAttempt)
    }

    func testExitZeroWithAReportIsARun() throws {
        let outcome = UnpromptedRecoveryClient.interpret(
            exitCode: 0,
            stdout: try fixtureData("recovery_run_report.json"),
            stderr: Data()
        )

        XCTAssertEqual(outcome, .ran(try runReport()))
    }

    /// Exit 3's two meanings arrive as one code on purpose, and both land here:
    /// from this app's side they are the same fact — the run was not authorized and
    /// nothing was attempted.
    func testExitThreeIsNotAuthorizedWhicheverOfItsTwoMeaningsItHas() {
        for stderr in [
            "glomeris free: --autopilot runs only inside a grant you wrote, and Autopilot is not "
                + "enabled. Nothing was attempted.",
            "glomeris free: --unattended needs a grant that allows starting a run without being "
                + "asked, and this one does not. Nothing was attempted.",
        ] {
            let outcome = UnpromptedRecoveryClient.interpret(
                exitCode: 3,
                stdout: Data(),
                stderr: Data(stderr.utf8)
            )

            guard case .notAuthorized(let message) = outcome else {
                return XCTFail("expected notAuthorized, got \(outcome)")
            }
            // The CLI's own sentence is carried through, so the two cases stay
            // distinguishable to a reader even though the app treats them alike.
            XCTAssertTrue(message.contains(stderr), message)
        }
    }

    /// Exit 75 *and* the lock's own token. The code alone would let a future 75
    /// read as "nothing happened", which is the one mistake that turns a lost
    /// attempt into a retry loop over a run that may have deleted things.
    func testTheBusyLockNeedsBothItsExitCodeAndItsToken() {
        let refusal = Data(#"{"reason":"busy","message":"another execution is in progress"}"#.utf8)

        XCTAssertEqual(
            UnpromptedRecoveryClient.interpret(exitCode: 75, stdout: refusal, stderr: Data()),
            .busy("Unattended recovery: another execution is in progress")
        )

        // Same body, different code: not the lock.
        guard case .failed = UnpromptedRecoveryClient.interpret(
            exitCode: 70, stdout: refusal, stderr: Data()
        ) else {
            return XCTFail("a refusal on a code that is not 75 must not read as the lock")
        }

        // Same code, a body that is not the lock's: also not the lock.
        let other = Data(#"{"reason":"scoped_path","message":"outside every configured root"}"#.utf8)
        guard case .failed = UnpromptedRecoveryClient.interpret(
            exitCode: 75, stdout: other, stderr: Data()
        ) else {
            return XCTFail("a different refusal on 75 must not read as the lock")
        }
    }

    func testAnUnreadableReportOnExitZeroIsAFailureAndNotARun() {
        guard case .failed = UnpromptedRecoveryClient.interpret(
            exitCode: 0, stdout: Data("not json".utf8), stderr: Data()
        ) else {
            return XCTFail("exit 0 with no readable report must not claim a run happened")
        }
    }

    // MARK: - What the app may say about one

    /// The partition `mayHaveDeleted` exists for, stated against the one it is
    /// nearly identical to.
    ///
    /// `notAuthorized` is where the two disagree, and getting that wrong in either
    /// direction is a real defect: read as "may have deleted" the app worries a user
    /// about a run `free` refused before it started, and read the other way round
    /// `failed` would reassure them about a run that stopped mid-deletion.
    func testWhatMayHaveDeletedIsNotTheSameQuestionAsWhatSpentTheAttempt() throws {
        let refused = UnpromptedRecoveryOutcome.notAuthorized("no grant covers this")
        XCTAssertTrue(refused.consumesTheAttempt)
        XCTAssertFalse(refused.mayHaveDeleted, "free refuses exit 3 before the loop starts")

        let stopped = UnpromptedRecoveryOutcome.failed("the run was stopped before it answered")
        XCTAssertTrue(stopped.consumesTheAttempt)
        XCTAssertTrue(stopped.mayHaveDeleted, "a run that stopped without reporting is unaccounted")

        let held = UnpromptedRecoveryOutcome.busy("another execution is in progress")
        XCTAssertFalse(held.consumesTheAttempt)
        XCTAssertFalse(held.mayHaveDeleted)
    }

    /// Campaign §9, in the smallest place it applies: a run is judged on the figure
    /// the volume was re-measured for, not on having run.
    func testARunIsJudgedOnItsMeasuredFigureRatherThanOnHavingRun() throws {
        XCTAssertTrue(UnpromptedRecoveryOutcome.ran(try runReport()).mayHaveDeleted)
        XCTAssertFalse(
            UnpromptedRecoveryOutcome.ran(try runReportThatTookNothing()).mayHaveDeleted,
            "the loop ran, re-read the volume, and nothing had gone"
        )
    }

    /// No automatic run — no runner, a grant that never opted in, an attempt never
    /// made — keeps the sentence every Mac had before HORO-1510.
    func testWithNoAutomaticRunTheReassuranceStands() {
        XCTAssertEqual(
            UnpromptedRecoveryAccount.deletionClause(after: nil),
            UnpromptedRecoveryAccount.nothingDeleted
        )
    }

    /// A user who comes back to a machine that deleted things on their behalf is owed
    /// the amount, in the sentence that mentions it — and must not also be told that
    /// nothing was deleted.
    func testAfterARunThatReclaimedTheClauseNamesTheMeasuredAmount() throws {
        let clause = UnpromptedRecoveryAccount.deletionClause(after: .ran(try runReport()))

        XCTAssertTrue(clause.contains(try runReport().bytesFreedMeasuredHuman), clause)
        XCTAssertFalse(clause.contains(UnpromptedRecoveryAccount.nothingDeleted), clause)
    }

    /// "Tried, and there is nothing here it may take" is a different thing to say
    /// than "not tried yet", and the difference is what tells a user whether pressing
    /// Review & recover is likely to help.
    func testARunThatTookNothingSaysSoRatherThanSayingNothingHappened() throws {
        let clause = UnpromptedRecoveryAccount.deletionClause(
            after: .ran(try runReportThatTookNothing())
        )

        XCTAssertEqual(clause, "Autopilot ran and found nothing it could safely reclaim.")
    }

    /// A run that stopped without reporting is the one outcome the app cannot account
    /// for, and it must not be accounted for by reassurance: "may already have been
    /// reclaimed" is what sends a user to look, and it is the only direction it is safe
    /// to be wrong in.
    ///
    /// The words are pinned rather than paraphrased, because this sentence is the whole
    /// behaviour — a `deletionClause` that quietly returned the reassurance here would
    /// be indistinguishable from a correct one to every other test in this file.
    func testAnUnaccountedRunSaysSoRatherThanReassuring() {
        XCTAssertEqual(
            UnpromptedRecoveryAccount.deletionClause(after: .failed("stopped")),
            "An automatic recovery run stopped without reporting, so some space may already "
                + "have been reclaimed."
        )
    }

    /// The invariant tying the sentence to the property that decides it: the
    /// reassurance is never said about an outcome that may have deleted something.
    ///
    /// Only that one direction holds, and the exception is worth stating rather than
    /// designing around — a run that took nothing has deleted nothing and *still* does
    /// not get the reassurance, because "tried, and there is nothing here it may take"
    /// is a different thing to tell someone deciding whether to press Review & recover.
    ///
    /// A loop rather than a case list, so a fifth outcome added later has this asked of
    /// it without anyone remembering to.
    func testTheReassuranceIsNeverSaidAboutARunThatMayHaveDeleted() throws {
        for outcome: UnpromptedRecoveryOutcome in [
            .ran(try runReport()),
            .ran(try runReportThatTookNothing()),
            .notAuthorized("no"),
            .busy("held"),
            .failed("stopped"),
        ] where outcome.mayHaveDeleted {
            XCTAssertNotEqual(
                UnpromptedRecoveryAccount.deletionClause(after: outcome),
                UnpromptedRecoveryAccount.nothingDeleted,
                "\(outcome)"
            )
        }
    }

    /// The two outcomes that attempted nothing keep the reassurance, because for them
    /// it is simply true.
    func testAnAttemptThatNeverStartedKeepsTheReassurance() {
        for outcome: UnpromptedRecoveryOutcome in [.notAuthorized("no"), .busy("held")] {
            XCTAssertEqual(
                UnpromptedRecoveryAccount.deletionClause(after: outcome),
                UnpromptedRecoveryAccount.nothingDeleted,
                "\(outcome) attempted nothing"
            )
        }
    }

    /// Which outcomes are dressed as a problem, and which are not.
    ///
    /// A run that reclaimed 139 GB is `success` rather than a warning: by
    /// `GlomerisStateMessage`'s own documentation that kind means something the user
    /// asked for happened, and on this path the asking is the grant they wrote.
    /// `failed` is the single outcome the app cannot account for, so it is the single
    /// one shown as a failure.
    func testOnlyAnUnaccountedRunIsShownAsAFailure() throws {
        for outcome: UnpromptedRecoveryOutcome? in [
            nil,
            .ran(try runReport()),
            .ran(try runReportThatTookNothing()),
            .notAuthorized("no"),
            .busy("held"),
        ] {
            XCTAssertEqual(
                UnpromptedRecoveryAccount.message(after: outcome).kind,
                .success,
                "\(String(describing: outcome)) is an account, not a problem"
            )
        }

        XCTAssertEqual(
            UnpromptedRecoveryAccount.message(after: .failed("stopped")).kind,
            .failure
        )
    }

    /// Whatever the outcome, the message a surface renders says the same thing as the
    /// sentence the notification carries. The two cards and the banner differing only
    /// in glyph is the drift this shared enum exists to prevent.
    func testTheStateMessageAndTheSentenceNeverDisagree() throws {
        for outcome: UnpromptedRecoveryOutcome? in [
            nil,
            .ran(try runReport()),
            .ran(try runReportThatTookNothing()),
            .notAuthorized("no"),
            .busy("held"),
            .failed("stopped"),
        ] {
            XCTAssertEqual(
                UnpromptedRecoveryAccount.message(after: outcome).title,
                UnpromptedRecoveryAccount.deletionClause(after: outcome)
            )
        }
    }

    /// The reassurance is worded once, and this is the guard that keeps it that way.
    ///
    /// Three surfaces have to account for one automatic run. Each of them said
    /// "Nothing has been deleted." as a constant before HORO-1510, and each of them
    /// would have gone on saying it — truthfully everywhere except the one Mac where
    /// it matters. A fourth surface that writes the sentence for itself is the same
    /// defect again, so it fails here rather than in front of a user.
    func testTheReassuranceIsWrittenInExactlyOnePlace() throws {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .appendingPathComponent("Sources")

        var files: [String] = []

        for name in try FileManager.default.contentsOfDirectory(atPath: sources.path)
            .filter({ $0.hasSuffix(".swift") })
            .sorted()
        {
            let source = try String(
                contentsOf: sources.appendingPathComponent(name), encoding: .utf8
            )
            for line in source.split(separator: "\n", omittingEmptySubsequences: false) {
                let code = line.trimmingCharacters(in: .whitespaces)
                guard !code.hasPrefix("//"), code.contains("Nothing has been deleted") else {
                    continue
                }
                files.append(name)
                break
            }
        }

        XCTAssertEqual(
            files,
            ["UnpromptedRecovery.swift"],
            """
            The sentence that tells a user nothing was deleted must be read from \
            UnpromptedRecoveryAccount, which checks whether it is true, rather than \
            written out by a surface that cannot (HORO-1510).
            """
        )
    }

    // MARK: - The vector

    /// `--unattended` is this app's statement about itself, and saying it is what
    /// lets Rust tell a run nobody started from one somebody typed. Its absence
    /// would make the two indistinguishable, and the check in `free` unreachable.
    func testTheVectorDeclaresItselfUnattendedAndAsksForTheGrant() {
        let arguments = UnpromptedRecoveryClient.arguments(
            goalUsedPercent: 60,
            projectRootsStore: ProjectRootsStore(defaults: TestUserDefaults.inMemory())
        )

        XCTAssertEqual(arguments.first, "free")
        XCTAssertTrue(arguments.contains("--autopilot"))
        XCTAssertTrue(arguments.contains("--unattended"))
        XCTAssertEqual(
            arguments.firstIndex(of: "--goal-used-percent").map { arguments[$0 + 1] },
            "60"
        )

        // No stop file: there is no window open to press Stop in. The envelope's
        // action, byte and time budgets are what bound an unattended run instead.
        XCTAssertFalse(arguments.contains("--stop-file"))
        // And nothing that would make it a preview — a run that previewed would
        // report having reclaimed nothing and spend the episode's attempt on it.
        XCTAssertFalse(arguments.contains("--dry-run"))
    }

    // MARK: - The loop

    @MainActor
    private func makeMonitor(
        showing outcomes: [PressureEpisodeOutcome],
        runner: StubUnpromptedRecovery?
    ) -> (PressureEpisodeMonitor, StubPressureReader, StubBannerRecorder) {
        let cli = StubPressureReader(showOutcomes: outcomes)
        let banners = StubBannerRecorder()
        return (
            PressureEpisodeMonitor(client: cli, banners: banners, unpromptedRecovery: runner),
            cli,
            banners
        )
    }

    /// The default, stated as a test rather than as an absence: a monitor with no
    /// runner cannot start anything, and behaves exactly as HORO-1508 left it.
    @MainActor
    func testAMonitorWithNoRunnerRaisesABannerAndStartsNothing() async throws {
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: nil)

        await monitor.pollOnce()

        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertNil(monitor.lastUnpromptedOutcome)
        XCTAssertNil(monitor.unpromptedMode, "no grant was read, so none may be reported")
    }

    /// AC 2's other half, and §6: a runner is present, the grant has not opted in,
    /// and the banner goes up with nothing deleted.
    @MainActor
    func testAGrantThatHasNotOptedInStillOnlyGetsABanner() async throws {
        let runner = StubUnpromptedRecovery(grant: try grant(startsUnprompted: false))
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()

        XCTAssertEqual(runner.runs, [], "nothing may run without the grant saying so")
        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertEqual(monitor.unpromptedMode, .askedFirst)
        XCTAssertNil(monitor.lastUnpromptedOutcome)
    }

    /// An `autopilot show` this app could not read is not consent either — and the
    /// monitor reports no mode at all rather than inventing one.
    @MainActor
    func testAnUnreadableGrantLeavesTheMonitorAsking() async throws {
        let runner = StubUnpromptedRecovery(grant: nil)
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()

        XCTAssertEqual(runner.runs, [])
        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertNil(monitor.unpromptedMode)
    }

    /// The whole path: threshold crossed → bounded run → re-read → the episode has
    /// closed, so no banner.
    ///
    /// The second report is what makes this the closed loop rather than a claim
    /// about one: the decision not to raise is taken against a volume measured
    /// *after* the deletions, not against the goal the run was aiming at.
    @MainActor
    func testAnOptedInGrantRunsOnceAndSaysNothingWhenTheDiskRecovers() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        let (monitor, cli, banners) = makeMonitor(
            showing: [.status(try owed()), .status(try recovered())],
            runner: runner
        )

        await monitor.pollOnce()

        XCTAssertEqual(runner.runs, [60], "the run aims at the goal the report named")
        XCTAssertEqual(banners.raised, [], "the run resolved it, so there was nothing to ask")
        XCTAssertEqual(cli.calls, ["show", "show"], "the second read is what decided the silence")
        XCTAssertEqual(monitor.unpromptedMode, .startsOnPressure)
        XCTAssertEqual(monitor.lastUnpromptedOutcome, .ran(try runReport()))
    }

    /// A bounded run that could not get the disk under its goal must not leave the
    /// user uninformed — and must not leave them waiting a poll interval either.
    /// The banner goes up in the same poll, from the report read after the run.
    @MainActor
    func testARunThatDidNotResolveItStillReachesTheUserInTheSamePoll() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        // Still owed on the second read: the disk is not under the goal.
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()

        XCTAssertEqual(runner.runs.count, 1)
        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertEqual(banners.raised.first?.key, key)
    }

    /// And it reaches them with an account of that run, which is what the banner's
    /// last sentence is composed from.
    ///
    /// This is the case the whole account exists for: the user is being told about a
    /// disk that is still too full *after* something was deleted on their behalf, and
    /// a banner ending "Nothing has been deleted" would be the app's most confident
    /// false sentence.
    @MainActor
    func testTheBannerAfterARunCarriesWhatThatRunDid() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()

        XCTAssertEqual(banners.raised.first?.automaticRun, .ran(try runReport()))
    }

    /// The default Mac's banner carries nothing, so its reassurance stands.
    @MainActor
    func testABannerWithNoRunBehindItCarriesNothing() async throws {
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: nil)

        await monitor.pollOnce()

        XCTAssertEqual(banners.raised.count, 1)
        XCTAssertNil(banners.raised.first?.automaticRun)
    }

    /// The outcome is remembered against the episode it belongs to, and the episode
    /// check is the whole point of storing it that way.
    ///
    /// Without it, next month's pressure event would open by reporting a deletion that
    /// happened during this one — an account that is wrong in the direction that
    /// matters, because it tells a user space was reclaimed when the disk in front of
    /// them has had nothing taken from it.
    @MainActor
    func testAnOutcomeIsNotReportedAgainstADifferentEpisode() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        let (monitor, _, _) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()

        XCTAssertEqual(
            monitor.automaticRun(forEpisodeIn: try owed()),
            .ran(try runReport()),
            "the episode it ran for"
        )
        XCTAssertNil(monitor.automaticRun(forEpisodeIn: try owedInASecondEpisode()))
        XCTAssertNil(
            monitor.automaticRun(forEpisodeIn: try recovered()),
            "a report with no open episode has no automatic run to account for"
        )
    }

    /// The deep link carries it too, because "Review & recover" lands on a window that
    /// opens with the same account — and a user who arrives there *because* a bounded
    /// run did not finish the job must not be told that nothing has been deleted.
    @MainActor
    func testReviewAndRecoverCarriesTheRunIntoTheRecoveryWindow() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        let opener = StubRecoveryOpener()
        let monitor = PressureEpisodeMonitor(
            client: StubPressureReader(showOutcomes: [.status(try owed())]),
            banners: StubBannerRecorder(),
            deepLink: opener,
            unpromptedRecovery: runner
        )

        await monitor.pollOnce()
        await monitor.answer(RecoveryDeepLink.reviewToken)

        XCTAssertEqual(opener.opened.count, 1)
        XCTAssertEqual(opener.opened.first??.automaticRun, .ran(try runReport()))
    }

    /// Five polls of a disk that stays full: one run.
    ///
    /// The recursion after a run is bounded to one extra pass, the episode's
    /// attempt is spent, and the banner already raised is not raised again. This
    /// asserts the outcome all three exist for — opting in bought one bounded
    /// attempt, not a sweep every thirty seconds.
    @MainActor
    func testRepeatedPollsOfOneEpisodeStartOneRun() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        for _ in 0..<5 {
            await monitor.pollOnce()
        }

        XCTAssertEqual(runner.runs.count, 1)
        XCTAssertEqual(banners.raised.count, 1)
    }

    /// A busy lock does not spend the attempt, so the next banner-worth of pressure
    /// runs.
    ///
    /// Driven through the monitor rather than the planner because the wiring is the
    /// part that could be wrong: `consumesTheAttempt` is consulted in one place, and
    /// an inverted reading there looks identical from the outside until an episode
    /// either loses its attempt or keeps being given new ones.
    @MainActor
    func testABusyLockDoesNotSpendTheEpisodesAttempt() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .busy("another execution is in progress")
        )
        let (monitor, cli, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()
        XCTAssertEqual(runner.runs.count, 1)
        // Nothing was deleted, so nothing is re-read — and the banner this poll
        // owed goes up rather than being swallowed by a run that did not happen.
        XCTAssertEqual(cli.calls.filter { $0 == "show" }.count, 1)
        XCTAssertEqual(banners.raised.count, 1)

        // The post-snooze reminder: a second crossing-worth of attention, and the
        // attempt the busy lock did not consume is still there to be used.
        cli.showOutcomes = [.status(try owedAgain())]
        runner.outcome = .ran(try runReport())
        await monitor.pollOnce()

        XCTAssertEqual(runner.runs.count, 2)
        XCTAssertEqual(monitor.lastUnpromptedOutcome, .ran(try runReport()))
    }

    /// And the converse, so the pair is pinned from both sides: a run that *did*
    /// happen spends the attempt, and the same banner-worth of pressure does not get
    /// another one.
    ///
    /// The banner is made not to reach the screen, which is what isolates the claim.
    /// `lastRaised` is only set for a banner that appeared, so both polls reach the
    /// recovery decision — and the second one is stopped by the spent attempt alone
    /// rather than by the banner non-reentrancy that would mask it.
    @MainActor
    func testASpentAttemptIsNotRenewedByAFurtherPoll() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .ran(try runReport())
        )
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)
        banners.reachesTheScreen = false

        await monitor.pollOnce()
        XCTAssertEqual(runner.runs.count, 1)

        await monitor.pollOnce()

        XCTAssertEqual(runner.runs.count, 1, "this key's attempt was already spent")
        XCTAssertEqual(
            banners.raised.count,
            2,
            "and the banner is still attempted, so the episode is not left unreported"
        )
    }

    /// A revoke between the grant read and the spawn lands on exit 3, and this app
    /// does not argue with it: Rust is where the check lives, and a client that
    /// treated its own read as the authority would be the second decider.
    @MainActor
    func testARunRefusedByRustIsRecordedAndTheUserIsAsked() async throws {
        let runner = StubUnpromptedRecovery(
            grant: try grant(startsUnprompted: true),
            outcome: .notAuthorized("Unattended recovery: not authorized")
        )
        let (monitor, _, banners) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()

        XCTAssertEqual(runner.runs.count, 1)
        XCTAssertEqual(
            monitor.lastUnpromptedOutcome,
            .notAuthorized("Unattended recovery: not authorized")
        )
        // Asked, which is what a refusal should fall back to rather than silence.
        XCTAssertEqual(banners.raised.count, 1)
    }

    /// The grant is re-read on every poll that owes a banner, so `revoke` takes
    /// effect here exactly as it does everywhere else in this product. A grant read
    /// once at launch would make this the one place it did not — and a poll that
    /// owes nothing does not ask at all, because reading it is for deciding rather
    /// than for watching a settings file every thirty seconds.
    @MainActor
    func testTheGrantIsReadPerOwedBannerAndNotOtherwise() async throws {
        let runner = StubUnpromptedRecovery(grant: try grant(startsUnprompted: false))
        let (monitor, cli, _) = makeMonitor(showing: [.status(try owed())], runner: runner)

        await monitor.pollOnce()
        XCTAssertEqual(runner.grantReads, 1)

        // A second banner-worth of pressure, so the poll owes something again.
        cli.showOutcomes = [.status(try owedAgain())]
        await monitor.pollOnce()
        XCTAssertEqual(runner.grantReads, 2)

        cli.showOutcomes = [.status(try recovered())]
        await monitor.pollOnce()
        XCTAssertEqual(runner.grantReads, 2)
    }

    /// `SIGTERM` partway through a deletion is unreportable, and
    /// `GlomerisClient.runRaw` sends one when its task is cancelled. So the
    /// unattended run must not be a child of the poll loop — which `stop()` and app
    /// teardown both cancel.
    ///
    /// A source check, like `GlomerisClientTests`'s guard on `performClean()`, and
    /// for the same reason: what has to be true is a property of the call site, and
    /// no runtime assertion can see the difference between an awaited child task and
    /// an awaited unstructured one.
    func testTheUnpromptedRunIsNeverReachedFromACancellableTask() throws {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // GlomerisMenuBar
            .appendingPathComponent("Sources")

        var callSites = 0

        for name in try FileManager.default.contentsOfDirectory(atPath: sources.path)
            .filter({ $0.hasSuffix(".swift") })
            .sorted()
        {
            let source = try String(
                contentsOf: sources.appendingPathComponent(name), encoding: .utf8
            )
            let lines = source.split(separator: "\n", omittingEmptySubsequences: false)
            for (index, line) in lines.enumerated() {
                let code = line.trimmingCharacters(in: .whitespaces)
                guard !code.hasPrefix("//"), code.contains(".run(goalUsedPercent:") else {
                    continue
                }
                callSites += 1

                // The `Task {}` that detaches it from the poll loop's cancellation
                // has to be the line immediately above, not merely somewhere in the
                // file: an unstructured task elsewhere would let this pass while the
                // run itself stayed a child of `pollTask`.
                let preceding = lines[..<index]
                    .last { !$0.trimmingCharacters(in: .whitespaces).isEmpty }?
                    .trimmingCharacters(in: .whitespaces)
                XCTAssertEqual(
                    preceding,
                    "let outcome = await Task { @MainActor in",
                    """
                    \(name):\(index + 1) starts an unattended recovery run outside an \
                    unstructured Task {}. The poll loop is cancelled by stop() and by app \
                    teardown, and GlomerisClient.runRaw turns that into SIGTERM — mid-deletion, \
                    unreportably (HORO-1510).
                    """
                )
            }
        }

        XCTAssertEqual(
            callSites,
            1,
            """
            expected exactly one unattended-run call site in Sources. Zero means this guard has \
            been silently defeated by a rename — update it rather than deleting it. More than \
            one means a second vector needs the same reasoning applied to it.
            """
        )
    }
}

// MARK: - Doubles

/// An `UnpromptedRecoveryRunning` that records and deletes nothing.
///
/// The whole reason the seam exists. A double here is not standing in for a real
/// `free --autopilot --unattended` because spawning one would be slow — it is
/// because spawning one would delete from the machine running the suite.
@MainActor
private final class StubUnpromptedRecovery: UnpromptedRecoveryRunning {
    var grant: AutopilotEnvelopeDto?
    var outcome: UnpromptedRecoveryOutcome

    private(set) var grantReads = 0

    /// The goal each run was asked for, in order.
    private(set) var runs: [Int] = []

    init(
        grant: AutopilotEnvelopeDto?,
        outcome: UnpromptedRecoveryOutcome = .failed("no outcome scripted")
    ) {
        self.grant = grant
        self.outcome = outcome
    }

    func readGrant() async -> AutopilotEnvelopeDto? {
        grantReads += 1
        return grant
    }

    func run(goalUsedPercent: Int) async -> UnpromptedRecoveryOutcome {
        runs.append(goalUsedPercent)
        return outcome
    }
}

/// A scripted `pressure` CLI.
///
/// Its own rather than shared with `PressureEpisodeMonitorTests`, whose doubles are
/// file-private — and it wants a different shape anyway: `markNotified` echoes the
/// *current* script rather than the first report, so a test can advance the script
/// between polls and have the whole monitor see the new state.
@MainActor
private final class StubPressureReader: PressureEpisodeReading {
    /// The last entry repeats, which is what a disk hovering over the threshold
    /// looks like from this app's side.
    var showOutcomes: [PressureEpisodeOutcome]

    /// Every call in order: `"show"`, `"notified"`, `"respond:<token>"`.
    private(set) var calls: [String] = []

    init(showOutcomes: [PressureEpisodeOutcome]) {
        self.showOutcomes = showOutcomes
    }

    func show() async -> PressureEpisodeOutcome {
        calls.append("show")
        guard let first = showOutcomes.first else { return .malformedOutput }
        if showOutcomes.count > 1 { showOutcomes.removeFirst() }
        return first
    }

    func markNotified() async -> PressureEpisodeOutcome {
        calls.append("notified")
        return showOutcomes.first ?? .malformedOutput
    }

    func respond(_ responseToken: String) async -> PressureEpisodeOutcome {
        calls.append("respond:" + responseToken)
        return showOutcomes.first ?? .malformedOutput
    }
}

/// A `RecoveryDeepLinkOpening` that records the context instead of opening a window.
///
/// Its own rather than shared with `PressureEpisodeMonitorTests`, whose doubles are
/// file-private.
@MainActor
private final class StubRecoveryOpener: RecoveryDeepLinkOpening {
    private(set) var opened: [RecoveryDeepLinkContext?] = []

    func openRecovery(_ context: RecoveryDeepLinkContext?) {
        opened.append(context)
    }
}

/// A banner raiser that records instead of notifying.
@MainActor
private final class StubBannerRecorder: PressureBannerRaising {
    var reachesTheScreen = true
    private(set) var raised: [PressureBanner] = []
    var onAnswer: ((String) -> Void)?

    func prepare(responseTokens: [String]) {}

    func raise(_ banner: PressureBanner) async -> Bool {
        raised.append(banner)
        return reachesTheScreen
    }
}
