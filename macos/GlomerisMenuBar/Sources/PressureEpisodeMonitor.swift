//
//  PressureEpisodeMonitor.swift
//  GlomerisMenuBar
//
//  HORO-1508: the thing that notices a banner is owed while nothing is on screen.
//
//  ---------------------------------------------------------------------
//  Why this exists at all
//  ---------------------------------------------------------------------
//  Every other CLI-reading surface in this app polls only while it is visible:
//  `StatusHealthSectionView` fetches on appear and every 10 seconds while the
//  popover is open, and stops when it closes. That is right for a card nobody is
//  looking at — and useless for a notification, whose entire purpose is to arrive
//  when nobody is looking. So this is the app's one background reader.
//
//  ---------------------------------------------------------------------
//  It decides nothing about pressure
//  ---------------------------------------------------------------------
//  See the standing project rule in GlomerisMenuBarApp.swift. This type never
//  compares a percentage against a threshold. It reads `notification_due`, which
//  the daemon set, and the hysteresis, the snooze deadline, the episode identity
//  and the answer set all stay in `src/monitor/episode.rs`. A copy of the
//  threshold rule here would be a second decider, and the one that ran more often
//  would win.
//
//  The one judgement that *is* this app's: whether a banner it already put on
//  screen is the same banner. That is not a policy — it is non-reentrancy, and it
//  cannot be delegated, because the record that would answer it is only updated
//  *after* the banner has been raised. See `PressureBannerKey`.
//
//  Nothing here deletes anything. A banner asks a question.
//

import Foundation

// MARK: - Which banner is which

/// Identity of one banner-worth of pressure, as two figures the CLI reports.
///
/// The episode id alone is not enough, and the reason is AC 3. "Remind me later"
/// snoozes an episode; when the snooze elapses the daemon makes the *same
/// episode* owe a notification again. Keyed on the id alone, the second banner
/// would be suppressed as a duplicate and "remind me later" would mean "never
/// again" — the deterministic behaviour AC 3 asks for, silently inverted.
///
/// `notificationsRaised` is what separates them: Rust increments it when the app
/// reports the banner reached the screen, so the post-snooze banner is the second
/// for that episode and reads as a different key. Both figures come from the
/// report; neither is computed here.
struct PressureBannerKey: Equatable, Hashable {
    let episodeId: UInt64
    /// Banners already recorded for this episode, from
    /// `PressureEpisodeReportDto.notificationsRaised`.
    let notificationsRaised: UInt32

    init(episodeId: UInt64, notificationsRaised: UInt32) {
        self.episodeId = episodeId
        self.notificationsRaised = notificationsRaised
    }

    init(_ episode: PressureEpisodeReportDto) {
        self.init(
            episodeId: episode.episodeId,
            notificationsRaised: episode.notificationsRaised
        )
    }
}

// MARK: - What to do with one report

/// What one `pressure show` answer means for the screen.
///
/// A pure function of the report and what this session has already raised, so
/// every acceptance scenario AC 7 lists — crossing, recovery below the hysteresis
/// margin, re-crossing, snooze, ignore — is an assertion about values rather than
/// something that needs a disk to fill up and a banner to appear.
enum PressureBannerDecision: Equatable {
    /// Put this banner on screen, then report it with `pressure notified`.
    case raise(PressureBannerKey)

    /// Nothing is owed. Either the disk is below the threshold, or the episode's
    /// banner has already been shown, or it is snoozed — this type does not need
    /// to know which, because Rust has already reduced all three to
    /// `notification_due == false`.
    case nothingOwed

    /// A banner with this exact identity is already on screen from this session,
    /// and the record has not caught up yet. Distinct from `.nothingOwed` so a
    /// storm and a quiet disk cannot be confused in a log or a test.
    case alreadyRaised(PressureBannerKey)
}

/// The rule, as one function.
enum PressureBannerPlan {
    /// Reads `notification_due` — never a percentage.
    ///
    /// `lastRaised` is this session's memory of the last banner that reached the
    /// screen, and is the guard against AC 5's storm. It is needed *as well as*
    /// the CLI's own `notification_due` because the two are not simultaneous: the
    /// banner goes up, and only then does `pressure notified` clear the flag. A
    /// poll landing in that window reads a report that still says a banner is owed
    /// — and would raise a second one.
    ///
    /// It is also the reason a failed `pressure notified` does not produce a
    /// storm. If the record could not be written the flag stays set for every
    /// subsequent poll, and this is what stops each of them becoming a banner. The
    /// cost is that a banner shown but not recorded is not shown again, which is
    /// the right way round: a question asked once and lost is better than the same
    /// question every thirty seconds.
    static func decide(
        report: PressureStatusReportDto,
        lastRaised: PressureBannerKey?
    ) -> PressureBannerDecision {
        // Both conditions come from Rust. `notification_due` at the top level is
        // the daemon's answer; the episode is what identifies it. A report that
        // says a banner is owed but carries no episode cannot be acted on — there
        // would be nothing to record the answer against — and that combination is
        // one `build_pressure_status_report` does not produce.
        guard report.notificationDue, let episode = report.episode else {
            return .nothingOwed
        }
        let key = PressureBannerKey(episode)
        if key == lastRaised {
            return .alreadyRaised(key)
        }
        return .raise(key)
    }
}

// MARK: - Raising a banner

/// One banner's worth of content, composed from the report.
///
/// Every figure is the CLI's, and the two percentages are carried separately
/// rather than as one "level", because the campaign's whole point about these two
/// numbers is that a user who reads them as one has been told the wrong thing: the
/// threshold is when to speak, the goal is where recovery stops.
struct PressureBanner: Equatable {
    let key: PressureBannerKey

    /// Where the disk is now, percent used.
    let currentUsedPercent: Double

    /// What the user asked to be told at, percent used, in the CLI's own wording.
    let notifyAtDescription: String

    /// Free space now, in the CLI's own rendering.
    let freeHuman: String

    /// Where a recovery run would stop, in the CLI's own wording.
    let goalDescription: String

    /// The answers this banner may offer, as the tokens the CLI published. Never a
    /// set this app chose: a button built from anything else would be refused
    /// after the user pressed it.
    let responseTokens: [String]

    init(key: PressureBannerKey, report: PressureStatusReportDto) {
        self.key = key
        currentUsedPercent = report.current.usedPercent
        notifyAtDescription = report.notifyAtDescription
        freeHuman = report.current.freeHuman
        goalDescription = report.defaultGoal.description
        responseTokens = report.responses
    }
}

/// Puts a banner on screen and reports back which button was pressed.
///
/// A protocol so the monitor can be driven in tests without a notification
/// centre, an app bundle, or a user to press anything — none of which a test may
/// arrange. The real implementation is `PressureNotificationCenterBanner`.
///
/// `@MainActor` because every implementation of it touches AppKit or
/// `UNUserNotificationCenter`, and because the monitor that calls it is.
@MainActor
protocol PressureBannerRaising: AnyObject {
    /// Called before the first banner, with the tokens the CLI published, so the
    /// buttons are built from the CLI's answer set rather than from a list in
    /// Swift.
    func prepare(responseTokens: [String])

    /// Puts `banner` on screen. Returns `true` when it reached the screen — which
    /// is what `pressure notified` means, and is why it is not assumed.
    func raise(_ banner: PressureBanner) async -> Bool

    /// Installed once. Called with one of the published tokens when the user
    /// presses a button.
    var onAnswer: ((String) -> Void)? { get set }
}

// MARK: - The loop

/// Polls `pressure show` in the background, raises what is owed, and records the
/// answer.
///
/// `ObservableObject` so the popover's recovery surface can show the same pending
/// episode the banner is about (AC 6: the actions have to be reachable without
/// the notification, for anyone who dismissed it or has banners turned off). Held
/// by `GlomerisMenuBarApp` as a plain `let`, never as an `@StateObject` — see that
/// file's header for what a scene-level observed store did to the menu-bar item.
/// Reading a `let` in `App.body` creates no subscription; the popover observes it
/// and only the popover is invalidated.
///
/// `@MainActor`, unlike `ScanState` and `PlanState` next door. Those are written
/// only from a view's own `.task`, which is already main-isolated; this one is
/// written from a loop nobody is watching, and its `lastRaised` guard is the only
/// thing standing between an overlapping poll and a second banner. A guard that
/// can be read and written from two threads is not a guard.
@MainActor
final class PressureEpisodeMonitor: ObservableObject {
    /// The episode a banner is owed for, or was last raised for, as the CLI last
    /// reported it. `nil` when the disk is below the threshold.
    ///
    /// Published so the in-app path can offer the same three answers as the
    /// banner. It is the report's own episode, not a copy this app maintains.
    @Published private(set) var episode: PressureEpisodeReportDto?

    /// The whole last answer, kept so the recovery surface can state the context a
    /// deep link arrived with — current usage, threshold and goal — without
    /// running its own `pressure show`.
    @Published private(set) var lastReport: PressureStatusReportDto?

    /// What went wrong with the last poll, if anything. Shown rather than
    /// swallowed: a background reader that silently stops is indistinguishable
    /// from a disk that is fine.
    @Published private(set) var lastErrorMessage: String?

    /// How often to ask.
    ///
    /// Thirty seconds, not the ten the popover's status card uses. A threshold
    /// crossing is not urgent to the second — the daemon has already been watching
    /// continuously and the episode is durable — and this one runs whether or not
    /// anybody is looking, so the cost is paid all day. Each poll is one short
    /// `pressure show`, which reads two small files and one `statfs`.
    static let pollInterval: Duration = .seconds(30)

    private let client: PressureEpisodeReading
    private let banners: PressureBannerRaising

    /// This session's memory of the last banner that reached the screen. See
    /// `PressureBannerPlan.decide`.
    private var lastRaised: PressureBannerKey?

    /// The running loop, so it can be stopped. Nothing in the app stops it today;
    /// the handle exists so a test does not leave one running past its own
    /// lifetime.
    private var pollTask: Task<Void, Never>?

    /// Guards against a poll landing while a banner is mid-flight.
    ///
    /// `lastRaised` is only set once `raise` returns, and raising is `async`. Two
    /// overlapping polls would both see no key and both raise. This is not policy
    /// either — it is the same non-reentrancy, one step earlier.
    private var isRaising = false

    init(client: PressureEpisodeReading, banners: PressureBannerRaising) {
        self.client = client
        self.banners = banners
        banners.onAnswer = { [weak self] token in
            // A button press arrives from the notification centre's own callback,
            // so it becomes an answer through the same path a poll does.
            Task { @MainActor [weak self] in await self?.answer(token) }
        }
    }

    /// Starts polling. Idempotent: a second call is ignored rather than starting a
    /// second loop, because two loops would produce two banners for one episode
    /// and `lastRaised` would not save them from each other.
    func start() {
        guard pollTask == nil else { return }
        pollTask = Task { @MainActor [weak self] in
            // Polls immediately, then waits. A disk that was already over the
            // threshold when the app launched is the ordinary case after a restart
            // (the episode is durable), and waiting out the first interval would
            // leave an owed banner unshown for no reason.
            while !Task.isCancelled {
                await self?.pollOnce()
                do {
                    try await Task.sleep(for: Self.pollInterval)
                } catch {
                    return  // cancelled
                }
            }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
    }

    /// One poll: ask, decide, raise, record.
    ///
    /// `internal` so tests drive it directly rather than waiting out an interval —
    /// an acceptance scenario about a storm has to be able to poll five times in a
    /// row, and doing that through the timer would take two and a half minutes.
    func pollOnce() async {
        let outcome = await client.show()
        adopt(outcome)

        guard let report = outcome.report else { return }

        let decision = PressureBannerPlan.decide(report: report, lastRaised: lastRaised)
        guard case .raise(let key) = decision, !isRaising else { return }

        isRaising = true
        defer { isRaising = false }

        banners.prepare(responseTokens: report.responses)
        let reachedTheScreen = await banners.raise(PressureBanner(key: key, report: report))

        // Set only on the path where a banner actually appeared. A raise that
        // failed must not consume the episode's one chance to be shown: the flag
        // is still set in the record, so the next poll tries again.
        guard reachedTheScreen else { return }
        lastRaised = key

        // What `notifications_raised` counts: banners the user could have seen,
        // not banners this app intended. That is the figure AC 5 is checked
        // against, and it is why this is reported rather than assumed.
        adopt(await client.markNotified())
    }

    /// Sends the user's answer. One of the tokens the CLI published — this app
    /// cannot compose another.
    ///
    /// `internal` for the same reason as `pollOnce`: pressing a button is what AC
    /// 3, 4 and 7 are about, and a test must be able to do it.
    func answer(_ responseToken: String) async {
        adopt(await client.respond(responseToken))

        // Immediately, rather than at the next interval. An answer changes the
        // record — a snooze deadline, or an ignored episode — and the in-app
        // surface showing the old state for up to thirty seconds after the user
        // answered would read as the answer not having been taken.
        adopt(await client.show())
    }

    /// Takes on whatever an invocation turned out to be.
    ///
    /// `.nothingToRecord` clears the error rather than setting one, and that is
    /// deliberate: it is the CLI saying the disk recovered before the button was
    /// pressed, which is good news. Presenting it as a fault would have the user
    /// chasing a condition that had already resolved.
    private func adopt(_ outcome: PressureEpisodeOutcome) {
        switch outcome {
        case .status(let report):
            lastReport = report
            episode = report.episode
            lastErrorMessage = nil

        case .nothingToRecord:
            lastErrorMessage = nil

        case .usageError(let detail):
            lastErrorMessage =
                "This app and the installed glomeris disagree about the pressure commands, so "
                + "disk-pressure alerts are not running. It said: " + detail

        case .malformedOutput:
            lastErrorMessage =
                "glomeris printed something this app could not read, so disk-pressure alerts are "
                + "not running. The app and the CLI are probably different versions."

        case .failed(let detail):
            lastErrorMessage = detail
        }
    }
}
