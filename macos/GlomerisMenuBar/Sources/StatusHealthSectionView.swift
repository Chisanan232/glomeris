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
struct DaemonHealthViewModel: Equatable {
    let loadedText: String
    let heartbeatText: String

    init(_ dto: DaemonStatusReportDto) {
        loadedText = dto.loaded ? "Loaded: Yes" : "Loaded: No"
        if let ageSecs = dto.heartbeatAgeSecs {
            heartbeatText = "Last heartbeat: \(Self.formatAge(ageSecs)) ago"
        } else {
            heartbeatText = "Last heartbeat: no heartbeat recorded"
        }
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
    static func shortMessage(_ error: Error, subject: String) -> String {
        guard let clientError = error as? GlomerisClientError else {
            return "\(subject): failed — \(error.localizedDescription)"
        }
        switch clientError {
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

    @State private var statusReport: StatusReportDto?
    @State private var daemonReport: DaemonStatusReportDto?
    @State private var fetchErrors = SectionFetchErrors()
    @State private var pollTask: Task<Void, Never>?

    init(
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore(),
        pollInterval: TimeInterval = 10
    ) {
        self.client = client
        self.projectRootsStore = projectRootsStore
        self.pollInterval = pollInterval
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Status")
                .font(.headline)

            if let statusReport {
                Text("Disk: \(statusReport.pressureState) — \(statusReport.freeHuman) free of \(statusReport.totalHuman)")
                    .font(.caption)
            } else {
                Text("Disk: loading…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Divider()

            Text("Daemon Health")
                .font(.headline)

            if let daemonReport {
                let viewModel = DaemonHealthViewModel(daemonReport)
                Text(viewModel.loadedText)
                    .font(.caption)
                Text(viewModel.heartbeatText)
                    .font(.caption)
            } else {
                Text("Daemon: loading…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            ForEach(fetchErrors.messages, id: \.self) { message in
                Text(message)
                    .font(.caption2)
                    .foregroundStyle(.red)
            }
        }
        .padding(.vertical, 4)
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
        let (statusResult, daemonResult) = await (status, daemon)

        if let statusResult {
            statusReport = statusResult
        }
        if let daemonResult {
            daemonReport = daemonResult
        }
    }

    @MainActor
    private func fetchStatus() async -> StatusReportDto? {
        do {
            let result = try await client.run(
                ["status", "--json"] + projectRootsStore.commandLineArguments,
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
