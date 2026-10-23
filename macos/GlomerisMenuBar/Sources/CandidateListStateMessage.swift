//
//  CandidateListStateMessage.swift
//  GlomerisMenuBar
//
//  HORO-2173: the "what is this list showing instead of rows" decision, and the
//  row's visual label, shared by the popover card and the candidates window so
//  the two surfaces cannot disagree about an empty, filtered-out, scanning,
//  failed or incomplete scan. Moved verbatim from CandidatesSectionView; no
//  behaviour change and no policy: it only chooses wording for state the CLI
//  already reported.
//

import SwiftUI

enum CandidateListStateMessage {
    static func message(for scan: ScanState) -> GlomerisStateMessage? {
        let visibleCandidates = CandidateListShaping.shape(
            scan.candidates, filter: scan.safetyFilter, order: scan.sortOrder)
        // Checked before the "no candidates at all" cases: a non-empty scan
        // whose rows are all filtered out is a different situation from an
        // empty scan, and must not borrow its wording.
        if !scan.candidates.isEmpty, visibleCandidates.isEmpty {
            return .filteredOut(
                "No candidates match this filter",
                detail: "Glomeris found \(scan.candidates.count) "
                    + "\(scan.candidates.count == 1 ? "candidate" : "candidates"), "
                    + "but none are \(scan.safetyFilter.midSentenceDescription). "
                    + "Change the filter in the view options to see them."
            )
        }
        guard scan.candidates.isEmpty else { return nil }
        if scan.isScanning {
            return .loading("Scanning for reclaimable space…")
        }
        if scan.lastErrorMessage != nil {
            // The failure message below is the whole story; an "all clear"
            // sitting next to it would contradict it.
            return nil
        }
        if scan.lastScannedAt == nil {
            return .notLookedYet(
                "No scan yet",
                detail: "Refresh to look for space you can reclaim."
            )
        }
        // HORO-1484: checked before the all-clear below, because "nothing worth
        // reclaiming" is a claim about the machine and this scan did not earn
        // it — one of the places Glomeris looks never answered, so whatever is
        // there is unknown rather than absent.
        //
        // HORO-1576 put the commonest case of that on the same path: on an
        // installation with no project root configured, three detectors examined
        // nothing, so the all-clear below would be claiming a clean bill of
        // health over directories nobody opened. That is the state a new user
        // sees first, which is the reason it may not be the one state that
        // reassures without evidence.
        if let partial = IncompleteDiscoveryWording.emptyListMessage(
            for: scan.failedDetectors, notConfigured: scan.notConfiguredDetectors) {
            return partial
        }
        return .empty(
            "Nothing worth reclaiming",
            detail: "Everything Glomeris can see is either in use or already small."
        )
    }
}

/// One candidate's row content: what it is and how much is at stake, the safety
/// verdict, the actionability chip and the path. Not a Button; the popover wraps
/// it in its one row Button, the window uses it as a plain list row.
struct CandidateRowLabel: View {
    let row: CandidateRowViewModel
    let showsChevron: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                Text(row.kindTerm.title)
                    .font(GlomerisDesign.primaryFont)
                    .lineLimit(1)
                Spacer(minLength: GlomerisDesign.inlineSpacing)
                GlomerisBadgeView(term: row.impactTerm, filled: false)
            }

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: row.safetyTerm)
                if let impactTierTerm = row.impactTierTerm {
                    // Filled, so it carries weight without carrying a
                    // hue — its tone is `.neutral` at every tier.
                    GlomerisBadgeView(term: impactTierTerm)
                }
                Spacer(minLength: 0)
                // Affordance only: it says "there is more behind this
                // row", which is what a tap does. It carries no state,
                // so it is hidden from VoiceOver rather than read out
                // once per row.
                if showsChevron {
                    Image(systemName: "chevron.right")
                        .imageScale(.small)
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }
            }

            // HORO-1323, and on its own line rather than beside the safety
            // badge. In a 340pt panel a third chip on that line clips on a
            // long safety title, and the thing it would clip is the safety
            // verdict — the one item on the row that must never be lost.
            // Costing a few points of height on the minority of rows that
            // cannot be cleaned is the cheaper trade, and it also makes
            // those rows visibly taller, which is itself part of what the
            // founder pass asked for: a non-deletable item should look
            // different before anyone presses anything.
            if let actionabilityTerm = row.actionabilityTerm {
                GlomerisBadgeView(term: actionabilityTerm)
            }

            GlomerisPathText(path: row.id)
        }
    }
}
