//
//  AiPlanSectionViewTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1308. Four things are asserted here, in descending order of how much
//  they matter:
//
//  1. A model recommendation cannot make anything executable. The row built
//     from a PROTECTED suggestion carries no willingness to act, keeps both
//     refusal sentences, and still quotes the model — attributed, beside the
//     refusal, never instead of it.
//  2. `llm-plan`'s exit-code contract is honoured, including the case that is
//     easy to get wrong: exit 1 prints a complete report FIRST and then fails,
//     so throwing the body away would discard the only structured account of
//     what the provider did.
//  3. The six request states have six distinct presentations, and the two that
//     are not failures are not dressed as failures.
//  4. Mechanically: nothing in the view spawns `llm-plan` automatically, sorts
//     the model's order, renders `priority`, or builds a badge out of model
//     text.
//
//  The row tests run against the real golden fixture
//  (`tests/fixtures/dto/llm_plan_report.json`) rather than hand-built DTOs, so
//  they exercise the exact wire shape `build_llm_plan_report` emits. Only the
//  few shapes that fixture deliberately does not contain are built from inline
//  JSON below.
//

import XCTest

final class AiPlanSectionViewTests: XCTestCase {
    // MARK: - Fixtures

    /// The repo root, found the same way `DtoGoldenFixturesTests` finds it.
    private static let repoRoot: URL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // GlomerisMenuBar
        .deletingLastPathComponent() // macos
        .deletingLastPathComponent() // repo root

    private func goldenPlan() throws -> LlmPlanReportDto {
        let url = Self.repoRoot.appendingPathComponent("tests/fixtures/dto/llm_plan_report.json")
        return try JSONDecoder().decode(LlmPlanReportDto.self, from: try Data(contentsOf: url))
    }

    private func goldenPlanData() throws -> Data {
        let url = Self.repoRoot.appendingPathComponent("tests/fixtures/dto/llm_plan_report.json")
        return try Data(contentsOf: url)
    }

    private func decodeReport(_ json: String) throws -> LlmPlanReportDto {
        try JSONDecoder().decode(LlmPlanReportDto.self, from: Data(json.utf8))
    }

    private static func readSource() throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/AiPlanSectionView.swift")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }

    /// The source with whole-line comments removed.
    ///
    /// Every "this must not appear" guard below runs against this rather than
    /// the raw text, because the file's own prose explains at length *why* it
    /// contains no `.sorted`, no `fingerprintToken` and no rendered
    /// `priority` — and a guard that trips on its own rationale would force
    /// the explanation to be deleted to stay green. Same reason, and the same
    /// mechanism, as `scripts/check-no-policy-label-branching.sh`.
    private static func readCode() throws -> String {
        try readSource()
            .components(separatedBy: "\n")
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    /// An empty plan with no provider error — the "your provider had nothing
    /// to say" shape, which the golden fixture cannot also be.
    private static let emptyPlanJSON = """
    {"items":[],"dropped_unknown_resource":0,"dropped_unknown_action":0,"provider_error":null}
    """

    /// A provider that failed mid-call. Rust prints exactly this and exits 1.
    private static let providerErrorPlanJSON = """
    {"items":[],"dropped_unknown_resource":0,"dropped_unknown_action":0,
     "provider_error":"HTTP 429 from provider"}
    """

    /// A non-executable item with NEITHER a skip reason nor a refusal reason.
    /// Not a shape Rust emits today, which is exactly why it is worth pinning:
    /// a row that silently says nothing about whether Glomeris will act is the
    /// one failure mode of `machineVerdictLines` that would not look wrong.
    private static let silentlyRefusedPlanJSON = """
    {"items":[{"resource_id":"x:/tmp/x","policy_label":"UNKNOWN_INCOMPLETE",
      "requested_action_id":null,"priority":null,"model_reason":null,"explain":null,
      "skip_reason":null,
      "candidate":{"resource_id":"x:/tmp/x","kind":"cargo_target_dir","reclaimable_bytes":null,
        "reclaimable_human":null,"reclaimable_bytes_is_lower_bound":true,"impact_tier":null,
        "policy_label":"UNKNOWN_INCOMPLETE","reasons":[],"executable":false,
        "offered_actions":[],"refusal_reason":null},
      "completeness":"failed","confidence":"low"}],
     "dropped_unknown_resource":0,"dropped_unknown_action":0,"provider_error":null}
    """

    // MARK: - A recommendation cannot enable anything

    /// The one that matters. The golden fixture's third item is a confident,
    /// plausible-sounding recommendation to delete an SSH private key.
    func testAProtectedSuggestionYieldsNoWillingnessToAct() throws {
        let protected = try goldenPlan().items[2]
        let row = AiPlanRowViewModel(protected)

        // Both sentences, because they say different things: one says no
        // action was rendered at all, the other names the policy reason code.
        XCTAssertEqual(
            row.machineVerdictLines,
            [
                "PROTECTED — no cleanup action is ever rendered for this resource",
                "PROTECTED: protected_credential_material",
            ]
        )

        // And nothing anywhere in the row hints that it could be run. Checked
        // as a property of the whole verdict text rather than of one field, so
        // a future reword cannot reintroduce a willingness claim.
        let verdict = row.machineVerdictLines.joined(separator: " ")
        XCTAssertFalse(verdict.contains("willing"))
        XCTAssertFalse(verdict.contains("confirm"))
    }

    /// The model's sentence survives — suppressing it would be its own kind of
    /// dishonesty — but it is carried as a quotation and has no effect on the
    /// verdict beside it.
    func testAProtectedSuggestionStillCarriesTheModelsReasonAsAQuotation() throws {
        let protected = try goldenPlan().items[2]
        let row = AiPlanRowViewModel(protected)

        XCTAssertEqual(row.modelReason, "looks like a stale build directory")

        let label = row.accessibilityLabel
        let attribution = try XCTUnwrap(
            label.range(of: "The model's reason, which is advice and not a verdict:")
        )
        let quote = try XCTUnwrap(label.range(of: "looks like a stale build directory"))
        XCTAssertLessThan(
            attribution.lowerBound,
            quote.lowerBound,
            "VoiceOver must hear who said it before hearing what was said"
        )
    }

    /// The model was most eloquent about the item it was most wrong about, and
    /// gave no rationale at all for the one that genuinely needs confirming.
    /// So neither the presence nor the absence of a rationale may track what
    /// Glomeris is willing to do.
    func testTheModelsRationaleDoesNotTrackWhatGlomerisWillDo() throws {
        let items = try goldenPlan().items
        let explainedAndRefused = AiPlanRowViewModel(items[2])
        let unexplainedAndAllowed = AiPlanRowViewModel(items[1])

        XCTAssertNotNil(explainedAndRefused.modelReason)
        XCTAssertNil(unexplainedAndAllowed.modelReason)
        XCTAssertEqual(
            unexplainedAndAllowed.machineVerdictLines,
            ["Glomeris will ask you to confirm this before anything runs."]
        )
    }

    // MARK: - Verdict lines for the allowed shapes

    func testAnAutoSafeSuggestionSaysGlomerisIsWillingAndDoesNotPromiseToAsk() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[0])
        XCTAssertEqual(
            row.machineVerdictLines,
            ["Glomeris is willing to run this. Open the row to review it and clean."]
        )
    }

    /// An `ASK` row must promise a confirmation, because that promise is the
    /// entire user-visible difference between ASK and AUTO_SAFE.
    func testAnAskSuggestionPromisesAConfirmation() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[1])
        XCTAssertEqual(
            row.machineVerdictLines,
            ["Glomeris will ask you to confirm this before anything runs."]
        )
    }

    func testANonExecutableRowWithNoSentencesAtAllStillStatesARefusal() throws {
        let report = try decodeReport(Self.silentlyRefusedPlanJSON)
        let row = AiPlanRowViewModel(report.items[0])

        XCTAssertEqual(
            row.machineVerdictLines,
            ["Glomeris has no action it is willing to run for this resource."]
        )
    }

    // MARK: - Evidence axes

    /// `completeness`/`confidence` come from the plan ITEM, not from the
    /// nested candidate — `detect --json` does not carry them, which is why
    /// the candidates list cannot show this axis and this card can.
    func testEvidenceAndConfidenceTermsComeFromTheItem() throws {
        let items = try goldenPlan().items

        let complete = AiPlanRowViewModel(items[0])
        XCTAssertEqual(complete.completenessTerm.axis, GlomerisVocabulary.completenessAxis)
        XCTAssertEqual(complete.completenessTerm.title, "Fully measured")
        XCTAssertEqual(complete.confidenceTerm.axis, GlomerisVocabulary.confidenceAxis)
        XCTAssertEqual(complete.confidenceTerm.title, "High confidence")

        let partial = AiPlanRowViewModel(items[1])
        XCTAssertEqual(partial.completenessTerm.title, "Partly measured")
        XCTAssertEqual(partial.confidenceTerm.title, "Medium confidence")
    }

    /// The PROTECTED item is fully measured and high-confidence, and is also
    /// the one Glomeris refuses outright. Confidence is not safety; this
    /// fixture makes the two disagree so nothing can quietly conflate them.
    func testHighConfidenceDoesNotSoftenARefusal() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[2])
        XCTAssertEqual(row.confidenceTerm.title, "High confidence")
        XCTAssertEqual(row.safetyTerm.tone, .guarded)
        XCTAssertFalse(row.machineVerdictLines.isEmpty)
    }

    /// Size is not safety either: the PROTECTED item is the smallest of the
    /// three and the AUTO_SAFE one carries the `notable` tier chip.
    func testImpactAndSafetyRemainIndependentAxesOnARow() throws {
        let items = try goldenPlan().items
        let safe = AiPlanRowViewModel(items[0])
        let protected = AiPlanRowViewModel(items[2])

        XCTAssertEqual(safe.impactTerm.title, "2.0 GB")
        XCTAssertEqual(safe.impactTerm.tone, .neutral)
        XCTAssertEqual(safe.impactTierTerm?.title, "Worth a look")

        XCTAssertEqual(protected.impactTerm.title, "1.0 KB")
        XCTAssertNil(protected.impactTierTerm, "a normal-sized resource gets no emphasis chip")
        // Not tinted for being small, just as the 2.0 GB one is not tinted for
        // being large. Hue belongs to the safety axis alone.
        XCTAssertEqual(protected.impactTerm.tone, .neutral)
    }

    // MARK: - Accessibility ordering

    /// A screen-reader user hears the machine's verdict before the model's
    /// opinion — the same priority the badges give a sighted user by sitting
    /// above the quotation.
    func testAccessibilityLabelPutsTheMachineVerdictBeforeTheModelsReason() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[2])
        let label = row.accessibilityLabel

        let verdict = try XCTUnwrap(label.range(of: "PROTECTED — no cleanup action"))
        let model = try XCTUnwrap(label.range(of: "The model's reason"))
        XCTAssertLessThan(verdict.lowerBound, model.lowerBound)
    }

    func testAccessibilityLabelNamesEveryAxisAndThePath() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[0])
        let label = row.accessibilityLabel

        XCTAssertTrue(label.contains(GlomerisVocabulary.safetyAxis))
        XCTAssertTrue(label.contains(GlomerisVocabulary.impactAxis))
        XCTAssertTrue(label.contains(GlomerisVocabulary.completenessAxis))
        XCTAssertTrue(label.contains(GlomerisVocabulary.confidenceAxis))
        XCTAssertTrue(label.contains("Path: cargo_target_dir:/Users/dev/proj/target."))
    }

    // MARK: - The exit-code contract

    func testExitZeroWithAReportIsAPlan() throws {
        let outcome = AiPlanInterpretation.interpret(
            exitCode: 0,
            stdout: try goldenPlanData(),
            stderr: Data()
        )
        guard case .plan(let report) = outcome else {
            return XCTFail("exit 0 with a report must be a plan, got \(outcome)")
        }
        XCTAssertEqual(report.items.count, 3)
        XCTAssertNil(report.providerError)
    }

    /// The case worth the most: exit 1 means the provider call failed, and the
    /// CLI printed the whole report to stdout BEFORE exiting. Treating the
    /// exit code as authoritative and discarding stdout would throw away the
    /// only structured account of what happened.
    func testExitOneStillYieldsTheReportThatWasPrintedBeforeIt() throws {
        let outcome = AiPlanInterpretation.interpret(
            exitCode: 1,
            stdout: Data(Self.providerErrorPlanJSON.utf8),
            stderr: Data("glomeris llm-plan: provider call failed\n".utf8)
        )
        guard case .plan(let report) = outcome else {
            return XCTFail("exit 1 with a printed report must keep it, got \(outcome)")
        }
        XCTAssertEqual(report.providerError, "HTTP 429 from provider")
    }

    func testExitTwoWithTheMissingConfigurationSentenceIsNotConfigured() {
        let stderr = "glomeris llm-plan: missing LLM configuration — set GLOMERIS_LLM_API_KEY, "
            + "GLOMERIS_LLM_BASE_URL, and GLOMERIS_LLM_MODEL, or pass --plan-file <path> instead\n"
        XCTAssertEqual(
            AiPlanInterpretation.interpret(exitCode: 2, stdout: Data(), stderr: Data(stderr.utf8)),
            .notConfigured
        )
    }

    /// Exit 2 also covers usage errors, and those are this app's bug rather
    /// than the user's missing setting. Conflating them would show a settings
    /// prompt that fixes nothing.
    func testExitTwoForAnyOtherReasonIsAFailureNotAMissingConfiguration() {
        let stderr = "glomeris llm-plan: unrecognized argument '--nope'\n"
        XCTAssertEqual(
            AiPlanInterpretation.interpret(exitCode: 2, stdout: Data(), stderr: Data(stderr.utf8)),
            .failed("glomeris llm-plan: unrecognized argument '--nope'")
        )
    }

    func testExitTwoWithNoStderrStillSaysSomethingActionable() {
        let outcome = AiPlanInterpretation.interpret(exitCode: 2, stdout: Data(), stderr: Data())
        guard case .failed(let message) = outcome else {
            return XCTFail("expected a failure, got \(outcome)")
        }
        XCTAssertFalse(message.isEmpty)
    }

    /// Exit 0 with output this app cannot decode is a version skew between the
    /// app and the `glomeris` on `PATH` — distinct from a provider problem,
    /// and with a different remedy.
    func testExitZeroWithUndecodableStdoutIsMalformedOutput() {
        XCTAssertEqual(
            AiPlanInterpretation.interpret(
                exitCode: 0,
                stdout: Data("not json at all".utf8),
                stderr: Data()
            ),
            .malformedOutput
        )
    }

    func testANonZeroExitWithNoReadableReportCarriesTheCliStderr() {
        XCTAssertEqual(
            AiPlanInterpretation.interpret(
                exitCode: 1,
                stdout: Data(),
                stderr: Data("glomeris llm-plan: could not enumerate project roots\n".utf8)
            ),
            .failed("glomeris llm-plan: could not enumerate project roots")
        )
    }

    func testAnUnexpectedExitCodeWithNoStderrNamesTheCode() {
        let outcome = AiPlanInterpretation.interpret(exitCode: 9, stdout: Data(), stderr: Data())
        guard case .failed(let message) = outcome else {
            return XCTFail("expected a failure, got \(outcome)")
        }
        XCTAssertTrue(message.contains("9"), "an opaque exit code must at least be named")
    }

    /// Drift guard across the language boundary: the marker this app matches
    /// on has to actually appear in the sentence Rust emits. If someone
    /// rewords `run_llm_plan_command`'s message, "no provider configured"
    /// silently becomes "hard failure" and the user gets a red triangle
    /// instead of a settings prompt.
    func testTheMissingConfigurationMarkerAppearsInTheRustSourceThatEmitsIt() throws {
        let mainRs = Self.repoRoot.appendingPathComponent("src/main.rs")
        let source = try String(contentsOf: mainRs, encoding: .utf8)
        XCTAssertTrue(
            source.contains(AiPlanInterpretation.missingConfigurationMarker),
            "src/main.rs no longer contains "
                + "'\(AiPlanInterpretation.missingConfigurationMarker)' — the AI Plan card's "
                + "no-provider state is now unreachable. Update the marker, not this test."
        )
    }

    // MARK: - One state, one presentation

    func testNeverHavingAskedIsNotPresentedAsAFindingOrAFailure() throws {
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: nil, isPlanning: false)
        )
        XCTAssertEqual(message.kind, .empty)
        // `notLookedYet`'s glyph, not `empty`'s checkmark: nobody has earned a
        // clean bill of health here.
        XCTAssertEqual(message.symbolName, "magnifyingglass")
    }

    func testAskingInFlightIsLoadingAndNamesWhatIsBeingWaitedOn() throws {
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: nil, isPlanning: true)
        )
        XCTAssertEqual(message.kind, .loading)
        XCTAssertNotEqual(message.title, "Loading…")
    }

    /// Nothing was sent and nothing was charged, so this must not read as an
    /// error. It is an opt-in prompt.
    func testNoProviderConfiguredIsNotPresentedAsAFailure() throws {
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .notConfigured, isPlanning: false)
        )
        XCTAssertEqual(message.kind, .empty)
        XCTAssertNotEqual(message.tone, .critical)
        // The Finder-launch caveat is the single most likely reason a user who
        // did configure a provider still lands here.
        let detail = try XCTUnwrap(message.detail)
        XCTAssertTrue(detail.contains("shell environment"))
    }

    func testAVersionSkewIsPresentedAsAFailure() throws {
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .malformedOutput, isPlanning: false)
        )
        XCTAssertEqual(message.kind, .failure)
    }

    func testAProviderErrorIsPresentedAsAFailureAndQuotesIt() throws {
        let report = try decodeReport(Self.providerErrorPlanJSON)
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .plan(report), isPlanning: false)
        )
        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("HTTP 429 from provider"))
    }

    /// Ordering inside `.plan` is load-bearing: a failed provider call usually
    /// also yields zero items, and "No suggestions" would report a call that
    /// never completed as a considered answer.
    func testAProviderErrorOutranksTheEmptyPlanMessage() throws {
        let report = try decodeReport(Self.providerErrorPlanJSON)
        XCTAssertTrue(report.items.isEmpty)

        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .plan(report), isPlanning: false)
        )
        XCTAssertEqual(message.kind, .failure)
        XCTAssertFalse(message.title.contains("No suggestions"))
    }

    func testAnEmptyPlanFromAHealthyProviderIsNotAFailure() throws {
        let report = try decodeReport(Self.emptyPlanJSON)
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .plan(report), isPlanning: false)
        )
        XCTAssertEqual(message.kind, .empty)
        XCTAssertEqual(message.title, "No suggestions")
    }

    func testAPlanWithRowsShowsNoMessageAtAll() throws {
        let report = try goldenPlan()
        XCTAssertNil(AiPlanStateMessages.message(for: .plan(report), isPlanning: false))
    }

    func testEverySituationGetsADistinctPresentation() throws {
        let titles = [
            AiPlanStateMessages.message(for: nil, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: nil, isPlanning: true)?.title,
            AiPlanStateMessages.message(for: .notConfigured, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: .malformedOutput, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: .failed("boom"), isPlanning: false)?.title,
            AiPlanStateMessages.message(
                for: .plan(try decodeReport(Self.emptyPlanJSON)),
                isPlanning: false
            )?.title,
        ].compactMap { $0 }

        XCTAssertEqual(titles.count, 6)
        XCTAssertEqual(Set(titles).count, 6, "two situations share one sentence: \(titles)")
    }

    // MARK: - Discarded suggestions are stated, not swallowed

    func testDroppedSuggestionsAreReportedWithBothReasonsAndATotal() throws {
        let text = try XCTUnwrap(AiPlanSectionView.droppedText(try goldenPlan()))
        XCTAssertTrue(text.contains("3 suggestions were discarded"))
        XCTAssertTrue(text.contains("1 named a resource Glomeris never found"))
        XCTAssertTrue(text.contains("2 asked for an action Glomeris does not have"))
    }

    func testNothingIsSaidWhenNothingWasDropped() throws {
        XCTAssertNil(AiPlanSectionView.droppedText(try decodeReport(Self.emptyPlanJSON)))
    }

    func testASingleDroppedSuggestionIsPhrasedInTheSingular() throws {
        let json = """
        {"items":[],"dropped_unknown_resource":1,"dropped_unknown_action":0,"provider_error":null}
        """
        let text = try XCTUnwrap(AiPlanSectionView.droppedText(try decodeReport(json)))
        XCTAssertTrue(text.contains("1 suggestion was discarded"))
        XCTAssertFalse(text.contains("suggestions were"))
    }

    // MARK: - Mechanical source invariants

    /// A paid network call must never be a side effect of opening a popover.
    /// Same shape as `CandidatesSectionViewTests
    /// .testDetectIsOnlyCalledWithProgressJSONAndNeverOnATimer`.
    func testLlmPlanIsConstructedOnceAndNeverRunsAutomatically() throws {
        let code = try Self.readCode()

        let occurrences = code.components(separatedBy: "\"llm-plan\"").count - 1
        XCTAssertEqual(occurrences, 1, "llm-plan must be constructed in exactly one place")
        XCTAssertTrue(code.contains("[\"llm-plan\", \"--json\", \"--progress-json\"]"))

        XCTAssertFalse(code.contains("Timer("), "no code path may ask for a plan on a Timer")
        XCTAssertFalse(code.contains("Task.sleep"), "no code path may retry in a sleep loop")
        XCTAssertFalse(code.contains("pollTask"), "no polling task may drive llm-plan")
        XCTAssertFalse(code.contains(".task {"), "a plan must never run from an appear trigger")
        XCTAssertFalse(code.contains(".onAppear"), "a plan must never run on appear")
    }

    func testRunLlmPlanIsOnlyInvokedFromTheAskButtonAction() throws {
        let code = try Self.readCode()
        // One definition plus exactly one call site.
        XCTAssertEqual(code.components(separatedBy: "runLlmPlan()").count - 1, 2)
        XCTAssertTrue(code.contains("planTask = Task { await runLlmPlan() }"))
    }

    /// Stop cancels the request; it does not pretend to. And the cancellation
    /// goes through the `Task`, which is what `GlomerisClient` turns into a
    /// `SIGTERM` for the child (HORO-1308).
    func testStopCancelsTheRunningRequest() throws {
        let source = try Self.readSource()
        XCTAssertTrue(source.contains("Button(\"Stop\")"))
        XCTAssertTrue(source.contains("planTask?.cancel()"))
    }

    /// No execution path, and no re-ranking of the model's order.
    func testTheCardNeverExecutesAnythingNorReordersTheModelsAdvice() throws {
        let code = try Self.readCode()

        XCTAssertFalse(code.contains("\"execute\""), "the AI Plan card must never run execute")
        XCTAssertFalse(code.contains("performClean"), "no cleanup call site may live here")
        XCTAssertFalse(
            code.contains("fingerprintToken"),
            "a plan carries no consent token and must not pretend to"
        )
        XCTAssertFalse(
            code.contains(".sorted"),
            "re-ordering the model's advice in Swift would be a third ranking"
        )
    }

    /// `priority` is a second, independently-wrong-able copy of the claim the
    /// list order already makes. It stays on the DTO for `--json` consumers
    /// and off the screen.
    func testThePlanItemsPriorityIsNeverRendered() throws {
        XCTAssertFalse(try Self.readCode().contains("priority"), "priority must not reach the UI")
    }

    /// A chip is this app's grammar for "a verdict was reached". Every badge on
    /// a row therefore has to be built from a `GlomerisVocabulary` term, never
    /// from a model-supplied string — which is what putting a model's sentence
    /// in a chip would do.
    func testEveryBadgeIsBuiltFromAVocabularyTermAndNeverFromModelText() throws {
        let code = try Self.readCode()
        let sites = code.components(separatedBy: "GlomerisBadgeView(term: ").dropFirst()

        XCTAssertFalse(sites.isEmpty, "this guard has been defeated by a rename — update it")
        for site in sites {
            let argument = String(site.prefix { $0 != "," && $0 != ")" })
            XCTAssertTrue(
                argument.hasSuffix("Term"),
                "badge built from `\(argument)`, which is not a vocabulary term"
            )
        }
    }

    /// The model's sentence is rendered by `modelQuote`, and that function must
    /// contain no badge — the attribution has to be a quotation. Asserted on
    /// the function's own text so a future edit inside it is what trips this,
    /// rather than an unrelated badge elsewhere in the file.
    func testTheModelQuoteIsNotRenderedAsABadge() throws {
        let source = try Self.readSource()
        let afterSignature = try XCTUnwrap(
            source.range(of: "private func modelQuote(_ reason: String) -> some View {")
        )
        // The function is short and is the last `@ViewBuilder` before
        // `runLlmPlan()`, so ending the window at that definition reads the
        // whole body and nothing after it.
        let rest = source[afterSignature.upperBound...]
        let end = rest.range(of: "private func runLlmPlan()")?.lowerBound ?? rest.endIndex
        let functionText = String(rest[..<end])

        XCTAssertFalse(
            functionText.contains("GlomerisBadgeView"),
            "the model's words must be a quotation, not a verdict chip"
        )
        XCTAssertTrue(functionText.contains("The model says"), "the quote must be attributed")
    }

    /// The standing rule of the product, on screen before anything is asked
    /// for — and the fact that asking is what sends data anywhere.
    func testTheCardStatesWhoDecidesAndWhatAskingCosts() throws {
        let source = try Self.readSource()
        XCTAssertTrue(source.contains("The model recommends. Glomeris decides what may run."))
        XCTAssertTrue(source.contains("may cost money"))
        XCTAssertTrue(
            source.contains("Settings shows exactly what would be sent"),
            "the privacy preview must be discoverable from here"
        )
        // HORO-1309 moved the preview into the GUI. Until then this card told
        // the user to run `glomeris llm-plan --print-payload` in a terminal —
        // advice that was correct and also unusable for the audience of a
        // menu-bar app, which is exactly the class of instruction this campaign
        // is removing. Asserted as an absence so it cannot come back as a
        // "helpful" addition once the GUI surface exists.
        XCTAssertFalse(
            source.contains("Run `glomeris"),
            "the card must not send the user to a terminal for something the GUI now does"
        )
    }
}
