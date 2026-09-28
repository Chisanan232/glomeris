//
//  GlomerisPopoverView.swift
//  GlomerisMenuBar
//
//  HORO-1059 introduced this as an empty popover skeleton; HORO-1062 adds
//  the status + daemon-health section (StatusHealthSectionView); HORO-1063
//  adds the candidates list section (CandidatesSectionView); HORO-1066
//  adds the recent history + action audit section
//  (HistoryAuditSectionView); HORO-1308 adds the advisory AI Plan section
//  (AiPlanSectionView). See the standing project rule in
//  GlomerisMenuBarApp.swift before adding anything here beyond
//  presentation.
//
//  ---------------------------------------------------------------------
//  HORO-1306: the shell around the cards
//  ---------------------------------------------------------------------
//  This was a 260pt column of three sections separated by dividers, all of
//  it growing without limit. Four things changed.
//
//  1. Width comes from `GlomerisDesign.popoverWidth`. 260pt could not fit
//     a safety badge and a size on one line, so nearly every row wrapped,
//     and the wrapping was most of what made the panel hard to read.
//
//  2. The sections are cards now (each section owns its own
//     `GlomerisCard`), so the dividers between them are gone — spacing
//     does the grouping. The two dividers that remain are structural: they
//     mark where the fixed header and footer stop and the scrolling body
//     begins.
//
//  3. The body scrolls within `GlomerisDesign.maxBodyHeight`. A menu-bar
//     popover that grows with its content stops being a glance and starts
//     covering the screen whose disk it is reporting on — and the history
//     card alone can hold twenty rows.
//
//  4. Order is deliberate and is also the VoiceOver reading order: what
//     the disk is doing now, then what could be reclaimed, then what has
//     already happened. Primary state first, history last.
//
//  The footer is a small addition beyond this ticket's acceptance
//  criteria, made deliberately: `MenuBarExtra(.window)` draws no menu, so
//  before this the popover was the app's only surface and it had no route
//  to the project-roots preferences that decide what `detect` even looks
//  at (HORO-1067), and no way to quit. A redesign that leaves the panel a
//  dead end has not finished the job. HORO-1309's provider setup will land
//  in the same Settings scene.
//
//  ---------------------------------------------------------------------
//  HORO-1357: the detail view opens *in* the panel, not in a sheet
//  ---------------------------------------------------------------------
//  Both routes into `CandidateDetailView` — a candidates-list row and an
//  AI Plan row — used to present it as `.sheet(item:)` from inside the
//  card that owned the row. `MenuBarExtra(.window)` is a non-activating
//  panel, and presenting or resizing a sheet over one can order the panel
//  out from under the sheet, which is exactly the "the detail view does
//  not appear" report. A menu-bar panel is not a window that can host a
//  modal over itself.
//
//  So this shell owns the one level of navigation there is
//  (`CandidateDetailNavigation`, defined in CandidateDetailView.swift),
//  the two cards report a tapped resource id upward through
//  `onOpenDetail`, and the detail opens over the scrolling body in place —
//  same panel, same window, no presentation at all. The header and footer
//  stay put, so the route out of the panel never disappears behind a
//  drill-down. `scrollingBody` explains why the overview it covers is
//  hidden rather than removed.
//
//  This is also why the sections and the detail are threaded the same
//  `client`/`projectRootsStore`: the detail is now a sibling of the cards
//  rather than something a card presents, so the shell is the place those
//  dependencies meet. It is presentation wiring only — no policy moved up
//  here, and the detail still makes its own `explain` call and reads its
//  own `executable`/`requiresConfirmation`/`fingerprintToken`.
//
//  No new `Divider()`: the back control is a labelled button inside the
//  body, and the two dividers in this file remain the structural
//  header/footer rules they were.
//

import AppKit
import SwiftUI

struct GlomerisPopoverView: View {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    /// HORO-1365: the completed scan and the requested AI plan are owned here,
    /// and handed down to the cards that draw them.
    ///
    /// Here, specifically, because this view is the one place that is both above
    /// every drill-down and below the scene. Above: `navigation` is declared in
    /// this file and read only in this file's body, so no route into a detail
    /// and no route back out of one can reach these objects — a `@StateObject`
    /// is created once per view identity, and re-evaluating a body does not
    /// re-create it. Below: putting them on the `App` would make every progress
    /// line of a scan re-evaluate `App.body`, which constructs the Settings tabs
    /// and with them a synchronous keychain read (see GlomerisMenuBarApp.swift).
    ///
    /// Only the Refresh and Ask buttons write to either one. Nothing observes a
    /// timer, an appearance or a preference change to clear them.
    ///
    /// This is presentation state, and it stays presentation state. Both objects
    /// hold what the CLI reported, verbatim; they classify nothing and decide
    /// nothing, so the standing project rule is not bent by them.
    @StateObject private var scan = ScanState()
    @StateObject private var plan = PlanState()

    /// HORO-1506: the recovery goal, its pre-flight and its result, owned here
    /// for the same two reasons and with more at stake than either sibling.
    ///
    /// A finished recovery run is the only result in this panel that describes
    /// bytes which are already gone. Losing a scan costs a scan; losing a run
    /// report costs the only account anywhere in the UI of what was deleted and
    /// how much came back. So it sits above every drill-down, like the others.
    ///
    /// It decides nothing either. The goal is a number the user chose; everything
    /// else in it is a report the CLI produced.
    @StateObject private var recovery = RecoveryState()

    /// The panel is one level deep: the overview, or one candidate's detail.
    ///
    /// This one IS `@State`, and correctly so: which candidate the user is
    /// looking at right now is exactly the kind of thing that may reset, and
    /// a panel that opens on the overview rather than on whatever was last
    /// drilled into is the behaviour we want.
    @State private var navigation = CandidateDetailNavigation()

    /// HORO-1508: the disk-pressure reader, observed rather than owned.
    ///
    /// The one store in this panel that is *not* created here, and it cannot be.
    /// It polls while nothing is on screen, so it has to outlive this view — it is
    /// created once by `GlomerisMenuBarApp` and handed down. The note on that
    /// property explains why it is a plain `let` up there rather than an observed
    /// store, which is the same reason the three above are owned down here.
    @ObservedObject private var pressure: PressureEpisodeMonitor

    init(
        pressure: PressureEpisodeMonitor,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.pressure = pressure
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            scrollingBody
            Divider()
            footer
        }
        .frame(width: GlomerisDesign.popoverWidth)
    }

    /// The overview and the detail share one place in the panel, and the
    /// overview is *hidden* there rather than removed.
    ///
    /// It was removed at first — `if detail else sections` — and that quietly
    /// undid the fix it was part of: the cards kept their results in their own
    /// `@State`, so taking them out of the hierarchy discarded them. Drilling
    /// into a candidate and pressing back landed on "No scan yet", and the user
    /// had to run the scan again to reach any other candidate.
    ///
    /// HORO-1365 moved those results out from under this decision entirely —
    /// `scan` and `plan` are owned by this view, not by the cards, so nothing
    /// this property does can reach them — and a later change of shape here
    /// cannot resurrect that bug. What layering still buys is the rest of the
    /// state a mounted view carries and nobody hoists: scroll offset, the
    /// view-options menu, the status and history cards' own fetches. Coming
    /// back to a list
    /// scrolled to where you left it is the difference between navigation and
    /// a reset, so the layering stays on those grounds.
    ///
    /// `opacity` keeps the overview alive and out of sight,
    /// `allowsHitTesting(false)` keeps its scroll view from taking the wheel
    /// events meant for the detail, and `accessibilityHidden` keeps a screen
    /// reader from reading a list the user cannot see. The panel therefore
    /// stays as tall as the overview while a detail is open, which is the cost
    /// of this, and a steady height is no worse than one that jumps on every
    /// drill-down.
    private var scrollingBody: some View {
        ZStack(alignment: .top) {
            sections
                .opacity(navigation.isShowingDetail ? 0 : 1)
                .allowsHitTesting(!navigation.isShowingDetail)
                .accessibilityHidden(navigation.isShowingDetail)
            if let resourceId = navigation.resourceId {
                detail(resourceId)
            }
        }
        // HORO-1367: a range, not a ceiling. `maxHeight` alone stated no
        // preference a `ScrollView` would pass on, so the panel sized itself
        // from `MenuBarExtra`'s own default and opened far shorter than the
        // ceiling allowed — see `GlomerisDesign.minBodyHeight`. The floor is
        // the preference; the ceiling still keeps the panel off the screen it
        // is reporting on.
        //
        // On the ZStack and not on `sections`, which is where it went first.
        // Both branches occupy this space, and the detail is the taller one:
        // it stacks a back bar, a header, the Clean button, a refusal line and
        // an outcome message around its own inner scroll region. Bounding only
        // the overview would leave the one surface that can actually outgrow
        // the display unbounded, and the detail's inner
        // `.frame(maxHeight: maxBodyHeight)` is not that bound — it is the
        // shape this ticket's own diagnosis says states no preference at all.
        .frame(
            minHeight: Self.bodyHeightLimits.min,
            maxHeight: Self.bodyHeightLimits.max
        )
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: GlomerisDesign.inlineSpacing) {
            // The same mark that is in the menu bar, so the panel and the
            // thing that opened it are recognisably one product.
            // `.renderingMode(.template)` is explicit rather than relying on
            // the image's own `isTemplate`, so the mark follows the label
            // colour in both appearances instead of staying flat black.
            Image(nsImage: MenuBarAppearance.menuBarImage(pointSize: 15))
                .renderingMode(.template)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(MenuBarAppearance.title)
                .font(GlomerisDesign.titleFont)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, GlomerisDesign.outerPadding)
        .padding(.vertical, GlomerisDesign.cardPadding)
    }

    // MARK: - Body

    /// Status → recovery goal → candidates → AI plan → history. Also the
    /// VoiceOver reading order.
    ///
    /// HORO-1506 puts the recovery goal second, directly under the disk reading
    /// it works from. The order is the product's own claim about itself: the disk
    /// is this full, here is the goal, here is the material, here is optional
    /// help with prioritising it. Recovery above the candidates list because
    /// setting a goal is the capability and the list is what it draws on — a user
    /// who never opens the list should still be able to say "get me down to 70%
    /// used" and have Glomeris work toward it. AI Plan stays last of the three
    /// for HORO-1308's reason, which this change strengthens rather than
    /// disturbs: Glomeris's own ranking is the default answer, and a provider's
    /// advice is a second opinion on it.
    ///
    /// HORO-1308 puts the AI Plan card *after* the candidates list rather
    /// than above it, on purpose and not for lack of prominence: Glomeris's
    /// own ranking is the default answer to "what should I clean", and a
    /// provider's advice is a second opinion on it. Leading with the advice
    /// would invert that, and the AI Plan card's own "Glomeris's own ranking
    /// is the list above" line would stop being true. It also keeps the
    /// unconfigured case out of the way: a user with no provider sees an
    /// opt-in prompt below a panel that already works, not a dead card at the
    /// top of one.
    private var sections: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                StatusHealthSectionView()
                // HORO-1508 AC 6, above the recovery goal and below the status:
                // it is a question about the disk this panel has just described,
                // and the answer to it is the card underneath. It draws nothing
                // when no episode is open, so the ordinary panel is unchanged.
                PressureAlertSectionView(monitor: pressure)
                RecoverySectionView(
                    recovery: recovery,
                    client: client,
                    projectRootsStore: projectRootsStore
                )
                CandidatesSectionView(
                    scan: scan,
                    client: client,
                    projectRootsStore: projectRootsStore,
                    onOpenDetail: { navigation.open($0) }
                )
                AiPlanSectionView(
                    plan: plan,
                    client: client,
                    projectRootsStore: projectRootsStore,
                    onOpenDetail: { navigation.open($0) }
                )
                HistoryAuditSectionView()
            }
            .padding(GlomerisDesign.outerPadding)
        }
    }

    /// The height range to offer the body, bounded by the display.
    ///
    /// Read from `NSScreen` rather than a `GeometryReader`, because the
    /// question is how much room the *panel* may take on the display, and a
    /// geometry proxy inside the panel can only report the space the panel
    /// has already been given.
    ///
    /// The shortest display attached, not `NSScreen.main`. `main` is the screen
    /// with the focused window, and this panel is non-activating — it never
    /// becomes key, so `main` reports whatever the user was in before they
    /// clicked the menu bar. That guess has a safe direction and an unsafe one:
    /// too *little* height costs nothing, because the body scrolls internally
    /// and no state becomes unreachable, while too much asks for height the
    /// display the panel actually opened on does not have, and puts the footer
    /// — Settings and Quit — off the bottom of the screen. Taking the minimum
    /// removes the unsafe direction outright. It is also free in practice:
    /// every display that ships on or alongside a Mac clears the height at
    /// which the comfortable range is capped anyway, so a second monitor only
    /// changes this number if it is genuinely small, which is the case where
    /// being conservative is right.
    private static var bodyHeightLimits: (min: CGFloat, max: CGFloat) {
        let visibleHeight = NSScreen.screens
            .map(\.visibleFrame.height)
            .min() ?? 0
        return GlomerisDesign.bodyHeightLimits(visibleScreenHeight: visibleHeight)
    }

    // MARK: - Detail

    /// One candidate, over the hidden overview. `CandidateDetailView` brings
    /// its own inner `ScrollView`, so this is deliberately not wrapped in a
    /// second one — nested scroll views inside a 480pt panel are a worse
    /// reading experience than the sheet this replaced.
    private func detail(_ resourceId: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            backBar
            CandidateDetailView(
                resourceId: resourceId,
                client: client,
                projectRootsStore: projectRootsStore
            )
            // `id:` so switching candidates without going back through the
            // overview rebuilds the view, and its `.task { loadExplain() }`
            // runs for the resource actually being shown.
            .id(resourceId)
        }
    }

    private var backBar: some View {
        HStack(spacing: 0) {
            Button {
                navigation.back()
            } label: {
                Label("All candidates", systemImage: "chevron.left")
            }
            .buttonStyle(.borderless)
            .font(GlomerisDesign.captionFont)
            // Escape is what a sheet used to answer to, and it is the shortcut
            // a user who has just drilled in will reach for.
            .keyboardShortcut(.escape, modifiers: [])
            .accessibilityLabel("Back to all candidates")
            .accessibilityHint("Returns to the status, candidates and history overview")
            Spacer(minLength: 0)
        }
        .padding(.horizontal, GlomerisDesign.outerPadding)
        .padding(.top, GlomerisDesign.cardPadding)
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: GlomerisDesign.sectionSpacing) {
            settingsButton
            Spacer(minLength: 0)
            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.borderless)
            .font(GlomerisDesign.captionFont)
            .accessibilityLabel("Quit Glomeris")
        }
        .padding(.horizontal, GlomerisDesign.outerPadding)
        .padding(.vertical, GlomerisDesign.cardPadding)
    }

    /// HORO-1510: the `#available` shim this used to hold inline now lives in
    /// `GlomerisSettingsLink`, because a second surface needs the same route in
    /// and a version check copied per surface is one that goes stale in one copy.
    private var settingsButton: some View {
        GlomerisSettingsLink(
            title: "Project roots…",
            accessibilityLabel: "Open project roots settings"
        )
    }
}

#Preview {
    // Constructed and deliberately never `start()`ed: a preview that polled would
    // spawn a `glomeris pressure` process every thirty seconds for as long as the
    // canvas stayed open, and the pressure card draws nothing until a poll has
    // reported an episode anyway. What the preview shows is the panel without one,
    // which is the ordinary case.
    GlomerisPopoverView(pressure: .production())
}
