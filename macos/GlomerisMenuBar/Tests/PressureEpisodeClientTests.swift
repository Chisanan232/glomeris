//
//  PressureEpisodeClientTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1508. The app's half of the daemon/app seam: the three argument vectors
//  and the exit-code contract they are read under.
//
//  The claims worth pinning here are not "the CLI can be run". They are:
//
//    * exit 3 is not a failure. It is the CLI saying "understood, and there was
//      nothing to record" — the ordinary cause being the good news that the disk
//      recovered before the button was pressed. A client that reported it as a
//      malfunction would show a fault for a race that resolved itself correctly,
//      and would keep retrying a refusal that can only ever repeat;
//    * exit 3's *body* survives. `GlomerisClient.run` discards stdout for every
//      non-zero exit, and for this verb stdout is the entire answer — which is why
//      the client uses `runRaw`;
//    * a version skew is never read as a refusal, and a refusal is never read as a
//      version skew. One is this app's problem; neither is the user's to fix, but
//      only one of them means the app and the CLI disagree about what exists;
//    * this app cannot invent a fourth answer. The three tokens are published by
//      `pressure show`; none of them is spelled as a literal in the command
//      builder, so there is no place for a fourth to be typed;
//    * none of the three verbs carries `--project-root`, and none names a path.
//
//  The report tests start from the same `tests/fixtures/dto/` fixtures the Rust
//  side asserts `pressure show --json` serializes to, so a change to the report's
//  shape reaches this file rather than being absorbed by a hand-written literal.
//

import XCTest

final class PressureEpisodeClientTests: XCTestCase {

    // MARK: - Fixtures

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

    /// Threshold 85% used, disk at 91% used, one open episode with a banner owed.
    private func statusReport() throws -> PressureStatusReportDto {
        try JSONDecoder().decode(
            PressureStatusReportDto.self,
            from: try fixtureData("pressure_status_report.json")
        )
    }

    private func rejectionReport() throws -> PressureRejectionReportDto {
        try JSONDecoder().decode(
            PressureRejectionReportDto.self,
            from: try fixtureData("pressure_rejection_report.json")
        )
    }

    // MARK: - Interpretation

    func testExitZeroWithAReportIsTheStatus() throws {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 0,
            stdout: try fixtureData("pressure_status_report.json"),
            stderr: Data()
        )
        guard case .status(let decoded) = outcome else {
            return XCTFail("expected .status, got \(outcome)")
        }
        // The two figures the notifier decides on, and they are not the same
        // number: what the user asked to be told at, and where the disk is now.
        XCTAssertEqual(decoded.notifyAtUsedPercent, 85.0)
        XCTAssertEqual(decoded.current.usedPercent, 91.0)
        XCTAssertTrue(decoded.notificationDue)
    }

    /// The case this whole type exists for. `EXIT_PRESSURE_NOTHING_TO_RECORD` is
    /// 3, it prints a structured reason on stdout, and it is not a failure.
    func testExitThreeWithARejectionIsNothingToRecordAndNotAFailure() throws {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: PressureEpisodeInterpretation.nothingToRecordExitCode,
            stdout: try fixtureData("pressure_rejection_report.json"),
            stderr: Data()
        )
        guard case .nothingToRecord(let decoded) = outcome else {
            return XCTFail("expected .nothingToRecord, got \(outcome)")
        }
        XCTAssertEqual(decoded.reason, "no_notification_due")
        // Rust's own sentence, not a paraphrase.
        XCTAssertEqual(
            decoded.message,
            "no pressure notification is owed (there is no open episode, or its notification "
                + "was already raised)"
        )
        // And it is settled: retrying would produce the same refusal forever.
        XCTAssertTrue(outcome.isSettled)
    }

    /// Exit 3 from a CLI that refused for a reason it did not publish, or from the
    /// non-`--json` path. Still nothing was recorded, so it is still
    /// `.nothingToRecord` — but under a token that claims nothing specific.
    func testExitThreeWithNoReportIsStillNothingToRecordUnderAnHonestToken() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 3,
            stdout: Data(),
            stderr: Data("glomeris pressure: there is no open pressure episode".utf8)
        )
        guard case .nothingToRecord(let synthesised) = outcome else {
            return XCTFail("expected .nothingToRecord, got \(outcome)")
        }
        XCTAssertEqual(synthesised.reason, PressureRejectionReportDto.unknownReason)
        XCTAssertEqual(synthesised.message, "there is no open pressure episode")
    }

    /// The synthesised token must stay outside the set Rust publishes, so it can
    /// never be mistaken for a specific refusal — and the wording it gets must be
    /// the honest one rather than a confident sentence about a state nothing
    /// established.
    func testTheSynthesisedTokenIsNotOneOfRustsAndReadsAsUnrecognised() {
        for real in ["no_open_episode", "no_notification_due"] {
            XCTAssertNotEqual(PressureRejectionReportDto.unknownReason, real)
        }
        let term = GlomerisVocabulary.episodeRejection(PressureRejectionReportDto.unknownReason)
        XCTAssertEqual(term.tone, .unknown)
        XCTAssertEqual(term.axis, GlomerisVocabulary.episodeRejectionAxis)
    }

    /// Exit 2 saying the subcommand does not exist: the installed CLI is older
    /// than this app. Not the user's to fix, and not a refusal of anything.
    func testExitTwoNamingAnUnknownSubcommandIsAVersionSkew() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: Data("glomeris pressure: unknown subcommand 'show'".utf8)
        )
        XCTAssertEqual(outcome, .usageError("unknown subcommand 'show'"))
    }

    /// The other skew shape: the verb exists but the answer token does not.
    func testExitTwoNamingAnUnknownAnswerIsAlsoAVersionSkew() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: Data("glomeris pressure: unknown answer 'snooze' (one of …)".utf8)
        )
        guard case .usageError = outcome else {
            return XCTFail("expected .usageError, got \(outcome)")
        }
    }

    /// A usage error prints the message and then the command's whole usage block.
    /// Only the first line is a sentence; the rest is a help page, and rendering
    /// it inside a notification or a card would be nonsense.
    func testOnlyTheFirstStderrLineBecomesTheSentence() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: Data(
                """
                glomeris pressure: unknown subcommand 'shwo'
                Usage: glomeris pressure [show|notified|respond <answer>]

                  show      what is owed
                """.utf8)
        )
        XCTAssertEqual(outcome, .usageError("unknown subcommand 'shwo'"))
    }

    func testExitTwoWithoutASkewMarkerIsAPlainFailure() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: Data("glomeris pressure: show takes no arguments (got '--verbose')".utf8)
        )
        XCTAssertEqual(outcome, .failed("show takes no arguments (got '--verbose')"))
    }

    func testExitOneIsAStateFileFailureWithItsOwnSentence() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 1,
            stdout: Data(),
            stderr: Data("glomeris pressure: failed to record the episode state: disk full".utf8)
        )
        XCTAssertEqual(outcome, .failed("failed to record the episode state: disk full"))
    }

    func testExitZeroWithUnreadableStdoutIsMalformed() {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 0,
            stdout: Data("Disk usage: 91% used\nAlert at: 85% used".utf8),
            stderr: Data()
        )
        XCTAssertEqual(outcome, .malformedOutput)
    }

    /// A report on stdout beats the exit code, so an unfamiliar non-zero exit
    /// cannot throw away the only structured account of the disk.
    func testAReportOnStdoutWinsOverANonZeroExit() throws {
        let outcome = PressureEpisodeInterpretation.interpret(
            exitCode: 9,
            stdout: try fixtureData("pressure_status_report.json"),
            stderr: Data("glomeris pressure: something went wrong".utf8)
        )
        guard case .status = outcome else {
            return XCTFail("expected .status, got \(outcome)")
        }
    }

    /// The two shapes must stay decode-disjoint. If they ever converge, the order
    /// in `interpret` is what decides, and this is the assertion that would fail
    /// first.
    func testAStatusReportIsNotAlsoDecodableAsARejection() throws {
        XCTAssertNil(
            try? JSONDecoder().decode(
                PressureRejectionReportDto.self,
                from: try fixtureData("pressure_status_report.json")
            ))
        XCTAssertNil(
            try? JSONDecoder().decode(
                PressureStatusReportDto.self,
                from: try fixtureData("pressure_rejection_report.json")
            ))
    }

    /// Silence is the one outcome a background service must not act on as though
    /// it were news. Every failure path produces a sentence.
    func testEveryFailurePathStillSaysSomething() {
        for code: Int32 in [1, 2, 7] {
            let outcome = PressureEpisodeInterpretation.interpret(
                exitCode: code, stdout: Data(), stderr: Data())
            guard case .failed(let detail) = outcome else {
                return XCTFail("exit \(code): expected .failed, got \(outcome)")
            }
            XCTAssertFalse(detail.isEmpty, "exit \(code) produced an empty sentence")
        }
        let three = PressureEpisodeInterpretation.interpret(
            exitCode: 3, stdout: Data(), stderr: Data())
        guard case .nothingToRecord(let refusal) = three else {
            return XCTFail("exit 3 with nothing printed must still be .nothingToRecord")
        }
        XCTAssertFalse(refusal.message.isEmpty)
    }

    // MARK: - Which outcomes are worth retrying

    /// The judgement a polling loop reads. Getting `.nothingToRecord` wrong here
    /// is the expensive direction: an unsettled refusal would be retried at poll
    /// speed forever, for a condition that has already resolved.
    func testOnlyTheOutcomesTheCliUnderstoodAreSettled() throws {
        XCTAssertTrue(PressureEpisodeOutcome.status(try statusReport()).isSettled)
        XCTAssertTrue(PressureEpisodeOutcome.nothingToRecord(try rejectionReport()).isSettled)

        XCTAssertFalse(PressureEpisodeOutcome.usageError("skew").isSettled)
        XCTAssertFalse(PressureEpisodeOutcome.malformedOutput.isSettled)
        XCTAssertFalse(PressureEpisodeOutcome.failed("no binary").isSettled)
    }

    /// Only a status carries a report. A refusal that handed one back would let a
    /// caller refresh its view of the disk from a call that read nothing.
    func testOnlyAStatusCarriesAReport() throws {
        XCTAssertNotNil(PressureEpisodeOutcome.status(try statusReport()).report)
        XCTAssertNil(PressureEpisodeOutcome.nothingToRecord(try rejectionReport()).report)
        XCTAssertNil(PressureEpisodeOutcome.malformedOutput.report)
        XCTAssertNil(PressureEpisodeOutcome.failed("no binary").report)
    }

    // MARK: - The argument vectors

    func testEveryVerbAsksForJsonAndNamesNoPath() {
        let vectors = [
            PressureEpisodeCommands.show,
            PressureEpisodeCommands.notified,
            PressureEpisodeCommands.respond("review_and_recover"),
        ]
        for vector in vectors {
            XCTAssertEqual(vector.first, "pressure", "\(vector) is not a pressure invocation")
            XCTAssertTrue(vector.contains("--json"), "\(vector) does not ask for JSON")
            for argument in vector {
                XCTAssertFalse(
                    argument.hasPrefix("/") || argument.hasPrefix("~"),
                    "\(vector) names a filesystem path: \(argument)"
                )
            }
        }
    }

    /// An episode is about the whole volume and is opened by a daemon that knows
    /// nothing of this app's configured projects. A `--project-root` here would
    /// read as a per-project answer to a machine-wide record.
    func testNoVerbIsProjectRootScoped() {
        for vector in [
            PressureEpisodeCommands.show,
            PressureEpisodeCommands.notified,
            PressureEpisodeCommands.respond("ignore_episode"),
        ] {
            XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(vector))
            XCTAssertFalse(vector.contains("--project-root"))
        }
    }

    /// The answer goes where `src/main.rs` reads it from: the first positional
    /// after the verb, before `--json`.
    func testTheAnswerIsTheFirstPositionalAfterTheVerb() {
        XCTAssertEqual(
            PressureEpisodeCommands.respond("remind_later"),
            ["pressure", "respond", "remind_later", "--json"]
        )
    }

    /// Every token the CLI publishes round-trips into a vector unchanged. This is
    /// what lets the notifier build its buttons from `responses` without knowing
    /// what any of them say.
    func testEveryPublishedAnswerSurvivesIntoItsVector() throws {
        let published = try statusReport().responses
        XCTAssertEqual(published.count, 3, "the fixture should publish three answers")
        for token in published {
            XCTAssertEqual(PressureEpisodeCommands.respond(token)[2], token)
        }
    }

    /// The structural reason this app cannot offer a fourth button: none of the
    /// three tokens is written down in the command builder, so there is nowhere
    /// for a fourth to be typed. The tokens it sends came from `responses`, or
    /// from an action identifier built out of it.
    ///
    /// Asserted against the source text, the same way `GlomerisClientTests` pins
    /// "no shell interpreter" — a rule about what is *not* written cannot be
    /// checked by calling anything.
    func testTheCommandBuilderSpellsNoAnswerTokenItself() throws {
        let source = try String(
            contentsOf: URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()  // Tests
                .deletingLastPathComponent()  // GlomerisMenuBar
                .appendingPathComponent("Sources/PressureEpisodeClient.swift"),
            encoding: .utf8
        )
        for token in ["review_and_recover", "remind_later", "ignore_episode"] {
            XCTAssertFalse(
                source.contains("\"\(token)\""),
                "PressureEpisodeClient.swift spells \(token) as a literal — the answer tokens "
                    + "must arrive from the CLI's own `responses`, or this app can invent one"
            )
        }
    }
}
