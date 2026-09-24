//
//  OverviewState.swift
//  GlomerisMenuBar
//
//  HORO-1365: what a scan found and what a provider suggested are owned
//  above every drill-down — by `GlomerisPopoverView` — instead of inside the
//  cards that draw them.
//
//  ---------------------------------------------------------------------
//  The defect this exists to make structurally impossible
//  ---------------------------------------------------------------------
//  Both results used to live in `@State` on `CandidatesSectionView` and
//  `AiPlanSectionView`. `@State` is owned by a view's position in the
//  hierarchy, so those results lasted exactly as long as the cards stayed
//  mounted — and the panel's one drill-down decides whether they do. When
//  the in-panel detail was first wired up it swapped the overview out
//  (`if detail else sections`), both cards left the hierarchy, and pressing
//  back landed on "No scan yet" with the scan and the plan gone. That was
//  reproduced mechanically on the 22 Sep 17:52 dogfood bundle: Refresh →
//  open a candidate → Back returned "No scan yet" / "Refresh to look for
//  space you can reclaim." and "not asked yet" / "No plan yet".
//
//  The immediate fix layered the detail over the overview in a `ZStack` so
//  the cards stay mounted. That works, and it is still there for the scroll
//  position it also preserves — but it makes a *presentation* decision the
//  only thing protecting a *result*. Any future re-presentation (a swap, a
//  `NavigationStack`, a conditional branch) silently reintroduces the same
//  data loss, which is precisely how it survived being fixed once already.
//
//  So the lifetime is moved above the drill-down. These objects are created
//  once by `GlomerisPopoverView` — the `MenuBarExtra` content root, which
//  also declares the `navigation` the detail turns on — and handed down.
//  Hiding the overview, removing it, re-presenting it or rebuilding the
//  whole body all leave a completed scan and a completed plan exactly where
//  they were, because re-evaluating a body does not re-create a
//  `@StateObject`.
//
//  Not one level higher, on the `App`. That was tried, and it is worse: an
//  observable object on the scene makes every publish re-evaluate
//  `App.body`, which constructs the Settings tabs whether or not a Settings
//  window exists — and `AiProviderPreferencesView.init` seeds its status
//  with a synchronous keychain read. A single Refresh publishes on every
//  progress line, so the scene-level version turned one scan into a burst of
//  main-thread `SecItemCopyMatching` calls; on a bundle whose code identity
//  the keychain ACL does not recognise, one of them blocked behind a
//  `SecurityAgent` prompt and the app's menu-bar item disappeared mid-scan.
//  The popover root is above everything this ticket is about and below that.
//
//  ---------------------------------------------------------------------
//  What may replace a result, and what may not
//  ---------------------------------------------------------------------
//  Only the user, explicitly. A scan is replaced by pressing Refresh, a
//  plan by pressing "Ask AI for a plan" — those two buttons remain the sole
//  writers of `candidates` and `outcome`, so there is no appear-trigger,
//  timer or navigation side effect that can invalidate either one.
//
//  HORO-1366 adds a third result to `PlanState` — what an Apply Plan run
//  previewed and did — and it follows the same rule. Its writers are the Apply,
//  Cancel and Dismiss buttons and the run itself; a new plan clears it through
//  `resetApplyState()`, because a preview naming resources from a plan that is
//  no longer on screen is worse than no preview. Nothing else invalidates it,
//  so a finished batch's per-item detail survives a drill-down and a Back for
//  the same reason the plan does.
//
//  A project-roots change keeps its existing documented behaviour and is
//  deliberately NOT wired to anything here: `ProjectRootsStore` is read at
//  spawn time, so changing the roots takes effect on the next explicit
//  Refresh or Ask. Adding an automatic invalidation would be a new
//  behaviour, not a fix, and would hand navigation a second route to the
//  clearing this ticket exists to remove.
//
//  ---------------------------------------------------------------------
//  Still no policy, no evidence and no decisions here
//  ---------------------------------------------------------------------
//  These are stores, not view models: DTOs decoded by the CLI go in
//  verbatim and come out verbatim. Nothing here interprets a policy label,
//  re-ranks a plan, decides what may run, or computes anything a report
//  already states. The standing rule in GlomerisMenuBarApp.swift applies
//  here as it does everywhere else in this target.
//

import Foundation

/// The result of the last `detect` run, plus the two view controls that
/// decide how it is shown.
///
/// The controls live here with the result on purpose: a filter and an order
/// that reset on a drill-down change which candidates are on screen when the
/// user comes back, which is the same defect in a quieter form.
final class ScanState: ObservableObject {
    @Published var candidates: [DetectCandidateReportDto] = []
    @Published var lastScannedAt: Date?
    @Published var isScanning = false
    @Published var progressStatusText: String?
    @Published var lastErrorMessage: String?

    /// HORO-1307 view controls. Both default to "show me everything, in the
    /// order Glomeris recommends", so the panel a user opens for the first
    /// time is never silently filtered.
    @Published var sortOrder: CandidateSortOrder = .recommended
    @Published var safetyFilter: CandidateSafetyFilter = .all
}

/// Where an Apply Plan run has got to (HORO-1366).
///
/// One property rather than several flags, because the states are genuinely
/// exclusive and the pairs that would be nonsense — reviewing a preview while a
/// batch is mid-flight, or showing a finished result beside a live one — are
/// the ones a set of booleans invites. The associated values are the
/// already-assembled `PlanApplication` types, so this enum stores and does not
/// compute.
enum PlanApplicationPhase: Equatable {
    /// No batch has been asked for since the plan arrived.
    case idle
    /// Re-running `explain` for each plan item. Read-only, and cancellable.
    case preparing
    /// The preview is on screen and nothing has run. The user can still
    /// cancel, which is the "allow cancel before execution" the ticket asks
    /// for — the only point at which cancelling is safe.
    case reviewing(PlanApplicationPreview)
    /// Items are being executed, one at a time. Not cancellable: see
    /// `PlanState.applyTask`'s absence below.
    case applying(PlanApplicationPreview)
    /// Every authorised item was attempted, or the batch stopped. Kept on
    /// screen until the user dismisses it or asks for a new plan, so a result
    /// can be read afterwards rather than only as it happens.
    case finished(PlanApplicationResult)
}

/// The result of the last `llm-plan` run, and what the user has asked Glomeris
/// to do about it.
final class PlanState: ObservableObject {
    @Published var outcome: AiPlanOutcome?
    @Published var lastPlannedAt: Date?
    @Published var isPlanning = false
    @Published var progressStatusText: String?
    @Published var lastErrorMessage: String?

    /// The running request, held only so Stop can cancel it — deliberately
    /// not `@Published`, since nothing renders the handle itself and the
    /// Stop button appears on `isPlanning`. `GlomerisClient` turns the
    /// cancellation into a `SIGTERM` for the child and a `.cancelled`
    /// error, and `SectionFetchErrors.shortMessage` returns no message for
    /// that case — a request the user stopped is not a failure to report
    /// (HORO-1308).
    var planTask: Task<Void, Never>?

    // MARK: - Apply Plan (HORO-1366)

    @Published var applyPhase: PlanApplicationPhase = .idle

    /// Whether the user has opted in to including the items that ask first.
    ///
    /// Defaults to `false` on every new preview, and `resetApplyState()` puts
    /// it back. An ASK item is one Glomeris will not touch without being told
    /// to, and a control that remembers "yes" from a previous plan would carry
    /// that instruction to resources the user never saw.
    @Published var applyIncludesConfirmable = false

    /// The rows that have already been attempted in the current run, so a
    /// batch in progress shows what it has done rather than only a spinner.
    /// Folded into the `.finished` result when the run ends.
    @Published var applyCompletedItems: [PlanApplicationItemResult] = []

    @Published var applyProgressText: String?

    /// The `explain` sweep that builds a preview, held so the user can cancel
    /// a slow one. Safe to cancel for the reason `GlomerisClient.runRaw`'s
    /// header gives: `explain` observes and advises, and interrupting one
    /// loses nothing but the answer.
    var preparePreviewTask: Task<Void, Never>?

    /// There is deliberately no `applyTask` handle, and the absence is the
    /// safety property rather than an omission.
    ///
    /// `GlomerisClient.runRaw`'s header states the rule this enforces:
    /// `execute` must never be reachable from a cancellable task, because a
    /// `SIGTERM` partway through a deletion leaves the filesystem in a state
    /// neither this app nor the audit log could describe. A stored handle is
    /// what a future Stop button would reach for, and a batch is exactly the
    /// surface on which one looks reasonable — so the handle does not exist.
    /// `AiPlanSectionView` launches the run in a detached `Task {}` from a
    /// button action and keeps nothing; the cancel the ticket asks for happens
    /// in `.reviewing`, before anything has run.

    /// `true` from the moment the `execute` loop starts until it ends.
    ///
    /// This is a fact about a running child process, not display state, which
    /// is why `resetApplyState()` deliberately does not clear it and why the
    /// phase is not used for it. Clearing the phase is something the UI does
    /// routinely, and a guard that no second batch can start has to survive
    /// that.
    @Published private(set) var isApplyingBatch = false

    /// Claims the right to run a batch, or reports that one already holds it.
    ///
    /// A compare-and-set rather than a plain setter, so the invariant lives in
    /// one place. Both calls are on the main actor — every caller is
    /// `@MainActor` — so the read and the write cannot interleave.
    ///
    /// Two concurrent batches would take turns losing. The execution lock
    /// (`src/executor/lock.rs`) is exclusive and non-blocking: one child takes
    /// it and the other is refused `busy`, which halts that batch and tells the
    /// user another execution is already in progress. That sentence would be
    /// true and the cause would be us, so the fix is to not start the second
    /// batch rather than to explain the collision afterwards.
    func beginApplyingBatch() -> Bool {
        if isApplyingBatch { return false }
        isApplyingBatch = true
        return true
    }

    func endApplyingBatch() {
        isApplyingBatch = false
    }

    /// Clears everything about a batch. Called when a new plan replaces the
    /// one a preview or result was about — a preview naming resources from a
    /// plan the user can no longer see is worse than no preview.
    ///
    /// Cancels the `explain` sweep as well as clearing the phase. Clearing the
    /// phase alone was not enough: the sweep runs on its own task, and on
    /// finishing it assigns `.reviewing(preview)` unconditionally. So a reset
    /// during a sweep produced exactly the state this function exists to
    /// prevent — a preview, with a live Apply button, naming resources from a
    /// plan that is no longer on screen. The sweep is `explain` only, so
    /// cancelling it loses nothing but the answer.
    ///
    /// It does **not** stop a running batch, because nothing can: see the
    /// absent `applyTask` above.
    func resetApplyState() {
        preparePreviewTask?.cancel()
        preparePreviewTask = nil
        applyPhase = .idle
        applyIncludesConfirmable = false
        applyCompletedItems = []
        applyProgressText = nil
    }
}
