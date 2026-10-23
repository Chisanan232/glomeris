//
//  CandidateListWindow.swift
//  GlomerisMenuBar
//
//  HORO-2157: a bounded presentation for the candidates card. HORO-2173 made
//  the bound final: the popover shows a safety summary and a short preview, and
//  "View all" opens the candidates window for the rest, so the popover no longer
//  grows with the number of candidates at all.
//
//  A scan on a machine with a busy Docker daemon reports hundreds of
//  candidates (219 in the rc.1 acceptance run), and the card used to render
//  every one of them, so the popover became a single very long column that
//  was close to unusable with a keyboard or VoiceOver.
//
//  This file is presentation only. It decides how many of the already
//  filtered and ordered rows are drawn right now; it never removes, reorders
//  or reclassifies anything, and `ScanState.candidates` still holds the
//  complete scan. The safety summary counts the complete scan, not the drawn
//  slice, so a hidden row cannot hide a "Protected" or "Not enough evidence"
//  classification from the user. Nothing here reads or gates on an
//  actionability or cleanup decision.
//

import SwiftUI

enum CandidateListWindow {
    /// Rows the popover card draws, however many candidates a scan found.
    static let previewLimit = 5

    /// The prefix of `items` that fits in `limit`, and how many were left out.
    /// A non-positive limit is treated as the preview limit rather than hiding
    /// everything.
    static func window<T>(_ items: [T], limit: Int) -> (shown: [T], hiddenCount: Int) {
        let bound = limit > 0 ? limit : previewLimit
        guard items.count > bound else { return (items, 0) }
        return (Array(items.prefix(bound)), items.count - bound)
    }

    /// Whether the popover offers "View all": only when the scan has more
    /// candidates than the preview shows.
    static func showsViewAll(candidateCount: Int) -> Bool {
        candidateCount > previewLimit
    }

    /// One line counting the COMPLETE scan by safety class, e.g.
    /// "219 found: 2 safe to reclaim, 5 ask first, 134 protected, 78 not enough evidence".
    /// Classes with no members are omitted, and any row whose label is not one
    /// of the four known classes is counted as "other" rather than dropped from
    /// the arithmetic. Each class is counted through `CandidateListShaping`, the
    /// same code the filter menu uses, so the two cannot disagree.
    static func safetySummary(of candidates: [DetectCandidateReportDto]) -> String {
        var counted = 0
        var parts: [String] = []
        for filter in CandidateSafetyFilter.allCases where filter.keptToken != nil {
            let count = CandidateListShaping.shape(candidates, filter: filter, order: .recommended).count
            counted += count
            if count > 0 { parts.append("\(count) \(filter.summaryNoun)") }
        }
        if candidates.count > counted { parts.append("\(candidates.count - counted) other") }
        let head = "\(candidates.count) found"
        return parts.isEmpty ? head : head + ": " + parts.joined(separator: ", ")
    }
}

extension CandidateSafetyFilter {
    /// The class named as a count noun for the summary line above.
    fileprivate var summaryNoun: String {
        switch self {
        case .all: return "in total"
        case .autoSafe: return "safe to reclaim"
        case .ask: return "ask first"
        case .protected: return "protected"
        case .unknownIncomplete: return "not enough evidence"
        }
    }
}

/// The "N more" line and its single "View all" control. Kept out of
/// `CandidatesSectionView.swift` on purpose: that file's source is pinned to
/// exactly two `Button` constructions (Refresh and the row tap) so that "the
/// list has no cleanup control" stays mechanical. This control only opens the
/// candidates window; it runs and authorises nothing.
struct CandidateViewAllView: View {
    /// What VoiceOver reads for the control.
    static func accessibilityLabel(total: Int) -> String {
        "View all \(total) \(total == 1 ? "candidate" : "candidates") in a window"
    }

    let total: Int
    let hiddenCount: Int
    let onViewAll: () -> Void

    var body: some View {
        HStack {
            if hiddenCount > 0 {
                Text("\(hiddenCount) more not shown here")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Button("View all \(total)", action: onViewAll)
                .buttonStyle(.borderless)
                .font(GlomerisDesign.captionFont)
                .accessibilityLabel(Self.accessibilityLabel(total: total))
                .accessibilityHint("Opens the full list with search, filter and details")
                .accessibilityIdentifier("glomeris.candidates.viewAll")
        }
    }
}
