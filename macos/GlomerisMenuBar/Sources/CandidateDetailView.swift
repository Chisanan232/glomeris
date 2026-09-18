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
//  The confirmation prompt is a SwiftUI `.alert`, not a second `.sheet`
//  — this view is itself already presented as a `.sheet` from a
//  `MenuBarExtra(.window)` popover, and a sheet-on-a-sheet inside that
//  host is the riskiest presentation shape available; `.alert` avoids it
//  entirely.
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
    let reclaimableText: String
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
        VStack(alignment: .leading, spacing: 8) {
            Text("Candidate detail")
                .font(.headline)

            if isLoading {
                ProgressView()
            } else if let viewModel {
                detailContent(for: viewModel)

                Button("Clean") {
                    if viewModel.requiresConfirmation {
                        showConfirmationAlert = true
                    } else {
                        Task { await performClean() }
                    }
                }
                .disabled(!viewModel.isCleanEnabled)
                .disabled(isExecuting)

                if isExecuting {
                    HStack(spacing: 6) {
                        ProgressView()
                            .controlSize(.small)
                        if let executeProgressText {
                            Text(executeProgressText)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                if let executeOutcomeText {
                    Text(executeOutcomeText)
                        .font(.caption2)
                        .foregroundStyle(executeSucceeded ? .green : .red)
                }
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
        .alert("Confirm cleanup", isPresented: $showConfirmationAlert) {
            Button("Cancel", role: .cancel) {}
            Button("Confirm") {
                Task { await performClean() }
            }
        } message: {
            Text("This action requires explicit confirmation before it runs.")
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
            executeOutcomeText = "execute could not be started: \(String(describing: error))"
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
            return .succeeded(actualReclaimedBytes: bytes, human: humanByteCount(bytes))
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

/// Human-readable byte count for the measured `actual_reclaimed_bytes`
/// — Rust supplies only the raw byte count for this field (unlike the
/// pre-execute estimate, which already carries a `*_human` string), so
/// this view formats it itself.
func humanByteCount(_ bytes: UInt64?) -> String {
    guard let bytes else { return "unknown" }
    let formatter = ByteCountFormatter()
    formatter.countStyle = .file
    return formatter.string(fromByteCount: Int64(bytes))
}

#Preview {
    CandidateDetailView(resourceId: "/tmp/example/target")
}
