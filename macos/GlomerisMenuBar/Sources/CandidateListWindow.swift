//
//  CandidateListWindow.swift
//  GlomerisMenuBar
//
//  HORO-2157: a bounded initial presentation for the candidates card.
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
    /// Rows drawn before the user asks for more.
    static let initialLimit = 25
    /// Rows added by each "Show more".
    static let step = 25

    /// The prefix of `items` that fits in `limit`, and how many were left out.
    /// A non-positive limit is treated as the initial limit rather than
    /// hiding everything.
    static func window<T>(_ items: [T], limit: Int) -> (shown: [T], hiddenCount: Int) {
        let bound = limit > 0 ? limit : initialLimit
        guard items.count > bound else { return (items, 0) }
        return (Array(items.prefix(bound)), items.count - bound)
    }

    /// The limit after one "Show more", never past the number of rows.
    static func expanded(from limit: Int, total: Int) -> Int {
        min(max(limit, initialLimit) + step, max(total, initialLimit))
    }

    /// One line counting the COMPLETE scan by safety class, e.g.
    /// "219 found: 2 safe to reclaim, 5 ask first, 134 protected, 78 not enough evidence".
    /// Classes with no members are omitted. Counts come from the same
    /// `keptToken` mapping the filter uses, so the summary and the filter
    /// cannot disagree about what a class is.
    static func safetySummary(of candidates: [DetectCandidateReportDto]) -> String {
        let parts: [String] = CandidateSafetyFilter.allCases.compactMap { filter in
            guard let token = filter.keptToken else { return nil }
            let count = candidates.filter { $0.policyLabel == token }.count
            guard count > 0 else { return nil }
            return "\(count) \(filter.summaryNoun)"
        }
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

/// The "N hidden" line and its single "Show more" control. Kept out of
/// `CandidatesSectionView.swift` on purpose: that file's source is pinned to
/// exactly two `Button` constructions (Refresh and the row tap) so that "the
/// list has no cleanup control" stays mechanical. This control only widens
/// how many rows are drawn.
struct CandidateShowMoreView: View {
    let hiddenCount: Int
    let onShowMore: () -> Void

    var body: some View {
        if hiddenCount > 0 {
            HStack {
                Text("\(hiddenCount) more hidden")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                Button("Show more", action: onShowMore)
                    .buttonStyle(.borderless)
                    .font(GlomerisDesign.captionFont)
                    .accessibilityLabel(
                        "Show \(min(CandidateListWindow.step, hiddenCount)) more of \(hiddenCount) hidden candidates"
                    )
            }
        }
    }
}
