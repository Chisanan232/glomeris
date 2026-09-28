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
//  ---------------------------------------------------------------------
//  It can now start something that deletes (HORO-1510)
//  ---------------------------------------------------------------------
//  This header used to say "nothing here deletes anything", and that is no longer
//  true, so it says this instead.
//
//  A grant that has opted in — `autopilot enable --respond-to-alerts`, off by
//  default — lets this monitor start one bounded `free --autopilot --unattended`
//  in answer to a banner it would otherwise raise. It adds no bound of its own:
//  the allowlist, the action, byte and time budgets and the pressure floor are all
//  in the envelope and all enforced in Rust, and `free` refuses the run outright
//  if the grant does not cover it. What this file decides is *whether to start
//  one*, and it decides that by reading `startsUnprompted` — which Rust computed.
//
//  Two bounds are this file's own, and neither is policy. One bounded run per
//  poll, and one attempt per banner-worth of pressure: see
//  `UnpromptedRecoveryPlan` and `attemptUnpromptedRecovery`. Without an opted-in
//  grant, and in every test that does not pass a runner, nothing here deletes
//  anything and a banner asks a question.
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

    /// What an automatic run already did for this episode, or `nil` if it never
    /// had one (HORO-1510).
    ///
    /// Carried so the banner's last sentence can be true. A notification raised
    /// after an unprompted run is raised *because that run did not resolve the
    /// pressure* — exactly the case where "Nothing has been deleted" is wrong. No
    /// default value, deliberately: a surface that forgets to pass this reassures
    /// the user about a deletion that happened, and a `nil` default is how that
    /// gets forgotten.
    let automaticRun: UnpromptedRecoveryOutcome?

    init(
        key: PressureBannerKey,
        report: PressureStatusReportDto,
        automaticRun: UnpromptedRecoveryOutcome?
    ) {
        self.key = key
        currentUsedPercent = report.current.usedPercent
        notifyAtDescription = report.notifyAtDescription
        freeHuman = report.current.freeHuman
        goalDescription = report.defaultGoal.description
        responseTokens = report.responses
        self.automaticRun = automaticRun
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

    /// Where "show me" goes. Optional so the monitor can be constructed — and
    /// driven — with no window server at all; a `nil` here means answers are
    /// recorded and nothing is opened, which is the correct behaviour for a
    /// headless test rather than a degraded one.
    ///
    /// Held strongly, deliberately. A `weak` reference here would turn "the
    /// presenter was released" into "the button silently does nothing", which is
    /// the failure mode this deep link is shaped to avoid. Nothing it points at
    /// holds the monitor, so there is no cycle to break.
    private let deepLink: RecoveryDeepLinkOpening?

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

    /// HORO-1510's optional half: where a bounded unprompted run comes from, or
    /// `nil` when this monitor cannot start one at all.
    ///
    /// Optional so the notification-only behaviour — the default, and everything
    /// HORO-1508 built — is reachable with no recovery runner in the picture. A
    /// monitor constructed without one raises banners and nothing else, which is
    /// the correct shape for every test that is not about this path: the
    /// destructive capability is absent rather than switched off.
    private let unpromptedRecovery: UnpromptedRecoveryRunning?

    /// The last banner-worth of pressure an automatic run was attempted for.
    ///
    /// The same non-reentrancy `lastRaised` provides, for the other path. See
    /// `UnpromptedRecoveryPlan.decide` for why it is keyed on the whole banner key
    /// and not on the episode id.
    private var lastAutomaticAttempt: PressureBannerKey?

    /// Guards against a poll landing while a run is mid-flight. A recovery run can
    /// last the whole of its time budget, which is longer than the poll interval by
    /// default — so unlike `isRaising`, this one is load-bearing in the ordinary
    /// case rather than in a race.
    private var isRecovering = false

    /// What the grant said at the last poll that read it, so the popover can say
    /// which mode is in force without running `autopilot show` itself.
    ///
    /// `nil` until a poll has had reason to read it — which is only when a banner
    /// is owed. A settings pane the user has never opened on a disk that has never
    /// filled up has nothing to report here, and inventing `askedFirst` for that
    /// case would be stating a grant nobody read.
    @Published private(set) var unpromptedMode: AutopilotUnpromptedMode?

    /// What an automatic run did for the episode it ran against, so a surface
    /// speaking about *that* episode can account for it.
    ///
    /// Keyed on the episode rather than on the banner key, because the claim it
    /// settles — whether anything has been deleted — is a fact about the episode: a
    /// post-snooze reminder is a second banner for the same pressure, and the
    /// deletion an earlier banner's run performed is still the reason the figures
    /// moved. One slot rather than a set, because `pressure show` reports at most
    /// one open episode, so there is never a second one to remember.
    private var automaticRunByEpisode: (episodeId: UInt64, outcome: UnpromptedRecoveryOutcome)?

    /// The last automatic run's outcome, so the in-app surface can account for a
    /// run the user was not present for. Published for the same reason the banner
    /// path publishes its episode: an action taken on someone's behalf that leaves
    /// no trace they can find is worse than one they had to start.
    @Published private(set) var lastUnpromptedOutcome: UnpromptedRecoveryOutcome?

    init(
        client: PressureEpisodeReading,
        banners: PressureBannerRaising,
        deepLink: RecoveryDeepLinkOpening? = nil,
        unpromptedRecovery: UnpromptedRecoveryRunning? = nil
    ) {
        self.client = client
        self.banners = banners
        self.deepLink = deepLink
        self.unpromptedRecovery = unpromptedRecovery
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
        await pollOnce(allowingUnpromptedRecovery: true)
    }

    /// `allowingUnpromptedRecovery` is `false` on the second pass of a poll that
    /// already ran one, which is what bounds this to a single extra pass rather
    /// than a loop that could keep starting runs for as long as the disk stayed
    /// full. The envelope's budgets bound each run; this bounds how many runs one
    /// poll may start, and the answer is one.
    private func pollOnce(allowingUnpromptedRecovery: Bool) async {
        let outcome = await client.show()
        adopt(outcome)

        guard let report = outcome.report else { return }

        let decision = PressureBannerPlan.decide(report: report, lastRaised: lastRaised)
        guard case .raise(let key) = decision, !isRaising else { return }

        // HORO-1510 §10. Before the banner, and not instead of it. A grant that has
        // not opted in returns `false` here immediately and the banner goes up as it
        // always did.
        if allowingUnpromptedRecovery, await attemptUnpromptedRecovery(owed: key, report: report) {
            // Something was deleted, so every figure in `report` is now a claim
            // about a volume that no longer exists — including `notification_due`.
            // Ask again and decide on what is true now: if the run got the disk
            // under its goal the episode has closed and nothing is owed, and if it
            // could not, the banner the user was always going to get goes up in the
            // same poll rather than thirty seconds later.
            await pollOnce(allowingUnpromptedRecovery: false)
            return
        }

        isRaising = true
        defer { isRaising = false }

        banners.prepare(responseTokens: report.responses)
        let reachedTheScreen = await banners.raise(
            PressureBanner(
                key: key,
                report: report,
                // A banner raised after an unprompted run is raised *because* that
                // run did not resolve the pressure, so this is the ordinary case
                // for an opted-in Mac rather than an edge one.
                automaticRun: automaticRun(forEpisodeIn: report)
            )
        )

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

    // MARK: - The run nobody asked for (HORO-1510)

    /// Starts one bounded unprompted run if the grant says it may, and reports
    /// whether it did.
    ///
    /// `true` means the filesystem may have changed and the caller must re-read.
    /// `false` means nothing was attempted — no runner, no grant, a grant that does
    /// not cover it, this episode's attempt already spent, or no goal this app can
    /// express — and in every one of those the banner path takes over unchanged.
    private func attemptUnpromptedRecovery(
        owed key: PressureBannerKey,
        report: PressureStatusReportDto
    ) async -> Bool {
        guard let runner = unpromptedRecovery, !isRecovering else { return false }

        // Read every poll that owes a banner rather than once at launch: `revoke`
        // takes effect on the next run everywhere else in this product, and a
        // cached grant here would be the one place it did not.
        let grant = await runner.readGrant()
        unpromptedMode = grant.map(AutopilotUnpromptedMode.make)

        let decision = UnpromptedRecoveryPlan.decide(
            owed: key,
            startsUnprompted: grant?.startsUnprompted,
            lastAttempted: lastAutomaticAttempt
        )
        guard case .run = decision else { return false }

        // The Recovery card's own conversion, for the reason
        // `RecoveryDeepLinkContext` gives: a second rounding rule would act on a
        // goal the user's setting does not name. `nil` is not a goal, so there is
        // nothing to run toward and the user is asked instead.
        guard let goalUsedPercent = RecoverySectionView.storedDefaultGoal(
            usedPercent: report.defaultGoal.usedPercent
        ) else {
            return false
        }

        isRecovering = true
        defer { isRecovering = false }

        // `Task {}` and then `.value`, which looks redundant and is not. This
        // deletes things, and `GlomerisClient.runRaw` sends `SIGTERM` when its task
        // is cancelled — so it must not run as a child of `pollTask`, which `stop()`
        // and app teardown both cancel. An unstructured task inherits no
        // cancellation, so awaiting its value here waits for the run to finish
        // whatever happens to the loop around it. Pinned by
        // `UnpromptedRecoveryTests.testTheUnpromptedRunIsNeverReachedFromACancellableTask`.
        let outcome = await Task { @MainActor in
            await runner.run(goalUsedPercent: goalUsedPercent)
        }.value

        // The outcome is the whole account, including of a run that failed —
        // deliberately not copied into `lastErrorMessage`, which the next
        // `pressure show` clears. An automatic run's report surviving exactly until
        // the following poll would be worse than not keeping it.
        lastUnpromptedOutcome = outcome
        automaticRunByEpisode = (episodeId: key.episodeId, outcome: outcome)
        if outcome.consumesTheAttempt {
            lastAutomaticAttempt = key
        }

        // A busy lock mutated nothing, so there is nothing for the caller to
        // re-read and no reason to delay the banner by a round trip.
        return outcome.consumesTheAttempt
    }

    /// What an automatic run did for the episode `report` describes, if that
    /// episode is the one a run was made for.
    ///
    /// The episode check is the whole function. Without it, an outcome from the
    /// pressure event last Tuesday would be reported against today's — and in the
    /// direction that matters: a surface telling someone that space was reclaimed
    /// during an episode in which nothing was.
    ///
    /// `internal` so a test can ask the same question the surfaces do.
    func automaticRun(forEpisodeIn report: PressureStatusReportDto) -> UnpromptedRecoveryOutcome? {
        guard
            let episodeId = report.episode?.episodeId,
            let record = automaticRunByEpisode,
            record.episodeId == episodeId
        else { return nil }
        return record.outcome
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

        // AC 2. Opened *after* the re-read, so the surface is handed the disk as
        // it is now rather than as it was when the banner went up — and regardless
        // of what `respond` returned, because the two refusals it can give both
        // mean the pressure resolved itself, and a user who asked to see Recovery
        // should still see it. A `nil` context is passed on rather than
        // suppressing the window: pressing this button and having nothing happen
        // is the one outcome worth avoiding at the cost of a sparser screen.
        guard RecoveryDeepLink.opensRecovery(responseToken) else { return }
        deepLink?.openRecovery(
            lastReport.map { report in
                RecoveryDeepLinkContext(
                    report: report,
                    automaticRun: automaticRun(forEpisodeIn: report)
                )
            }
        )
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
