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
//  HORO-1064: tapping a row opens `CandidateDetailView`, the SOLE host
//  of the Clean button — see that file's header for why. There is no
//  "Clean" button, and no button of any kind that triggers cleanup,
//  anywhere in this file's row rendering; see CandidateDetailViewTests'
//  testNoCleanButtonExistsInCandidatesSectionView and
//  testCandidatesSectionViewHasOnlyTheTwoKnownButtonsAndNeverConstructsExecute,
//  which mechanically grep this file for exactly that invariant — the
//  second one pins the button count at two (Refresh, row tap), so adding
//  any control here is a deliberate act with a test to update.
//  (This comment previously named a test that does not exist under that
//  name; corrected while rewriting the rows, since a pointer to a
//  non-existent guard is worse than no pointer at all.)
//
//  ---------------------------------------------------------------------
//  HORO-1306: what a row says, and which axes it can honestly show
//  ---------------------------------------------------------------------
//  A row used to be "cargo_target_dir … ≥ 12.4 GB": a raw Rust enum tag
//  and a number, with the safety class — the single most important thing
//  about a candidate — nowhere on screen at all. You had to open the
//  detail sheet to find out whether Glomeris considered a resource safe to
//  reclaim, which is precisely backwards for a list whose job is to be
//  scanned.
//
//  A row now carries two badges, and DELIBERATELY not three:
//
//    * storage impact — always neutral, at every size. A big number is an
//      opportunity, not a hazard (see GlomerisVocabulary's header).
//    * safety class   — the policy verdict, in words, with its own symbol.
//
//  The third axis from this ticket's design principles — evidence
//  confidence — is NOT rendered here, because `detect --json` does not
//  emit it per candidate: `completeness`/`confidence` exist on the
//  `explain` report only, which is what the detail sheet already shows.
//  Inventing a confidence chip from what a row does have would be a
//  classification made in Swift, and a fabricated one at that. The one
//  evidence fact a row *does* carry is whether the measurement was
//  partial, and that travels honestly inside the impact badge as the
//  leading `≥` and its explanation.
//
//  Row order is whatever `detect` returned. Ranking candidates is a
//  judgment about what matters most, and HORO-1307 owns it — sorting them
//  here would put that judgment in the thin client and then have to be
//  undone.
//

import SwiftUI

/// Pure, directly-testable formatting step from one `DetectCandidateReportDto`
/// to the terms a row renders. In particular, the impact term reads
/// `reclaimableBytesIsLowerBound` directly off the DTO — it never infers a
/// lower bound from `policyLabel` or `reasons`.
///
/// The three terms are built by `GlomerisVocabulary`, which is handed an
/// anonymous token and hands back wording: nothing here decides anything,
/// and in particular `safetyTerm` is display copy for a verdict Rust has
/// already reached, never a verdict of its own.
struct CandidateRowViewModel: Equatable, Identifiable {
    let id: String

    /// What kind of thing this is, in plain language ("Rust build output"),
    /// rendered as prose rather than as a chip so it does not compete with
    /// the two axes that need scanning.
    let kindTerm: GlomerisTerm

    /// How much space is at stake. Neutral at every size.
    let impactTerm: GlomerisTerm

    /// What Glomeris is allowed to do with it.
    let safetyTerm: GlomerisTerm

    /// The impact term's wording on its own ("≥ 1.0 MB", "1.0 MB", "Size
    /// unknown"). Kept as a named property because the `≥`-for-a-partial-
    /// measurement convention is an AC of HORO-1063 and is asserted
    /// directly, without going through SwiftUI rendering.
    var reclaimableText: String { impactTerm.title }

    /// One sentence for VoiceOver, because the row is a single button and
    /// would otherwise be read as a run-on of four separate chips. Ordered
    /// the way the row is scanned: what it is, whether it is safe, how much
    /// is at stake, where it lives.
    var accessibilityLabel: String {
        "\(kindTerm.title). \(safetyTerm.axis): \(safetyTerm.title). "
            + "\(impactTerm.axis): \(impactTerm.title). Path: \(id)."
    }

    init(_ dto: DetectCandidateReportDto) {
        id = dto.resourceId
        kindTerm = GlomerisVocabulary.kind(dto.kind)
        impactTerm = GlomerisVocabulary.storageImpact(
            human: dto.reclaimableHuman,
            isLowerBound: dto.reclaimableBytesIsLowerBound
        )
        safetyTerm = GlomerisVocabulary.safety(dto.policyLabel)
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
    /// HORO-1064: which candidate's detail view is open, if any. Detail
    /// view (and its Clean button) are the ONLY thing a row tap ever
    /// opens — no inline action runs from this list.
    @State private var selectedCandidate: DetectCandidateReportDto?

    init(
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        GlomerisCard(title: "Reclaimable space", trailing: countText) {
            controlRow

            if isScanning, let progressStatusText {
                Text(progressStatusText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }

            if let stateMessage {
                GlomerisStateMessageView(message: stateMessage)
            } else {
                rows
            }

            // Additive, never a replacement — the same rule the status card
            // follows. A scan that fails after an earlier one succeeded
            // leaves the previous list visible, which is useful, but showing
            // it as though it were current is the defect HORO-1297 fixed.
            if let lastErrorMessage {
                GlomerisStateMessageView(message: .failure(lastErrorMessage))
            }
        }
        .sheet(item: $selectedCandidate) { candidate in
            CandidateDetailView(
                resourceId: candidate.resourceId,
                client: client,
                projectRootsStore: projectRootsStore
            )
        }
    }

    // MARK: - Header

    private var controlRow: some View {
        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button(isScanning ? "Scanning…" : "Refresh") {
                Task { await runDetect() }
            }
            .disabled(isScanning)

            Text(lastScannedText)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)

            Spacer(minLength: 0)
        }
    }

    /// Shown beside the card title. `nil` while there is nothing to count,
    /// so the title line stays quiet rather than announcing "0 items" next
    /// to a message that already says so in words.
    private var countText: String? {
        guard !candidates.isEmpty else { return nil }
        return candidates.count == 1 ? "1 item" : "\(candidates.count) items"
    }

    private var lastScannedText: String {
        guard let lastScannedAt else { return "not scanned yet" }
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .medium
        return "scanned \(formatter.string(from: lastScannedAt))"
    }

    // MARK: - Body states

    /// The four things this card can be showing instead of rows, and why
    /// they are four rather than one:
    ///
    ///   - a first scan in flight is slow on a cold cache and must not look
    ///     like a stuck or broken install;
    ///   - never having scanned is not a clean bill of health;
    ///   - having scanned and found nothing IS good news;
    ///   - a failed scan must never be presented as either of the last two,
    ///     which is why an empty list plus an error message does not also
    ///     claim "nothing worth reclaiming".
    private var stateMessage: GlomerisStateMessage? {
        guard candidates.isEmpty else { return nil }
        if isScanning {
            return .loading("Scanning for reclaimable space…")
        }
        if lastErrorMessage != nil {
            // The failure message below is the whole story; an "all clear"
            // sitting next to it would contradict it.
            return nil
        }
        if lastScannedAt == nil {
            return .notLookedYet(
                "No scan yet",
                detail: "Refresh to look for space you can reclaim."
            )
        }
        return .empty(
            "Nothing worth reclaiming",
            detail: "Everything Glomeris can see is either in use or already small."
        )
    }

    @ViewBuilder
    private var rows: some View {
        ForEach(Array(candidates.map(CandidateRowViewModel.init).enumerated()), id: \.element.id) { index, row in
            if index > 0 {
                Divider()
            }
            rowView(row)
        }
    }

    /// One candidate. Two lines of substance plus its path: what it is and
    /// how much is at stake on the first line, the safety verdict on the
    /// second, the path underneath as identity.
    ///
    /// The impact badge is unfilled and the safety badge is filled, so the
    /// eye lands on the verdict first when scanning a long list — a size is
    /// only interesting once you know whether you are allowed to act on it.
    @ViewBuilder
    private func rowView(_ row: CandidateRowViewModel) -> some View {
        Button {
            selectedCandidate = candidates.first { $0.resourceId == row.id }
        } label: {
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                    Text(row.kindTerm.title)
                        .font(GlomerisDesign.primaryFont)
                        .lineLimit(1)
                    Spacer(minLength: GlomerisDesign.inlineSpacing)
                    GlomerisBadgeView(term: row.impactTerm, filled: false)
                }

                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    GlomerisBadgeView(term: row.safetyTerm)
                    Spacer(minLength: 0)
                    // Affordance only: it says "there is more behind this
                    // row", which is what a tap does. It carries no state,
                    // so it is hidden from VoiceOver rather than read out
                    // once per row.
                    Image(systemName: "chevron.right")
                        .imageScale(.small)
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }

                GlomerisPathText(path: row.id)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(row.accessibilityLabel)
        .accessibilityHint("Opens the evidence and the available actions for this resource.")
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
            lastErrorMessage = SectionFetchErrors.shortMessage(error, subject: "detect")
        }

        isScanning = false
        progressStatusText = nil
    }
}

#Preview {
    CandidatesSectionView()
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
