//
//  RecoveryDeepLinkWindow.swift
//  GlomerisMenuBar
//
//  HORO-1508 AC 2, the surface end: where "Review & recover" actually lands.
//
//  ---------------------------------------------------------------------
//  Why a window and not the popover
//  ---------------------------------------------------------------------
//  The obvious answer would be "open the menu-bar panel, scrolled to Recovery",
//  and it is not available: `MenuBarExtra` has no public API to open its own
//  window programmatically. Nothing in this app can show that panel; only a click
//  on the status item can.
//
//  The other route considered and rejected was a URL scheme — `CFBundleURLTypes`
//  plus an `onOpenURL` handler — which widens the bundle's *public* surface so
//  that anything on the machine can ask Glomeris to open its recovery screen, in
//  order to solve a problem entirely internal to one process. A window this
//  process owns and opens directly is smaller in every dimension that matters.
//
//  ---------------------------------------------------------------------
//  It has its own RecoveryState, and that is not a workaround
//  ---------------------------------------------------------------------
//  The popover's `RecoveryState` is a `@StateObject` created inside
//  `GlomerisPopoverView`, so nothing outside that view can reach it. This window
//  therefore holds its own — which is honest rather than merely necessary: a goal
//  set here is this window's, and a pre-flight measured here was measured against
//  it. What keeps the two from contradicting each other is that neither owns the
//  truth. Every figure either shows came from the CLI, and a run started from
//  either goes through the same executor, whose lock refuses a second
//  concurrent run.
//
//  Nothing here starts a run. The window shows the recovery surface; a human
//  presses the button on it, as they would have from the panel.
//

import AppKit
import SwiftUI

// MARK: - What the window is showing

/// The alert context the window opened on, and the recovery state it hosts.
///
/// Separated from the AppKit presenter below so the part that decides anything is
/// testable: a test may not put a window on screen, but it can assert that
/// arriving from an alert seeds the goal the alert named, and that doing so never
/// overwrites a goal the user has already chosen here.
@MainActor
final class RecoveryDeepLinkWindowModel: ObservableObject {
    /// The alert this window was last opened from, or `nil` when it was opened
    /// without a readable report. Drives the header only.
    @Published private(set) var context: RecoveryDeepLinkContext?

    /// This window's recovery state. Created once and kept, so re-opening the
    /// window while a run's result is on screen does not discard the only account
    /// anywhere in the UI of what was deleted.
    let recovery = RecoveryState()

    /// Takes on a newly arrived alert.
    ///
    /// The goal is *seeded*, not set: `adoptStoredDefaultGoal` declines once the
    /// user has moved the control in this window, which is the behaviour that
    /// matters if a second alert arrives while they are deciding. A `nil`
    /// `goalUsedPercent` — a figure the control cannot represent — leaves the goal
    /// alone rather than seeding a guess.
    func adopt(_ context: RecoveryDeepLinkContext?) {
        self.context = context
        guard let goalUsedPercent = context?.goalUsedPercent else { return }
        recovery.adoptStoredDefaultGoal(goalUsedPercent)
    }
}

/// The window's content: why it opened, then the recovery surface itself.
struct RecoveryDeepLinkWindowView: View {
    @ObservedObject var model: RecoveryDeepLinkWindowModel

    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    init(
        model: RecoveryDeepLinkWindowModel,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.model = model
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                if let context = model.context {
                    header(context)
                }
                RecoverySectionView(
                    recovery: model.recovery,
                    client: client,
                    projectRootsStore: projectRootsStore
                )
            }
            .padding(GlomerisDesign.outerPadding)
        }
        .frame(width: GlomerisDesign.popoverWidth)
    }

    /// Why this window is open, in the figures the alert was raised from.
    ///
    /// Shown above the recovery card rather than folded into it, because it is a
    /// different kind of statement: the card is live and re-reads the disk for
    /// itself, while this is the reading that caused the alert. Both percentages
    /// appear, labelled, for the reason the rest of this campaign keeps
    /// repeating — one says when Glomeris speaks, the other says where a run
    /// stops.
    private func header(_ context: RecoveryDeepLinkContext) -> some View {
        GlomerisCard(title: "Why you are seeing this") {
            GlomerisDetailRow(label: "Disk when alerted") {
                Text(
                    String(
                        format: "%.1f%% used, %@ free",
                        context.currentUsedPercent,
                        context.currentFreeHuman
                    )
                )
                .font(GlomerisDesign.secondaryFont)
            }
            GlomerisDetailRow(label: "You asked to be alerted at") {
                Text(context.notifyAtDescription)
                    .font(GlomerisDesign.secondaryFont)
            }
            GlomerisDetailRow(label: "Default recovery goal") {
                Text(context.goalDescription)
                    .font(GlomerisDesign.secondaryFont)
            }
            // What has happened so far, then what happens next. The first half is
            // the shared account rather than a constant: on a Mac whose grant
            // allows it, the user is on this screen *because* a bounded run
            // already ran and did not reach the goal (HORO-1510), and a window
            // that opened by reassuring them nothing had been deleted would be
            // the most confident wrong sentence in the app.
            GlomerisStateMessageView(
                message: UnpromptedRecoveryAccount.message(after: context.automaticRun)
            )
            Text("Recovery starts when you start it.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .contain)
    }
}

// MARK: - Opening it

/// Brings the recovery window forward when an alert is answered with "Review &
/// recover".
///
/// Deliberately thin, and untested by construction: everything it does is
/// `NSWindow` and `NSApp`, which a test bundle has no business driving, so
/// everything that *decides* anything lives in `RecoveryDeepLinkWindowModel`
/// above, where it is asserted directly.
@MainActor
final class RecoveryDeepLinkWindowPresenter: RecoveryDeepLinkOpening {
    private let model = RecoveryDeepLinkWindowModel()
    private var window: NSWindow?

    func openRecovery(_ context: RecoveryDeepLinkContext?) {
        model.adopt(context)
        let window = existingOrNewWindow()
        // Ordered front *and* activated, because this app is an accessory
        // (`LSUIElement`): without activating, a window it orders front on behalf
        // of a button the user just pressed can appear behind whatever they were
        // working in, which reads as the button having done nothing.
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    private func existingOrNewWindow() -> NSWindow {
        if let window {
            return window
        }
        let created = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: GlomerisDesign.popoverWidth, height: 560),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        created.title = "\(MenuBarAppearance.title) — Recovery"
        created.contentView = NSHostingView(rootView: RecoveryDeepLinkWindowView(model: model))
        // Kept alive across a close. Without this AppKit releases the window when
        // the user closes it, and the next alert answered with "Review & recover"
        // would reach a deallocated object — a crash, in the one path whose whole
        // purpose is that pressing the button does something.
        created.isReleasedWhenClosed = false
        created.center()
        window = created
        return created
    }
}
