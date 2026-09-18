//
//  CandidatesSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1063: candidates list — a cached `detect --json` snapshot ("Last
//  scanned: <time>") plus an explicit Refresh button that re-runs the scan
//  with live per-detector progress (Phase A's NDJSON `--progress-json`
//  stream, HORO-1052, via GlomerisClient's live-progress callback added in
//  this same ticket).
//
//  `detect` is invoked from exactly one place below: the function below, called
//  only from the Refresh button's action. There is no appear-triggered task
//  auto-scan on first appearance, no `Timer`, and no re-scan loop of any
//  kind — see the ticket's core AC ("no code path in this ticket ever
//  calls detect on a timer") and `CandidatesSectionViewTests
//  .testDetectIsOnlyCalledWithProgressJSONAndNeverOnATimer`, which
//  mechanically greps this file for exactly that invariant.
//
//  Judgment call on "first load": rather than have first-appearance
//  silently trigger one initial scan (technically not a *timer*, but the
//  same "surprise latency with no explicit user action" shape the
//  founder-dogfood finding was about), this view shows an explicit empty
//  "No scan yet" prompt until the user taps Refresh themselves. This
//  keeps the invariant simple to prove (`detect` has exactly one call
//  site, gated by exactly one button action) and matches the ticket's
//  own framing of Refresh as "the" explicit trigger.
//
//  See the standing project rule in GlomerisMenuBarApp.swift: this view
//  renders `executable`/`offered_actions`/`refusal_reason` fields exactly
//  as `detect --json` computed them. It must never re-derive
//  "can this be cleaned" from `policy_label` or `reasons` — those two
//  fields are shown only as human-readable context, never branched on.
//

import SwiftUI

/// Pure, directly-testable formatting step from one `DetectCandidateReportDto`
/// to display strings. In particular, `reclaimableText` reads
/// `reclaimableBytesIsLowerBound` directly off the DTO — it never infers a
/// lower bound from `policyLabel` or `reasons`.
struct CandidateRowViewModel: Equatable, Identifiable {
    let id: String
    let kindText: String
    let reclaimableText: String

    init(_ dto: DetectCandidateReportDto) {
        id = dto.resourceId
        kindText = dto.kind
        if let human = dto.reclaimableHuman {
            reclaimableText = dto.reclaimableBytesIsLowerBound ? "\u{2265} \(human)" : human
        } else {
            reclaimableText = "unknown"
        }
    }
}

/// Pure formatting step from a `ProgressEventDto` to a one-line status
/// string, shown while a scan is running.
enum ProgressStatusText {
    static func text(for event: ProgressEventDto) -> String {
        switch event {
        case .detectorStarted(let detector):
            return "Scanning: \(detector)…"
        case .detectorFinished(let detector, let candidatesFound):
            return "Finished \(detector) (\(candidatesFound) found)"
        }
    }
}

/// Popover section: cached candidates list + explicit Refresh with live
/// progress. No polling, no timer — see file header.
struct CandidatesSectionView: View {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    @State private var candidates: [DetectCandidateReportDto] = []
    @State private var lastScannedAt: Date?
    @State private var isScanning = false
    @State private var progressStatusText: String?
    @State private var lastErrorMessage: String?

    init(
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Candidates")
                    .font(.headline)
                Spacer()
                Button(isScanning ? "Scanning…" : "Refresh") {
                    Task { await runDetect() }
                }
                .disabled(isScanning)
            }

            Text(lastScannedText)
                .font(.caption2)
                .foregroundStyle(.secondary)

            if isScanning, let progressStatusText {
                Text(progressStatusText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if candidates.isEmpty, !isScanning {
                Text(lastScannedAt == nil ? "No scan yet — tap Refresh to scan." : "No candidates found.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(candidates.map(CandidateRowViewModel.init)) { row in
                    HStack {
                        Text(row.kindText)
                            .font(.caption)
                        Spacer()
                        Text(row.reclaimableText)
                            .font(.caption)
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
    }

    private var lastScannedText: String {
        guard let lastScannedAt else { return "Last scanned: never" }
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .medium
        return "Last scanned: \(formatter.string(from: lastScannedAt))"
    }

    /// The one and only place `detect` is invoked in this file — always
    /// with `--progress-json`, always in direct response to the Refresh
    /// button's action closure above, never from an appear-triggered task, a `Timer`,
    /// or any re-scan loop.
    private func runDetect() async {
        isScanning = true
        progressStatusText = nil
        lastErrorMessage = nil

        do {
            let result = try await client.run(
                ["detect", "--json", "--progress-json"] + projectRootsStore.commandLineArguments,
                outputType: DetectReportDto.self,
                progressType: ProgressEventDto.self,
                onProgress: { event in
                    Task { @MainActor in
                        progressStatusText = ProgressStatusText.text(for: event)
                    }
                }
            )
            candidates = result.output.candidates
            lastScannedAt = Date()
        } catch {
            lastErrorMessage = "detect: \(String(describing: error))"
        }

        isScanning = false
        progressStatusText = nil
    }
}

#Preview {
    CandidatesSectionView()
        .frame(width: 260)
}
