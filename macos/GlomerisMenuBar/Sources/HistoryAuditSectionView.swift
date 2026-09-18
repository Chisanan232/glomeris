//
//  HistoryAuditSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1066: popover section showing the last N pressure transitions
//  (`glomeris history --json`, HORO-1046) and the last N action-audit
//  records (`glomeris actions history --json`, HORO-1057's `actions.jsonl`
//  tail). Both are independent, already-computed JSON reports — this view
//  renders their fields and makes no decision of its own. See the
//  standing project rule in GlomerisMenuBarApp.swift: `policyLabel` below
//  is DISPLAY TEXT ONLY, never branched on; the "AUTO_SAFE vs.
//  refused/aborted" visual distinction this ticket's AC asks for reads
//  `outcome`/`abortReason` directly (same pattern as
//  `describeExecuteOutcome` in CandidateDetailView.swift), never
//  `policyLabel`'s text.
//
//  Neither `glomeris history` nor `glomeris actions history` accepts a
//  `--project-root` flag (see `book/src/cli_reference.md`) — both read
//  global daemon-state files (`history.tsv` / `actions.jsonl`), not
//  project-scoped detection state — so, unlike StatusHealthSectionView's
//  `status --json` call, this view does not append
//  `ProjectRootsStore.commandLineArguments`.
//
//  Polling-vs-explicit-refresh design decision: this section follows
//  StatusHealthSectionView's poll-on-appear-and-interval convention,
//  NOT CandidatesSectionView's explicit-refresh-only pattern. That choice
//  is deliberate, not an oversight of CandidatesSectionView's precedent:
//  HORO-1063's AC specifically forbids putting `detect` on a timer because
//  `detect` is (per that ticket's own doc comments) an unpredictable-
//  latency discovery scan the user should trigger explicitly, not a cheap
//  background read. `history` and `actions history` are the opposite
//  shape — bounded, read-only, non-destructive, side-effect-free tails of
//  two small local files, already the pattern `status --json` (a similarly
//  cheap read) uses on a timer in StatusHealthSectionView. Nothing in
//  either ticket's AC restricts polling these two reads, so the cheaper
//  read's precedent applies here, not `detect`'s.
//

import SwiftUI

/// Pure, directly-testable formatting step from one `HistoryEventReportDto`
/// to display strings.
struct HistoryEventRowViewModel: Equatable, Identifiable {
    let id: String
    let timeText: String
    let transitionText: String
    let usedPercentText: String
    let freeText: String

    init(_ dto: HistoryEventReportDto) {
        id = "\(dto.unixTimeSecs)-\(dto.from)-\(dto.to)"
        timeText = Self.formatTimestamp(dto.unixTimeSecs)
        transitionText = "\(dto.from) \u{2192} \(dto.to)"
        usedPercentText = String(format: "%.1f%% used", dto.usedPercent)
        freeText = "\(dto.freeHuman) free"
    }

    private static func formatTimestamp(_ unixTimeSecs: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unixTimeSecs))
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

/// Pure, directly-testable formatting step from one
/// `ActionHistoryEventReportDto` to display strings. `isSuccess` is a
/// field read of `outcome` — never `policyLabel` — so the AC's "AUTO_SAFE
/// vs. refused/aborted must be visually distinguishable" requirement is
/// satisfied without inventing new policy logic in this layer.
struct ActionHistoryRowViewModel: Equatable, Identifiable {
    let id: String
    let timeText: String
    let actionText: String
    let policyLabelText: String
    let outcomeText: String
    let detailText: String?
    let source: String
    /// `true` only for `outcome == "succeeded"` — a direct field read, the
    /// same pattern `describeExecuteOutcome` in CandidateDetailView.swift
    /// uses for its own outcome switch. Drives the outcome text's color in
    /// `HistoryAuditSectionView`, never `policyLabelText`'s.
    let isSuccess: Bool

    init(_ dto: ActionHistoryEventReportDto) {
        id = "\(dto.timestamp)-\(dto.actionId)-\(dto.resourceId)"
        timeText = Self.formatTimestamp(dto.timestamp)
        actionText = "\(dto.actionId) on \(dto.resourceId)"
        policyLabelText = dto.policyLabel
        source = dto.source

        switch dto.outcome {
        case "succeeded":
            isSuccess = true
            outcomeText = "Succeeded (\(dto.actualReclaimedHuman ?? "unknown reclaimed"))"
            detailText = nil
        case "failed":
            isSuccess = false
            outcomeText = "Failed"
            detailText = dto.abortReason
        case "aborted_by_revalidation":
            isSuccess = false
            outcomeText = "Aborted"
            detailText = dto.abortReason
        default:
            isSuccess = false
            outcomeText = "Unrecognized outcome: \(dto.outcome)"
            detailText = dto.abortReason
        }
    }

    private static func formatTimestamp(_ timestamp: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(timestamp))
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

/// Popover section: recent pressure-transition history + recent
/// action-audit records. Polled on appear and every `pollInterval`
/// seconds while the popover is open — see file header for why this
/// section, unlike CandidatesSectionView, follows the polling convention.
struct HistoryAuditSectionView: View {
    private let client: GlomerisClient
    private let historyLimit: Int
    private let actionHistoryLimit: Int
    private let pollInterval: TimeInterval

    @State private var historyEvents: [HistoryEventReportDto] = []
    @State private var actionHistoryEvents: [ActionHistoryEventReportDto] = []
    @State private var lastErrorMessage: String?
    @State private var pollTask: Task<Void, Never>?

    init(
        client: GlomerisClient = GlomerisClient(),
        historyLimit: Int = 10,
        actionHistoryLimit: Int = 10,
        pollInterval: TimeInterval = 10
    ) {
        self.client = client
        self.historyLimit = historyLimit
        self.actionHistoryLimit = actionHistoryLimit
        self.pollInterval = pollInterval
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Recent History")
                .font(.headline)

            if historyEvents.isEmpty {
                Text("No pressure transitions recorded yet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(historyEvents.map(HistoryEventRowViewModel.init)) { row in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(row.transitionText)
                            .font(.caption)
                        Text("\(row.timeText) — \(row.usedPercentText), \(row.freeText)")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Divider()

            Text("Action Audit")
                .font(.headline)

            if actionHistoryEvents.isEmpty {
                Text("No actions recorded yet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(actionHistoryEvents.map(ActionHistoryRowViewModel.init)) { row in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(row.actionText)
                            .font(.caption)
                        HStack(spacing: 4) {
                            Text(row.outcomeText)
                                .font(.caption2)
                                .foregroundStyle(row.isSuccess ? .green : .red)
                            Text("(\(row.policyLabelText), \(row.source))")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        if let detailText = row.detailText {
                            Text(detailText)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        Text(row.timeText)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
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
        async let history = fetchHistory()
        async let actionHistory = fetchActionHistory()
        let (historyResult, actionHistoryResult) = await (history, actionHistory)

        if let historyResult {
            historyEvents = historyResult
        }
        if let actionHistoryResult {
            actionHistoryEvents = actionHistoryResult
        }
    }

    private func fetchHistory() async -> [HistoryEventReportDto]? {
        do {
            let result = try await client.run(
                ["history", "--json", "--limit", String(historyLimit)],
                outputType: HistoryReportDto.self,
                progressType: EmptyProgressDto.self
            )
            lastErrorMessage = nil
            return result.output.events
        } catch {
            lastErrorMessage = "history: \(String(describing: error))"
            return nil
        }
    }

    private func fetchActionHistory() async -> [ActionHistoryEventReportDto]? {
        do {
            let result = try await client.run(
                ["actions", "history", "--json", "--limit", String(actionHistoryLimit)],
                outputType: ActionHistoryReportDto.self,
                progressType: EmptyProgressDto.self
            )
            lastErrorMessage = nil
            return result.output.events
        } catch {
            lastErrorMessage = "actions history: \(String(describing: error))"
            return nil
        }
    }
}

#Preview {
    HistoryAuditSectionView()
        .frame(width: 260)
}
