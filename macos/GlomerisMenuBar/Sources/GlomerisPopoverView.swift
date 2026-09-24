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

    /// The panel is one level deep: the overview, or one candidate's detail.
    ///
    /// This one IS `@State`, and correctly so: which candidate the user is
    /// looking at right now is exactly the kind of thing that may reset, and
    /// a panel that opens on the overview rather than on whatever was last
    /// drilled into is the behaviour we want.
    @State private var navigation = CandidateDetailNavigation()

    init(
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
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

    /// Status → candidates → AI plan → history. Also the VoiceOver reading
    /// order.
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
        // HORO-1367: a range, not a ceiling. `maxHeight` alone stated no
        // preference a `ScrollView` would pass on, so the panel sized itself
        // from `MenuBarExtra`'s own default and opened far shorter than the
        // ceiling allowed — see `GlomerisDesign.minBodyHeight`. The floor is
        // the preference; the ceiling still keeps the panel off the screen it
        // is reporting on.
        .frame(
            minHeight: Self.bodyHeightLimits.min,
            maxHeight: Self.bodyHeightLimits.max
        )
    }

    /// The height range to offer the body, bounded by the display.
    ///
    /// Read from `NSScreen` rather than a `GeometryReader`, because the
    /// question is how much room the *panel* may take on the display, and a
    /// geometry proxy inside the panel can only report the space the panel
    /// has already been given.
    ///
    /// On a multi-display setup this may take its bound from the display the
    /// user is acting on rather than the one the panel opened over. That
    /// affects only how much height the panel asks for: the body scrolls
    /// internally either way, so no state becomes unreachable if the guess is
    /// the less generous of the two.
    private static var bodyHeightLimits: (min: CGFloat, max: CGFloat) {
        let visibleHeight = NSScreen.main?.visibleFrame.height
            ?? NSScreen.screens.first?.visibleFrame.height
            ?? 0
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

    @ViewBuilder private var settingsButton: some View {
        if #available(macOS 14.0, *) {
            // The supported way in: it opens the app's `Settings` scene
            // without this view knowing anything about window management.
            SettingsLink {
                Text("Project roots…")
            }
            .buttonStyle(.borderless)
            .font(GlomerisDesign.captionFont)
            .accessibilityLabel("Open project roots settings")
        } else {
            // macOS 13 has no `SettingsLink`. The responder-chain action the
            // standard Settings menu item sends is the documented route on
            // that version; the deployment target is 13.0, so it has to be
            // here even though 14+ takes the branch above.
            Button("Project roots…") {
                NSApp.sendAction(Selector(("showPreferencesWindow:")), to: nil, from: nil)
            }
            .buttonStyle(.borderless)
            .font(GlomerisDesign.captionFont)
            .accessibilityLabel("Open project roots settings")
        }
    }
}

#Preview {
    GlomerisPopoverView()
}
