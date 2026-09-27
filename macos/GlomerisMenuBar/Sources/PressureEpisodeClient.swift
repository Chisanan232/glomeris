//
//  PressureEpisodeClient.swift
//  GlomerisMenuBar
//
//  HORO-1508: the app's half of the daemon/app seam, as values.
//
//  `UNUserNotificationCenter` is the only macOS API that can put buttons on a
//  notification, and it refuses to run outside an app bundle. The process that
//  notices disk pressure is a bare launchd job. So the process that knows a
//  banner is owed is not the process that can raise one, and `glomeris pressure`
//  is the seam: the daemon records that a notification is *due*, this app asks
//  what is due, raises it, and reports back what the user pressed.
//
//  ---------------------------------------------------------------------
//  This file decides nothing
//  ---------------------------------------------------------------------
//  See the standing project rule in GlomerisMenuBarApp.swift. In particular this
//  app never compares a percentage against a threshold to decide whether to
//  speak: it reads `notification_due`. Recomputing that here would put the
//  hysteresis, the snooze deadline and the episode identity in two places, and
//  the copy that ran more often would win. The three answers it may send are the
//  three tokens `pressure show` publishes in `responses` — there is deliberately
//  no way to express a fourth, and none of them is spelled as a literal at the
//  call site that sends it.
//
//  Nothing here deletes anything. An episode decides when the user is spoken to,
//  never what may be removed.
//

import Foundation

// MARK: - Commands

/// The argument vectors the pressure surface runs, as data.
///
/// Pure values rather than literals inline in a view or a service, so three
/// properties are assertable rather than eyeballed: nothing here names a
/// filesystem path or a credential, none of them carries `--project-root` (see
/// `GlomerisCliProjectRootScopeTests` — an episode is about the whole volume and
/// is opened by a daemon that knows nothing of this app's configured projects),
/// and the answer token is never spelled here.
enum PressureEpisodeCommands {
    /// Read-only. Does not observe, does not open an episode, writes nothing —
    /// `pressure_show` in `src/main.rs` exists in that shape on purpose, so a Mac
    /// whose daemon was never installed cannot be notified by whatever process
    /// happened to run the GUI.
    static let show = ["pressure", "show", "--json"]

    /// "The banner is on screen." Separate from an answer because only one of
    /// them ever happens: a user who ignores a banner never answers it, and an
    /// episode left looking un-notified is re-raised at the next poll — the storm
    /// AC 5 forbids.
    static let notified = ["pressure", "notified", "--json"]

    /// The user's answer, as one of the tokens `show` published.
    ///
    /// Takes the token rather than a Swift enum precisely so this app cannot
    /// invent one: the value that reaches here came from the report's `responses`
    /// array or from a notification action identifier that was built from it.
    static func respond(_ responseToken: String) -> [String] {
        ["pressure", "respond", responseToken, "--json"]
    }
}

// MARK: - Interpreting one invocation

/// What one `glomeris pressure …` invocation turned out to be.
///
/// Five cases, matching the CLI's own exit contract (`src/main.rs`), and the
/// third one is why this type exists rather than `GlomerisClient.run`:
///
///   - **0** — a status report on stdout. All three verbs print the same shape,
///     so there is one success case rather than three;
///   - **3** — `EXIT_PRESSURE_NOTHING_TO_RECORD`, with a rejection report on
///     stdout. *Not a failure.* The ordinary cause is benign and expected: the
///     disk recovered between the banner appearing and the button being pressed,
///     so there is no longer an episode to answer. A caller that reported this as
///     a malfunction would show the user a fault for a race that resolved itself
///     correctly;
///   - **2** with prose — this app sent arguments the installed CLI does not
///     have. A version skew, and this app's problem, not the user's;
///   - **1** — the settings or the episode file could not be read or written;
///   - exit 0 with undecodable stdout is the other half of a version skew.
enum PressureEpisodeOutcome: Equatable {
    case status(PressureStatusReportDto)
    case nothingToRecord(PressureRejectionReportDto)
    case usageError(String)
    case malformedOutput
    case failed(String)

    /// The report, when there is one. `nil` for every case that did not produce
    /// one, so a caller that only wants to refresh its view of the disk can do it
    /// in one line without a `switch` that would have to enumerate outcomes it
    /// does not act on.
    var report: PressureStatusReportDto? {
        guard case .status(let report) = self else { return nil }
        return report
    }

    /// `true` when the episode record is unchanged *and* this app should stop
    /// trying for now, rather than treat the outcome as a transient error worth
    /// retrying at speed.
    ///
    /// `.nothingToRecord` is the case this is here for: it is the one non-zero
    /// exit that means the CLI understood perfectly and there was simply nothing
    /// to record. Retrying it would produce the same refusal forever.
    var isSettled: Bool {
        switch self {
        case .status, .nothingToRecord:
            return true
        case .usageError, .malformedOutput, .failed:
            return false
        }
    }
}

/// Pure, directly-testable mapping from a `pressure` verb's exit code and streams
/// to a `PressureEpisodeOutcome`.
enum PressureEpisodeInterpretation {
    /// The prefix the CLI puts on its own stderr lines, stripped so the sentence
    /// reads as a sentence rather than as a log line. Matched as a literal; a
    /// mismatch degrades to showing the line verbatim.
    static let stderrPrefix = "glomeris pressure: "

    /// Tells "this app sent something that CLI does not have" apart from "that
    /// request was refused". Only one of them is anybody's to act on, and it is
    /// not the user's.
    ///
    /// Two markers because `pressure` has two usage shapes an older CLI could
    /// produce: it does not know the subcommand at all, or it does not know the
    /// answer token. Either way the remedy is the same.
    static let usageErrorMarkers = ["unknown subcommand", "unknown answer"]

    /// Exit 3. Named rather than inline so the one number this file depends on
    /// from `src/main.rs` is stated once, next to what it means.
    static let nothingToRecordExitCode: Int32 = 3

    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> PressureEpisodeOutcome {
        // Decoded before the exit code is judged, for the reason
        // `RecoverySettingsInterpretation` does it: whatever was printed is a
        // better account of what happened than the exit code alone. The two
        // shapes are decode-disjoint — a status report requires
        // `notify_at_description`, `current` and `default_goal`; a rejection
        // requires `reason` and `message` — so either match is unambiguous.
        if let report = try? JSONDecoder().decode(PressureStatusReportDto.self, from: stdout) {
            return .status(report)
        }
        if let rejection = try? JSONDecoder().decode(
            PressureRejectionReportDto.self, from: stdout)
        {
            return .nothingToRecord(rejection)
        }

        let stderrText = String(decoding: stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)

        if exitCode == Self.nothingToRecordExitCode {
            // Exit 3 with no decodable report: the non-`--json` path prints the
            // same sentence to stderr, and an older CLI might exit 3 for a reason
            // this app has not learned. Either way nothing was recorded, so the
            // honest outcome is still `.nothingToRecord` — synthesised with the
            // reason token that says exactly that much and no more.
            return .nothingToRecord(
                PressureRejectionReportDto(
                    reason: PressureRejectionReportDto.unknownReason,
                    message: stderrText.isEmpty
                        ? "glomeris had nothing to record but did not say why."
                        : Self.withoutPrefix(stderrText)
                )
            )
        }

        if exitCode == 2 {
            if Self.usageErrorMarkers.contains(where: stderrText.contains) {
                return .usageError(
                    stderrText.isEmpty
                        ? "The installed glomeris rejected the arguments this app sent."
                        : Self.withoutPrefix(stderrText)
                )
            }
            return .failed(
                stderrText.isEmpty
                    ? "glomeris refused this request but said nothing about why."
                    : Self.withoutPrefix(stderrText)
            )
        }

        if exitCode == 0 {
            return .malformedOutput
        }

        return .failed(
            stderrText.isEmpty
                ? "glomeris pressure exited with code \(exitCode) and printed no result."
                : Self.withoutPrefix(stderrText)
        )
    }

    /// The first line of a multi-line stderr, un-prefixed.
    ///
    /// Only the first: a usage error prints the message and then the whole usage
    /// block for the command, and a UI that showed all of it would render a help
    /// page inside a sentence.
    private static func withoutPrefix(_ text: String) -> String {
        let firstLine = text.split(separator: "\n", maxSplits: 1).first.map(String.init) ?? text
        guard firstLine.hasPrefix(Self.stderrPrefix) else { return firstLine }
        return String(firstLine.dropFirst(Self.stderrPrefix.count))
    }
}

// MARK: - The three verbs

/// Runs the pressure verbs and hands back what each one turned out to be.
///
/// A type rather than three free functions so the notifier can be tested against
/// a double: everything AC 1, 3, 4, 5 and 7 are about is a sequence of these
/// calls and their answers, and none of that should need a real disk, a real
/// daemon or a real banner to assert.
protocol PressureEpisodeReading: Sendable {
    /// What is owed, if anything. Changes nothing.
    func show() async -> PressureEpisodeOutcome

    /// Records that the banner reached the screen.
    func markNotified() async -> PressureEpisodeOutcome

    /// Records the user's answer, as one of the published tokens.
    func respond(_ responseToken: String) async -> PressureEpisodeOutcome
}

/// `PressureEpisodeReading` over the real CLI.
///
/// Every verb returns an outcome and none of them throws. A spawn failure — no
/// binary installed, or one that cannot launch — is mapped to `.failed` with the
/// same wording every other section uses, because from the caller's point of view
/// it is the same situation as a CLI that ran and could not answer: there is no
/// news about the disk, and there is nothing to notify about. The alternative,
/// throwing, would make a polling loop's happy path carry a `try` for a case it
/// can only respond to by waiting for the next poll.
struct PressureEpisodeClient: PressureEpisodeReading {
    private let client: GlomerisClient

    init(client: GlomerisClient = GlomerisClient()) {
        self.client = client
    }

    func show() async -> PressureEpisodeOutcome {
        await run(PressureEpisodeCommands.show, subject: "pressure show")
    }

    func markNotified() async -> PressureEpisodeOutcome {
        await run(PressureEpisodeCommands.notified, subject: "pressure notified")
    }

    func respond(_ responseToken: String) async -> PressureEpisodeOutcome {
        await run(
            PressureEpisodeCommands.respond(responseToken),
            subject: "pressure respond"
        )
    }

    /// `runRaw`, not `run`: exit 3 carries a meaningful body on stdout, and
    /// `GlomerisClient.run` throws away stdout for every non-zero exit code. The
    /// body is the whole point of that exit code.
    private func run(_ arguments: [String], subject: String) async -> PressureEpisodeOutcome {
        do {
            let raw = try await client.runRaw(arguments, progressType: ProgressEventDto.self)
            return PressureEpisodeInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
        } catch {
            return .failed(
                SectionFetchErrors.shortMessage(error, subject: subject)
                    ?? "The \(subject) check was stopped before it answered."
            )
        }
    }
}
