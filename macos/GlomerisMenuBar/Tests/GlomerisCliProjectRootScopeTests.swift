//
//  GlomerisCliProjectRootScopeTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1501: the project-root scope table, over every command this app runs.
//
//  The defect being pinned is a false positive, not a false negative: `status`
//  was *given* roots it does not accept, and the CLI exits 2. So the cases that
//  matter most here are the ones asserting `false` — and they are asserted for
//  every command the app invokes, not only for `status`, because the next
//  occurrence of this bug will be at whichever call site is added next.
//

import XCTest

final class GlomerisCliProjectRootScopeTests: XCTestCase {
    /// Every argument vector the app builds today, with the answer each one
    /// must get. Taken from the invocations in the nine views that call
    /// `GlomerisClient.run(_:)`; `scripts/check-app-cli-invocations-match-cli-contract.sh`
    /// is what keeps this list equal to the real set of call sites, and what
    /// keeps the `true` rows equal to the commands the Rust CLI really accepts
    /// `--project-root` on.
    private static let invocations: [(arguments: [String], acceptsRoots: Bool)] = [
        // Root-scoped: the roots change what these four return.
        (["detect", "--json", "--progress-json"], true),
        (["explain", "cargo-target-dir:/Users/dev/a/target", "--json"], true),
        (["llm-plan", "--json", "--progress-json"], true),
        (["llm-plan", "--print-payload", "--json"], true),
        (
            [
                "execute", "--action-id", "clean-cargo-target",
                "--resource-id", "cargo-target-dir:/Users/dev/a/target",
                "--json", "--progress-json",
            ],
            true
        ),
        // Not root-scoped. `status` is the one this ticket exists for: it
        // reports disk figures and runs no detectors, so a root cannot change
        // its answer — and it rejects the flag outright since HORO-1322.
        (["status", "--json"], false),
        (["daemon", "status", "--json"], false),
        (["llm-check", "--json"], false),
        (["history", "--json", "--limit", "50"], false),
        (["actions", "history", "--json", "--limit", "50"], false),
        (["autopilot", "show", "--json"], false),
        (["autopilot", "revoke", "--json"], false),
        (["autopilot", "enable", "--json", "--kinds", "cargo_target_dir"], false),
        // HORO-1507. Two numbers in a config file. Nothing about them is
        // discovered, so there is nothing for a root to scope — and a
        // `--project-root` on a command that writes a global setting would read
        // as a per-project setting, which is not what it would be.
        (["settings", "show", "--json"], false),
        (
            [
                "settings", "set",
                "--notify-at-used-percent", "85",
                "--default-goal-used-percent", "60",
                "--json",
            ],
            false
        ),
        // HORO-1508. The pressure surface reads and writes one episode record
        // about the whole volume. Nothing is discovered, so there is nothing for
        // a root to scope — and scoping it would be worse than useless: an
        // episode is opened by the daemon, which has no notion of which project
        // the app happens to have configured, so a per-project answer here could
        // not match the record it is answering about.
        (["pressure", "show", "--json"], false),
        (["pressure", "notified", "--json"], false),
        (["pressure", "respond", "review_and_recover", "--json"], false),
    ]

    func testEveryInvocationTheAppBuildsGetsTheRightAnswer() {
        for invocation in Self.invocations {
            XCTAssertEqual(
                GlomerisCliProjectRootScope.acceptsProjectRoots(invocation.arguments),
                invocation.acceptsRoots,
                "\(invocation.arguments.joined(separator: " ")) was classified wrongly"
            )
        }
    }

    /// The list above has to keep covering both answers, or a future edit that
    /// dropped every `false` row would leave a green suite asserting nothing
    /// about the case that broke.
    func testTheInvocationListCoversBothAnswers() {
        XCTAssertGreaterThanOrEqual(Self.invocations.filter { $0.acceptsRoots }.count, 4)
        XCTAssertGreaterThanOrEqual(Self.invocations.filter { !$0.acceptsRoots }.count, 13)
    }

    func testStatusIsNotRootScopedUnderAnyArgumentShape() {
        // Including the shape the defect produced, which is what a regression
        // would look like: the roots already appended.
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(["status"]))
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(["status", "--json"]))
        XCTAssertFalse(
            GlomerisCliProjectRootScope.acceptsProjectRoots(
                ["status", "--json", "--project-root", "/Users/dev/a"]
            )
        )
        XCTAssertFalse(GlomerisCliProjectRootScope.accepts(command: "status"))
    }

    /// The command is the first token and only the first token. A vector that
    /// merely mentions a root-scoped command later on is not that command —
    /// `explain` takes a resource id positionally, so a token that looks like a
    /// command can legitimately appear in argument position.
    func testOnlyTheLeadingTokenDecides() {
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(["status", "detect"]))
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(["history", "explain"]))
        XCTAssertTrue(GlomerisCliProjectRootScope.acceptsProjectRoots(["explain", "status"]))
    }

    /// An unrecognised command gets nothing. The alternative — defaulting to
    /// "attach them" — is how this bug would come back the first time someone
    /// adds a command whose flags this table has not learned.
    func testUnknownAndEmptyCommandsGetNoRoots() {
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots([]))
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(["emergency", "--json"]))
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots(["not-a-command"]))
        XCTAssertFalse(GlomerisCliProjectRootScope.acceptsProjectRoots([""]))
    }

    /// `accepts(command:)` and `acceptsProjectRoots(_:)` must not be able to
    /// disagree — the second exists only to spare its callers the
    /// `arguments.first` unwrap, and two answers for one question is the shape
    /// of the defect this table replaces.
    func testTheTwoEntryPointsAgree() {
        for invocation in Self.invocations {
            guard let command = invocation.arguments.first else { continue }
            XCTAssertEqual(
                GlomerisCliProjectRootScope.accepts(command: command),
                GlomerisCliProjectRootScope.acceptsProjectRoots(invocation.arguments),
                "the two entry points disagree about \(command)"
            )
        }
    }

    /// The table's contents, stated once so that growing it is a deliberate act
    /// with a test to update rather than a silent widening of what the app
    /// attaches roots to.
    ///
    /// Five, not the seven the CLI accepts: `clean` is a command this app never
    /// invokes, and `autopilot run` would be given none on purpose (see
    /// `GlomerisCliProjectRootScope`'s header). The Rust side of that comparison
    /// is mechanical and lives in
    /// `scripts/check-app-cli-invocations-match-cli-contract.sh`.
    ///
    /// `free` joined the set in HORO-1506, when the Recovery card became the
    /// first place the app invokes it. It belongs here because it discovers
    /// before it reclaims: unscoped, its preview would count an opportunity
    /// drawn from a different set of directories than the candidate list shown
    /// beneath it, and the run that follows would reclaim from a third set.
    func testTheTableIsExactlyTheFiveRootScopedCommands() {
        XCTAssertEqual(
            GlomerisCliProjectRootScope.rootScopedCommands,
            ["detect", "explain", "llm-plan", "execute", "free"]
        )
    }
}
