//
//  SingleInstanceGuard.swift
//  GlomerisMenuBar
//
//  ============================================================================
//  THE DEFECT THIS EXISTS FOR — HORO-1453
//  ============================================================================
//  Repeated build/install/update/DogFood cycles left several identical Glomeris
//  icons in the menu bar at once. Measured on a live session: five
//  `GlomerisMenuBar` processes, five distinct `.app` bundles, one shared bundle
//  identifier, and — read from the accessibility tree — exactly one menu-bar
//  item each. So the duplication was never a rendering fault. This app declares
//  one `MenuBarExtra` and creates one status item per process; there were simply
//  five processes.
//
//  Why five. macOS deduplicates by bundle *path*, not by bundle identifier, and
//  only on one of the four routes into this app. Measured, same session:
//
//    open A.app                    -> launches
//    open A.app again              -> no new process; LaunchServices activates
//                                     the one already running
//    open -n A.app                 -> a SECOND instance of the same bundle
//    A.app/Contents/MacOS/<exe>    -> a second instance; LaunchServices is not
//                                     consulted at all
//    open B.app (same bundle id)   -> another instance; a different path is a
//                                     different app as far as LaunchServices is
//                                     concerned
//
//  Every development and DogFood cycle here produces a new bundle path — an
//  install that copies beside the last one rather than over it, an Xcode build
//  directory, a disposable verification bundle — and most of them are launched
//  by running the executable directly. So the one route macOS protects was the
//  one route nobody used, and nothing in the app itself noticed.
//
//  Ruled out by the same measurements, recorded so they are not re-investigated:
//  a single process never owns two status items; the `com.glomeris.monitor`
//  LaunchAgent runs the CLI headless (`daemon run`) and was not even loaded; no
//  Glomeris login item exists; and the Rust CLI has no path that launches a GUI,
//  so the stale `/opt/homebrew/bin` and `/usr/local/bin` copies are not
//  implicated in this at all.
//
//  ============================================================================
//  WHY THIS IS NOT A POLICY DECISION
//  ============================================================================
//  This target's standing rule is that no classification, planning or execution
//  decision may live in Swift. That rule is about *storage* policy — which
//  resources may be deleted, and on whose authority. It has nothing to say about
//  how many copies of this presenter may own the menu bar, which is a macOS
//  application-lifecycle question and cannot be answered anywhere else: the Rust
//  CLI has no view of NSRunningApplication, and no `glomeris` subcommand is
//  consulted here. No resource is inspected, no cleanup is planned, and nothing
//  on disk is touched. The only process this file can end is another instance of
//  this same app, identified by bundle identifier.
//
//  ============================================================================
//  THE RULE
//  ============================================================================
//  The instance the user just launched is the one they asked for.
//
//    * No other instance          -> run.
//    * Another instance of the SAME bundle path at the SAME version already owns
//      the menu bar -> that instance is this instance in every respect a user
//      can perceive, so yield to it and exit. Nothing flickers and no state
//      changes hands. (This is the `open -n` and direct-invocation case.)
//    * Any instance from a different path or at a different version -> it is a
//      stale or superseded build. Ask it to quit, then run. (This is the
//      update/reinstall case, and the new-DogFood-path case.)
//
//  Both clauses can apply at once — an equivalent instance plus a stale one — and
//  then both apply: the stale one is asked to quit and this process yields to the
//  equivalent one. Either way the session converges to exactly one.
//
//  The deliberate trade-off: launching an OLDER build while a newer one runs
//  leaves the older one — the launch wins, the version does not. Refusing a
//  launch the user explicitly performed would be the more surprising behaviour,
//  and "whatever I opened last is what I am looking at" is the rule that needs no
//  explanation. Downgrades are visible in the popover's version line.
//
//  `decide` is separated from every AppKit call so the rule can be tested
//  against constructed instances rather than against whatever happens to be
//  running on the machine. `enforce` is the thin live adapter.
//

import AppKit
import Foundation

/// One running copy of this app, reduced to the four facts the rule needs.
///
/// `launchDate` is optional because it genuinely is: an instance started by
/// running the executable directly, rather than through LaunchServices, is
/// reported by `NSRunningApplication` with a `nil` launch date. Measured — the
/// rule may not depend on it being present.
struct GlomerisInstance: Equatable {
    let processIdentifier: pid_t
    /// The `.app` bundle's path, standardised. Two instances launched from the
    /// same bundle through different routes must compare equal here.
    let bundlePath: String
    /// `CFBundleShortVersionString`, or nil when it could not be read.
    let version: String?
    let launchDate: Date?
}

/// What this process should do about the instances it found.
struct SingleInstanceDecision: Equatable {
    /// Instances to ask to quit, oldest first. Always a subset of the instances
    /// handed to `decide`, and never this process.
    let terminate: [pid_t]
    /// False when an equivalent instance already owns the menu bar and this
    /// process should exit instead of adding a second item.
    let keepRunning: Bool
    /// The equivalent instance being deferred to, when there is one.
    let yieldedTo: pid_t?

    static let runAlone = SingleInstanceDecision(terminate: [], keepRunning: true, yieldedTo: nil)
}

enum SingleInstanceGuard {
    // MARK: - The rule

    /// Pure. No process is enumerated, signalled or terminated here.
    ///
    /// - Parameters:
    ///   - own: this process.
    ///   - others: every other running instance sharing this bundle identifier.
    ///     `own` is filtered out by process identifier if it appears.
    static func decide(own: GlomerisInstance, others: [GlomerisInstance]) -> SingleInstanceDecision {
        let peers = others.filter { $0.processIdentifier != own.processIdentifier }
        guard !peers.isEmpty else { return .runAlone }

        let equivalent = peers.filter { isEquivalent($0, to: own) }
        let superseded = peers.filter { !isEquivalent($0, to: own) }

        // Oldest first, so the log reads in the order the duplicates appeared and
        // the decision is stable whatever order the system enumerated them in.
        let toTerminate = superseded.sorted(by: olderFirst).map(\.processIdentifier)

        guard let incumbent = equivalent.sorted(by: olderFirst).first else {
            return SingleInstanceDecision(terminate: toTerminate, keepRunning: true, yieldedTo: nil)
        }
        return SingleInstanceDecision(
            terminate: toTerminate,
            keepRunning: false,
            yieldedTo: incumbent.processIdentifier
        )
    }

    /// Same bundle on disk, same version. Anything less is a different build and
    /// this process supersedes it.
    ///
    /// A version that could not be read is never equivalent: an instance we
    /// cannot identify is exactly the one that should not be allowed to keep a
    /// menu-bar item on the strength of a guess.
    private static func isEquivalent(_ peer: GlomerisInstance, to own: GlomerisInstance) -> Bool {
        guard let peerVersion = peer.version, let ownVersion = own.version else { return false }
        return peer.bundlePath == own.bundlePath && peerVersion == ownVersion
    }

    /// A missing launch date sorts oldest — it belongs to a directly-invoked
    /// instance, which in this project is always a development build that
    /// predates whatever is being launched now.
    private static func olderFirst(_ lhs: GlomerisInstance, _ rhs: GlomerisInstance) -> Bool {
        let left = lhs.launchDate ?? .distantPast
        let right = rhs.launchDate ?? .distantPast
        if left != right { return left < right }
        return lhs.processIdentifier < rhs.processIdentifier
    }

    // MARK: - The live adapter

    /// Reads the current instance list, applies the rule, asks the superseded
    /// instances to quit, and returns whether this process should carry on.
    ///
    /// Call before any scene is constructed: a process that is going to yield
    /// must never create a status item, or the duplicate icon appears anyway for
    /// as long as it takes to decide.
    ///
    /// - Parameter gracePeriod: how long to wait for a superseded instance to act
    ///   on the quit request before escalating. The escalation is scoped to
    ///   processes this method itself selected — instances of this same app, by
    ///   bundle identifier, never this process.
    @discardableResult
    static func enforce(gracePeriod: TimeInterval = 5.0) -> Bool {
        guard let identifier = Bundle.main.bundleIdentifier else {
            // Nothing to compare against. A build with no bundle identifier is
            // not something to silently exit over.
            return true
        }
        let own = describe(NSRunningApplication.current) ?? GlomerisInstance(
            processIdentifier: ProcessInfo.processInfo.processIdentifier,
            bundlePath: Bundle.main.bundleURL.standardizedFileURL.path,
            version: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String,
            launchDate: nil
        )
        let running = NSRunningApplication.runningApplications(withBundleIdentifier: identifier)
        let others = running.compactMap(describe)

        let decision = decide(own: own, others: others)
        terminate(decision.terminate, among: running, gracePeriod: gracePeriod)

        if let incumbent = decision.yieldedTo {
            NSLog(
                "[glomeris] another instance of this exact build (pid %d) already owns the menu "
                    + "bar; exiting instead of adding a second item",
                incumbent
            )
        }
        return decision.keepRunning
    }

    /// Asks each selected instance to quit, then verifies. Only processes present
    /// in `among` — the bundle-identifier-matched list this call enumerated — can
    /// be signalled, and `NSRunningApplication.current` is never among them
    /// because `decide` filters it out by process identifier.
    private static func terminate(
        _ pids: [pid_t],
        among running: [NSRunningApplication],
        gracePeriod: TimeInterval
    ) {
        let selected = running.filter { pids.contains($0.processIdentifier) }
        guard !selected.isEmpty else { return }

        for app in selected {
            NSLog(
                "[glomeris] superseding an older instance (pid %d) at %@",
                app.processIdentifier,
                app.bundleURL?.path ?? "an unknown bundle"
            )
            app.terminate()
        }

        // Liveness is checked with `kill(pid, 0)` rather than with
        // `NSRunningApplication.isTerminated`, deliberately. This runs before the
        // run loop starts, and `isTerminated` is updated from a notification that
        // needs a running loop to be delivered — polling it here would report
        // every instance as alive for the whole grace period and escalate to a
        // forced quit every single time.
        let deadline = Date().addingTimeInterval(gracePeriod)
        var stubborn = selected.filter { isAlive($0.processIdentifier) }
        while !stubborn.isEmpty, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.1)
            stubborn = stubborn.filter { isAlive($0.processIdentifier) }
        }
        for app in stubborn {
            // A presenter with nothing unsaved that ignored a quit request for
            // the whole grace period. Leaving it alive would leave the duplicate
            // icon this exists to remove.
            NSLog(
                "[glomeris] pid %d did not act on the quit request within %.0fs; forcing it",
                app.processIdentifier,
                gracePeriod
            )
            app.forceTerminate()
        }
    }

    /// `kill(pid, 0)` signals nothing; it only asks whether the process exists
    /// and is signallable by this user. `EPERM` means it exists but belongs to
    /// someone else, which for a process we just enumerated as our own app means
    /// it is still alive.
    private static func isAlive(_ pid: pid_t) -> Bool {
        if kill(pid, 0) == 0 { return true }
        return errno == EPERM
    }

    private static func describe(_ app: NSRunningApplication) -> GlomerisInstance? {
        guard let bundleURL = app.bundleURL else { return nil }
        let path = bundleURL.standardizedFileURL.path
        let version = Bundle(url: bundleURL)?
            .infoDictionary?["CFBundleShortVersionString"] as? String
        return GlomerisInstance(
            processIdentifier: app.processIdentifier,
            bundlePath: path,
            version: version,
            launchDate: app.launchDate
        )
    }
}
