//
//  CandidateBrowserWindow.swift
//  GlomerisMenuBar
//
//  HORO-2173: the full candidate list, in a window of its own.
//
//  The popover is a 420pt-wide panel that never becomes key, so a long list
//  there can neither be navigated with the arrow keys nor sensibly nested
//  inside its one scroll view. A scan on a busy Docker host reports hundreds of
//  candidates (219 in the rc.1 run), so the popover now shows a summary and a
//  few rows and opens this window for the rest: a sidebar `List` (table-backed
//  on macOS, so lazy, with arrow-key selection and row semantics for free) and
//  the existing `CandidateDetailView` beside it.
//
//  Presentation only, and it grants nothing:
//
//  - It reads the popover's own `ScanState`; it never runs `detect` (there is
//    no Refresh here, so the single detect call site stays in the popover) and
//    never classifies, filters by policy or reorders what the CLI said.
//  - Every row is shown with the same safety, impact and actionability terms the
//    popover uses, so a Protected or not-actionable row looks the same here.
//  - `CandidateDetailView` is reused unchanged. Whether a Clean control exists,
//    and whether the CLI will honour it, is decided there and in Rust exactly as
//    before; the CLI's execution lock and observed-fingerprint consent refuse a
//    stale or concurrent action, so two surfaces showing one resource is safe.
//  - A selection that is no longer shown (gone from a newer scan, or hidden by
//    the filter or search) is never replaced by a neighbouring row.
//

import AppKit
import SwiftUI

struct CandidateBrowserView: View {
    @ObservedObject var scan: ScanState
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    @State private var selection: String?
    @State private var search = ""

    init(
        scan: ScanState,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.scan = scan
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    private var rows: [DetectCandidateReportDto] {
        CandidateBrowserRows.rows(
            scan.candidates, filter: scan.safetyFilter, order: scan.sortOrder, search: search)
    }

    var body: some View {
        let shown = rows
        let outcome = CandidateSelection.reconcile(selected: selection, shownIds: shown.map(\.resourceId))
        VStack(alignment: .leading, spacing: 0) {
            GlomerisQABanner()
            summaryBar(shownCount: shown.count)
            Divider()
            NavigationSplitView {
                sidebar(shown, outcome: outcome)
                    .navigationSplitViewColumnWidth(min: 340, ideal: 400, max: 560)
            } detail: {
                detailPane(outcome)
            }
        }
        .frame(minWidth: 880, minHeight: 520)
        // A container element of its own: an identifier set directly on this
        // stack is inherited by every child that has none of its own, and was
        // measured replacing the summary's and the banner's.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("glomeris.browser.root")
    }

    // MARK: - Summary and notices

    private func summaryBar(shownCount: Int) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Text(CandidateListWindow.safetySummary(of: scan.candidates))
                    .font(GlomerisDesign.secondaryFont)
                    .accessibilityIdentifier("glomeris.browser.summary")
                Spacer(minLength: 0)
                if shownCount != scan.candidates.count {
                    Text("Showing \(shownCount) of \(scan.candidates.count)")
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                }
            }
            // Additive notices, the same rules the popover card follows: the
            // rows stay, and nothing here lets them stand for a complete scan
            // when one of the places Glomeris looks never answered.
            if scan.isScanning {
                Text(scan.progressStatusText ?? "Scanning for reclaimable space…")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }
            if let advisory = IncompleteDiscoveryWording.advisoryMessage(
                for: scan.failedDetectors, notConfigured: scan.notConfiguredDetectors),
                CandidateListStateMessage.message(for: scan)
                    != IncompleteDiscoveryWording.emptyListMessage(
                        for: scan.failedDetectors, notConfigured: scan.notConfiguredDetectors) {
                GlomerisStateMessageView(message: advisory)
            }
            if let scanCoverageNote = scan.scanCoverageNote {
                GlomerisStateMessageView(message: .notConfigured(scanCoverageNote))
            }
            if let lastErrorMessage = scan.lastErrorMessage {
                GlomerisStateMessageView(message: .failure(lastErrorMessage))
            }
        }
        .padding(.horizontal, GlomerisDesign.outerPadding)
        .padding(.vertical, GlomerisDesign.cardPadding)
    }

    // MARK: - Sidebar

    @ViewBuilder
    private func sidebar(_ shown: [DetectCandidateReportDto], outcome: CandidateSelection.Outcome) -> some View {
        let binding = Binding<String?>(
            get: {
                if case .selected(let id) = outcome { return id }
                return nil
            },
            set: { selection = $0 }
        )
        Group {
            if shown.isEmpty {
                emptyState(shownCount: shown.count)
            } else {
                List(selection: binding) {
                    ForEach(shown) { candidate in
                        row(candidate)
                    }
                }
                .accessibilityLabel("Candidates, \(shown.count)")
                .accessibilityIdentifier("glomeris.browser.list")
            }
        }
        .searchable(text: $search, placement: .sidebar, prompt: "Search path or kind")
        .toolbar {
            ToolbarItem {
                Menu {
                    Picker("Order", selection: $scan.sortOrder) {
                        ForEach(CandidateSortOrder.allCases) { Text($0.label).tag($0) }
                    }
                    Picker("Show", selection: $scan.safetyFilter) {
                        ForEach(CandidateSafetyFilter.allCases) { Text($0.label).tag($0) }
                    }
                } label: {
                    Label(
                        "View options",
                        systemImage: scan.safetyFilter == .all
                            ? "line.3.horizontal.decrease.circle"
                            : "line.3.horizontal.decrease.circle.fill")
                }
                .accessibilityLabel(
                    scan.safetyFilter == .all
                        ? "View options. Showing all candidates, \(scan.sortOrder.label)."
                        : "View options. Filtered to \(scan.safetyFilter.label), \(scan.sortOrder.label).")
            }
        }
    }

    @ViewBuilder
    private func emptyState(shownCount: Int) -> some View {
        if let message = CandidateListStateMessage.message(for: scan) {
            GlomerisStateMessageView(message: message).padding(GlomerisDesign.outerPadding)
        } else if CandidateBrowserRows.searchHidesEverything(
            totalCandidates: scan.candidates.count, shownCount: shownCount)
        {
            GlomerisStateMessageView(
                message: .filteredOut(
                    "No candidates match this search",
                    detail: "Clear the search to see all \(scan.candidates.count) candidates."
                )
            ).padding(GlomerisDesign.outerPadding)
        }
        // Otherwise (a failed scan with no rows) the summary bar above already
        // carries the failure, and saying anything more here would contradict it.
    }

    private func row(_ candidate: DetectCandidateReportDto) -> some View {
        let model = CandidateRowViewModel(candidate)
        return CandidateRowLabel(row: model, showsChevron: false)
            .padding(.vertical, 2)
            // Long identifiers: `GlomerisPathText` truncates in the middle, with
            // the full path in its tooltip and in the spoken label below.
            // A list row, not a Button, so the AXButton/AXPress concern of
            // HORO-1363 does not apply: the List supplies the row and selection
            // semantics, and this supplies the one label.
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(model.accessibilityLabel)
            .accessibilityIdentifier("glomeris.browser.row.\(candidate.resourceId)")
    }

    // MARK: - Detail

    @ViewBuilder
    private func detailPane(_ outcome: CandidateSelection.Outcome) -> some View {
        switch outcome {
        case .selected(let id):
            ScrollView {
                CandidateDetailView(resourceId: id, client: client, projectRootsStore: projectRootsStore)
                    // Rebuilt per resource so its explain task runs for the
                    // candidate actually shown.
                    .id(id)
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("glomeris.browser.detail")
        case .unavailable:
            placeholder(
                "No longer shown",
                "That candidate is not in the latest scan, or the current filter or search hides it. "
                    + "Pick a candidate from the list.",
                identifier: "glomeris.browser.placeholder.unavailable")
        case .none:
            placeholder(
                "Select a candidate",
                "Its evidence and the actions Glomeris will and will not take are shown here.",
                identifier: "glomeris.browser.placeholder.none")
        }
    }

    private func placeholder(_ title: String, _ detail: String, identifier: String) -> some View {
        VStack(spacing: 6) {
            Text(title).font(GlomerisDesign.titleFont)
            Text(detail)
                .font(GlomerisDesign.secondaryFont)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(GlomerisDesign.outerPadding)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier(identifier)
    }
}

// MARK: - Opening it

/// An `NSWindow` that closes on Cmd-W. This is an accessory (`LSUIElement`) app
/// with no menu bar of its own, so the standard Close shortcut has nothing to
/// land on unless the window answers it itself.
final class CandidateBrowserNSWindow: NSWindow {
    /// Key code of the physical W key on an ANSI keyboard.
    private static let physicalWKeyCode: UInt16 = 13

    /// Under a non-ASCII input source (Zhuyin, Cyrillic, ...) the event's
    /// character is that script's letter, not "w", so the character alone would
    /// drop the shortcut for those users. An ASCII character is trusted as is
    /// (Dvorak keeps "w" where it is); only a non-ASCII one falls back to the
    /// physical W key.
    static func isCloseShortcut(_ event: NSEvent) -> Bool {
        guard event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command else { return false }
        let character = event.charactersIgnoringModifiers ?? ""
        if character.unicodeScalars.count == 1, character.unicodeScalars.allSatisfy(\.isASCII) {
            return character == "w"
        }
        return event.keyCode == physicalWKeyCode
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if Self.isCloseShortcut(event) {
            performClose(nil)
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
}

/// Opens the candidates window from the popover's "View all" control.
///
/// Thin and untested by construction (it is all `NSWindow` and `NSApp`, which a
/// test bundle has no business driving); everything that decides anything is in
/// `CandidateBrowserRows` and `CandidateSelection`. The popover owns it and
/// passes its live `ScanState` on every open, so the window can never observe a
/// stale or second scan (the HORO-1365 defect).
@MainActor
final class CandidateBrowserWindowPresenter: ObservableObject {
    private var window: NSWindow?
    private var hostingView: NSHostingView<CandidateBrowserView>?

    func open(scan: ScanState, client: GlomerisClient, projectRootsStore: ProjectRootsStore) {
        let root = CandidateBrowserView(scan: scan, client: client, projectRootsStore: projectRootsStore)
        if let hostingView, let window {
            hostingView.rootView = root
            window.makeKeyAndOrderFront(nil)
        } else {
            let created = CandidateBrowserNSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 1000, height: 640),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered,
                defer: false
            )
            let host = NSHostingView(rootView: root)
            created.title = "\(MenuBarAppearance.title) — Reclaimable space"
            created.contentView = host
            created.minSize = NSSize(width: 880, height: 520)
            // Kept alive across a close so the next "View all" reuses it.
            created.isReleasedWhenClosed = false
            created.center()
            window = created
            hostingView = host
            created.makeKeyAndOrderFront(nil)
        }
        // Ordered front *and* activated: this app is an accessory, so without
        // activating, a window opened for a button the user just pressed can
        // appear behind whatever they were working in.
        NSApp.activate(ignoringOtherApps: true)
    }
}
