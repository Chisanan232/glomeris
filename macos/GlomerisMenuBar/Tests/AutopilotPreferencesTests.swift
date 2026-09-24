//
//  AutopilotPreferencesTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1310. Covers the parts of the Autopilot settings screen that are not
//  SwiftUI: the exit-code contract, the grant the form composes, and the
//  sentences said about the result.
//
//  The claims worth pinning here are not "the form works". They are:
//
//    * a draft can never compose a grant the CLI would reject — every budget
//      stays inside the ceilings the CLI reported, and no argument names a
//      path or a credential;
//    * a pre-authorization cannot outlive the kind it applies to;
//    * a failed revoke never reads as a success, and a failed *enable* never
//      claims a grant that exists — the two failure directions are opposites
//      and are worded by two separate functions on purpose;
//    * the report on stdout beats the exit code, so a non-zero exit cannot
//      throw away the only structured account of what is now in force.
//
//  The draft tests start from the same `tests/fixtures/dto/` fixture the Rust
//  side asserts `autopilot show --json` serializes to, so a change to the
//  report's shape reaches this file rather than being absorbed by a
//  hand-written literal.
//

import XCTest

final class AutopilotPreferencesTests: XCTestCase {

    // MARK: - Fixtures

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func loadEnvelopeFixture() throws -> AutopilotEnvelopeDto {
        let url = Self.fixturesDir.appendingPathComponent("autopilot_envelope_report.json")
        return try JSONDecoder().decode(AutopilotEnvelopeDto.self, from: Data(contentsOf: url))
    }

    private func fixtureData() throws -> Data {
        try Data(
            contentsOf: Self.fixturesDir.appendingPathComponent("autopilot_envelope_report.json"))
    }

    // MARK: - Interpretation

    func testExitZeroWithAReportIsTheEnvelope() throws {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 0,
            stdout: try fixtureData(),
            stderr: Data()
        )

        guard case .envelope(let report) = outcome else {
            return XCTFail("expected an envelope, got \(outcome)")
        }
        XCTAssertTrue(report.enabled)
        XCTAssertEqual(report.allowedKinds, ["node_modules", "cargo_target_dir"])
    }

    /// The report wins over a non-zero exit. Nothing in today's contract
    /// prints one alongside a failure, but the ordering is the safe one: if a
    /// future version exits non-zero *after* writing, the state that reached
    /// the file is still what the screen shows.
    func testAReportOnStdoutBeatsANonZeroExit() throws {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 1,
            stdout: try fixtureData(),
            stderr: Data("glomeris autopilot: something went wrong".utf8)
        )

        guard case .envelope = outcome else {
            return XCTFail("expected the report to win, got \(outcome)")
        }
    }

    func testStoreFailureIsAFailureWithTheCliSentence() {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 1,
            stdout: Data(),
            stderr: Data("glomeris autopilot: failed to write the envelope: disk full".utf8)
        )

        XCTAssertEqual(outcome, .failed("failed to write the envelope: disk full"))
    }

    func testUnrecognizedArgumentIsAUsageError() {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 2,
            stdout: Data("usage: glomeris autopilot …".utf8),
            stderr: Data("glomeris autopilot: unrecognized argument '--json'".utf8)
        )

        XCTAssertEqual(outcome, .usageError("unrecognized argument '--json'"))
    }

    /// Exit 2 without that marker is a refused grant, not a version skew —
    /// the user's request, not the app's vocabulary.
    func testOtherUsageExitsAreFailures() {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: Data("glomeris autopilot: --max-actions: 99 is above the ceiling of 25".utf8)
        )

        XCTAssertEqual(outcome, .failed("--max-actions: 99 is above the ceiling of 25"))
    }

    func testExitZeroWithUnreadableOutputIsMalformed() {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 0,
            stdout: Data("enabled: yes".utf8),
            stderr: Data()
        )

        XCTAssertEqual(outcome, .malformedOutput)
    }

    func testASilentFailureStillSaysSomething() {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 9,
            stdout: Data(),
            stderr: Data()
        )

        guard case .failed(let detail) = outcome else {
            return XCTFail("expected a failure, got \(outcome)")
        }
        XCTAssertTrue(detail.contains("9"), "the exit code is the only fact there is: \(detail)")
    }

    /// An unprefixed line is shown verbatim rather than mangled by an
    /// over-eager strip.
    func testAnUnprefixedSentenceSurvivesIntact() {
        let outcome = AutopilotEnvelopeInterpretation.interpret(
            exitCode: 1,
            stdout: Data(),
            stderr: Data("something else entirely".utf8)
        )

        XCTAssertEqual(outcome, .failed("something else entirely"))
    }

    // MARK: - The draft

    func testTheDraftStartsFromWhatIsInForce() throws {
        let draft = AutopilotDraft.from(try loadEnvelopeFixture())

        XCTAssertEqual(draft.allowedKinds, ["node_modules", "cargo_target_dir"])
        XCTAssertEqual(draft.preauthorizedAsks, ["node_modules:rebuild_cost_high"])
        XCTAssertEqual(draft.maxActions, 2)
        XCTAssertEqual(draft.maxGigabytes, 2)
        XCTAssertEqual(draft.maxDurationSeconds, 120)
        XCTAssertEqual(draft.minPressure, "PRESSURED")
    }

    /// Pressing Enable without touching anything must re-grant what was there.
    /// The byte budget is the one that could quietly shrink, because it is the
    /// only field the form does not hold in the CLI's own units.
    func testAnUntouchedDraftAsksForWhatWasAlreadyGranted() throws {
        let report = try loadEnvelopeFixture()
        let draft = AutopilotDraft.from(report)

        XCTAssertEqual(draft.maxBytes, report.maxBytes)
        XCTAssertEqual(UInt32(draft.clampedMaxActions), report.maxActions)
        XCTAssertEqual(UInt64(draft.clampedMaxDurationSeconds), report.maxDurationSecs)
    }

    /// Rounding is up, so a budget the stepper cannot represent exactly is
    /// never silently reduced by opening this screen.
    func testASubGigabyteBudgetRoundsUpRatherThanToZero() {
        XCTAssertEqual(AutopilotDraft.gigabytesRoundingUp(0), 0)
        XCTAssertEqual(AutopilotDraft.gigabytesRoundingUp(1), 1)
        XCTAssertEqual(AutopilotDraft.gigabytesRoundingUp(512 * 1024 * 1024), 1)
        XCTAssertEqual(
            AutopilotDraft.gigabytesRoundingUp(UInt64(AutopilotDraft.bytesPerGigabyte)), 1)
        XCTAssertEqual(
            AutopilotDraft.gigabytesRoundingUp(UInt64(AutopilotDraft.bytesPerGigabyte) + 1), 2)
    }

    func testTheGrantIsStatedInFull() throws {
        let draft = AutopilotDraft.from(try loadEnvelopeFixture())
        let arguments = draft.commandArguments

        XCTAssertEqual(arguments[0], "autopilot")
        XCTAssertEqual(arguments[1], "enable")
        XCTAssertTrue(arguments.contains("--json"))

        // Every field, every time. `enable` builds from a revoked envelope, so
        // an omitted flag is not "leave it alone" but "reset it to a default".
        for flag in [
            "--kinds", "--max-actions", "--max-bytes", "--max-duration", "--min-pressure",
        ] {
            XCTAssertTrue(arguments.contains(flag), "\(flag) must always be sent")
        }

        XCTAssertEqual(value(of: "--kinds", in: arguments), "cargo_target_dir,node_modules")
        XCTAssertEqual(value(of: "--max-actions", in: arguments), "2")
        XCTAssertEqual(value(of: "--max-bytes", in: arguments), "2147483648")
        XCTAssertEqual(value(of: "--max-duration", in: arguments), "120")
        XCTAssertEqual(value(of: "--min-pressure", in: arguments), "PRESSURED")
        XCTAssertEqual(
            value(of: "--preauthorize-ask", in: arguments), "node_modules:rebuild_cost_high")
    }

    func testNoPressureThresholdIsSaidRatherThanLeftOut() throws {
        var draft = AutopilotDraft.from(try loadEnvelopeFixture())
        draft.minPressure = nil

        XCTAssertEqual(value(of: "--min-pressure", in: draft.commandArguments), "none")
    }

    /// The property that keeps every one of the envelope's usage errors off
    /// the screen: whatever the steppers are asked for, what is sent is inside
    /// the ceilings the CLI itself reported.
    func testNoDraftCanAskForMoreThanTheCeilings() throws {
        let report = try loadEnvelopeFixture()
        var draft = AutopilotDraft.from(report)
        draft.maxActions = 9_999
        draft.maxGigabytes = 9_999
        draft.maxDurationSeconds = 9_999

        XCTAssertEqual(UInt32(draft.clampedMaxActions), report.ceilings.maxActions)
        XCTAssertLessThanOrEqual(draft.maxBytes, report.ceilings.maxBytes)
        XCTAssertEqual(UInt64(draft.clampedMaxDurationSeconds), report.ceilings.maxDurationSecs)

        let arguments = draft.commandArguments
        XCTAssertEqual(value(of: "--max-actions", in: arguments), "25")
        XCTAssertEqual(value(of: "--max-bytes", in: arguments), "68719476736")
        XCTAssertEqual(value(of: "--max-duration", in: arguments), "900")
    }

    func testNoDraftCanAskForAnEnabledGrantThatDoesNothing() throws {
        var draft = AutopilotDraft.from(try loadEnvelopeFixture())
        draft.maxActions = 0
        draft.maxGigabytes = 0
        draft.maxDurationSeconds = 0

        XCTAssertEqual(draft.clampedMaxActions, AutopilotDraft.minimumActions)
        XCTAssertEqual(draft.clampedMaxGigabytes, AutopilotDraft.minimumGigabytes)
        XCTAssertEqual(draft.clampedMaxDurationSeconds, AutopilotDraft.minimumDurationSeconds)
    }

    /// Unticking a kind withdraws its pre-authorizations too. The alternative
    /// — keeping them — leaves the envelope holding advance consent about
    /// something Autopilot may not touch, which reads as a wider grant than it
    /// is and comes back into force the moment the kind is re-ticked.
    func testUntickingAKindWithdrawsItsAdvanceConsent() throws {
        var draft = AutopilotDraft.from(try loadEnvelopeFixture())
        XCTAssertEqual(draft.effectiveAsks, ["node_modules:rebuild_cost_high"])

        draft.allowedKinds.remove("node_modules")

        XCTAssertEqual(draft.effectiveAsks, [])
        XCTAssertFalse(draft.commandArguments.contains("--preauthorize-ask"))
    }

    func testAdvanceConsentIsSentOncePerPair() throws {
        var draft = AutopilotDraft.from(try loadEnvelopeFixture())
        draft.preauthorizedAsks.insert(
            AutopilotDraft.askToken(kind: "cargo_target_dir", reason: "rebuild_cost_high"))

        let arguments = draft.commandArguments
        XCTAssertEqual(
            arguments.filter { $0 == "--preauthorize-ask" }.count, 2,
            "a pre-authorization is a (kind, reason) pair, not a blanket switch")
        XCTAssertEqual(
            draft.effectiveAsks,
            ["cargo_target_dir:rebuild_cost_high", "node_modules:rebuild_cost_high"])
    }

    func testAKindlessGrantIsNotOffered() throws {
        var draft = AutopilotDraft.from(try loadEnvelopeFixture())
        XCTAssertTrue(draft.isGrantable)

        draft.allowedKinds = []

        // The CLI calls an empty allowlist a usage error rather than a silent
        // no-op, so the button is disabled instead of composing one.
        XCTAssertFalse(draft.isGrantable)
    }

    /// Nothing the form composes may name a path or a secret: `ps` shows an
    /// argument vector to every process on the machine.
    func testTheGrantNamesNoPathAndNoCredential() throws {
        var draft = AutopilotDraft.from(try loadEnvelopeFixture())
        draft.allowedKinds = ["node_modules", "xcode_derived_data"]

        for argument in draft.commandArguments {
            XCTAssertFalse(argument.contains("/"), "\(argument) looks like a path")
            XCTAssertFalse(argument.contains("~"), "\(argument) looks like a path")
            for forbidden in ["--api-key", "--key", "--token", "--password"] {
                XCTAssertNotEqual(argument, forbidden)
            }
        }
    }

    /// The whole point of the report carrying its own refusals: what the form
    /// can ask for is bounded by what the CLI said was possible.
    func testTheFormOffersNothingTheCliRefuses() throws {
        let report = try loadEnvelopeFixture()
        let draft = AutopilotDraft.from(report)

        for kind in draft.allowedKinds {
            XCTAssertTrue(
                report.allowlistableKinds.contains(kind),
                "\(kind) is not one of the kinds the CLI said may be allowlisted")
            XCTAssertFalse(report.neverAllowlistableKinds.contains(kind))
        }

        for ask in draft.effectiveAsks {
            let reason = String(ask.drop(while: { $0 != ":" }).dropFirst())
            XCTAssertTrue(
                report.preauthorizableReasons.contains(reason),
                "\(reason) is not a reason the CLI said may be answered in advance")
            XCTAssertFalse(report.neverPreauthorizableReasons.contains(reason))
        }
    }

    // MARK: - Commands

    func testTheReadAndTheRevocationAreExactlyWhatTheyLook() {
        XCTAssertEqual(AutopilotCommands.show, ["autopilot", "show", "--json"])
        XCTAssertEqual(AutopilotCommands.revoke, ["autopilot", "revoke", "--json"])
    }

    func testEnableIsTheDraftAndNothingElse() throws {
        let draft = AutopilotDraft.from(try loadEnvelopeFixture())

        XCTAssertEqual(AutopilotCommands.enable(draft), draft.commandArguments)
    }

    /// The only `autopilot run` this screen knows about cannot delete
    /// anything, and it is text rather than a button.
    func testTheOfferedRunIsADryRun() {
        XCTAssertTrue(AutopilotCommands.dryRunPreview.contains("--dry-run"))
        XCTAssertEqual(AutopilotCommands.dryRunPreviewText, "glomeris autopilot run --dry-run")

        for destructive in ["execute", "free", "clean", "emergency"] {
            XCTAssertFalse(AutopilotCommands.dryRunPreview.contains(destructive))
        }
    }

    // MARK: - Status wording

    func testOffAndOnAreNotTheSameSentenceWithAWordSwapped() throws {
        let off = AutopilotStatusViewModel.make(try revokedReport())
        let on = AutopilotStatusViewModel.make(try enabledReport())

        XCTAssertNotEqual(off.title, on.title)
        XCTAssertNotEqual(off.detail, on.detail)
        XCTAssertNotEqual(off.symbolName, on.symbolName)
        XCTAssertNotEqual(off.tone, on.tone)
    }

    /// Off is not an achievement and not a fault; on is worth noticing and is
    /// also the feature working as designed. Red on a user's own grant would
    /// teach them that Glomeris is broken whenever they use it.
    func testNeitherStateIsMiscoloured() throws {
        XCTAssertEqual(AutopilotStatusViewModel.make(try revokedReport()).tone, .neutral)
        XCTAssertEqual(AutopilotStatusViewModel.make(try enabledReport()).tone, .caution)
    }

    // MARK: - Enable wording

    func testAGrantThatTookEffectIsReportedAsSuccess() throws {
        let message = AutopilotEnableWording.message(.envelope(try enabledReport()))

        XCTAssertEqual(message.kind, .success)
    }

    /// The readback disagreeing with the request is the one case where a
    /// success would be dangerous: the user would believe they had granted
    /// something that is not in force, or the reverse.
    func testAGrantThatCameBackOffIsNotReportedAsSuccess() throws {
        let message = AutopilotEnableWording.message(.envelope(try revokedReport()))

        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("autopilot show"))
    }

    func testARefusedGrantSaysNothingWasGranted() {
        let message = AutopilotEnableWording.message(.failed("disk full"))

        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("Nothing was granted"))
        XCTAssertTrue(message.title.contains("disk full"))
    }

    /// Exit 0 means `save_envelope` returned Ok, so the grant IS in force and
    /// only the readback failed. Reporting that as "nothing was granted" would
    /// be exactly backwards, and would leave standing deletion authority in
    /// place while telling the user it is not.
    func testAnUnreadableSuccessDoesNotClaimNothingHappened() {
        let message = AutopilotEnableWording.message(.malformedOutput)

        XCTAssertEqual(message.kind, .failure)
        XCTAssertFalse(message.title.lowercased().contains("nothing was granted"))
        XCTAssertTrue(message.title.contains("saved"))
        XCTAssertTrue(message.title.contains("revoke"))
    }

    func testAVersionSkewOnEnableBlamesTheInstall() {
        let message = AutopilotEnableWording.message(.usageError("unrecognized argument '--json'"))

        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("versions"))
        XCTAssertTrue(message.title.contains("nothing was granted"))
    }

    // MARK: - Revoke wording

    func testARevocationThatTookEffectSaysSoAndKeepsTheLimits() throws {
        let message = AutopilotRevokeWording.message(.envelope(try revokedReport()))

        XCTAssertEqual(message.kind, .success)
        XCTAssertTrue(message.title.contains("off"))
        XCTAssertTrue(message.title.contains("limits"))
    }

    /// Every way a revocation can fail must say that the authorization may
    /// still be in force. This is the one sentence on the screen whose absence
    /// would be actively unsafe, so it is asserted over all of them at once
    /// rather than case by case.
    func testEveryFailedRevocationWarnsThatItIsStillInForce() throws {
        let failures: [AutopilotEnvelopeOutcome] = [
            .envelope(try enabledReport()),
            .usageError("unrecognized argument '--json'"),
            .failed("disk full"),
        ]

        for outcome in failures {
            let message = AutopilotRevokeWording.message(outcome)
            XCTAssertEqual(message.kind, .failure, "\(outcome) must not read as a success")
            XCTAssertTrue(
                message.title.lowercased().contains("in force")
                    || message.title.contains("ON"),
                "\(outcome) must say the authorization may still be in force: \(message.title)")
            XCTAssertTrue(
                message.title.contains("glomeris autopilot revoke"),
                "\(outcome) must say how to revoke it another way: \(message.title)")
        }
    }

    /// Exit 0 here means the revocation was written. It must not warn that the
    /// grant may still be in force — that would train the user to distrust a
    /// revocation that worked, and the next one might not be tested.
    func testAnUnreadableRevocationConfirmsItHappened() {
        let message = AutopilotRevokeWording.message(.malformedOutput)

        XCTAssertTrue(message.title.contains("was revoked"))
        XCTAssertTrue(message.title.contains("out of date"))
    }

    // MARK: - Expiry

    /// The envelope has no expiry field, so the screen says so. Asserted
    /// because the failure mode is silence: a consent screen that simply never
    /// mentions expiry leaves the reader to assume the grant lapses.
    func testTheScreenAdmitsTheGrantDoesNotExpire() {
        XCTAssertTrue(AutopilotWording.expiry.contains("does not expire"))
        XCTAssertTrue(AutopilotWording.expiry.contains("until you revoke it"))
        XCTAssertTrue(AutopilotWording.limitsSurviveRevocation.contains("Revoking keeps"))
        XCTAssertTrue(AutopilotWording.replacesCurrentGrant.contains("replaces the whole"))
    }

    func testTheCeilingsAreQuotedFromTheReport() throws {
        let draft = AutopilotDraft.from(try loadEnvelopeFixture())
        let note = AutopilotWording.ceilingsNote(draft)

        XCTAssertTrue(note.contains("25"))
        XCTAssertTrue(note.contains("64"))
        XCTAssertTrue(note.contains("900"))
    }

    // MARK: - Spoken labels

    /// HORO-1470. The status card shows its title and its detail as two
    /// `Text` views, so the only thing separating them for a listener is
    /// punctuation. Pinned as "the title ends in a sentence mark and the
    /// detail follows it", not as a whole-string literal: the wording of both
    /// halves is `AutopilotStatusViewModel.make`'s business and re-wording it
    /// must not have to come through here.
    func testTheStatusCardIsSpokenAsSentencesAndNotAsAComma() throws {
        for report in [try revokedReport(), try enabledReport()] {
            let status = AutopilotStatusViewModel.make(report)
            let spoken = status.accessibilityLabel

            XCTAssertEqual(
                spoken, "\(status.title). \(status.detail)",
                "the card's two Text views must be joined by a sentence boundary"
            )
            XCTAssertFalse(
                spoken.contains("\(status.title), "),
                "\(spoken) splices the title onto the detail with a comma"
            )
        }
    }

    /// The fourteen rows behind the "things you cannot answer in advance"
    /// disclosure, same shape and same defect as the status card.
    ///
    /// Driven from the fixture's own reason list rather than from a chosen
    /// example, so a reason added to the CLI is covered here the moment the
    /// fixture is regenerated.
    func testEveryNeverPreauthorizableRowIsSpokenAsSentences() throws {
        let reasons = try enabledReport().neverPreauthorizableReasons
        XCTAssertEqual(
            reasons.count, 14,
            "the disclosure's own label counts these; a change belongs in both places"
        )

        for reason in reasons {
            let term = GlomerisVocabulary.reason(reason)
            let spoken = AutopilotTermLabel.spoken(term)

            XCTAssertEqual(spoken, "\(term.title). \(term.explanation)", "reason \(reason)")
            // The axis prefix is deliberately absent: these rows render the
            // title as prose, not as a chip, so nothing on screen says it.
            XCTAssertFalse(spoken.hasPrefix("\(term.axis):"), "reason \(reason)")
        }
    }

    /// Anti-vacuity for both assertions above.
    ///
    /// The two tests would pass against a composer that did nothing at all if
    /// every `title` happened to end in a period already — none do, but that
    /// is a property of today's wording rather than of the code under test.
    /// This reproduces SwiftUI's `children: .combine` join on the same inputs
    /// and requires the assertion to reject it, so a regression to the
    /// pre-fix behaviour cannot pass.
    func testTheComposerIsWhatMakesTheDifferenceNotTheWording() throws {
        let status = AutopilotStatusViewModel.make(try revokedReport())

        // What SwiftUI produced before this fix, and what the live
        // accessibility tree read out: HORO-1470's reported string.
        let combined = "\(status.title), \(status.detail)"
        XCTAssertEqual(combined, "Autopilot is off, No run can delete anything. Glomeris still "
            + "detects and explains; it just will not act without being asked each time.")
        XCTAssertNotEqual(
            status.accessibilityLabel, combined,
            "the fix must not reproduce SwiftUI's comma join"
        )

        // And the titles really do lack punctuation of their own, so the
        // period in the composed label is something `SpokenLabel` added
        // rather than something the wording supplied.
        XCTAssertFalse(status.title.hasSuffix("."))
        for reason in try enabledReport().neverPreauthorizableReasons {
            let term = GlomerisVocabulary.reason(reason)
            XCTAssertFalse(term.title.hasSuffix("."), "reason \(reason)")
            XCTAssertNotEqual(
                AutopilotTermLabel.spoken(term), "\(term.title), \(term.explanation)",
                "reason \(reason)"
            )
        }
    }

    // MARK: - Helpers

    private func value(of flag: String, in arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else {
            return nil
        }
        return arguments[index + 1]
    }

    private func enabledReport() throws -> AutopilotEnvelopeDto {
        try loadEnvelopeFixture()
    }

    /// The same report with the grant withdrawn, built field by field from
    /// the fixture rather than from a second literal: the limits surviving a
    /// revocation is the documented behaviour, and a hand-written "revoked"
    /// report would be free to disagree with it.
    private func revokedReport() throws -> AutopilotEnvelopeDto {
        let enabled = try enabledReport()
        return AutopilotEnvelopeDto(
            enabled: false,
            allowedKinds: [],
            askPreauthorizations: [],
            maxActions: enabled.maxActions,
            maxBytes: enabled.maxBytes,
            maxBytesHuman: enabled.maxBytesHuman,
            maxDurationSecs: enabled.maxDurationSecs,
            minPressure: nil,
            ceilings: enabled.ceilings,
            allowlistableKinds: enabled.allowlistableKinds,
            neverAllowlistableKinds: enabled.neverAllowlistableKinds,
            preauthorizableReasons: enabled.preauthorizableReasons,
            neverPreauthorizableReasons: enabled.neverPreauthorizableReasons,
            pressureStates: enabled.pressureStates,
            neverExecutableLabels: enabled.neverExecutableLabels,
            aiAuthority: enabled.aiAuthority,
            storedAt: enabled.storedAt
        )
    }
}
