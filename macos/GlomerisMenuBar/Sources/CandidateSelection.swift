//
//  CandidateSelection.swift
//  GlomerisMenuBar
//
//  HORO-2173: the pure rules behind the candidates window. Presentation only:
//  nothing here classifies, authorises or removes a candidate, and the scan
//  stays exactly as the CLI reported it.
//

import Foundation

enum CandidateBrowserRows {
    /// The window's rows: the shared filter and order, then an optional search.
    /// Search is a case- and width-insensitive substring match on the resource id
    /// (the path) and the kind token. Width-insensitive because an input method
    /// that types full-width digits and letters ("００００１", measured with a
    /// Japanese-style input source in the HORO-2176 UI run) must find the same
    /// rows as the half-width characters it stands for. It only hides rows from
    /// this list; a hidden row is still in the scan and still counted in the
    /// summary.
    static func rows(
        _ candidates: [DetectCandidateReportDto],
        filter: CandidateSafetyFilter,
        order: CandidateSortOrder,
        search: String
    ) -> [DetectCandidateReportDto] {
        let shaped = CandidateListShaping.shape(candidates, filter: filter, order: order)
        let needle = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return shaped }
        return shaped.filter { matches($0.resourceId, needle) || matches($0.kind, needle) }
    }

    private static func matches(_ text: String, _ needle: String) -> Bool {
        text.range(of: needle, options: [.caseInsensitive, .widthInsensitive]) != nil
    }
}

extension CandidateBrowserRows {
    /// True only when the scan has candidates and the search leaves none of them
    /// showing. An empty scan (never scanned, scanning, failed, nothing found) is
    /// a different situation with its own wording and must not borrow this one.
    static func searchHidesEverything(totalCandidates: Int, shownCount: Int) -> Bool {
        totalCandidates > 0 && shownCount == 0
    }
}

enum CandidateSelection {
    enum Outcome: Equatable {
        /// Nothing was selected.
        case none
        /// The selected resource is in the rows being shown.
        case selected(String)
        /// A resource was selected and is not in the rows being shown: it is
        /// gone from the latest scan, or the filter or search now hides it.
        case unavailable(String)
    }

    /// Reconciles a remembered selection with the rows now shown. A missing row
    /// is never replaced by a neighbour: the detail pane carries the Clean
    /// control, and acting on a row the user did not choose is the one thing this
    /// must not do.
    static func reconcile(selected: String?, shownIds: [String]) -> Outcome {
        guard let selected else { return .none }
        return shownIds.contains(selected) ? .selected(selected) : .unavailable(selected)
    }
}
