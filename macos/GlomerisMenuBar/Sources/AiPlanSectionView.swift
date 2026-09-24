//
//  AiPlanSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1308: the AI Plan card — a first-class GUI surface over the advisory
//  `glomeris llm-plan --json` report, so asking a provider "what should I
//  clean first?" no longer requires a terminal.
//
//  ---------------------------------------------------------------------
//  The one sentence this whole file exists to make visible on screen
//  ---------------------------------------------------------------------
//  AI recommends. Policy decides. Executor verifies.
//
//  Two fields in an `llm-plan` item are a provider's words — `modelReason`
//  and `priority` — and so is the ORDER of the items. Everything else was
//  computed locally by the real policy engine from real evidence and reads
//  identically whether or not a provider was ever contacted. This card is
//  therefore built around one visual rule: a model's claim is rendered as an
//  attributed quotation, and a machine verdict is rendered as a badge or a
//  plain statement. They never share a presentation, so "the model was
//  confident" can never be mistaken on a glance for "Glomeris agreed".
//
//  ---------------------------------------------------------------------
//  Why there is no rank number on a row
//  ---------------------------------------------------------------------
//  `plan_with_llm` pushes validated items in the order the provider returned
//  them and never sorts; validation only drops entries (see the doc comment
//  on `LlmPlanItemReportDto`). So the list order here is advice, whereas the
//  candidates list's order is Glomeris's own ranking (`reporting::ranking`).
//  Numbering these rows "1, 2, 3" would dress a model's opinion in the exact
//  typography this app uses for machine judgments, and `priority` — a second,
//  independently-wrong-able copy of the same claim — would make it worse by
//  contradicting the order it sits in. Both are deliberately unrendered; the
//  one-line note above the rows says whose order it is instead.
//
//  There is equally no `.sorted` call in this file. Re-ordering a provider's
//  advice in Swift would be a third ranking, invented by the thinnest layer
//  in the system, and the honest way to disagree with the order is to read
//  the candidates list — which is right above this card.
//
//  ---------------------------------------------------------------------
//  Nothing here can execute anything
//  ---------------------------------------------------------------------
//  A row tap opens `CandidateDetailView`, the sole host of the Clean button,
//  by resource id — exactly as a candidates-list row does. That view issues
//  its own `explain` call and reads `executable` /
//  `offeredActions[].requiresConfirmation` / `fingerprintToken` from *that*
//  report, so an ASK resource still goes through its existing
//  confirmation-and-fingerprint path and an AUTO_SAFE one still goes through
//  the registered-action path. A plan item carries no `fingerprintToken` at
//  all (asserted on the wire in `DtoGoldenFixturesTests
//  .testLlmPlanItemsCarryNoFingerprintToken`), so a recommendation cannot
//  arrive pre-consented, and there is no second enablement path for one to
//  travel down. No string a model produced reaches an argument array: the
//  only value this file forwards is `candidate.resourceId`, which Rust set
//  from its own discovered evidence.
//
//  ---------------------------------------------------------------------
//  Nothing is sent anywhere until the user asks
//  ---------------------------------------------------------------------
//  `llm-plan` is invoked from exactly one place below, called only from the
//  "Ask AI for a plan" button's action. No `.onAppear`, no `.task`, no
//  `Timer`, no retry loop — a paid network call must never be a side effect
//  of opening a popover. `AiPlanSectionViewTests` greps this file for exactly
//  that invariant.
//
//  ---------------------------------------------------------------------
//  Why this reads the raw result instead of `client.run`
//  ---------------------------------------------------------------------
//  `llm-plan` exits 1 when the provider call itself failed — but it prints
//  the complete JSON report, `provider_error` and all, to stdout FIRST. A
//  `client.run` call would map that exit code to `executionFailed` and throw
//  the body away, losing the only structured account of what went wrong. So
//  this uses `runRaw` and maps the exit code itself, in `AiPlanInterpretation`
//  below — which is a pure function precisely so the five outcomes can be
//  asserted without rendering SwiftUI or spawning anything.
//

import SwiftUI

/// What one `llm-plan` invocation turned out to be.
///
/// Five cases rather than "a report or an error", because four of them need
/// materially different copy and exactly one of them is the user's problem to
/// fix:
///
///   - `plan` is the normal answer, and it carries a non-`nil`
///     `providerError` when the provider failed mid-flight — Rust still
///     reports what it managed, and so does this card;
///   - `notConfigured` is not a failure at all. Nothing was sent, nothing was
///     charged, and the remedy is a setting;
///   - `malformedOutput` means the CLI exited 0 with something this app
///     cannot decode, which is a version skew between the app and the
///     `glomeris` on `PATH` — a different problem from the provider's;
///   - `failed` is everything else, carrying the CLI's own stderr rather than
///     a sentence invented here.
enum AiPlanOutcome: Equatable {
    case plan(LlmPlanReportDto)
    case notConfigured
    case malformedOutput
    case failed(String)
}

/// Pure, directly-testable mapping from `llm-plan`'s exit code and streams to
/// an `AiPlanOutcome`.
///
/// The exit-code contract is `run_llm_plan_command`'s, read from the source
/// rather than assumed (`src/main.rs`):
///
///   * **0** — the report was printed to stdout;
///   * **1** — `provider_error` was set; the report was STILL printed to
///     stdout first, then the process exited 1. Decoding stdout is what keeps
///     that account;
///   * **2** — either missing LLM configuration (a specific sentence on
///     stderr) or a usage error. The two are told apart by that sentence,
///     because only one of them is worth showing the user a settings prompt
///     for; a usage error means this app sent arguments the installed
///     `glomeris` does not understand, which is the app's bug, not theirs.
enum AiPlanInterpretation {
    /// A substring of the CLI's own exit-2 sentence, matched rather than
    /// reproduced in full: the full sentence names three environment
    /// variables and will grow a fourth, and a guard that breaks on rewording
    /// would silently reclassify "no provider configured" as a hard failure.
    static let missingConfigurationMarker = "missing LLM configuration"

    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> AiPlanOutcome {
        let stderrText = Self.trimmedText(stderr)

        if exitCode == 2 {
            if stderrText.contains(Self.missingConfigurationMarker) {
                return .notConfigured
            }
            return .failed(
                stderrText.isEmpty
                    ? "glomeris rejected the arguments this app sent (exit 2)."
                    : stderrText
            )
        }

        // Tried before the exit code is judged, because exit 1 prints a full
        // report and throwing it away would discard the only structured
        // description of the provider failure.
        if let report = try? JSONDecoder().decode(LlmPlanReportDto.self, from: stdout) {
            return .plan(report)
        }

        if exitCode == 0 {
            return .malformedOutput
        }

        return .failed(
            stderrText.isEmpty
                ? "glomeris llm-plan exited with code \(exitCode) and printed no plan."
                : stderrText
        )
    }

    private static func trimmedText(_ data: Data) -> String {
        String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Pure, directly-testable mapping from a request's state to the one message
/// shown instead of — or, for a partial answer, above — the rows.
///
/// Extracted from the view because AC 7 of HORO-1308 is a claim about six
/// distinct situations having distinct, correct presentations, and a claim like
/// that is worth asserting rather than eyeballing. In particular the two
/// non-failures are easy to get wrong and expensive to get wrong:
///
///   * having never asked is not a finding, so it must not wear `empty`'s
///     checkmark — nobody has earned a clean bill of health;
///   * having no provider configured is not an error at all. Nothing was sent,
///     nothing was charged, and dressing it in a red triangle tells a user
///     something is broken when the answer is "this is opt-in".
///
/// `AiPlanSectionViewTests` pins both, and pins that a provider failure and a
/// version skew DO read as failures.
enum AiPlanStateMessages {
    /// `nil` means "the rows are the whole answer" — the only case with
    /// nothing to say.
    static func message(for outcome: AiPlanOutcome?, isPlanning: Bool) -> GlomerisStateMessage? {
        guard let outcome else {
            if isPlanning {
                return .loading("Asking your AI provider for a plan…")
            }
            return .notLookedYet(
                "No plan yet",
                detail: "Nothing has been sent anywhere. Ask for a plan when you want one."
            )
        }

        switch outcome {
        case .plan(let report):
            if let providerError = report.providerError {
                // Verbatim from Rust. This is the report the CLI printed
                // before exiting 1, which is exactly why it is worth reading.
                //
                // Checked before the empty case, and the ordering is load-
                // bearing: a failed provider call usually also yields zero
                // items, and "No suggestions" would report that as a
                // considered answer rather than as a call that did not
                // complete.
                return .failure(
                    "Your AI provider did not return a usable plan: \(providerError)"
                )
            }
            if report.items.isEmpty {
                return .empty(
                    "No suggestions",
                    detail: "Your provider answered, but had nothing to propose for what "
                        + "Glomeris found."
                )
            }
            return nil

        case .notConfigured:
            // The environment note is here because it is the single most
            // likely reason a user who *has* configured a provider in their
            // shell still lands here: a menu-bar agent launched from Finder
            // inherits no shell environment. HORO-1309 made that fixable
            // without a terminal, and the view renders a Settings button
            // beside this message — a message cannot carry a control.
            return .notLookedYet(
                "No AI provider configured",
                detail: "Glomeris found no provider settings, so it sent nothing. Set one up in "
                    + "Settings — note that this app does not inherit your shell environment, "
                    + "so variables exported in a terminal are not visible here."
            )

        case .malformedOutput:
            return .failure(
                "The installed glomeris reported success but this app could not read its "
                    + "plan. The app and the CLI are probably different versions."
            )

        case .failed(let message):
            return .failure(message)
        }
    }
}

/// Pure formatting step from one plan item to what a row renders.
///
/// `machineVerdictLines` is the interesting one: it is derived ONLY from
/// `skipReason`, `candidate.executable`, `candidate.refusalReason` and
/// `candidate.offeredActions[].requiresConfirmation` — the structured fields
/// that actually gate behaviour — and never from the policy label's text or
/// from anything the model said. `modelReason` sits beside it as a quotation
/// and has no influence on it whatsoever.
struct AiPlanRowViewModel: Equatable {
    /// Taken from `candidate.resourceId` rather than the item's own copy.
    /// The two are equal by construction in `build_llm_plan_report`, and both
    /// come from local discovery rather than from the model — but the
    /// candidate projection is the thing this app treats as authoritative
    /// everywhere else, so it is the one read here too.
    let resourceId: String

    let kindTerm: GlomerisTerm
    let impactTerm: GlomerisTerm
    let impactTierTerm: GlomerisTerm?
    let safetyTerm: GlomerisTerm

    /// The third axis from this campaign's design principles, and the reason
    /// the AI Plan card can show it when the candidates list cannot: a plan
    /// item carries `completeness`/`confidence` per row, whereas
    /// `detect --json` does not. Rendered unfilled and on its own line so it
    /// reads as "how well do we know this", never as part of the safety
    /// verdict — high confidence in a PROTECTED classification does not make
    /// the resource any less protected.
    let completenessTerm: GlomerisTerm
    let confidenceTerm: GlomerisTerm

    /// The provider's sentence, or `nil` when it gave none. Already bounded
    /// and control-character-stripped in Rust.
    let modelReason: String?

    /// What Glomeris itself says about acting on this suggestion, in reading
    /// order. Usually one line; two for a refusal that has both an
    /// item-level skip reason and a candidate-level reason code, because
    /// those say different things — "no action was rendered at all" versus
    /// "here is the policy reason" — and the refusal is the row that can
    /// least afford to be summarised.
    let machineVerdictLines: [String]

    /// One sentence for VoiceOver, because the row is a single button.
    ///
    /// Ordered deliberately: what it is, what Glomeris will permit, how much
    /// is at stake, how well it is known, and only THEN what the model said,
    /// explicitly attributed. A screen-reader user hears the machine's
    /// verdict before the model's opinion, which is the same priority a
    /// sighted user gets from the badges sitting above the quotation.
    var accessibilityLabel: String {
        var label = "\(kindTerm.title). \(safetyTerm.axis): \(safetyTerm.title). "
            + "\(impactTerm.axis): \(impactTerm.title)."
        if let impactTierTerm {
            label += " \(impactTierTerm.title)."
        }
        label += " \(completenessTerm.axis): \(completenessTerm.title). "
            + "\(confidenceTerm.axis): \(confidenceTerm.title)."
        for line in machineVerdictLines {
            label += " \(line)"
        }
        if let modelReason {
            label += " The model's reason, which is advice and not a verdict: \(modelReason)"
        }
        return label + " Path: \(resourceId)."
    }

    init(_ dto: LlmPlanItemReportDto) {
        let candidate = dto.candidate
        resourceId = candidate.resourceId
        kindTerm = GlomerisVocabulary.kind(candidate.kind)
        impactTerm = GlomerisVocabulary.storageImpact(
            human: candidate.reclaimableHuman,
            isLowerBound: candidate.reclaimableBytesIsLowerBound
        )
        impactTierTerm = GlomerisVocabulary.impactTier(candidate.impactTier)
        safetyTerm = GlomerisVocabulary.safety(candidate.policyLabel)
        completenessTerm = GlomerisVocabulary.completeness(dto.completeness)
        confidenceTerm = GlomerisVocabulary.confidence(dto.confidence)
        modelReason = dto.modelReason
        machineVerdictLines = Self.verdictLines(dto)
    }

    /// Built from the gating fields alone. `skipReason` and `refusalReason`
    /// are passed through verbatim — this layer never rewords a refusal.
    ///
    /// HORO-1323 moved the four sentences out to `CandidateActionability`,
    /// unchanged. They were written here, but the candidates list and the
    /// Clean button needed the same wording, and three copies of "what
    /// Glomeris will do about this resource" is three chances for them to
    /// disagree about it. What stays here is the part that is genuinely this
    /// card's own: a plan item can also carry a `skip_reason`, which is the
    /// planner explaining why it rendered no action at all, and that is a
    /// different statement from the candidate's refusal reason.
    private static func verdictLines(_ dto: LlmPlanItemReportDto) -> [String] {
        var lines: [String] = []
        if let skipReason = dto.skipReason {
            lines.append(skipReason)
        }

        let actionability = CandidateActionability(
            executable: dto.candidate.executable,
            offeredActions: dto.candidate.offeredActions,
            refusalReason: dto.candidate.refusalReason
        )

        switch actionability {
        case .refusedWithoutStatedReason:
            // Behaviour preserved on every input the CLI can produce: when the
            // CLI gave no reason of its own but the planner did explain itself,
            // the generic sentence adds nothing and is left off. Unreachable in
            // practice — every branch of `executable_fields` returns a reason —
            // but changing it silently while refactoring would be a behaviour
            // change smuggled in as a tidy-up, so
            // `testASkipReasonSuppressesTheGenericSentence` pins it.
            //
            // Two inputs Rust cannot emit do differ from the pre-HORO-1323
            // code, both in the direction of saying less: a `skip_reason` byte
            // -identical to one of the permitted sentences is no longer
            // duplicated, and `refusal_reason: Some("")` no longer produces a
            // blank verdict line.
            if lines.isEmpty {
                lines.append(actionability.sentence)
            }
        case .readyToClean, .asksFirstThenCleans, .refused:
            if !lines.contains(actionability.sentence) {
                lines.append(actionability.sentence)
            }
        }

        return lines
    }
}

/// Popover section: an explicitly-requested AI plan, rendered so the model's
/// advice and Glomeris's verdict can never be confused. No polling, no timer,
/// no first-appearance call — see file header.
struct AiPlanSectionView: View {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    /// Read at spawn time, not at init time, so a provider configured in the
    /// Settings window takes effect on the next Ask without reopening the
    /// popover (HORO-1309). Holding the resolved environment here instead
    /// would cache a credential for the lifetime of a view.
    private let settingsStore: GlomerisLlmSettingsStore

    /// HORO-1365: the plan this card renders is NOT owned here. A plan the
    /// user paid a provider for must outlive any drill-down into one of the
    /// resources it mentions, and `@State` on this view could not promise
    /// that — its lifetime is the view's place in the hierarchy, which the
    /// panel's detail navigation decides. See `OverviewState.swift`.
    ///
    /// This card is still the only thing that writes it, and still only from
    /// the Ask button below.
    @ObservedObject private var plan: PlanState

    /// What a row tap does: report the suggested resource upward, so the
    /// popover shell shows the same detail view a candidates-list row leads
    /// to (HORO-1357). It is the only thing a row tap ever does; no action
    /// runs from this list.
    ///
    /// This was a `.sheet(item:)` presented from this card. Both entry points
    /// into the detail view now go through the shell, so there is exactly one
    /// presentation of it to get right — and a sheet, which on a
    /// `MenuBarExtra(.window)` panel can order the panel out when it appears
    /// or resizes, is no longer any part of it.
    private let onOpenDetail: (String) -> Void

    /// `plan` has deliberately NO default, for the reason given on
    /// `CandidatesSectionView.init`: a defaulted `PlanState()` is a silent
    /// route back to "No plan yet" over a plan that exists.
    init(
        plan: PlanState,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore(),
        settingsStore: GlomerisLlmSettingsStore = GlomerisLlmSettingsStore(),
        onOpenDetail: @escaping (String) -> Void = { _ in }
    ) {
        self.plan = plan
        self.client = client
        self.projectRootsStore = projectRootsStore
        self.settingsStore = settingsStore
        self.onOpenDetail = onOpenDetail
    }

    var body: some View {
        GlomerisCard(title: "AI Plan", trailing: countText) {
            controlRow
            provenanceNote

            if plan.isPlanning, let progressStatusText = plan.progressStatusText {
                Text(progressStatusText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }

            outcomeBody

            // Additive, never a replacement — the same rule the status and
            // candidates cards follow. A failed request leaves an earlier
            // plan visible, which is useful, but the freshness stamp above
            // still says when that plan was actually made.
            if let lastErrorMessage = plan.lastErrorMessage {
                GlomerisStateMessageView(message: .failure(lastErrorMessage))
            }
        }
    }

    // MARK: - Header

    private var controlRow: some View {
        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button(plan.isPlanning ? "Asking…" : "Ask AI for a plan") {
                // Set here as well as in `runLlmPlan`, for the reason given on
                // the Refresh button — and with a worse consequence if the
                // window is hit: a second tap would overwrite `planTask`,
                // orphaning the first request where Stop can no longer reach it,
                // and paying a provider twice for one question.
                plan.isPlanning = true
                plan.planTask = Task { await runLlmPlan() }
            }
            // HORO-1366 adds `isApplyingBatch`. A new plan replaces the one a
            // batch is running, and a batch cannot be stopped — so asking mid-
            // batch would leave a result to be rendered under a plan it was not
            // about, and let a second Apply start beside the first. Refusing the
            // question for the few seconds deletions take is the honest answer;
            // `applyPlan`'s own latch is the backstop, not the gate.
            .disabled(plan.isPlanning || plan.isApplyingBatch)

            if plan.isPlanning {
                Button("Stop") {
                    plan.planTask?.cancel()
                }
            }

            Text(lastPlannedText)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)

            Spacer(minLength: 0)
        }
    }

    /// Always visible, including before anything has been asked for: it is
    /// the standing rule of the product, not a caption on a result. Kept to
    /// two short lines because a paragraph here would be scrolled past, and
    /// the second line is the one a first-time user needs — that pressing the
    /// button is what sends anything anywhere.
    ///
    /// HORO-1367 shortened the second line. Every fact it carried is still
    /// here — that asking sends a summary, that it goes to the user's own
    /// provider, that it may cost money, and that Settings shows *exactly*
    /// what would be sent without sending it — because each one is a thing
    /// the user would be wronged by not knowing. What went was the wording
    /// around them: at the old width this line wrapped to three lines of
    /// tertiary caption directly above the button it describes, which is the
    /// shape a reader skips.
    ///
    /// "Shows exactly what would be sent" is the one phrase here that must
    /// not be paraphrased. It was briefly shortened to "previews it", which
    /// is true of a summary, a sample or an approximation as well — and the
    /// claim being made is the stronger one, that what Settings displays is
    /// the payload itself. On a product whose case rests on showing its
    /// evidence, trading that for four words is the wrong trade.
    private var provenanceNote: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("The model recommends. Glomeris decides what may run.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(
                "Asking sends a summary of what Glomeris found to your provider "
                    + "and may cost money. "
                    + "Settings shows exactly what would be sent, without sending it."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.tertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var lastPlannedText: String {
        guard let lastPlannedAt = plan.lastPlannedAt else { return "not asked yet" }
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .medium
        return "asked \(formatter.string(from: lastPlannedAt))"
    }

    /// Beside the card title, and only once there is a plan with rows in it —
    /// "0 suggestions" next to a message that already says so in words is
    /// noise.
    private var countText: String? {
        guard case .plan(let report) = plan.outcome, !report.items.isEmpty else { return nil }
        return report.items.count == 1 ? "1 suggestion" : "\(report.items.count) suggestions"
    }

    // MARK: - Body states

    /// A message and rows are not mutually exclusive here, unlike in the
    /// candidates card: a provider that failed partway can still have returned
    /// usable suggestions, and `llm-plan` reports both. So the message comes
    /// first and the rows follow if there are any, rather than one replacing
    /// the other.
    @ViewBuilder
    private var outcomeBody: some View {
        if let message = AiPlanStateMessages.message(for: plan.outcome, isPlanning: plan.isPlanning) {
            GlomerisStateMessageView(message: message)
        }

        // HORO-1309: the one state with a remedy the user can act on from
        // here. Rendered as a control beside the message rather than as more
        // words inside it, because `AiPlanStateMessages` is a pure token->copy
        // mapper and putting a button in it would give the message-building
        // layer the ability to act.
        if plan.outcome == .notConfigured {
            GlomerisSettingsButton(title: "Set up an AI provider…")
        }

        if case .plan(let report) = plan.outcome {
            if !report.items.isEmpty {
                orderingNote
                rows(report.items)
            }

            if let droppedText = Self.droppedText(report) {
                Divider()
                Text(droppedText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            // HORO-1366. Embedded, not implemented here: this card renders what
            // a provider said and must stay a surface that cannot delete
            // anything — its tests assert its source contains no `execute`, no
            // `performClean` and no fingerprint token, and `ApplyPlanView` is a
            // sibling precisely so those guards keep holding. Below the rows
            // because a user should read the suggestions before being offered a
            // way to act on all of them, and the decision about whether to offer
            // one at all is made inside it from the items themselves.
            ApplyPlanView(
                plan: plan,
                items: report.items,
                client: client,
                projectRootsStore: projectRootsStore
            )
        }
    }

    /// Whose order this is. One line, above the rows, because the alternative
    /// — numbering the rows — would state the opposite of the truth in the
    /// app's own typography for machine judgments. See file header.
    private var orderingNote: some View {
        // HORO-1367: "The model's order" says what "In the order the model
        // suggested" said, in a third of the words. The second clause is the
        // load-bearing half and is untouched.
        Text("The model's order. Glomeris's own ranking is the list above.")
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.tertiary)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// The suggestions Rust refused to validate, stated rather than swallowed.
    ///
    /// Pure and `static` so the wording is assertable without a view. A plan
    /// with three rows and two silently discarded items is not a three-row
    /// plan, and a user deciding how much to trust a provider is entitled to
    /// know it asked for things that do not exist.
    static func droppedText(_ report: LlmPlanReportDto) -> String? {
        var parts: [String] = []
        if report.droppedUnknownResource > 0 {
            parts.append(
                "\(report.droppedUnknownResource) named a resource Glomeris never found"
            )
        }
        if report.droppedUnknownAction > 0 {
            parts.append(
                "\(report.droppedUnknownAction) asked for an action Glomeris does not have"
            )
        }
        guard !parts.isEmpty else { return nil }

        let total = report.droppedUnknownResource + report.droppedUnknownAction
        let subject = total == 1 ? "1 suggestion was" : "\(total) suggestions were"
        return "\(subject) discarded before reaching this list: \(parts.joined(separator: ", "))."
    }

    /// Rows are keyed by position, not by `resourceId`: a provider may name
    /// the same resource twice, and `ForEach` over duplicate identities
    /// misrenders. Position is also the honest identity here, since the order
    /// is the model's.
    @ViewBuilder
    private func rows(_ items: [LlmPlanItemReportDto]) -> some View {
        ForEach(Array(items.enumerated()), id: \.offset) { index, item in
            if index > 0 {
                Divider()
            }
            rowView(item)
        }
    }

    /// One suggestion.
    ///
    /// Reading order is machine-first, model-last, and the two are visually
    /// unrelated: badges and plain statements for what Glomeris determined, a
    /// ruled and italic quotation for what the provider said. The quotation
    /// is deliberately NOT a `GlomerisBadgeView` — a chip is this app's
    /// grammar for "a verdict was reached", and putting a model's sentence in
    /// one would launder an opinion into a finding.
    @ViewBuilder
    private func rowView(_ item: LlmPlanItemReportDto) -> some View {
        let row = AiPlanRowViewModel(item)

        Button {
            // By resource id, into the same detail view a candidates-list row
            // leads to. That view makes its own `explain` call and reads its
            // own `executable`/`requiresConfirmation`/`fingerprintToken`, so
            // the plan cannot shortcut consent — see file header.
            onOpenDetail(item.candidate.resourceId)
        } label: {
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                    Text(row.kindTerm.title)
                        .font(GlomerisDesign.primaryFont)
                        .lineLimit(1)
                    Spacer(minLength: GlomerisDesign.inlineSpacing)
                    GlomerisBadgeView(term: row.impactTerm, filled: false)
                }

                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    GlomerisBadgeView(term: row.safetyTerm)
                    if let impactTierTerm = row.impactTierTerm {
                        GlomerisBadgeView(term: impactTierTerm)
                    }
                    Spacer(minLength: 0)
                    // Affordance only — it carries no state, so it is hidden
                    // from VoiceOver rather than read out once per row.
                    Image(systemName: "chevron.right")
                        .imageScale(.small)
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }

                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    GlomerisBadgeView(term: row.completenessTerm, filled: false)
                    GlomerisBadgeView(term: row.confidenceTerm, filled: false)
                    Spacer(minLength: 0)
                }

                ForEach(Array(row.machineVerdictLines.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if let modelReason = row.modelReason {
                    modelQuote(modelReason)
                }

                GlomerisPathText(path: row.resourceId)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // HORO-1363: deliberately NO `.accessibilityElement(children: .ignore)`
        // here — same reason as the candidates-list row. `.ignore` replaced
        // this `Button` with a plain container in the live tree, so the row
        // read as `AXUnknown` with no actions and a VoiceOver user had no way
        // to open the suggestion. The explicit label below already collapses
        // the row to one element without discarding the role.
        .accessibilityLabel(row.accessibilityLabel)
        .accessibilityHint("Opens the evidence and the available actions for this resource.")
    }

    /// The provider's sentence, attributed and set apart: a leading rule, a
    /// `sparkles` label naming who said it, and italic secondary text. Three
    /// signals rather than one, so the attribution survives a user who cannot
    /// distinguish italics, a narrow popover that clips the rule, and
    /// VoiceOver — which gets the attribution in words from the row's own
    /// label.
    @ViewBuilder
    private func modelQuote(_ reason: String) -> some View {
        HStack(alignment: .top, spacing: GlomerisDesign.inlineSpacing) {
            RoundedRectangle(cornerRadius: 1)
                .fill(.tertiary)
                .frame(width: 2)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 3) {
                    Image(systemName: "sparkles")
                        .imageScale(.small)
                    Text("The model says")
                        .font(GlomerisDesign.badgeFont)
                }
                .foregroundStyle(.secondary)
                Text(reason)
                    .font(GlomerisDesign.captionFont)
                    .italic()
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    // MARK: - The one call site

    /// The one and only place `llm-plan` is invoked in this file — always
    /// with `--json --progress-json`, always in direct response to the "Ask
    /// AI for a plan" button's action closure above, never from an
    /// appear-triggered task, a `Timer`, or a retry loop. A network call that
    /// may cost money must be something the user did, not something the
    /// popover did.
    ///
    /// `runRaw` rather than `run` on purpose: exit 1 still carries a full
    /// report on stdout. See the file header and `AiPlanInterpretation`.
    ///
    /// `@MainActor` for the same reason as `CandidatesSectionView.runDetect()`
    /// — read that doc comment for what the annotation does and does not claim
    /// — and with one extra beneficiary here: `plan.planTask` is a plain `var`
    /// on a shared object, written from this function and from the Ask button
    /// and read by Stop. With all three on the main actor there is no window in
    /// which a finishing request nils the handle while Stop is reading it, which
    /// would have cancelled nothing and left a child `glomeris llm-plan` running
    /// and a provider request still billable.
    ///
    /// `internal` rather than `private` so the tests can drive it against a
    /// pinned fixture binary. Its one production call site is still the Ask
    /// button.
    @MainActor
    func runLlmPlan() async {
        plan.isPlanning = true
        plan.progressStatusText = nil
        plan.lastErrorMessage = nil
        // HORO-1366: a preview or a result belongs to the plan it was made
        // from. Asking again replaces that plan, so anything the previous one
        // was going to do — or had done — stops being on screen with it. Left
        // behind, a preview would go on naming resources from a list the user
        // can no longer see, and its Apply button would still be live.
        plan.resetApplyState()

        do {
            // `withEnvironment` is what makes the Settings window's provider
            // configuration reach the CLI: this app is `LSUIElement` and, when
            // launched from Finder, inherits no shell environment at all, so
            // before HORO-1309 the documented `GLOMERIS_LLM_*` setup silently
            // did nothing here. `childEnvironment` is a COMPLETE environment,
            // not an overlay — `Process.environment` replaces wholesale, and
            // dropping PATH would break executable resolution.
            let raw = try await client
                .withEnvironment(settingsStore.childEnvironment())
                .runRaw(
                    ["llm-plan", "--json", "--progress-json"]
                        + projectRootsStore.commandLineArguments,
                    progressType: ProgressEventDto.self,
                    onProgress: { event in
                        Task { @MainActor in
                            plan.progressStatusText = ProgressStatusText.text(for: event)
                        }
                    }
                )
            plan.outcome = AiPlanInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
            plan.lastPlannedAt = Date()
        } catch {
            // `nil` for a cancellation, and a sentence for everything else.
            // A stopped request deliberately leaves `lastPlannedAt` alone, so
            // any plan still on screen keeps its real timestamp instead of
            // claiming to be current.
            plan.lastErrorMessage = SectionFetchErrors.shortMessage(error, subject: "AI plan")
        }

        plan.isPlanning = false
        plan.progressStatusText = nil
        plan.planTask = nil
    }
}

#Preview {
    AiPlanSectionView(plan: PlanState())
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
