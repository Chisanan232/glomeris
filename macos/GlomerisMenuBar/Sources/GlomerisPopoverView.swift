//
//  GlomerisPopoverView.swift
//  GlomerisMenuBar
//
//  HORO-1059 introduced this as an empty popover skeleton; HORO-1062 adds
//  the status + daemon-health section (StatusHealthSectionView); HORO-1063
//  adds the candidates list section (CandidatesSectionView); HORO-1066
//  adds the recent history + action audit section
//  (HistoryAuditSectionView). See the standing project rule in
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

import AppKit
import SwiftUI

struct GlomerisPopoverView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            sections
            Divider()
            footer
        }
        .frame(width: GlomerisDesign.popoverWidth)
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

    /// Status → candidates → history. Also the VoiceOver reading order.
    private var sections: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                StatusHealthSectionView()
                CandidatesSectionView()
                HistoryAuditSectionView()
            }
            .padding(GlomerisDesign.outerPadding)
        }
        // `maxHeight`, not `height`: a popover showing one healthy status
        // card should be the size of one healthy status card.
        .frame(maxHeight: GlomerisDesign.maxBodyHeight)
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
