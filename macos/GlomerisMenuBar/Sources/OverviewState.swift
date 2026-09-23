//
//  OverviewState.swift
//  GlomerisMenuBar
//
//  HORO-1365: what a scan found and what a provider suggested are owned
//  here — above the view tree — instead of inside the cards that draw them.
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
//  So the lifetime is moved off the view tree entirely. These objects are
//  created once by `GlomerisMenuBarApp` and handed down, and navigating in
//  or out of a detail cannot reach them. Hiding the overview, removing it,
//  re-presenting it or rebuilding the whole popover all leave a completed
//  scan and a completed plan exactly where they were.
//
//  ---------------------------------------------------------------------
//  What may replace a result, and what may not
//  ---------------------------------------------------------------------
//  Only the user, explicitly. A scan is replaced by pressing Refresh, a
//  plan by pressing "Ask AI for a plan" — those two buttons remain the sole
//  writers of `candidates` and `outcome`, so there is no appear-trigger,
//  timer or navigation side effect that can invalidate either one.
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

/// The result of the last `llm-plan` run.
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
}
