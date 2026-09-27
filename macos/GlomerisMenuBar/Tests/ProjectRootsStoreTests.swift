//
//  ProjectRootsStoreTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1067: ProjectRootsStore persistence and argument-shaping tests.
//
//  HORO-1501: the argument-shaping half now asserts WHICH commands the roots
//  are shaped for, because the store used to format them for any caller that
//  asked and one of the callers was `status`, which rejects them.
//

import XCTest

// The test target compiles ProjectRootsStore.swift directly as one of its
// own sources (see project.yml) rather than importing the app module,
// matching GlomerisClientTests's pattern — GlomerisMenuBar is an
// `LSUIElement` app target with no importable framework product.

final class ProjectRootsStoreTests: XCTestCase {
    /// Fresh storage per test, so tests never see each other's persisted
    /// state and never touch the domain the app itself writes to —
    /// `UserDefaults.standard`, which for a bundled app is the domain named
    /// by its bundle identifier (HORO-1456). In memory rather than a named
    /// suite: see `TestUserDefaults` (HORO-1486).
    private func makeDefaults() -> UserDefaults {
        TestUserDefaults.inMemory()
    }

    func testAddedRootPersistsAcrossFreshStoreInstance() {
        let defaults = makeDefaults()
        ProjectRootsStore(defaults: defaults).addRoot("/Users/dev/project-a")

        // A fresh store instance over the same UserDefaults simulates an
        // app restart — reading must not depend on any in-memory cache.
        let reopened = ProjectRootsStore(defaults: defaults)
        XCTAssertEqual(reopened.roots, ["/Users/dev/project-a"])
    }

    func testRemovingRootActuallyRemovesIt() {
        let defaults = makeDefaults()
        let store = ProjectRootsStore(defaults: defaults)
        store.addRoot("/Users/dev/project-a")
        store.addRoot("/Users/dev/project-b")

        store.removeRoot("/Users/dev/project-a")

        XCTAssertEqual(store.roots, ["/Users/dev/project-b"])
        // Removal takes effect on the next read with no restart needed —
        // confirm via a fresh instance too, same as the persistence test.
        XCTAssertEqual(ProjectRootsStore(defaults: defaults).roots, ["/Users/dev/project-b"])
    }

    func testDuplicateRootIsNotStoredTwice() {
        let defaults = makeDefaults()
        let store = ProjectRootsStore(defaults: defaults)

        store.addRoot("/Users/dev/project-a")
        store.addRoot("/Users/dev/project-a")

        XCTAssertEqual(store.roots, ["/Users/dev/project-a"])
    }

    // MARK: - Argument shaping (HORO-1501)
    //
    // These used to assert an unconditional `commandLineArguments` property:
    // the roots, formatted, for whatever the caller was about to run. That is
    // the defect — `status` takes no project-root argument and the Status card
    // appended them anyway, so the card failed for exactly those users who had
    // configured a root. The shape assertions below are the same assertions
    // through the command-aware API that replaced it.

    /// Fresh store over its own storage, with `roots` already configured.
    private func makeStore(roots: [String]) -> ProjectRootsStore {
        let store = ProjectRootsStore(defaults: makeDefaults())
        for root in roots {
            store.addRoot(root)
        }
        return store
    }

    func testProjectRootArgumentShapeForMultipleRoots() {
        let store = makeStore(roots: ["/Users/dev/project-a", "/Users/dev/project-b"])

        XCTAssertEqual(
            store.projectRootArguments(forCommand: "detect"),
            ["--project-root", "/Users/dev/project-a", "--project-root", "/Users/dev/project-b"]
        )
    }

    func testProjectRootArgumentsEmptyWhenNoRootsConfigured() {
        XCTAssertTrue(makeStore(roots: []).projectRootArguments(forCommand: "detect").isEmpty)
    }

    // MARK: - `status` gets no roots, whatever is configured
    //
    // Three separate cases rather than one loop, because the count is the whole
    // variable in the defect: zero roots is the state in which the broken code
    // worked, one is the state in which it started failing, and several is the
    // state a user of the feature is actually in.

    func testStatusInvocationIsUnchangedWithNoRootsConfigured() {
        XCTAssertEqual(makeStore(roots: []).scoped(["status", "--json"]), ["status", "--json"])
    }

    func testStatusInvocationIsUnchangedWithOneRootConfigured() {
        let store = makeStore(roots: ["/Users/dev/project-a"])

        XCTAssertEqual(store.scoped(["status", "--json"]), ["status", "--json"])
        XCTAssertEqual(store.roots.count, 1, "the root must really be configured")
    }

    func testStatusInvocationIsUnchangedWithSeveralRootsConfigured() {
        let store = makeStore(roots: [
            "/Users/dev/project-a",
            "/Users/dev/project-b",
            "/Users/dev/project-c",
        ])

        let arguments = store.scoped(["status", "--json"])

        XCTAssertEqual(arguments, ["status", "--json"])
        XCTAssertFalse(
            arguments.contains("--project-root"),
            "`glomeris status` exits 2 on an unrecognised argument (HORO-1322)"
        )
        XCTAssertEqual(store.roots.count, 3, "the roots must really be configured")
    }

    /// The other invocations that take no roots. Asserted alongside `status`
    /// because the next instance of this bug will be at whichever of them is
    /// edited next, and because two of them lead with a token the CLI reads as a
    /// group rather than a command.
    func testInvocationsThatTakeNoRootsAreUnchanged() {
        let store = makeStore(roots: ["/Users/dev/project-a", "/Users/dev/project-b"])

        for arguments in [
            ["daemon", "status", "--json"],
            ["llm-check", "--json"],
            ["history", "--json", "--limit", "50"],
            ["actions", "history", "--json", "--limit", "50"],
            ["autopilot", "show", "--json"],
            ["autopilot", "revoke", "--json"],
        ] {
            XCTAssertEqual(
                store.scoped(arguments),
                arguments,
                "`\(arguments.joined(separator: " "))` does not take project roots"
            )
        }
    }

    // MARK: - The root-scoped commands still receive every configured root

    func testDetectInvocationReceivesEveryConfiguredRoot() {
        let store = makeStore(roots: ["/Users/dev/project-a", "/Users/dev/project-b"])

        XCTAssertEqual(
            store.scoped(["detect", "--json", "--progress-json"]),
            [
                "detect", "--json", "--progress-json",
                "--project-root", "/Users/dev/project-a",
                "--project-root", "/Users/dev/project-b",
            ]
        )
    }

    func testDetectInvocationIsUnchangedWhenNoRootsAreConfigured() {
        // Not a special case in the CLI — no roots means "search the default
        // locations" — but it is the state in which the Status-card defect was
        // invisible, so it is asserted here too rather than assumed.
        XCTAssertEqual(
            makeStore(roots: []).scoped(["detect", "--json", "--progress-json"]),
            ["detect", "--json", "--progress-json"]
        )
    }

    /// `explain` takes its resource id positionally, and the roots go after it.
    func testExplainKeepsItsPositionalArgumentBeforeTheRoots() {
        let store = makeStore(roots: ["/Users/dev/project-a"])

        XCTAssertEqual(
            store.scoped(["explain", "cargo-target-dir:/Users/dev/a/target", "--json"]),
            [
                "explain", "cargo-target-dir:/Users/dev/a/target", "--json",
                "--project-root", "/Users/dev/project-a",
            ]
        )
    }

    func testLlmPlanInvocationReceivesEveryConfiguredRoot() {
        // The roots bound what leaves the machine for a BYOK provider
        // (HORO-1298), so losing them here would widen egress rather than
        // merely widen a search.
        let store = makeStore(roots: ["/Users/dev/project-a", "/Users/dev/project-b"])

        XCTAssertEqual(
            store.scoped(["llm-plan", "--json", "--progress-json"]),
            [
                "llm-plan", "--json", "--progress-json",
                "--project-root", "/Users/dev/project-a",
                "--project-root", "/Users/dev/project-b",
            ]
        )
    }

    /// The two mid-vector sites get their roots from
    /// `projectRootArguments(forCommand:)`, so the command is named there too.
    func testMutatingAndPreviewCommandsStillReceiveTheirRoots() {
        let store = makeStore(roots: ["/Users/dev/project-a"])
        let expected = ["--project-root", "/Users/dev/project-a"]

        XCTAssertEqual(store.projectRootArguments(forCommand: "execute"), expected)
        XCTAssertEqual(store.projectRootArguments(forCommand: "explain"), expected)
        XCTAssertEqual(store.projectRootArguments(forCommand: "llm-plan"), expected)
    }

    func testProjectRootArgumentsAreEmptyForCommandsThatRejectThem() {
        let store = makeStore(roots: ["/Users/dev/project-a", "/Users/dev/project-b"])

        for command in ["status", "daemon", "llm-check", "history", "actions", "autopilot"] {
            XCTAssertTrue(
                store.projectRootArguments(forCommand: command).isEmpty,
                "`glomeris \(command)` takes no project-root argument"
            )
        }
    }

    /// Order is the order roots were added, for every root-scoped command. The
    /// CLI treats the set as unordered, but an unstable order would make every
    /// argument-vector assertion in this target flaky.
    func testRootOrderFollowsTheOrderTheyWereAdded() {
        let store = makeStore(roots: ["/z-added-first", "/a-added-second"])

        XCTAssertEqual(
            store.projectRootArguments(forCommand: "detect"),
            ["--project-root", "/z-added-first", "--project-root", "/a-added-second"]
        )
    }

    func testAnEmptyArgumentVectorIsReturnedUntouched() {
        XCTAssertEqual(makeStore(roots: ["/Users/dev/project-a"]).scoped([]), [])
    }
}
