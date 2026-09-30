//
//  StatusHealthSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1062: popover section showing disk pressure (`status --json`) and
//  daemon health (`daemon status --json`). Polled on appear and every 10
//  seconds while the popover is open, via the existing GlomerisClient
//  spawn wrapper (HORO-1060) — no new subprocess mechanism is introduced
//  here.
//
//  See the standing project rule in GlomerisMenuBarApp.swift: this view
//  renders two independent, already-computed JSON reports. It makes no
//  policy/health decision of its own — in particular, HORO-1045's own AC
//  requires launch-agent-loaded and heartbeat freshness to be rendered as
//  two visibly distinct facts, never collapsed into one derived "healthy"
//  indicator. `DaemonHealthViewModel` below is a pure formatting/mapping
//  step (JSON field -> display string), not a health judgment: it never
//  combines `loaded` and `heartbeatAgeSecs` into a single boolean or
//  color, so a caller cannot lose either fact by rendering only the
//  model's output.
//

import SwiftUI

/// Pure view-model derived from one `DaemonStatusReportDto`. Kept as a
/// separate, directly-testable type so HORO-1062's core AC — "loaded" and
/// "heartbeat freshness" stay independently distinguishable — can be
/// asserted on without going through SwiftUI view rendering.
///
/// HORO-1306 replaced the two display strings ("Loaded: Yes", "Last
/// heartbeat: 8s ago") with two `GlomerisTerm`s. The AC being protected is
/// that the two facts remain independent, not the literal wording, and
/// "Loaded: Yes" described what a launchd plist thinks rather than what
/// the product is doing. They are still two separate stored properties
/// derived from two separate DTO fields, so neither can be recovered from
/// the other and no combined "healthy" value exists to render instead.
struct DaemonHealthViewModel: Equatable {
    let loadedTerm: GlomerisTerm
    let heartbeatTerm: GlomerisTerm

    /// The formatted age on its own ("8s", "2h"), or nil when no heartbeat
    /// has ever been recorded. Exposed because absence and age are
    /// different facts and a caller may want to distinguish them without
    /// re-parsing a sentence.
    let heartbeatAgeDescription: String?

    init(_ dto: DaemonStatusReportDto) {
        loadedTerm = GlomerisVocabulary.monitorLoaded(dto.loaded)
        heartbeatAgeDescription = dto.heartbeatAgeSecs.map(Self.formatAge)
        heartbeatTerm = GlomerisVocabulary.monitorHeartbeat(
            ageDescription: heartbeatAgeDescription
        )
    }

    private static func formatAge(_ seconds: UInt64) -> String {
        if seconds < 60 {
            return "\(seconds)s"
        }
        let minutes = seconds / 60
        if minutes < 60 {
            return "\(minutes)m"
        }
        let hours = minutes / 60
        return "\(hours)h"
    }
}

/// Pure view-model for the Disk space card, derived from one
/// `StatusReportDto` (HORO-1506).
///
/// Extracted for the same reason `DaemonHealthViewModel` was: the AC being
/// protected here is mechanical — the figure on screen, the figure VoiceOver
/// reads and the bar's own spoken value must all be the same used-percentage on
/// the same axis — and "the same" is only checkable by asserting on values. The
/// card used to build all three inline, which is how they came to disagree:
/// `"\(statusReport.freeHuman) free of \(statusReport.totalHuman)"` carried no
/// percentage at all, while the bar spoke `Int(fraction * 100)` with no axis
/// word. Nothing could observe that but a person with a screen reader.
///
/// It makes no decision. Every field is a rendering of `usedPercent` or of two
/// strings the CLI already rendered; it never compares the percentage against a
/// threshold, which remains the CLI's job (see `PressureEpisodeMonitor`).
struct DiskCapacityViewModel: Equatable {
    /// The card's primary figure, e.g. `"94.2% used"`.
    let figureText: String

    /// What VoiceOver reads for that figure, e.g. `"94.2 percent used"`.
    let spokenFigure: String

    /// The secondary line, e.g. `"26.8 GB free of 460.4 GB"`. The CLI's own
    /// byte strings, verbatim — this app does not re-render a size it was given
    /// a rendering for.
    let bytesText: String

    /// The bar's fill, `0...1`.
    ///
    /// Derived from the raw measurement rather than from the truncated display
    /// figure: a bar is a continuous encoding and has no tenth to land on, and
    /// clamping is all the correction it needs.
    ///
    /// A non-finite reading gives an empty bar, not a `NaN` one. `min`/`max` do
    /// not filter `NaN` — both comparisons against it are false, so it passes
    /// straight through a clamp — and a `NaN` reaching `ProgressView(value:)` is
    /// not a rendering this app should find out about at the founder's desk.
    /// Empty is also the honest shape: `figureText` says "usage unavailable"
    /// beside it, so the bar is not standing in for a measurement.
    let barFraction: Double

    /// The bar's accessibility value: the same spoken figure, plus the absolute
    /// context, as one string.
    ///
    /// The bar is one accessibility element and a listener can step onto it
    /// alone, so it carries both — the neighbouring `Text` views are separate
    /// elements. `SpokenLabel` does the joining, as everywhere else here.
    let spokenBarValue: String

    init(_ dto: StatusReportDto) {
        figureText = GlomerisUsedPercent.text(dto.usedPercent)
        spokenFigure = GlomerisUsedPercent.spoken(dto.usedPercent)
        bytesText = "\(dto.freeHuman) free of \(dto.totalHuman)"
        barFraction = dto.usedPercent.isFinite ? min(max(dto.usedPercent / 100, 0), 1) : 0
        spokenBarValue = SpokenLabel.compose([spokenFigure, bytesText])
    }
}

/// The two fetches this section performs, each with its own error slot.
///
/// HORO-1297: they previously shared a single `lastErrorMessage`, and they
/// run concurrently — so whichever finished last won. A `daemon status`
/// failure was routinely erased microseconds later by the `status` fetch
/// that had succeeded, which is how the very defect that broke this panel
/// managed to leave no trace in it. Independent slots mean a failure of
/// either fetch stays visible for as long as it persists, and clears only
/// when *that* fetch succeeds.
struct SectionFetchErrors: Equatable {
    var status: String?
    var daemon: String?

    /// Outstanding messages in rendering order. Empty when both fetches
    /// are healthy — which is the only condition under which this section
    /// shows no error at all.
    var messages: [String] {
        [status, daemon].compactMap { $0 }
    }

    /// One short sentence fit for a ~260pt popover, naming the subcommand
    /// and what kind of failure it was.
    ///
    /// Shared by every popover section that runs the CLI — candidates,
    /// candidate detail, and history/audit all route their failures through
    /// here too (HORO-1295), so one install problem reads the same wherever
    /// it shows up instead of once as a sentence and four times as a Swift
    /// error dump.
    ///
    /// `String(describing:)` on a `DecodingError` renders several lines of
    /// Swift type and coding-path detail. That is exactly the wrong thing
    /// to put here: HORO-1297's symptom was malformed CLI stdout, and the
    /// user needs to be told *that* — not handed a `typeMismatch(Swift.
    /// Bool, Swift.DecodingError.Context(...))` dump they cannot act on.
    /// The underlying detail is not invented or hidden, just summarised;
    /// `GlomerisClientError` already carries it for anyone logging.
    ///
    /// Returns `nil` for exactly one case: the call was cancelled (HORO-1308).
    /// A user who pressed Stop, or who closed the popover while a poll was
    /// mid-flight, has not encountered an error and must not be shown one —
    /// and since every error slot in every section is already a `String?`,
    /// "no message" is the one honest value for it. Every other outcome still
    /// produces a sentence.
    static func shortMessage(_ error: Error, subject: String) -> String? {
        guard let clientError = error as? GlomerisClientError else {
            return "\(subject): failed — \(error.localizedDescription)"
        }
        switch clientError {
        case .cancelled:
            return nil
        case .outputDecodingFailed:
            return "\(subject): the CLI's output was not the expected JSON."
        case .executionFailed(let detail):
            let trimmed = detail.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty
                ? "\(subject): the CLI could not be run."
                : "\(subject): \(trimmed)"
        case .usage(let detail):
            let trimmed = detail.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty
                ? "\(subject): the CLI rejected these arguments."
                : "\(subject): \(trimmed)"
        case .unexpectedExitCode(let code):
            return "\(subject): the CLI exited with code \(code)."
        case .executableNotFound(let searched):
            // Names the places that were actually searched rather than
            // telling the user to install "somewhere", because HORO-1295 was
            // precisely a case of the binary being installed and the app
            // looking in the wrong place. Someone who already ran
            // `brew install glomeris` needs to be able to see that.
            return "\(subject): the glomeris CLI was not found. Looked in: "
                + searched.joined(separator: ", ") + "."
        }
    }
}

/// Popover section: disk pressure + daemon health, polled on appear and
/// every 10 seconds while visible.
struct StatusHealthSectionView: View {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore
    private let pollInterval: TimeInterval
    private let cliIdentityProbe: GlomerisCliIdentityProbe

    @State private var statusReport: StatusReportDto?
    @State private var daemonReport: DaemonStatusReportDto?
    @State private var cliIdentity: GlomerisCliIdentityOutcome?
    @State private var fetchErrors = SectionFetchErrors()
    @State private var pollTask: Task<Void, Never>?

    init(
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore(),
        pollInterval: TimeInterval = 10,
        cliIdentityProbe: GlomerisCliIdentityProbe = GlomerisCliIdentityProbe()
    ) {
        self.client = client
        self.projectRootsStore = projectRootsStore
        self.pollInterval = pollInterval
        self.cliIdentityProbe = cliIdentityProbe
    }

    var body: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
            diskCard
            monitorCard
            cliCard
        }
        .task {
            await refresh()
            pollTask = Task {
                while !Task.isCancelled {
                    try? await Task.sleep(nanoseconds: UInt64(pollInterval * 1_000_000_000))
                    if Task.isCancelled { break }
                    await refresh()
                }
            }
        }
        .onDisappear {
            pollTask?.cancel()
            pollTask = nil
        }
    }

    // MARK: - Cards

    /// Disk space. The pressure badge is the headline because it is the one
    /// thing worth glancing at; the percentage is the figure under it, and the
    /// byte figures are the supporting detail under that.
    ///
    /// HORO-1506 put the percentage there. Before it, this card showed a badge,
    /// a free/total byte pair and an untitled bar — so the only percentage
    /// anywhere on it was inside the bar's progress value, which no human
    /// reads. Both figures the user configures (the alert threshold and the
    /// recovery goal) are expressed in percent used, so reading this card meant
    /// dividing two byte counts in your head before you could tell whether you
    /// were near either of them. The percentage leads and the bytes stay: they
    /// answer a different question ("how much room is left"), and dropping them
    /// would trade one mental conversion for the opposite one.
    ///
    /// Every value comes from `DiskCapacityViewModel`, which renders the
    /// percentage through `GlomerisUsedPercent` rather than a local `%.1f` — see
    /// that file for why the rule truncates. The figure's accessibility label is
    /// the spoken form, so VoiceOver says "94.2 percent used" and not a bare
    /// number.
    ///
    /// The failure message renders IN ADDITION to any report already on
    /// screen, never instead of it. A poll that fails after one has
    /// succeeded leaves last-known-good figures visible — which is useful —
    /// but silently showing them as though they were current is the exact
    /// defect HORO-1297 fixed, so the error sits underneath them.
    @ViewBuilder
    private var diskCard: some View {
        GlomerisCard(title: GlomerisVocabulary.pressureAxis) {
            if let statusReport {
                let pressure = GlomerisVocabulary.pressure(statusReport.pressureState)
                let capacity = DiskCapacityViewModel(statusReport)
                GlomerisBadgeView(term: pressure)
                Text(capacity.figureText)
                    .font(GlomerisDesign.titleFont)
                    .accessibilityLabel(capacity.spokenFigure)
                Text(capacity.bytesText)
                    .font(GlomerisDesign.secondaryFont)
                    .foregroundStyle(.secondary)
                capacityBar(capacity: capacity, tone: pressure.tone)
            } else if fetchErrors.status == nil {
                GlomerisStateMessageView(message: .loading("Checking disk space…"))
            }

            if let message = fetchErrors.status {
                GlomerisStateMessageView(message: .failure(message))
            }
        }
    }

    /// The background monitor. Two rows because they are two facts: the
    /// agent can be loaded and silent, or unloaded with an old heartbeat
    /// still on disk, and collapsing them hides exactly the case that
    /// HORO-1297 was.
    @ViewBuilder
    private var monitorCard: some View {
        GlomerisCard(title: GlomerisVocabulary.monitorAxis) {
            if let daemonReport {
                let viewModel = DaemonHealthViewModel(daemonReport)
                GlomerisDetailRow(label: "Status") {
                    GlomerisBadgeView(term: viewModel.loadedTerm)
                }
                GlomerisDetailRow(label: "Last check-in") {
                    GlomerisBadgeView(term: viewModel.heartbeatTerm)
                }
            } else if fetchErrors.daemon == nil {
                GlomerisStateMessageView(message: .loading("Checking the background monitor…"))
            }

            // Same rule as the disk card: additive, never a replacement.
            if let message = fetchErrors.daemon {
                GlomerisStateMessageView(message: .failure(message))
            }
        }
    }

    /// Which `glomeris` the app is driving (HORO-1466).
    ///
    /// Every row here exists because the alternative was measured to be
    /// undiagnosable. Two binaries on one machine reported the same
    /// `--version` while disagreeing about whether an unscoped mutating action
    /// may be offered, so the version is not shown at all and the content
    /// hash is: see `GlomerisCliIdentity`'s header.
    ///
    /// The path and the fingerprint are rendered together, always, from one
    /// `GlomerisCliIdentity` — which cannot exist without both.
    /// `scripts/check-cli-identity-is-reported.sh` pins that so this card
    /// cannot regress to showing a path with no identity beside it, which is
    /// the defect one level up from the one this ticket fixes.
    @ViewBuilder
    private var cliCard: some View {
        GlomerisCard(title: GlomerisVocabulary.cliAxis) {
            switch cliIdentity {
            case .identified(let identity):
                GlomerisBadgeView(
                    term: GlomerisVocabulary.cliExpectation(
                        identity.expectation,
                        path: identity.location.url.path
                    )
                )
                GlomerisDetailRow(label: "In use") {
                    GlomerisPathText(path: identity.location.url.path)
                }
                GlomerisDetailRow(label: "Found") {
                    Text(GlomerisVocabulary.cliSource(identity.location.source))
                        .font(GlomerisDesign.secondaryFont)
                }
                GlomerisDetailRow(label: "Fingerprint") {
                    fingerprintText(identity.content)
                }
                // Only when there is a second hash to show. Rendering an
                // "Expected" row that repeated the one above would imply the
                // comparison had been made in the cases where it has not.
                if case .differs(let expected) = identity.expectation {
                    GlomerisDetailRow(label: "Expected") {
                        fingerprintText(.sha256(expected))
                    }
                }
            case .notFound(let searched):
                GlomerisStateMessageView(
                    message: .failure(
                        "No glomeris command-line tool was found. Looked in: "
                            + searched.joined(separator: ", ") + "."
                    )
                )
            case nil:
                GlomerisStateMessageView(message: .loading("Checking which tool is in use…"))
            }
        }
    }

    /// A content hash, abbreviated on screen with the whole thing in the
    /// tooltip and the accessibility label.
    ///
    /// Same reasoning as `GlomerisPathText`, for the same 260pt column: 64 hex
    /// characters do not fit, and the abbreviation is the part that
    /// discriminates. Nothing is withheld — hovering or VoiceOver gives the
    /// full value.
    @ViewBuilder
    private func fingerprintText(_ content: GlomerisCliContent) -> some View {
        switch content {
        case .sha256(let hash):
            Text(content.shortDescription)
                .font(GlomerisDesign.monospacedFont)
                .help(hash)
                .accessibilityLabel("Fingerprint: \(hash)")
        case .unreadable(let reason):
            Text(content.shortDescription)
                .font(GlomerisDesign.secondaryFont)
                .foregroundStyle(.secondary)
                .help(reason)
                .accessibilityLabel(SpokenLabel.compose(["Fingerprint unavailable", reason]))
        }
    }

    /// A capacity bar, tinted by the pressure tone the badge above already
    /// states in words. It is a redundant second encoding of one fact, not
    /// a new one — which is the only reason a bare colour is acceptable
    /// here. Its accessibility value carries the percentage, because a
    /// filled rectangle conveys nothing to VoiceOver.
    ///
    /// HORO-1506 replaced that value. It used to be built as
    /// `"\(Int(fraction * 100)) percent"`, which was wrong twice over: no axis
    /// word, so a listener could not tell used from free; and a separate
    /// truncation to a whole number, so the same measurement was spoken as
    /// "72 percent" here while the Recovery card rendered "72.0% used" from the
    /// same field. It is now `GlomerisUsedPercent.spoken`, the one rule, so the
    /// bar cannot disagree with the figure printed above it.
    ///
    /// Both the fill and the spoken value come from `DiskCapacityViewModel`, so
    /// what this renders is the same three values the tests assert on — the bar
    /// cannot be corrected without the assertion following it.
    @ViewBuilder
    private func capacityBar(capacity: DiskCapacityViewModel, tone: GlomerisTone) -> some View {
        ProgressView(value: capacity.barFraction)
            .progressViewStyle(.linear)
            .tint(tone.color)
            .accessibilityLabel("Disk used")
            .accessibilityValue(capacity.spokenBarValue)
    }

    /// Both fetches write `@State`, so all three of these are pinned to the
    /// main actor.
    ///
    /// They are dispatched with `async let` and therefore run concurrently.
    /// Without the isolation they inherit from here, two concurrent tasks
    /// would mutate `fetchErrors` — and SwiftUI state generally — off the
    /// main actor: a data race that can tear a two-field struct or corrupt a
    /// `String`'s storage, which is a particularly bad failure mode for the
    /// one surface whose job is to report failures honestly (HORO-1297).
    ///
    /// Isolation costs no concurrency here. Both fetchers spend their time
    /// suspended on subprocess I/O, so they still interleave and both
    /// children still run at once; only the `@State` writes are serialised,
    /// and `GlomerisClient` already reads the pipes off the main thread.
    @MainActor
    private func refresh() async {
        async let status = fetchStatus()
        async let daemon = fetchDaemonStatus()
        // Runs alongside them and spawns nothing: the probe stats and hashes a
        // file, so it answers even when the CLI is too broken to execute —
        // which is exactly when knowing which file it is matters most.
        async let identity = cliIdentityProbe.probeOffMainThread()
        let (statusResult, daemonResult, identityResult) = await (status, daemon, identity)

        if let statusResult {
            statusReport = statusResult
        }
        if let daemonResult {
            daemonReport = daemonResult
        }
        // Assigned unconditionally: unlike the two fetches, every outcome of a
        // probe is a fact worth replacing the previous one with. There is no
        // "failed, keep the last known good" case, because not finding a
        // binary is itself the answer rather than a failure to get one.
        cliIdentity = identityResult
    }

    /// HORO-1501: `["status", "--json"]`, and nothing else.
    ///
    /// This used to append `projectRootsStore.commandLineArguments`. `glomeris
    /// status` reports disk figures and runs no detectors, so a project root
    /// cannot change its answer and the command does not accept one — it exits 2
    /// on an unrecognised argument (HORO-1322). The card therefore failed for
    /// every user who had configured a root and worked for everyone who had not,
    /// which is why four correct-looking sibling call sites kept it company for
    /// as long as they did.
    ///
    /// Still routed through `scoped(_:)` rather than passing the array directly.
    /// The vector is identical either way; what differs is that the question
    /// "does this command take the configured roots?" is asked and answered in
    /// one table for every invocation in the app, instead of being left to
    /// whoever edits this line next.
    @MainActor
    private func fetchStatus() async -> StatusReportDto? {
        do {
            let result = try await client.run(
                projectRootsStore.scoped(["status", "--json"]),
                outputType: StatusReportDto.self,
                progressType: EmptyProgressDto.self
            )
            fetchErrors.status = nil
            return result.output
        } catch {
            fetchErrors.status = SectionFetchErrors.shortMessage(error, subject: "status")
            return nil
        }
    }

    @MainActor
    private func fetchDaemonStatus() async -> DaemonStatusReportDto? {
        do {
            let result = try await client.run(
                ["daemon", "status", "--json"],
                outputType: DaemonStatusReportDto.self,
                progressType: EmptyProgressDto.self
            )
            fetchErrors.daemon = nil
            return result.output
        } catch {
            fetchErrors.daemon = SectionFetchErrors.shortMessage(error, subject: "daemon status")
            return nil
        }
    }
}

/// Neither `status --json` nor `daemon status --json` emit
/// `--progress-json` NDJSON lines today — this is a placeholder `Decodable`
/// so `GlomerisClient.run(_:)`'s generic `Progress` parameter has
/// something concrete to bind to at these call sites.
struct EmptyProgressDto: Decodable {}

#Preview {
    StatusHealthSectionView()
        .frame(width: 260)
}
