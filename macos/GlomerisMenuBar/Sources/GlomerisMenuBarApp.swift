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
    /// HORO-1365: a completed scan and a requested AI plan are owned here, at
    /// the scene root, and handed down — not held as `@State` by the cards that
    /// draw them.
    ///
    /// The founder report this fixes: refresh, ask for a plan, open one
    /// suggested candidate, press back — and the scan and the plan were gone,
    /// with the overview reading "No scan yet". The cause was ownership, not
    /// rendering: `@State` lives and dies with a view's place in the hierarchy,
    /// and the panel's drill-down is what decides that place. A result the user
    /// waited for (and, for a plan, paid a provider for) must not be something
    /// a navigation gesture can reach.
    ///
    /// `@StateObject` here is the whole point — created once, for the life of
    /// the process, above every view that can be built, hidden, removed or
    /// re-presented. Only the Refresh and Ask buttons write to them.
    ///
    /// This is presentation state, and it stays presentation state. These
    /// objects hold what the CLI reported, verbatim; they classify nothing and
    /// decide nothing, so the standing rule above is not bent by them.
    @StateObject private var scan = ScanState()
    @StateObject private var plan = PlanState()

    var body: some Scene {
        // HORO-1305: a custom template image rather than a `systemImage`
        // name. `Image(nsImage:)` is used instead of drawing the
        // `GlomerisMarkShape` directly in the label because a status item
        // needs an AppKit template image to be recoloured correctly for
        // light/dark menu bars, highlight state and tinted wallpapers.
        MenuBarExtra {
            GlomerisPopoverView(scan: scan, plan: plan)
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
        Settings {
            TabView {
                ProjectRootsPreferencesView()
                    .tabItem {
                        Label("Projects", systemImage: "folder")
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
