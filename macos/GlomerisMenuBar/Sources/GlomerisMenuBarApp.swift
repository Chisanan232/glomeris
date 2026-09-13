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
    var body: some Scene {
        MenuBarExtra("Glomeris", systemImage: "externaldrive.badge.gearshape") {
            GlomerisPopoverView()
        }
        .menuBarExtraStyle(.window)
    }
}
