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
    func testEachSettingSourceReadsDifferently() {
        let sources: [GlomerisLlmSettingSource] = [.settings, .environment, .absent]
        let titles = sources.map(GlomerisLlmSettingSourceWording.title)
        let symbols = sources.map(GlomerisLlmSettingSourceWording.symbolName)

        XCTAssertEqual(Set(titles).count, sources.count, "shared wording: \(titles)")
        XCTAssertEqual(Set(symbols).count, sources.count, "shared symbol: \(symbols)")
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
        for source in [GlomerisLlmSettingSource.settings, .environment, .absent] {
            let name = GlomerisLlmSettingSourceWording.symbolName(source)
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

    // MARK: - Source-level guards

    /// AC 7's strongest form. `llm-check` spends the user's money, so it must
    /// run only from a button the user pressed — never on appearance, never on
    /// a timer, never as a retry. Asserted structurally because "it does not
    /// happen by itself" is not something a unit test can observe the absence
    /// of by running.
    func testNeitherCommandCanRunWithoutTheUserPressingSomething() throws {
        let code = try Self.readCode()

        for trigger in [".onAppear", ".task {", ".task(", "Timer", "DispatchQueue.main.asyncAfter",
                        ".refreshable", ".onReceive"] {
            XCTAssertFalse(
                code.contains(trigger),
                "\(trigger) would make a paid request happen on its own")
        }
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
            code.range(of: "private func saveKey() {")
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
        XCTAssertTrue(
            code.contains("if store.hasStoredApiKey"),
            "offered exactly when there is something to remove")
        XCTAssertTrue(code.contains("store.deleteApiKey()"))
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
