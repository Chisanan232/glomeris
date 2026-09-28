//
//  WorkspaceFamilyRowViewModel.swift
//  GlomerisMenuBar
//
//  HORO-1511: turning `detect --json`'s `workspaces` array into the wording
//  the developer-projects card shows.
//
//  ============================================================================
//  WHAT THIS IS FOR
//  ============================================================================
//  A machine with nine checkouts of one repository shows up in the candidates
//  list as nine unrelated-looking rows, in nine different places in a list
//  ordered by size. "This project accounts for 30 GB across nine worktrees" is
//  the sentence that makes that list readable, and `WorkspaceFamilyReportDto`
//  carries the figures for it.
//
//  ============================================================================
//  THE AGGREGATE EXPLAINS. THE MEMBER AUTHORISES. NOT NEGOTIABLE.
//  ============================================================================
//  src/workspace/mod.rs states the rule on the Rust side — a family has no
//  action, no action id and no `executable` field, structurally rather than by
//  convention, and a caller that wants to know what may run has to go back to
//  the member's own candidate report. This file is that caller, and it is the
//  place where the rule could be broken quietly, because it is the first place
//  in the codebase to hold a family and a set of candidates at the same time.
//
//  So it goes back. Every member row here is built from the
//  `DetectCandidateReportDto` its resource id names, and everything it says
//  about what Glomeris may do comes from `CandidateActionability` over that
//  candidate's own `executable` / `offered_actions` / `refusal_reason` — the
//  same three fields, read the same way, as the candidates list above it. The
//  family's counts are never consulted for that question. They cannot be: a
//  count of how many members are protected does not say which ones, and a
//  surface that guessed would be inventing policy in Swift.
//
//  The family's own byte totals are never presented as an amount a run will
//  reclaim, either. They are summed from detector *estimates* (campaign §9
//  reserves completion and progress for re-measured filesystem state), they
//  deliberately exclude protected bulk, and `isLowerBound` says when even the
//  parts that are counted may be larger. `Self.notAuthoritySentence` is said
//  for every family for that reason, rather than only for the ones that look
//  risky: a project with nothing outstanding in it is exactly the case where
//  "30 GB" reads as an invitation.
//
//  ============================================================================
//  NOTHING HERE IS RECOMPUTED
//  ============================================================================
//  `holdsWorkInProgress` is Rust's OR over four inputs — uncommitted changes,
//  untracked files, something using the worktree, commits no remote has — and
//  all four are rendered separately on the same row. Recomputing the flag from
//  them in Swift would be easy, would look like a simplification, and would be
//  the standing thin-client rule broken at the one place it protects a
//  developer's unpushed work: Rust counts a *failed* probe as possible use and
//  as possible unpushed work, and a Swift OR written from what is on screen
//  would not. The flag is read, never derived. `WorkspaceFamilyRowViewModelTests`
//  pins that with a worktree whose four inputs all look settled and whose flag
//  is nevertheless true.
//
//  Nor is anything re-ordered. Families arrive sorted by their shared git
//  directory and worktrees by their root (`group_families` builds both from a
//  `BTreeMap`), which makes this card an index — the same thing
//  `CandidateSortOrder.path` is, and for the same reason: a second ranking
//  here could drift from Rust's, and the one that matters, biggest first, is
//  already what the candidates list does.
//

import Foundation

/// One discovered resource, as a member of a worktree, joined back to the
/// candidate that authorises it.
///
/// The optionals are all one condition: whether the resource id named a
/// candidate in the same report. Within one `detect --json` it always does, and
/// `every_workspace_member_id_names_a_candidate_in_the_same_report`
/// (tests/dto_golden_fixtures.rs) is what keeps that true on the Rust side. The
/// case is modelled anyway, and modelled as "this is not in the scan" rather
/// than by dropping the row, because a family that quietly listed four of its
/// five members would misstate the one figure this card exists to give.
struct WorkspaceMemberRowViewModel: Equatable, Identifiable {
    /// The resource id, which is the path — the same id the candidates list
    /// and the detail view use.
    let id: String

    /// All four `nil` together when `id` named no candidate in this report.
    let kindTerm: GlomerisTerm?
    let impactTerm: GlomerisTerm?
    let safetyTerm: GlomerisTerm?

    /// What Glomeris will do about this resource, from the candidate's own
    /// three actionability fields. Never from the family's counts.
    let actionability: CandidateActionability?

    var isPresentInThisScan: Bool { actionability != nil }

    init(resourceId: String, candidate: DetectCandidateReportDto?) {
        id = resourceId
        guard let candidate else {
            kindTerm = nil
            impactTerm = nil
            safetyTerm = nil
            actionability = nil
            return
        }
        kindTerm = GlomerisVocabulary.kind(candidate.kind)
        impactTerm = GlomerisVocabulary.storageImpact(
            human: candidate.reclaimableHuman,
            isLowerBound: candidate.reclaimableBytesIsLowerBound
        )
        safetyTerm = GlomerisVocabulary.safety(candidate.policyLabel)
        actionability = CandidateActionability(
            executable: candidate.executable,
            offeredActions: candidate.offeredActions,
            refusalReason: candidate.refusalReason
        )
    }

    /// Said on every row, including the permitted ones, for the reason
    /// `CandidateRowViewModel.accessibilityLabel` gives: a sighted reader can
    /// compare two rows and see which of them carries a refusal, and a
    /// screen-reader user hears one row at a time and cannot.
    var accessibilityLabel: String {
        guard let kindTerm, let impactTerm, let safetyTerm, let actionability else {
            return SpokenLabel.compose([
                "This resource is not in the current scan, so there is nothing here to act on.",
                "Path: \(id)",
            ])
        }
        return SpokenLabel.compose([
            kindTerm.title,
            SpokenLabel.clause(safetyTerm.axis, safetyTerm.title),
            SpokenLabel.clause(impactTerm.axis, impactTerm.title),
            SpokenLabel.clause(CandidateActionability.axis, actionability.sentence),
            "Path: \(id)",
        ])
    }
}

/// One checkout of a repository: where it is, what state the developer's work
/// in it is in, and which discovered resources belong to it.
struct WorkspaceWorktreeRowViewModel: Equatable, Identifiable {
    let id: String
    let root: String

    /// "Main checkout" or "Linked worktree" — `git worktree add` siblings are
    /// the ones a person forgets they still have, so which is which is worth
    /// saying rather than leaving to the path.
    let checkoutText: String

    /// The branch name, or what stands in its place. Two different things
    /// stand in its place, and `WorkspaceWorktreeReportDto.branch` documents
    /// how they are told apart: a detached HEAD has no branch but its upstream
    /// and merge state were still read, while a probe that could not run
    /// reports `"unknown"` for both.
    let branchText: String

    let activityTerm: GlomerisTerm
    let upstreamTerm: GlomerisTerm
    let mergedTerm: GlomerisTerm

    /// Which branch the merge answer is about. Rendered beside `mergedTerm`,
    /// because "already merged" without it is half a fact
    /// (`MergedState::compared_against`).
    let comparedAgainstText: String?

    /// `nil` unless the branch tracks an upstream, which is the only case the
    /// counts exist for.
    let aheadBehindText: String?

    /// Uncommitted changes, untracked files, or both. `nil` when neither —
    /// unlike the sentence below, which is always said.
    let localChangesText: String?

    /// Rust's `holds_work_in_progress`, in words. Read, never recomputed from
    /// the four fields above it — see this file's header.
    ///
    /// Said in both directions rather than only when true, because a
    /// screen-reader user has nothing to compare a silent row against.
    let workInProgressSentence: String

    let members: [WorkspaceMemberRowViewModel]

    init(_ dto: WorkspaceWorktreeReportDto, candidates: [String: DetectCandidateReportDto]) {
        id = dto.root
        root = dto.root
        checkoutText = dto.linkedWorktree ? "Linked worktree" : "Main checkout"
        activityTerm = GlomerisVocabulary.worktreeActivity(dto.activity)
        upstreamTerm = GlomerisVocabulary.worktreeUpstream(dto.upstream)
        mergedTerm = GlomerisVocabulary.worktreeMerged(dto.merged)

        if let branch = dto.branch, !branch.isEmpty {
            branchText = branch
        } else if dto.upstream == "unknown" && dto.merged == "unknown" {
            branchText = "Branch state could not be read"
        } else {
            branchText = "No branch — detached HEAD"
        }

        comparedAgainstText = dto.mergedInto

        if let ahead = dto.ahead, let behind = dto.behind {
            aheadBehindText =
                "\(ahead) commits not yet on its upstream, \(behind) behind it"
        } else {
            aheadBehindText = nil
        }

        switch (dto.dirty, dto.untracked) {
        case (true, true):
            localChangesText = "Uncommitted changes and untracked files"
        case (true, false):
            localChangesText = "Uncommitted changes"
        case (false, true):
            localChangesText = "Untracked files"
        case (false, false):
            localChangesText = nil
        }

        workInProgressSentence =
            dto.holdsWorkInProgress
            ? "Holds work in progress."
            : "No work in progress was found here."

        members = dto.memberResourceIds.map {
            WorkspaceMemberRowViewModel(resourceId: $0, candidate: candidates[$0])
        }
    }

    var accessibilityLabel: String {
        SpokenLabel.compose([
            checkoutText,
            SpokenLabel.clause("Branch", branchText),
            SpokenLabel.clause(activityTerm.axis, activityTerm.title),
            SpokenLabel.clause(upstreamTerm.axis, upstreamTerm.title),
            aheadBehindText,
            SpokenLabel.clause(mergedTerm.axis, mergedTerm.title),
            comparedAgainstText.map { "Compared against \($0)" },
            localChangesText,
            workInProgressSentence,
            WorkspaceCounts.resources(UInt64(members.count)) + " here",
            "Path: \(root)",
        ])
    }
}

/// One line of a family's member tally: how many of its resources are in one
/// policy class.
///
/// A count and a label, never a byte figure. `protectedCount` has no
/// `protectedBytes` to pair with it by design (`WorkspaceFamilyReportDto`), and
/// giving the tally a size column would be the first step towards putting bulk
/// a run can never take into a project's reclaimable total.
struct WorkspaceMemberTally: Equatable, Identifiable {
    let id: String
    let label: String
    let countText: String
}

/// Shared count wording, so "1 worktree" and "3 worktrees" are decided once.
enum WorkspaceCounts {
    static func worktrees(_ count: UInt64) -> String {
        count == 1 ? "1 worktree" : "\(count) worktrees"
    }

    static func resources(_ count: UInt64) -> String {
        count == 1 ? "1 resource" : "\(count) resources"
    }
}

/// Every worktree sharing one git directory — one repository's checkouts — and
/// what they add up to.
struct WorkspaceFamilyRowViewModel: Equatable, Identifiable {
    let id: String

    /// The repository directory's own name, for the card's primary line. The
    /// shared git directory is kept as `commonDir` and rendered as secondary
    /// detail: two checkouts of two different `app` repositories are not the
    /// same project, and only the full path tells them apart.
    let projectName: String
    let commonDir: String

    let worktreeCountText: String

    /// The family's estimated AUTO_SAFE bulk, in the same `≥`-for-a-partial-
    /// measurement wording the candidates list uses.
    let actionableNowTerm: GlomerisTerm

    /// The ASK bulk, or `nil` when there is none. Kept separate from the
    /// figure above rather than added to it, because one of them is space a
    /// run could take unattended and the other is space it must be given
    /// permission for.
    let requiresConfirmationTerm: GlomerisTerm?

    /// The non-zero classes only, in the order the policy classes escalate.
    let tallies: [WorkspaceMemberTally]

    /// Present when any member went unmeasured, because the totals above are
    /// then not the whole story — and an unmeasured resource is not a
    /// zero-byte one.
    let unmeasuredSentence: String?

    /// Always present, in both directions.
    let outstandingWorkSentence: String

    let worktrees: [WorkspaceWorktreeRowViewModel]

    /// Spoken on every family, and rendered once at the top of the card — the
    /// same split `CandidateRowViewModel` makes for the same reason: on screen
    /// one line above every project covers all of them, and in speech there is
    /// no "above", so each project has to carry it.
    ///
    /// Said for every family, not only the ones that look risky. See this
    /// file's header: the project with nothing outstanding in it is exactly
    /// where a large total reads as an invitation.
    static let notAuthoritySentence =
        "This total explains where the space is. It is not permission: each resource is still "
        + "cleaned, or refused, on its own evidence."

    init(_ dto: WorkspaceFamilyReportDto, candidates: [String: DetectCandidateReportDto]) {
        id = dto.commonDir
        commonDir = dto.commonDir
        projectName = Self.projectName(commonDir: dto.commonDir)
        worktreeCountText = WorkspaceCounts.worktrees(dto.worktreeCount)

        actionableNowTerm = GlomerisVocabulary.storageImpact(
            human: dto.actionableNowHuman,
            isLowerBound: dto.isLowerBound
        )
        requiresConfirmationTerm =
            dto.requiresConfirmationCount == 0
            ? nil
            : GlomerisVocabulary.storageImpact(
                human: dto.requiresConfirmationHuman,
                isLowerBound: dto.isLowerBound
            )

        tallies = [
            ("Ready to clean", dto.actionableNowCount),
            ("Asks first", dto.requiresConfirmationCount),
            ("Protected", dto.protectedCount),
            ("Not classified", dto.unknownCount),
        ]
        .filter { $0.1 > 0 }
        .map { WorkspaceMemberTally(id: $0.0, label: $0.0, countText: "\($0.1)") }

        unmeasuredSentence =
            dto.unmeasuredCount == 0
            ? nil
            : "Size unknown for \(WorkspaceCounts.resources(dto.unmeasuredCount)) here, so these "
                + "totals are not the whole story."

        let holding = dto.worktreesHoldingWorkInProgress
        if holding == 0 {
            outstandingWorkSentence =
                "No work in progress was found in \(WorkspaceCounts.worktrees(dto.worktreeCount))."
        } else if dto.worktreeCount == 1 {
            outstandingWorkSentence = "This worktree holds work in progress."
        } else {
            outstandingWorkSentence =
                "Work in progress in \(holding) of these "
                + "\(WorkspaceCounts.worktrees(dto.worktreeCount))."
        }

        worktrees = dto.worktrees.map {
            WorkspaceWorktreeRowViewModel($0, candidates: candidates)
        }
    }

    /// The repository directory's name, from the shared git directory a family
    /// is identified by.
    ///
    /// `/Users/dev/proj/.git` is the ordinary shape and gives "proj". A bare
    /// repository's `/srv/git/proj.git` keeps its suffix, because that is its
    /// name. Anything that reduces to nothing falls back to the path itself
    /// rather than to an empty title.
    ///
    /// The empty-string guard is not defensive padding:
    /// `URL(fileURLWithPath: "")` resolves against the process's working
    /// directory, so stripping `/.git` from `"/.git"` and handing the remainder
    /// to `URL` names whatever directory the app happens to be running from.
    static func projectName(commonDir: String) -> String {
        var path = commonDir
        if path.hasSuffix("/.git") {
            path.removeLast("/.git".count)
        }
        guard !path.isEmpty else { return commonDir }
        let name = URL(fileURLWithPath: path).lastPathComponent
        return name.isEmpty || name == "/" ? commonDir : name
    }

    var accessibilityLabel: String {
        SpokenLabel.compose(
            [
                projectName,
                worktreeCountText,
                SpokenLabel.clause("Ready to clean", actionableNowTerm.title),
                requiresConfirmationTerm.map { "Asks first: \($0.title)" },
            ]
                + tallies.map { "\($0.label): \($0.countText)" }
                + [
                    unmeasuredSentence,
                    outstandingWorkSentence,
                    Self.notAuthoritySentence,
                ]
        )
    }
}

/// Builds the card's rows from one `detect --json` report.
///
/// Takes both halves of the report on purpose: the families say what belongs
/// together, and only the candidates say what may be done about any of it.
enum WorkspaceFamilies {
    static func rows(
        _ families: [WorkspaceFamilyReportDto],
        candidates: [DetectCandidateReportDto]
    ) -> [WorkspaceFamilyRowViewModel] {
        // Ids are unique within a report; `uniquingKeysWith` is here so a
        // malformed one produces a wrong row rather than a crash.
        let byId = Dictionary(
            candidates.map { ($0.resourceId, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        return families.map { WorkspaceFamilyRowViewModel($0, candidates: byId) }
    }
}
