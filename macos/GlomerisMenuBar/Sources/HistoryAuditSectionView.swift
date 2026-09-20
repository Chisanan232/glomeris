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
//  `outcome` directly (via `GlomerisVocabulary.outcome`, the same table
//  `CandidateDetailView` uses for a live execute result), never
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
//  ---------------------------------------------------------------------
//  HORO-1306: two separate questions, two cards, and one error each
//  ---------------------------------------------------------------------
//  This was one undifferentiated column of `.caption2` text with a divider
//  in the middle, and it answered its two questions in the CLI's own
//  vocabulary: "OK → WARN", "AUTO_SAFE, execute". Three things changed.
//
//  1. It is two cards, because they answer two unrelated questions — what
//     has been happening to the disk, and what Glomeris has actually done
//     to it. The second is the audit trail a user checks when deciding
//     whether to trust the thing, so it now says who triggered each action
//     in words rather than leaving `free` and `emergency` to be guessed at.
//
//  2. Every raw token goes through `GlomerisVocabulary`. Notably that makes
//     `aborted_by_revalidation` read as "Stopped safely" in caution rather
//     than as a red failure — it is the revalidation guard doing its job,
//     and painting it as a fault would teach a user that Glomeris breaks
//     whenever it protects them. The raw token stays on every term.
//
//  3. Each list owns its own error message. Before, both fetches wrote one
//     shared `lastErrorMessage` and each cleared it on success, so a
//     healthy `history` read silently erased a failing `actions history`
//     one — the audit trail could be unreadable and the popover would say
//     nothing about it.
//

import SwiftUI

/// Pure, directly-testable formatting step from one `HistoryEventReportDto`
/// to display strings.
struct HistoryEventRowViewModel: Equatable, Identifiable {
    let id: String
    let timeText: String
    let usedPercentText: String
    let freeText: String

    /// The two ends of the transition, as terms. `from`/`to` are
    /// `PressureState::as_str()` output (see `HistoryEventReport`'s Rust doc
    /// comment), which is the same vocabulary `status --json`'s
    /// `pressure_state` uses — so the popover's history and its status card
    /// name the same state the same way instead of inventing two wordings.
    let fromTerm: GlomerisTerm
    let toTerm: GlomerisTerm

    /// "Plenty of room → Filling up".
    var transitionText: String {
        "\(fromTerm.title) \u{2192} \(toTerm.title)"
    }

    /// The same transition in the CLI's own tokens. This is a log, and a log
    /// whose entries have been paraphrased is harder to quote in a bug
    /// report than one that shows both.
    var rawTransitionText: String {
        "\(fromTerm.token) \u{2192} \(toTerm.token)"
    }

    var accessibilityLabel: String {
        "\(timeText). \(GlomerisVocabulary.pressureAxis) went from "
            + "\(fromTerm.title) to \(toTerm.title). \(usedPercentText), \(freeText)."
    }

    init(_ dto: HistoryEventReportDto) {
        id = "\(dto.unixTimeSecs)-\(dto.from)-\(dto.to)"
        timeText = Self.formatTimestamp(dto.unixTimeSecs)
        usedPercentText = String(format: "%.1f%% used", dto.usedPercent)
        freeText = "\(dto.freeHuman) free"
        fromTerm = GlomerisVocabulary.pressure(dto.from)
        toTerm = GlomerisVocabulary.pressure(dto.to)
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
/// `ActionHistoryEventReportDto` to display strings.
///
/// `outcomeTerm` is built from `outcome` — never from `policyLabel` — so the
/// AC's "AUTO_SAFE vs. refused/aborted must be visually distinguishable"
/// requirement is carried by the field that actually records what happened,
/// without inventing any policy logic in this layer.
struct ActionHistoryRowViewModel: Equatable, Identifiable {
    let id: String
    let timeText: String

    /// What ran, e.g. `cargo.clean.target_dir`. Shown verbatim: action ids
    /// are stable identifiers a user may need to quote, and there is no
    /// vocabulary for them because the action registry is open-ended.
    let actionId: String

    /// What it ran on. Rendered as a path rather than folded into a
    /// sentence, so middle truncation keeps both ends of it legible.
    let resourceId: String

    /// What happened. This is what tones the row.
    let outcomeTerm: GlomerisTerm

    /// Who triggered it — you, the recovery loop, or the emergency path.
    let sourceTerm: GlomerisTerm

    /// What the policy said about the resource at the time. Context only.
    let safetyTerm: GlomerisTerm

    /// How much space came back, when the record reports it.
    let reclaimedText: String?

    /// Why it stopped, verbatim from `abort_reason`.
    let detailText: String?

    var accessibilityLabel: String {
        var parts = [
            "\(timeText). \(actionId).",
            "\(outcomeTerm.axis): \(outcomeTerm.title).",
        ]
        if let reclaimedText {
            parts.append("\(reclaimedText).")
        }
        if let detailText {
            parts.append("\(detailText).")
        }
        parts.append("\(sourceTerm.axis): \(sourceTerm.title).")
        parts.append("\(safetyTerm.axis): \(safetyTerm.title).")
        parts.append("Path: \(resourceId).")
        return parts.joined(separator: " ")
    }

    init(_ dto: ActionHistoryEventReportDto) {
        id = "\(dto.timestamp)-\(dto.actionId)-\(dto.resourceId)"
        timeText = Self.formatTimestamp(dto.timestamp)
        actionId = dto.actionId
        resourceId = dto.resourceId
        outcomeTerm = GlomerisVocabulary.outcome(dto.outcome)
        sourceTerm = GlomerisVocabulary.actionSource(dto.source)
        safetyTerm = GlomerisVocabulary.safety(dto.policyLabel)
        reclaimedText = dto.actualReclaimedHuman.map { "reclaimed \($0)" }
        detailText = dto.abortReason
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
    /// One error per list — see the file header: a shared one let a healthy
    /// read erase a broken one.
    @State private var historyErrorMessage: String?
    @State private var actionHistoryErrorMessage: String?
    /// Until the first poll returns, "nothing recorded yet" would be a claim
    /// about two files nobody has read.
    @State private var hasLoadedOnce = false
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
        VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
            diskHistoryCard
            actionHistoryCard
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

    // MARK: - Disk space history

    private var diskHistoryCard: some View {
        GlomerisCard(title: "Disk space history") {
            if let message = stateMessage(
                isEmpty: historyEvents.isEmpty,
                errorMessage: historyErrorMessage,
                loadingSubject: "Reading the disk-space log…",
                emptyTitle: "No changes recorded yet",
                emptyDetail: "A line is logged here each time free space crosses a threshold."
            ) {
                GlomerisStateMessageView(message: message)
            } else {
                ForEach(
                    Array(historyEvents.map(HistoryEventRowViewModel.init).enumerated()),
                    id: \.element.id
                ) { index, row in
                    if index > 0 {
                        Divider()
                    }
                    historyRow(row)
                }
            }

            // Additive, never a replacement: a failed poll leaves the last
            // good list on screen, but must say that it is the last good one.
            if let historyErrorMessage {
                GlomerisStateMessageView(message: .failure(historyErrorMessage))
            }
        }
    }

    private func historyRow(_ row: HistoryEventRowViewModel) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: row.fromTerm, filled: false)
                Image(systemName: "arrow.right")
                    .imageScale(.small)
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
                // The state it moved TO is the one that matters, so only that
                // end is filled.
                GlomerisBadgeView(term: row.toTerm)
                Spacer(minLength: 0)
            }
            Text("\(row.timeText) — \(row.usedPercentText), \(row.freeText)")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(row.accessibilityLabel)
    }

    // MARK: - What Glomeris has done

    private var actionHistoryCard: some View {
        GlomerisCard(title: "What Glomeris has done") {
            if let message = stateMessage(
                isEmpty: actionHistoryEvents.isEmpty,
                errorMessage: actionHistoryErrorMessage,
                loadingSubject: "Reading the action log…",
                emptyTitle: "Nothing has been cleaned yet",
                emptyDetail: "Every action is recorded here, including the ones Glomeris refuses."
            ) {
                GlomerisStateMessageView(message: message)
            } else {
                ForEach(
                    Array(actionHistoryEvents.map(ActionHistoryRowViewModel.init).enumerated()),
                    id: \.element.id
                ) { index, row in
                    if index > 0 {
                        Divider()
                    }
                    actionRow(row)
                }
            }

            if let actionHistoryErrorMessage {
                GlomerisStateMessageView(message: .failure(actionHistoryErrorMessage))
            }
        }
    }

    private func actionRow(_ row: ActionHistoryRowViewModel) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: row.outcomeTerm)
                if let reclaimedText = row.reclaimedText {
                    Text(reclaimedText)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }

            Text(row.actionId)
                .font(GlomerisDesign.secondaryFont)
                .lineLimit(1)
            GlomerisPathText(path: row.resourceId)

            if let detailText = row.detailText {
                Text(detailText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            // Who asked for it and what the policy said, unfilled so they
            // read as provenance rather than competing with the outcome.
            HStack(spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: row.sourceTerm, filled: false)
                GlomerisBadgeView(term: row.safetyTerm, filled: false)
                Spacer(minLength: 0)
                Text(row.timeText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(row.accessibilityLabel)
    }

    // MARK: - Body states

    /// The one place both lists decide what to show instead of rows, so the
    /// two cannot drift apart. `nil` means "render the rows".
    ///
    /// A failure returns `nil` deliberately: the error is rendered additively
    /// below the list, and an "all clear" sitting beside it would contradict
    /// it — the same rule the candidates and status cards follow.
    private func stateMessage(
        isEmpty: Bool,
        errorMessage: String?,
        loadingSubject: String,
        emptyTitle: String,
        emptyDetail: String
    ) -> GlomerisStateMessage? {
        guard isEmpty else { return nil }
        if !hasLoadedOnce { return .loading(loadingSubject) }
        if errorMessage != nil { return nil }
        return .nothingRecorded(emptyTitle, detail: emptyDetail)
    }

    // MARK: - Fetching

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
        hasLoadedOnce = true
    }

    private func fetchHistory() async -> [HistoryEventReportDto]? {
        do {
            let result = try await client.run(
                ["history", "--json", "--limit", String(historyLimit)],
                outputType: HistoryReportDto.self,
                progressType: EmptyProgressDto.self
            )
            historyErrorMessage = nil
            return result.output.events
        } catch {
            historyErrorMessage = SectionFetchErrors.shortMessage(error, subject: "history")
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
            actionHistoryErrorMessage = nil
            return result.output.events
        } catch {
            actionHistoryErrorMessage = SectionFetchErrors.shortMessage(
                error,
                subject: "actions history"
            )
            return nil
        }
    }
}

#Preview {
    HistoryAuditSectionView()
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
