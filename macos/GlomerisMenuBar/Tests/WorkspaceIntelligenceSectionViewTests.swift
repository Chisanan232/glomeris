//
//  WorkspaceIntelligenceSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1550 AC 2 and AC 4.
//
//  Every claim this card makes is a claim about an *absence* — no baseline yet,
//  no provider configured, no answer from a service, no such command in the
//  installed CLI — and an absence is exactly what looking at the screen cannot
//  check. So the four things worth pinning are pinned here as values:
//
//    1. `WorkspaceContextInterpretation` telling "the installed CLI has no such
//       command" apart from "nothing is configured" and from "the read broke".
//       All three are exit-2-adjacent silences and conflating them is AC 4's
//       whole failure mode.
//    2. `patternSentence` rendering a baseline still short of its minimum as a
//       measurement in progress rather than as "workflow: unknown".
//    3. `providerStateSentence` keeping not-set-up, ready and refused as three
//       states read off three fields rather than one status word.
//    4. The two always-visible sentences — history does not outrank today,
//       silence is not an answer — surviving every failure state, which is
//       checkable only from the source, because it is a fact about where they
//       sit relative to the `switch`.
//
//  The source-reading assertions at the end follow
//  `GlomerisPopoverViewTests`' technique for the same reason it does: what is
//  being asserted is *where* something is, and no value can carry that.
//

import XCTest

final class WorkspaceIntelligenceSectionViewTests: XCTestCase {
    /// Comment-stripped source of the card. Read in `setUpWithError` rather than
    /// lazily, so a renamed or moved file fails loudly instead of letting every
    /// source assertion below pass over an empty string.
    private var code: String = ""

    override func setUpWithError() throws {
        code = Self.strippedOfComments(try Self.readSource("WorkspaceIntelligenceSectionView.swift"))
        XCTAssertFalse(code.isEmpty)
    }

    // MARK: - The four outcomes of one read

    /// A decodable report is a report, and that is decided before the exit code
    /// is looked at.
    func testDecodableStdoutIsAReportWhateverTheExitCode() throws {
        let data = try Self.loadFixture("external_context_preview_report_disabled.json")

        for exitCode: Int32 in [0, 1, 2] {
            let outcome = WorkspaceContextInterpretation.interpret(
                ExternalContextPreviewReportDto.self,
                exitCode: exitCode,
                stdout: data,
                stderr: Data("glomeris: something went wrong".utf8)
            )
            guard case .report = outcome else {
                return XCTFail("exit \(exitCode) with a printed report must still be a report")
            }
        }
    }

    /// The fourth outcome, and the one this type exists for. `external-context`
    /// does not exist in an older `glomeris`, and the top-level arm in
    /// `src/main.rs` says so on stderr with exit 2.
    func testAnInstalledCliWithNoSuchCommandIsItsOwnOutcome() {
        let stderr = "glomeris: unknown command 'external-context'"
        let outcome = WorkspaceContextInterpretation.interpret(
            ExternalContextPreviewReportDto.self,
            exitCode: 2,
            stdout: Data(),
            stderr: Data(stderr.utf8)
        )

        XCTAssertEqual(outcome, .cliTooOld(stderr), "the line is carried verbatim, not summarised")
    }

    /// The discrimination that makes the case above worth having: a rejected
    /// *flag* is this app sending something wrong, not an old CLI, and it must
    /// not be reported to the user as a version skew they cannot act on.
    ///
    /// This is the positive control for `unknownCommandMarker`. Both inputs are
    /// exit 2 with a `glomeris:`-prefixed stderr, so nothing but the marker
    /// separates them.
    func testARejectedFlagIsThisAppsOwnBugAndNotAnOldCli() {
        let stderr = "glomeris external-context: takes no arguments (got '--nope')"
        let outcome = WorkspaceContextInterpretation.interpret(
            ExternalContextPreviewReportDto.self,
            exitCode: 2,
            stdout: Data(),
            stderr: Data(stderr.utf8)
        )

        XCTAssertEqual(outcome, .failed(stderr))
        XCTAssertNotEqual(outcome, .cliTooOld(stderr))
    }

    /// Exit 0 and something unreadable: the CLI believes it answered, so this is
    /// a version mismatch in the report shape rather than a failed read.
    func testSuccessfulExitWithUnreadableOutputIsMalformedAndNotAFailure() {
        let outcome = WorkspaceContextInterpretation.interpret(
            WorkflowProfileReportDto.self,
            exitCode: 0,
            stdout: Data("not json".utf8),
            stderr: Data()
        )

        XCTAssertEqual(outcome, .malformedOutput)
    }

    /// A non-zero exit that is neither of the above reports what the CLI said,
    /// and says something itself when the CLI said nothing — an empty failure
    /// message is the one shape a user can do nothing at all with.
    func testAFailedReadCarriesWhatWasSaidOrSaysThatNothingWas() {
        XCTAssertEqual(
            WorkspaceContextInterpretation.interpret(
                WorkflowProfileReportDto.self,
                exitCode: 1,
                stdout: Data(),
                stderr: Data("glomeris workflow-profile: could not read the store\n".utf8)
            ),
            .failed("glomeris workflow-profile: could not read the store")
        )

        let silent = WorkspaceContextInterpretation.interpret(
            WorkflowProfileReportDto.self,
            exitCode: 9,
            stdout: Data(),
            stderr: Data("   \n".utf8)
        )
        guard case .failed(let message) = silent else { return XCTFail("expected a failure") }
        XCTAssertTrue(message.contains("9"), message)
        XCTAssertFalse(message.isEmpty)
    }

    /// Non-vacuity for all four: no two of them are the same value, which is the
    /// property every sentence below relies on.
    func testTheFourOutcomesAreFourDistinctValues() {
        let outcomes: [WorkspaceContextOutcome<WorkflowProfileReportDto>] = [
            .report(Self.profile()),
            .cliTooOld("x"),
            .malformedOutput,
            .failed("x"),
        ]
        for (index, left) in outcomes.enumerated() {
            for (otherIndex, right) in outcomes.enumerated() where otherIndex != index {
                XCTAssertNotEqual(left, right)
            }
        }
    }

    /// Both argument vectors read and neither writes. `workflow-profile record`
    /// is what writes a baseline, and a panel that recorded an observation every
    /// time it was opened would be measuring itself.
    func testNeitherReadAsksTheCliToWriteAnythingOrToScopeARoot() {
        let both = WorkspaceContextCommands.workflowProfile + WorkspaceContextCommands.externalContext
        XCTAssertEqual(WorkspaceContextCommands.workflowProfile, ["workflow-profile", "show", "--json"])
        XCTAssertEqual(WorkspaceContextCommands.externalContext, ["external-context", "--json"])
        XCTAssertFalse(both.contains("record"), "this surface must never write a baseline")
        XCTAssertFalse(both.contains("--project-root"), "neither subcommand takes roots")
    }

    // MARK: - AC 2: a pending answer is not an empty one

    /// The shipped fixture is `mode: "unknown"`, `confidence: "insufficient"`,
    /// one more look needed — a measurement that was taken and whose answer is
    /// pending. The word "unknown" must not reach the screen from it.
    func testABaselineShortOfItsMinimumReadsAsPendingAndNeverAsUnknown() throws {
        let report = try Self.decodeFixture(
            "workflow_profile_report.json",
            as: WorkflowProfileReportDto.self
        )
        XCTAssertEqual(report.confidence, "insufficient", "sanity: this is the pending fixture")
        XCTAssertEqual(report.mode, "unknown", "sanity: and its mode is the one that must not show")

        let sentence = WorkspaceIntelligencePresentation.patternSentence(report)
        XCTAssertFalse(
            sentence.lowercased().contains("unknown"),
            "a pending measurement must not be reported with the word its mode happens to carry"
        )
        XCTAssertTrue(sentence.contains("1 look"), "singular, and it names how many more: \(sentence)")
        XCTAssertTrue(
            sentence.contains("is not a finding that this machine has none"),
            "the absence of a named pattern must be stated as not-a-finding"
        )
    }

    /// And once there is enough, the shape is what shows.
    func testAnObservedBaselineNamesTheShapeItRecorded() {
        let sentence = WorkspaceIntelligencePresentation.patternSentence(
            Self.profile(confidence: "observed", mode: "parallel_multi_worktree")
        )
        XCTAssertTrue(sentence.contains("several working trees at once"), sentence)
        XCTAssertFalse(sentence.contains("has not named a pattern"), sentence)
    }

    /// A confidence this app has never heard of states nothing rather than
    /// falling through to a shape — the default arm of a vocabulary switch is
    /// where a silent wrong answer would come from.
    func testAnUnrecognisedConfidenceStatesNothing() {
        let sentence = WorkspaceIntelligencePresentation.patternSentence(
            Self.profile(confidence: "probably", mode: "serial_single_checkout")
        )
        XCTAssertTrue(sentence.contains("does not recognise"), sentence)
        XCTAssertTrue(sentence.contains("probably"), "the tag is shown as it arrived")
        XCTAssertFalse(sentence.contains("one checkout at a time"), "no shape may be claimed")
    }

    /// §12 forbids describing a *user* as advanced, beginner, good, bad or
    /// expert, and the way this surface holds that line is by having no
    /// evaluative vocabulary at all: every mode is worded as a property of the
    /// working trees.
    func testNoModeIsWordedAsAJudgementOfWhoeverUsesTheMachine() {
        let forbidden = [
            "advanced", "beginner", "expert", "novice", "good", "bad", "power user",
            "sophisticated", "heavy", "casual", "professional",
        ]
        let modes = [
            "serial_single_checkout", "serial_multi_branch", "parallel_multi_worktree",
            "mixed", "unknown", "something_else",
        ]
        for mode in modes {
            let shape = WorkspaceIntelligencePresentation.habitShape(mode).lowercased()
            XCTAssertFalse(shape.isEmpty)
            for word in forbidden {
                XCTAssertFalse(shape.contains(word), "\(mode) → \(shape)")
            }
        }
        // Non-vacuity: the five known tags really are five different sentences,
        // so the check above ran over distinct wordings rather than one.
        XCTAssertEqual(Set(modes.map(WorkspaceIntelligencePresentation.habitShape)).count, modes.count)
    }

    /// The three states of a baseline stay three things. `never_collected` and
    /// `unreadable` have different next steps — write one, versus find out why
    /// the store would not open — and a shared "no history" would send somebody
    /// to re-run a recorder that is running fine.
    func testTheThreeBaselineStatesAreThreeDifferentThings() throws {
        let collected = try Self.decodeFixture(
            "workflow_profile_report.json",
            as: WorkflowProfileReportDto.self
        )
        XCTAssertNil(
            WorkspaceIntelligencePresentation.habitStateMessage(collected),
            "a collected baseline has rows to draw, so there is no state message"
        )

        let never = try XCTUnwrap(
            WorkspaceIntelligencePresentation.habitStateMessage(
                try Self.decodeFixture(
                    "workflow_profile_report_never_collected.json",
                    as: WorkflowProfileReportDto.self
                )
            )
        )
        // Not `empty`'s checkmark: no look has happened, so a clean bill of
        // health is not this message's to give.
        XCTAssertEqual(never.kind, .empty)
        XCTAssertEqual(never.symbolName, "magnifyingglass")
        XCTAssertTrue(
            try XCTUnwrap(never.detail).contains("not a finding about how this machine is used"),
            "a baseline nobody wrote must not read as a fact about the machine"
        )

        let unreadable = try XCTUnwrap(
            WorkspaceIntelligencePresentation.habitStateMessage(
                Self.profile(state: "unreadable", unreadableReason: "permission_denied")
            )
        )
        XCTAssertEqual(unreadable.kind, .failure)
        XCTAssertNotEqual(unreadable, never)

        let unrecognised = try XCTUnwrap(
            WorkspaceIntelligencePresentation.habitStateMessage(Self.profile(state: "dunno"))
        )
        XCTAssertTrue(unrecognised.title.contains("does not recognise"), unrecognised.title)
        XCTAssertTrue(unrecognised.title.contains("dunno"))
    }

    /// An unreadable store is not evidence in either direction, and it borrows
    /// the AI card's wording for the reason it failed so that one `ProbeReason`
    /// tag has one wording app-wide.
    func testAnUnreadableBaselineIsNotEvidenceEitherWay() {
        let reasoned = WorkspaceIntelligencePresentation.unreadableHabitMessage("tool_absent")
        XCTAssertTrue(
            reasoned.title.contains(AiPlanSectionView.unavailableReasonPhrase("tool_absent")),
            reasoned.title
        )
        XCTAssertTrue(reasoned.title.contains("in either direction"), reasoned.title)

        // And with no reason given it still refuses to conclude anything.
        let silent = WorkspaceIntelligencePresentation.unreadableHabitMessage(nil)
        XCTAssertTrue(silent.title.contains("did not say why"), silent.title)
        XCTAssertTrue(silent.title.contains("in either direction"), silent.title)
    }

    /// The counts the pattern was read off, so an "unknown" can be argued with —
    /// and stated as counts, with no adjective anywhere.
    func testTheSupportingCountsAreReportedAsCounts() throws {
        let report = try Self.decodeFixture(
            "workflow_profile_report.json",
            as: WorkflowProfileReportDto.self
        )
        let lines = WorkspaceIntelligencePresentation.supportLines(report.support)
        XCTAssertEqual(lines.count, 5)
        XCTAssertTrue(
            lines.contains(where: { $0.contains("Most working trees seen at once: 4") }),
            "\(lines)"
        )

        let observations = WorkspaceIntelligencePresentation.observationsSentence(report)
        XCTAssertTrue(observations.contains("2 looks"), observations)
        XCTAssertTrue(observations.contains("1 repository"), observations)

        XCTAssertTrue(
            WorkspaceIntelligencePresentation.retentionSentence(report).contains("90 days"),
            "retention is what bounds this store, and it belongs on screen with it"
        )
        XCTAssertTrue(
            WorkspaceIntelligencePresentation.retentionSentence(report).contains("hour"),
            "3600 seconds is an hour to a reader"
        )
    }

    // MARK: - AC 4: three silences, three states, none of them an answer

    /// Not set up, set up and usable, set up and refused. Three sentences read
    /// off `configured` and `ready`, which is why those arrive as two fields.
    func testAProviderNobodySetUpAndOneWhoseCredentialWasRefusedAreDifferentStates() {
        let notConfigured = WorkspaceIntelligencePresentation.providerStateSentence(
            Self.provider(configured: false, ready: false)
        )
        let ready = WorkspaceIntelligencePresentation.providerStateSentence(
            Self.provider(configured: true, ready: true)
        )
        let refused = WorkspaceIntelligencePresentation.providerStateSentence(
            Self.provider(configured: true, ready: false, refusal: "the service answered 401")
        )

        XCTAssertEqual(Set([notConfigured, ready, refused]).count, 3, "three states, three sentences")
        XCTAssertTrue(notConfigured.contains("not set up"), notConfigured)
        XCTAssertTrue(ready.contains("credential variable is present"), ready)
        XCTAssertTrue(refused.contains("not usable"), refused)

        // None of the three may read as an answer about the remote world.
        for sentence in [notConfigured, ready, refused] {
            XCTAssertFalse(sentence.lowercased().contains("no pull request"), sentence)
            XCTAssertFalse(sentence.lowercased().contains("no task"), sentence)
        }
    }

    /// The sentence AC 4 is, stated once above the providers so it covers all
    /// three silences whatever an individual row says.
    func testSilenceIsStatedNotToBeAnAnswer() {
        let sentence = WorkspaceIntelligencePresentation.remoteSilenceIsNotAnAnswerSentence
        for situation in ["Off", "not set up", "unreachable"] {
            XCTAssertTrue(sentence.contains(situation), sentence)
        }
        XCTAssertTrue(
            sentence.contains("None of them means that no pull request and no task exist"),
            sentence
        )
    }

    /// And the equivalent for history: §12's rule, in the place a reader would
    /// otherwise have to infer it.
    func testHistoryIsStatedNotToOutrankToday() {
        let sentence = WorkspaceIntelligencePresentation.habitIsNotTodaySentence
        XCTAssertTrue(sentence.contains("not a reading of what is happening now"), sentence)
        XCTAssertTrue(
            sentence.contains("today's measurement is the one that counts"),
            "the tie-break has to be named, not implied"
        )
    }

    /// A configuration file that would not parse. Everything below it is the
    /// empty default rather than anything read from the file, so "unknown rather
    /// than empty" is the claim, and it is the one Rust's own prose makes first.
    func testAnUnparseableConfigMakesTheSetupUnknownRatherThanEmpty() throws {
        let broken = Self.externalContext(configError: "line 3: expected a table")
        let message = try XCTUnwrap(WorkspaceIntelligencePresentation.configErrorMessage(broken))
        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("line 3: expected a table"), message.title)
        XCTAssertTrue(message.title.contains("unknown rather than empty"), message.title)

        XCTAssertNil(
            WorkspaceIntelligencePresentation.configErrorMessage(Self.externalContext()),
            "a config that parsed has nothing to report here"
        )
    }

    /// Nothing configured is stated as nothing asked and nothing sent, which is
    /// the only state in which a bare "no result" would be true — and even here
    /// it is phrased as Glomeris not asking rather than as a finding.
    func testNothingConfiguredIsPhrasedAsNothingAskedAndNothingSent() throws {
        let disabled = try Self.decodeFixture(
            "external_context_preview_report_disabled.json",
            as: ExternalContextPreviewReportDto.self
        )
        let sentence = WorkspaceIntelligencePresentation.remoteEnabledSentence(disabled)
        XCTAssertTrue(sentence.contains("asked GitHub and Jira nothing"), sentence)
        XCTAssertTrue(sentence.contains("sent them nothing"), sentence)

        let enabled = try Self.decodeFixture(
            "external_context_preview_report.json",
            as: ExternalContextPreviewReportDto.self
        )
        XCTAssertNotEqual(
            WorkspaceIntelligencePresentation.remoteEnabledSentence(enabled),
            sentence
        )
    }

    /// The most privacy-relevant lines on the screen: the scope, the endpoint,
    /// and the *name* of a credential variable. Never a value, and the line says
    /// so, so a reader is not left wondering what "credential" means here.
    func testAProviderNamesItsCredentialVariableAndNeverItsValue() {
        let lines = WorkspaceIntelligencePresentation.providerDetailLines(
            Self.provider(
                configured: true,
                ready: true,
                credentialEnv: "GLOMERIS_GITHUB_TOKEN",
                endpoint: "https://api.github.com",
                repositoryHost: "github.com"
            )
        )
        XCTAssertEqual(lines.count, 3)
        XCTAssertTrue(
            lines.contains(where: { $0.contains("whose remote is on github.com") }),
            "\(lines)"
        )
        XCTAssertTrue(
            lines.contains("Credential read from GLOMERIS_GITHUB_TOKEN — never its value"),
            "\(lines)"
        )

        // Jira is correlated by an explicit issue key and so has no host to
        // scope by. Reporting one would advertise a rule that does not exist.
        let jira = WorkspaceIntelligencePresentation.providerDetailLines(
            Self.provider(configured: true, ready: false, repositoryHost: nil)
        )
        XCTAssertFalse(jira.contains(where: { $0.contains("remote is on") }), "\(jira)")
    }

    /// The egress list is worth printing only because of the "and nothing else",
    /// so that is in the heading rather than in prose somewhere above it.
    func testTheEgressHeadingStatesTheAndNothingElse() {
        XCTAssertEqual(
            WorkspaceIntelligencePresentation.egressHeadingText(1),
            "1 field may reach a model, and nothing else"
        )
        XCTAssertEqual(
            WorkspaceIntelligencePresentation.egressHeadingText(6),
            "6 fields may reach a model, and nothing else"
        )
    }

    /// Every field in the shipped fixture renders with its whole value set, and
    /// a numeric one says what it is instead of looking like an empty list
    /// nobody filled in.
    func testEveryEgressFieldRendersItsCompleteVocabularyOrSaysWhyItHasNone() throws {
        let report = try Self.decodeFixture(
            "external_context_preview_report.json",
            as: ExternalContextPreviewReportDto.self
        )
        XCTAssertFalse(report.egressFields.isEmpty, "sanity: this is the configured fixture")

        for field in report.egressFields {
            let sentence = WorkspaceIntelligencePresentation.egressFieldSentence(field)
            XCTAssertTrue(sentence.hasPrefix(field.field), sentence)
            XCTAssertFalse(sentence.contains("does not recognise"), sentence)
            if field.vocabulary.isEmpty {
                XCTAssertTrue(sentence.contains("whole number of days"), sentence)
            } else {
                for value in field.vocabulary {
                    XCTAssertTrue(sentence.contains(value), "\(value) missing from: \(sentence)")
                }
            }
        }
    }

    /// The negative list is the answer to "did you send my branch name", and no
    /// list of what travels answers that. It is rendered from Rust's own words.
    func testTheNeverSentListIsRenderedAndNotSummarised() throws {
        let report = try Self.decodeFixture(
            "external_context_preview_report_disabled.json",
            as: ExternalContextPreviewReportDto.self
        )
        XCTAssertFalse(report.neverSent.isEmpty)
        XCTAssertTrue(
            WorkspaceIntelligencePresentation.neverSentHeading.contains("to a provider or to a model"),
            "both egress directions, because they are two different questions"
        )
    }

    /// An older CLI's two sentences: what happened, and the conclusion a reader
    /// must not draw. Both halves are required — §13's rule is that missing
    /// evidence must never become negative evidence, and the first half alone
    /// leaves the reader to supply the second.
    func testAnOlderCliIsExplainedAsAMissingCommandAndNotAsAnEmptyAnswer() {
        let habit = WorkspaceIntelligencePresentation.habitCliTooOldMessage(
            "glomeris: unknown command 'workflow-profile'"
        )
        XCTAssertTrue(habit.title.contains("no `workflow-profile` command"), habit.title)
        XCTAssertTrue(
            habit.title.contains("not a machine with no recorded habit"),
            habit.title
        )

        let remote = WorkspaceIntelligencePresentation.remoteCliTooOldMessage(
            "glomeris: unknown command 'external-context'"
        )
        XCTAssertTrue(remote.title.contains("not a setup with nothing configured"), remote.title)
        XCTAssertTrue(
            remote.title.contains("not evidence that no pull request or task exists"),
            "AC 4 has to hold in the state where the question was never put"
        )
        XCTAssertNotEqual(habit, remote, "the two commands' consequences are different consequences")
    }

    // MARK: - Small renderings

    func testACountCarriesItsNoun() {
        XCTAssertEqual(WorkspaceIntelligencePresentation.count(0, "look", "looks"), "0 looks")
        XCTAssertEqual(WorkspaceIntelligencePresentation.count(1, "look", "looks"), "1 look")
        XCTAssertEqual(WorkspaceIntelligencePresentation.count(2, "look", "looks"), "2 looks")
    }

    /// The shipped interval is 3600 seconds, and a reader reads "hour".
    func testAnIntervalIsReadAsTheUnitItDividesInto() {
        XCTAssertEqual(WorkspaceIntelligencePresentation.durationPhrase(3600), "hour")
        XCTAssertEqual(WorkspaceIntelligencePresentation.durationPhrase(7200), "2 hours")
        XCTAssertEqual(WorkspaceIntelligencePresentation.durationPhrase(60), "minute")
        XCTAssertEqual(WorkspaceIntelligencePresentation.durationPhrase(300), "5 minutes")
        XCTAssertEqual(WorkspaceIntelligencePresentation.durationPhrase(90), "90 seconds")
        XCTAssertEqual(WorkspaceIntelligencePresentation.durationPhrase(1), "second")
    }

    /// Two providers with different tags render as two different names, and one
    /// this app has never seen is shown as it arrived rather than dropped — it
    /// is a tag, and a row that vanished would understate what is configured.
    func testAnUnrecognisedProviderTagIsShownRatherThanDropped() {
        XCTAssertEqual(
            WorkspaceIntelligencePresentation.providerName("github_pull_requests"),
            "GitHub pull requests"
        )
        XCTAssertEqual(WorkspaceIntelligencePresentation.providerName("jira_issues"), "Jira issues")
        XCTAssertEqual(WorkspaceIntelligencePresentation.providerName("gitlab_mrs"), "gitlab_mrs")
    }

    // MARK: - Where things sit (source assertions)

    /// AC 2 and AC 4's sentences are claims about this card's own authority, so
    /// they must be on screen in the states where a reader is *most* likely to
    /// conclude an absence: loading, and all three failures. That is a fact
    /// about where they are relative to the `switch`, which no value can carry.
    func testTheTwoLoadBearingSentencesAreOutsideTheSwitchThatCanFail() throws {
        for (property, sentence) in [
            ("habitGroup", "habitIsNotTodaySentence"),
            ("remoteGroup", "remoteSilenceIsNotAnAnswerSentence"),
        ] {
            let body = try Self.propertyBody(named: property, in: code)
            let rendered = try XCTUnwrap(
                body.range(of: sentence),
                "\(property) must render \(sentence)"
            )
            let branch = try XCTUnwrap(
                body.range(of: "switch "),
                "\(property) must be the property that branches on the outcome"
            )
            XCTAssertLessThan(
                rendered.lowerBound, branch.lowerBound,
                "\(sentence) must be rendered before the switch, or a failed read drops the one "
                    + "sentence that says an absence is not a finding"
            )
        }
    }

    /// And every one of the four outcomes is actually branched on, in both
    /// groups — a missing arm would not compile, but an arm that fell into
    /// `default` would, and would render a version skew as a plain failure.
    func testBothGroupsHandleAllFourOutcomesExplicitly() throws {
        for property in ["habitGroup", "remoteGroup"] {
            let body = try Self.propertyBody(named: property, in: code)
            for arm in ["case nil:", "case .report(", "case .cliTooOld(", "case .malformedOutput?:",
                        "case .failed("] {
                XCTAssertTrue(body.contains(arm), "\(property) is missing \(arm)")
            }
            XCTAssertFalse(
                body.contains("default:"),
                "\(property) must name every outcome, so a new one fails to compile here"
            )
        }
    }

    /// §17: workspace intelligence explains WHY and does not become a second
    /// control plane. Kept structurally, by giving the surface nothing to act
    /// with — the only controls in the file are disclosure toggles, which reveal
    /// text that is already on this machine.
    func testTheCardHasNothingToActWith() {
        for forbidden in ["Button(", ".disabled(", "onTapGesture", "Toggle(", "TextField("] {
            XCTAssertFalse(code.contains(forbidden), "\(forbidden) would make this a control")
        }
        for forbidden in ["\"execute\"", "\"free\"", "\"clean\"", "\"emergency\"", "\"autopilot\"",
                          "\"record\"", "actionId", "action_id"] {
            XCTAssertFalse(code.contains(forbidden), "\(forbidden) has no business in this card")
        }
        XCTAssertTrue(code.contains("DisclosureGroup"), "sanity: it does have disclosures")
    }

    /// AC 6. Two headings a screen reader can jump between, two composed labels
    /// so a multi-line group is read as one statement rather than as fragments,
    /// and the composition done by the shared helper rather than by string
    /// concatenation that would lose a full stop.
    func testTheCardIsReadableByAScreenReader() {
        XCTAssertTrue(code.contains(".accessibilityAddTraits(.isHeader)"))
        XCTAssertTrue(code.contains("SpokenLabel.compose("))
        XCTAssertEqual(
            code.components(separatedBy: ".accessibilityElement(children: .ignore)").count - 1,
            2,
            "the two multi-line groups that read as one statement each"
        )
    }

    /// AC 7. Its state is its own, and the read happens once per appearance —
    /// no timer, and nothing that would re-run while a user is reading it. The
    /// panel hides the overview rather than removing it on drill-down, so local
    /// state is what survives coming back, and a store handed in from above
    /// would be one more thing to keep alive for no gain.
    func testTheCardOwnsItsOwnStateAndReadsOncePerAppearance() {
        XCTAssertEqual(
            code.components(separatedBy: ".task {").count - 1, 1,
            "one read per appearance"
        )
        XCTAssertFalse(code.contains("Timer"), "nothing here moves while the panel is open")
        XCTAssertFalse(code.contains("@StateObject"), "two local reads need no store")
        XCTAssertTrue(code.contains("@State private var workflow"))
        XCTAssertTrue(code.contains("@State private var external"))
    }

    /// Two slots, not one, for HORO-1297's reason: an older CLI answers one of
    /// these and not the other, and a shared error slot lets whichever read
    /// finished last erase the other's failure.
    func testEachReadHasItsOwnSlotSoNeitherCanEraseTheOther() throws {
        let body = try Self.propertyBody(named: "load", in: code, kind: "func load() async")
        XCTAssertTrue(body.contains("workflow = await"))
        XCTAssertTrue(body.contains("external = await"))
    }

    // MARK: - Helpers

    /// Drops whole-line `//` comments, matching `GlomerisPopoverViewTests`. This
    /// file's header discusses `record`, `Button` and every outcome name at
    /// length, so an un-stripped assertion would fail while the code is right.
    private static func strippedOfComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    private static func propertyBody(
        named name: String,
        in source: String,
        kind: String? = nil
    ) throws -> String {
        let signature = try XCTUnwrap(
            source.range(of: kind.map { "private \($0)" } ?? "private var \(name): some View {"),
            "no declaration for \(name) — renamed, or no longer where this test looks"
        )
        let rest = source[signature.upperBound...]
        let end = try XCTUnwrap(rest.range(of: "\n    }\n"), "could not find the end of \(name)")
        return String(rest[..<end.upperBound])
    }

    private static func readSource(_ fileName: String) throws -> String {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: url, encoding: .utf8)
    }

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private static func loadFixture(_ name: String) throws -> Data {
        try Data(contentsOf: fixturesDir.appendingPathComponent(name))
    }

    private static func decodeFixture<T: Decodable>(_ name: String, as type: T.Type) throws -> T {
        try JSONDecoder().decode(T.self, from: loadFixture(name))
    }

    /// A baseline shaped like the shipped one, with the fields under test
    /// overridable. Built rather than fixtured for the states Rust has no golden
    /// file for — an unreadable store, and tags this app must not recognise.
    private static func profile(
        state: String = "collected",
        unreadableReason: String? = nil,
        confidence: String = "insufficient",
        mode: String = "unknown",
        observationCount: UInt32 = 2,
        observationsStillNeeded: UInt32 = 1
    ) -> WorkflowProfileReportDto {
        WorkflowProfileReportDto(
            state: state,
            unreadableReason: unreadableReason,
            storedAt: "2026-09-29T09:00:00Z",
            mode: mode,
            confidence: confidence,
            observationCount: observationCount,
            observationsStillNeeded: observationsStillNeeded,
            minimumIntervalSecs: 3600,
            retentionDays: 90,
            support: WorkflowSupportReportDto(
                spanningDays: 2,
                repositoriesObserved: 1,
                parallelObservations: 2,
                serialObservations: 0,
                mixedObservations: 0,
                singleCheckoutBranchChanges: 0,
                mostWorktreesSeenAtOnce: 4
            ),
            authority: "This is context and never permission."
        )
    }

    private static func provider(
        source: String = "github_pull_requests",
        configured: Bool,
        ready: Bool,
        credentialEnv: String? = nil,
        endpoint: String? = nil,
        repositoryHost: String? = nil,
        refusal: String? = nil
    ) -> ExternalProviderPreviewReportDto {
        ExternalProviderPreviewReportDto(
            source: source,
            configured: configured,
            ready: ready,
            credentialEnv: credentialEnv,
            endpoint: endpoint,
            repositoryHost: repositoryHost,
            refusal: refusal
        )
    }

    private static func externalContext(
        configError: String? = nil
    ) -> ExternalContextPreviewReportDto {
        ExternalContextPreviewReportDto(
            enabled: false,
            configPath: "/Users/dev/.config/glomeris/external.toml",
            configExists: configError != nil,
            configError: configError,
            providers: [],
            egressFields: [],
            neverSent: ["the branch name"]
        )
    }
}
