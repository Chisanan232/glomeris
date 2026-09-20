//
//  StatusHealthSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1062: proves the ticket's core AC — launch-agent-loaded and
//  heartbeat freshness are rendered as two independently distinguishable
//  facts, never collapsed into one combined "healthy" indicator — against
//  DaemonHealthViewModel, the pure mapping step StatusHealthSectionView
//  renders from. Testing the view-model directly (rather than the
//  SwiftUI view tree) is what makes "independently distinguishable"
//  mechanically checkable: two DaemonStatusReportDto inputs that disagree
//  on both axes must produce two view-model outputs that disagree on
//  both axes independently, not just "look different overall".
//

import XCTest

final class StatusHealthSectionViewTests: XCTestCase {
    private func daemonReport(loaded: Bool, heartbeatAgeSecs: UInt64?) -> DaemonStatusReportDto {
        DaemonStatusReportDto(
            plistInstalled: loaded,
            plistPath: "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist",
            loaded: loaded,
            heartbeatAgeSecs: heartbeatAgeSecs
        )
    }

    /// Core AC: loaded=true+fresh-heartbeat vs. loaded=false+no-heartbeat
    /// must differ on the loaded axis AND the heartbeat axis
    /// independently — not merely differ overall, which a single
    /// collapsed "Healthy"/"Unhealthy" field would already satisfy.
    ///
    /// HORO-1306 changed the two properties from display strings to
    /// `GlomerisTerm`s. The wording moved; what this test protects did not.
    func testLoadedAndHeartbeatAreIndependentlyDistinguishable() {
        let healthy = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 8))
        let neverRan = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: nil))

        XCTAssertNotEqual(healthy.loadedTerm, neverRan.loadedTerm)
        XCTAssertNotEqual(healthy.heartbeatTerm, neverRan.heartbeatTerm)

        // Cross-check: a report that is loaded but has never sent a
        // heartbeat must show "loaded" distinctly from a report that is
        // both loaded and has a fresh heartbeat, proving the two facts
        // are not derived from each other.
        let loadedButNoHeartbeat = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: nil))
        XCTAssertEqual(loadedButNoHeartbeat.loadedTerm, healthy.loadedTerm)
        XCTAssertNotEqual(loadedButNoHeartbeat.heartbeatTerm, healthy.heartbeatTerm)
    }

    /// The same independence, stated on the axis a reader of the popover
    /// would use: neither term's *title* may be recoverable from the other
    /// fact. A combined indicator would make both titles change together.
    func testNeitherFactCanBeReadOffTheOther() {
        let loadedFresh = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 8))
        let loadedSilent = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: nil))
        let unloadedStaleHeartbeat = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: 8))

        // Same loaded fact, different heartbeat fact.
        XCTAssertEqual(loadedFresh.loadedTerm.title, loadedSilent.loadedTerm.title)
        XCTAssertNotEqual(loadedFresh.heartbeatTerm.title, loadedSilent.heartbeatTerm.title)

        // Same heartbeat fact, different loaded fact — the case a single
        // "healthy" boolean would erase.
        XCTAssertEqual(loadedFresh.heartbeatTerm.title, unloadedStaleHeartbeat.heartbeatTerm.title)
        XCTAssertNotEqual(loadedFresh.loadedTerm.title, unloadedStaleHeartbeat.loadedTerm.title)
    }

    func testLoadedTrueReadsAsRunning() {
        let viewModel = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 8))
        XCTAssertEqual(viewModel.loadedTerm.title, "Running")
        XCTAssertEqual(viewModel.loadedTerm.tone, .positive)
        XCTAssertEqual(viewModel.heartbeatTerm.title, "8s ago")
    }

    func testLoadedFalseReadsAsNotRunningAndSaysWhatThatCosts() {
        let viewModel = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: nil))
        XCTAssertEqual(viewModel.loadedTerm.title, "Not running")
        XCTAssertNotEqual(
            viewModel.loadedTerm.tone,
            .positive,
            "an unwatched disk must not read as reassuring"
        )
        // The consequence, not just the state: a user who sees "Not
        // running" needs to know what they lose by leaving it that way.
        XCTAssertTrue(viewModel.loadedTerm.explanation.contains("not be warned"))
    }

    /// The "daemon not installed / never ran" case: `heartbeatAgeSecs`
    /// is `nil`, which must render as an explicit message, not a crash
    /// or a garbage "0s ago"/empty string.
    func testNilHeartbeatAgeSaysSoRatherThanShowingZero() {
        let viewModel = DaemonHealthViewModel(daemonReport(loaded: false, heartbeatAgeSecs: nil))
        XCTAssertEqual(viewModel.heartbeatTerm.title, "No check-in yet")
        XCTAssertNil(viewModel.heartbeatAgeDescription)
        XCTAssertFalse(viewModel.heartbeatTerm.title.contains("0s"))
    }

    func testHeartbeatAgeFormatsMinutesAndHours() {
        let minutes = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 125))
        XCTAssertEqual(minutes.heartbeatAgeDescription, "2m")
        XCTAssertEqual(minutes.heartbeatTerm.title, "2m ago")

        let hours = DaemonHealthViewModel(daemonReport(loaded: true, heartbeatAgeSecs: 7_300))
        XCTAssertEqual(hours.heartbeatAgeDescription, "2h")
        XCTAssertEqual(hours.heartbeatTerm.title, "2h ago")
    }

    /// A heartbeat age must not be toned as stale or fresh at any age.
    /// Picking the cutoff would be inventing a freshness threshold in
    /// Swift, and thresholds are the CLI's to set — the standing rule in
    /// GlomerisMenuBarApp.swift keeps that judgment out of this target.
    /// The age is stated plainly and the user judges it.
    func testHeartbeatAgeCarriesNoFreshnessVerdict() {
        for seconds: UInt64 in [0, 8, 125, 7_300, 604_800] {
            let viewModel = DaemonHealthViewModel(
                daemonReport(loaded: true, heartbeatAgeSecs: seconds)
            )
            XCTAssertEqual(
                viewModel.heartbeatTerm.tone,
                .neutral,
                """
                a \(seconds)s-old heartbeat is toned \
                \(viewModel.heartbeatTerm.tone) — that is a staleness \
                threshold, which belongs in the Rust CLI
                """
            )
        }
    }

    // MARK: - HORO-1297: a failing fetch must stay visible

    /// The regression proper. `status` and `daemon status` are dispatched
    /// concurrently and each reports into the shared error state. When the
    /// two shared one slot, a daemon failure was overwritten by the
    /// status success that landed after it — so the panel showed a healthy
    /// disk line and *no* indication that daemon health had failed to
    /// decode at all. Ordering the writes the unlucky way must still leave
    /// the daemon message outstanding.
    func testDaemonFailureIsNotErasedByAConcurrentStatusSuccess() {
        var errors = SectionFetchErrors()

        errors.daemon = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed("typeMismatch(...)"),
            subject: "daemon status"
        )
        // The status fetch finishes second and succeeds — it clears only
        // its own slot.
        errors.status = nil

        XCTAssertEqual(errors.messages.count, 1)
        XCTAssertTrue(errors.messages[0].hasPrefix("daemon status:"))
    }

    /// Symmetry: the same must hold with the roles reversed, so this is a
    /// property of the type rather than an artifact of one ordering.
    func testStatusFailureIsNotErasedByAConcurrentDaemonSuccess() {
        var errors = SectionFetchErrors()
        errors.status = SectionFetchErrors.shortMessage(
            GlomerisClientError.executionFailed("no such file"),
            subject: "status"
        )
        errors.daemon = nil

        XCTAssertEqual(errors.messages.count, 1)
        XCTAssertTrue(errors.messages[0].hasPrefix("status:"))
    }

    func testBothFailuresAreRenderedNotJustTheLastOne() {
        var errors = SectionFetchErrors()
        errors.status = SectionFetchErrors.shortMessage(
            GlomerisClientError.unexpectedExitCode(9),
            subject: "status"
        )
        errors.daemon = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed("dataCorrupted(...)"),
            subject: "daemon status"
        )

        XCTAssertEqual(errors.messages.count, 2)
    }

    func testNoErrorsRendersNothing() {
        XCTAssertTrue(SectionFetchErrors().messages.isEmpty)
    }

    /// A recovered fetch clears its own message, so a transient failure
    /// does not stick around forever.
    func testSucceedingAgainClearsThatFetchsMessage() {
        var errors = SectionFetchErrors()
        errors.daemon = SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed("x"),
            subject: "daemon status"
        )
        XCTAssertFalse(errors.messages.isEmpty)

        errors.daemon = nil
        XCTAssertTrue(errors.messages.isEmpty)
    }

    /// "Visibly" means legible. A multi-line `DecodingError` dump in a
    /// 260pt popover is technically visible and practically useless; the
    /// message must be one line that names the subcommand and says the
    /// output was not the expected JSON.
    func testDecodeFailureMessageIsOneActionableLineNotADecodingErrorDump() throws {
        let message = try XCTUnwrap(SectionFetchErrors.shortMessage(
            GlomerisClientError.outputDecodingFailed(
                "typeMismatch(Swift.Bool, Swift.DecodingError.Context(codingPath: [], debugDescription: \"…\"))"
            ),
            subject: "daemon status"
        ))

        XCTAssertEqual(message, "daemon status: the CLI's output was not the expected JSON.")
        XCTAssertFalse(message.contains("\n"))
        XCTAssertFalse(message.contains("typeMismatch"))
        XCTAssertFalse(message.contains("codingPath"))
    }

    func testExecutionFailureSurfacesTheCliStderrVerbatim() {
        let message = SectionFetchErrors.shortMessage(
            GlomerisClientError.executionFailed("glomeris daemon status: HOME is not set\n"),
            subject: "daemon status"
        )
        XCTAssertEqual(message, "daemon status: glomeris daemon status: HOME is not set")
    }

    /// A spawn failure with nothing on stderr must still say something —
    /// never render as a bare "daemon status: " with an empty tail.
    func testEmptyStderrStillProducesAMeaningfulSentence() {
        let message = SectionFetchErrors.shortMessage(
            GlomerisClientError.executionFailed("   \n"),
            subject: "daemon status"
        )
        XCTAssertEqual(message, "daemon status: the CLI could not be run.")
    }

    /// A non-`GlomerisClientError` (anything thrown from outside the
    /// client's own typed set) must not fall through to an empty string.
    func testUnknownErrorTypeStillProducesAMessage() throws {
        struct Weird: Error {}
        let message = try XCTUnwrap(
            SectionFetchErrors.shortMessage(Weird(), subject: "status")
        )
        XCTAssertTrue(message.hasPrefix("status: failed —"))
        XCTAssertGreaterThan(message.count, "status: failed —".count)
    }

    // MARK: - Cancellation (HORO-1308)

    /// The one case that must produce no message at all. Pressing Stop on the
    /// AI Plan, or closing the popover mid-poll, is a user decision — rendering
    /// it as a red failure line would teach the user to distrust a panel whose
    /// entire job is to be trusted about failures (HORO-1297).
    func testCancellationProducesNoMessageAtAll() {
        XCTAssertNil(
            SectionFetchErrors.shortMessage(
                GlomerisClientError.cancelled,
                subject: "AI plan"
            )
        )
    }

    /// …and it is the ONLY case that does. A future `GlomerisClientError` case
    /// added without a `shortMessage` arm would be caught by Swift's exhaustive
    /// `switch`, but a case added and then mapped to `nil` out of convenience
    /// would not be — so every other constructible case is asserted non-nil
    /// here.
    func testEveryNonCancellationCaseStillProducesAMessage() {
        let others: [GlomerisClientError] = [
            .executableNotFound(searched: ["PATH"]),
            .executionFailed("boom"),
            .outputDecodingFailed("boom"),
            .unexpectedExitCode(7),
            .usage("bad flag"),
        ]
        for error in others {
            XCTAssertNotNil(
                SectionFetchErrors.shortMessage(error, subject: "status"),
                "\(error) must still produce a sentence"
            )
        }
    }

    // MARK: - Missing CLI (HORO-1295)

    /// HORO-1295's user was someone who had already installed the CLI, at a
    /// path the app never consulted. So "not found" on its own is the one
    /// unhelpful thing this message could say: it must name the places that
    /// were searched, so a mismatch between where the binary is and where the
    /// app looked is visible from the popover rather than only from the
    /// source.
    func testMissingCliMessageNamesWhereTheAppLooked() throws {
        let searched = GlomerisExecutableLocator(
            bundledExecutableURL: nil,
            pathVariable: "/usr/bin:/bin:/usr/sbin:/sbin"
        ).searchedLocations

        let message = try XCTUnwrap(SectionFetchErrors.shortMessage(
            GlomerisClientError.executableNotFound(searched: searched),
            subject: "status"
        ))

        XCTAssertEqual(
            message,
            "status: the glomeris CLI was not found. Looked in: "
                + "the app bundle, PATH, /opt/homebrew/bin, /usr/local/bin."
        )
        XCTAssertFalse(message.contains("\n"))
    }

    /// Mechanical cross-view guard, in the style of
    /// `GlomerisClientTests.testSourceContainsNoShellExecution`: every view
    /// that runs the CLI routes its failures through `shortMessage`. Before
    /// HORO-1295 four of the five call sites interpolated the raw Swift error
    /// instead, which is how a missing binary reached the popover as a
    /// `DecodingError`-shaped dump. Asserted on the source because the
    /// property is absolute — there is no view for which a raw dump is the
    /// right thing to show.
    func testNoViewRendersARawSwiftErrorDescription() throws {
        let sourcesDirectory = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources")

        for name in [
            "StatusHealthSectionView.swift",
            "CandidatesSectionView.swift",
            "CandidateDetailView.swift",
            "HistoryAuditSectionView.swift",
        ] {
            let source = try String(
                contentsOf: sourcesDirectory.appendingPathComponent(name),
                encoding: .utf8
            )
            let code = source
                .split(separator: "\n", omittingEmptySubsequences: false)
                .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
                .joined(separator: "\n")

            XCTAssertFalse(
                code.contains("String(describing: error)"),
                """
                \(name) must not put a raw Swift error into a user-facing \
                string; route it through SectionFetchErrors.shortMessage \
                (HORO-1295).
                """
            )
        }
    }
}
