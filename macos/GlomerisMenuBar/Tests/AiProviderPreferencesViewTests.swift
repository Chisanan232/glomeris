//
//  AiProviderPreferencesViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1309. Covers the four pure types `AiProviderPreferencesView.swift`
//  extracts from its body — the two exit-code interpreters, the result
//  formatter, and the command vectors — plus a set of source-level guards for
//  the properties a unit test cannot reach.
//
//  Why so much of this is source-level: the claims that matter most here are
//  absences. "The privacy preview never sends anything" and "the key is never
//  displayed" are not behaviours to invoke, they are code paths that must not
//  exist, and the only way to assert one from a test is to look. Same mechanism
//  as `AiPlanSectionViewTests`' guards against `.sorted` and `fingerprintToken`,
//  and the same reason `scripts/check-credential-store-uses-keychain.sh` exists
//  as a script rather than as a test: two independent readers of the same file,
//  one of which runs even if this target is never built.
//
//  Nothing in here spawns `glomeris`, and nothing touches the real keychain —
//  `GlomerisLlmSettingsStoreTests` already covers the storage boundary with an
//  in-memory `CredentialStore`.
//

import AppKit
import XCTest

final class AiProviderPreferencesViewTests: XCTestCase {
    // MARK: - Helpers

    private static let repoRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // GlomerisMenuBar
        .deletingLastPathComponent() // macos
        .deletingLastPathComponent() // repo root

    private func fixtureData(_ name: String) throws -> Data {
        try Data(
            contentsOf: Self.repoRoot.appendingPathComponent("tests/fixtures/dto/\(name)"))
    }

    private static func readSource() throws -> String {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/AiProviderPreferencesView.swift")
        return try String(contentsOf: url, encoding: .utf8)
    }

    /// The source with whole-line comments removed. Every "this must not
    /// appear" guard runs against this, because the file's own prose explains
    /// at length why it contains no `.onAppear` and no second `withEnvironment`
    /// — a guard that tripped on its own rationale would force the explanation
    /// to be deleted to stay green.
    private static func readCode() throws -> String {
        try readSource()
            .components(separatedBy: "\n")
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    private func data(_ text: String) -> Data { Data(text.utf8) }

    // MARK: - LlmCheckInterpretation

    /// Exit 0 with a report is the easy half. The interesting half is below.
    func testExitZeroWithAReportIsThatReport() throws {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 0,
            stdout: try fixtureData("llm_check_report_ok.json"),
            stderr: Data()
        )

        guard case .report(let report) = outcome else {
            return XCTFail("expected a report, got \(outcome)")
        }
        XCTAssertEqual(report.outcome, "ok")
        XCTAssertEqual(report.responseExcerpt, "ok")
    }

    /// The single most important branch in this file. `run_llm_check_command`
    /// prints the whole report and *then* exits 1 when the outcome is not
    /// `"ok"`, so a failed check is a completed check whose report is the only
    /// structured account of how it failed. Judging the exit code first and
    /// discarding stdout is exactly the collapse HORO-1299 was about.
    func testExitOneStillCarriesTheFullReport() throws {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 1,
            stdout: try fixtureData("llm_check_report_rejected.json"),
            stderr: Data()
        )

        guard case .report(let report) = outcome else {
            return XCTFail("exit 1 must not throw the diagnosis away, got \(outcome)")
        }
        XCTAssertEqual(report.outcome, "rejected")
        XCTAssertEqual(report.endpointPath, "/v1/chat/completions")
        XCTAssertTrue(try XCTUnwrap(report.error).contains("HTTP 401"))
    }

    /// Exit 2 has no JSON at all — `provider_from_env` refuses before any
    /// report exists — which is why `.misconfigured` carries a sentence rather
    /// than a report. Asserted because the asymmetry with `llm-plan` (which
    /// does print a report on its non-zero path) is a real trap.
    func testExitTwoIsMisconfiguredAndCarriesTheClisOwnSentence() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: data(
                "glomeris llm-check: LLM provider configuration is invalid: base URL must not "
                    + "end in /chat/completions\n")
        )

        XCTAssertEqual(
            outcome,
            .misconfigured(
                "LLM provider configuration is invalid: base URL must not end in "
                    + "/chat/completions"),
            "the prefix is stripped; the CLI's own explanation is not"
        )
    }

    func testExitTwoWithNoStderrStillSaysSomething() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 2, stdout: Data(), stderr: Data())

        guard case .misconfigured(let detail) = outcome else {
            return XCTFail("expected misconfigured, got \(outcome)")
        }
        XCTAssertFalse(detail.isEmpty, "a silent refusal must not render as a blank result")
    }

    /// Also exit 2, but this one is the app's bug rather than the user's: the
    /// installed `glomeris` does not have a flag this app sent. Telling them
    /// apart is what stops a version skew from being reported as "your settings
    /// are wrong", which would send the user editing correct fields.
    func testUnrecognizedArgumentIsToldApartFromABadConfiguration() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 2,
            stdout: Data(),
            stderr: data("glomeris llm-check: unrecognized argument '--json'\n")
        )

        XCTAssertEqual(outcome, .usageError("unrecognized argument '--json'"))
    }

    func testExitZeroWithUndecodableOutputIsVersionSkewNotSuccess() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 0, stdout: data("connection ok\n"), stderr: Data())

        XCTAssertEqual(outcome, .malformedOutput)
    }

    func testAnyOtherExitCodeCarriesStderrVerbatim() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 101, stdout: Data(), stderr: data("thread 'main' panicked\n"))

        XCTAssertEqual(outcome, .failed("thread 'main' panicked"))
    }

    func testAnUnprefixedStderrLineIsShownRatherThanMangled() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 70, stdout: Data(), stderr: data("dyld: library not loaded\n"))

        XCTAssertEqual(
            outcome, .failed("dyld: library not loaded"),
            "a line that does not start with the CLI's prefix must survive intact")
    }

    func testAFailureWithNoStderrNamesTheExitCode() {
        let outcome = LlmCheckInterpretation.interpret(
            exitCode: 137, stdout: Data(), stderr: Data())

        guard case .failed(let detail) = outcome else {
            return XCTFail("expected failed, got \(outcome)")
        }
        XCTAssertTrue(detail.contains("137"), "the only fact available must be reported")
    }

    // MARK: - LlmCheckResultViewModel

    /// AC 4's actual claim: a wrong key, an unreachable host and a URL that is
    /// not an API root must read as three different things. That is a claim
    /// about this mapping, and it is the one HORO-1299 failed.
    func testAWrongKeyAnUnreachableHostAndAWrongPathAllReadDifferently() {
        let tokens = ["rejected", "unreachable", "unusable_response", "misconfigured"]
        let models = tokens.map { token in
            LlmCheckResultViewModel.make(
                .report(
                    LlmCheckReportDto(
                        outcome: token,
                        model: "m",
                        endpointPath: "/v1/chat/completions",
                        error: "detail for \(token)",
                        responseExcerpt: nil
                    )))
        }

        let titles = models.map { $0.term?.title ?? "" }
        XCTAssertEqual(
            Set(titles).count, tokens.count,
            "these four outcomes share wording: \(titles)")
        let explanations = models.map { $0.term?.explanation ?? "" }
        XCTAssertEqual(
            Set(explanations).count, tokens.count,
            "these four outcomes share an explanation, so the user cannot act on them")
    }

    func testASuccessfulCheckReportsTheModelThePathAndWhatCameBack() throws {
        let report = try JSONDecoder().decode(
            LlmCheckReportDto.self, from: try fixtureData("llm_check_report_ok.json"))
        let model = LlmCheckResultViewModel.make(.report(report))

        XCTAssertEqual(model.term?.token, "ok")
        XCTAssertNil(model.detail, "there is nothing wrong to explain")
        XCTAssertEqual(model.model, "gpt-4o-mini")
        XCTAssertEqual(model.endpointPath, "/v1/chat/completions")
        XCTAssertEqual(
            model.responseExcerpt, "ok",
            "proof that something answered *as a chat-completions API*, not merely that "
                + "something answered")
    }

    /// The exit-2 path and a report-carried `"misconfigured"` must be
    /// indistinguishable on screen: the fact that matters is identical either
    /// way — nothing was sent, and a field is wrong. This also keeps the
    /// vocabulary entry for `"misconfigured"` live, which is what makes the
    /// `vocabulary-covers-cli-tokens` CI job honest about it.
    func testAnExitTwoRefusalIsWordedAsMisconfiguredJustLikeAReportWouldBe() {
        let synthesised = LlmCheckResultViewModel.make(.misconfigured("base URL is not an API root"))
        let reported = LlmCheckResultViewModel.make(
            .report(
                LlmCheckReportDto(
                    outcome: "misconfigured",
                    model: "",
                    endpointPath: "",
                    error: "base URL is not an API root",
                    responseExcerpt: nil
                )))

        XCTAssertEqual(synthesised.term, reported.term)
        XCTAssertEqual(synthesised.detail, reported.detail)
        XCTAssertEqual(
            synthesised, reported,
            "the user must not be able to tell which path produced this")
    }

    /// Empty strings are the report's way of saying "not applicable" — the
    /// human renderer has the same rule. A `GlomerisDetailRow` reading
    /// "Model:" with nothing after it is worse than no row.
    func testEmptyModelAndPathBecomeNoRowRatherThanAnEmptyRow() {
        let model = LlmCheckResultViewModel.make(
            .report(
                LlmCheckReportDto(
                    outcome: "misconfigured",
                    model: "",
                    endpointPath: "",
                    error: nil,
                    responseExcerpt: nil
                )))

        XCTAssertNil(model.model)
        XCTAssertNil(model.endpointPath)
    }

    /// A version skew is this app disagreeing with the CLI, not a provider
    /// result. Giving it an `llmCheckOutcome` chip would attribute a local
    /// install problem to the provider — and to the user's key.
    func testVersionSkewGetsNoOutcomeBadge() {
        for outcome in [LlmCheckOutcome.usageError("unrecognized argument '--json'"),
                        .malformedOutput,
                        .failed("dyld: library not loaded")] {
            let model = LlmCheckResultViewModel.make(outcome)
            XCTAssertNil(model.term, "\(outcome) must not be worded as a provider result")
            XCTAssertNotNil(model.detail, "...but it must still say something")
        }
    }

    func testAUsageErrorSaysItIsAVersionSkewAndQuotesTheCli() throws {
        let model = LlmCheckResultViewModel.make(.usageError("unrecognized argument '--json'"))
        let detail = try XCTUnwrap(model.detail)

        XCTAssertTrue(detail.contains("different versions"))
        XCTAssertTrue(
            detail.contains("unrecognized argument '--json'"),
            "the CLI's own words are the only actionable part")
        XCTAssertTrue(
            detail.contains("no check ran"),
            "the user must know nothing was sent and nothing was charged")
    }

    func testAFailureIsCarriedVerbatimWithNothingInvented() {
        let model = LlmCheckResultViewModel.make(.failed("dyld: library not loaded"))

        XCTAssertEqual(model.detail, "dyld: library not loaded")
        XCTAssertNil(model.model)
        XCTAssertNil(model.endpointPath)
        XCTAssertNil(model.responseExcerpt)
    }

    // MARK: - GlomerisLlmAddressWording

    /// HORO-1355. The address help must say three things, because leaving any
    /// one of them out is what let a host root be configured and then blamed on
    /// the key: that a path belongs in the address, what that path usually is,
    /// and what Glomeris appends to whatever is given.
    ///
    /// Asserted on the constant the view renders, so the copy cannot be
    /// weakened without this failing. That the view *shows* it — rather than
    /// declaring it unused — is what the accessibility hint and the caption in
    /// `providerCard` are for, and is verified on the running app.
    func testTheAddressHelpNamesThePathTheUsualValueAndWhatGlomerisAppends() {
        let hint = GlomerisLlmAddressWording.pathHint

        XCTAssertTrue(
            hint.lowercased().contains("path"),
            "the address help must name the thing that goes wrong: \(hint)")
        XCTAssertTrue(
            hint.contains("/v1"),
            "naming a path requirement without naming the usual path leaves the "
                + "user guessing: \(hint)")
        XCTAssertTrue(
            hint.contains("/chat/completions"),
            "the user cannot tell an API root from a completions URL without "
                + "knowing what Glomeris appends: \(hint)")
    }

    /// The sentence must not read as a refusal. A path-less address is accepted
    /// by the CLI on purpose — some providers serve completions at their root —
    /// so wording it as a rule would contradict `validate_base_url` and turn a
    /// working configuration into one the user believes is broken.
    func testTheAddressHelpIsGuidanceRatherThanARule() {
        let hint = GlomerisLlmAddressWording.pathHint.lowercased()

        for absolute in ["must ", "required", "invalid", "not allowed"] {
            XCTAssertFalse(
                hint.contains(absolute),
                "\"\(absolute)\" states a rule this app does not enforce and the "
                    + "CLI does not hold: \(hint)")
        }
        XCTAssertTrue(
            hint.contains("usually"),
            "the usual case has to read as usual, not universal: \(hint)")
    }

    // MARK: - LlmPayloadPreviewInterpretation

    func testThePayloadPreviewDecodesTheReport() throws {
        let outcome = LlmPayloadPreviewInterpretation.interpret(
            exitCode: 0,
            stdout: try fixtureData("llm_payload_report.json"),
            stderr: Data()
        )

        guard case .payload(let report) = outcome else {
            return XCTFail("expected a payload, got \(outcome)")
        }
        XCTAssertEqual(report.resourceAliases.count, 2)
        XCTAssertTrue(report.userPrompt.contains("resource_1"))
        XCTAssertFalse(
            report.userPrompt.contains("/Users/dev/proj/target"),
            "the preview would be disclosing the wrong thing")
    }

    func testThePayloadPreviewTreatsExitZeroWithGarbageAsSkew() {
        let outcome = LlmPayloadPreviewInterpretation.interpret(
            exitCode: 0, stdout: data("system prompt: ...\n"), stderr: Data())

        XCTAssertEqual(outcome, .malformedOutput)
    }

    func testThePayloadPreviewCarriesStderrOnFailure() {
        let outcome = LlmPayloadPreviewInterpretation.interpret(
            exitCode: 1, stdout: Data(), stderr: data("glomeris llm-plan: no project roots\n"))

        XCTAssertEqual(outcome, .failed("glomeris llm-plan: no project roots"))
    }

    func testThePayloadPreviewFailureWithNoStderrNamesTheExitCode() {
        let outcome = LlmPayloadPreviewInterpretation.interpret(
            exitCode: 2, stdout: Data(), stderr: Data())

        guard case .failed(let detail) = outcome else {
            return XCTFail("expected failed, got \(outcome)")
        }
        XCTAssertTrue(detail.contains("2"))
    }

    /// AC 7, asserted on the shape of the type rather than on behaviour.
    /// `--print-payload` returns before a provider is constructed, so there is
    /// no provider failure to report and no credential to be missing. A preview
    /// that could say "no provider configured" would be a preview that had
    /// tried to use one — so that case must be unrepresentable, not merely
    /// unreachable.
    func testThePayloadPreviewHasNoNotConfiguredCaseAtAll() throws {
        let code = try Self.readCode()
        let declaration = try XCTUnwrap(
            code.range(of: "enum LlmPayloadPreviewOutcome: Equatable {")
                .flatMap { start in
                    code[start.upperBound...].range(of: "}").map { String(code[start.upperBound..<$0.lowerBound]) }
                })

        XCTAssertFalse(declaration.contains("notConfigured"))
        XCTAssertFalse(declaration.contains("misconfigured"))
        XCTAssertEqual(
            declaration.components(separatedBy: "case ").count - 1, 3,
            "three cases, and adding a fourth is a claim about egress that needs a look")
    }

    // MARK: - AiProviderCommands

    func testTheConnectionTestAsksForJsonAndNothingElse() {
        XCTAssertEqual(AiProviderCommands.connectionTest, ["llm-check", "--json"])
    }

    /// The flag that makes the preview a preview. Without it this same command
    /// is a live, billed planning request.
    func testThePreviewPassesPrintPayload() {
        let arguments = AiProviderCommands.payloadPreview(projectRootArguments: [])

        XCTAssertEqual(arguments, ["llm-plan", "--print-payload", "--json"])
    }

    /// Scoped identically to what `AiPlanSectionView` sends a live `llm-plan`.
    /// A preview scoped differently from the real call would disclose the wrong
    /// payload, which is worse than disclosing none.
    func testThePreviewIsScopedLikeTheRealPlan() {
        let roots = ["--project-root", "/Users/dev/proj", "--project-root", "/Users/dev/other"]
        let arguments = AiProviderCommands.payloadPreview(projectRootArguments: roots)

        XCTAssertEqual(arguments.suffix(roots.count), roots.suffix(roots.count))
        XCTAssertTrue(arguments.contains("--print-payload"))
    }

    /// AC 2. The CLI refuses these by flag name and `ps` would show them; this
    /// asserts the app never gets that far. Checked over both vectors rather
    /// than by reading the source, because an argument list is exactly the kind
    /// of thing a later "just pass it explicitly" edit would add.
    func testNeitherCommandCarriesACredentialFlagOrValue() {
        let vectors = [
            AiProviderCommands.connectionTest,
            AiProviderCommands.payloadPreview(projectRootArguments: ["--project-root", "/tmp/p"]),
        ]

        for arguments in vectors {
            for argument in arguments {
                for forbidden in ["--api-key", "--key", "--token", "GLOMERIS_LLM_API_KEY", "sk-"] {
                    XCTAssertFalse(
                        argument.contains(forbidden),
                        "\(arguments) carries \(forbidden)")
                }
            }
        }
    }

    // MARK: - GlomerisLlmSettingSourceWording

    /// AC 3's user-visible half. "Why is this working when I never typed it in"
    /// and "why is it using the wrong key" are the same question, and a screen
    /// that words an inherited value the same as a configured one cannot answer
    /// either.
    /// Driven off `allCases` rather than a hand-written list, so adding a source
    /// without wording it fails here instead of rendering as whatever the
    /// `switch` happens to fall through to.
    func testEachSettingSourceReadsDifferently() {
        let sources = GlomerisLlmSettingSource.allCases
        let titles = sources.map { GlomerisLlmSettingSourceWording.title($0) }
        let symbols = sources.map { GlomerisLlmSettingSourceWording.symbolName($0) }

        XCTAssertEqual(Set(titles).count, sources.count, "shared wording: \(titles)")
        XCTAssertEqual(Set(symbols).count, sources.count, "shared symbol: \(symbols)")
    }

    /// HORO-1368. "Not looked at yet" is not a fourth configuration, and it must
    /// not read as one — a row saying "Not set" about a key that is about to
    /// resolve as set is a false statement the user may act on by pasting a key
    /// they already have stored.
    func testTheUnresolvedKeyIsWordedAsAnActivityRatherThanAsAState() {
        let pending = GlomerisLlmSettingSourceWording.title(nil)

        XCTAssertEqual(pending, GlomerisLlmSettingSourceWording.pendingTitle)
        for source in GlomerisLlmSettingSource.allCases {
            XCTAssertNotEqual(
                pending, GlomerisLlmSettingSourceWording.title(source),
                "'still looking' must not be worded as \(source)")
            XCTAssertNotEqual(
                GlomerisLlmSettingSourceWording.symbolName(nil),
                GlomerisLlmSettingSourceWording.symbolName(source))
        }
        XCTAssertFalse(
            pending.lowercased().contains("not set"),
            "an unresolved key must not claim there is no key: \(pending)")
    }

    /// AC 6. The condition that used to take the whole menu-bar item with it now
    /// has to be a sentence the user can act on. Three things have to be in it,
    /// because leaving any one out sends the user somewhere useless: that macOS
    /// refused, that replacing the build is why, and that saving again fixes it.
    func testAnUnreadableKeyIsExplainedWithSomethingTheUserCanDo() {
        let explanation = GlomerisLlmSettingSourceWording.unreadableExplanation.lowercased()

        XCTAssertTrue(explanation.contains("keychain"), "name where the refusal came from")
        XCTAssertTrue(
            explanation.contains("build"),
            "the cause is a changed code identity; without it the user blames the key")
        XCTAssertTrue(
            explanation.contains("again"),
            "the remedy is re-saving the same key: \(explanation)")

        // And it must not be worded as the key being wrong. It is not — the
        // stored key may be perfectly valid and simply unreachable.
        for misattribution in ["invalid key", "wrong key", "incorrect"] {
            XCTAssertFalse(
                explanation.contains(misattribution),
                "\"\(misattribution)\" blames the credential for a keychain refusal")
        }

        XCTAssertNotEqual(
            GlomerisLlmSettingSourceWording.title(.unreadable),
            GlomerisLlmSettingSourceWording.title(.absent),
            "stored-but-unreachable and never-stored need different remedies")
    }

    /// HORO-1471 AC 1. Silence and refusal are told apart because their remedies
    /// are opposite: `.unreadable` is fixed by pasting the key again, and doing
    /// that here would simply block on the same unanswered keychain. Four things
    /// have to be in this sentence — where the silence came from, that it is not
    /// the same as having no key, that nothing was lost, and what actually helps
    /// — and the one thing that must be out of it is any suggestion to re-paste.
    func testAKeychainThatWentQuietIsNotExplainedAsARefusal() {
        let explanation = GlomerisLlmSettingSourceWording.unresponsiveExplanation.lowercased()

        XCTAssertTrue(explanation.contains("keychain"), "name what went quiet")
        XCTAssertTrue(
            explanation.contains("did not answer"),
            "silence, not a refusal: \(explanation)")
        XCTAssertTrue(
            explanation.contains("not the same as"),
            "the user must not read this as 'you have no key stored'")
        XCTAssertTrue(
            explanation.contains("changed or removed"),
            "nothing was lost, and saying so is the difference between worry and patience")
        XCTAssertTrue(
            explanation.contains("log out") || explanation.contains("restart"),
            "the only remedy is restarting the service: \(explanation)")

        // The remedy for a refusal, applied to silence, is actively wrong: the
        // write goes to the same place the read is stuck in.
        for misdirection in ["paste", "not set", "try saving it again"] {
            XCTAssertFalse(
                explanation.contains(misdirection),
                "\"\(misdirection)\" sends the user at a keychain that is not answering")
        }

        XCTAssertNotEqual(
            GlomerisLlmSettingSourceWording.unresponsiveExplanation,
            GlomerisLlmSettingSourceWording.unreadableExplanation,
            "a refusal and a silence cannot share one sentence")
        XCTAssertFalse(
            GlomerisLlmSettingSourceWording.title(.unresponsive).lowercased()
                .contains("not set"),
            "an unknown key must not be titled as no key")
    }

    func testTheEnvironmentSourceNamesWhereTheValueCameFrom() {
        let title = GlomerisLlmSettingSourceWording.title(.environment)

        XCTAssertTrue(
            title.lowercased().contains("environment"),
            "an inherited value the user cannot see the origin of is the bug AC 3 is about")
    }

    /// Every symbol must actually resolve — a mistyped SF Symbol name is not a
    /// compile error, it renders as nothing, and "Key: Not set" with no icon
    /// beside it reads as a layout glitch rather than as a state.
    func testEverySourceSymbolResolvesToARealSFSymbol() {
        let names = GlomerisLlmSettingSource.allCases.map {
            GlomerisLlmSettingSourceWording.symbolName($0)
        } + [GlomerisLlmSettingSourceWording.symbolName(nil)]

        for name in names {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not an SF Symbol")
        }
    }

    /// Both symbols used by the privacy preview's two groups, for the same
    /// reason. Read out of the source rather than hard-coded here so a renamed
    /// symbol cannot pass by being renamed in two places.
    func testThePrivacyPreviewSymbolsResolve() throws {
        let code = try Self.readCode()

        for name in ["paperplane.fill", "lock.fill"] {
            XCTAssertTrue(code.contains(name), "\(name) is no longer used; update this test")
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) is not an SF Symbol")
        }
    }

    // MARK: - GlomerisLlmKeyRemovalWording

    /// The case this type exists for. Per the precedence rule the stored key
    /// beats an inherited `GLOMERIS_LLM_API_KEY`, so deleting the stored one can
    /// *promote* the inherited one into use — and someone pressing a button
    /// labelled "Remove key" is more likely revoking access than tidying a
    /// field. An unqualified "Key removed" would be true about the keychain and
    /// wrong about the product.
    func testRemovingTheKeyWhileOneIsInheritedSaysSoAndNamesTheVariable() {
        let message = GlomerisLlmKeyRemovalWording.message(
            removed: .done, remainingSource: .environment)

        XCTAssertEqual(message.kind, .success, "the removal did happen")
        XCTAssertTrue(
            message.title.contains("GLOMERIS_LLM_API_KEY"),
            "the user cannot unset what the app will not name: \(message.title)")
        XCTAssertTrue(
            message.title.lowercased().contains("instead"),
            "must say the inherited key takes over, not merely that one exists")
    }

    func testRemovingTheOnlyKeyIsReportedWithoutACaveat() {
        let message = GlomerisLlmKeyRemovalWording.message(
            removed: .done, remainingSource: .absent)

        XCTAssertEqual(message.kind, .success)
        XCTAssertFalse(
            message.title.contains("GLOMERIS_LLM_API_KEY"),
            "there is nothing left to warn about; a caveat here would teach the user to ignore it")
    }

    /// A keychain refusal is not a removal. Reporting it as one would leave the
    /// user believing a credential is gone while it is still stored and still
    /// being used.
    func testAKeychainRefusalIsAFailureNotASuccess() {
        let message = GlomerisLlmKeyRemovalWording.message(
            removed: .refused, remainingSource: .settings)

        XCTAssertEqual(message.kind, .failure)
        XCTAssertFalse(
            message.title.lowercased().contains("removed from your keychain"),
            "nothing was removed")
    }

    /// HORO-1476. A removal that was never sent is a failure like a refusal, but
    /// not the same failure: the user has to be told the key is still there, and
    /// told why pressing the button again now will not help either.
    func testARemovalThatWasNeverAttemptedSaysTheKeyIsStillStored() {
        let message = GlomerisLlmKeyRemovalWording.message(
            removed: .notAttempted, remainingSource: .settings)

        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(
            message.title.lowercased().contains("still in your keychain"),
            """
            someone revoking access needs to be told the key survived, in those \
            words: \(message.title)
            """)
        XCTAssertFalse(
            message.title.lowercased().contains("refused"),
            "macOS refused nothing — it was never asked: \(message.title)")
    }

    /// All four outcomes are materially different situations, so all four must
    /// read differently — including the two successes, which differ only in
    /// whether access was actually revoked, and the two failures, which differ in
    /// whether the keychain was ever asked.
    func testTheFourRemovalOutcomesAreDistinguishable() {
        let titles = [
            GlomerisLlmKeyRemovalWording.message(removed: .done, remainingSource: .environment),
            GlomerisLlmKeyRemovalWording.message(removed: .done, remainingSource: .absent),
            GlomerisLlmKeyRemovalWording.message(removed: .refused, remainingSource: .settings),
            GlomerisLlmKeyRemovalWording.message(
                removed: .notAttempted, remainingSource: .settings),
        ].map(\.title)

        XCTAssertEqual(Set(titles).count, titles.count, "shared wording: \(titles)")
    }

    // MARK: - GlomerisLlmKeySaveWording (HORO-1476)

    func testASavedKeyIsReportedAsSaved() {
        let message = GlomerisLlmKeySaveWording.message(.done)

        XCTAssertEqual(message.kind, .success)
        XCTAssertTrue(message.title.lowercased().contains("saved"))
    }

    /// The distinction this type was introduced for. Both are failures, and the
    /// user's next move is different: a refusal is about the item's permissions,
    /// silence is about the machine's keychain service, and only one of them is
    /// worth pressing **Save key** again for.
    func testARefusalAndAWriteThatWasNeverSentReadDifferently() {
        let refused = GlomerisLlmKeySaveWording.message(.refused)
        let notAttempted = GlomerisLlmKeySaveWording.message(.notAttempted)

        XCTAssertEqual(refused.kind, .failure)
        XCTAssertEqual(notAttempted.kind, .failure)
        XCTAssertNotEqual(refused.title, notAttempted.title)
        XCTAssertTrue(
            refused.title.lowercased().contains("refused"),
            "a refusal is a decision macOS made, and saying so is what makes it actionable")
        XCTAssertFalse(
            notAttempted.title.lowercased().contains("refused"),
            """
            macOS refused nothing — it stopped answering, and calling that a \
            refusal sends the user to fix a permission that is not broken: \
            \(notAttempted.title)
            """)
    }

    /// Both failure sentences have to settle the question the user actually has
    /// after pasting a provider key into a field that then emptied itself.
    func testNeitherSaveFailureLeavesTheKeyUnaccountedFor() {
        for outcome in [GlomerisCredentialWriteOutcome.refused, .notAttempted] {
            let title = GlomerisLlmKeySaveWording.message(outcome).title.lowercased()
            XCTAssertTrue(
                title.contains("nothing was saved"),
                "\(outcome) does not say the key was not saved: \(title)")
            XCTAssertTrue(
                title.contains("nothing was written anywhere else"),
                """
                \(outcome) leaves open whether a copy of the key landed somewhere \
                that is not the keychain: \(title)
                """)
        }
    }

    // MARK: - Source-level guards

    /// A run that throws must not leave the previous run's verdict on screen.
    /// This matters more here than elsewhere in the app because
    /// `SectionFetchErrors.shortMessage` returns `nil` for a cancellation by
    /// design: with no error message to render, a stale "Connection OK" would
    /// reappear as though it were the answer to the test the user just stopped.
    /// Asserted by position rather than by presence — `checkOutcome` is assigned
    /// inside the `do` block too, so only an assignment *before* it clears the
    /// old value.
    func testBothRunnersClearTheirPreviousResultBeforeRunning() throws {
        let code = try Self.readCode()

        for (runner, outcome) in [
            ("private func runConnectionTest() async {", "checkOutcome"),
            ("private func runPayloadPreview() async {", "previewOutcome"),
        ] {
            guard let start = code.range(of: runner) else {
                return XCTFail("\(runner) no longer exists; update this test")
            }
            guard let doBlock = code.range(of: "do {", range: start.upperBound..<code.endIndex)
            else {
                return XCTFail("\(runner) no longer has a do block; update this test")
            }
            let preamble = String(code[start.upperBound..<doBlock.lowerBound])
            XCTAssertTrue(
                preamble.contains("\(outcome) = nil"),
                "\(runner) does not clear \(outcome) before running")
        }
    }

    /// AC 7's strongest form. `llm-check` spends the user's money, so it must
    /// run only from a button the user pressed — never on appearance, never on
    /// a timer, never as a retry. Asserted structurally because "it does not
    /// happen by itself" is not something a unit test can observe the absence
    /// of by running.
    ///
    /// HORO-1368 introduced one automatic trigger — the `.task` that resolves
    /// the key's availability — so this can no longer be "nothing runs by
    /// itself". It is narrowed instead of dropped: exactly one `.task`, calling
    /// exactly one function, and that function is checked below to run no
    /// command. Deleting this guard would have been the easy way to green, and
    /// it is the guard that stops an "auto-test on open" from being added.
    func testNeitherCommandCanRunWithoutTheUserPressingSomething() throws {
        let code = try Self.readCode()

        for trigger in [".onAppear", ".task(", "Timer", "DispatchQueue.main.asyncAfter",
                        ".refreshable", ".onReceive"] {
            XCTAssertFalse(
                code.contains(trigger),
                "\(trigger) would make a paid request happen on its own")
        }

        XCTAssertEqual(
            code.components(separatedBy: ".task {").count - 1, 1,
            "exactly one thing on this screen may happen without a press")
        XCTAssertTrue(
            code.contains(".task { await loadCredentialStatus() }"),
            "and it must be that one, whole — a `.task` doing anything else here is "
                + "a command one edit away from running on open")
    }

    /// The other half: the one automatic trigger cannot spend anything. It reads
    /// the keychain and assigns to `status`; it does not touch `client`, does not
    /// name a command, and does not reach the credential *value*.
    func testTheOneAutomaticTaskRunsNoCommandAndSpendsNothing() throws {
        let code = try Self.readCode()
        let body = try XCTUnwrap(
            code.range(of: "private func loadCredentialStatus() async {")
                .flatMap { start in
                    code[start.upperBound...].range(of: "\n    }").map {
                        String(code[start.upperBound..<$0.lowerBound])
                    }
                },
            "loadCredentialStatus no longer exists; update this test")

        for forbidden in ["client", "runRaw", "AiProviderCommands", "withEnvironment",
                          "childEnvironment", "secret(forKey"] {
            XCTAssertFalse(
                body.contains(forbidden),
                "\(forbidden) in the automatic task would make opening this screen do "
                    + "something the user did not ask for")
        }
        XCTAssertTrue(
            body.contains("await store.resolvedStatus()"),
            "the availability query, off the main thread, and nothing else")
    }

    /// The preview runs on the plain client, and there must be exactly one
    /// `withEnvironment` call in the file — the connection test's. A second one
    /// would mean the preview had been handed a credential it has no use for,
    /// which is a preview one keystroke from being a request.
    func testOnlyTheConnectionTestIsGivenTheCredential() throws {
        let code = try Self.readCode()

        XCTAssertEqual(
            code.components(separatedBy: "withEnvironment").count - 1, 1,
            "exactly one command on this screen may carry the key")

        let previewBody = try XCTUnwrap(
            code.range(of: "private func runPayloadPreview() async {")
                .flatMap { start in
                    code[start.upperBound...].range(of: "\n    }").map {
                        String(code[start.upperBound..<$0.lowerBound])
                    }
                })
        XCTAssertFalse(
            previewBody.contains("withEnvironment"),
            "the preview needs no key, so it does not get one")
        XCTAssertFalse(
            previewBody.contains("childEnvironment"),
            "not even indirectly")
        XCTAssertTrue(previewBody.contains("AiProviderCommands.payloadPreview"))
    }

    /// AC 5 and AC 2. The key is bound to a `SecureField` and to nothing else;
    /// there is no "reveal" affordance, no `TextField` for it, and no path from
    /// storage back to the screen — `GlomerisLlmSettingsStore` exposes a `Bool`
    /// and no getter at all. `scripts/check-credential-store-uses-keychain.sh`
    /// asserts the persistence/logging/argv half of this over the whole target;
    /// this asserts the display half over this file.
    func testTheKeyIsNeverDisplayedAndNeverReadBack() throws {
        let code = try Self.readCode()

        XCTAssertTrue(code.contains("SecureField(\"Paste your key\", text: $apiKey)"))
        XCTAssertFalse(
            code.contains("TextField(\"Paste your key\""),
            "the key must never be bound to a plain text field")
        for forbidden in ["showKey", "isKeyVisible", "revealKey", "store.apiKey", "secret(forKey"] {
            XCTAssertFalse(code.contains(forbidden), "\(forbidden) would put the key on screen")
        }
    }

    /// The typed key is cleared whatever happened. Leaving it in a `@State`
    /// after a failed write keeps a secret in memory for as long as the window
    /// is open, and the user can always paste again.
    func testTheTypedKeyIsClearedEvenWhenTheWriteFails() throws {
        let code = try Self.readCode()
        let saveBody = try XCTUnwrap(
            code.range(of: "private func saveKey() async {")
                .flatMap { start in
                    code[start.upperBound...].range(of: "\n    }").map {
                        String(code[start.upperBound..<$0.lowerBound])
                    }
                })

        XCTAssertTrue(saveBody.contains("apiKey = \"\""))
        let clearIndex = try XCTUnwrap(saveBody.range(of: "apiKey = \"\"")).lowerBound
        let branchIndex = try XCTUnwrap(saveBody.range(of: "keyActionMessage")).lowerBound
        XCTAssertTrue(
            clearIndex < branchIndex,
            "the key must be cleared before the success/failure branch, not inside one")
    }

    /// The standing project rule. This screen renders a CLI verdict; it does
    /// not reach one. No policy label, no URL validation, no model list, no
    /// HTTP client.
    func testThisScreenDecidesNothingAboutProvidersOrPolicy() throws {
        let code = try Self.readCode()

        for forbidden in ["AUTO_SAFE", "PROTECTED", "URLSession", "URLRequest", "URLComponents",
                         "hasSuffix(\"/chat/completions\")", "validate", "knownModels"] {
            XCTAssertFalse(code.contains(forbidden), "\(forbidden) belongs in the Rust CLI")
        }
    }

    /// AC 8. Removing a credential must be visible and labelled with what it
    /// does. `role: .destructive` is what makes it read as destructive without
    /// a confirmation sheet standing between a user and revoking a key they
    /// believe is compromised.
    func testRemovingTheKeyIsAnExplicitDestructiveAction() throws {
        let code = try Self.readCode()

        XCTAssertTrue(code.contains("Button(\"Remove key\", role: .destructive)"))
        // Driven by the already-resolved status rather than by asking the store
        // again (HORO-1368 AC 4): this is inside `body`, so `store.hasStoredApiKey`
        // here would be a keychain query per re-evaluation of the view.
        XCTAssertTrue(
            code.contains("if status.apiKey == .settings"),
            "offered exactly when there is something of ours to remove")
        XCTAssertFalse(
            code.contains("store.hasStoredApiKey"),
            "a keychain question asked from inside body is the HORO-1368 defect")
        XCTAssertTrue(code.contains("await store.resolvedDeleteApiKey()"))
    }

    /// HORO-1368 AC 1 and AC 3, as a structural claim over this file: the
    /// keychain is reachable from here only through the store's `resolved*`
    /// members, every one of which hops off the main thread. The synchronous
    /// spellings still exist on the store — `GlomerisLlmSettingsStoreTests` uses
    /// them, and they are what the resolved ones call — so "this screen does not
    /// use them" has to be asserted rather than made impossible.
    ///
    /// Complementary to the behavioural proof in
    /// `GlomerisLlmSettingsStoreTests.testResolvedReadsHappenOffTheMainThread`:
    /// that one shows the hop really happens, this one shows this screen takes it.
    func testEveryKeychainTouchOnThisScreenGoesThroughTheOffThreadRoute() throws {
        let code = try Self.readCode()

        for synchronous in ["store.status(", "store.childEnvironment(", "store.setApiKey(",
                            "store.deleteApiKey(", "store.apiKeyAvailability"] {
            XCTAssertFalse(
                code.contains(synchronous),
                "\(synchronous) reaches the keychain on whichever thread calls it, and on "
                    + "this screen that is the main one")
        }

        // The one synchronous store call that is allowed, because it reads
        // UserDefaults and never the keychain — it is what lets `init` seed
        // without I/O (AC 2).
        XCTAssertTrue(code.contains("store.statusWithoutApiKey()"))
        for resolved in ["await store.resolvedStatus()", "await store.resolvedChildEnvironment()",
                         "await store.resolvedSetApiKey(", "await store.resolvedDeleteApiKey()"] {
            XCTAssertTrue(code.contains(resolved), "\(resolved) is gone; update this test")
        }
    }

    /// AC 2's structural half. `init` runs while `App.body` is being evaluated,
    /// before any scene exists, so anything it does happens on the main thread
    /// whether or not a Settings window is ever opened.
    func testTheViewSeedsItselfWithoutTouchingTheKeychain() throws {
        let code = try Self.readCode()
        let initBody = try XCTUnwrap(
            code.range(of: "projectRootsStore: ProjectRootsStore = ProjectRootsStore()\n    ) {")
                .flatMap { start in
                    code[start.upperBound...].range(of: "\n    }").map {
                        String(code[start.upperBound..<$0.lowerBound])
                    }
                },
            "the initialiser signature changed; update this test")

        XCTAssertTrue(
            initBody.contains("_status = State(initialValue: store.statusWithoutApiKey())"),
            "init must seed from UserDefaults only")
        for forbidden in ["resolved", "status()", "hasStoredApiKey", "availability", "Task {"] {
            XCTAssertFalse(
                initBody.contains(forbidden),
                "\(forbidden) in init would put a keychain query back on the launch path")
        }
    }

    /// The connection test costs money, so the button must say what it does
    /// before it is pressed, and must be disabled when the request could not
    /// possibly reach a provider.
    func testTheTestButtonSaysWhatItCostsAndIsDisabledWhenIncomplete() throws {
        let source = try Self.readSource()

        XCTAssertTrue(source.contains("Sends one tiny request"))
        XCTAssertTrue(source.contains(".disabled(isTesting || !status.isComplete)"))
    }

    /// The privacy preview's whole point is the separation between the two
    /// lists. Rendering them as one would misrepresent the thing the card
    /// exists to disclose.
    func testThePrivacyPreviewSeparatesWhatLeavesFromWhatStays() throws {
        let source = try Self.readSource()

        XCTAssertTrue(source.contains("\"Leaves this Mac\""))
        XCTAssertTrue(source.contains("\"Stays on this Mac\""))
        // Matched on the tail of the sentence rather than the whole of it: the
        // copy is a concatenation of two literals, and a guard spanning the
        // seam would fail on a rewrap rather than on a change of meaning.
        XCTAssertTrue(
            source.contains("leaves this Mac until you ask for a plan"),
            "the card must state that opening it sends nothing")
    }

    /// `.positive` is reserved for AUTO_SAFE and for something that succeeded
    /// (see `GlomerisTone.style`). Data being withheld is neither — it is a
    /// deliberate hold, which is `.guarded`. Asserted because green here would
    /// quietly redefine the app's one colour that means "safe to act on".
    func testWithheldDataIsGuardedNotPositive() throws {
        let code = try Self.readCode()

        XCTAssertTrue(code.contains("tone: .guarded"))
        XCTAssertFalse(
            code.contains("tone: .positive"),
            "green means AUTO_SAFE or succeeded, not 'kept private'")
    }

    /// Every text field and the key field carry an accessibility label, and the
    /// source rows are combined into one element each rather than read out as
    /// a loose icon followed by a fragment of a sentence.
    func testTheScreenIsLabelledForVoiceOver() throws {
        let source = try Self.readSource()

        for label in ["Provider API address", "Model name", "API key"] {
            XCTAssertTrue(
                source.contains(".accessibilityLabel(\"\(label)\")"),
                "\(label) has no accessibility label")
        }
        XCTAssertTrue(source.contains(".accessibilityElement(children: .combine)"))
        XCTAssertTrue(source.contains(".accessibilityElement(children: .contain)"))
    }

    // MARK: - Nothing on the launch path reads the keychain (HORO-1368 AC 4)

    /// The behavioural half of AC 4, and the one that actually reproduces the
    /// defect's mechanism.
    ///
    /// `App.body` constructs this view whether or not a Settings window is open
    /// — `OverviewStateTests.testTheAppSceneHoldsNoObservableStateBecauseItsBodyBuildsTheSettingsTabs`
    /// asserts that path exists — so `init` and every re-evaluation of `body`
    /// happen on the main thread during launch and on every scene update. Before
    /// this ticket that chain ended in a synchronous `SecItemCopyMatching`, which
    /// on a build the keychain item's ACL does not admit blocks behind a
    /// `SecurityAgent` prompt until somebody answers it; AppKit then removes the
    /// status item, and there is no other way into the app.
    ///
    /// Asserted by counting touches rather than by reading the source, because
    /// the touch can be reintroduced from anywhere this view reaches.
    @MainActor
    func testNeitherConstructingNorEvaluatingTheScreenTouchesTheKeychain() {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": "sk-test-only"])
        let view = Self.makeView(credentials: credentials)

        XCTAssertEqual(
            credentials.touches, [],
            "init reached the keychain: \(credentials.touches)")

        // Materialising the body is what `App.body` does to build the Settings
        // tabs. It must be free of keychain work too — the pre-fix version asked
        // the store `hasStoredApiKey` from inside `body`, so every re-evaluation
        // was another query.
        _ = view.body

        XCTAssertEqual(
            credentials.touches, [],
            "evaluating body reached the keychain: \(credentials.touches)")
    }

    /// And the screen still works: the availability query does happen, once the
    /// view is on screen and off the main thread. Without this, the test above
    /// would pass just as well against a screen that never resolved the key at
    /// all and permanently displayed "Checking your keychain…".
    @MainActor
    func testTheScreenDoesResolveTheKeyOnceItIsOnScreen() async {
        let credentials = RecordingCredentialStore(secrets: ["llmApiKey": "sk-test-only"])
        let store = Self.makeStore(credentials: credentials)

        // What `.task { await loadCredentialStatus() }` does. The `.task` itself
        // needs a rendered view to fire, which a unit test has no way to produce
        // — `testNeitherCommandCanRunWithoutTheUserPressingSomething` pins that
        // the modifier is attached and calls exactly this.
        let status = await store.resolvedStatus(environment: [:])

        XCTAssertEqual(status.apiKey, .settings)
        XCTAssertEqual(credentials.operations, [.availability])
        XCTAssertFalse(
            credentials.touches.allSatisfy(\.wasOnMainThread),
            "the resolution must not run where the defect was")
    }

    private static func makeStore(credentials: RecordingCredentialStore)
        -> GlomerisLlmSettingsStore {
        GlomerisLlmSettingsStore(
            defaults: UserDefaults(
                suiteName: "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)")!,
            credentials: credentials
        )
    }

    /// Built with throwaway `UserDefaults` suites so nothing here reads or writes
    /// the running app's own preferences.
    private static func makeView(credentials: RecordingCredentialStore)
        -> AiProviderPreferencesView {
        AiProviderPreferencesView(
            store: makeStore(credentials: credentials),
            projectRootsStore: ProjectRootsStore(
                defaults: UserDefaults(
                    suiteName: "dev.glomeris.GlomerisMenuBarTests.\(UUID().uuidString)")!)
        )
    }

    /// Both text fields write straight through to the store, so the field and
    /// the request can never disagree about which endpoint is in use. The
    /// deprecated `.onChange(of:perform:)` is unavailable warning-free at this
    /// deployment target, and a Save button for these two would reintroduce
    /// unsaved state the connection test could then contradict.
    func testTheEndpointAndModelFieldsHaveNoUnsavedState() throws {
        let code = try Self.readCode()

        XCTAssertTrue(code.contains("store.endpoint = newValue"))
        XCTAssertTrue(code.contains("store.model = newValue"))
        XCTAssertFalse(
            code.contains(".onChange(of:"),
            "deprecated at macOS 14 while the deployment target is 13.0")
    }
}
