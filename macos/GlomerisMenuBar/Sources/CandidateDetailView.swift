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
//  Button enablement and confirmation-sheet visibility are FIELD READS
//  of `ExplainReportDto.executable` / `OfferedActionDto
//  .requiresConfirmation` — see `CandidateDetailViewModel` below, which
//  is the pure, directly-testable mapping step this view renders from
//  (same pattern as `CandidateRowViewModel` in CandidatesSectionView.swift
//  and `DaemonHealthViewModel` in StatusHealthSectionView.swift). Neither
//  decision ever consults `policyLabel` or `reasons` — those two fields
//  are shown only as human-readable context. See the standing project
//  rule in GlomerisMenuBarApp.swift.
//
//  HORO-1064/HORO-1065 boundary: this view builds the detail UI, the
//  field-driven Clean-button enablement, and the confirmation
//  sheet/flow up through "user confirmed". The actual `glomeris execute`
//  subprocess invocation, its NDJSON streamed progress, and the measured
//  (`actual_reclaimed_bytes`) success display are HORO-1065's scope —
//  `performClean()` below is a deliberate stub with a TODO(HORO-1065)
//  marker, not a real execution path. See the PR description for why
//  this boundary was chosen over a minimal non-streaming `execute` call.
//

import SwiftUI

/// Pure, directly-testable mapping step from one `ExplainReportDto` to
/// the two decisions `CandidateDetailView`'s Clean button and
/// confirmation sheet need. Both are field reads only — see file header.
struct CandidateDetailViewModel: Equatable {
    /// Field read of `executable` — never inferred from `policyLabel`.
    let isCleanEnabled: Bool
    /// Field read of the matching offered action's `requiresConfirmation`
    /// — never inferred from `policyLabel` (e.g. never
    /// `policyLabel == "ASK"`). `false` when there is no offered action
    /// at all (a non-executable resource has none).
    let requiresConfirmation: Bool

    let resourceId: String
    let kind: String
    let detector: String
    let sources: [String]
    let logicalSizeText: String
    let reclaimableText: String
    let completeness: String
    let confidence: String
    let activeUseSignals: [String]
    let regenerability: String
    let policyLabel: String
    let reasons: [String]
    let refusalReason: String?

    init(_ report: ExplainReportDto) {
        isCleanEnabled = report.executable
        requiresConfirmation = report.offeredActions.first?.requiresConfirmation ?? false

        resourceId = report.resourceId
        kind = report.kind
        detector = report.detector
        sources = report.sources
        logicalSizeText = report.logicalHuman ?? "unknown"
        if let human = report.reclaimableHuman {
            reclaimableText = report.reclaimableBytesIsLowerBound ? "\u{2265} \(human)" : human
        } else {
            reclaimableText = "unknown"
        }
        completeness = report.completeness
        confidence = report.confidence
        activeUseSignals = report.activeUseSignals
        regenerability = report.regenerability
        policyLabel = report.policyLabel
        reasons = report.reasons
        refusalReason = report.refusalReason
    }
}

/// Detail view for one candidate, driven entirely by one
/// `glomeris explain <resource_id> --json` call. Opened from
/// `CandidatesSectionView` by tapping a row.
struct CandidateDetailView: View {
    let resourceId: String
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    @State private var viewModel: CandidateDetailViewModel?
    @State private var isLoading = true
    @State private var errorMessage: String?
    @State private var showConfirmationSheet = false

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
        VStack(alignment: .leading, spacing: 8) {
            Text("Candidate detail")
                .font(.headline)

            if isLoading {
                ProgressView()
            } else if let viewModel {
                detailContent(for: viewModel)

                Button("Clean") {
                    if viewModel.requiresConfirmation {
                        showConfirmationSheet = true
                    } else {
                        performClean()
                    }
                }
                .disabled(!viewModel.isCleanEnabled)
            }

            if let errorMessage {
                Text(errorMessage)
                    .font(.caption2)
                    .foregroundStyle(.red)
            }
        }
        .padding()
        .frame(minWidth: 280)
        .task {
            await loadExplain()
        }
        .sheet(isPresented: $showConfirmationSheet) {
            confirmationSheet
        }
    }

    @ViewBuilder
    private func detailContent(for viewModel: CandidateDetailViewModel) -> some View {
        Group {
            labeledRow("Resource", viewModel.resourceId)
            labeledRow("Kind", viewModel.kind)
            labeledRow("Detected by", viewModel.detector)
            if !viewModel.sources.isEmpty {
                labeledRow("Sources", viewModel.sources.joined(separator: ", "))
            }
            labeledRow("Logical size", viewModel.logicalSizeText)
            labeledRow("Reclaimable (estimate)", viewModel.reclaimableText)
            labeledRow("Completeness", viewModel.completeness)
            labeledRow("Confidence", viewModel.confidence)
            if !viewModel.activeUseSignals.isEmpty {
                labeledRow("Active-use signals", viewModel.activeUseSignals.joined(separator: ", "))
            }
            labeledRow("Regenerability", viewModel.regenerability)
            labeledRow("Policy", viewModel.policyLabel)
            if !viewModel.reasons.isEmpty {
                labeledRow("Reasons", viewModel.reasons.joined(separator: ", "))
            }
            if let refusalReason = viewModel.refusalReason {
                labeledRow("Refusal reason", refusalReason)
            }
        }
    }

    private func labeledRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(width: 130, alignment: .leading)
            Text(value)
                .font(.caption)
            Spacer()
        }
    }

    private var confirmationSheet: some View {
        VStack(spacing: 12) {
            Text("Confirm cleanup")
                .font(.headline)
            Text("This action requires explicit confirmation before it runs.")
                .font(.caption)
                .multilineTextAlignment(.center)
            HStack {
                Button("Cancel", role: .cancel) {
                    showConfirmationSheet = false
                }
                Button("Confirm") {
                    showConfirmationSheet = false
                    performClean()
                }
            }
        }
        .padding()
        .frame(minWidth: 240)
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
            errorMessage = "explain: \(String(describing: error))"
        }
        isLoading = false
    }

    /// TODO(HORO-1065): replace this stub with the real
    /// `glomeris execute --action-id <id> --resource-id <resourceId>
    /// [--confirm-ask --observed-fingerprint <fingerprintToken>]
    /// --progress-json` invocation, streamed NDJSON progress, and a
    /// display of the measured `actual_reclaimed_bytes` on success. This
    /// ticket (HORO-1064) intentionally stops here, at "user confirmed
    /// (or confirmation not required) — do nothing destructive yet".
    private func performClean() {}
}

#Preview {
    CandidateDetailView(resourceId: "/tmp/example/target")
}
