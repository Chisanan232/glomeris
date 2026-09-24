//
//  ApplyPlanView.swift
//  GlomerisMenuBar
//
//  HORO-1366: reviewing a plan and applying it, without the plan gaining any
//  authority by being applied.
//
//  ---------------------------------------------------------------------
//  Why this is a file of its own and not part of AiPlanSectionView
//  ---------------------------------------------------------------------
//  That card's tests assert three absences in its source: no `"execute"`, no
//  `performClean`, no `fingerprintToken`. Those guards are worth keeping
//  exactly as they are — the card that renders a provider's advice should still
//  be a surface that cannot delete anything, and a reader who wants to know
//  whether the AI Plan rows can execute should be able to establish it by
//  reading one file.
//
//  So the batch is a sibling the card embeds, with its own guards in
//  `ApplyPlanViewTests`, and the concerns stay separable: `AiPlanSectionView`
//  shows what the model said, this file shows what Glomeris will do about it.
//
//  ---------------------------------------------------------------------
//  The two call sites, and why there is no third
//  ---------------------------------------------------------------------
//  `preparePreview()` runs `explain` once per plan item. `applyPlan()` runs
//  `execute` once per authorised step, sequentially. There is no batch
//  subcommand in the CLI and this file deliberately does not ask for one:
//
//    * `autopilot run` is the wrong shape despite the name. It requires a
//      standing enabled envelope with kind allowlists and pre-authorised ASK
//      reasons, so applying one reviewed plan through it would mean either
//      making the user enable Autopilot first or writing a temporary envelope
//      behind their back — granting standing authority as a side effect of a
//      one-shot action. It also re-runs its own correlation instead of
//      consuming a reviewed plan, and its report is `Display`-only, so there
//      would be nothing to render per item.
//
//    * A new Rust `apply-plan` would be a second mutation path to audit. The
//      loop below is N invocations of the same `execute` a single-item Clean
//      makes, which means every one of `executor::execute`'s eight
//      pre-mutation checks — identity snapshot, fresh evidence, fingerprint
//      match, policy-class change, widened reasons, degraded completeness,
//      re-plan, structural refusal, and `verify_identity_unchanged` immediately
//      before each syscall — applies to every item, unchanged and without
//      anything here having to know they exist. AC4's "the same execution
//      invariants as single-item Clean" is then true by construction.
//
//  `buildExecuteArguments` is the same pure builder `CandidateDetailView` uses;
//  this file calls it and does not build an argument array of its own.
//
//  ---------------------------------------------------------------------
//  Cancelling, and the one place it is safe
//  ---------------------------------------------------------------------
//  `GlomerisClient.runRaw`'s header sets the rule: `execute` must never be
//  reachable from a cancellable task, because a `SIGTERM` partway through a
//  deletion leaves the filesystem in a state neither this app nor the audit log
//  could describe. So:
//
//    * the `explain` sweep IS cancellable — it observes, and interrupting one
//      loses nothing but the answer — and has a Stop button;
//    * the `execute` loop is launched from a button action in a detached
//      `Task {}`, never from `.task {}`, which SwiftUI cancels on disappear,
//      and no handle to it is kept anywhere (see `PlanState`);
//    * the cancel the ticket asks for is in the review step, before anything
//      has run, where cancelling costs nothing.
//
//  ---------------------------------------------------------------------
//  Why the review is in the panel and not an alert
//  ---------------------------------------------------------------------
//  HORO-1357: `MenuBarExtra(.window)` is a non-activating panel, and
//  presenting a modal over one can order the panel out from under it — that
//  ticket's whole symptom. An enumerated in-panel review is also the stronger
//  consent surface: a user reading a list of named resources with their sizes
//  and a separate opt-in for the ones that ask first is better informed than
//  one clicking OK on a sentence.
//
//  ---------------------------------------------------------------------
//  No provider environment reaches a child process here
//  ---------------------------------------------------------------------
//  `AiPlanSectionView.runLlmPlan()` wraps its client in
//  `withEnvironment(settingsStore.childEnvironment())`, because `llm-plan` is
//  the call that talks to a provider. Neither `explain` nor `execute` does, so
//  this file uses the client as handed to it and never touches the settings
//  store. An API key is therefore absent from the environment of every child
//  process a batch spawns, and `ApplyPlanViewTests` asserts that absence.
//

import SwiftUI

struct ApplyPlanView: View {
    @ObservedObject private var plan: PlanState
    private let items: [LlmPlanItemReportDto]
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    init(
        plan: PlanState,
        items: [LlmPlanItemReportDto],
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.plan = plan
        self.items = items
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            Divider()
            switch plan.applyPhase {
            case .idle:
                idleBody
            case .preparing:
                preparingBody
            case .reviewing(let preview):
                reviewBody(preview)
            case .applying(let preview):
                applyingBody(preview)
            case .finished(let result):
                resultBody(result)
            }
        }
    }

    // MARK: - Idle

    /// AC1 and AC7 together: one entry point when the plan holds something
    /// actionable, and no destructive-sounding control when it does not.
    ///
    /// The ellipsis is load-bearing. "Apply plan…" promises further UI, which
    /// is what it does — it re-checks every suggestion and shows what would
    /// run. Nothing is deleted by pressing it, and a title without the
    /// ellipsis would claim otherwise.
    @ViewBuilder
    private var idleBody: some View {
        if PlanApplicationPreview.snapshotSuggestsAnApplicableItem(items) {
            Button("Apply plan…") {
                plan.preparePreviewTask = Task { await preparePreview() }
            }
            .accessibilityLabel("Review and apply this plan")
            .accessibilityHint(
                "Re-checks every suggestion against Glomeris and shows what would run. "
                    + "Nothing is deleted until you confirm."
            )
            Text("Re-checks every suggestion first. Nothing runs until you confirm.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            // Not a dimmed Apply button: a disabled destructive control still
            // asserts that applying this plan is a thing that could happen,
            // and for a plan of refusals it is not. The rows above already
            // carry each resource's own reason (HORO-1323), so this line
            // points at them rather than repeating them.
            Text("Nothing in this plan can be cleaned. Each suggestion above says why.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - Preparing

    private var preparingBody: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            GlomerisStateMessageView(message: .loading("re-checking every suggestion"))
            if let applyProgressText = plan.applyProgressText {
                Text(applyProgressText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }
            Button("Stop") {
                plan.preparePreviewTask?.cancel()
            }
            .accessibilityLabel("Stop re-checking")
        }
    }

    // MARK: - Reviewing

    /// The preview. Reading order is what will happen, then how much space it
    /// is expected to free, then what will not happen and why, then the
    /// controls — so a user who stops reading early has still read the part
    /// that matters most.
    @ViewBuilder
    private func reviewBody(_ preview: PlanApplicationPreview) -> some View {
        let include = plan.applyIncludesConfirmable
        let attempted = preview.stepsToAttempt(includingConfirmable: include)

        Text("Review before applying")
            .font(GlomerisDesign.cardTitleFont)

        Text(Self.summaryText(preview, includingConfirmable: include))
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

        if let estimate = preview.reclaimableEstimateText(includingConfirmable: include) {
            // "About", and only where Rust reported a size. The measured figure
            // comes afterwards from `actual_reclaimed_bytes`; this one is an
            // estimate and says so.
            Text("About \(estimate) would be reclaimed.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }

        if !attempted.isEmpty {
            stepList("Will run", steps: attempted, showReason: false)
        }

        if !preview.confirmableSteps.isEmpty {
            // AC5's explicit confirmation, and it defaults to off. A single
            // affirmative control that names how many items it authorises and
            // that they ask first is consent the user gave for this batch; it
            // does not persist, because `resetApplyState()` clears it with
            // every new preview.
            Toggle(isOn: $plan.applyIncludesConfirmable) {
                Text(Self.confirmableToggleTitle(preview.confirmableSteps.count))
            }
            .font(GlomerisDesign.captionFont)
            .accessibilityHint(
                "These resources are only ever cleaned when you say so explicitly. "
                    + "Leave this off to skip them."
            )
            if !include {
                stepList("Asks first — not included", steps: preview.confirmableSteps, showReason: false)
            }
        }

        // AC6: every skipped item, with the CLI's own reason beside it.
        if !preview.skippedSteps.isEmpty {
            stepList("Will be skipped", steps: preview.skippedSteps, showReason: true)
        }

        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button(Self.applyButtonTitle(attempted.count)) {
                // A detached `Task {}` from a button action, and no handle
                // kept. See the file header: `execute` must not be reachable
                // from anything cancellable.
                Task { await applyPlan(preview) }
            }
            .disabled(attempted.isEmpty)
            .accessibilityHint("Runs the items listed above, one at a time.")

            Button("Cancel") {
                plan.resetApplyState()
            }
            .accessibilityLabel("Cancel without applying anything")

            Spacer(minLength: 0)
        }

        if attempted.isEmpty {
            Text("Nothing is selected to run.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Applying

    @ViewBuilder
    private func applyingBody(_ preview: PlanApplicationPreview) -> some View {
        let total = preview.stepsToAttempt(includingConfirmable: plan.applyIncludesConfirmable).count
        GlomerisStateMessageView(message: .loading("applying \(total == 1 ? "1 item" : "\(total) items")"))

        if let applyProgressText = plan.applyProgressText {
            Text(applyProgressText)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }

        // No Stop button, and the absence is deliberate — see the file header.
        // Saying so is better than a user hunting for one.
        Text("Deletions cannot be interrupted safely, so this runs to the end.")
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.tertiary)
            .fixedSize(horizontal: false, vertical: true)

        if !plan.applyCompletedItems.isEmpty {
            resultRows(plan.applyCompletedItems)
        }
    }

    // MARK: - Finished

    @ViewBuilder
    private func resultBody(_ result: PlanApplicationResult) -> some View {
        if result.isCompleteSuccess {
            GlomerisStateMessageView(message: .success(Self.resultHeadline(result)))
        } else {
            // Not `.failure`: a batch that stopped at a PROTECTED resource is
            // Glomeris working, and the user has nothing to fix. Colouring it
            // as an error would teach them the product breaks every time it
            // correctly declines — the same reason `CandidateActionability`
            // uses `.guarded` rather than `.critical`.
            Text(Self.resultHeadline(result))
                .font(GlomerisDesign.primaryFont)
                .fixedSize(horizontal: false, vertical: true)
        }

        if let stoppedEarlyReason = result.stoppedEarlyReason {
            Text("The batch stopped: \(stoppedEarlyReason)")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }

        resultRows(result.items)

        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button("Done") {
                plan.resetApplyState()
            }
            .accessibilityLabel("Dismiss this result")
            Spacer(minLength: 0)
        }
    }

    // MARK: - Shared row rendering

    /// Keyed by position, not by resource id: a provider may name the same
    /// resource twice, and `ForEach` over duplicate identities misrenders. The
    /// AI Plan card's rows are keyed the same way for the same reason.
    @ViewBuilder
    private func stepList(_ title: String, steps: [PlanApplicationStep], showReason: Bool) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(GlomerisDesign.badgeFont)
                .foregroundStyle(.secondary)
            ForEach(Array(steps.enumerated()), id: \.offset) { _, step in
                VStack(alignment: .leading, spacing: 1) {
                    HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                        Text(step.kindTerm.title)
                            .font(GlomerisDesign.captionFont)
                            .lineLimit(1)
                        Spacer(minLength: GlomerisDesign.inlineSpacing)
                        Text(step.reclaimableText)
                            .font(GlomerisDesign.captionFont)
                            .foregroundStyle(.secondary)
                    }
                    if showReason, let reason = step.disposition.skipReason {
                        Text(reason)
                            .font(GlomerisDesign.captionFont)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    GlomerisPathText(path: step.resourceId)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(Self.stepAccessibilityLabel(step, group: title))
            }
        }
    }

    @ViewBuilder
    private func resultRows(_ items: [PlanApplicationItemResult]) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                VStack(alignment: .leading, spacing: 1) {
                    Text(item.kindTerm.title)
                        .font(GlomerisDesign.captionFont)
                        .lineLimit(1)
                    Text(item.status.message)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    GlomerisPathText(path: item.resourceId)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(Self.resultAccessibilityLabel(item))
            }
        }
    }

    // MARK: - Wording
    //
    // `static` and pure so every sentence below is assertable without building
    // a view — the same reason `AiPlanSectionView.droppedText` is.

    static func summaryText(_ preview: PlanApplicationPreview, includingConfirmable: Bool) -> String {
        var parts: [String] = []
        let willRun = preview.stepsToAttempt(includingConfirmable: includingConfirmable).count
        parts.append(willRun == 1 ? "1 item will run" : "\(willRun) items will run")

        let confirmable = preview.confirmableSteps.count
        if confirmable > 0, !includingConfirmable {
            parts.append(
                confirmable == 1
                    ? "1 asks for confirmation and is not included"
                    : "\(confirmable) ask for confirmation and are not included"
            )
        }

        let skipped = preview.skippedSteps.count
        if skipped > 0 {
            parts.append(skipped == 1 ? "1 will be skipped" : "\(skipped) will be skipped")
        }
        return parts.joined(separator: ", ") + "."
    }

    static func confirmableToggleTitle(_ count: Int) -> String {
        count == 1
            ? "Also apply the 1 item that asks for confirmation"
            : "Also apply the \(count) items that ask for confirmation"
    }

    static func applyButtonTitle(_ count: Int) -> String {
        count == 1 ? "Apply 1 item" : "Apply \(count) items"
    }

    /// The honest headline, plus the measured total where there is one. The
    /// count sentence comes from `PlanApplicationResult.headline`, which cannot
    /// report a partial batch as a finished one (AC8), and the size is only
    /// ever appended to it — never shown instead of it.
    static func resultHeadline(_ result: PlanApplicationResult) -> String {
        guard let reclaimedText = result.reclaimedText else { return result.headline }
        return "\(result.headline) Reclaimed \(reclaimedText)."
    }

    static func stepAccessibilityLabel(_ step: PlanApplicationStep, group: String) -> String {
        var parts = [
            "\(group): \(step.kindTerm.title)",
            "\(GlomerisVocabulary.impactAxis): \(step.reclaimableText)",
            step.safetyTerm.accessibilityLabel,
            "\(CandidateActionability.axis): \(step.actionability.sentence)",
        ]
        parts.append("Path: \(step.resourceId)")
        return parts.joined(separator: ". ") + "."
    }

    static func resultAccessibilityLabel(_ item: PlanApplicationItemResult) -> String {
        "\(item.kindTerm.title). \(GlomerisVocabulary.outcomeAxis): \(item.status.message) "
            + "Path: \(item.resourceId)."
    }

    // MARK: - The `explain` sweep

    /// Re-checks every plan item against the CLI and assembles the preview.
    ///
    /// One `explain` per item, in the plan's order. Read-only, so this is the
    /// cancellable half — `GlomerisClient` turns a cancellation into a
    /// `SIGTERM` for the child and a `.cancelled` error, and
    /// `SectionFetchErrors.shortMessage` returns `nil` for exactly that case,
    /// which is how the loop below tells "the user pressed Stop" from "this
    /// resource can no longer be described".
    ///
    /// A failure for one item is not a failure of the preview: it becomes a
    /// `.skippedStale` step carrying the CLI's own message, so a plan holding
    /// one resource that has since been deleted still previews and still
    /// applies the rest — there is deliberately no wholesale "the preview
    /// failed" state, because every way the sweep can fail is a fact about one
    /// resource and belongs on that resource's row.
    ///
    /// `internal` so the tests can drive it against a fixture binary; its one
    /// production call site is the "Apply plan…" button.
    @MainActor
    func preparePreview() async {
        plan.applyCompletedItems = []
        plan.applyIncludesConfirmable = false
        plan.applyPhase = .preparing

        var steps: [PlanApplicationStep] = []
        for (index, item) in items.enumerated() {
            if Task.isCancelled { break }
            plan.applyProgressText = "Re-checking \(index + 1) of \(items.count)…"
            let resourceId = item.candidate.resourceId
            do {
                let result = try await client.run(
                    ["explain", resourceId, "--json"] + projectRootsStore.commandLineArguments,
                    outputType: ExplainReportDto.self,
                    progressType: ProgressEventDto.self
                )
                steps.append(PlanApplicationStep(explain: result.output))
            } catch {
                guard let reason = SectionFetchErrors.shortMessage(error, subject: "explain") else {
                    // `nil` means cancelled. Abandon the whole preview rather
                    // than present a partial one the user did not ask for and
                    // could not tell was partial.
                    plan.applyPhase = .idle
                    plan.applyProgressText = nil
                    return
                }
                steps.append(PlanApplicationStep(
                    staleResourceId: resourceId,
                    kind: item.candidate.kind,
                    policyLabel: item.candidate.policyLabel,
                    reason: reason
                ))
            }
        }

        if Task.isCancelled {
            plan.applyPhase = .idle
        } else {
            plan.applyPhase = .reviewing(PlanApplicationPreview(steps: steps))
        }
        plan.applyProgressText = nil
        // `plan.preparePreviewTask` is deliberately left alone. A finished
        // sweep clearing it would, in the one ordering that matters, clear a
        // *newer* sweep's handle: reset → new Apply plan → the old sweep
        // finally returns and nils the handle belonging to the run now in
        // flight, leaving nothing for the next reset to cancel. Cancelling an
        // already-finished task is a no-op, so a stale handle is harmless and
        // a missing one is not.
    }

    // MARK: - The `execute` loop

    /// Runs the authorised steps, one at a time, and assembles the result.
    ///
    /// Strictly sequential, because `~/Library/Application Support/Glomeris/
    /// execution.lock` is exclusive and non-blocking (`src/executor/lock.rs`):
    /// two concurrent `execute` children would have one of them refuse with
    /// `busy` for no reason but our own concurrency. When a `busy` refusal does
    /// arrive it means a *different* invocation holds the lock, so the batch
    /// stops — every remaining item would refuse identically, and a retry loop
    /// against a lock another process owns is not a fix.
    ///
    /// Every other outcome continues, which is the existing deterministic
    /// execution policy and not an instruction from anywhere: one resource
    /// being refused, aborted at revalidation or failing says nothing about the
    /// next, and stopping would silently narrow a batch the user authorised.
    ///
    /// No `try` escapes to the caller and no state is left mid-flight: the
    /// result is assembled from whatever the loop recorded, so a batch that
    /// stops still reports per item.
    ///
    /// Sequential *within* one run, which is only the whole story while there is
    /// one run: the loop has no cancellable handle by design, so nothing can
    /// stop a batch and a second one entered while the first awaits a child
    /// would put two `execute` processes in flight. `beginApplyingBatch()` is
    /// the compare-and-set that refuses that, and it is a latch on `PlanState`
    /// rather than a look at `applyPhase` because clearing the phase is
    /// something the UI does routinely.
    ///
    /// `internal` so the tests can drive it; its one production call site is
    /// the Apply button's detached `Task {}`.
    @MainActor
    func applyPlan(_ preview: PlanApplicationPreview) async {
        let includingConfirmable = plan.applyIncludesConfirmable
        let indices = preview.attemptedIndices(includingConfirmable: includingConfirmable)
        guard !indices.isEmpty else { return }
        guard plan.beginApplyingBatch() else { return }
        defer { plan.endApplyingBatch() }

        plan.applyCompletedItems = []
        plan.applyPhase = .applying(preview)

        var statuses: [Int: PlanApplicationItemStatus] = [:]
        var completed: [PlanApplicationItemResult] = []
        var stoppedEarlyReason: String?

        for (position, index) in indices.enumerated() {
            let step = preview.steps[index]
            plan.applyProgressText = "Cleaning \(position + 1) of \(indices.count): \(step.kindTerm.title)"

            let status = await executeStep(step)
            statuses[index] = status
            completed.append(PlanApplicationItemResult(
                resourceId: step.resourceId,
                kind: step.kind,
                status: status
            ))
            plan.applyCompletedItems = completed

            if status.haltsBatch {
                stoppedEarlyReason = status.message
                break
            }
        }

        plan.applyPhase = .finished(PlanApplicationResult.assemble(
            preview: preview,
            includingConfirmable: includingConfirmable,
            statusesByStepIndex: statuses,
            stoppedEarlyReason: stoppedEarlyReason
        ))
        plan.applyProgressText = nil
        plan.applyCompletedItems = []
    }

    /// One step's `execute`, through the same pure argument builder the
    /// single-item Clean uses.
    ///
    /// `requiresConfirmation` is read from the step's own disposition, and the
    /// token from the step's own `explain` report — `buildExecuteArguments`
    /// adds `--confirm-ask` and `--observed-fingerprint` together or not at
    /// all, matching `execute`'s own requirement. A step with no action id
    /// cannot reach here (only runnable steps are attempted, and those always
    /// carry one), and if one ever did the guard below declines rather than
    /// inventing an id.
    @MainActor
    private func executeStep(_ step: PlanApplicationStep) async -> PlanApplicationItemStatus {
        guard let actionId = step.actionId else {
            return .notAttempted(reason: "Glomeris offered no action for this resource.")
        }

        let arguments = buildExecuteArguments(
            actionId: actionId,
            resourceId: step.resourceId,
            projectRootsArguments: projectRootsStore.commandLineArguments,
            requiresConfirmation: step.disposition == .willRunAfterConfirming,
            fingerprintToken: step.fingerprintToken
        )

        do {
            let raw = try await client.runRaw(
                arguments,
                progressType: ProgressEventDto.self,
                onProgress: { event in
                    // Each event gets its own unstructured task, so these are
                    // unordered with respect to each other and to the loop. The
                    // phase check is what keeps that contained: without it a
                    // late write can land after the batch ended and put a
                    // deletion line — "Deleting …" — into `preparingBody`,
                    // which renders the same field for a read-only re-check.
                    // Within a run the worst case is an out-of-date line, which
                    // a progress indicator is allowed to be.
                    Task { @MainActor in
                        guard case .applying = plan.applyPhase else { return }
                        plan.applyProgressText = ProgressStatusText.text(for: event)
                    }
                }
            )
            return PlanApplicationItemStatus.from(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderrText: String(data: raw.stderr, encoding: .utf8) ?? ""
            )
        } catch {
            // `shortMessage` returns `nil` only for a cancellation, which
            // cannot happen here — nothing holds a handle to this run. Treated
            // as a failure rather than assumed away: an item whose outcome we
            // do not know is not an item that cleaned.
            return .failed(message: SectionFetchErrors.shortMessage(
                error,
                subject: "execute could not be started"
            ) ?? "execute could not be started.")
        }
    }
}

#Preview {
    ApplyPlanView(plan: PlanState(), items: [])
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
