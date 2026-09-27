//
//  RecoverySectionView.swift
//  GlomerisMenuBar
//
//  HORO-1506: the recovery goal as a first-class product surface — "set a
//  disk-space goal, Glomeris safely works toward it" — over the closed loop the
//  CLI already has.
//
//  ---------------------------------------------------------------------
//  What this card does NOT do
//  ---------------------------------------------------------------------
//  It does not implement a recovery loop. `glomeris free` owns the loop, the
//  budgets, the per-iteration rescan and the decision that a goal was reached;
//  this card asks for a pre-flight with `--dry-run --json`, shows what came
//  back, and then asks for a real run. Every figure it displays was computed in
//  Rust, and the one number it sends is the goal the user set.
//
//  It also does not convert between the two ways a target can be expressed. The
//  product axis is *target usage* — "get me down to 70% used" — and the recovery
//  loop's own axis is free space. `RecoveryGoal` in `src/executor/goal.rs` is the
//  single adapter between them, tested against the loop's own `target_met`
//  predicate over a grid of goals and volume sizes. A `100 - x` here would be a
//  second, unchecked conversion of exactly the kind this ticket exists to
//  remove, so the only percentage this file ever writes is the one the user
//  chose, always labelled `used`, and every rendered target string comes from a
//  report's own `description`/`target` field.
//
//  ---------------------------------------------------------------------
//  Why the wording lives in two pure types
//  ---------------------------------------------------------------------
//  `RecoveryPreviewSummary` and `RecoveryRunSummary` are built from one DTO each
//  and hold only stored properties, following `DaemonHealthViewModel`'s
//  precedent in StatusHealthSectionView.swift. The view's own state is
//  `private`, so anything worded inside `body` can only be checked by grepping
//  source text — and a guard that reads source cannot tell whether the sentence
//  it found is the one a user would see. These two can be constructed in a test
//  from a golden fixture and asserted on directly.
//
//  The rules they exist to pin:
//
//    * a stop is never reported as plain success. `target_met` is the only thing
//      that produces a success message, and it comes from the re-measured free
//      space rather than from the sum of what was deleted (campaign §9). A run
//      that stopped because nothing safe was left says so, in the CLI's own
//      words, with the badge that belongs to that stop reason.
//    * the reclaimable opportunity is three figures, never one. Adding
//      confirmation-gated space to immediately-actionable space would invite a
//      user to read space they have not authorised as space they are about to
//      get back, and protected space has no total here at all.
//    * every reported byte figure in a *result* is measured. The estimate-vs-
//      measurement distinction is stated in Rust's own caveats, which are
//      rendered verbatim.
//
//  ---------------------------------------------------------------------
//  Cancellation
//  ---------------------------------------------------------------------
//  The pre-flight is cancellable; the run is not, and that asymmetry is the
//  safety property. `GlomerisClient.runRaw`'s header states the rule: a
//  `SIGTERM` partway through a deletion leaves the filesystem in a state neither
//  this app nor the audit log could describe. So the real run is launched from a
//  button action in a detached `Task {}` — never from `.task {}`, which SwiftUI
//  cancels on disappear — and no handle is kept. The campaign's "stop after the
//  current action" is a cooperative stop inside the Rust loop (HORO-1509), not a
//  signal from here, and until it exists this card says plainly before starting
//  that a run cannot be interrupted.
//

import SwiftUI

/// One `free --dry-run --json` pre-flight, as the card shows it.
///
/// Every property is a display string derived from one field, or from one field
/// plus the vocabulary. Nothing here adds two figures together.
struct RecoveryPreviewSummary: Equatable {
    /// The goal, on both axes, in the CLI's own sentence: `"60% used (40%
    /// free)"`. Never reassembled from the two numbers beside it — a bare
    /// percentage whose axis is unstated is the defect this ticket names.
    let goalText: String
    /// e.g. `"88.0% used — 55.9 GB free of 465.7 GB"`. The percentage is
    /// formatted to one decimal to match `print_recovery_preview_report`, so the
    /// GUI and the terminal cannot disagree about the same reading.
    let currentText: String
    let pressureTerm: GlomerisTerm
    /// How much more free space the goal needs, measured from the current
    /// reading. `nil` once the goal is already met, because "0 B more needed" is
    /// a figure and the situation is a different one — see `goalIsAlreadyMet`.
    let stillNeededText: String?
    /// `true` when current usage is already at or below the goal. A real run
    /// would be refused in that state (`RecoveryGoal::progress_toward`), and
    /// Rust says so in a caveat, so the card disables the action rather than
    /// offering one that cannot start.
    let goalIsAlreadyMet: Bool
    /// Space a run could reclaim without asking, as `"2.0 GB across 1
    /// candidate"`.
    let actionableNowText: String
    /// Space behind a confirmation, or `nil` when there is none. A separate
    /// figure on purpose: it is not part of what a run will do.
    let requiresConfirmationText: String?
    /// Candidates no run can act on, or `nil` when there are none. A count, and
    /// deliberately not a byte total — bytes that cannot be reclaimed are not an
    /// opportunity, and a total for them is the first thing a summary row would
    /// add up.
    let notExecutableText: String?
    /// The CLI's planning judgment, worded as the hint it is.
    let reachabilityText: String
    /// Rust's sentences, verbatim and in order.
    let caveats: [String]

    init(_ dto: RecoveryPreviewReportDto) {
        goalText = dto.goal.description
        currentText = String(format: "%.1f%% used", dto.current.usedPercent)
            + " — \(dto.current.freeHuman) free of \(dto.current.totalHuman)"
        pressureTerm = GlomerisVocabulary.pressure(dto.current.pressureState)
        goalIsAlreadyMet = dto.bytesNeeded == 0
        stillNeededText = dto.bytesNeeded == 0
            ? nil
            : "\(dto.bytesNeededHuman) more free space needed"

        // `isLowerBound` is one flag for the whole opportunity, so it prefixes
        // both figures. That errs in the safe direction: `≥ 2.0 GB` is true of
        // an exactly-measured 2.0 GB, whereas dropping the marker from a figure
        // that really is a floor would state a measurement Glomeris does not
        // have.
        actionableNowText = Self.spaceAcrossCandidates(
            human: dto.opportunity.actionableNowHuman,
            isLowerBound: dto.opportunity.isLowerBound,
            count: dto.opportunity.actionableNowCount
        )
        requiresConfirmationText = dto.opportunity.requiresConfirmationCount == 0
            ? nil
            : Self.spaceAcrossCandidates(
                human: dto.opportunity.requiresConfirmationHuman,
                isLowerBound: dto.opportunity.isLowerBound,
                count: dto.opportunity.requiresConfirmationCount
            )
        notExecutableText = Self.notExecutableText(
            count: dto.opportunity.notExecutableCount,
            protectedCount: dto.opportunity.protectedCount
        )

        // Worded as a hint in both directions, because it is one in both
        // directions: `false` does not mean a run is pointless, since estimates
        // are often floors, and `true` does not promise the goal will be
        // reached. Any sentence here has to survive both of those.
        reachabilityText = dto.goalAppearsReachable
            ? "On this estimate the goal looks reachable. Only a real run can tell."
            : "On this estimate there may not be enough to reclaim. "
                + "A run would still take what it safely can."
        caveats = dto.caveats
    }

    private static func spaceAcrossCandidates(
        human: String,
        isLowerBound: Bool,
        count: Int
    ) -> String {
        let size = GlomerisVocabulary.storageImpact(human: human, isLowerBound: isLowerBound).title
        return "\(size) across \(count) \(count == 1 ? "item" : "items")"
    }

    private static func notExecutableText(count: Int, protectedCount: Int) -> String? {
        if count == 0 { return nil }
        let items = "\(count) \(count == 1 ? "item" : "items")"
        if protectedCount == 0 {
            return "\(items) no run can act on"
        }
        return "\(items) no run can act on, \(protectedCount) of them protected"
    }
}

/// One finished `free --json` run, as the card shows it.
///
/// The type exists mainly to hold one rule: `outcomeMessage` is a
/// ``GlomerisStateMessage/success(_:)`` if and only if the CLI measured the goal
/// as met. Everything else — out of safe candidates, a budget reached, no
/// headway, an error — is reported through the stop-reason badge and the CLI's
/// own detail sentence, so no termination can render as a bare "Done".
struct RecoveryRunSummary: Equatable {
    let stopReasonTerm: GlomerisTerm
    /// The CLI's one-sentence explanation of the stop, verbatim.
    let stopReasonDetail: String
    /// The goal or floor the run worked toward, always naming its axis.
    let goalText: String
    /// Measured, never estimated: `"2.0 GB reclaimed"`.
    let reclaimedText: String
    /// `"55.9 GB free → 58.0 GB free"`, both figures re-read from the volume.
    let freeSpaceText: String
    /// `"2 rounds · 1 action run · 3 left alone"`.
    let effortText: String
    /// Present only for the two outcomes that are a claim in their own right:
    /// the goal was met, or the run hit an error. `nil` otherwise, because the
    /// badge and `stopReasonDetail` already say what happened and a second
    /// sentence would have to either repeat them or soften them.
    let outcomeMessage: GlomerisStateMessage?
    let caveats: [String]

    init(_ dto: RecoveryRunReportDto) {
        stopReasonTerm = GlomerisVocabulary.stopReason(dto.stopReason)
        stopReasonDetail = dto.stopReasonDetail
        // A used-percent goal reports its own two-axis sentence; a raw
        // `--target` floor has no `goal` at all and its `target` string names
        // the free-space axis itself. Neither is rewritten here, which is what
        // keeps a free-space figure from ever being read as a usage one.
        goalText = dto.goal?.description ?? dto.target
        reclaimedText = "\(dto.bytesFreedMeasuredHuman) reclaimed"
        freeSpaceText = "\(dto.startedFreeHuman) free → \(dto.finalFreeHuman) free"
        effortText = Self.effortText(dto)

        if let error = dto.error, !error.isEmpty {
            outcomeMessage = .failure("The run stopped with an error: \(error)")
        } else if dto.targetMet {
            // The only success sentence in this file, and it quotes a measured
            // figure. `targetMet` is decided in Rust from the final re-read of
            // free space, not from the sum of what was deleted (campaign §9).
            outcomeMessage = .success("Goal reached — \(dto.bytesFreedMeasuredHuman) reclaimed.")
        } else {
            outcomeMessage = nil
        }
        caveats = dto.caveats
    }

    private static func effortText(_ dto: RecoveryRunReportDto) -> String {
        let rounds = "\(dto.iterationsRun) \(dto.iterationsRun == 1 ? "round" : "rounds")"
        let run = "\(dto.actionsExecuted) \(dto.actionsExecuted == 1 ? "action" : "actions") run"
        let left = "\(dto.actionsDeclinedOrSkipped) left alone"
        return "\(rounds) · \(run) · \(left)"
    }
}

/// Popover section: the recovery goal, its pre-flight, and the run.
///
/// Placed above the candidates list in `GlomerisPopoverView`, because the goal
/// is the product capability and the list is the material it works from.
struct RecoverySectionView: View {
    /// The card's title, and the noun the whole feature is named by. A stored
    /// constant so the tests can name it without re-typing it.
    static let cardTitle = "Recovery goal"

    /// The goal control's bounds and step.
    ///
    /// The CLI accepts any percentage in 0…100; this range is the app's
    /// convenience, not the contract's limit. Whole multiples of five keep the
    /// control operable from the keyboard in a 340pt column, and the ends are
    /// clipped because a goal of 0% used asks Glomeris to empty the disk and
    /// 100% used can never be an improvement on anything.
    static let goalRange = 5...95
    static let goalStep = 5

    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    /// HORO-1365's rule: the goal and its result are not owned here. This card
    /// is their only writer, but it does not get to decide how long they live —
    /// see `OverviewState.swift`.
    @ObservedObject private var recovery: RecoveryState

    init(
        recovery: RecoveryState,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.recovery = recovery
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        GlomerisCard(title: Self.cardTitle) {
            goalControl
            phaseContent
            // Additive, as everywhere else in the panel: a failure to run the
            // CLI appears beneath whatever is already on screen rather than
            // replacing it, so a stale pre-flight is never silently presented
            // as current (HORO-1297).
            if let message = recovery.lastErrorMessage {
                GlomerisStateMessageView(message: .failure(message))
            }
        }
    }

    // MARK: - The goal

    /// A labelled stepper, deliberately not a slider.
    ///
    /// The campaign's accessibility rule names unlabeled percentage sliders as
    /// the thing not to build: a slider's value is a position, it needs a second
    /// element to say what the position means, and the two can disagree. The
    /// label here states the number and its axis in the same string a sighted
    /// user reads, and the stepper moves in fixed increments that a keyboard and
    /// VoiceOver can both report exactly.
    ///
    /// Disabled while a run is in flight. Not for tidiness: changing the goal
    /// clears the pre-flight, and a card showing no goal progress while a child
    /// process is still deleting things would be describing a machine state that
    /// is not the one it is in.
    @ViewBuilder
    private var goalControl: some View {
        Stepper(value: goalBinding, in: Self.goalRange, step: Self.goalStep) {
            Text(Self.goalLabel(usedPercent: recovery.goalUsedPercent))
                .font(GlomerisDesign.primaryFont)
        }
        .disabled(recovery.isRecovering)
        .accessibilityValue(Self.goalAccessibilityValue(usedPercent: recovery.goalUsedPercent))
        .accessibilityHint(
            "The percentage of the disk still in use that Glomeris should work down to."
        )
    }

    /// `"Get down to 70% used"` — the axis is in the sentence, always.
    ///
    /// `static` and `internal` so a test can assert that, rather than the
    /// assertion being a grep for a percent sign in this file.
    static func goalLabel(usedPercent: Int) -> String {
        "Get down to \(usedPercent)% used"
    }

    /// What VoiceOver reads as the stepper's value: `"70 percent used"`.
    ///
    /// Spelled out rather than `%`, because a spoken `%` is at the mercy of the
    /// voice, and this is the one number in the card a user can change — a value
    /// read as a bare "70" would leave the axis to be inferred from a label read
    /// some moments earlier. Static for the same reason `goalLabel` is: the
    /// campaign's rule that spoken state must distinguish current usage from the
    /// target is only kept if something checks it.
    static func goalAccessibilityValue(usedPercent: Int) -> String {
        "\(usedPercent) percent used"
    }

    private var goalBinding: Binding<Int> {
        Binding(
            get: { recovery.goalUsedPercent },
            set: { newValue in
                guard newValue != recovery.goalUsedPercent else { return }
                recovery.goalUsedPercent = newValue
                // Every figure in a pre-flight is measured against the goal that
                // produced it, so a preview headed "60% used" under a control
                // now reading 50 would describe a run this button would no
                // longer start.
                recovery.resetGoalProgress()
            }
        )
    }

    // MARK: - Phases

    @ViewBuilder
    private var phaseContent: some View {
        switch recovery.phase {
        case .idle:
            reviewButton
            Text("Checks what could be reclaimed. Nothing is deleted by looking.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
        case .previewing:
            GlomerisStateMessageView(
                message: .loading(recovery.progressStatusText ?? "Checking what could be reclaimed…")
            )
            Button("Stop") {
                // Safe to cancel: `free --dry-run` discovers and reports,
                // mutating nothing.
                recovery.previewTask?.cancel()
            }
            .accessibilityLabel("Stop checking")
        case .reviewing(let preview):
            previewContent(RecoveryPreviewSummary(preview))
            recoverButton(preview: preview, alreadyMet: preview.bytesNeeded == 0)
        case .refused(let rejection):
            refusalContent(rejection)
        case .recovering(let preview):
            runningContent(RecoveryPreviewSummary(preview))
        case .finished(let report):
            resultContent(RecoveryRunSummary(report))
        }
    }

    @ViewBuilder
    private var reviewButton: some View {
        Button("Review & recover…") {
            recovery.previewTask = Task { await runPreview() }
        }
        .accessibilityHint("Shows what would be reclaimed before anything runs.")
    }

    /// The pre-flight, in the order §4 requires it be readable: where the disk
    /// is now, where the goal is, how far that is, and what is available to
    /// close the gap.
    @ViewBuilder
    private func previewContent(_ summary: RecoveryPreviewSummary) -> some View {
        GlomerisDetailRow(label: "Now") {
            VStack(alignment: .leading, spacing: 2) {
                GlomerisBadgeView(term: summary.pressureTerm)
                Text(summary.currentText)
                    .font(GlomerisDesign.secondaryFont)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        GlomerisDetailRow(label: "Goal") {
            Text(summary.goalText)
                .font(GlomerisDesign.secondaryFont)
        }
        if let stillNeeded = summary.stillNeededText {
            GlomerisDetailRow(label: "To go") {
                Text(stillNeeded)
                    .font(GlomerisDesign.secondaryFont)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        GlomerisDetailRow(label: "Ready now") {
            Text(summary.actionableNowText)
                .font(GlomerisDesign.secondaryFont)
        }
        // Each on its own row, never folded into the figure above: one total
        // would read as space the user is about to get back.
        if let confirmation = summary.requiresConfirmationText {
            GlomerisDetailRow(label: "Asks first") {
                Text(confirmation)
                    .font(GlomerisDesign.secondaryFont)
            }
        }
        if let notExecutable = summary.notExecutableText {
            GlomerisDetailRow(label: "Off limits") {
                Text(notExecutable)
                    .font(GlomerisDesign.secondaryFont)
            }
        }
        Text(summary.reachabilityText)
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        caveatList(summary.caveats)
    }

    /// The one button in this file that deletes anything.
    ///
    /// Disabled when the goal is already met, because a run would be refused in
    /// that state and Rust's own caveat above already says so — offering an
    /// action that cannot start would make the refusal look like a fault.
    @ViewBuilder
    private func recoverButton(preview: RecoveryPreviewReportDto, alreadyMet: Bool) -> some View {
        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button("Recover") {
                // A detached `Task {}` from a button action, and no handle kept:
                // see the file header.
                Task { await runRecovery(preview: preview) }
            }
            .disabled(alreadyMet)
            .accessibilityHint(
                "Works toward the goal, reclaiming only what Glomeris may act on without asking."
            )
            Button("Cancel") {
                recovery.resetGoalProgress()
            }
            .accessibilityLabel("Cancel without recovering anything")
            Spacer(minLength: 0)
        }
        if !alreadyMet {
            // Said before the run rather than after it, because it is a fact
            // about what the user is about to start. A cooperative stop arrives
            // with the progress contract; until then there is nothing to offer
            // and claiming otherwise would be worse than saying so.
            Text("A run cannot be interrupted once it starts. It stops when the goal is "
                + "reached or when nothing safe is left.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// A goal the CLI declined. Not an error: nothing failed and nothing ran.
    @ViewBuilder
    private func refusalContent(_ rejection: RecoveryGoalRejectionReportDto) -> some View {
        GlomerisBadgeView(term: GlomerisVocabulary.goalRejection(rejection.reason))
        // Rust decided the refusal and Rust words it. Re-explaining a judgment
        // this app did not make is how two surfaces come to describe one rule
        // differently.
        Text(rejection.message)
            .font(GlomerisDesign.secondaryFont)
            .fixedSize(horizontal: false, vertical: true)
        Button("Set a different goal") {
            recovery.resetGoalProgress()
        }
    }

    @ViewBuilder
    private func runningContent(_ summary: RecoveryPreviewSummary) -> some View {
        GlomerisStateMessageView(message: .loading("Working toward \(summary.goalText)…"))
        GlomerisDetailRow(label: "Started at") {
            Text(summary.currentText)
                .font(GlomerisDesign.secondaryFont)
                .fixedSize(horizontal: false, vertical: true)
        }
        Text("Each step is checked again before it runs, so this can take a while. "
            + "Per-step progress is not reported yet.")
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// The result. Reads the same whether the goal was reached or not — the
    /// badge and the CLI's own sentence say which, and neither can be reduced to
    /// "Done".
    @ViewBuilder
    private func resultContent(_ summary: RecoveryRunSummary) -> some View {
        GlomerisBadgeView(term: summary.stopReasonTerm)
        Text(summary.stopReasonDetail)
            .font(GlomerisDesign.secondaryFont)
            .fixedSize(horizontal: false, vertical: true)
        if let message = summary.outcomeMessage {
            GlomerisStateMessageView(message: message)
        }
        GlomerisDetailRow(label: "Goal") {
            Text(summary.goalText)
                .font(GlomerisDesign.secondaryFont)
        }
        GlomerisDetailRow(label: "Reclaimed") {
            Text(summary.reclaimedText)
                .font(GlomerisDesign.secondaryFont)
        }
        GlomerisDetailRow(label: "Free space") {
            Text(summary.freeSpaceText)
                .font(GlomerisDesign.secondaryFont)
                .fixedSize(horizontal: false, vertical: true)
        }
        GlomerisDetailRow(label: "Work done") {
            Text(summary.effortText)
                .font(GlomerisDesign.secondaryFont)
                .fixedSize(horizontal: false, vertical: true)
        }
        caveatList(summary.caveats)
        Button("Dismiss") {
            recovery.resetGoalProgress()
        }
        .accessibilityLabel("Dismiss this result")
    }

    /// Rust's caveats, verbatim and in order.
    ///
    /// Rendered rather than summarised: they are the estimate-versus-measurement
    /// distinction the campaign requires be stated, and a client that picked
    /// which of them to show would be deciding which of its own numbers were
    /// misleading.
    @ViewBuilder
    private func caveatList(_ caveats: [String]) -> some View {
        ForEach(caveats, id: \.self) { caveat in
            Text(caveat)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - The CLI

    /// `free --goal-used-percent N --dry-run --json --progress-json`.
    ///
    /// `runRaw` rather than `run`, so a non-zero exit's stdout survives:
    /// `run` throws and discards it, and for this command stdout is where the
    /// explanation lives. A pre-flight cannot be refused for a non-improving
    /// goal — `free --dry-run` reports that as a caveat and exits 0 — but it can
    /// still exit 2 for an argument this app got wrong, and a decodable body is
    /// the difference between saying what happened and saying "exit 2".
    @MainActor
    private func runPreview() async {
        recovery.phase = .previewing
        recovery.progressStatusText = nil
        recovery.lastErrorMessage = nil

        do {
            let raw = try await client.runRaw(
                Self.previewArguments(
                    goalUsedPercent: recovery.goalUsedPercent,
                    projectRootsStore: projectRootsStore
                ),
                progressType: ProgressEventDto.self,
                onProgress: { event in
                    Task { @MainActor in
                        recovery.progressStatusText = ProgressStatusText.text(for: event)
                    }
                }
            )
            let decoder = JSONDecoder()
            if raw.exitCode == 0,
               let report = try? decoder.decode(RecoveryPreviewReportDto.self, from: raw.stdout) {
                recovery.phase = .reviewing(report)
            } else {
                recovery.phase = .idle
                recovery.lastErrorMessage = Self.unreadableOutcomeMessage(
                    subject: "free --dry-run",
                    exitCode: raw.exitCode,
                    stdout: raw.stdout,
                    stderr: raw.stderr
                )
            }
        } catch {
            recovery.phase = .idle
            // `nil` for a cancelled call, which is not a failure to report: the
            // user pressed Stop, or the popover closed mid-check (HORO-1308).
            recovery.lastErrorMessage = SectionFetchErrors.shortMessage(error, subject: "free --dry-run")
        }
        recovery.progressStatusText = nil
        recovery.previewTask = nil
    }

    /// `free --goal-used-percent N --json`, for real.
    ///
    /// No `--progress-json`: a real run emits no progress events, and `free`
    /// refuses the flag outside `--dry-run` rather than accepting it and
    /// streaming nothing. Passing it would be asking for a stream that does not
    /// exist and being told so, with exit 2, for every run.
    ///
    /// `internal` so the tests can drive it; its one production call site is the
    /// Recover button's detached `Task {}`.
    @MainActor
    func runRecovery(preview: RecoveryPreviewReportDto) async {
        // The compare-and-set, for the reason `RecoveryState.beginRecovering()`
        // documents: two runs would take turns losing to an exclusive execution
        // lock, and the refusal would be true with us as its cause.
        guard recovery.beginRecovering() else { return }
        defer { recovery.endRecovering() }

        recovery.phase = .recovering(preview)
        recovery.lastErrorMessage = nil

        do {
            let raw = try await client.runRaw(
                Self.runArguments(
                    goalUsedPercent: recovery.goalUsedPercent,
                    projectRootsStore: projectRootsStore
                ),
                progressType: EmptyProgressDto.self
            )
            if let phase = Self.phase(forRunExitCode: raw.exitCode, stdout: raw.stdout) {
                recovery.phase = phase
            } else {
                recovery.phase = Self.unreadableOutcomePhase(
                    exitCode: raw.exitCode,
                    stdout: raw.stdout,
                    startedFrom: preview
                )
                recovery.lastErrorMessage = Self.unreadableOutcomeMessage(
                    subject: "free",
                    exitCode: raw.exitCode,
                    stdout: raw.stdout,
                    stderr: raw.stderr
                )
            }
        } catch {
            // Not `.reviewing(preview)`: a throw here means the child could not be
            // run or its output could not be collected, and the same reasoning as
            // ``unreadableOutcomePhase(exitCode:stdout:startedFrom:)`` applies —
            // the run's effect is unknown, so the stale pre-flight must go.
            recovery.phase = .idle
            recovery.lastErrorMessage = SectionFetchErrors.shortMessage(error, subject: "free")
        }
    }

    // MARK: - Pure argument and outcome mapping

    /// The complete pre-flight vector, project roots attached.
    ///
    /// The `scoped(_:)` call is inside the builder rather than around it at the
    /// call site, which is what makes "this invocation carries the user's
    /// configured folders" a property of a function a test can call. HORO-1501
    /// was one card that dropped its roots while every report still looked
    /// healthy; a builder that returns an unscoped vector is the same defect
    /// waiting for a second call site to forget.
    ///
    /// `free` is root-scoped — it discovers before it reclaims — so it is in
    /// `GlomerisCliProjectRootScope.rootScopedCommands`, and the store decides
    /// what that means rather than this file.
    ///
    /// The percentage is written on the used axis and nowhere else. There is no
    /// `--target` call site in this app.
    static func previewArguments(
        goalUsedPercent: Int,
        projectRootsStore: ProjectRootsStore
    ) -> [String] {
        projectRootsStore.scoped([
            "free", "--goal-used-percent", "\(goalUsedPercent)",
            "--dry-run", "--json", "--progress-json",
        ])
    }

    static func runArguments(
        goalUsedPercent: Int,
        projectRootsStore: ProjectRootsStore
    ) -> [String] {
        projectRootsStore.scoped(["free", "--goal-used-percent", "\(goalUsedPercent)", "--json"])
    }

    /// Maps a real run's exit code and stdout to the phase the card should show,
    /// or `nil` when the output cannot be read at all.
    ///
    /// Exit 0 is a finished run — including one that reclaimed nothing. Exit 2
    /// carries a goal rejection on stdout: the volume can free up between the
    /// pre-flight and the run, at which point a goal that was an improvement
    /// stops being one, and the user needs to be told that rather than shown a
    /// generic failure. Every other code — the busy execution lock's own refusal
    /// among them — has its message on one of the streams and is reported by
    /// ``unreadableOutcomeMessage(subject:exitCode:stdout:stderr:)``.
    ///
    /// The `exitCode == 2` guard on the rejection is load-bearing, not tidiness.
    /// `RecoveryGoalRejectionReportDto`'s two percentages are optional, so its
    /// required fields are exactly `reason` and `message` — which is also the
    /// whole of `ExecuteRefusalReportDto`. The execution lock's `busy` refusal
    /// would therefore decode cleanly as a goal rejection, and the card would
    /// tell a user their goal had been declined when in fact another run simply
    /// held the lock. Two DTOs whose JSON shapes overlap cannot be told apart by
    /// trying them in order; the exit code is what distinguishes them.
    ///
    /// `static` and pure so the exit-code contract can be tested without
    /// spawning anything.
    static func phase(forRunExitCode exitCode: Int32, stdout: Data) -> RecoveryPhase? {
        let decoder = JSONDecoder()
        if exitCode == 0 {
            guard let report = try? decoder.decode(RecoveryRunReportDto.self, from: stdout) else {
                return nil
            }
            return .finished(report)
        }
        if exitCode == 2,
           let rejection = try? decoder.decode(RecoveryGoalRejectionReportDto.self, from: stdout) {
            return .refused(rejection)
        }
        return nil
    }

    /// What to show after a run whose outcome could not be read.
    ///
    /// The pre-flight is only safe to keep on screen if nothing ran. That is
    /// knowable in exactly one case: the execution lock refused, `reason:
    /// "busy"`, before the loop started, so the volume is as the pre-flight
    /// described it and the user can press Recover again once the other run
    /// finishes.
    ///
    /// Every other unreadable outcome goes back to `.idle`, deliberately losing a
    /// report the user was reading. A run that got far enough to fail may have
    /// deleted things, which makes each of the pre-flight's figures a claim about
    /// a volume that no longer exists — and the card would be sitting under a
    /// Recover button offering to act on candidates that may already be gone.
    /// Made to re-check, the user gets measured numbers; allowed to act on stale
    /// ones, they get numbers that were true before an unknown amount of
    /// deletion.
    static func unreadableOutcomePhase(
        exitCode: Int32,
        stdout: Data,
        startedFrom preview: RecoveryPreviewReportDto
    ) -> RecoveryPhase {
        if let refusal = try? JSONDecoder().decode(ExecuteRefusalReportDto.self, from: stdout),
           refusal.reason == Self.executionLockBusyReason {
            return .reviewing(preview)
        }
        return .idle
    }

    /// The execution lock's own refusal token, from `RefusalReason::as_str` in
    /// `src/reporting/dto.rs`.
    ///
    /// The one token in this file compared as a string rather than handed to the
    /// vocabulary, because here it is not wording — it is the evidence that the
    /// loop never started, and a phase decision rests on it.
    ///
    /// The coupling to Rust is indirect and worth being exact about:
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` diffs `RefusalReason`'s
    /// tokens against `GlomerisVocabulary.refusal`'s case labels, so a rename on
    /// the Rust side turns that check red for the `busy` label there. This
    /// literal has to equal that label, and a test pins the two together — so a
    /// rename cannot land as a silent behaviour change here, but the red appears
    /// in the vocabulary first.
    static let executionLockBusyReason = "busy"

    /// One sentence for an invocation that produced no report this app could
    /// read.
    ///
    /// Prefers the CLI's own words, in the order the CLI chooses them. The
    /// execution lock's refusal is the case worth naming: `free` exits 75 with an
    /// `ExecuteRefusalReport` — `reason: "busy"` — on **stdout**, not stderr, and
    /// its message already says another execution is in progress. Reporting that
    /// as "exit 75" would hide a sentence the user can act on. Usage and
    /// capacity-read errors are prose on stderr. Only when both streams are
    /// unreadable does this name the exit code, which is the honest floor: a
    /// number, rather than a cause invented for it.
    static func unreadableOutcomeMessage(
        subject: String,
        exitCode: Int32,
        stdout: Data,
        stderr: Data
    ) -> String {
        let decoder = JSONDecoder()
        if let refusal = try? decoder.decode(ExecuteRefusalReportDto.self, from: stdout) {
            return "\(subject): \(refusal.message)"
        }
        let text = String(data: stderr, encoding: .utf8) ?? ""
        let detail = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !detail.isEmpty {
            return "\(subject): \(detail)"
        }
        return "\(subject): did not complete (exit \(exitCode))."
    }
}

#Preview {
    RecoverySectionView(recovery: RecoveryState())
        .frame(width: GlomerisDesign.popoverWidth)
}
