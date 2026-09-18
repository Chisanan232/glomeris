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

/// Popover section: disk pressure + daemon health, polled on appear and
/// every 10 seconds while visible.
struct StatusHealthSectionView: View {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore
    private let pollInterval: TimeInterval

    @State private var statusReport: StatusReportDto?
    @State private var daemonReport: DaemonStatusReportDto?
    @State private var lastErrorMessage: String?
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

            if let lastErrorMessage {
                Text(lastErrorMessage)
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

    private func fetchStatus() async -> StatusReportDto? {
        do {
            let result = try await client.run(
                ["status", "--json"] + projectRootsStore.commandLineArguments,
                outputType: StatusReportDto.self,
                progressType: EmptyProgressDto.self
            )
            lastErrorMessage = nil
            return result.output
        } catch {
            lastErrorMessage = "status: \(String(describing: error))"
            return nil
        }
    }

    private func fetchDaemonStatus() async -> DaemonStatusReportDto? {
        do {
            let result = try await client.run(
                ["daemon", "status", "--json"],
                outputType: DaemonStatusReportDto.self,
                progressType: EmptyProgressDto.self
            )
            lastErrorMessage = nil
            return result.output
        } catch {
            lastErrorMessage = "daemon status: \(String(describing: error))"
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
