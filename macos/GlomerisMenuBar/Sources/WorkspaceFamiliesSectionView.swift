//
//  WorkspaceFamiliesSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1511 AC 5: "UI can explain 'this project/worktree family consumes X'
//  while still showing exact action/refusal evidence."
//
//  ============================================================================
//  WHY THIS IS ITS OWN CARD AND NOT PART OF THE CANDIDATES LIST
//  ============================================================================
//  It answers a different question. "Reclaimable space" is ordered biggest
//  first and answers "what is worth doing"; this card is ordered by path and
//  answers "what does this belong to, and what else belongs with it" — which is
//  the question nine checkouts of one repository create, because they appear in
//  that list as nine unrelated rows scattered through it by size.
//
//  It is also the reason the two must not merge. `CandidatesSectionView` has a
//  tested invariant that it contains exactly two controls — Refresh and the row
//  tap that opens a detail — so that the list which names deletable things
//  cannot grow a third way to act on them. Adding a project grouping there
//  would have had to either break that invariant or be inert inside a view
//  whose every element is tappable.
//
//  ============================================================================
//  THIS CARD CANNOT ACT, STRUCTURALLY
//  ============================================================================
//  There is no `Button` in this file, no `GlomerisClient`, no command
//  construction and no `.disabled(...)`. The only control is the disclosure
//  toggle per project, which reveals text. That is checked mechanically by
//  scripts/check-workspace-aggregation-has-no-authority.sh, which grew a Swift
//  half for this ticket — the Rust rule is that the aggregate type carries no
//  permission-shaped field, and the Swift rule is that the surface rendering it
//  has nothing to act with.
//
//  What it shows instead, for every member of every worktree, is that member's
//  own evidence: its kind, its size, its safety class and the
//  `CandidateActionability` sentence built from the candidate's `executable` /
//  `offered_actions` / `refusal_reason`. Same three fields, read the same way,
//  as the list above. So the card can say "this project accounts for 7.5 GB"
//  and, in the same breath, that 5 GB of it asks first and 1 GB of it is
//  refused — rather than presenting a project total as something that could be
//  acted on wholesale.
//
//  ============================================================================
//  IT DRAWS NOTHING WHEN THERE IS NOTHING TO GROUP
//  ============================================================================
//  `DetectReportDto.workspaces` is empty both when no candidate belongs to a
//  git working tree and when the resolved CLI predates the field. Neither is a
//  failure and neither is worth a card, so the panel a user opens is unchanged
//  in both cases — the same rule `PressureAlertSectionView` follows when no
//  episode is open. An empty card headed "Developer projects" would read as
//  "you have none", which is a claim this app has no evidence for.
//

import SwiftUI

struct WorkspaceFamiliesSectionView: View {
    @ObservedObject var scan: ScanState

    /// Rebuilt from the store rather than cached, so the card cannot show a
    /// family from one scan beside candidates from the next: both halves come
    /// from the same `ScanState` assignment (`CandidatesSectionView.runDetect`).
    private var families: [WorkspaceFamilyRowViewModel] {
        WorkspaceFamilies.rows(scan.workspaces, candidates: scan.candidates)
    }

    var body: some View {
        if families.isEmpty {
            EmptyView()
        } else {
            card(families)
        }
    }

    private func card(_ families: [WorkspaceFamilyRowViewModel]) -> some View {
        GlomerisCard(
            title: "Developer projects",
            trailing: families.count == 1 ? "1 project" : "\(families.count) projects"
        ) {
            Text(WorkspaceFamilyRowViewModel.notAuthoritySentence)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            ForEach(families) { family in
                DisclosureGroup {
                    VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
                        ForEach(family.worktrees) { worktree in
                            worktreeView(worktree)
                        }
                    }
                    .padding(.top, GlomerisDesign.inlineSpacing)
                } label: {
                    familySummary(family)
                }
            }
        }
    }

    // MARK: - One project

    /// The always-visible summary. Read as one accessibility element carrying
    /// `family.accessibilityLabel`, which ends in the not-permission sentence:
    /// on screen that sentence is at the top of the card and covers every
    /// project at once, and in speech there is no "top of the card", so each
    /// project says it for itself.
    private func familySummary(_ family: WorkspaceFamilyRowViewModel) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                Text(family.projectName)
                    .font(GlomerisDesign.primaryFont.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: GlomerisDesign.inlineSpacing)
                Text(family.actionableNowTerm.title)
                    .font(GlomerisDesign.secondaryFont)
            }

            Text(
                ([family.worktreeCountText]
                    + family.tallies.map { "\($0.label) \($0.countText)" })
                    .joined(separator: " · ")
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)

            if let confirmation = family.requiresConfirmationTerm {
                Text("Asks first: \(confirmation.title)")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }

            Text(family.outstandingWorkSentence)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if let unmeasured = family.unmeasuredSentence {
                Text(unmeasured)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            GlomerisPathText(path: family.commonDir)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(family.accessibilityLabel)
    }

    // MARK: - One checkout

    private func worktreeView(_ worktree: WorkspaceWorktreeRowViewModel) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            VStack(alignment: .leading, spacing: 2) {
                Text("\(worktree.checkoutText) · \(worktree.branchText)")
                    .font(GlomerisDesign.secondaryFont)
                    .fixedSize(horizontal: false, vertical: true)

                Text(stateLine(worktree))
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                if let changes = worktree.localChangesText {
                    Text(changes)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                }

                Text(worktree.workInProgressSentence)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                GlomerisPathText(path: worktree.root)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(worktree.accessibilityLabel)

            ForEach(worktree.members) { member in
                memberView(member)
            }
        }
    }

    /// Activity, publish state and merge state as one line of prose — and never
    /// as chips. See `GlomerisVocabulary.worktreeMerged`: a green "Already
    /// merged" badge beside a PROTECTED resource would read as clearance, so
    /// the coloured axes on this card stay the ones that describe a resource.
    private func stateLine(_ worktree: WorkspaceWorktreeRowViewModel) -> String {
        var parts = [worktree.activityTerm.title, worktree.upstreamTerm.title]
        if let aheadBehind = worktree.aheadBehindText {
            parts.append(aheadBehind)
        }
        if let against = worktree.comparedAgainstText {
            parts.append("\(worktree.mergedTerm.title) into \(against)")
        } else {
            parts.append(worktree.mergedTerm.title)
        }
        return parts.joined(separator: " · ")
    }

    // MARK: - One resource inside a checkout

    /// The member's own evidence, from the candidate its resource id names.
    ///
    /// This is the half of AC 5 that keeps the other half honest: the project
    /// total above is an attention figure, and this is what Glomeris will
    /// actually do about each part of it.
    private func memberView(_ member: WorkspaceMemberRowViewModel) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            if let kind = member.kindTerm, let impact = member.impactTerm,
                let safety = member.safetyTerm, let actionability = member.actionability
            {
                HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                    Text(kind.title)
                        .font(GlomerisDesign.captionFont)
                        .lineLimit(1)
                    Spacer(minLength: GlomerisDesign.inlineSpacing)
                    GlomerisBadgeView(term: safety)
                    Text(impact.title)
                        .font(GlomerisDesign.captionFont)
                }
                Text(actionability.sentence)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Text("This resource is not in the current scan.")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }
            GlomerisPathText(path: member.id)
        }
        .padding(.leading, GlomerisDesign.cardPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(member.accessibilityLabel)
    }
}

#Preview {
    WorkspaceFamiliesSectionView(scan: ScanState())
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
