//
//  SingleInstanceGuardTests.swift
//  GlomerisMenuBarTests
//
//  HORO-1453. The founder saw several identical Glomeris icons at once; the
//  accessibility tree showed one item per process, so the bug was that several
//  processes were alive, and the fix is a rule about which of them may be.
//
//  What is asserted here is the rule and the property it exists for: after any
//  launch, from any route, at any version, from any bundle path, exactly one
//  instance is left running. `SingleInstanceGuard.decide` is pure, so that
//  property can be asserted as a simulation over many iterations rather than by
//  launching and killing real applications — which the live lifecycle check does
//  separately, and which cannot run in CI.
//
//  The live routes those simulated launches stand in for were measured, not
//  assumed: `open` on an already-running bundle adds no process, while `open -n`,
//  direct invocation of the executable, and `open` on a second bundle path with
//  the same identifier each add one. The three that add a process are the three
//  modelled below.
//

import XCTest

final class SingleInstanceGuardTests: XCTestCase {
    // MARK: - Fixtures

    private static let canonical = "/Users/dev/Applications/GlomerisMenuBar.app"
    private static let otherPath = "/Users/dev/glomeris-dogfood-mvp2/GlomerisMenuBar.app"

    private func instance(
        _ pid: pid_t,
        path: String = SingleInstanceGuardTests.canonical,
        version: String? = "0.2.0",
        launched: TimeInterval? = 0
    ) -> GlomerisInstance {
        GlomerisInstance(
            processIdentifier: pid,
            bundlePath: path,
            version: version,
            launchDate: launched.map { Date(timeIntervalSince1970: $0) }
        )
    }

    // MARK: - AC 1: a fresh launch runs

    func testAnInstanceWithNoPeersJustRuns() {
        let decision = SingleInstanceGuard.decide(own: instance(100), others: [])

        XCTAssertEqual(decision, .runAlone)
        XCTAssertTrue(decision.keepRunning)
        XCTAssertTrue(decision.terminate.isEmpty)
        XCTAssertNil(decision.yieldedTo)
    }

    /// `NSRunningApplication.runningApplications(withBundleIdentifier:)` includes
    /// the calling process, so the rule has to drop it. If it did not, every
    /// launch would try to terminate itself.
    func testTheCallingProcessIsNeverItsOwnPeer() {
        let own = instance(100)
        let decision = SingleInstanceGuard.decide(own: own, others: [own])

        XCTAssertEqual(decision, .runAlone)
        XCTAssertFalse(decision.terminate.contains(100))
    }

    // MARK: - AC 2: relaunching the same build adds nothing

    /// The `open -n` and direct-invocation case. The instance already running is
    /// the same build from the same place, so a second one has nothing to add and
    /// exits rather than putting a second icon in the menu bar.
    func testAnIdenticalInstanceIsYieldedToRatherThanDuplicated() {
        let decision = SingleInstanceGuard.decide(
            own: instance(200, launched: 100),
            others: [instance(100, launched: 0)]
        )

        XCTAssertFalse(decision.keepRunning, "this process must not add a second status item")
        XCTAssertEqual(decision.yieldedTo, 100)
        XCTAssertTrue(decision.terminate.isEmpty, "the incumbent is this same build; leave it alone")
    }

    /// Nothing flickers: the surviving item belongs to the process that already
    /// had it, so the icon never disappears and reappears.
    func testYieldingNeverTerminatesTheInstanceItYieldsTo() {
        let incumbent = instance(100, launched: 0)
        let decision = SingleInstanceGuard.decide(own: instance(200, launched: 100), others: [incumbent])

        XCTAssertFalse(decision.terminate.contains(incumbent.processIdentifier))
    }

    func testTheOldestIdenticalInstanceIsTheOneKept() {
        let decision = SingleInstanceGuard.decide(
            own: instance(400, launched: 300),
            others: [instance(300, launched: 200), instance(100, launched: 0), instance(200, launched: 100)]
        )

        XCTAssertEqual(decision.yieldedTo, 100)
        XCTAssertFalse(decision.keepRunning)
    }

    // MARK: - AC 3: an update supersedes the build it replaces

    func testANewerVersionAtTheSamePathSupersedesTheOlderOne() {
        let decision = SingleInstanceGuard.decide(
            own: instance(200, version: "0.3.0", launched: 100),
            others: [instance(100, version: "0.2.0", launched: 0)]
        )

        XCTAssertTrue(decision.keepRunning)
        XCTAssertEqual(decision.terminate, [100])
        XCTAssertNil(decision.yieldedTo)
    }

    /// The documented trade-off, asserted so it cannot change by accident: the
    /// launch wins, not the higher version number. Launching an older build is
    /// something a person did on purpose, and refusing it silently would be the
    /// more surprising behaviour.
    func testAnOlderVersionAlsoSupersedesBecauseTheLaunchWins() {
        let decision = SingleInstanceGuard.decide(
            own: instance(200, version: "0.1.0", launched: 100),
            others: [instance(100, version: "0.3.0", launched: 0)]
        )

        XCTAssertTrue(decision.keepRunning)
        XCTAssertEqual(decision.terminate, [100])
    }

    // MARK: - AC 4: a new bundle path does not leave the old one owning an icon

    /// The install route that produced the reported bug: each DogFood cycle
    /// copies the app beside the last one instead of over it, and macOS
    /// deduplicates by path, so both ran.
    func testADifferentBundlePathAtTheSameVersionIsStillSuperseded() {
        let decision = SingleInstanceGuard.decide(
            own: instance(200, path: Self.canonical, launched: 100),
            others: [instance(100, path: Self.otherPath, launched: 0)]
        )

        XCTAssertTrue(decision.keepRunning)
        XCTAssertEqual(decision.terminate, [100])
        XCTAssertNil(decision.yieldedTo, "a different bundle is not this bundle, whatever it says")
    }

    /// An instance whose version could not be read is the one least safe to leave
    /// holding a menu-bar item, so it is superseded rather than mistaken for an
    /// equivalent.
    func testAnInstanceWithAnUnreadableVersionIsNotTreatedAsEquivalent() {
        let peerUnknown = SingleInstanceGuard.decide(
            own: instance(200, launched: 100),
            others: [instance(100, version: nil, launched: 0)]
        )
        XCTAssertEqual(peerUnknown.terminate, [100])
        XCTAssertTrue(peerUnknown.keepRunning)

        let ownUnknown = SingleInstanceGuard.decide(
            own: instance(200, version: nil, launched: 100),
            others: [instance(100, launched: 0)]
        )
        XCTAssertEqual(ownUnknown.terminate, [100])
        XCTAssertTrue(ownUnknown.keepRunning)
    }

    // MARK: - Both clauses at once

    /// A stale build and an identical one running together — which is exactly the
    /// state a machine reaches after a few DogFood cycles. The stale one is asked
    /// to quit and this process yields to the identical one, so the session lands
    /// on one instance without restarting the one that was already correct.
    func testAStaleInstanceIsRetiredEvenWhileYieldingToAnIdenticalOne() {
        let decision = SingleInstanceGuard.decide(
            own: instance(300, launched: 200),
            others: [
                instance(100, launched: 0),
                instance(200, path: Self.otherPath, version: "0.1.0", launched: 100),
            ]
        )

        XCTAssertEqual(decision.terminate, [200])
        XCTAssertFalse(decision.keepRunning)
        XCTAssertEqual(decision.yieldedTo, 100)
    }

    // MARK: - Ordering

    func testSupersededInstancesAreListedOldestFirst() {
        let decision = SingleInstanceGuard.decide(
            own: instance(400, version: "0.3.0", launched: 300),
            others: [
                instance(300, version: "0.2.2", launched: 200),
                instance(100, version: "0.2.0", launched: 0),
                instance(200, version: "0.2.1", launched: 100),
            ]
        )

        XCTAssertEqual(decision.terminate, [100, 200, 300])
    }

    /// A directly-invoked instance is reported with no launch date at all —
    /// measured, not assumed. It sorts oldest, and more importantly the rule must
    /// not stop working for it.
    func testAnInstanceWithNoLaunchDateIsStillHandled() {
        let decision = SingleInstanceGuard.decide(
            own: instance(400, version: "0.3.0", launched: 300),
            others: [
                instance(200, version: "0.2.0", launched: 100),
                instance(100, version: "0.2.0", launched: nil),
            ]
        )

        XCTAssertEqual(decision.terminate, [100, 200])
        XCTAssertTrue(decision.keepRunning)
    }

    func testTwoPeersWithTheSameLaunchDateAreOrderedByProcessIdentifier() {
        let decision = SingleInstanceGuard.decide(
            own: instance(400, version: "0.3.0", launched: 300),
            others: [
                instance(300, version: "0.2.0", launched: 100),
                instance(100, version: "0.2.0", launched: 100),
            ]
        )

        XCTAssertEqual(decision.terminate, [100, 300])
    }

    // MARK: - AC 6: the property, over many iterations

    /// The acceptance criterion the ticket states as a runbook — at least five
    /// install/update/relaunch cycles, one instance every time — asserted as a
    /// property of the rule rather than as five hand-written cases.
    ///
    /// The simulation is deliberately hostile: it alternates the three launch
    /// routes that actually add a process, changes the bundle path on some
    /// iterations and the version on others, and re-launches an identical build on
    /// the rest. After every single launch the world must hold exactly one
    /// instance.
    func testEveryLaunchRouteConvergesOnOneInstanceOverManyIterations() {
        var world: [GlomerisInstance] = []
        var nextPid: pid_t = 1000
        var clock: TimeInterval = 0

        // path, version: a fresh install to a new directory, an in-place update, a
        // plain relaunch of the same build, a downgrade, and a build directory.
        let launches: [(String, String)] = [
            (Self.canonical, "0.2.0"),
            (Self.canonical, "0.2.0"),
            (Self.otherPath, "0.2.0"),
            (Self.otherPath, "0.3.0"),
            (Self.canonical, "0.3.0"),
            (Self.canonical, "0.3.0"),
            (Self.canonical, "0.1.0"),
            (Self.otherPath, "0.1.0"),
        ]

        for (iteration, launch) in launches.enumerated() {
            nextPid += 1
            clock += 10
            let launched = instance(nextPid, path: launch.0, version: launch.1, launched: clock)

            let decision = SingleInstanceGuard.decide(own: launched, others: world)
            world.removeAll { decision.terminate.contains($0.processIdentifier) }
            if decision.keepRunning {
                world.append(launched)
            }

            XCTAssertEqual(
                world.count, 1,
                "iteration \(iteration + 1) of \(launches.count) left \(world.count) instances: "
                    + world.map { "pid \($0.processIdentifier)" }.joined(separator: ", ")
            )
        }

        // …and the survivor is the build that was launched last, from the path it
        // was launched from — not merely "some Glomeris instance".
        XCTAssertEqual(world.first?.bundlePath, Self.otherPath)
        XCTAssertEqual(world.first?.version, "0.1.0")
        XCTAssertEqual(world.first?.processIdentifier, nextPid)
    }

    /// The same property from the other direction: whatever the decision, the
    /// instances it retires plus the one left running must account for every
    /// instance that was alive. A rule that forgot one would leave an icon behind.
    func testNoInstanceIsEverLeftUnaccountedFor() {
        let peers = [
            instance(100, launched: 0),
            instance(200, path: Self.otherPath, version: "0.1.0", launched: 100),
            instance(300, version: nil, launched: 200),
        ]

        for ownVersion in ["0.1.0", "0.2.0", "0.3.0", nil] {
            for ownPath in [Self.canonical, Self.otherPath] {
                let own = instance(400, path: ownPath, version: ownVersion, launched: 300)
                let decision = SingleInstanceGuard.decide(own: own, others: peers)

                var survivors = peers
                    .filter { !decision.terminate.contains($0.processIdentifier) }
                    .map(\.processIdentifier)
                if decision.keepRunning { survivors.append(own.processIdentifier) }

                XCTAssertEqual(
                    survivors.count, 1,
                    "own=\(ownPath) \(ownVersion ?? "nil") left \(survivors) running"
                )
                if !decision.keepRunning {
                    XCTAssertEqual(
                        survivors, [decision.yieldedTo],
                        "the survivor must be the instance that was yielded to"
                    )
                }
            }
        }
    }

    // MARK: - Where the rule is applied

    /// The rule has exactly one place it can run, and both ways of getting it
    /// wrong were reached in practice while fixing this ticket.
    ///
    /// Too late in the obvious way: `body`, or a view's `onAppear`. By then the
    /// status item is already on its way to the menu bar, so the duplicate icon
    /// appears and then vanishes, which is still a duplicate icon.
    ///
    /// Too late in the non-obvious way: an `NSApplicationDelegate` hook. SwiftUI
    /// instantiates the whole scene graph inside `App.main()`, before
    /// `NSApplication.finishLaunching` delivers a single delegate callback —
    /// measured on a build wired to `applicationWillFinishLaunching`, where a
    /// second instance sampled long after launch was still inside `body` with its
    /// delegate not yet constructed, and both processes were running. The live
    /// lifecycle check failed on exactly that.
    ///
    /// Asserted on the source text because neither the scene body nor an
    /// application launch can be exercised from a unit test, and what has to hold
    /// is a fact about where the call is written.
    func testTheGuardRunsFromTheAppInitialiserAndNowhereElse() throws {
        let entryPoint = Self.stripComments(try Self.readSource("GlomerisMenuBarApp.swift"))

        // The initialiser: everything between the start of the App struct and the
        // scene body it must run before.
        let structStart = try XCTUnwrap(entryPoint.range(of: "struct GlomerisMenuBarApp: App {"))
        let bodyStart = try XCTUnwrap(entryPoint.range(of: "var body: some Scene"))
        let beforeBody = entryPoint[structStart.upperBound..<bodyStart.lowerBound]

        XCTAssertTrue(
            beforeBody.contains("init() {"),
            "the app needs an initialiser to decide in, before any scene exists"
        )
        XCTAssertTrue(
            beforeBody.contains("SingleInstanceGuard.enforce()"),
            "the initialiser must be what enforces the rule"
        )
        XCTAssertTrue(
            beforeBody.contains("exit(0)"),
            "a yielding process has to actually exit, before it draws anything"
        )

        // Not from a delegate callback: that arrives after the scene graph is
        // built, and after anything blocking in it.
        XCTAssertFalse(
            entryPoint.contains("NSApplicationDelegateAdaptor"),
            "an application-delegate hook runs after the scene graph is "
                + "instantiated, which is too late to prevent a second status item"
        )

        // And not from the scene body, whichever file it moves to.
        let sceneBody = entryPoint[bodyStart.lowerBound...]
        XCTAssertFalse(
            sceneBody.contains("SingleInstanceGuard"),
            "the scene body must not enforce the rule — the status item is already "
                + "on its way by then"
        )
        for file in ["GlomerisMenuBarApp.swift", "GlomerisPopoverView.swift"] {
            let text = Self.stripComments(try Self.readSource(file))
            XCTAssertFalse(
                text.contains("onAppear") && text.contains("SingleInstanceGuard"),
                "\(file) must not enforce the rule from a view's lifecycle"
            )
        }
    }

    /// One `MenuBarExtra`, so one process is one icon. This is the fact that made
    /// the live accessibility-tree count meaningful, and a second scene here would
    /// silently invalidate every count in this ticket's evidence.
    func testTheAppDeclaresExactlyOneMenuBarExtra() throws {
        let code = Self.stripComments(try Self.readSource("GlomerisMenuBarApp.swift"))

        XCTAssertEqual(
            code.components(separatedBy: "MenuBarExtra").count - 1, 1,
            "one MenuBarExtra: one process is one icon, which is what makes the live "
                + "accessibility-tree count mean anything"
        )
    }

    // MARK: - Helpers

    /// Comments are stripped so a placement assertion cannot be satisfied — or
    /// broken — by prose. Both files here document the calls they make.
    private static func stripComments(_ source: String) -> String {
        source
            .split(separator: "\n", omittingEmptySubsequences: false)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }

    private static func readSource(_ fileName: String) throws -> String {
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // GlomerisMenuBar
            .appendingPathComponent("Sources/\(fileName)")
        return try String(contentsOf: sourceURL, encoding: .utf8)
    }
}
