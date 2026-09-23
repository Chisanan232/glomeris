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
//  HORO-1064: tapping a row leads to `CandidateDetailView`, the SOLE host
//  of the Clean button — see that file's header for why. HORO-1357 changed
//  how it gets there and nothing about what it is: the tap is reported
//  upward via `onOpenDetail`, and the popover shell shows the detail view
//  in place of this list, instead of this view presenting it as a sheet
//  over a panel that a sheet can dismiss. There is no
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
//  ---------------------------------------------------------------------
//  HORO-1307: priority, thresholds, and who decides them
//  ---------------------------------------------------------------------
//  Row order is still whatever `detect` returned — but `detect` now returns
//  a canonical order (biggest reclaimable size first, unmeasured last,
//  deterministic; see `reporting::ranking` in the Rust crate). Before that,
//  candidates arrived in detector-registration order, so a 40 GB Cargo
//  `target/` could sit below a 2 MB npm cache. Fixing that in Rust rather
//  than here is the whole point: "what matters most" is a product judgment,
//  the CLI and this app must not disagree about it, and a `.sorted` call in
//  a thin client would be exactly the kind of second opinion this codebase
//  is built to avoid.
//
//  For the same reason there is no size sort in the menu below. Re-sorting
//  by size here would duplicate Rust's ranking and then drift from its
//  tie-break and lower-bound rules — `≥ 5 GB` outranks an exact 5 GB, and
//  that subtlety would silently go missing. The menu offers only
//  "Recommended", which is Rust's order passed through untouched, and
//  "Path", which is an alphabetical index rather than a competing judgment
//  about importance.
//
//  ---------------------------------------------------------------------
//  HORO-1365: this card draws the scan, it does not own it
//  ---------------------------------------------------------------------
//  The candidate list, the scan timestamp and the two view controls live in
//  `ScanState`, handed in and required. This view is still the only thing
//  that writes them, and still only from the Refresh button — but its own
//  mount/unmount no longer decides whether a completed scan survives.
//  See `OverviewState.swift` for the defect that made this necessary.
//
//  A row can now carry a THIRD badge, but only sometimes: the
//  `impact_tier` chip appears on `notable`/`large` candidates and on nothing
//  else. A chip on every row is not emphasis, it is noise. The tier is
//  neutral-toned at every level, like the size badge and for the same
//  reason — "Biggest wins" is not a warning, and hue stays reserved for the
//  safety axis so the two axes can never be confused at a glance.
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

    /// Whether this is one of the few candidates worth pausing on, or `nil`
    /// for the ordinary majority (HORO-1307). Read straight off the DTO's
    /// `impactTier` — the magnitude judgment is Rust's, not this app's.
    let impactTierTerm: GlomerisTerm?

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
    /// The tier is appended after the size rather than replacing it, and is
    /// omitted entirely when there is none, so VoiceOver users get the same
    /// "only the notable rows are called out" signal that sighted users get
    /// from the chip — rather than hearing an extra clause on every row, or
    /// nothing at all.
    var accessibilityLabel: String {
        var label = "\(kindTerm.title). \(safetyTerm.axis): \(safetyTerm.title). "
            + "\(impactTerm.axis): \(impactTerm.title)."
        if let impactTierTerm {
            label += " \(impactTierTerm.title)."
        }
        return label + " Path: \(id)."
    }

    init(_ dto: DetectCandidateReportDto) {
        id = dto.resourceId
        kindTerm = GlomerisVocabulary.kind(dto.kind)
        impactTerm = GlomerisVocabulary.storageImpact(
            human: dto.reclaimableHuman,
            isLowerBound: dto.reclaimableBytesIsLowerBound
        )
        impactTierTerm = GlomerisVocabulary.impactTier(dto.impactTier)
        safetyTerm = GlomerisVocabulary.safety(dto.policyLabel)
    }
}

/// How the list is ordered on screen.
///
/// Deliberately only two cases, and deliberately no "largest first": that
/// IS `recommended`, because `detect` already returns candidates biggest
/// first (`reporting::ranking`). Offering it as a separate option would
/// imply the default is something else, and implementing it here would be a
/// second ranking that could drift from Rust's — see the file header.
enum CandidateSortOrder: String, CaseIterable, Identifiable {
    /// Rust's canonical order, passed through completely untouched.
    case recommended
    /// Alphabetical by path: an index for finding a known resource, not a
    /// claim about what matters.
    case path

    var id: String { rawValue }

    var label: String {
        switch self {
        case .recommended: return "Biggest first"
        case .path: return "Path (A–Z)"
        }
    }
}

/// Which safety classes to show.
///
/// This is a view filter and nothing else. It hides rows; it cannot enable,
/// authorise or perform anything. That is structurally guaranteed rather
/// than merely intended: this list has no action affordance at all — its
/// only two buttons are Refresh and the row tap, and a test pins that count
/// — so there is nothing here for a filter to unlock. The Clean button
/// lives solely in `CandidateDetailView` and reads `executable` from
/// `explain`.
///
/// `PROTECTED` is a first-class filter value rather than something hidden by
/// default: "what on this machine is off-limits, and why" is a legitimate
/// question, and a product that quietly omits protected items teaches users
/// that protection means invisibility.
enum CandidateSafetyFilter: String, CaseIterable, Identifiable {
    case all
    case autoSafe
    case ask
    case protected
    case unknownIncomplete

    var id: String { rawValue }

    var label: String {
        switch self {
        case .all: return "All"
        case .autoSafe: return "Safe to reclaim"
        case .ask: return "Asks first"
        case .protected: return "Protected"
        case .unknownIncomplete: return "Not enough evidence"
        }
    }

    /// The same filter named as a noun phrase, for use mid-sentence in the
    /// "nothing matched" message.
    ///
    /// Separate from `label` because a picker label and a sentence fragment
    /// are not interchangeable: "Not enough evidence" is right on a menu row
    /// and produces "none are not enough evidence" in prose. Empty-state copy
    /// is exactly where a user is already confused, so it is the last place
    /// that can afford a garbled sentence.
    var midSentenceDescription: String {
        switch self {
        case .all: return "candidates"
        case .autoSafe: return "safe to reclaim"
        case .ask: return "waiting on your confirmation"
        case .protected: return "protected"
        case .unknownIncomplete: return "short on evidence"
        }
    }

    /// The `policy_label` token this filter keeps, or `nil` for "keep
    /// everything".
    ///
    /// Comparing tokens to decide *visibility* is not policy branching:
    /// nothing downstream of this changes what Glomeris is permitted to do.
    /// `scripts/check-no-policy-label-branching.sh` guards the dangerous
    /// version of this — a `policyLabel` comparison that gates an action —
    /// and that script's Check 1 scans for exactly that shape. Keeping the
    /// mapping in this enum, with no reference to `policyLabel` and no
    /// conditional, keeps the guard meaningful instead of teaching it to
    /// tolerate a near-miss.
    var keptToken: String? {
        switch self {
        case .all: return nil
        case .autoSafe: return "AUTO_SAFE"
        case .ask: return "ASK"
        case .protected: return "PROTECTED"
        case .unknownIncomplete: return "UNKNOWN_INCOMPLETE"
        }
    }
}

/// Pure, directly-testable list shaping: apply a filter, then an order.
///
/// Extracted from the view so the interesting behaviour — that
/// `.recommended` is a no-op, that filtering never invents or reorders
/// anything, and that an empty result is distinguishable from an empty scan
/// — is asserted without rendering SwiftUI.
enum CandidateListShaping {
    static func shape(
        _ candidates: [DetectCandidateReportDto],
        filter: CandidateSafetyFilter,
        order: CandidateSortOrder
    ) -> [DetectCandidateReportDto] {
        let filtered: [DetectCandidateReportDto]
        if let kept = filter.keptToken {
            filtered = candidates.filter { $0.policyLabel == kept }
        } else {
            filtered = candidates
        }

        switch order {
        case .recommended:
            // Rust's order, untouched. Not a `.sorted` call by design.
            return filtered
        case .path:
            return filtered.sorted { $0.resourceId < $1.resourceId }
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

    /// HORO-1365: what the last scan found is NOT owned here. This card is
    /// the only thing that writes it — from the Refresh button, below — but
    /// it does not get to decide how long it lives. `@State` here would tie
    /// the result to this view's position in the hierarchy, and the panel's
    /// drill-down is what decides that; see `OverviewState.swift`.
    @ObservedObject private var scan: ScanState

    /// HORO-1064/HORO-1357: what a row tap does — report the tapped
    /// resource upward, so the popover shell can show its detail view in
    /// place of this list.
    ///
    /// This used to be a `.sheet(item:)` presented from right here. A sheet
    /// on a `MenuBarExtra(.window)` panel is a fragile shape: the panel
    /// does not take key focus, and presenting or resizing a sheet on it can
    /// order it out — which is exactly what expanding the detail view's raw
    /// values did. Reporting the tap upward keeps the whole interaction
    /// inside the one panel, and leaves this list with no presentation of
    /// its own to get wrong.
    ///
    /// Still the ONLY thing a row tap ever does: no action runs from this
    /// list, and nothing here decides what may run.
    private let onOpenDetail: (String) -> Void

    /// `scan` has deliberately NO default. A defaulted `ScanState()` would let
    /// any call site quietly construct a fresh, empty scan and render "No scan
    /// yet" over a scan that had in fact completed — the HORO-1365 defect
    /// again, arriving through a convenience rather than through a
    /// re-presentation. Every real call site has to pass the one the app owns.
    init(
        scan: ScanState,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore(),
        onOpenDetail: @escaping (String) -> Void = { _ in }
    ) {
        self.scan = scan
        self.client = client
        self.projectRootsStore = projectRootsStore
        self.onOpenDetail = onOpenDetail
    }

    var body: some View {
        GlomerisCard(title: "Reclaimable space", trailing: countText) {
            controlRow

            if scan.isScanning, let progressStatusText = scan.progressStatusText {
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
            if let lastErrorMessage = scan.lastErrorMessage {
                GlomerisStateMessageView(message: .failure(lastErrorMessage))
            }
        }
    }

    // MARK: - Header

    private var controlRow: some View {
        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button(scan.isScanning ? "Scanning…" : "Refresh") {
                Task { await runDetect() }
            }
            .disabled(scan.isScanning)

            Text(lastScannedText)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)

            Spacer(minLength: 0)

            viewOptionsMenu
        }
    }

    /// Sort and filter, folded into one menu rather than two visible
    /// pickers: in a panel this narrow, two always-expanded controls would
    /// take more room than the first candidate row and compete with the
    /// content they exist to organise. `Menu` also adds no `Button(`, which
    /// keeps the "this list has exactly two buttons" invariant — and the
    /// argument that it cannot authorise anything — intact and testable.
    ///
    /// The label shows a dot when a filter is active, because a filtered
    /// list that looks unfiltered is how a user concludes Glomeris found
    /// nothing when it found plenty.
    private var viewOptionsMenu: some View {
        Menu {
            Picker("Order", selection: $scan.sortOrder) {
                ForEach(CandidateSortOrder.allCases) { order in
                    Text(order.label).tag(order)
                }
            }
            Picker("Show", selection: $scan.safetyFilter) {
                ForEach(CandidateSafetyFilter.allCases) { filter in
                    Text(filter.label).tag(filter)
                }
            }
        } label: {
            Image(
                systemName: scan.safetyFilter == .all
                    ? "line.3.horizontal.decrease.circle"
                    : "line.3.horizontal.decrease.circle.fill"
            )
        }
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(
            scan.safetyFilter == .all
                ? "View options. Showing all candidates, \(scan.sortOrder.label)."
                : "View options. Filtered to \(scan.safetyFilter.label), \(scan.sortOrder.label)."
        )
    }

    /// The candidates actually rendered, after the user's filter and order.
    private var visibleCandidates: [DetectCandidateReportDto] {
        CandidateListShaping.shape(scan.candidates, filter: scan.safetyFilter, order: scan.sortOrder)
    }

    /// Shown beside the card title. `nil` while there is nothing to count,
    /// so the title line stays quiet rather than announcing "0 items" next
    /// to a message that already says so in words.
    ///
    /// When a filter is hiding rows this reads "3 of 11 items". Showing only
    /// the visible count would misreport what the scan actually found, and
    /// showing only the total would contradict the list underneath it.
    private var countText: String? {
        guard !scan.candidates.isEmpty else { return nil }
        let visible = visibleCandidates.count
        if visible == scan.candidates.count {
            return visible == 1 ? "1 item" : "\(visible) items"
        }
        return "\(visible) of \(scan.candidates.count) items"
    }

    private var lastScannedText: String {
        guard let lastScannedAt = scan.lastScannedAt else { return "not scanned yet" }
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .medium
        return "scanned \(formatter.string(from: lastScannedAt))"
    }

    // MARK: - Body states

    /// The five things this card can be showing instead of rows, and why
    /// they are five rather than one:
    ///
    ///   - a first scan in flight is slow on a cold cache and must not look
    ///     like a stuck or broken install;
    ///   - never having scanned is not a clean bill of health;
    ///   - having scanned and found nothing IS good news;
    ///   - a failed scan must never be presented as either of the last two,
    ///     which is why an empty list plus an error message does not also
    ///     claim "nothing worth reclaiming";
    ///   - and (HORO-1307) a filter hiding every row is the user's own doing,
    ///     not a fact about their disk. Reusing "Nothing worth reclaiming"
    ///     there would be an outright false claim about the machine, and the
    ///     worst kind: silently self-inflicted and with the fix — clear the
    ///     filter — invisible.
    private var stateMessage: GlomerisStateMessage? {
        // Checked before the "no candidates at all" cases: a non-empty scan
        // whose rows are all filtered out is a different situation from an
        // empty scan, and must not borrow its wording.
        if !scan.candidates.isEmpty, visibleCandidates.isEmpty {
            return .filteredOut(
                "No candidates match this filter",
                detail: "Glomeris found \(scan.candidates.count) "
                    + "\(scan.candidates.count == 1 ? "candidate" : "candidates"), "
                    + "but none are \(scan.safetyFilter.midSentenceDescription). "
                    + "Change the filter in the view options to see them."
            )
        }
        guard scan.candidates.isEmpty else { return nil }
        if scan.isScanning {
            return .loading("Scanning for reclaimable space…")
        }
        if scan.lastErrorMessage != nil {
            // The failure message below is the whole story; an "all clear"
            // sitting next to it would contradict it.
            return nil
        }
        if scan.lastScannedAt == nil {
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
        ForEach(Array(visibleCandidates.map(CandidateRowViewModel.init).enumerated()), id: \.element.id) { index, row in
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
    ///
    /// HORO-1307: the tier chip, when present, sits on the second line beside
    /// the safety badge rather than next to the size. Two chips crowding the
    /// right edge of the first line would wrap on a long size string, and
    /// putting "Biggest wins" beside the safety verdict is also the honest
    /// arrangement — it reads as one more independent fact about the
    /// candidate, not as a qualifier on the number.
    @ViewBuilder
    private func rowView(_ row: CandidateRowViewModel) -> some View {
        Button {
            onOpenDetail(row.id)
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
                    if let impactTierTerm = row.impactTierTerm {
                        // Filled, so it carries weight without carrying a
                        // hue — its tone is `.neutral` at every tier.
                        GlomerisBadgeView(term: impactTierTerm)
                    }
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
        scan.isScanning = true
        scan.progressStatusText = nil
        scan.lastErrorMessage = nil

        do {
            let result = try await client.run(
                ["detect", "--json", "--progress-json"] + projectRootsStore.commandLineArguments,
                outputType: DetectReportDto.self,
                progressType: ProgressEventDto.self,
                onProgress: { event in
                    Task { @MainActor in
                        scan.progressStatusText = ProgressStatusText.text(for: event)
                    }
                }
            )
            scan.candidates = result.output.candidates
            scan.lastScannedAt = Date()
        } catch {
            scan.lastErrorMessage = SectionFetchErrors.shortMessage(error, subject: "detect")
        }

        scan.isScanning = false
        scan.progressStatusText = nil
    }
}

#Preview {
    CandidatesSectionView(scan: ScanState())
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
