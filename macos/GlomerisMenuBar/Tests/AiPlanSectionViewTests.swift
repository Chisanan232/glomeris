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

    /// HORO-1550: the contract-version-2 golden fixture, which is what the card
    /// now asks for and renders.
    ///
    /// The version 1 helpers above stay, and are still used by every row test
    /// below, because `AiPlanRowViewModel`'s version 1 initializer is still a
    /// real entry point and the version 1 fixture is where the PROTECTED
    /// SSH-key item lives — the single most valuable input in this file.
    private static let workspacePlanFixture = "tests/fixtures/dto/workspace_plan_report.json"

    private func goldenWorkspacePlanData() throws -> Data {
        try Data(contentsOf: Self.repoRoot.appendingPathComponent(Self.workspacePlanFixture))
    }

    private func goldenWorkspacePlan() throws -> WorkspacePlanReportDto {
        try JSONDecoder().decode(
            WorkspacePlanReportDto.self,
            from: try goldenWorkspacePlanData()
        )
    }

    private func decodeWorkspaceReport(_ json: String) throws -> WorkspacePlanReportDto {
        try JSONDecoder().decode(WorkspacePlanReportDto.self, from: Data(json.utf8))
    }

    /// Every `dropped` counter at zero, spelled out rather than built with a
    /// helper: this is the wire format, all seventeen keys are always present on
    /// it, and a constant that quietly omitted one would stop being a test of
    /// what Rust actually sends.
    private static let noDroppedJSON = """
    {"unknown_resource":0,"unoffered_action":0,"unknown_disposition":0,"duplicate_item":0,
     "unknown_observation_kind":0,"unknown_probe":0,"unknown_probe_subject":0,
     "incompatible_probe_subject":0,"duplicate_evidence_request":0,"uncited_evidence_ref":0,
     "degraded_unknown_confidence":0,"degraded_unknown_workflow_mode":0,"truncated_items":0,
     "truncated_observations":0,"truncated_evidence_requests":0,"truncated_uncertainties":0,
     "truncated_evidence_refs":0}
    """

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

    /// `readCode()` with adjacent string literals joined, so a phrase that the
    /// line-length limit split across a `+` can still be found whole.
    ///
    /// Added in HORO-1367 after this exact false failure: shortening the
    /// provenance note moved the wrap point, `"may cost money"` became
    /// `"…and may " + "cost money…"`, and the assertion that the card still
    /// discloses the cost failed on copy that discloses it perfectly well. A
    /// guard that reports a missing disclosure because of where a line broke
    /// is a guard that will eventually be silenced rather than believed.
    ///
    /// The pattern requires a closing quote, then only whitespace, a `+`, more
    /// whitespace, and an opening quote — so it joins concatenated literals
    /// and cannot reach across an argument list, where a comma intervenes.
    private static func readJoinedCopy() throws -> String {
        try readCode().replacingOccurrences(
            of: "\"\\s*\\+\\s*\"",
            with: "",
            options: .regularExpression
        )
    }

    /// An empty plan with no provider error — the "your provider had nothing
    /// to say" shape, which the golden fixture cannot also be.
    ///
    /// Empty in every field, not just in `items`. Under the version 2 contract
    /// those are two different replies: a reply with no suggestions but two
    /// observations HAS something to say, and this constant exists to be the
    /// other one.
    private static let emptyPlanJSON = """
    {"contract_version":2,"contract_declared":true,"profile":null,"items":[],
     "observations":[],"evidence_requests":[],"dropped":\(AiPlanSectionViewTests.noDroppedJSON),
     "expansion":null,"provider_error":null}
    """

    /// A provider that failed mid-call. Rust prints exactly this and exits 1.
    private static let providerErrorPlanJSON = """
    {"contract_version":2,"contract_declared":false,"profile":null,"items":[],
     "observations":[],"evidence_requests":[],"dropped":\(AiPlanSectionViewTests.noDroppedJSON),
     "expansion":null,"provider_error":"HTTP 429 from provider"}
    """

    /// No suggestions, but a workspace reading and two observations. HORO-1550:
    /// the shape that must NOT read as "your provider had nothing to propose".
    private static let observationsOnlyPlanJSON = """
    {"contract_version":2,"contract_declared":true,
     "profile":{"mode":"parallel_multi_worktree","confidence":"inferred",
       "evidence_refs":["workflow_history"],"summary":null},
     "items":[],
     "observations":[{"kind":"conflicting_evidence","evidence_refs":["workspace_1"],
       "detail":"A working tree reports no upstream and also reports merged commits."}],
     "evidence_requests":[{"probe_id":"git_branch_state","subject_ref":"workspace_1",
       "reason":null}],
     "dropped":\(AiPlanSectionViewTests.noDroppedJSON),"expansion":null,"provider_error":null}
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

    /// The same shape, but with the planner explaining itself. The distinction
    /// between this and `silentlyRefusedPlanJSON` is one field, and it selects
    /// the one branch of `verdictLines` that suppresses the generic sentence.
    private static let plannerExplainedRefusalPlanJSON = """
    {"items":[{"resource_id":"x:/tmp/x","policy_label":"UNKNOWN_INCOMPLETE",
      "requested_action_id":null,"priority":null,"model_reason":null,"explain":null,
      "skip_reason":"the model named an action that does not exist",
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

    /// The suppression rule, which had no test before: when the CLI reported no
    /// refusal reason of its own but the planner DID explain itself, the
    /// planner's sentence stands alone and the generic one is left off.
    ///
    /// Worth pinning precisely because the code path is unreachable from Rust
    /// today. `verdictLines` guards it with an `if lines.isEmpty` whose comment
    /// says removing it would be a behaviour change smuggled in as a tidy-up —
    /// and until now, doing exactly that would have shipped green.
    func testASkipReasonSuppressesTheGenericSentence() throws {
        let report = try decodeReport(Self.plannerExplainedRefusalPlanJSON)
        let row = AiPlanRowViewModel(report.items[0])

        XCTAssertEqual(
            row.machineVerdictLines,
            ["the model named an action that does not exist"]
        )
        // Specifically: the planner's explanation is not followed by a vaguer
        // restatement of the same fact.
        XCTAssertFalse(
            row.machineVerdictLines.contains("Glomeris has no action it is willing to run for this resource."),
            "the generic sentence was appended after the planner already explained itself"
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

    // MARK: - HORO-1451: the joins, not the parts
    //
    // Every assertion above this section is a `contains`, which is precisely
    // how the defect survived: each clause was present, and the string that
    // joined them was heard as one unpunctuated run. The tests below assert the
    // whole label by equality, so where one clause ends and the next begins is
    // pinned rather than assumed.
    //
    // The expected strings are written out here by hand rather than assembled
    // from the row's own terms. A test that rebuilt the label the way the
    // implementation does would agree with any implementation, including the
    // broken one.

    /// The reported row. The fixture's PROTECTED suggestion is the one shape
    /// that carries BOTH refusal clauses — the planner's "no action was
    /// rendered" and the candidate's policy reason — plus a model quotation and
    /// a path, and Rust terminates none of the three.
    ///
    /// Before this ticket this read "…High confidence. PROTECTED — no cleanup
    /// action is ever rendered for this resource PROTECTED:
    /// protected_credential_material The model's reason, which is advice and not
    /// a verdict: looks like a stale build directory Path: …" — three
    /// unterminated joins, and a listener with no way to tell where the refusal
    /// stopped and the model's opinion started.
    func testTheRefusedSuggestionsWholeLabelIsSpokenAsSentences() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[2])

        XCTAssertEqual(
            row.accessibilityLabel,
            "Rust build output. Safety: Protected. Storage impact: 1.0 KB. "
                + "Evidence: Fully measured. Confidence: High confidence. "
                + "Cleanup: PROTECTED \u{2014} no cleanup action is ever rendered for this "
                + "resource. PROTECTED: protected_credential_material. "
                + "The model's reason, which is advice and not a verdict: "
                + "looks like a stale build directory. "
                + "Path: cargo_target_dir:/Users/dev/.ssh/id_ed25519."
        )
    }

    /// One refusal clause rather than two, and no model reason at all — the
    /// planner explained itself and the candidate gave no reason of its own.
    /// The clause count changes; the punctuation does not.
    func testASingleRefusalClauseIsAlsoTerminated() throws {
        let row = AiPlanRowViewModel(
            try decodeReport(Self.plannerExplainedRefusalPlanJSON).items[0]
        )

        XCTAssertEqual(
            row.accessibilityLabel,
            "Rust build output. Safety: Not enough evidence. Storage impact: Size unknown. "
                + "Evidence: Measurement failed. Confidence: Low confidence. "
                + "Cleanup: the model named an action that does not exist. Path: x:/tmp/x."
        )
    }

    /// Absent optional clauses. This row has no impact tier and no model
    /// reason, so two of the eight clauses are missing — and a dropped clause
    /// must leave no trace, neither a doubled space nor a stranded period.
    func testAnAskSuggestionWithNoOptionalClausesLeavesNoResidue() throws {
        let label = AiPlanRowViewModel(try goldenPlan().items[1]).accessibilityLabel

        XCTAssertEqual(
            label,
            "node_modules. Safety: Asks first. Storage impact: 512.0 MB. "
                + "Evidence: Partly measured. Confidence: Medium confidence. "
                + "Cleanup: Glomeris will ask you to confirm this before anything runs. "
                + "Path: node_modules:/Users/dev/proj/node_modules."
        )
        XCTAssertFalse(label.contains("  "), label)
    }

    /// The other half of the same defect. This row's cleanup sentence and model
    /// reason both already end in a period — `ApplyPlanView` joined with ". "
    /// and produced "…review it and clean.. " from exactly this shape — and the
    /// permitted sentence also has an interior full stop, which a rule that
    /// counted marks rather than looking at the end would trip over.
    func testAPermittedSuggestionGainsNoSecondFullStop() throws {
        let label = AiPlanRowViewModel(try goldenPlan().items[0]).accessibilityLabel

        XCTAssertEqual(
            label,
            "Rust build output. Safety: Safe to reclaim. Storage impact: 2.0 GB. "
                + "Worth a look. Evidence: Fully measured. Confidence: High confidence. "
                + "Cleanup: Glomeris is willing to run this. Open the row to review it and "
                + "clean. The model's reason, which is advice and not a verdict: "
                + "Largest build output and nothing is using it. "
                + "Path: cargo_target_dir:/Users/dev/proj/target."
        )
        XCTAssertFalse(label.contains(".."), label)
    }

    /// A long refusal and a quoted model sentence, together. Both are the cases
    /// where a composer is most tempted to intervene: the refusal is 300-odd
    /// characters of the planner's own words, and the quotation's last character
    /// is a quote mark sitting behind the period that really does end it.
    func testALongRefusalAndAQuotedModelReasonAreBothSpokenWhole() throws {
        let refusal = "cargo.clean.target_dir cannot be planned for this resource: "
            + "the manifest at /Users/dev/proj/Cargo.toml names a workspace member that is "
            + "missing, so the planner cannot establish which target directory this resource "
            + "corresponds to and has nothing it could delete"
        let modelReason = "The directory \u{201C}looks stale to me.\u{201D}"
        let json = """
        {"items":[{"resource_id":"cargo_target_dir:/Users/dev/proj/target",
          "policy_label":"AUTO_SAFE","requested_action_id":null,"priority":1,
          "model_reason":"\(modelReason)","explain":null,"skip_reason":null,
          "candidate":{"resource_id":"cargo_target_dir:/Users/dev/proj/target",
            "kind":"cargo_target_dir","reclaimable_bytes":1024,"reclaimable_human":"1.0 KB",
            "reclaimable_bytes_is_lower_bound":false,"impact_tier":null,
            "policy_label":"AUTO_SAFE","reasons":[],"executable":false,
            "offered_actions":[],"refusal_reason":"\(refusal)"},
          "completeness":"complete","confidence":"high"}],
         "dropped_unknown_resource":0,"dropped_unknown_action":0,"provider_error":null}
        """
        let label = AiPlanRowViewModel(try decodeReport(json).items[0]).accessibilityLabel

        // Neither shortened nor reflowed: the refusal appears whole, once.
        XCTAssertTrue(label.contains("Cleanup: \(refusal)."), label)
        // The quotation keeps the mark it already had and gains no second one.
        XCTAssertTrue(label.contains("and not a verdict: \(modelReason) Path:"), label)
        XCTAssertFalse(label.contains(".."), label)
        XCTAssertFalse(label.contains("\u{201D}."), label)
    }

    /// One resource, two lists, one description of it. The candidates list and
    /// the AI plan card build their own labels from the same
    /// `DetectCandidateReportDto`, and before HORO-1451 only the candidates list
    /// terminated its cleanup clause — so the same refusal was spoken two
    /// different ways depending on which list the user was in.
    ///
    /// Asserted on a plan item with no `skip_reason`, because that is the shape
    /// where the two rows genuinely have the same thing to say. When the planner
    /// does add a statement of its own, the plan row says more by design.
    func testTheSameRefusalIsSpokenIdenticallyInBothLists() throws {
        let refusal = "no registered cleanup action for this resource kind"
        let json = """
        {"items":[{"resource_id":"docker_images:docker","policy_label":"ASK",
          "requested_action_id":null,"priority":1,"model_reason":null,"explain":null,
          "skip_reason":null,
          "candidate":{"resource_id":"docker_images:docker","kind":"docker_images",
            "reclaimable_bytes":1024,"reclaimable_human":"1.0 KB",
            "reclaimable_bytes_is_lower_bound":false,"impact_tier":null,"policy_label":"ASK",
            "reasons":[],"executable":false,"offered_actions":[],
            "refusal_reason":"\(refusal)"},
          "completeness":"complete","confidence":"high"}],
         "dropped_unknown_resource":0,"dropped_unknown_action":0,"provider_error":null}
        """
        let item = try decodeReport(json).items[0]

        let planLabel = AiPlanRowViewModel(item).accessibilityLabel
        let candidateLabel = CandidateRowViewModel(item.candidate).accessibilityLabel

        let clause = "\(CandidateActionability.axis): \(refusal)."
        XCTAssertTrue(planLabel.contains(clause), planLabel)
        XCTAssertTrue(candidateLabel.contains(clause), candidateLabel)

        // And the same clause reaches "Path:" the same way in both, which is the
        // join the defect broke.
        XCTAssertTrue(planLabel.contains("\(clause) Path:"), planLabel)
        XCTAssertTrue(candidateLabel.contains("\(clause) Path:"), candidateLabel)
    }

    // MARK: - The exit-code contract

    func testExitZeroWithAReportIsAPlan() throws {
        let outcome = AiPlanInterpretation.interpret(
            exitCode: 0,
            stdout: try goldenWorkspacePlanData(),
            stderr: Data()
        )
        guard case .plan(let report) = outcome else {
            return XCTFail("exit 0 with a report must be a plan, got \(outcome)")
        }
        XCTAssertEqual(report.items.count, 2)
        XCTAssertNil(report.providerError)
        XCTAssertEqual(report.contractVersion, 2)
    }

    /// HORO-1550. A version 1 report is a perfectly valid thing for a CLI to
    /// print, and it is not a version 2 one. Reading it as a version 2 plan
    /// would show a user a plan with no dispositions, no confidences and no
    /// uncertainties and give them no reason to doubt it — which is the exact
    /// mistake `--contract-version`'s own refusal path exists to prevent.
    func testAVersionOneReportIsNotReadAsAVersionTwoPlan() throws {
        XCTAssertEqual(
            AiPlanInterpretation.interpret(
                exitCode: 0,
                stdout: try goldenPlanData(),
                stderr: Data()
            ),
            .malformedOutput
        )
    }

    /// The version is checked, not just the shape. A future contract could be
    /// decodable as this one and mean something else.
    func testAReportDeclaringAnotherContractVersionIsNotRead() throws {
        let json = Self.emptyPlanJSON.replacingOccurrences(
            of: "\"contract_version\":2",
            with: "\"contract_version\":3"
        )
        XCTAssertNotEqual(json, Self.emptyPlanJSON, "nothing to substitute — the constant changed")
        XCTAssertEqual(
            AiPlanInterpretation.interpret(exitCode: 0, stdout: Data(json.utf8), stderr: Data()),
            .malformedOutput
        )
    }

    /// HORO-1550: a `glomeris` older than the flag rejects it while parsing
    /// arguments — before any provider is contacted, so nothing was sent and
    /// nothing was billed. Its own outcome, because the remedy is a CLI update
    /// rather than a setting or a bug report.
    func testAnOlderCliRejectingTheContractFlagIsItsOwnOutcome() {
        let stderr = "glomeris llm-plan: unrecognized argument '--contract-version'\n"
        XCTAssertEqual(
            AiPlanInterpretation.interpret(exitCode: 2, stdout: Data(), stderr: Data(stderr.utf8)),
            .contractUnsupported
        )
    }

    /// Anti-vacuity for the test above, and the reason the marker names the flag
    /// instead of matching "unrecognized argument" on its own: this app sending
    /// an argument the CLI does not know is this app's bug, and it must stay a
    /// failure rather than become "your CLI is too old".
    func testAnUnrecognizedArgumentThatIsNotTheContractFlagStaysAFailure() {
        let stderr = "glomeris llm-plan: unrecognized argument '--evidence-rounds'\n"
        guard
            case .failed = AiPlanInterpretation.interpret(
                exitCode: 2,
                stdout: Data(),
                stderr: Data(stderr.utf8)
            )
        else {
            return XCTFail("a different unrecognized argument must not read as an old CLI")
        }
    }

    /// Drift guard across the language boundary, the same shape as the
    /// missing-configuration one below: the sentence this app matches on has to
    /// be the sentence Rust emits, and the flag has to be one Rust accepts. If
    /// `--contract-version` is ever renamed, the card must fail loudly here
    /// rather than start reporting every current CLI as too old.
    func testTheContractFlagAndItsRejectionSentenceMatchTheRustSource() throws {
        let mainRs = Self.repoRoot.appendingPathComponent("src/main.rs")
        let source = try String(contentsOf: mainRs, encoding: .utf8)

        XCTAssertTrue(
            source.contains("\"\(AiPlanInterpretation.contractVersionFlag)\" => {"),
            "src/main.rs no longer handles \(AiPlanInterpretation.contractVersionFlag)"
        )
        XCTAssertTrue(
            source.contains("unrecognized argument '{other}'"),
            "the sentence `unsupportedContractMarker` is built from is gone from src/main.rs"
        )
        XCTAssertEqual(
            AiPlanInterpretation.unsupportedContractMarker,
            "unrecognized argument '\(AiPlanInterpretation.contractVersionFlag)'"
        )
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

    /// Not the user's fault, but still a failure: the card cannot answer the
    /// question. The copy has to carry the two facts a user would otherwise
    /// assume the worst about — that nothing was sent and nothing was charged.
    func testAnOlderCliIsAFailureThatStatesNothingWasSentOrCharged() throws {
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .contractUnsupported, isPlanning: false)
        )
        XCTAssertEqual(message.kind, .failure)
        XCTAssertTrue(message.title.contains("Nothing was sent"))
        XCTAssertTrue(message.title.contains("nothing was charged"))
        XCTAssertNotEqual(
            message.title,
            try XCTUnwrap(
                AiPlanStateMessages.message(for: .malformedOutput, isPlanning: false)
            ).title,
            "a CLI too old to answer and a CLI whose answer cannot be read have different remedies"
        )
    }

    func testAProviderErrorIsPresentedAsAFailureAndQuotesIt() throws {
        let report = try decodeWorkspaceReport(Self.providerErrorPlanJSON)
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
        let report = try decodeWorkspaceReport(Self.providerErrorPlanJSON)
        XCTAssertTrue(report.items.isEmpty)

        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .plan(report), isPlanning: false)
        )
        XCTAssertEqual(message.kind, .failure)
        XCTAssertFalse(message.title.contains("No suggestions"))
    }

    func testAnEmptyPlanFromAHealthyProviderIsNotAFailure() throws {
        let report = try decodeWorkspaceReport(Self.emptyPlanJSON)
        let message = try XCTUnwrap(
            AiPlanStateMessages.message(for: .plan(report), isPlanning: false)
        )
        XCTAssertEqual(message.kind, .empty)
        XCTAssertEqual(message.title, "No suggestions")
    }

    /// HORO-1550, and the reason `saidSomethingBesidesItems` exists. Under the
    /// version 1 contract "no items" and "no answer" were one fact. They are not
    /// under version 2: a reply that proposes nothing, names a workflow shape,
    /// reports a conflict and asks for a branch check has said the most useful
    /// thing in the whole feature, and covering it with "had nothing to propose"
    /// would hide exactly that.
    func testAReplyWithNoSuggestionsButRealObservationsIsNotCalledEmpty() throws {
        let report = try decodeWorkspaceReport(Self.observationsOnlyPlanJSON)
        XCTAssertTrue(report.items.isEmpty)
        XCTAssertTrue(report.saidSomethingBesidesItems)
        XCTAssertNil(
            AiPlanStateMessages.message(for: .plan(report), isPlanning: false),
            "the explanation block is the answer here; a state message would talk over it"
        )
    }

    /// Anti-vacuity for the test above: a reply whose every suggestion was
    /// *discarded* proposed nothing usable, and counting validation failures as
    /// "it said something" would dress them up as findings.
    func testDiscardedSuggestionsDoNotCountAsHavingSaidSomething() throws {
        let json = Self.emptyPlanJSON.replacingOccurrences(
            of: "\"unknown_resource\":0",
            with: "\"unknown_resource\":4"
        )
        XCTAssertNotEqual(json, Self.emptyPlanJSON, "nothing to substitute — the constant changed")

        let report = try decodeWorkspaceReport(json)
        XCTAssertEqual(report.dropped.unknownResource, 4)
        XCTAssertFalse(report.saidSomethingBesidesItems)
        XCTAssertEqual(
            try XCTUnwrap(
                AiPlanStateMessages.message(for: .plan(report), isPlanning: false)
            ).title,
            "No suggestions"
        )
    }

    func testAPlanWithRowsShowsNoMessageAtAll() throws {
        let report = try goldenWorkspacePlan()
        XCTAssertNil(AiPlanStateMessages.message(for: .plan(report), isPlanning: false))
    }

    func testEverySituationGetsADistinctPresentation() throws {
        let titles = [
            AiPlanStateMessages.message(for: nil, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: nil, isPlanning: true)?.title,
            AiPlanStateMessages.message(for: .notConfigured, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: .contractUnsupported, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: .malformedOutput, isPlanning: false)?.title,
            AiPlanStateMessages.message(for: .failed("boom"), isPlanning: false)?.title,
            AiPlanStateMessages.message(
                for: .plan(try decodeWorkspaceReport(Self.emptyPlanJSON)),
                isPlanning: false
            )?.title,
        ].compactMap { $0 }

        XCTAssertEqual(titles.count, 7)
        XCTAssertEqual(Set(titles).count, 7, "two situations share one sentence: \(titles)")
    }

    // MARK: - Discarded suggestions are stated, not swallowed

    func testDroppedSuggestionsAreReportedWithBothReasonsAndATotal() throws {
        // The golden fixture's `dropped` deliberately spans all three families:
        // two refusals, one degraded claim and two truncated uncertainties.
        let texts = AiPlanSectionView.discardedTexts(try goldenWorkspacePlan())
        let discarded = try XCTUnwrap(texts.first { $0.contains("discarded") })

        XCTAssertTrue(discarded.contains("3 parts of the reply were discarded"))
        XCTAssertTrue(discarded.contains("1 named a resource Glomeris never found"))
        XCTAssertTrue(discarded.contains("1 asked for an action Glomeris does not offer for it"))
        XCTAssertTrue(discarded.contains("1 asked for a check Glomeris does not run"))
    }

    /// HORO-1550. Three families, three sentences, and the split is the test:
    /// a *degraded* claim was kept with its confidence downgraded, and a
    /// *truncated* list was cut by Glomeris's own bound rather than by anything
    /// the provider did. Reporting either as "discarded" would misattribute it —
    /// the first makes the model look worse than it was, the second blames the
    /// provider for Glomeris's limit.
    func testADegradedClaimAndAGlomerisLimitAreNotReportedAsDiscardedSuggestions() throws {
        let texts = AiPlanSectionView.discardedTexts(try goldenWorkspacePlan())
        XCTAssertEqual(texts.count, 3, "expected one sentence per family: \(texts)")

        let degraded = try XCTUnwrap(texts.first { $0.contains("read as unknown") })
        XCTAssertTrue(degraded.contains("1 claim was read as unknown"))
        XCTAssertTrue(degraded.contains("a confidence that is not in the contract"))
        XCTAssertFalse(degraded.contains("discarded"))

        let truncated = try XCTUnwrap(texts.first { $0.contains("cut short") })
        XCTAssertTrue(truncated.contains("2 entries were cut short by Glomeris's own limit"))
        XCTAssertTrue(truncated.contains("2 uncertainties on a suggestion"))
        XCTAssertFalse(truncated.contains("discarded"))
    }

    func testNothingIsSaidWhenNothingWasDropped() throws {
        XCTAssertTrue(
            AiPlanSectionView.discardedTexts(
                try decodeWorkspaceReport(Self.emptyPlanJSON)
            ).isEmpty
        )
    }

    func testASingleDroppedSuggestionIsPhrasedInTheSingular() throws {
        let json = Self.emptyPlanJSON.replacingOccurrences(
            of: "\"unknown_resource\":0",
            with: "\"unknown_resource\":1"
        )
        XCTAssertNotEqual(json, Self.emptyPlanJSON, "nothing to substitute — the constant changed")

        let texts = AiPlanSectionView.discardedTexts(try decodeWorkspaceReport(json))
        XCTAssertEqual(texts.count, 1)
        XCTAssertTrue(texts[0].contains("1 part of the reply was discarded"))
        XCTAssertFalse(texts[0].contains("parts of the reply were"))
    }

    /// Every counter Rust can set has a phrase, and the total is summed from the
    /// same list the phrases come from.
    ///
    /// This is the guard for the failure mode a seventeen-counter struct invites:
    /// a counter added in Rust, mirrored in the DTO, and forgotten here — which
    /// would silently discard part of a reply and tell the user nothing. It works
    /// by setting every counter to 1 and requiring the three totals to add up to
    /// seventeen, so an unmentioned counter shows up as arithmetic rather than as
    /// missing prose nobody notices.
    func testEveryDroppedCounterIsAccountedForInSomeSentence() throws {
        let json = Self.emptyPlanJSON.replacingOccurrences(of: "\":0", with: "\":1")
        let report = try decodeWorkspaceReport(json)
        let texts = AiPlanSectionView.discardedTexts(report)
        XCTAssertEqual(texts.count, 3, "all three families should be present: \(texts)")

        let totals = texts.compactMap { text -> Int? in
            // The leading integer of each sentence, which `tally` builds from the
            // same array as the breakdown that follows it.
            let digits = text.prefix { $0.isNumber }
            return digits.isEmpty ? 1 : Int(digits)
        }
        XCTAssertEqual(
            totals.reduce(0, +), 17,
            "a `dropped` counter has no phrase in `discardedTexts` — it would be discarded "
                + "silently. Totals were \(totals) across: \(texts)"
        )
    }

    // MARK: - Mechanical source invariants

    /// A paid network call must never be a side effect of opening a popover.
    /// Same shape as `CandidatesSectionViewTests
    /// .testDetectIsOnlyCalledWithProgressJSONAndNeverOnATimer`.
    func testLlmPlanIsConstructedOnceAndNeverRunsAutomatically() throws {
        let code = try Self.readCode()

        let occurrences = code.components(separatedBy: "\"llm-plan\"").count - 1
        XCTAssertEqual(occurrences, 1, "llm-plan must be constructed in exactly one place")

        // HORO-1550: version 2 and nothing else, built from the same constants
        // the interpreter matches on so the request and the rejection detector
        // cannot drift apart.
        XCTAssertTrue(code.contains("AiPlanInterpretation.contractVersionFlag,"))
        XCTAssertTrue(code.contains("\"\\(AiPlanInterpretation.expectedContractVersion)\","))
        XCTAssertTrue(code.contains("\"--json\","))
        XCTAssertTrue(code.contains("\"--progress-json\","))

        // A multi-round expansion is several billed provider calls. One button
        // press must never decide to make more than one.
        XCTAssertFalse(
            code.contains("--evidence-rounds"),
            "the Ask button must not request a multi-round expansion"
        )

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

    /// The key that Ask needs is fetched off the main thread (HORO-1368).
    ///
    /// `runLlmPlan()` is `@MainActor`, and the synchronous
    /// `childEnvironment(basedOn:)` reads the API key's *value* — which
    /// `SecItemCopyMatching` can only produce by decrypting the item, which
    /// consults its ACL, which on a build the item does not admit waits on a
    /// `SecurityAgent` prompt. A main thread waiting on that cannot service the
    /// status item, and AppKit removes it: the app vanishes from the menu bar
    /// with no way back in. So this is the one call site on this screen that can
    /// really block, and it has to go through the store's `resolved` route.
    ///
    /// Asserted by name rather than by behaviour because the failure is a thread
    /// identity, and the store's own tests already prove the `resolved` route
    /// leaves the main thread. What can regress *here* is someone deleting an
    /// `await` to quiet a warning.
    func testTheProviderKeyForAskIsFetchedOffTheMainThread() throws {
        let code = try Self.readCode()

        XCTAssertTrue(
            code.contains(".withEnvironment(await settingsStore.resolvedChildEnvironment())"),
            "the Ask path must build its child environment through the off-thread route"
        )
        XCTAssertFalse(
            code.contains("settingsStore.childEnvironment("),
            "a main-thread keychain read here is HORO-1368: the menu-bar item disappears"
        )
        // Case-insensitive so this counts the `resolvedChildEnvironment` spelling
        // too — and it has to, because counting only the lowercase one reported
        // zero on a file that assembles the environment exactly once.
        XCTAssertEqual(
            code.lowercased().components(separatedBy: "childenvironment").count - 1, 1,
            "exactly one place may assemble the provider environment"
        )
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

    /// The rows are in the model's order, and the note says so. The second
    /// half is the load-bearing one: it is what stops the card reading as
    /// Glomeris's own verdict, and it is the reason HORO-1308 placed this
    /// card *below* the candidates list rather than above it.
    ///
    /// Pinned because HORO-1367 shortened this line for density, and the
    /// half that would be tempting to drop next is the longer one.
    func testTheOrderingNoteStillSaysWhoseOrderItIsAndWhereGlomerisOwnRankingIs() throws {
        let code = try Self.readCode()
        XCTAssertTrue(code.contains("The model's order"), "the rows' order must be attributed")
        XCTAssertTrue(
            code.contains("Glomeris's own ranking is the list above"),
            "without this the card reads as Glomeris's verdict rather than a second opinion"
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
            source.range(
                of: "private func modelQuote(_ reading: AiPlanModelReadingViewModel) -> some View {"
            ),
            "no `modelQuote(_ reading:)` — renamed, or no longer a function"
        )
        // Ends at the next `private func`, which is `modelSentence`. HORO-1550
        // widened this block from one sentence to the whole reading, so it is no
        // longer the last `@ViewBuilder` in the file and windowing to
        // `runLlmPlan()` would swallow the workspace block and the wording
        // helpers with it.
        let rest = source[afterSignature.upperBound...]
        let end = try XCTUnwrap(
            rest.range(of: "\n    private func ")?.lowerBound,
            "could not find the end of modelQuote"
        )
        let functionText = String(rest[..<end])

        XCTAssertFalse(
            functionText.contains("GlomerisBadgeView"),
            "the model's words must be a quotation, not a verdict chip"
        )
        XCTAssertTrue(functionText.contains("The model says"), "the quote must be attributed")

        // HORO-1550: the disposition is the field most likely to be mistaken for
        // the policy verdict, so it renders through the same helper as the quote
        // rather than through anything of its own.
        XCTAssertTrue(
            functionText.contains("modelSentence(reading.dispositionSentence)"),
            "the model's recommendation must be styled exactly like its quote"
        )
    }

    // MARK: - HORO-1550: the model's reading is a tier, not a verdict

    /// AC 3. The wire format keeps the machine's row nested inside the model's
    /// wrapper, and the view models keep the same split — so a surface rendering
    /// "what Glomeris determined" and a surface rendering "what the model read
    /// into it" cannot draw from the same properties.
    func testTheModelsRecommendationNeverBecomesAPolicyVerdict() throws {
        let report = try goldenWorkspacePlan()
        let asked = report.items[1]
        XCTAssertEqual(asked.disposition, "recommend_now")
        XCTAssertEqual(asked.item.candidate.policyLabel, "ASK")

        let row = AiPlanRowViewModel(asked)

        // The machine's side is untouched by the model's enthusiasm.
        XCTAssertEqual(row.safetyTerm, GlomerisVocabulary.safety("ASK"))
        XCTAssertTrue(
            row.machineVerdictLines.joined(separator: " ").lowercased().contains("ask"),
            "the machine verdict must still say Glomeris asks first: \(row.machineVerdictLines)"
        )

        // And the model's side is nowhere in it.
        let reading = try XCTUnwrap(row.modelReading)
        XCTAssertEqual(reading.dispositionSentence, "Recommends reclaiming this now.")
        for line in row.machineVerdictLines {
            XCTAssertFalse(
                line.contains("Recommends reclaiming"),
                "the model's recommendation leaked into a machine verdict line"
            )
        }
    }

    /// A `defer` against an `AUTO_SAFE` resource: the disposition disagrees with
    /// the policy class in the other direction. Neither one moves.
    func testAModelThatDefersDoesNotDowngradeTheMachinesVerdictEither() throws {
        let report = try goldenWorkspacePlan()
        let deferred = report.items[0]
        XCTAssertEqual(deferred.disposition, "defer")
        XCTAssertEqual(deferred.item.candidate.policyLabel, "AUTO_SAFE")

        let row = AiPlanRowViewModel(deferred)
        XCTAssertEqual(row.safetyTerm, GlomerisVocabulary.safety("AUTO_SAFE"))
        XCTAssertEqual(
            try XCTUnwrap(row.modelReading).dispositionSentence,
            "Suggests leaving this for now."
        )
    }

    /// §15: the model may recommend, and its recommendation may only ever reduce
    /// what a bulk action sweeps up. A `defer` stays out of Apply; nothing the
    /// model says can put anything *into* it.
    func testOnlyRecommendNowItemsReachABulkApply() throws {
        let report = try goldenWorkspacePlan()
        let applied = AiPlanSectionView.recommendedNowItems(report)

        XCTAssertEqual(applied.count, 1)
        XCTAssertEqual(applied[0].candidate.resourceId, report.items[1].item.candidate.resourceId)
        XCTAssertFalse(
            applied.contains { $0.candidate.resourceId == report.items[0].item.candidate.resourceId },
            "a resource the model deferred must not be in a one-click batch"
        )
    }

    /// AC 6, and §13's rule at the level a user actually meets it: every
    /// uncertainty the model reported is spoken, attributed, and audibly not a
    /// finding.
    func testEveryUncertaintyIsSpokenAndAttributedToTheModel() throws {
        let report = try goldenWorkspacePlan()
        let item = report.items[1]
        XCTAssertEqual(item.uncertainties.count, 2)

        let label = AiPlanRowViewModel(item).accessibilityLabel
        XCTAssertTrue(label.contains("The model's reading, which is advice and not a verdict"))
        for uncertainty in item.uncertainties {
            XCTAssertTrue(label.contains(uncertainty), "an uncertainty is not spoken: \(uncertainty)")
        }
        XCTAssertTrue(label.contains("It says it does not know."))
        XCTAssertTrue(label.contains("It cites: resource_2, workflow_history"))
    }

    /// The machine's verdict is heard before the model's reading, which is the
    /// same priority the badges give a sighted user.
    func testTheSpokenRowPutsTheMachineVerdictBeforeTheModelsReading() throws {
        let label = AiPlanRowViewModel(try goldenWorkspacePlan().items[1]).accessibilityLabel
        let verdict = try XCTUnwrap(label.range(of: CandidateActionability.axis))
        let reading = try XCTUnwrap(label.range(of: "The model's reading"))
        XCTAssertTrue(
            verdict.lowerBound < reading.lowerBound,
            "a listener hears the model's opinion before Glomeris's verdict: \(label)"
        )
    }

    /// A version 1 row has no reading, and its single-sentence attribution is
    /// unchanged — the two initializers do not bleed into each other.
    func testAVersionOneRowStillCarriesNoReadingAndKeepsItsOwnAttribution() throws {
        let row = AiPlanRowViewModel(try goldenPlan().items[0])
        XCTAssertNil(row.modelReading)
        XCTAssertTrue(
            row.accessibilityLabel.contains("The model's reason, which is advice and not a verdict")
        )
    }

    /// Unrecognised vocabulary is named, never rendered as nothing. A newer CLI
    /// paired with this app is the one skew `contractUnsupported` cannot catch,
    /// and dropping the model's recommendation from the one place a user looks
    /// for it would be the worst way to meet it.
    func testAnUnrecognisedDispositionOrConfidenceIsNamedRatherThanSwallowed() throws {
        let report = try goldenWorkspacePlan()
        let data = try goldenWorkspacePlanData()
        let mutated = String(decoding: data, as: UTF8.self)
            .replacingOccurrences(of: "\"disposition\": \"defer\"", with: "\"disposition\": \"nope\"")
            .replacingOccurrences(
                of: "\"model_confidence\": \"inferred\"",
                with: "\"model_confidence\": \"maybe\""
            )
        XCTAssertNotEqual(
            mutated, String(decoding: data, as: UTF8.self),
            "nothing to substitute — the fixture's formatting changed"
        )

        let item = try JSONDecoder()
            .decode(WorkspacePlanReportDto.self, from: Data(mutated.utf8))
            .items[0]
        XCTAssertNotEqual(item.disposition, report.items[0].disposition)

        let reading = AiPlanModelReadingViewModel(item)
        XCTAssertTrue(reading.dispositionSentence.contains("nope"))
        XCTAssertTrue(reading.dispositionSentence.contains("does not recognise"))
        XCTAssertTrue(reading.confidenceSentence.contains("maybe"))
        XCTAssertTrue(reading.confidenceSentence.contains("does not recognise"))
    }

    // MARK: - HORO-1550: a check that could not run is not an answer

    /// §13's most important sentence, at the one place on screen where breaking
    /// it would be invisible: a probe that timed out, hit a missing tool or was
    /// never attempted has told Glomeris nothing, and must not read as a
    /// negative finding.
    func testAnUnansweredCheckSaysSoRatherThanReadingAsANegativeFinding() throws {
        let expansion = try XCTUnwrap(try goldenWorkspacePlan().expansion)
        let unavailable = try XCTUnwrap(expansion.findings.first { $0.unavailableReason != nil })
        XCTAssertEqual(unavailable.unavailableReason, "not_attempted")

        let sentence = AiPlanSectionView.findingSentence(unavailable)
        XCTAssertTrue(sentence.contains("could not check"))
        XCTAssertTrue(sentence.contains("Glomeris did not run it"))
        XCTAssertTrue(
            sentence.contains("That is not an answer either way"),
            "an unavailable probe must state that it is not evidence: \(sentence)"
        )
    }

    /// And an answered one reads as an answer, so the sentence above is a real
    /// distinction rather than a disclaimer on everything.
    func testAnAnsweredCheckReadsAsAnAnswer() throws {
        let expansion = try XCTUnwrap(try goldenWorkspacePlan().expansion)
        let answered = try XCTUnwrap(expansion.findings.first { $0.unavailableReason == nil })

        let sentence = AiPlanSectionView.findingSentence(answered)
        XCTAssertTrue(sentence.contains("checked"))
        XCTAssertFalse(sentence.contains("could not check"))
        XCTAssertFalse(sentence.contains("not an answer"))
    }

    /// Every `ProbeReason` tag means "no information" and none of them means
    /// "no". Asserted over the whole vocabulary because the wording is what
    /// carries it, and one phrase drifting into a negative claim is exactly the
    /// regression this guard is for.
    func testNoUnavailableReasonPhraseAssertsAnAbsence() {
        let reasons = [
            "tool_absent", "tool_not_running", "permission_denied", "timed_out",
            "rate_limited", "failed", "not_attempted",
        ]
        var phrases: Set<String> = []
        for reason in reasons {
            let phrase = AiPlanSectionView.unavailableReasonPhrase(reason)
            XCTAssertFalse(phrase.contains("does not recognise"), "\(reason) has no phrase")
            for forbidden in ["nothing is", "no pull request", "no task", "idle", "not in use"] {
                XCTAssertFalse(
                    phrase.lowercased().contains(forbidden),
                    "\(reason) reads as a negative finding: \(phrase)"
                )
            }
            phrases.insert(phrase)
        }
        XCTAssertEqual(phrases.count, reasons.count, "two reasons share one phrase: \(phrases)")
    }

    /// §12. The profile describes a workspace and never a person, and it never
    /// states a shape without the model's own confidence in it attached.
    func testTheWorkflowProfileSentenceRanksNobodyAndCarriesItsOwnConfidence() throws {
        let profile = try XCTUnwrap(try goldenWorkspacePlan().profile)
        let sentence = AiPlanSectionView.profileSentence(profile)

        XCTAssertTrue(sentence.contains("a mix of one-at-a-time and parallel working trees"))
        XCTAssertTrue(
            sentence.contains("inferred rather than observed"),
            "a shape must never be stated flat: \(sentence)"
        )
        for judgement in ["advanced", "beginner", "expert", "good", "bad", "should", "you are"] {
            XCTAssertFalse(
                sentence.lowercased().contains(judgement),
                "the profile judges the user: \(sentence)"
            )
        }
    }

    /// Asking is not finding out. The two live in different parts of the report
    /// and must read differently, or "it wanted to check" becomes "it checked".
    func testARequestForEvidenceDoesNotReadAsAnAnswer() throws {
        let request = try XCTUnwrap(try goldenWorkspacePlan().evidenceRequests.first)
        let sentence = AiPlanSectionView.evidenceRequestSentence(request)

        XCTAssertTrue(sentence.contains("It asked Glomeris to check"))
        XCTAssertTrue(sentence.contains("any pull request"))
        XCTAssertTrue(sentence.contains("workspace_1"))
        XCTAssertFalse(sentence.contains("checked"), "a request must not read as a finding")
    }

    /// §16's bounds exist because a bounded loop may stop with questions
    /// outstanding, so the sentence has to carry both halves: how far it got AND
    /// that it had not finished.
    func testTheExpansionSentenceSaysBothHowFarItGotAndThatItWasStillAsking() throws {
        let expansion = try XCTUnwrap(try goldenWorkspacePlan().expansion)
        XCTAssertFalse(expansion.converged)

        let sentence = AiPlanSectionView.expansionSentence(expansion)
        XCTAssertTrue(sentence.contains("2 of an allowed 12 checks"))
        XCTAssertTrue(sentence.contains("3 of 3 rounds"))
        XCTAssertTrue(
            sentence.contains("still asking"),
            "a run that stopped at a limit must not read as one that finished: \(sentence)"
        )
    }

    /// The converged case, so the clause above is a distinction and not boilerplate.
    func testARunThatRanOutOfQuestionsDoesNotClaimItWasCutOff() throws {
        let data = try goldenWorkspacePlanData()
        let mutated = String(decoding: data, as: UTF8.self).replacingOccurrences(
            of: "\"stopped_because\": \"round_limit\"",
            with: "\"stopped_because\": \"nothing_more_asked\""
        )
        XCTAssertNotEqual(
            mutated, String(decoding: data, as: UTF8.self),
            "nothing to substitute — the fixture's formatting changed"
        )

        let expansion = try XCTUnwrap(
            try JSONDecoder()
                .decode(WorkspacePlanReportDto.self, from: Data(mutated.utf8))
                .expansion
        )
        let sentence = AiPlanSectionView.expansionSentence(expansion)
        XCTAssertTrue(sentence.contains("it had nothing more to ask"))
        XCTAssertFalse(sentence.contains("still asking"))
    }

    /// An observation with no detail still says that the model raised that kind
    /// of point. Dropping the entry would be the tidier rendering and the less
    /// honest one.
    func testAnObservationWithNoDetailStillNamesWhatKindItWas() throws {
        let observations = try goldenWorkspacePlan().observations
        let detailed = try XCTUnwrap(observations.first { $0.detail != nil })
        let bare = try XCTUnwrap(observations.first { $0.detail == nil })

        XCTAssertEqual(
            AiPlanSectionView.observationSentence(detailed),
            "Evidence that disagrees with itself: \(try XCTUnwrap(detailed.detail))"
        )
        XCTAssertEqual(
            AiPlanSectionView.observationSentence(bare),
            "Evidence it says it did not get."
        )
    }

    /// The explanation block explains and offers nothing to press. §17: Workspace
    /// Intelligence supports the Recovery Goal rather than becoming a second
    /// control plane.
    func testTheWorkspaceReadingBlockHasNoControlsInIt() throws {
        let source = try Self.readCode()
        let start = try XCTUnwrap(
            source.range(of: "private func workspaceReadingBlock(")?.upperBound,
            "no `workspaceReadingBlock(` — renamed, or no longer a function"
        )
        let rest = source[start...]
        let end = try XCTUnwrap(
            rest.range(of: "\n    static func profileSentence(")?.lowerBound,
            "could not find the end of workspaceReadingBlock"
        )
        let body = String(rest[..<end])

        for control in ["Button", "Toggle", "GlomerisSettingsButton", "onTapGesture", "Menu("] {
            XCTAssertFalse(body.contains(control), "the explanation block grew a control: \(control)")
        }
        XCTAssertTrue(
            body.contains("The model's reading of this workspace"),
            "the block must attribute itself"
        )
        XCTAssertFalse(
            body.contains("GlomerisBadgeView"),
            "nothing the model inferred may be chipped"
        )
    }

    /// The standing rule of the product, on screen before anything is asked
    /// for — and the fact that asking is what sends data anywhere.
    ///
    /// HORO-1367 shortened this note, and that is exactly the kind of edit
    /// that quietly drops a disclosure — so each of the four facts is
    /// asserted separately rather than as one sentence. Shorter wording is
    /// allowed to break this test; losing a fact is not.
    ///
    /// Run against `readJoinedCopy()`, not `readSource()`, for two reasons:
    /// the note's own doc comment lists the four facts it is keeping, so raw
    /// text would satisfy every assertion here from the rationale alone; and
    /// a fact that happens to straddle a line wrap is still disclosed.
    func testTheCardStatesWhoDecidesAndWhatAskingCosts() throws {
        let source = try Self.readJoinedCopy()
        XCTAssertTrue(source.contains("The model recommends. Glomeris decides what may run."))
        XCTAssertTrue(source.contains("sends a summary"), "that asking transmits anything at all")
        XCTAssertTrue(source.contains("your provider"), "whose provider receives it")
        XCTAssertTrue(source.contains("may cost money"), "that asking can be billed")
        XCTAssertTrue(
            source.contains("Settings shows exactly what would be sent"),
            "the preview claim must stay the exact one: \"previews it\" is true of a summary or "
                + "a sample, and what Settings displays is the payload itself"
        )
        XCTAssertTrue(
            source.contains("without sending it"),
            "and it must be clear that previewing does not itself send"
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

    // MARK: - HORO-1363: the suggestion row stays actionable

    /// The same defect, and the same fix, as the candidate row — see
    /// `CandidatesSectionViewTests.testTheCandidateRowRemainsAPressableButton`.
    /// A suggestion row carries the model's reason and nothing else in this app
    /// repeats it, so a row a VoiceOver user cannot press is a row whose
    /// evidence they cannot reach.
    func testTheSuggestionRowRemainsAPressableButton() throws {
        let body = try Self.rowViewBody(in: try Self.readCode())

        XCTAssertTrue(
            body.contains("Button {"),
            "the row must stay a Button — that is where AXPress comes from"
        )
        XCTAssertFalse(
            Self.ignoresItsChildren(body),
            "HORO-1363: .accessibilityElement(children: .ignore) is back on the AI Plan row, "
                + "which costs it the AXButton role and the AXPress action"
        )
        XCTAssertTrue(body.contains(".accessibilityLabel(row.accessibilityLabel)"))
        XCTAssertTrue(body.contains(".accessibilityHint("))
        XCTAssertFalse(body.contains("func modelQuote"), "the rowView window over-ran its function")
    }

    /// Anti-vacuity for the test above.
    func testThePressableSuggestionRowGuardWouldCatchTheModifierReturning() throws {
        let body = try Self.rowViewBody(in: try Self.readCode())
        let regressed = body.replacingOccurrences(
            of: ".buttonStyle(.plain)",
            with: ".buttonStyle(.plain)\n        .accessibilityElement(children: .ignore)"
        )
        XCTAssertNotEqual(regressed, body, "no .buttonStyle(.plain) to splice onto — the row changed shape")
        XCTAssertTrue(
            Self.ignoresItsChildren(regressed),
            "the guard would not notice the modifier returning"
        )
    }

    /// `rowView`'s body, windowed at the function's own indentation.
    private static func rowViewBody(in source: String) throws -> String {
        let signature = try XCTUnwrap(
            source.range(of: "private func rowView("),
            "no `private func rowView(` — renamed, or no longer a function"
        )
        let rest = source[signature.upperBound...]
        let end = try XCTUnwrap(rest.range(of: "\n    }\n"), "could not find the end of rowView")
        return String(rest[..<end.upperBound])
    }

    private static func ignoresItsChildren(_ body: String) -> Bool {
        body.contains(".accessibilityElement(children: .ignore)")
    }
}
