//
//  CandidateDetailView.swift
//  GlomerisMenuBar
//
//  HORO-1064: candidate detail view — the SOLE host of the Clean button.
//
//  Why this view only, never an inline per-row button in
//  CandidatesSectionView: `explain --json` is what supplies
//  `fingerprint_token` (HORO-1051), which HORO-1065's real `execute`
//  invocation must pin consent to. An inline per-row button in the
//  candidates list would need a second `explain` call per row and would
//  tempt an implementer into unpinned ASK consent — deliberately not
//  built. See CandidatesSectionViewTests for the mechanical proof that no
//  "Clean"/button exists anywhere in that file's row rendering.
//
//  Button enablement and confirmation-alert visibility are FIELD READS
//  of `ExplainReportDto.executable` / `OfferedActionDto
//  .requiresConfirmation` — see `CandidateDetailViewModel` below, which
//  is the pure, directly-testable mapping step this view renders from
//  (same pattern as `CandidateRowViewModel` in CandidatesSectionView.swift
//  and `DaemonHealthViewModel` in StatusHealthSectionView.swift). Neither
//  decision ever consults the policy-label field or the reasons field —
//  those two are shown only as human-readable context. See the standing
//  project rule in GlomerisMenuBarApp.swift.
//
//  The confirmation prompt is a SwiftUI `.alert`, and deliberately not a
//  `.sheet`. HORO-1357 is the reason that distinction is now load-bearing
//  rather than stylistic: this view used to be presented AS a sheet from
//  the `MenuBarExtra(.window)` popover, and that never reliably appeared,
//  because presenting or resizing a sheet over a non-activating panel can
//  order the panel out. The fix was to stop presenting it — the popover
//  shell swaps this view in for its own body (see GlomerisPopoverView) —
//  so this view is now a plain child of the panel, not a modal over it.
//  An `.alert` is the one prompt shape that host handles, and it stays.
//
//  HORO-1064/HORO-1065 boundary: this view builds the detail UI, the
//  field-driven Clean-button enablement, and the confirmation
//  alert/flow up through "user confirmed". HORO-1065 adds the real
//  `glomeris execute` subprocess invocation from that point on: it
//  streams `--progress-json` NDJSON via `GlomerisClient`'s existing
//  `onProgress` mechanism (HORO-1063, unchanged here), and renders
//  whatever `ExecuteReportDto`/`ExecuteRefusalReportDto` JSON comes back
//  on stdout as a specific, outcome-unique message — see
//  `describeExecuteOutcome` below.
//
//  Fingerprint pass-through (the security-sensitive part of this
//  ticket): `--observed-fingerprint <token>` is passed ONLY the
//  `fingerprintToken` this view already captured from the ONE `explain
//  --json` call `loadExplain()` makes when the view first appears —
//  carried in `CandidateDetailViewModel.fingerprintToken` below. There is
//  no second call to `explain` anywhere in this file, and no other
//  source for that token; `performClean()` reads
//  `viewModel.fingerprintToken` directly, never a freshly-fetched value.
//  See `CandidateDetailViewTests` for a mechanical proof (a fixture whose
//  stored token differs from a hypothetical fresh one) that the stored
//  value, not a refetched one, is what ends up in the constructed
//  argument array.
//
//  Per the project's standing thin-client rule (GlomerisMenuBarApp.swift):
//  this file constructs the `execute` command line, streams its
//  progress, and renders its JSON response — it never decides whether an
//  action is authorized. Every message this view shows for a
//  refusal/abort/failure is built directly from the `message`/
//  `failure_message`/`abort_reason` text the Rust binary itself already
//  decided and returned; this layer adds no new policy wording of its
//  own.
//

import Foundation
import SwiftUI

/// Pure, directly-testable mapping step from one `ExplainReportDto` to
/// the two decisions `CandidateDetailView`'s Clean button and
/// confirmation alert need. Both are field reads only — see file header.
struct CandidateDetailViewModel: Equatable {
    /// Field read of `executable` — never inferred from the policy-label
    /// field.
    let isCleanEnabled: Bool
    /// Field read of the matching offered action's `requiresConfirmation`
    /// — never inferred from the policy-label field's text (e.g. never a
    /// comparison against a literal like "ASK"). `false` when there is no
    /// offered action at all (a non-executable resource has none).
    let requiresConfirmation: Bool

    let resourceId: String
    let kind: String
    let detector: String
    let sources: [String]
    let logicalSizeText: String
    /// The reclaimable estimate's own wording, including the leading `≥`
    /// when the measurement was partial. Derived from `impactTerm` so the
    /// row in the candidates list and this sheet cannot word the same
    /// number two different ways.
    var reclaimableText: String { impactTerm.title }
    let reclaimableHuman: String?
    let reclaimableIsLowerBound: Bool
    let completeness: String
    let confidence: String
    let activeUseSignals: [String]
    let regenerability: String
    let policyLabel: String
    let reasons: [String]
    let refusalReason: String?
    /// The matching offered action's `action_id`, captured here so
    /// `performClean()` never has to re-derive or guess it. `nil` for a
    /// non-executable resource (no offered action exists).
    let actionId: String?
    /// Opaque fingerprint-pinning token, captured verbatim from the SAME
    /// `explain --json` call that produced this view model — never
    /// refetched. See file header. `nil` when `explain` didn't return one
    /// (e.g. the resource isn't `ASK`-classified).
    let fingerprintToken: String?

    // MARK: - HORO-1306: plain-language wording for the raw tokens above
    //
    // Every one of these is `GlomerisVocabulary` handed a token this view
    // model already received from `explain --json`. They add no judgment:
    // the raw tokens stay stored, unchanged, right above, and are still
    // rendered verbatim in the sheet's "Raw CLI values" section so anyone
    // comparing the popover against `--json` output can see exactly what
    // the CLI said.

    var kindTerm: GlomerisTerm { GlomerisVocabulary.kind(kind) }

    /// Display copy for the safety verdict Rust already reached. Not a
    /// verdict: the Clean button's enablement reads `isCleanEnabled` (a
    /// field read of `executable`) and nothing here.
    var safetyTerm: GlomerisTerm { GlomerisVocabulary.safety(policyLabel) }

    var impactTerm: GlomerisTerm {
        GlomerisVocabulary.storageImpact(
            human: reclaimableHuman,
            isLowerBound: reclaimableIsLowerBound
        )
    }

    var completenessTerm: GlomerisTerm { GlomerisVocabulary.completeness(completeness) }
    var confidenceTerm: GlomerisTerm { GlomerisVocabulary.confidence(confidence) }
    var regenerabilityTerm: GlomerisTerm { GlomerisVocabulary.regenerability(regenerability) }

    /// The policy reason codes, in plain language. Rendered as prose rather
    /// than chips — they explain the verdict, they are not a fourth axis to
    /// scan.
    var reasonTerms: [GlomerisTerm] { reasons.map(GlomerisVocabulary.reason) }

    // MARK: - HORO-1323: what the Clean button says about itself
    //
    // Everything below is wording. None of it gates anything: the button's
    // two `.disabled` modifiers still read `isCleanEnabled` and the view's
    // `isExecuting` directly, and `CandidateActionability` deliberately
    // exposes no boolean for one of them to reach for. See that file's
    // header.
    //
    // These live on the view model rather than inside the view because the
    // ticket's ACs are about the strings, and a string computed in a
    // `@ViewBuilder` can only be checked by rendering SwiftUI or by grepping
    // source. Here they are ordinary values a test can read.

    /// What Glomeris will do about this resource, in words, from the
    /// actionability fields alone.
    var actionability: CandidateActionability {
        CandidateActionability(
            executable: isCleanEnabled,
            requiresConfirmation: requiresConfirmation,
            refusalReason: refusalReason
        )
    }

    /// What the Clean button acts on, for `accessibilityValue`. The visible
    /// label is the single word "Clean", which out of context names no
    /// resource at all — this is the part that ties it to one.
    var cleanTargetDescription: String {
        "\(kindTerm.title), \(reclaimableText)"
    }

    /// What pressing it will do. Says "permanently" and "cannot be undone"
    /// because irreversibility is the one property of this control a reader
    /// cannot infer from anything else on the screen, and the reclaimable
    /// amount because that is the scale of it.
    var cleanActionDescription: String {
        "Permanently deletes \(resourceId). Cleaning would free \(reclaimableText). "
            + "This cannot be undone."
    }

    /// Why the button is unavailable, or `nil` when it is not.
    ///
    /// The two causes are genuinely different situations and this is the
    /// method that keeps them apart. A resource the CLI will not act on stays
    /// that way; a run already in flight clears in seconds. Before this
    /// ticket both collapsed into one dimmed control with nothing to
    /// distinguish them.
    ///
    /// They cannot in fact co-occur — `performClean` needs an `actionId` to
    /// start, and a non-executable resource has none, so `isExecuting` can
    /// never be `true` while `isCleanEnabled` is `false` — but the refusal is
    /// checked first regardless, because it is the permanent one and nothing
    /// here should depend on that reasoning staying true.
    func cleanUnavailableTerm(isExecuting: Bool) -> GlomerisTerm? {
        if let refusalTerm = actionability.term {
            return refusalTerm
        }
        // `busy` is a refusal reason the CLI emits in its own right, so the
        // wording for "something else is running" already exists in
        // GlomerisVocabulary and is not invented here.
        return isExecuting ? GlomerisVocabulary.refusal("busy") : nil
    }

    /// The Clean button's accessibility hint, and its tooltip — one string
    /// for both, so the spoken and the hovered explanation cannot drift.
    ///
    /// When the button is unavailable the hint becomes the reason, because a
    /// hint describing a deletion that cannot happen is worse than no hint:
    /// the question a reader has at that moment is why, and this ticket was
    /// filed because nothing answered it.
    func cleanButtonHint(isExecuting: Bool) -> String {
        if let unavailable = cleanUnavailableTerm(isExecuting: isExecuting) {
            return "Unavailable. \(unavailable.title). \(unavailable.explanation)"
        }
        if requiresConfirmation {
            return "\(actionability.sentence) \(cleanActionDescription)"
        }
        return cleanActionDescription
    }

    init(_ report: ExplainReportDto) {
        isCleanEnabled = report.executable
        requiresConfirmation = report.offeredActions.first?.requiresConfirmation ?? false
        actionId = report.offeredActions.first?.actionId
        fingerprintToken = report.fingerprintToken

        resourceId = report.resourceId
        kind = report.kind
        detector = report.detector
        sources = report.sources
        logicalSizeText = report.logicalHuman ?? "unknown"
        reclaimableHuman = report.reclaimableHuman
        reclaimableIsLowerBound = report.reclaimableBytesIsLowerBound
        completeness = report.completeness
        confidence = report.confidence
        activeUseSignals = report.activeUseSignals
        regenerability = report.regenerability
        policyLabel = report.policyLabel
        reasons = report.reasons
        refusalReason = report.refusalReason
    }
}

/// Which candidate the popover is currently showing in detail, if any
/// (HORO-1357).
///
/// A type rather than a bare `String?` for two reasons. It gives the drill-down
/// one owner — the popover shell — where before each section presented its own
/// sheet, so two entry points into the same detail view could disagree about
/// how it appears. And it makes the navigation this ticket is about directly
/// assertable: opening a candidate, going back, going back again, and opening a
/// second candidate while one is already open are all plain function calls with
/// no SwiftUI rendering in the way. The bug being fixed was a presentation
/// mechanism that could not be tested at all.
struct CandidateDetailNavigation: Equatable {
    /// The resource whose detail is on screen, or `nil` for the candidate
    /// overview. `private(set)` so the only ways to change it are the two
    /// intention-named methods below, rather than an assignment at some call
    /// site that means something else by it.
    private(set) var resourceId: String?

    /// Whether the detail is on screen. Read by the popover shell to hide the
    /// overview it is layered over — three modifiers there ask the same
    /// question, and `resourceId != nil` repeated three times says less.
    var isShowingDetail: Bool { resourceId != nil }

    /// Opening a candidate while another is already open replaces it, rather
    /// than stacking. There is no navigation stack here on purpose: the panel
    /// is one level deep, and "back" therefore always means the overview.
    mutating func open(_ resourceId: String) {
        self.resourceId = resourceId
    }

    /// Idempotent. The back control is only rendered while a detail is open,
    /// so today it cannot be invoked twice — but nothing about this type
    /// should depend on that staying true.
    mutating func back() {
        resourceId = nil
    }
}

/// Detail view for one candidate, driven entirely by one
/// `glomeris explain <resource_id> --json` call. Reached by tapping a row in
/// either `CandidatesSectionView` or `AiPlanSectionView`; both report the
/// tapped resource id up to `GlomerisPopoverView`, which shows this view in
/// place of its own body rather than presenting it (HORO-1357).
struct CandidateDetailView: View {
    let resourceId: String
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    @State private var viewModel: CandidateDetailViewModel?
    @State private var isLoading = true
    @State private var errorMessage: String?
    @State private var showConfirmationAlert = false
    /// HORO-1065: `execute` state. `isExecuting`/`executeProgressText`
    /// mirror `CandidatesSectionView`'s `isScanning`/`progressStatusText`
    /// pattern for `detect`. `executeOutcomeText`/`executeSucceeded`
    /// together carry the single rendered outcome message — see
    /// `describeExecuteOutcome` below for how every distinct outcome
    /// produces a distinct string.
    @State private var isExecuting = false
    @State private var executeProgressText: String?
    @State private var executeOutcomeText: String?
    @State private var executeSucceeded = false

    init(
        resourceId: String,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.resourceId = resourceId
        self.client = client
        self.projectRootsStore = projectRootsStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
            header

            if isLoading {
                GlomerisStateMessageView(
                    message: .loading("Gathering the evidence for this resource…")
                )
            } else if let viewModel {
                ScrollView {
                    VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                        safetyCard(viewModel)
                        impactCard(viewModel)
                        evidenceCard(viewModel)
                        rawValuesCard(viewModel)
                    }
                }
                .frame(maxHeight: GlomerisDesign.maxBodyHeight)

                Button("Clean") {
                    if viewModel.requiresConfirmation {
                        showConfirmationAlert = true
                    } else {
                        Task { await performClean() }
                    }
                }
                .disabled(!viewModel.isCleanEnabled)
                .disabled(isExecuting)
                // HORO-1323. The two enablement modifiers above are unchanged
                // field reads; these three are wording for what they did.
                // `accessibilityHint` and `help` are handed the same string on
                // purpose — both land in AXHelp, so two different strings
                // would mean the spoken and hovered explanations depended on
                // modifier order.
                .accessibilityValue(viewModel.cleanTargetDescription)
                .accessibilityHint(viewModel.cleanButtonHint(isExecuting: isExecuting))
                .help(viewModel.cleanButtonHint(isExecuting: isExecuting))

                if let unavailable = viewModel.cleanUnavailableTerm(isExecuting: isExecuting) {
                    cleanUnavailableLine(unavailable)
                }

                if isExecuting {
                    HStack(spacing: GlomerisDesign.inlineSpacing) {
                        ProgressView()
                            .controlSize(.small)
                        if let executeProgressText {
                            Text(executeProgressText)
                                .font(GlomerisDesign.captionFont)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                if let executeOutcomeText {
                    // The outcome text itself comes from Rust (see
                    // `describeExecuteOutcome`); only its presentation is
                    // chosen here, and it always carries a glyph and a
                    // sentence, never a bare colour.
                    GlomerisStateMessageView(
                        message: executeSucceeded
                            ? .success(executeOutcomeText)
                            : .failure(executeOutcomeText)
                    )
                }
            }

            if let errorMessage {
                GlomerisStateMessageView(message: .failure(errorMessage))
            }
        }
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
        .task {
            await loadExplain()
        }
        .alert("Confirm cleanup", isPresented: $showConfirmationAlert) {
            Button("Cancel", role: .cancel) {}
            Button("Confirm") {
                Task { await performClean() }
            }
        } message: {
            Text("This action requires explicit confirmation before it runs.")
        }
    }

    // MARK: - Header
    //
    // The sheet used to open on "Candidate detail" — a title that says
    // nothing about which candidate, followed by a `Resource` row buried
    // eleven key/value rows down. It now leads with what the thing IS and
    // where it lives, because those are the two questions someone opening
    // this sheet already has.

    @ViewBuilder
    private var header: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(viewModel?.kindTerm.title ?? "Candidate detail")
                .font(GlomerisDesign.titleFont)
            GlomerisPathText(path: viewModel?.resourceId ?? resourceId)
        }
    }

    // MARK: - Cards

    /// Safety comes first, because it is what decides whether anything else
    /// on this sheet matters. A refusal is rendered as a plain explanation
    /// rather than an error: PROTECTED is Glomeris working correctly, and
    /// the user has nothing to fix.
    @ViewBuilder
    private func safetyCard(_ viewModel: CandidateDetailViewModel) -> some View {
        GlomerisCard(title: GlomerisVocabulary.safetyAxis) {
            GlomerisBadgeView(term: viewModel.safetyTerm)
            Text(viewModel.safetyTerm.explanation)
                .font(GlomerisDesign.secondaryFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if !viewModel.reasonTerms.isEmpty {
                Divider()
                ForEach(Array(viewModel.reasonTerms.enumerated()), id: \.offset) { _, term in
                    reasonLine(term)
                }
            }

            if let refusalReason = viewModel.refusalReason {
                Divider()
                // Verbatim from Rust — this layer never rewords a refusal.
                GlomerisDetailRow(label: "Why not") {
                    Text(refusalReason)
                        .font(GlomerisDesign.captionFont)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// Why the Clean button is unavailable, shown immediately beneath it
    /// (HORO-1323).
    ///
    /// Deliberately not accessible-only. A disabled control communicates
    /// nothing on its own, which is this ticket's premise, and the fix has to
    /// work for the reader who can see the dimmed button perfectly well and
    /// still cannot tell why.
    ///
    /// For a refusal this repeats, word for word, the "Why not" row inside the
    /// safety card above — and that repetition is the point rather than an
    /// oversight. The card sits inside a bounded `ScrollView` and the button
    /// sits below it, so the reason can easily be scrolled out of sight at the
    /// exact moment it is needed. Repeating a refusal at the point of action
    /// is the safe direction to err in; the alternative is a reader
    /// concluding, as the founder pass did, that the product simply will not
    /// do what they asked.
    @ViewBuilder
    private func cleanUnavailableLine(_ term: GlomerisTerm) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
            if let symbolName = term.symbolName {
                Image(systemName: symbolName)
                    .imageScale(.small)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(term.title)
                    .font(GlomerisDesign.secondaryFont)
                    .fixedSize(horizontal: false, vertical: true)
                Text(term.explanation)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        // One element, so it is not read as a glyph followed by two unrelated
        // fragments. The label is the badge's own, which leads with the axis.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(term.accessibilityLabel)
    }

    /// One policy reason: the plain sentence, with the raw code beneath it
    /// in a monospaced caption so it stays greppable against `--json`.
    @ViewBuilder
    private func reasonLine(_ term: GlomerisTerm) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(term.title)
                .font(GlomerisDesign.secondaryFont)
                .fixedSize(horizontal: false, vertical: true)
            Text(term.explanation)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// Two sizes that are easy to confuse, so they are labelled by what
    /// they mean rather than by the field name: what cleaning would free
    /// versus how big the thing is on disk.
    @ViewBuilder
    private func impactCard(_ viewModel: CandidateDetailViewModel) -> some View {
        GlomerisCard(title: GlomerisVocabulary.impactAxis) {
            GlomerisDetailRow(label: "Cleaning would free") {
                GlomerisBadgeView(term: viewModel.impactTerm)
            }
            GlomerisDetailRow(label: "Total size on disk") {
                Text(viewModel.logicalSizeText)
                    .font(GlomerisDesign.secondaryFont)
            }
            Text(viewModel.impactTerm.explanation)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// How well Glomeris actually knows what it just claimed — the third
    /// axis, kept in its own card so it is never read as part of the safety
    /// verdict. High confidence in a PROTECTED classification does not make
    /// anything cleanable, and low confidence does not make it dangerous.
    @ViewBuilder
    private func evidenceCard(_ viewModel: CandidateDetailViewModel) -> some View {
        GlomerisCard(title: GlomerisVocabulary.completenessAxis) {
            GlomerisDetailRow(label: "Measurement") {
                GlomerisBadgeView(term: viewModel.completenessTerm)
            }
            GlomerisDetailRow(label: GlomerisVocabulary.confidenceAxis) {
                GlomerisBadgeView(term: viewModel.confidenceTerm)
            }
            GlomerisDetailRow(label: GlomerisVocabulary.regenerabilityAxis) {
                GlomerisBadgeView(term: viewModel.regenerabilityTerm)
            }
            GlomerisDetailRow(label: "Found by") {
                Text(viewModel.detector)
                    .font(GlomerisDesign.secondaryFont)
            }
            if !viewModel.sources.isEmpty {
                GlomerisDetailRow(label: "Evidence sources") {
                    Text(viewModel.sources.joined(separator: ", "))
                        .font(GlomerisDesign.captionFont)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if !viewModel.activeUseSignals.isEmpty {
                GlomerisDetailRow(label: "Signs of active use") {
                    Text(viewModel.activeUseSignals.joined(separator: ", "))
                        .font(GlomerisDesign.captionFont)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// The tokens exactly as `explain --json` emitted them.
    ///
    /// Collapsed by default, and deliberately kept rather than dropped:
    /// plain language is for reading, but anyone filing a bug, comparing
    /// the popover against the CLI, or writing a Jira comment needs the
    /// verbatim value. Hiding it would make the humanised copy the only
    /// account of what the CLI said, which is worse than an enum tag.
    @ViewBuilder
    private func rawValuesCard(_ viewModel: CandidateDetailViewModel) -> some View {
        GlomerisCard(title: "Raw CLI values") {
            DisclosureGroup("Show what `explain --json` returned") {
                VStack(alignment: .leading, spacing: 2) {
                    rawValue("resource_id", viewModel.resourceId)
                    rawValue("kind", viewModel.kind)
                    rawValue("policy_label", viewModel.policyLabel)
                    rawValue("reasons", viewModel.reasons.joined(separator: ", "))
                    rawValue("completeness", viewModel.completeness)
                    rawValue("confidence", viewModel.confidence)
                    rawValue("regenerability", viewModel.regenerability)
                }
                .padding(.top, 2)
            }
            .font(GlomerisDesign.captionFont)
        }
    }

    @ViewBuilder
    private func rawValue(_ field: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
            Text(field)
                .font(GlomerisDesign.monospacedFont)
                .foregroundStyle(.secondary)
            Text(value.isEmpty ? "—" : value)
                .font(GlomerisDesign.monospacedFont)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    /// The one and only call site for `explain`, run once when the view
    /// appears.
    private func loadExplain() async {
        isLoading = true
        errorMessage = nil
        do {
            let result = try await client.run(
                ["explain", resourceId, "--json"] + projectRootsStore.commandLineArguments,
                outputType: ExplainReportDto.self,
                progressType: ProgressEventDto.self
            )
            viewModel = CandidateDetailViewModel(result.output)
        } catch {
            errorMessage = SectionFetchErrors.shortMessage(error, subject: "explain")
        }
        isLoading = false
    }

    /// HORO-1065: the one and only call site for `execute`, run after the
    /// user confirmed (or confirmation wasn't required). Constructs the
    /// command from `viewModel`'s already-captured fields only — see
    /// `buildExecuteArguments` and the file header's fingerprint
    /// pass-through note.
    private func performClean() async {
        guard let viewModel, let actionId = viewModel.actionId else { return }

        isExecuting = true
        executeProgressText = nil
        executeOutcomeText = nil
        executeSucceeded = false

        let arguments = buildExecuteArguments(
            actionId: actionId,
            resourceId: resourceId,
            projectRootsArguments: projectRootsStore.commandLineArguments,
            requiresConfirmation: viewModel.requiresConfirmation,
            fingerprintToken: viewModel.fingerprintToken
        )

        do {
            let raw = try await client.runRaw(
                arguments,
                progressType: ProgressEventDto.self,
                onProgress: { event in
                    Task { @MainActor in
                        executeProgressText = ProgressStatusText.text(for: event)
                    }
                }
            )
            let outcome = describeExecuteOutcome(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderrText: String(data: raw.stderr, encoding: .utf8) ?? ""
            )
            switch outcome {
            case .succeeded(_, let human):
                executeSucceeded = true
                executeOutcomeText = "Cleaned — reclaimed \(human)."
            case .message(let text):
                executeSucceeded = false
                executeOutcomeText = text
            }
        } catch {
            executeSucceeded = false
            executeOutcomeText = SectionFetchErrors.shortMessage(
                error,
                subject: "execute could not be started"
            )
        }

        executeProgressText = nil
        isExecuting = false
    }
}

/// Pure builder for the `glomeris execute` argument array (HORO-1065).
/// `fingerprintToken` must always be the value already captured from the
/// view's one `explain --json` call — see `CandidateDetailView`'s file
/// header — never a value obtained by calling `explain` again here or
/// anywhere else. `--confirm-ask`/`--observed-fingerprint` are added
/// together, only when `requiresConfirmation` is `true` and a token is
/// present, matching `execute`'s own requirement that the two flags are
/// passed together or not at all (`book/src/cli_reference.md`).
func buildExecuteArguments(
    actionId: String,
    resourceId: String,
    projectRootsArguments: [String],
    requiresConfirmation: Bool,
    fingerprintToken: String?
) -> [String] {
    var arguments = ["execute", "--action-id", actionId, "--resource-id", resourceId]
    arguments += projectRootsArguments
    arguments += ["--json", "--progress-json"]
    if requiresConfirmation, let fingerprintToken {
        arguments += ["--confirm-ask", "--observed-fingerprint", fingerprintToken]
    }
    return arguments
}

/// One `glomeris execute` invocation's outcome, mapped from its raw exit
/// code and stdout bytes to what the view shows.
enum ExecuteOutcomeDisplay: Equatable {
    /// `human` is built from the real, measured `actual_reclaimed_bytes`
    /// — never from the pre-execute `reclaimable_bytes` estimate, which
    /// this type never even carries.
    case succeeded(actualReclaimedBytes: UInt64?, human: String)
    case message(String)
}

/// Maps one `execute --json --progress-json` invocation's raw exit code
/// and stdout bytes to a single outcome message (HORO-1065's core AC:
/// every distinct outcome/refusal renders its own specific message,
/// never a generic error). This performs no authorization/policy
/// judgment — every refusal/failure/abort string here is read directly
/// from a field (`message`/`failure_message`/`abort_reason`) the Rust
/// binary already decided; this function only assembles display text
/// around values Rust already produced, per the project's thin-client
/// rule.
func describeExecuteOutcome(exitCode: Int32, stdout: Data, stderrText: String) -> ExecuteOutcomeDisplay {
    let decoder = JSONDecoder()

    if let report = try? decoder.decode(ExecuteReportDto.self, from: stdout) {
        switch report.outcome {
        case "succeeded":
            let bytes = report.actualReclaimedBytes
            return .succeeded(
                actualReclaimedBytes: bytes,
                human: humanByteCount(bytes, rendered: report.actualReclaimedHuman)
            )
        case "failed":
            return .message("Execution failed: \(report.failureMessage ?? "no failure detail was reported")")
        case "aborted_by_revalidation":
            return .message(
                "Aborted before making any changes (revalidation): " +
                    "\(report.abortReason ?? "no abort reason was reported")"
            )
        case "dry_run":
            return .message("Dry run only — nothing was changed.")
        default:
            return .message("execute finished with an unrecognized outcome: \(report.outcome)")
        }
    }

    if let refusal = try? decoder.decode(ExecuteRefusalReportDto.self, from: stdout) {
        return .message(refusal.message)
    }

    // No decodable JSON body on stdout at all — the usage-error exit
    // code (2: bad/missing argument, or a malformed
    // --observed-fingerprint token), or a genuinely unexpected failure.
    // Never crash on unparseable output; fall back to a message built
    // from the exit code and whatever stderr text is available.
    let detail = stderrText.trimmingCharacters(in: .whitespacesAndNewlines)
    return .message("execute did not complete (exit \(exitCode))" + (detail.isEmpty ? "." : ": \(detail)"))
}

/// Human-readable byte count for the measured `actual_reclaimed_bytes`,
/// preferring the string Rust rendered (HORO-1312).
///
/// This used to call `ByteCountFormatter` with `countStyle = .file`, which is
/// 1000-based. Every other number in this panel comes from Rust's
/// `human_bytes`, which is 1024-based with the same KB/MB/GB labels — so a
/// 2 GiB cleanup rendered as an estimate of "2.0 GB" and a result of "2.15 GB",
/// and the honest reading of that is that 150 MB went missing.
///
/// The fallback for an older binary on `PATH` that emits no `*_human` key is
/// the raw count with an explicit unit, deliberately: it looks unformatted,
/// which is a truthful signal, where a locally-scaled "2.15 GB" would look
/// finished and be wrong. This app does not reimplement the convention —
/// whoever owns it emits the string.
func humanByteCount(_ bytes: UInt64?, rendered: String?) -> String {
    if let rendered, !rendered.isEmpty { return rendered }
    guard let bytes else { return "unknown" }
    return "\(bytes) bytes"
}

#Preview {
    CandidateDetailView(resourceId: "/tmp/example/target")
}
