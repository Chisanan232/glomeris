//
//  GlomerisMenuBarApp.swift
//  GlomerisMenuBar
//
//  ============================================================================
//  STANDING PROJECT RULE — READ BEFORE ADDING ANY CODE TO THIS TARGET
//  ============================================================================
//  GlomerisMenuBar is a THIN CLIENT ONLY, over the existing Rust `glomeris`
//  CLI's `--json` output (see HVDL-26 / Jira epic HORO-1043, "Glomeris MVP
//  2.0 — macOS menu-bar app"). This Swift codebase renders and lets a human
//  approve/decline what the Rust CLI already decided.
//
//  NO policy classification (AUTO_SAFE / ASK / PROTECTED), NO evidence
//  correlation, NO action planning, and NO filesystem execution logic may
//  EVER live here — not as a shortcut, not as an optimization, not "just
//  this once." All of that is owned by the Rust crate under `src/` and is
//  reached exclusively by spawning the `glomeris` binary and parsing its
//  JSON output. If a feature seems to require decision-making logic in
//  Swift, that is a signal the logic belongs in the Rust CLI instead.
//
//  This file is intentionally minimal: HORO-1059 is the menu-bar-icon +
//  empty-popover skeleton only. Spawning the Rust binary is a separate,
//  later ticket (HORO-1060).
//  ============================================================================
//

import SwiftUI

@main
struct GlomerisMenuBarApp: App {
    /// HORO-1508: the disk-pressure reader, and the only thing in this app that
    /// runs when nothing is on screen.
    ///
    /// A plain `let`, emphatically not an `@StateObject`, and the note on `body`
    /// below is the reason: an observed store at this level re-evaluates `body` on
    /// every change, and `body` constructs the Settings tabs. This monitor publishes
    /// on every poll, all day. The measured consequence of hoisting an observed
    /// store to this scene was the menu-bar item vanishing mid-session.
    ///
    /// It still has to live here rather than in the popover, because the popover
    /// does not exist until somebody clicks the icon — and a user who has to open
    /// the app to be told the disk is full has not been told anything. The card in
    /// the popover observes this object; nothing in this file does.
    private let pressure = PressureEpisodeMonitor.production()
    /// HORO-1453: one menu-bar item per logged-in user, decided before any scene
    /// exists.
    ///
    /// `init` is the only place this can go, and the alternatives were both
    /// measured rather than reasoned about:
    ///
    ///   * `body`, or a view's `onAppear`, is too late — by then the status item
    ///     is already on its way to the menu bar, and a duplicate icon that
    ///     appears and then disappears is still a duplicate icon.
    ///   * An `NSApplicationDelegate` hook is later still, which is not obvious:
    ///     SwiftUI evaluates this `body` and instantiates the whole scene graph
    ///     *inside* `App.main()`, before `NSApplication.finishLaunching` delivers
    ///     any delegate callback. A sampled stack of a second instance built that
    ///     way was sitting in `body` → `Settings` → `TabView` with the delegate
    ///     not yet constructed, so it had not had the chance to yield and the
    ///     duplicate process stayed. Worse, anything that blocks during scene
    ///     construction — see the note on `Settings` below — means the hook is
    ///     never reached at all.
    ///
    /// `init` runs before `body` is ever asked for, so a process that is going to
    /// yield never draws anything and never depends on the scene graph getting as
    /// far as launching.
    ///
    /// `exit(0)` rather than a graceful shutdown for the same reason: there is
    /// nothing to tear down yet. No window, no status item, no store, no child
    /// process — `GlomerisPopoverView` owns all of that and has not been built.
    ///
    /// See SingleInstanceGuard.swift for what was measured about the launch routes
    /// and for why the rule is "the launch wins".
    init() {
        if !SingleInstanceGuard.enforce() {
            exit(0)
        }
        // After the guard, never before. A process that is about to yield must not
        // start polling, must not register a notification category, and must not
        // ask for notification authorisation on behalf of the instance that is
        // staying — two readers would also mean two banners for one episode, which
        // `lastRaised` cannot prevent across processes.
        pressure.start()
    }

    /// HORO-1365 deliberately does NOT put the scan and the plan here.
    ///
    /// They belong above navigation, and `GlomerisPopoverView` is already above
    /// navigation — it is the `MenuBarExtra` content root, and the drill-down it
    /// owns happens strictly inside its own body. Hoisting them one level
    /// further, to this scene, buys nothing and costs something real: an
    /// `@StateObject` here makes every change to either store re-evaluate
    /// `body`, and `body` *constructs* the `Settings` tabs below, whether or not
    /// a Settings window is open. `AiProviderPreferencesView.init` reads the
    /// keychain synchronously to seed its status, so a scene-level store turned
    /// one scan — which publishes on every progress line — into a burst of
    /// main-thread `SecItemCopyMatching` calls. On a bundle whose code identity
    /// the keychain ACL does not recognise, each one can block behind a
    /// `SecurityAgent` prompt, and while it is blocked this app has no menu-bar
    /// item at all: no way in, and nothing on screen explaining why.
    ///
    /// Measured on a build that did hoist them here: the item vanished
    /// mid-session on the first Refresh. That is a worse failure than the bug
    /// being fixed, so the owner is the popover root. See OverviewState.swift.
    var body: some Scene {
        // HORO-1305: a custom template image rather than a `systemImage`
        // name. `Image(nsImage:)` is used instead of drawing the
        // `GlomerisMarkShape` directly in the label because a status item
        // needs an AppKit template image to be recoloured correctly for
        // light/dark menu bars, highlight state and tinted wallpapers.
        MenuBarExtra {
            GlomerisPopoverView()
        } label: {
            Image(nsImage: MenuBarAppearance.menuBarImage())
                .accessibilityLabel(MenuBarAppearance.title)
        }
        .menuBarExtraStyle(.window)

        // HORO-1067: the project-roots preference surface. A standard
        // SwiftUI `Settings` scene (Cmd+, / app menu "Settings…") — pure
        // presentation over ProjectRootsStore, no detect/explain/execute
        // wiring.
        //
        // HORO-1309 added the second tab. Both panes stay presentation over a
        // store; the AI tab additionally runs two read-only `glomeris`
        // subcommands from button actions (`llm-check`, `llm-plan
        // --print-payload`) and renders the tokens they print. Neither
        // classifies anything — see that file's header.
        //
        // HORO-1310 added the third. Autopilot is the one feature that grants
        // standing permission to delete without asking again, so it cannot be
        // CLI-only: the person granting it is the least likely to be reading
        // `--help`. That pane renders `autopilot show --json` and writes
        // through `autopilot enable|revoke`; every choice it offers — the
        // kinds, the ceilings, the refusals — arrives from the CLI as data,
        // and there is no way to start a run from it.
        //
        // HORO-1507 added the fourth, and it is the one that shipped as a CLI
        // flag first and should not have: the two numbers deciding when Glomeris
        // speaks up and where recovery stops are the product's own settings, not
        // arguments to a command. That pane renders `settings show --json` and
        // writes through `settings set` — including the limits each number has to
        // stay inside, so its steppers cannot compose a value the CLI refuses.
        // Nothing on it deletes anything.
        Settings {
            TabView {
                ProjectRootsPreferencesView()
                    .tabItem {
                        Label("Projects", systemImage: "folder")
                    }
                // Ahead of AI Provider on purpose (campaign §13): the recovery
                // goal is the product capability and AI assistance is optional
                // help with it, so the goal must not read as a setting reached
                // past the model's.
                RecoveryPreferencesView()
                    .tabItem {
                        Label("Recovery", systemImage: "gauge")
                    }
                AiProviderPreferencesView()
                    .tabItem {
                        Label("AI Provider", systemImage: "sparkles")
                    }
                AutopilotPreferencesView()
                    .tabItem {
                        Label("Autopilot", systemImage: "bolt.badge.automatic")
                    }
            }
        }
    }
}
