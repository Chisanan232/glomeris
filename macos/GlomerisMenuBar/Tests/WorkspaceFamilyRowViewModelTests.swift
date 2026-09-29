//
//  WorkspaceFamilyRowViewModelTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1511 AC 6 — "Tests cover mixed safe/active/dirty/merged members" — and
//  the three ACs the wording layer is where they can be broken: AC 2 (aggregate
//  bytes reconcile with the displayed members), AC 3 (dirty/active/unique work
//  is never made executable because its group looks stale) and AC 5 (the UI can
//  explain "this project consumes X" while still showing exact action/refusal
//  evidence).
//
//  The mixed case is `tests/fixtures/dto/detect_report_with_workspaces.json`,
//  which is a family of three checkouts holding, between them, one AUTO_SAFE
//  member, one ASK member, one PROTECTED member and one unmeasurable one —
//  across a clean merged main, a dirty unmerged feature branch, and a checkout
//  in use whose branch state no probe could read. It is used here rather than
//  reproduced because it is the same fixture the Rust side asserts against, so
//  the two halves of the join cannot drift apart in one place only.
//
//  Hand-built reports cover what one fixture cannot hold at once: a detached
//  HEAD beside a branch state that could not be read, the four local-change
//  combinations, a settled-looking worktree whose `holds_work_in_progress` is
//  nevertheless true, and a family with nothing outstanding in it at all.
//

import XCTest

final class WorkspaceFamilyRowViewModelTests: XCTestCase {
    // MARK: - The mixed fixture

    private static let fixturesDir: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .deletingLastPathComponent() // macos
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("tests/fixtures/dto")
    }()

    private func mixedReport() throws -> DetectReportDto {
        let url = Self.fixturesDir.appendingPathComponent("detect_report_with_workspaces.json")
        return try JSONDecoder().decode(DetectReportDto.self, from: try Data(contentsOf: url))
    }

    private func mixedFamily() throws -> WorkspaceFamilyRowViewModel {
        let report = try mixedReport()
        let rows = WorkspaceFamilies.rows(report.workspaces, candidates: report.candidates)
        return try XCTUnwrap(rows.first)
    }

    /// AC 1: summarised without losing the underlying resource identities.
    func testTheFamilyIsSummarisedWithoutLosingAnyMemberIdentity() throws {
        let family = try mixedFamily()

        XCTAssertEqual(family.projectName, "proj")
        XCTAssertEqual(family.commonDir, "/Users/dev/proj/.git")
        XCTAssertEqual(family.worktreeCountText, "3 worktrees")
        XCTAssertEqual(family.worktrees.count, 3)

        // Every member id survives into a row, with its exact identity: the
        // aggregate is an extra view of the candidates, never a replacement
        // that a person has to trust without being shown what is in it.
        XCTAssertEqual(
            family.worktrees.flatMap { $0.members.map(\.id) },
            [
                "cargo_target_dir:/Users/dev/proj/target",
                "node_modules:/Users/dev/proj-feature/node_modules",
                "docker_build_cache:/Users/dev/proj-feature/.docker",
                "xcode_derived_data:/Users/dev/Library/Developer/Xcode/DerivedData/App-h",
            ]
        )
    }

    /// AC 2: the aggregate figures reconcile with what is on screen. A total
    /// summed from five members and shown above four rows is the failure this
    /// pins — the shortfall would be invisible, because the missing row is
    /// missing.
    func testTheAggregateCountsReconcileWithTheMembersShown() throws {
        let report = try mixedReport()
        let family = try mixedFamily()
        let dto = try XCTUnwrap(report.workspaces.first)

        let shown = family.worktrees.flatMap(\.members)
        XCTAssertEqual(UInt64(shown.count), dto.memberCount)
        XCTAssertTrue(shown.allSatisfy(\.isPresentInThisScan))

        // And the class tally adds up to the same number, so the summary line
        // and the rows below it are two readings of one set.
        let tallied = family.tallies.compactMap { UInt64($0.countText) }.reduce(0, +)
        XCTAssertEqual(tallied, UInt64(shown.count))
    }

    /// AC 2 again, on the bytes: `is_lower_bound` is carried into both figures,
    /// so a project total cannot read as exact when part of it was not measured.
    func testTheFamilyBytesAreWordedAsALowerBoundWhenRustSaysSo() throws {
        let family = try mixedFamily()

        XCTAssertEqual(family.actionableNowTerm.title, "\u{2265} 2.0 GB")
        XCTAssertEqual(try XCTUnwrap(family.requiresConfirmationTerm).title, "\u{2265} 5.0 GB")
        XCTAssertEqual(
            family.unmeasuredSentence,
            "Size unknown for 1 resource here, so these totals are not the whole story."
        )
    }

    /// AC 3 and AC 5 together, and the whole point of the file: each member's
    /// actionability comes from its own candidate, so one family holds all four
    /// answers at once.
    func testEachMemberKeepsItsOwnActionabilityInsideOneFamily() throws {
        let family = try mixedFamily()
        let byId = Dictionary(
            uniqueKeysWithValues: family.worktrees.flatMap { $0.members.map { ($0.id, $0) } }
        )

        XCTAssertEqual(
            byId["cargo_target_dir:/Users/dev/proj/target"]?.actionability,
            .readyToClean
        )
        XCTAssertEqual(
            byId["node_modules:/Users/dev/proj-feature/node_modules"]?.actionability,
            .asksFirstThenCleans
        )
        XCTAssertEqual(
            byId["docker_build_cache:/Users/dev/proj-feature/.docker"]?.actionability,
            .refused(reason: "size could not be measured")
        )
        XCTAssertEqual(
            byId["xcode_derived_data:/Users/dev/Library/Developer/Xcode/DerivedData/App-h"]?
                .actionability,
            .refused(reason: "an active process is using this path")
        )
    }

    /// AC 3 in its sharpest form: a worktree that looks entirely spent — merged
    /// into its default branch, nothing ahead, clean, idle, and reporting no
    /// work in progress — still shows its member as refused, with the reason
    /// Rust gave and no permission of any kind.
    func testAProtectedMemberStaysRefusedInsideAWorktreeThatLooksSpent() throws {
        let family = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/abandoned/.git",
                worktreeCount: 1,
                actionableNowHuman: "0 B",
                actionableNowCount: 0,
                requiresConfirmationCount: 0,
                protectedCount: 1,
                holdingWorkInProgress: 0,
                worktrees: [
                    worktree(
                        root: "/Users/dev/abandoned",
                        branch: "old-feature",
                        activity: "idle",
                        upstream: "tracking",
                        ahead: 0,
                        behind: 40,
                        merged: "merged",
                        mergedInto: "origin/main",
                        holdsWorkInProgress: false,
                        memberResourceIds: ["cargo_target_dir:/Users/dev/abandoned/target"]
                    ),
                ]
            ),
            candidates: [
                candidate(
                    resourceId: "cargo_target_dir:/Users/dev/abandoned/target",
                    policyLabel: "PROTECTED",
                    executable: false,
                    refusalReason: "an active process is using this path"
                ),
            ]
        )

        let member = try XCTUnwrap(family.worktrees.first?.members.first)
        XCTAssertEqual(
            member.actionability, .refused(reason: "an active process is using this path")
        )
        // The refusal survives into what a screen-reader user hears, not just
        // into the model — "already merged" is said in the same breath.
        XCTAssertTrue(
            member.accessibilityLabel.contains("an active process is using this path"),
            member.accessibilityLabel
        )
        XCTAssertEqual(family.worktrees.first?.mergedTerm.title, "Already merged")
    }

    /// The thin-client rule at the one place it guards unpushed work. All four
    /// visible inputs look settled and Rust still says the worktree holds work
    /// in progress — which is what a *failed* probe produces. A Swift OR over
    /// what is on screen would say the opposite, and would be wrong.
    func testTheWorkInProgressFlagIsReadAndNeverRecomputedFromTheFourInputs() throws {
        let family = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/probe-failed/.git",
                worktreeCount: 1,
                holdingWorkInProgress: 1,
                worktrees: [
                    worktree(
                        root: "/Users/dev/probe-failed",
                        branch: "main",
                        dirty: false,
                        untracked: false,
                        activity: "idle",
                        upstream: "tracking",
                        ahead: 0,
                        behind: 0,
                        merged: "merged",
                        mergedInto: "origin/main",
                        holdsWorkInProgress: true,
                        memberResourceIds: []
                    ),
                ]
            ),
            candidates: []
        )

        let worktree = try XCTUnwrap(family.worktrees.first)
        XCTAssertNil(worktree.localChangesText)
        XCTAssertEqual(worktree.activityTerm.title, "Idle")
        XCTAssertEqual(worktree.workInProgressSentence, "Holds work in progress.")
        XCTAssertTrue(
            worktree.accessibilityLabel.contains("Holds work in progress."),
            worktree.accessibilityLabel
        )
        XCTAssertEqual(family.outstandingWorkSentence, "This worktree holds work in progress.")
    }

    /// Said in both directions, because a silent row is nothing to compare
    /// against when rows are heard one at a time.
    func testAWorktreeWithNoOutstandingWorkSaysSoRatherThanSayingNothing() throws {
        let family = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/clean/.git",
                worktreeCount: 2,
                holdingWorkInProgress: 0,
                worktrees: [
                    worktree(
                        root: "/Users/dev/clean",
                        branch: "main",
                        holdsWorkInProgress: false,
                        memberResourceIds: []
                    ),
                ]
            ),
            candidates: []
        )

        XCTAssertEqual(
            family.worktrees.first?.workInProgressSentence,
            "No work in progress was found here."
        )
        XCTAssertEqual(
            family.outstandingWorkSentence,
            "No work in progress was found in 2 worktrees."
        )
    }

    // MARK: - Branch wording

    /// A detached HEAD and a branch state that could not be read both arrive as
    /// `branch: null`, and `WorkspaceWorktreeReportDto.branch` documents the
    /// only thing that tells them apart. Wording the second as the first would
    /// claim a fact about the checkout that no probe established.
    func testABranchStateThatCouldNotBeReadIsNotWordedAsADetachedHead() throws {
        let unreadable = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/unreadable/.git",
                worktreeCount: 1,
                worktrees: [
                    worktree(
                        root: "/Users/dev/unreadable",
                        branch: nil,
                        activity: "unknown",
                        upstream: "unknown",
                        ahead: nil,
                        behind: nil,
                        merged: "unknown",
                        mergedInto: nil,
                        holdsWorkInProgress: true,
                        memberResourceIds: []
                    ),
                ]
            ),
            candidates: []
        )
        let detached = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/detached/.git",
                worktreeCount: 1,
                worktrees: [
                    worktree(
                        root: "/Users/dev/detached",
                        branch: nil,
                        activity: "idle",
                        upstream: "untracked",
                        ahead: nil,
                        behind: nil,
                        merged: "unknown",
                        mergedInto: nil,
                        holdsWorkInProgress: true,
                        memberResourceIds: []
                    ),
                ]
            ),
            candidates: []
        )

        XCTAssertEqual(
            unreadable.worktrees.first?.branchText, "Branch state could not be read"
        )
        XCTAssertEqual(detached.worktrees.first?.branchText, "No branch — detached HEAD")
    }

    /// The counts exist only against an upstream, so there is nothing to word
    /// when there is no upstream — and "0 ahead" would be a claim, not a blank.
    func testAheadAndBehindAreOnlyWordedWhenTheBranchTracksSomething() throws {
        let family = try mixedFamily()

        XCTAssertEqual(
            family.worktrees[0].aheadBehindText,
            "0 commits not yet on its upstream, 12 behind it"
        )
        XCTAssertEqual(
            family.worktrees[1].aheadBehindText,
            "3 commits not yet on its upstream, 0 behind it"
        )
        XCTAssertNil(family.worktrees[2].aheadBehindText)
    }

    func testLocalChangesAreWordedForEachCombinationAndOmittedForNeither() throws {
        let cases: [(dirty: Bool, untracked: Bool, expected: String?)] = [
            (true, true, "Uncommitted changes and untracked files"),
            (true, false, "Uncommitted changes"),
            (false, true, "Untracked files"),
            (false, false, nil),
        ]

        for (dirty, untracked, expected) in cases {
            let family = try decodeFamily(
                self.family(
                    commonDir: "/Users/dev/changes/.git",
                    worktreeCount: 1,
                    worktrees: [
                        worktree(
                            root: "/Users/dev/changes",
                            branch: "main",
                            dirty: dirty,
                            untracked: untracked,
                            holdsWorkInProgress: dirty || untracked,
                            memberResourceIds: []
                        ),
                    ]
                ),
                candidates: []
            )
            XCTAssertEqual(
                family.worktrees.first?.localChangesText,
                expected,
                "dirty=\(dirty) untracked=\(untracked)"
            )
        }
    }

    /// The three git-state axes reach the spoken label as words. §14: state a
    /// screen-reader user cannot hear is state this card does not have.
    func testEveryGitStateAxisIsSpokenAsWords() throws {
        let family = try mixedFamily()

        for worktree in family.worktrees {
            let label = worktree.accessibilityLabel
            for term in [worktree.activityTerm, worktree.upstreamTerm, worktree.mergedTerm] {
                XCTAssertTrue(
                    label.contains(term.axis) && label.contains(term.title),
                    "\(term.axis) / \(term.title) missing from: \(label)"
                )
            }
        }
    }

    // MARK: - The summary is not permission

    /// AC 5's second half, as a property of the spoken label rather than of a
    /// pixel: the family says what it adds up to and that the total is not
    /// permission, and it does not speak any member's verdict — the member rows
    /// do, each for itself. A family label that recited its members' verdicts
    /// would be the place a summary starts sounding like a decision.
    func testTheFamilySummarySaysWhatItIsAndIsNotWithoutSpeakingForItsMembers() throws {
        let family = try mixedFamily()
        let label = family.accessibilityLabel

        XCTAssertTrue(label.contains("proj"), label)
        XCTAssertTrue(label.contains("3 worktrees"), label)
        XCTAssertTrue(label.contains("Ready to clean: \u{2265} 2.0 GB"), label)
        XCTAssertTrue(label.contains("Asks first: \u{2265} 5.0 GB"), label)
        XCTAssertTrue(label.contains(WorkspaceFamilyRowViewModel.notAuthoritySentence), label)

        for member in family.worktrees.flatMap(\.members) {
            let sentence = try XCTUnwrap(member.actionability).sentence
            XCTAssertFalse(label.contains(sentence), "family spoke a member's verdict: \(sentence)")
            XCTAssertTrue(member.accessibilityLabel.contains(sentence), member.accessibilityLabel)
        }
    }

    /// Said for the harmless family too. A project with nothing outstanding and
    /// a large total is exactly where the figure reads as an invitation, so this
    /// is the case the sentence exists for — not the risky one.
    func testTheNotPermissionSentenceIsSaidEvenWhenNothingIsOutstanding() throws {
        let family = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/tidy/.git",
                worktreeCount: 1,
                actionableNowHuman: "30.0 GB",
                actionableNowCount: 1,
                requiresConfirmationCount: 0,
                protectedCount: 0,
                unknownCount: 0,
                unmeasuredCount: 0,
                isLowerBound: false,
                holdingWorkInProgress: 0,
                worktrees: [
                    worktree(
                        root: "/Users/dev/tidy",
                        branch: "main",
                        activity: "idle",
                        upstream: "tracking",
                        ahead: 0,
                        behind: 0,
                        merged: "merged",
                        mergedInto: "origin/main",
                        holdsWorkInProgress: false,
                        memberResourceIds: ["cargo_target_dir:/Users/dev/tidy/target"]
                    ),
                ]
            ),
            candidates: [
                candidate(
                    resourceId: "cargo_target_dir:/Users/dev/tidy/target",
                    reclaimableHuman: "30.0 GB",
                    policyLabel: "AUTO_SAFE",
                    executable: true,
                    requiresConfirmation: false
                ),
            ]
        )

        XCTAssertEqual(family.actionableNowTerm.title, "30.0 GB")
        XCTAssertNil(family.requiresConfirmationTerm)
        XCTAssertNil(family.unmeasuredSentence)
        XCTAssertEqual(family.tallies.map(\.label), ["Ready to clean"])
        XCTAssertTrue(
            family.accessibilityLabel.contains(
                WorkspaceFamilyRowViewModel.notAuthoritySentence
            ),
            family.accessibilityLabel
        )
    }

    // MARK: - Order and naming

    /// Rust's order is kept. Re-ranking here would be a second ranking that can
    /// drift from the one the candidates list already applies, and this card is
    /// an index rather than a leaderboard — so the smaller family stays first
    /// when its path sorts first.
    func testFamiliesAndWorktreesAreShownInTheOrderRustSentThem() throws {
        let json: [String: Any] = [
            "candidates": [],
            "workspaces": [
                family(commonDir: "/Users/dev/aaa/.git", worktreeCount: 1, actionableNowBytes: 1),
                family(
                    commonDir: "/Users/dev/zzz/.git",
                    worktreeCount: 1,
                    actionableNowBytes: 999_999_999
                ),
            ],
        ]
        let report = try decode(DetectReportDto.self, from: json)
        let rows = WorkspaceFamilies.rows(report.workspaces, candidates: report.candidates)

        XCTAssertEqual(rows.map(\.projectName), ["aaa", "zzz"])
    }

    func testTheProjectNameComesFromTheRepositoryDirectoryNotTheGitDirectory() {
        XCTAssertEqual(
            WorkspaceFamilyRowViewModel.projectName(commonDir: "/Users/dev/proj/.git"), "proj"
        )
        // A bare repository's directory *is* `proj.git`, so the suffix stays.
        XCTAssertEqual(
            WorkspaceFamilyRowViewModel.projectName(commonDir: "/srv/git/proj.git"), "proj.git"
        )
        // Nothing left to name falls back to the path rather than to a blank
        // heading, which would leave the card's rows unattributed.
        XCTAssertEqual(WorkspaceFamilyRowViewModel.projectName(commonDir: "/.git"), "/.git")
        XCTAssertEqual(WorkspaceFamilyRowViewModel.projectName(commonDir: "/"), "/")
    }

    // MARK: - The join

    /// A member whose id names no candidate is shown as absent rather than
    /// dropped: a family that quietly listed four of five members would
    /// understate itself against its own byte total, and nothing on screen
    /// would say so.
    func testAMemberMissingFromTheScanIsShownAsAbsentRatherThanDropped() throws {
        let family = try decodeFamily(
            self.family(
                commonDir: "/Users/dev/partial/.git",
                worktreeCount: 1,
                worktrees: [
                    worktree(
                        root: "/Users/dev/partial",
                        branch: "main",
                        holdsWorkInProgress: false,
                        memberResourceIds: ["node_modules:/Users/dev/partial/node_modules"]
                    ),
                ]
            ),
            candidates: []
        )

        let member = try XCTUnwrap(family.worktrees.first?.members.first)
        XCTAssertEqual(member.id, "node_modules:/Users/dev/partial/node_modules")
        XCTAssertFalse(member.isPresentInThisScan)
        XCTAssertNil(member.actionability)
        XCTAssertNil(member.safetyTerm)
        XCTAssertTrue(
            member.accessibilityLabel.contains("not in the current scan"),
            member.accessibilityLabel
        )
        // And it is never silently read as permitted.
        XCTAssertFalse(member.accessibilityLabel.contains("Ready to clean"))
    }

    /// Two different situations produce no families — a machine with no
    /// repository checkouts among its candidates, and a resolved CLI that
    /// predates the field. Neither is a failure, and both have to end in the
    /// card drawing nothing rather than in an empty card headed "Developer
    /// projects", which would assert something this app has not established.
    func testAReportWithNoFamiliesProducesNoRowsAtAll() throws {
        let report = try decode(
            DetectReportDto.self,
            from: [
                "candidates": [
                    candidate(
                        resourceId: "homebrew_cache:/Users/dev/Library/Caches/Homebrew",
                        kind: "homebrew_cache",
                        policyLabel: "AUTO_SAFE",
                        executable: true
                    ),
                ],
            ]
        )

        XCTAssertTrue(report.workspaces.isEmpty)
        XCTAssertTrue(WorkspaceFamilies.rows(report.workspaces, candidates: report.candidates)
            .isEmpty)
    }

    func testCountWordingIsSingularForOne() {
        XCTAssertEqual(WorkspaceCounts.worktrees(1), "1 worktree")
        XCTAssertEqual(WorkspaceCounts.worktrees(9), "9 worktrees")
        XCTAssertEqual(WorkspaceCounts.resources(1), "1 resource")
        XCTAssertEqual(WorkspaceCounts.resources(0), "0 resources")
    }

    // MARK: - JSON builders

    // The DTOs are decode-only mirrors of the Rust reports on purpose, so a
    // test builds the JSON a CLI would print rather than a Swift value a CLI
    // could never produce. Defaults cover the uninteresting fields so each test
    // states only what it is about.

    private func decode<T: Decodable>(_ type: T.Type, from json: [String: Any]) throws -> T {
        let data = try JSONSerialization.data(withJSONObject: json)
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func decodeFamily(
        _ json: [String: Any],
        candidates: [[String: Any]]
    ) throws -> WorkspaceFamilyRowViewModel {
        let report = try decode(
            DetectReportDto.self, from: ["candidates": candidates, "workspaces": [json]]
        )
        let rows = WorkspaceFamilies.rows(report.workspaces, candidates: report.candidates)
        return try XCTUnwrap(rows.first)
    }

    private func worktree(
        root: String,
        branch: String?,
        linkedWorktree: Bool = false,
        dirty: Bool = false,
        untracked: Bool = false,
        activity: String = "idle",
        upstream: String = "tracking",
        ahead: UInt32? = 0,
        behind: UInt32? = 0,
        merged: String = "merged",
        mergedInto: String? = "origin/main",
        patchEquivalence: String = "unknown",
        holdsWorkInProgress: Bool = false,
        memberResourceIds: [String] = []
    ) -> [String: Any] {
        var json: [String: Any] = [
            "root": root,
            "linked_worktree": linkedWorktree,
            "dirty": dirty,
            "untracked": untracked,
            "activity": activity,
            "upstream": upstream,
            "merged": merged,
            "patch_equivalence": patchEquivalence,
            "holds_work_in_progress": holdsWorkInProgress,
            "member_resource_ids": memberResourceIds,
        ]
        // `"unknown"` is the default because it is the one integration state
        // that is coherent beside any `merged` value a test picks — a bounded
        // probe that never ran says nothing either way. It carries its reason,
        // because an integration state without one is the absence this
        // campaign spends its effort refusing (HORO-1545).
        if patchEquivalence == "unknown" {
            json["equivalence_unknown_reason"] = "not_attempted"
        }
        if let ahead { json["ahead"] = ahead }
        if let behind { json["behind"] = behind }
        if let branch { json["branch"] = branch }
        if let mergedInto { json["merged_into"] = mergedInto }
        return json
    }

    private func family(
        commonDir: String,
        worktreeCount: UInt64,
        actionableNowBytes: UInt64 = 0,
        actionableNowHuman: String = "0 B",
        actionableNowCount: UInt64 = 0,
        requiresConfirmationHuman: String = "0 B",
        requiresConfirmationCount: UInt64 = 0,
        protectedCount: UInt64 = 0,
        unknownCount: UInt64 = 0,
        unmeasuredCount: UInt64 = 0,
        isLowerBound: Bool = false,
        holdingWorkInProgress: UInt64 = 0,
        worktrees: [[String: Any]] = []
    ) -> [String: Any] {
        [
            "common_dir": commonDir,
            "worktree_count": worktreeCount,
            "actionable_now_bytes": actionableNowBytes,
            "actionable_now_human": actionableNowHuman,
            "actionable_now_count": actionableNowCount,
            "requires_confirmation_bytes": 0,
            "requires_confirmation_human": requiresConfirmationHuman,
            "requires_confirmation_count": requiresConfirmationCount,
            "protected_count": protectedCount,
            "unknown_count": unknownCount,
            "unmeasured_count": unmeasuredCount,
            "is_lower_bound": isLowerBound,
            "worktrees_holding_work_in_progress": holdingWorkInProgress,
            "worktrees": worktrees,
        ]
    }

    private func candidate(
        resourceId: String,
        kind: String = "cargo_target_dir",
        reclaimableHuman: String? = "1.0 GB",
        policyLabel: String,
        executable: Bool,
        requiresConfirmation: Bool = false,
        refusalReason: String? = nil
    ) -> [String: Any] {
        var json: [String: Any] = [
            "resource_id": resourceId,
            "kind": kind,
            "reclaimable_bytes_is_lower_bound": false,
            "impact_tier": "notable",
            "policy_label": policyLabel,
            "reasons": [],
            "executable": executable,
            "offered_actions": executable
                ? [["action_id": "\(kind).clean", "requires_confirmation": requiresConfirmation]]
                : [],
        ]
        if let reclaimableHuman { json["reclaimable_human"] = reclaimableHuman }
        if let refusalReason { json["refusal_reason"] = refusalReason }
        return json
    }
}
