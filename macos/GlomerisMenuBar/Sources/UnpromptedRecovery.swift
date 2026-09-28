//
//  UnpromptedRecovery.swift
//  GlomerisMenuBar
//
//  HORO-1510: the optional half of §10 — a bounded recovery run that begins in
//  answer to a disk-pressure alert, with nobody present.
//
//  ---------------------------------------------------------------------
//  What this is not
//  ---------------------------------------------------------------------
//  It is not an "auto clean". Every bound on what such a run may do was written
//  by the user into the Autopilot envelope and is enforced in Rust: allowed
//  kinds, action count, byte total, wall clock, a disk-pressure floor, and which
//  `ASK` reasons were answered in advance. This file adds no bound of its own and
//  relaxes none, because it has no way to: it decides *whether to start one*, and
//  the thing it starts is `free --autopilot`, which is the same bounded loop the
//  Recover button runs.
//
//  The default is unchanged and stays unchanged: the threshold puts a
//  notification on screen and waits. This path exists only for a grant that says,
//  in as many words, that a run may begin without being asked
//  (`autopilot enable --respond-to-alerts`).
//
//  ---------------------------------------------------------------------
//  It reads authority; it never composes it
//  ---------------------------------------------------------------------
//  See the standing project rule at the top of GlomerisMenuBarApp.swift. The
//  question "may a run start unasked" is answered by `startsUnprompted` on the
//  grant, which Rust computed. This file quotes it.
//
//  And quoting it is not what makes the run legal: `free` itself refuses an
//  `--unattended` run the grant does not cover, exit 3, having attempted nothing.
//  Reading the grant here is so the app can *tell the user* which mode is in
//  force and not bother the CLI when it already knows the answer. If the two ever
//  disagree — a grant revoked in the half-second between the read and the spawn —
//  Rust wins, and the outcome below says so.
//

import Foundation

// MARK: - What one attempt turned out to be

/// The outcome of one `free --autopilot --unattended`.
///
/// Four cases, and the distinction that earns its keep is `busy` against the
/// other three. See ``consumesTheAttempt``.
enum UnpromptedRecoveryOutcome: Equatable {
    /// The loop ran and reported. Includes a run that reclaimed nothing: that is
    /// an outcome, not a failure, and its `stopReason` says which one.
    case ran(RecoveryRunReportDto)

    /// `free` refused: exit 3, the grant does not authorize starting unasked.
    ///
    /// Reachable even though the grant was read first, because the read and the
    /// spawn are not one operation — `revoke` between them lands here. Kept as a
    /// case of its own rather than folded into `failed`, because it is the one
    /// failure that is the safety model working.
    case notAuthorized(String)

    /// The execution lock was held: exit 75, before the loop started.
    ///
    /// The only outcome that provably mutated nothing, which is why it is the
    /// only one worth retrying. Same reasoning as
    /// `RecoverySectionView.unreadableOutcomePhase`.
    case busy(String)

    /// Anything else: a spawn failure, an unreadable report, a run that stopped
    /// on an error.
    case failed(String)

    /// Whether this episode's one automatic attempt has been used up.
    ///
    /// `false` for `busy` alone. Everything else is spent, including `failed` —
    /// deliberately, and it is the conservative direction: a run that got far
    /// enough to fail may already have deleted something, and an episode that
    /// retried on every poll would be a loop nobody authorized bounded only by
    /// how long the disk stayed full.
    /// Enumerated rather than written as `!isBusy`, so that a fifth case added
    /// later does not silently inherit an answer nobody chose for it.
    var consumesTheAttempt: Bool {
        switch self {
        case .busy:
            return false
        case .ran, .notAuthorized, .failed:
            return true
        }
    }
}

// MARK: - Whether to start one

/// What a pressure report that owes a banner should produce.
enum UnpromptedRecoveryDecision: Equatable {
    /// Start a bounded run for this episode, then look again.
    case run(PressureBannerKey)

    /// Put the banner up and wait for a person — the default, and what every
    /// grant that has not opted in produces.
    case askInstead

    /// This episode's one automatic attempt has already been made. The banner
    /// path takes over, so an episode a bounded run could not resolve still
    /// reaches the user.
    case alreadyAttempted(PressureBannerKey)
}

/// The rule, as one pure function.
enum UnpromptedRecoveryPlan {
    /// Decides on the grant's own conjunction and this session's memory.
    ///
    /// `startsUnprompted` is passed rather than the envelope so the caller cannot
    /// pass two fields and have this compose them — there is nothing here to
    /// compose. A `nil` means the grant could not be read at all, and the answer
    /// to that is `askInstead`: an unreadable grant is not consent.
    ///
    /// **One attempt per episode**, keyed on the whole
    /// ``PressureBannerKey`` — episode id *and* banners already raised. So the
    /// automatic run happens once per notification-worth of pressure rather than
    /// once per episode: a user who pressed "Remind me later" and let the snooze
    /// elapse has a second crossing-worth of attention owed, and gets a second
    /// bounded attempt. Keyed on the episode id alone, a long episode would get
    /// one attempt ever however many times it came back.
    static func decide(
        owed key: PressureBannerKey,
        startsUnprompted: Bool?,
        lastAttempted: PressureBannerKey?
    ) -> UnpromptedRecoveryDecision {
        guard startsUnprompted == true else { return .askInstead }
        if key == lastAttempted { return .alreadyAttempted(key) }
        return .run(key)
    }
}

// MARK: - Doing it

/// Reads the grant and runs one bounded unprompted recovery.
///
/// A protocol so the monitor can be driven in tests with no CLI, no disk and
/// nothing deleted — which is not a convenience here but the only way these paths
/// may be tested at all. A test that let a real `free --autopilot` run would be
/// deleting from the machine it runs on.
///
/// `@MainActor` because the monitor that calls it is, and because `readGrant`'s
/// result is published.
@MainActor
protocol UnpromptedRecoveryRunning: AnyObject {
    /// The grant as it stands, or `nil` if it could not be read.
    func readGrant() async -> AutopilotEnvelopeDto?

    /// One bounded run toward `goalUsedPercent`, declared as unattended.
    ///
    /// **Implementations must not be cancellable.** See
    /// `UnpromptedRecoveryClient.run`.
    func run(goalUsedPercent: Int) async -> UnpromptedRecoveryOutcome
}

/// `UnpromptedRecoveryRunning` over the real CLI.
@MainActor
final class UnpromptedRecoveryClient: UnpromptedRecoveryRunning {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    init(client: GlomerisClient = GlomerisClient(), projectRootsStore: ProjectRootsStore) {
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    func readGrant() async -> AutopilotEnvelopeDto? {
        do {
            let raw = try await client.runRaw(
                AutopilotCommands.show,
                progressType: ProgressEventDto.self
            )
            guard case .envelope(let report) = AutopilotEnvelopeInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            ) else {
                return nil
            }
            return report
        } catch {
            return nil
        }
    }

    /// The arguments, as a pure function so a test can read them without spawning
    /// anything. `--unattended` is this app's statement about itself, and saying
    /// it is what lets Rust tell a run nobody started from one somebody typed.
    ///
    /// No `--stop-file`: there is no window open to press Stop in. The run is
    /// bounded by the envelope's action, byte and time budgets instead, which is
    /// what makes an unattended run safe to start at all.
    ///
    /// `nonisolated` because it is a function of its arguments and nothing else.
    /// Inheriting this type's main-actor isolation would make the one thing here
    /// that touches no process reachable only from the main actor.
    nonisolated static func arguments(
        goalUsedPercent: Int,
        projectRootsStore: ProjectRootsStore
    ) -> [String] {
        projectRootsStore.scoped([
            "free", "--goal-used-percent", "\(goalUsedPercent)",
            "--autopilot", "--unattended",
            "--json",
        ])
    }

    /// Spawns the run.
    ///
    /// **Not from a cancellable task, ever.** `GlomerisClient.runRaw` installs a
    /// cancellation handler that sends `SIGTERM`, and this vector deletes things:
    /// a signal partway through would leave the filesystem in a state neither
    /// this app nor the audit log could describe. The caller's side of that
    /// contract is in `PressureEpisodeMonitor.attemptUnpromptedRecovery`, which
    /// runs this inside an unstructured `Task {}` so the poll loop's cancellation
    /// does not reach it — pinned by
    /// `UnpromptedRecoveryTests.testTheUnpromptedRunIsNeverReachedFromACancellableTask`.
    func run(goalUsedPercent: Int) async -> UnpromptedRecoveryOutcome {
        let arguments = Self.arguments(
            goalUsedPercent: goalUsedPercent,
            projectRootsStore: projectRootsStore
        )
        do {
            let raw = try await client.runRaw(arguments, progressType: ProgressEventDto.self)
            return Self.interpret(exitCode: raw.exitCode, stdout: raw.stdout, stderr: raw.stderr)
        } catch {
            return .failed(
                SectionFetchErrors.shortMessage(error, subject: "free --autopilot")
                    ?? "The unattended recovery run was stopped before it answered."
            )
        }
    }

    /// Maps one invocation to an outcome. `static`, `nonisolated` and pure: the
    /// exit-code contract is the thing worth testing here, and none of it needs a
    /// process or an actor.
    ///
    /// Exit 3's two meanings — Autopilot not enabled, and a grant that does not
    /// allow starting unasked — arrive as one code on purpose (see the CLI
    /// reference), and both land in `notAuthorized`, because from here they are
    /// the same fact: this run was not authorized and nothing was attempted.
    nonisolated static func interpret(
        exitCode: Int32,
        stdout: Data,
        stderr: Data
    ) -> UnpromptedRecoveryOutcome {
        if exitCode == 0 {
            guard let report = try? JSONDecoder()
                .decode(RecoveryRunReportDto.self, from: stdout) else {
                return .failed(
                    "The unattended recovery run finished and printed something this app could "
                        + "not read."
                )
            }
            return .ran(report)
        }

        let message = RecoverySectionView.unreadableOutcomeMessage(
            subject: "Unattended recovery",
            exitCode: exitCode,
            stdout: stdout,
            stderr: stderr
        )

        if exitCode == 3 {
            return .notAuthorized(message)
        }
        // The lock's refusal is on stdout with its own token; the exit code alone
        // would not distinguish it from a future 75, so both are required.
        if exitCode == 75,
           let refusal = try? JSONDecoder().decode(ExecuteRefusalReportDto.self, from: stdout),
           refusal.reason == RecoverySectionView.executionLockBusyReason {
            return .busy(message)
        }
        return .failed(message)
    }
}
