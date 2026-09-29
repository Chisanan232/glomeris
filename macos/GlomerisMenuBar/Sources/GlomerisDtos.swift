//
//  GlomerisDtos.swift
//  GlomerisMenuBar
//
//  HORO-1061: hand-written Swift `Codable` mirrors of the Rust
//  `reporting::dto` report types (`src/reporting/dto.rs` at the repo
//  root), covering exactly the DTOs backed by a golden fixture under
//  `tests/fixtures/dto/` — see that directory and
//  `tests/dto_golden_fixtures.rs` for the Rust half of this contract and
//  the scoping rationale.
//
//  These are pure data mirrors — decoding only, no policy/evidence/action
//  logic (see the standing project rule in GlomerisMenuBarApp.swift).
//  Field names and JSON key spelling must match the Rust `Serialize`
//  output exactly. The Rust side carries no `#[serde(rename...)]`, so its
//  keys are plain snake_case; a mirror here therefore either spells its
//  properties in snake_case or maps them in `CodingKeys`. The later DTOs do
//  the latter, and a mirror that does neither still compiles — a key that
//  never matches decodes an optional as `nil` and fails a required field only
//  at run time. That is what the golden-fixture tests are for.
//

import Foundation

/// Mirrors `reporting::dto::StatusReport`.
struct StatusReportDto: Decodable, Equatable {
    let totalBytes: UInt64
    let freeBytes: UInt64
    let usedPercent: Double
    let freeHuman: String
    let totalHuman: String
    let pressureState: String

    enum CodingKeys: String, CodingKey {
        case totalBytes = "total_bytes"
        case freeBytes = "free_bytes"
        case usedPercent = "used_percent"
        case freeHuman = "free_human"
        case totalHuman = "total_human"
        case pressureState = "pressure_state"
    }
}

/// Mirrors `reporting::dto::DaemonStatusReport`.
struct DaemonStatusReportDto: Decodable, Equatable {
    let plistInstalled: Bool
    let plistPath: String
    let loaded: Bool
    let heartbeatAgeSecs: UInt64?

    enum CodingKeys: String, CodingKey {
        case plistInstalled = "plist_installed"
        case plistPath = "plist_path"
        case loaded
        case heartbeatAgeSecs = "heartbeat_age_secs"
    }
}

/// Mirrors `reporting::dto::HistoryEventReport`.
struct HistoryEventReportDto: Decodable, Equatable {
    let unixTimeSecs: UInt64
    let from: String
    let to: String
    let usedPercent: Double
    let freeBytes: UInt64
    let freeHuman: String

    enum CodingKeys: String, CodingKey {
        case unixTimeSecs = "unix_time_secs"
        case from
        case to
        case usedPercent = "used_percent"
        case freeBytes = "free_bytes"
        case freeHuman = "free_human"
    }
}

/// Mirrors `reporting::dto::HistoryReport`.
struct HistoryReportDto: Decodable, Equatable {
    let events: [HistoryEventReportDto]
}

/// Mirrors `reporting::dto::ActionHistoryEventReport`.
struct ActionHistoryEventReportDto: Decodable, Equatable {
    let timestamp: UInt64
    let actionId: String
    let resourceId: String
    let policyLabel: String
    let outcome: String
    let abortReason: String?
    let actualReclaimedBytes: UInt64?
    let actualReclaimedHuman: String?
    let source: String

    enum CodingKeys: String, CodingKey {
        case timestamp
        case actionId = "action_id"
        case resourceId = "resource_id"
        case policyLabel = "policy_label"
        case outcome
        case abortReason = "abort_reason"
        case actualReclaimedBytes = "actual_reclaimed_bytes"
        case actualReclaimedHuman = "actual_reclaimed_human"
        case source
    }
}

/// Mirrors `reporting::dto::ActionHistoryReport`.
struct ActionHistoryReportDto: Decodable, Equatable {
    let events: [ActionHistoryEventReportDto]
}

/// Mirrors `reporting::dto::OfferedAction`.
struct OfferedActionDto: Decodable, Equatable {
    let actionId: String
    let requiresConfirmation: Bool

    enum CodingKeys: String, CodingKey {
        case actionId = "action_id"
        case requiresConfirmation = "requires_confirmation"
    }
}

/// Mirrors `reporting::dto::DetectCandidateReport`.
struct DetectCandidateReportDto: Decodable, Equatable, Identifiable {
    let resourceId: String
    let kind: String
    let reclaimableBytes: UInt64?
    let reclaimableHuman: String?
    let reclaimableBytesIsLowerBound: Bool
    /// HORO-1307: the tier Rust already decided for this candidate's size
    /// (`"unknown"`/`"normal"`/`"notable"`/`"large"`).
    ///
    /// Optional purely for forward/backward tolerance: the app resolves
    /// whichever `glomeris` is on `PATH`, which may predate this field. A
    /// non-optional `String` would fail the whole `DetectReportDto` decode
    /// and blank the candidates list entirely — a missing emphasis hint is
    /// not worth losing the list over. `GlomerisVocabulary.impactTier`
    /// treats `nil` as "nothing to call out", which is exactly right.
    ///
    /// Never branched on to decide whether an action is permitted. It is an
    /// emphasis hint about magnitude; `executable`/`offeredActions` remain
    /// the only authority on what may be done.
    let impactTier: String?
    let policyLabel: String
    let reasons: [String]
    let executable: Bool
    let offeredActions: [OfferedActionDto]
    let refusalReason: String?

    enum CodingKeys: String, CodingKey {
        case resourceId = "resource_id"
        case kind
        case reclaimableBytes = "reclaimable_bytes"
        case reclaimableHuman = "reclaimable_human"
        case reclaimableBytesIsLowerBound = "reclaimable_bytes_is_lower_bound"
        case impactTier = "impact_tier"
        case policyLabel = "policy_label"
        case reasons
        case executable
        case offeredActions = "offered_actions"
        case refusalReason = "refusal_reason"
    }

    /// `Identifiable` conformance for `CandidatesSectionView`'s
    /// `.sheet(item:)` (HORO-1064) — `resourceId` is already the stable,
    /// unique identity Rust assigns each candidate.
    var id: String { resourceId }
}

/// Mirrors `reporting::dto::DetectorHealthReport` (HORO-1484) — one
/// detector's outcome from the discovery pass that produced this report.
///
/// `status` is one of `"found"`, `"tool_absent"`, `"tool_not_running"`,
/// `"failed"`, produced by `cli::DetectorOutcome::tag()`. The four are NOT
/// interchangeable and the app must not collapse them: `tool_absent` means
/// the tool that would produce candidates is not installed, which is normal
/// state and a real answer; `tool_not_running` means it is installed but was
/// not answering, so its resources are presumably still there and this pass
/// could not see them (HORO-1544); `failed` means the probe did not answer,
/// so whatever that detector would have found is unknown. `reason` is
/// present only for `failed`.
struct DetectorHealthReportDto: Decodable, Equatable, Identifiable {
    let detector: String
    let status: String
    let candidatesFound: UInt64
    let reason: String?

    enum CodingKeys: String, CodingKey {
        case detector
        case status
        case candidatesFound = "candidates_found"
        case reason
    }

    var id: String { detector }

    /// Did this detector's probe fail? The one branch the UI is allowed to
    /// make on `status`, kept here so no view re-spells the tag.
    var didFail: Bool { status == "failed" }
}

/// Mirrors `reporting::dto::WorkspaceWorktreeReport` (HORO-1511) — one git
/// working tree inside a family, and what is outstanding in it.
///
/// ## Nothing here is permission
///
/// There is deliberately no `executable` field, no action id and no offered
/// action. A surface that wants to know what may be done with this
/// worktree's resources looks `memberResourceIds` up in
/// `DetectReportDto.candidates` and reads the candidate's own `executable`,
/// `offeredActions` and `refusalReason` — the same single enablement path
/// `LlmPlanItemReportDto.candidate` documents, for the same reason. "That
/// branch is already merged" and "nothing has touched this in four months"
/// are the most persuasive things this app can say about a directory, and
/// persuasive is not the same as authorized.
struct WorkspaceWorktreeReportDto: Decodable, Equatable, Identifiable {
    let root: String
    /// `true` for a `git worktree add` sibling, `false` for the
    /// repository's main checkout.
    let linkedWorktree: Bool
    let dirty: Bool
    let untracked: Bool
    /// `"in_use"`, `"idle"` or `"unknown"`. `"unknown"` is NOT `"idle"`: a
    /// correlation probe that could not answer lands here, and
    /// `holdsWorkInProgress` counts it as possible use.
    let activity: String
    /// `"untracked"`, `"tracking"` or `"unknown"`. `"untracked"` means there
    /// is no published counterpart to compare against — not that nothing is
    /// unpushed.
    let upstream: String
    /// Both `nil` unless `upstream` is `"tracking"`.
    let ahead: UInt32?
    let behind: UInt32?
    /// `nil` for a detached HEAD, and also for a branch state no probe could
    /// read — `upstream` and `merged` are `"unknown"` in that case, which is
    /// what tells the two apart.
    let branch: String?
    /// `"merged"`, `"not_merged"` or `"unknown"`. `"unknown"` is what a
    /// repository with no recorded default branch gets: Rust never guesses
    /// at `main`, so neither may a surface.
    let merged: String
    let mergedInto: String?
    /// `"equivalent"`, `"not_equivalent"`, `"not_applicable"` or
    /// `"unknown"` — whether the work here has landed on `mergedInto` by some
    /// route other than ancestry: a squash, a rebase, a cherry-pick
    /// (HORO-1545).
    ///
    /// `"not_applicable"` means the branch is merged outright so there was no
    /// separate question to ask. It is not a quieter `"not_equivalent"`, and
    /// `GlomerisVocabulary.worktreeEquivalence` keeps the two apart.
    let patchEquivalence: String
    /// `"per_commit_patch_id"` or `"content_identical"`, present only beside
    /// `"equivalent"`. The two are not equally strong — see the vocabulary.
    let equivalenceMethod: String?
    /// Why `patchEquivalence` is `"unknown"`, present only for that value.
    let equivalenceUnknownReason: String?
    /// Commits `mergedInto` does not contain, split by whether an equivalent
    /// patch is already there and by whether the per-commit method could
    /// classify them at all.
    ///
    /// All three present together or all three `nil` with
    /// `divergenceUnavailableReason` set. A `nil` is never a zero: "no commits
    /// of its own" is the one reading an unanswered probe must not produce.
    let commitsUniqueToHead: UInt32?
    let commitsEquivalentElsewhere: UInt32?
    let commitsUnclassified: UInt32?
    let divergenceUnavailableReason: String?
    /// When the newest commit here was written, and the same for the branch it
    /// was compared against, as seconds since the Unix epoch.
    ///
    /// Instants rather than ages because the CLI reads no clock; a surface
    /// showing an age does the subtraction itself. Each carries its own reason
    /// when absent — a repository with no recorded default branch has a
    /// readable tip and nothing to compare it against.
    let headTipCommittedAtUnix: UInt64?
    let headTipUnavailableReason: String?
    let comparisonTipCommittedAtUnix: UInt64?
    let comparisonTipUnavailableReason: String?
    /// Whether this worktree holds something that should stop a person
    /// treating it as spent: uncommitted work, untracked files, something
    /// using it, or commits no remote has.
    ///
    /// A sentence to show, never a gate. `false` does not make anything
    /// deletable — the resource still goes through the policy class its own
    /// evidence earned and through deletion-time revalidation.
    let holdsWorkInProgress: Bool
    /// The resource ids of this worktree's discovered candidates, in
    /// `DetectReportDto.candidates`' own order. Ids only: the aggregate
    /// explains, the candidate list authorizes.
    let memberResourceIds: [String]

    enum CodingKeys: String, CodingKey {
        case root
        case linkedWorktree = "linked_worktree"
        case dirty
        case untracked
        case activity
        case upstream
        case ahead
        case behind
        case branch
        case merged
        case mergedInto = "merged_into"
        case patchEquivalence = "patch_equivalence"
        case equivalenceMethod = "equivalence_method"
        case equivalenceUnknownReason = "equivalence_unknown_reason"
        case commitsUniqueToHead = "commits_unique_to_head"
        case commitsEquivalentElsewhere = "commits_equivalent_elsewhere"
        case commitsUnclassified = "commits_unclassified"
        case divergenceUnavailableReason = "divergence_unavailable_reason"
        case headTipCommittedAtUnix = "head_tip_committed_at_unix"
        case headTipUnavailableReason = "head_tip_unavailable_reason"
        case comparisonTipCommittedAtUnix = "comparison_tip_committed_at_unix"
        case comparisonTipUnavailableReason = "comparison_tip_unavailable_reason"
        case holdsWorkInProgress = "holds_work_in_progress"
        case memberResourceIds = "member_resource_ids"
    }

    var id: String { root }
}

/// Mirrors `reporting::dto::WorkspaceFamilyReport` (HORO-1511) — every
/// worktree sharing one git directory, i.e. one repository's checkouts, and
/// what they add up to.
///
/// This exists so a person can be told "this project accounts for 7.5 GB
/// across three worktrees" instead of reading thirty unrelated-looking
/// lines. That figure is an attention figure and nothing else: it is summed
/// from detector *estimates*, so it may not be what a run actually reclaims,
/// and `worktreesHoldingWorkInProgress` is published beside it precisely
/// because the bulk and the outstanding work are in the same group.
///
/// `protectedCount` is a count and there is no `protectedBytes`, by design:
/// presenting bytes a run can never take as part of a project's reclaimable
/// total is the exact misread the split prevents.
struct WorkspaceFamilyReportDto: Decodable, Equatable, Identifiable {
    /// The shared git directory that identifies this family.
    let commonDir: String
    let worktreeCount: UInt64
    /// Estimated space a recovery run could take from this family without
    /// asking — members the policy engine labelled `AUTO_SAFE`.
    let actionableNowBytes: UInt64
    let actionableNowHuman: String
    let actionableNowCount: UInt64
    /// Members labelled `ASK`: real bulk, but not automatic.
    let requiresConfirmationBytes: UInt64
    let requiresConfirmationHuman: String
    let requiresConfirmationCount: UInt64
    /// Counted, not summed. See this type's doc comment.
    let protectedCount: UInt64
    /// Members whose evidence was too thin to classify, which policy treats
    /// as protected.
    let unknownCount: UInt64
    /// Members no probe measured. Distinct from a zero-byte member, and the
    /// reason the totals above are not the whole story.
    let unmeasuredCount: UInt64
    /// `true` when a member inside one of the byte totals reported its
    /// estimate as a lower bound, so the real figure may be larger.
    let isLowerBound: Bool
    /// How many worktrees hold work in progress. `> 0` is why a family total
    /// must never read as "delete this project".
    let worktreesHoldingWorkInProgress: UInt64
    /// Sorted by root path on the Rust side, so two runs over one disk state
    /// agree and a surface does not need to re-sort.
    let worktrees: [WorkspaceWorktreeReportDto]

    enum CodingKeys: String, CodingKey {
        case commonDir = "common_dir"
        case worktreeCount = "worktree_count"
        case actionableNowBytes = "actionable_now_bytes"
        case actionableNowHuman = "actionable_now_human"
        case actionableNowCount = "actionable_now_count"
        case requiresConfirmationBytes = "requires_confirmation_bytes"
        case requiresConfirmationHuman = "requires_confirmation_human"
        case requiresConfirmationCount = "requires_confirmation_count"
        case protectedCount = "protected_count"
        case unknownCount = "unknown_count"
        case unmeasuredCount = "unmeasured_count"
        case isLowerBound = "is_lower_bound"
        case worktreesHoldingWorkInProgress = "worktrees_holding_work_in_progress"
        case worktrees
    }

    var id: String { commonDir }

    /// Members this family knows of, across every worktree. Not a byte
    /// figure and not a permission — the number of candidate rows a person
    /// would find if they went looking.
    var memberCount: UInt64 {
        actionableNowCount + requiresConfirmationCount + protectedCount + unknownCount
    }
}

/// Mirrors `reporting::dto::DetectReport`.
struct DetectReportDto: Decodable, Equatable {
    let candidates: [DetectCandidateReportDto]

    /// Per-detector health for the pass that produced `candidates`
    /// (HORO-1484).
    ///
    /// Defaults to empty when absent, because the app resolves whichever
    /// `glomeris` is on `PATH` and that binary may predate the field — the
    /// same forward/backward tolerance `impactTier` documents above. An
    /// empty array is correctly indistinguishable from "this binary does not
    /// report detector health", and `discoveryComplete` below defaults to
    /// `true` for exactly that case: an older CLI is not evidence that
    /// something failed.
    let detectors: [DetectorHealthReportDto]

    /// Did every detector answer? `false` means at least one probe failed,
    /// so `candidates` is NOT a complete account of what could be reclaimed
    /// and no surface may present it as one.
    ///
    /// Derived on the Rust side from `detectors`, never tracked separately,
    /// so it cannot disagree with the array it summarizes.
    let discoveryComplete: Bool

    /// Detectors whose probe failed — the subset a surface must show rather
    /// than rendering the candidate list as the whole picture.
    ///
    /// This, and not `discoveryComplete`, is what the panel reads, which is
    /// worth stating because the opposite looks tidier: a surface that says
    /// "part of this search did not finish" has to name what did not finish, or
    /// the sentence is unactionable, and only this array carries the names. The
    /// two cannot disagree — Rust derives the flag from the very slice this
    /// filters, and `DtoGoldenFixturesTests` pins that agreement in both
    /// directions — so reading the flag as well would add a second source of
    /// truth for a question that has one answer.
    var failedDetectors: [DetectorHealthReportDto] {
        detectors.filter(\.didFail)
    }

    /// Discovered resources grouped by the git worktree family they belong
    /// to (HORO-1511) — explanatory metadata for "where did my disk go",
    /// never an authorization.
    ///
    /// Empty when the CLI did not group, which includes a binary that
    /// predates the field: Rust omits the key entirely rather than emitting
    /// `[]`, and both read as "no grouping was done here" rather than as
    /// "this machine has no worktree families". A resource outside any git
    /// working tree, or one whose git probe failed, appears in `candidates`
    /// and in no family, so the families never account for the whole list.
    let workspaces: [WorkspaceFamilyReportDto]

    enum CodingKeys: String, CodingKey {
        case candidates
        case detectors
        case discoveryComplete = "discovery_complete"
        case workspaces
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        candidates = try container.decode([DetectCandidateReportDto].self, forKey: .candidates)
        detectors =
            try container.decodeIfPresent([DetectorHealthReportDto].self, forKey: .detectors) ?? []
        discoveryComplete =
            try container.decodeIfPresent(Bool.self, forKey: .discoveryComplete) ?? true
        workspaces =
            try container.decodeIfPresent([WorkspaceFamilyReportDto].self, forKey: .workspaces)
            ?? []
    }

    /// Non-decoding initializer for tests and previews.
    init(
        candidates: [DetectCandidateReportDto],
        detectors: [DetectorHealthReportDto] = [],
        discoveryComplete: Bool = true,
        workspaces: [WorkspaceFamilyReportDto] = []
    ) {
        self.candidates = candidates
        self.detectors = detectors
        self.discoveryComplete = discoveryComplete
        self.workspaces = workspaces
    }
}

/// Mirrors `reporting::dto::LlmPlanItemReport` (HORO-1308) — one row of the
/// advisory `llm-plan` ranking.
///
/// ## Two of these fields are the model's words. The rest are the machine's.
///
/// `priority` and `modelReason` are the only fields a provider chose.
/// Everything else — `policyLabel`, `completeness`, `confidence`, `explain`,
/// `skipReason`, and every field of `candidate` — was computed locally from
/// evidence and the real policy engine, and reads identically whether or not
/// a provider was ever contacted. A surface rendering `modelReason` MUST
/// attribute it to the model: it is a claim, and it sits next to evidence.
///
/// **The ORDER of `LlmPlanReport.items` is also the model's.** `plan_with_llm`
/// pushes validated items in the order the provider returned them and never
/// sorts; validation only drops entries. So a surface must not present a
/// position in this list as a Glomeris ranking — the candidates list's order
/// is Glomeris's judgment (`reporting::ranking`), this one is advice, and
/// conflating them would launder a model's opinion into a machine verdict.
///
/// ## Why `candidate` is nested rather than flattened
///
/// It is the byte-for-byte same projection `detect --json` prints for this
/// resource, so `AiPlanSectionView` builds its rows with the same
/// `CandidateRowViewModel` the candidates list uses, and reads
/// `candidate.executable` / `candidate.offeredActions` /
/// `candidate.refusalReason` as the only authority on what may be done.
/// There is deliberately no second enablement path for a recommendation to
/// travel down, and no `fingerprintToken` here at all — acting on a
/// suggestion still goes through its own `explain` call, exactly as acting
/// on a candidates-list row does.
struct LlmPlanItemReportDto: Decodable, Equatable, Identifiable {
    let resourceId: String
    let policyLabel: String
    let requestedActionId: String?
    /// The model's own priority number, carried verbatim and deliberately
    /// NOT rendered by `AiPlanSectionView`. It is a second copy of the claim
    /// the list order already makes, it can contradict that order, and two
    /// competing numberings on one row would read as though one of them were
    /// authoritative. Kept on the DTO because `--json` consumers should see
    /// everything the provider said.
    let priority: UInt32?
    /// The model's own rationale, already bounded to 400 characters and
    /// control-character-stripped in Rust (`sanitize_model_reason`) so it
    /// cannot rewrite a terminal line or crowd the real verdict off a
    /// fixed-width popover. `nil` when the model gave none.
    let modelReason: String?
    let explain: String?
    let skipReason: String?
    let candidate: DetectCandidateReportDto
    let completeness: String
    let confidence: String

    enum CodingKeys: String, CodingKey {
        case resourceId = "resource_id"
        case policyLabel = "policy_label"
        case requestedActionId = "requested_action_id"
        case priority
        case modelReason = "model_reason"
        case explain
        case skipReason = "skip_reason"
        case candidate
        case completeness
        case confidence
    }

    var id: String { resourceId }
}

/// Mirrors `reporting::dto::LlmPlanReport` (HORO-1308).
///
/// `providerError` is non-`nil` when the provider call itself failed; the CLI
/// still prints this whole report and then exits 1, which is why the AI Plan
/// card reads it through `runRaw` and decodes stdout on a non-zero exit
/// rather than throwing the body away.
///
/// The two `dropped*` counts record suggestions Rust refused to validate —
/// the model named a resource that was never discovered, or an action that is
/// not registered. They are surfaced rather than swallowed: a plan with three
/// rows and two silently discarded items is not a three-row plan.
struct LlmPlanReportDto: Decodable, Equatable {
    let items: [LlmPlanItemReportDto]
    let droppedUnknownResource: UInt32
    let droppedUnknownAction: UInt32
    let providerError: String?

    enum CodingKeys: String, CodingKey {
        case items
        case droppedUnknownResource = "dropped_unknown_resource"
        case droppedUnknownAction = "dropped_unknown_action"
        case providerError = "provider_error"
    }
}

/// Mirrors `reporting::dto::ProgressEvent` (HORO-1052) — one line of the
/// `--progress-json` NDJSON stream emitted on stderr while `detect`'s
/// discovery phase runs. The Rust side uses `#[serde(tag = "phase",
/// rename_all = "snake_case")]`, an internally-tagged enum
/// (`{"phase":"detector_started","detector":"..."}` /
/// `{"phase":"detector_finished","detector":"...","candidates_found":N}`),
/// so this mirror needs a hand-written `init(from:)` rather than the
/// simple per-field `CodingKeys` used elsewhere in this file.
enum ProgressEventDto: Decodable, Equatable {
    case detectorStarted(detector: String)
    /// HORO-1484: `outcome` and `reason` were added because
    /// `candidatesFound: 0` is what a detector that found nothing and a
    /// detector that never answered had in common, and it was all this event
    /// said about either. `outcome` is
    /// `"found"`/`"tool_absent"`/`"tool_not_running"`/`"failed"`; `reason` is
    /// present only for `"failed"`.
    ///
    /// `outcome` is optional for the same forward/backward reason as
    /// `DetectReportDto.detectors`: the resolved CLI may predate the field.
    /// `nil` means "this binary does not report outcomes", which is not the
    /// same as `"failed"` and must never be shown as one.
    case detectorFinished(
        detector: String, candidatesFound: Int, outcome: String?, reason: String?)

    private enum CodingKeys: String, CodingKey {
        case phase
        case detector
        case candidatesFound = "candidates_found"
        case outcome
        case reason
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let phase = try container.decode(String.self, forKey: .phase)
        switch phase {
        case "detector_started":
            let detector = try container.decode(String.self, forKey: .detector)
            self = .detectorStarted(detector: detector)
        case "detector_finished":
            let detector = try container.decode(String.self, forKey: .detector)
            let candidatesFound = try container.decode(Int.self, forKey: .candidatesFound)
            let outcome = try container.decodeIfPresent(String.self, forKey: .outcome)
            let reason = try container.decodeIfPresent(String.self, forKey: .reason)
            self = .detectorFinished(
                detector: detector,
                candidatesFound: candidatesFound,
                outcome: outcome,
                reason: reason
            )
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .phase,
                in: container,
                debugDescription: "Unknown ProgressEvent phase: \(phase)"
            )
        }
    }
}

/// Mirrors `reporting::dto::ExplainReport` (HORO-1051/HORO-1053). Backs
/// HORO-1064's candidate detail view — the sole host of the Clean
/// button. `executable` and `offeredActions[].requiresConfirmation` are
/// the two fields that view's button enablement and confirmation-sheet
/// gating read directly; `policyLabel`/`reasons` are shown only as
/// human-readable context and must never be branched on for either
/// decision (see the standing project rule in GlomerisMenuBarApp.swift).
struct ExplainReportDto: Decodable, Equatable {
    let resourceId: String
    let kind: String
    let detector: String
    let sources: [String]
    /// Logical size as reported by the filesystem — explicitly distinct
    /// from `reclaimableBytes`/`reclaimableHuman` below; the detail view
    /// must label the two separately rather than conflate them.
    let logicalBytes: UInt64?
    let logicalHuman: String?
    let reclaimableBytes: UInt64?
    let reclaimableHuman: String?
    let reclaimableBytesIsLowerBound: Bool
    let completeness: String
    let confidence: String
    let activeUseSignals: [String]
    let regenerability: String
    let policyLabel: String
    let reasons: [String]
    let nativeCleanupAvailable: Bool
    let nativeCleanupActionId: String?
    /// Opaque fingerprint-pinning token (HORO-1051) — carried, not
    /// interpreted, by this Swift layer. HORO-1065 will forward it
    /// verbatim as `execute --observed-fingerprint <token>`.
    let fingerprintToken: String?
    /// `true` only when a real registered action exists for this
    /// resource and its policy class doesn't unconditionally forbid it.
    /// This is the single field the Clean button's enablement reads.
    let executable: Bool
    /// Empty for a non-executable resource; one entry otherwise. The
    /// matching entry's `requiresConfirmation` is the single field the
    /// confirmation sheet's visibility reads.
    let offeredActions: [OfferedActionDto]
    let refusalReason: String?

    enum CodingKeys: String, CodingKey {
        case resourceId = "resource_id"
        case kind
        case detector
        case sources
        case logicalBytes = "logical_bytes"
        case logicalHuman = "logical_human"
        case reclaimableBytes = "reclaimable_bytes"
        case reclaimableHuman = "reclaimable_human"
        case reclaimableBytesIsLowerBound = "reclaimable_bytes_is_lower_bound"
        case completeness
        case confidence
        case activeUseSignals = "active_use_signals"
        case regenerability
        case policyLabel = "policy_label"
        case reasons
        case nativeCleanupAvailable = "native_cleanup_available"
        case nativeCleanupActionId = "native_cleanup_action_id"
        case fingerprintToken = "fingerprint_token"
        case executable
        case offeredActions = "offered_actions"
        case refusalReason = "refusal_reason"
    }
}

/// Mirrors `reporting::dto::ExecuteReport`. `outcome` is deliberately kept
/// as a plain `String` rather than a Swift `enum` — decoding an
/// unrecognized value must never fail the whole document (a new Rust
/// outcome variant should degrade to "unknown" in the UI, not crash
/// decoding of the rest of the report); a caller that needs exhaustive
/// handling switches on the known string constants itself.
struct ExecuteReportDto: Decodable, Equatable {
    let actionId: String
    let resourceId: String
    let outcome: String
    let failureMessage: String?
    let abortReason: String?
    let expectedReclaimedBytes: UInt64?
    let actualReclaimedBytes: UInt64?
    /// HORO-1312. Rust's own rendering of the two byte counts above. This app
    /// used to format the measured one itself with `ByteCountFormatter`, which
    /// is 1000-based, while every number Rust prints is 1024-based — so one
    /// panel showed a 2.1 GB result beside a 2.0 GB estimate for the same
    /// bytes.
    ///
    /// Optional because the binary on `PATH` is not necessarily the one this
    /// app was built alongside: an older `glomeris` omits these keys, and a
    /// non-optional field would fail the whole decode and blank the result
    /// rather than losing one string. See `describeExecuteOutcome` for what
    /// is shown when they are absent.
    let expectedReclaimedHuman: String?
    let actualReclaimedHuman: String?

    enum CodingKeys: String, CodingKey {
        case actionId = "action_id"
        case resourceId = "resource_id"
        case outcome
        case failureMessage = "failure_message"
        case abortReason = "abort_reason"
        case expectedReclaimedBytes = "expected_reclaimed_bytes"
        case actualReclaimedBytes = "actual_reclaimed_bytes"
        case expectedReclaimedHuman = "expected_reclaimed_human"
        case actualReclaimedHuman = "actual_reclaimed_human"
    }
}

/// Mirrors `reporting::dto::ExecuteRefusalReport` — printed to stdout on
/// every non-`Executed` `execute` outcome (HORO-1056): resource/action
/// resolution failures, every policy refusal, and the pre-resolution
/// `"busy"` lock-contention case. `reason` is deliberately kept as a
/// plain `String` rather than an enum for the same forward-compatibility
/// reason as `ExecuteReportDto.outcome` above.
struct ExecuteRefusalReportDto: Decodable, Equatable {
    let reason: String
    let message: String
}

/// Mirrors `reporting::dto::LlmCheckReport` — the result of one
/// `glomeris llm-check` run (HORO-1309).
///
/// `outcome` is a plain `String` for the same forward-compatibility reason as
/// `ExecuteReportDto.outcome`: a new Rust outcome must degrade to "something
/// unexpected happened" in the UI rather than fail decoding of the report that
/// would have explained it. The known values are in
/// `GlomerisVocabulary.llmCheckOutcome`, which the
/// `vocabulary-covers-cli-tokens` CI job checks against the Rust producer.
///
/// Note what is not here: no base URL and no API key. The Rust side never
/// emits them — see `LlmCheckReport`'s own doc comment — so there is nothing
/// for this app to accidentally render.
struct LlmCheckReportDto: Decodable, Equatable {
    let outcome: String
    let model: String
    let endpointPath: String
    let error: String?
    let responseExcerpt: String?

    enum CodingKeys: String, CodingKey {
        case outcome
        case model
        case endpointPath = "endpoint_path"
        case error
        case responseExcerpt = "response_excerpt"
    }
}

/// Mirrors `reporting::dto::LlmPayloadReport` — what `glomeris llm-plan
/// --print-payload` emits: the exact request a live plan would send, obtained
/// without sending it (HORO-1298, surfaced in the GUI by HORO-1309).
///
/// `systemPrompt` and `userPrompt` are the outbound bytes. `resourceAliases`
/// is the opposite: the local-only table mapping each wire id back to the real
/// resource, which is what makes the preview readable to the user while
/// keeping absolute paths off the wire. A privacy preview that showed the
/// aliases as though they were transmitted would misrepresent the thing it
/// exists to disclose, so the two must stay visually distinct wherever this is
/// rendered.
struct LlmPayloadReportDto: Decodable, Equatable {
    let systemPrompt: String
    let userPrompt: String
    let resourceAliases: [LlmPayloadResourceAliasDto]

    enum CodingKeys: String, CodingKey {
        case systemPrompt = "system_prompt"
        case userPrompt = "user_prompt"
        case resourceAliases = "resource_aliases"
    }
}

/// Mirrors `reporting::dto::LlmPayloadResourceAlias`: one wire id and the
/// local resource it stands for. `Identifiable` by the wire id, which the Rust
/// side generates uniquely per payload.
struct LlmPayloadResourceAliasDto: Decodable, Equatable, Identifiable {
    let wireResourceId: String
    let localResourceId: String

    var id: String { wireResourceId }

    enum CodingKeys: String, CodingKey {
        case wireResourceId = "wire_resource_id"
        case localResourceId = "local_resource_id"
    }
}

/// Mirrors `reporting::dto::AutopilotAskPreauthorizationReport` (HORO-1310):
/// one standing consent, naming exactly one resource kind and exactly one
/// policy reason.
///
/// Both are canonical CLI tokens, not prose. `GlomerisVocabulary.kind` and
/// `.reason` are what turn them into the words on screen, and
/// `scripts/check-vocabulary-covers-cli-tokens.sh` is what keeps those two
/// tables covering every token Rust can emit.
///
/// `Identifiable` by the pair, because the pair is what the envelope stores
/// and two entries can share a kind.
struct AutopilotAskPreauthorizationDto: Decodable, Equatable, Identifiable {
    let kind: String
    let reason: String

    var id: String { "\(kind):\(reason)" }
}

/// Mirrors `reporting::dto::AutopilotCeilingsReport` (HORO-1310): the limits
/// above which `autopilot enable` refuses a value outright.
///
/// Read from the CLI rather than written here so a stepper's maximum cannot
/// disagree with what `enable` accepts. A control offering a value the CLI
/// then rejects would read as the app lying about its own limits.
struct AutopilotCeilingsDto: Decodable, Equatable {
    let maxActions: UInt32
    let maxBytes: UInt64
    let maxBytesHuman: String
    let maxDurationSecs: UInt64

    enum CodingKeys: String, CodingKey {
        case maxActions = "max_actions"
        case maxBytes = "max_bytes"
        case maxBytesHuman = "max_bytes_human"
        case maxDurationSecs = "max_duration_secs"
    }
}

/// Mirrors `reporting::dto::AutopilotEnvelopeReport` (HORO-1310) — the whole
/// of what Autopilot is authorized to do, and the whole of what no
/// authorization can ever cover.
///
/// Every list here is data, including the refusals and the available choices.
/// That is the point: the Autopilot settings screen has to tell a user what
/// Glomeris will always refuse, and under the standing project rule at the top
/// of `GlomerisMenuBarApp.swift` it must not be the thing that decides what
/// that is. A Swift array of resource kinds would also go stale the first time
/// a detector is added, and the failure would be invisible — a kind the CLI
/// accepts that the GUI cannot offer.
///
/// Nothing on this type is `Encodable`. Changing an envelope goes through
/// `autopilot enable`/`revoke`, whose argument parsing runs the envelope's own
/// checked setters; there is no path by which this app hands Rust an envelope
/// to trust.
struct AutopilotEnvelopeDto: Decodable, Equatable {
    /// Whether Autopilot may run at all. Not the only thing that stops it: an
    /// enabled envelope with no allowed kinds, or a `maxActions` of zero,
    /// executes nothing either — so the screen must not present `enabled` on
    /// its own as "it will act".
    let enabled: Bool
    let allowedKinds: [String]
    let askPreauthorizations: [AutopilotAskPreauthorizationDto]
    let maxActions: UInt32
    let maxBytes: UInt64
    /// Rust's own rendering of `maxBytes`, for the same reason
    /// `ExecuteReportDto.actualReclaimedHuman` carries one: every number the
    /// CLI prints is 1024-based and `ByteCountFormatter` is not, so formatting
    /// it here would disagree with the CLI's own output for the same bytes.
    let maxBytesHuman: String
    let maxDurationSecs: UInt64
    /// The disk-pressure state a run must have reached, or `nil` when a run is
    /// not gated on pressure at all — which is the default, and is the more
    /// permissive of the two, so the screen must say which one is in force
    /// rather than leaving a blank row.
    let minPressure: String?
    /// Whether the grant says Autopilot may start a run in answer to a
    /// disk-pressure alert — the setting as the user left it, in force or not
    /// (HORO-1510). This is the one to bind a toggle to, so that revoking
    /// Autopilot does not read as having silently cleared the preference.
    let respondToAlerts: Bool
    /// Whether an unprompted run is authorized right now. The one to branch on.
    ///
    /// Two fields rather than one `&&` in Swift, and the reason is the standing
    /// rule at the top of `GlomerisMenuBarApp.swift`: composing `enabled` with
    /// `respondToAlerts` here would make this app the place that decides when it
    /// may delete things without being asked. Rust computes it in
    /// `AutopilotEnvelope::starts_unprompted`; this is a quotation.
    let startsUnprompted: Bool
    let ceilings: AutopilotCeilingsDto
    let allowlistableKinds: [String]
    /// Kinds that can never be allowlisted, whatever is asked for.
    let neverAllowlistableKinds: [String]
    let preauthorizableReasons: [String]
    /// Reasons a resource can be held back by that can never be
    /// pre-authorized. Narrower than "everything not in
    /// `preauthorizableReasons`" — the Rust producer leaves out the reasons
    /// that justify letting something through, since listing those would be a
    /// warning about nothing.
    let neverPreauthorizableReasons: [String]
    let pressureStates: [String]
    /// The policy labels no envelope can make executable.
    let neverExecutableLabels: [String]
    /// What the model's authority is, one sentence per fact, written in Rust
    /// because each is a claim about Rust's behaviour.
    let aiAuthority: [String]
    /// Where the envelope file lives, or `nil` if the path could not be
    /// resolved. Local, and never part of any provider request.
    let storedAt: String?

    enum CodingKeys: String, CodingKey {
        case enabled
        case allowedKinds = "allowed_kinds"
        case askPreauthorizations = "ask_preauthorizations"
        case maxActions = "max_actions"
        case maxBytes = "max_bytes"
        case maxBytesHuman = "max_bytes_human"
        case maxDurationSecs = "max_duration_secs"
        case minPressure = "min_pressure"
        case respondToAlerts = "respond_to_alerts"
        case startsUnprompted = "starts_unprompted"
        case ceilings
        case allowlistableKinds = "allowlistable_kinds"
        case neverAllowlistableKinds = "never_allowlistable_kinds"
        case preauthorizableReasons = "preauthorizable_reasons"
        case neverPreauthorizableReasons = "never_preauthorizable_reasons"
        case pressureStates = "pressure_states"
        case neverExecutableLabels = "never_executable_labels"
        case aiAuthority = "ai_authority"
        case storedAt = "stored_at"
    }
}

// MARK: - Recovery goal (HORO-1506)

/// Mirrors `reporting::dto::RecoveryGoalReport` — one recovery goal on both
/// axes, plus the one sentence that says which is which.
///
/// All three fields are decoded and all three are used. `usedPercent` is the
/// product-facing axis the GUI asks for; `freePercent` is the same goal in the
/// units the Rust recovery loop itself works in; `description` is the string
/// every surface shows verbatim.
///
/// Nothing in this app converts between the two. `RecoveryGoal` in
/// `src/executor/goal.rs` is the single typed adapter, and it is tested against
/// the loop's own `target_met` predicate over a grid of goals and volumes — so
/// a percentage arriving here has already been checked against the thing that
/// decides completion. A Swift `100 - x` would be a second, unchecked
/// conversion of exactly the kind HORO-1506 exists to remove.
struct RecoveryGoalReportDto: Decodable, Equatable {
    let usedPercent: Double
    let freePercent: Double
    /// e.g. `"60% used (40% free)"`. Shown as-is, never reassembled from the
    /// two numbers above: the campaign's rule is that no surface may display a
    /// bare percentage whose axis is unstated, and the only way two surfaces
    /// cannot word that differently is for neither of them to word it.
    let description: String

    enum CodingKeys: String, CodingKey {
        case usedPercent = "used_percent"
        case freePercent = "free_percent"
        case description
    }
}

/// Mirrors `reporting::dto::RecoveryOpportunityReport` — what is *estimated*
/// to be reclaimable now, split by what policy would actually permit.
///
/// The split is the whole point and the app must keep it: one total would
/// invite a user to read confirmation-gated and protected space as space they
/// are about to get back. Note what is deliberately absent — there is no
/// `protectedBytes`, because bytes no run can ever take are not an
/// opportunity, and a field for them is the first thing a well-meaning summary
/// row would add up.
///
/// Every byte figure here is an estimate. None of them may decide that a goal
/// was met; only re-measured free space does that (campaign §9).
struct RecoveryOpportunityReportDto: Decodable, Equatable {
    let actionableNowCount: Int
    let actionableNowBytes: UInt64
    let actionableNowHuman: String
    let requiresConfirmationCount: Int
    let requiresConfirmationBytes: UInt64
    let requiresConfirmationHuman: String
    let notExecutableCount: Int
    let protectedCount: Int
    /// `true` when at least one candidate behind the totals reported its
    /// estimate as a lower bound, so the real figure may be larger.
    /// `GlomerisVocabulary.storageImpact(human:isLowerBound:)` is what renders
    /// that as a `≥` rather than this file inventing a prefix.
    let isLowerBound: Bool

    enum CodingKeys: String, CodingKey {
        case actionableNowCount = "actionable_now_count"
        case actionableNowBytes = "actionable_now_bytes"
        case actionableNowHuman = "actionable_now_human"
        case requiresConfirmationCount = "requires_confirmation_count"
        case requiresConfirmationBytes = "requires_confirmation_bytes"
        case requiresConfirmationHuman = "requires_confirmation_human"
        case notExecutableCount = "not_executable_count"
        case protectedCount = "protected_count"
        case isLowerBound = "is_lower_bound"
    }
}

/// Mirrors `reporting::dto::RecoveryPreviewReport` — the pre-flight for a
/// recovery goal, from `glomeris free --dry-run --json`.
///
/// This is the report the Recovery card shows *before* offering to run
/// anything, and it is the reason the card can state current usage, the goal,
/// the bytes still needed and the reclaimable opportunity without computing any
/// of them.
///
/// `current` is the same `StatusReportDto` the Status card decodes, so the two
/// cards cannot disagree about how full the disk is.
struct RecoveryPreviewReportDto: Decodable, Equatable {
    let goal: RecoveryGoalReportDto
    let current: StatusReportDto
    let requiredFreeBytes: UInt64
    let requiredFreeHuman: String
    /// Additional free bytes still needed, measured from `current`.
    let bytesNeeded: UInt64
    let bytesNeededHuman: String
    let opportunity: RecoveryOpportunityReportDto
    /// A planning judgment only, and named "appears" in Rust for that reason:
    /// whether the estimated actionable bytes cover `bytesNeeded`.
    ///
    /// `false` does not mean a run is pointless — estimates are often lower
    /// bounds — and `true` does not mean the goal will be reached. Any wording
    /// this app puts next to it has to survive both of those being true, which
    /// is why the card renders it as a hint and never as a prediction.
    let goalAppearsReachable: Bool
    /// `false` as soon as any detector's probe failed. Same contract as
    /// `DetectReportDto.discoveryComplete`.
    let discoveryComplete: Bool
    /// Sentences written in Rust, shown verbatim, covering exactly the ways
    /// this report can mislead. Rendered rather than summarized: they are the
    /// estimate/measurement distinction the campaign requires be stated, and a
    /// client that paraphrased them would be deciding which of them matter.
    let caveats: [String]
    let detectors: [DetectorHealthReportDto]
    let candidates: [DetectCandidateReportDto]

    enum CodingKeys: String, CodingKey {
        case goal
        case current
        case requiredFreeBytes = "required_free_bytes"
        case requiredFreeHuman = "required_free_human"
        case bytesNeeded = "bytes_needed"
        case bytesNeededHuman = "bytes_needed_human"
        case opportunity
        case goalAppearsReachable = "goal_appears_reachable"
        case discoveryComplete = "discovery_complete"
        case caveats
        case detectors
        case candidates
    }

    /// Detectors whose probe failed, for the same advisory the candidates card
    /// shows. Derived here rather than decoded, because Rust derives
    /// `discoveryComplete` from the very same slice — computing it from
    /// `detectors` is the only way the two cannot drift apart.
    var failedDetectors: [DetectorHealthReportDto] {
        detectors.filter(\.didFail)
    }
}

/// One `measured` progress line's payload (HORO-1509).
///
/// The only statement of fact about free space in the recovery progress stream,
/// and therefore the only event a live display may update its "current usage"
/// from. `bytesFreedSoFar` is re-measured free space rather than a sum of
/// candidate estimates (campaign §9), which is what makes it safe to show as
/// progress.
struct RecoveryMeasuredProgressDto: Decodable, Equatable {
    let iteration: UInt32
    let totalBytes: UInt64
    let freeBytes: UInt64
    let usedPercent: Double
    let freeHuman: String
    let bytesFreedSoFar: UInt64
    let bytesFreedSoFarHuman: String

    enum CodingKeys: String, CodingKey {
        case iteration
        case totalBytes = "total_bytes"
        case freeBytes = "free_bytes"
        case usedPercent = "used_percent"
        case freeHuman = "free_human"
        case bytesFreedSoFar = "bytes_freed_so_far"
        case bytesFreedSoFarHuman = "bytes_freed_so_far_human"
    }
}

/// One `discovered` progress line's payload (HORO-1509).
///
/// `candidates` counts what has a resolvable action rather than what a detector
/// saw, and a non-zero `detectorsFailed` is what withdraws a later
/// `safe_exhausted`'s usual meaning: part of the disk was never looked at.
struct RecoveryDiscoveredProgressDto: Decodable, Equatable {
    let iteration: UInt32
    let candidates: UInt32
    let detectorsFailed: UInt32

    enum CodingKeys: String, CodingKey {
        case iteration
        case candidates
        case detectorsFailed = "detectors_failed"
    }
}

/// One `action_started` progress line's payload (HORO-1509).
struct RecoveryActionStartedProgressDto: Decodable, Equatable {
    let iteration: UInt32
    let resource: String
    let action: String
    /// The policy class the action was admitted under. Carried for display
    /// only — this app branches on no policy token, per the standing project
    /// rule in GlomerisMenuBarApp.swift.
    let policyLabel: String
    /// An estimate, named as one in Rust and named as one here. Absent when the
    /// detector could not size the resource, and absent means *not measured* —
    /// never zero. Must never be accumulated into a progress figure.
    let estimatedBytes: UInt64?
    let estimatedHuman: String?

    enum CodingKeys: String, CodingKey {
        case iteration
        case resource
        case action
        case policyLabel = "policy_label"
        case estimatedBytes = "estimated_bytes"
        case estimatedHuman = "estimated_human"
    }
}

/// One `action_finished` progress line's payload (HORO-1509).
struct RecoveryActionFinishedProgressDto: Decodable, Equatable {
    let iteration: UInt32
    let resource: String
    let action: String
    /// `succeeded`/`failed`/`aborted_by_revalidation`/`dry_run`, from
    /// `execution_outcome_tag` — the same producer the local audit log uses,
    /// which is what makes HORO-1509 AC5 (the run summary and the audit log
    /// agree on actions and outcomes) a property of the code. Worded by
    /// `GlomerisVocabulary.outcome`.
    let outcome: String
    /// What the executor measured for this one action. Absent when it could not
    /// be measured; a client showing "0 B" for it would be reporting a
    /// measurement nobody took.
    let reclaimedBytes: UInt64?
    let reclaimedHuman: String?
    /// Re-measured, cumulative, and the figure a progress display belongs on.
    let bytesFreedSoFar: UInt64
    let bytesFreedSoFarHuman: String

    enum CodingKeys: String, CodingKey {
        case iteration
        case resource
        case action
        case outcome
        case reclaimedBytes = "reclaimed_bytes"
        case reclaimedHuman = "reclaimed_human"
        case bytesFreedSoFar = "bytes_freed_so_far"
        case bytesFreedSoFarHuman = "bytes_freed_so_far_human"
    }
}

/// Mirrors `reporting::dto::RecoveryProgressEvent` (HORO-1509) — one line of
/// the NDJSON stream `glomeris free --progress-json` writes to stderr while a
/// real recovery run is in flight.
///
/// Internally tagged on `phase`, like ``ProgressEventDto``, so this needs a
/// hand-written `init(from:)`. The two streams share that key and nothing else:
/// `ProgressEventDto` describes a discovery scan, this describes a run that
/// mutates the filesystem, and no phase name appears in both — which is why
/// they are separate types rather than one enum with both sets of cases.
///
/// Each payload is decoded from the *same* keyed container as the tag, since
/// Rust flattens the variant's fields alongside `phase`. A payload struct
/// ignores the extra `phase` key, so `try Payload(from: decoder)` is the whole
/// of it.
///
/// An unrecognised phase throws, and the throw is the tolerant outcome here
/// rather than the strict one: `GlomerisClient.readAllWithLiveProgress` skips a
/// line it cannot decode, so a CLI newer than this app emitting an extra phase
/// costs one missed status update and nothing else. That is the opposite of the
/// rule for report tokens like `stopReason`, which stay `String` precisely
/// because failing their decode would leave the user with no result at all.
enum RecoveryProgressEventDto: Decodable, Equatable {
    /// Loop step 1: the filesystem was measured.
    case measured(RecoveryMeasuredProgressDto)
    /// Loop step 4 starting: detectors are being asked what exists *now*. The
    /// "rescanning" state, re-entered every iteration by design — the loop
    /// never reuses an earlier pass's list.
    case discovering(iteration: UInt32)
    /// Loop step 4 finished.
    case discovered(RecoveryDiscoveredProgressDto)
    /// Loop steps 5-6: evidence is being re-collected and reclassified before
    /// anything is chosen. A display must not offer a confirmation affordance
    /// from a stale earlier pass while this is in flight.
    case revalidating(iteration: UInt32)
    /// Loop step 8: a real mutation is about to run.
    case actionStarted(RecoveryActionStartedProgressDto)
    /// Loop step 9: the mutation finished.
    case actionFinished(RecoveryActionFinishedProgressDto)
    /// The user's cooperative stop was observed — between actions, never during
    /// one. The run finishes with `stop_reason: "stopped_by_user"`; this event
    /// is what lets the UI stop offering the button before that arrives.
    case stopRequested(iteration: UInt32)

    private enum CodingKeys: String, CodingKey {
        case phase
        case iteration
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let phase = try container.decode(String.self, forKey: .phase)
        switch phase {
        case "measured":
            self = .measured(try RecoveryMeasuredProgressDto(from: decoder))
        case "discovering":
            self = .discovering(iteration: try container.decode(UInt32.self, forKey: .iteration))
        case "discovered":
            self = .discovered(try RecoveryDiscoveredProgressDto(from: decoder))
        case "revalidating":
            self = .revalidating(iteration: try container.decode(UInt32.self, forKey: .iteration))
        case "action_started":
            self = .actionStarted(try RecoveryActionStartedProgressDto(from: decoder))
        case "action_finished":
            self = .actionFinished(try RecoveryActionFinishedProgressDto(from: decoder))
        case "stop_requested":
            self = .stopRequested(iteration: try container.decode(UInt32.self, forKey: .iteration))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .phase,
                in: container,
                debugDescription: "Unknown RecoveryProgressEvent phase: \(phase)"
            )
        }
    }

    /// Which pass of the loop produced this event.
    ///
    /// Every variant carries it — Rust's own tests assert that — so this is a
    /// total function rather than an optional, and a live display can show the
    /// iteration without knowing which phase it is in.
    var iteration: UInt32 {
        switch self {
        case .measured(let payload): return payload.iteration
        case .discovering(let iteration): return iteration
        case .discovered(let payload): return payload.iteration
        case .revalidating(let iteration): return iteration
        case .actionStarted(let payload): return payload.iteration
        case .actionFinished(let payload): return payload.iteration
        case .stopRequested(let iteration): return iteration
        }
    }
}

/// Mirrors `reporting::dto::RecoveryRemainingReport` — what a run that ran out
/// of safe work left behind (HORO-1509).
///
/// Four counts rather than one total, because each is a different next step:
/// the user can say yes to the first, can only wait for the second, will
/// never be offered the third, and can have the fourth simply by starting the
/// run themselves. Counts of candidates, never bytes — space the
/// run was not permitted to take is not an opportunity, which is the same rule
/// `RecoveryOpportunityReportDto` splits its own totals for.
struct RecoveryRemainingReportDto: Decodable, Equatable {
    /// Reachable, real, and waiting for the user to say yes.
    let requiresConfirmationCount: UInt32
    /// Refused by policy on evidence. A later run refuses these again.
    let protectedCount: UInt32
    /// Past policy, but the offered action refuses to run against the resource
    /// as it currently stands — a live tool, work in progress. This one may
    /// well be available tomorrow.
    let notExecutableCount: UInt32
    /// Left alone only because *this* Autopilot run was not authorized to take
    /// it — an unallowed resource kind, or a budget already spent (HORO-1510).
    /// Always `0` for a run the user started, which has no Autopilot envelope
    /// to refuse anything.
    ///
    /// Kept apart from the three above because it says nothing about the
    /// resource: the other three are facts about what is on the disk, and this
    /// one is a fact about the run's authority. Folded into `protectedCount` it
    /// would tell a user something is off limits when it is one click away.
    let notPermittedByAutopilotCount: UInt32

    enum CodingKeys: String, CodingKey {
        case requiresConfirmationCount = "requires_confirmation_count"
        case protectedCount = "protected_count"
        case notExecutableCount = "not_executable_count"
        case notPermittedByAutopilotCount = "not_permitted_by_autopilot_count"
    }
}

/// Mirrors `reporting::dto::RecoveryRunReport` — the outcome of a real
/// recovery run, from `glomeris free --json`.
///
/// Every byte figure is measured, never estimated, and `targetMet` comes from
/// the final re-measured free space rather than from the sum of what was
/// deleted (campaign §9).
struct RecoveryRunReportDto: Decodable, Equatable {
    /// The goal the run worked toward, present whenever it was started from a
    /// used-percent goal. `nil` for the raw `--target` free-space floor, whose
    /// value is reported in `target` instead — so a client cannot read a
    /// free-space figure as a usage one.
    let goal: RecoveryGoalReportDto?
    /// How the target was expressed, always naming its axis: `"20% free"`,
    /// `"5 GiB free"`.
    let target: String
    /// Stable snake_case tag from `reporting::dto::stop_reason_tag`. Turned
    /// into words by `GlomerisVocabulary.stopReason`, which
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` keeps covering every
    /// token Rust can emit.
    ///
    /// Kept as a plain `String` rather than a Swift enum, like every other
    /// token in this file: a CLI newer than this app emitting an unknown stop
    /// reason must render as "unrecognised", not fail the whole decode and
    /// leave the user with no result at all.
    let stopReason: String
    /// One sentence explaining the stop reason. Never the bare word "Done" —
    /// Rust guarantees that, and the campaign requires it: a run that stopped
    /// because nothing safe was left has to say so.
    let stopReasonDetail: String
    let error: String?
    /// Which Autopilot limit ended the run, as the gate's own snake_case token
    /// (`action_budget_exhausted`, `byte_budget_exhausted`,
    /// `disk_pressure_too_low`, …). Present only when
    /// `stopReason == "envelope_refused"` (HORO-1510).
    ///
    /// A separate field from `stopReason` so that stop reason stays a small
    /// closed set, and read from here rather than by matching words out of
    /// `stopReasonDetail`, which is prose and will be reworded. Turned into
    /// words by `GlomerisVocabulary.autopilotRefusal`, which already covers
    /// these tokens for `glomeris autopilot`.
    let envelopeRefusal: String?
    /// What the run's last discovery pass looked at and left alone. Present
    /// when `stopReason == "safe_exhausted"`, and when
    /// `stopReason == "envelope_refused"` *after* a pass that did look: no
    /// other stop concluded anything about the candidates it never reached, so
    /// zeros there would be a claim the run did not make. An Autopilot run
    /// refused before discovery — revoked, or the machine below its pressure
    /// floor — omits it for the same reason. `nil` means "not stated", never
    /// "none left" (HORO-1509, HORO-1510).
    let remaining: RecoveryRemainingReportDto?
    let iterationsRun: UInt32
    let actionsExecuted: UInt32
    let actionsDeclinedOrSkipped: UInt32
    /// Sum of **actual** reclaimed bytes.
    let bytesFreedMeasured: UInt64
    let bytesFreedMeasuredHuman: String
    let startedFreeBytes: UInt64
    let startedFreeHuman: String
    let finalFreeBytes: UInt64
    let finalFreeHuman: String
    /// Whether the goal was satisfied by the final re-measured free space.
    let targetMet: Bool
    let detectorFailures: [String]
    let discoveryComplete: Bool
    let caveats: [String]

    enum CodingKeys: String, CodingKey {
        case goal
        case target
        case stopReason = "stop_reason"
        case stopReasonDetail = "stop_reason_detail"
        case error
        case envelopeRefusal = "envelope_refusal"
        case remaining
        case iterationsRun = "iterations_run"
        case actionsExecuted = "actions_executed"
        case actionsDeclinedOrSkipped = "actions_declined_or_skipped"
        case bytesFreedMeasured = "bytes_freed_measured"
        case bytesFreedMeasuredHuman = "bytes_freed_measured_human"
        case startedFreeBytes = "started_free_bytes"
        case startedFreeHuman = "started_free_human"
        case finalFreeBytes = "final_free_bytes"
        case finalFreeHuman = "final_free_human"
        case targetMet = "target_met"
        case detectorFailures = "detector_failures"
        case discoveryComplete = "discovery_complete"
        case caveats
    }
}

/// Mirrors `reporting::dto::RecoveryGoalRejectionReport` — a goal refused
/// before anything ran.
///
/// Printed on stdout with exit code 2, which is why the Recovery card calls
/// `GlomerisClient.runRaw` rather than `run`: `run` throws on a non-zero exit
/// and discards stdout, so the explanation of *why* a goal was refused would be
/// lost exactly when the user needs it. The same reasoning
/// `CandidateDetailView.describeExecuteOutcome` documents for `execute`.
struct RecoveryGoalRejectionReportDto: Decodable, Equatable {
    /// Stable snake_case tag from `executor::goal::GoalRejection::as_str`:
    /// `"not_finite"`, `"out_of_range"`, `"not_an_improvement"`. A plain
    /// `String` for the same forward-compatibility reason as `stopReason`
    /// above.
    let reason: String
    /// The rejection's own text, shown verbatim. Rust decided the refusal and
    /// Rust words it; this app does not re-explain a judgment it did not make.
    let message: String
    /// The goal that was asked for, on the used axis. `nil` when the value was
    /// not a usable number at all.
    let goalUsedPercent: Double?
    /// The usage the refusal was measured against, present only when the
    /// refusal was decided against an observation.
    let currentUsedPercent: Double?

    enum CodingKeys: String, CodingKey {
        case reason
        case message
        case goalUsedPercent = "goal_used_percent"
        case currentUsedPercent = "current_used_percent"
    }
}

/// Mirrors `reporting::dto::RecoverySettingsReport` — the two stored
/// preferences, as `glomeris settings show --json` prints them (HORO-1507).
///
/// Two percentages, about the same disk, meaning different things. They are
/// decoded into separately named fields and rendered from separately named
/// labels for that reason alone: an app that held them in one array, or showed
/// them under one heading, would be one off-by-one away from telling a user
/// their recovery goal is the point at which they will be warned.
struct RecoverySettingsReportDto: Decodable, Equatable {
    /// When to call the user's attention to disk usage, in percent USED.
    let notifyAtUsedPercent: Double
    /// e.g. `"75% used"`. Shown as-is; never reassembled from the number above,
    /// for the same reason `RecoveryGoalReportDto.description` is not.
    let notifyAtDescription: String
    /// Where recovery should stop. Carries its own axes and its own sentence,
    /// so the goal shown here and the goal shown on the Recovery card are one
    /// number worded one way.
    let defaultGoal: RecoveryGoalReportDto
    /// What each of the two numbers may be, so a control can be built that
    /// cannot ask for one the CLI refuses.
    let bounds: RecoverySettingsBoundsReportDto
    /// Absent when the CLI could not resolve `$HOME`.
    let storedAt: String?
    /// `false` means these are the built-in defaults and nothing has been
    /// stored. Reported rather than inferred from the values, because a user may
    /// legitimately store the default numbers, and "nothing chosen yet" is a
    /// different thing to show than "these were chosen".
    let loadedFromFile: Bool

    enum CodingKeys: String, CodingKey {
        case notifyAtUsedPercent = "notify_at_used_percent"
        case notifyAtDescription = "notify_at_description"
        case defaultGoal = "default_goal"
        case bounds
        case storedAt = "stored_at"
        case loadedFromFile = "loaded_from_file"
    }
}

/// Mirrors `reporting::dto::RecoverySettingsBoundsReport` — the limits each
/// setting is validated against.
///
/// Read rather than known, for the reason the Autopilot pane reads its
/// ceilings: a control bounded by this app's own idea of the limits eventually
/// offers a value the CLI refuses, and the refusal lands after the user pressed
/// Save.
///
/// Note what is absent: any expression of "the goal must be below the
/// threshold". That is not a bound on either number — it moves as the other one
/// moves — and a client that turned it into a range would be reimplementing the
/// validator instead of reading it. The pane learns that rule the only honest
/// way, from the refusal.
struct RecoverySettingsBoundsReportDto: Decodable, Equatable {
    let notifyAtMinimumUsedPercent: Double
    let notifyAtMaximumUsedPercent: Double
    let goalMinimumUsedPercent: Double
    let goalMaximumUsedPercent: Double

    enum CodingKeys: String, CodingKey {
        case notifyAtMinimumUsedPercent = "notify_at_minimum_used_percent"
        case notifyAtMaximumUsedPercent = "notify_at_maximum_used_percent"
        case goalMinimumUsedPercent = "goal_minimum_used_percent"
        case goalMaximumUsedPercent = "goal_maximum_used_percent"
    }
}

/// Mirrors `reporting::dto::SettingsRejectionReport` — a settings change
/// refused before anything was written.
///
/// Printed on stdout with exit code 2, so the pane reads it through
/// `GlomerisClient.runRaw` for the same reason the Recovery card does: `run`
/// throws on a non-zero exit and discards stdout, which would lose the
/// explanation exactly when the user needs it.
struct SettingsRejectionReportDto: Decodable, Equatable {
    /// Stable snake_case tag from `settings::SettingsRejection::as_str`. Turned
    /// into words by `GlomerisVocabulary.settingsRejection`, which
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` keeps covering every
    /// token Rust can emit. A plain `String` for the same
    /// forward-compatibility reason as `stopReason`.
    let reason: String
    /// The refusal's own text, shown verbatim. Rust decided it and Rust words
    /// it; this app does not re-explain a judgment it did not make.
    let message: String
    /// Present only when the refusal involved the threshold. Absent rather than
    /// zero, because `0.0` would be a number nobody supplied.
    let notifyAtUsedPercent: Double?
    /// Present only when the refusal involved the goal.
    let goalUsedPercent: Double?

    enum CodingKeys: String, CodingKey {
        case reason
        case message
        case notifyAtUsedPercent = "notify_at_used_percent"
        case goalUsedPercent = "goal_used_percent"
    }
}

/// Mirrors `reporting::dto::PressureEpisodeReport` (HORO-1508) — one run of
/// disk pressure, from the poll that crossed the user's threshold to whatever
/// they last said about it.
///
/// An episode is not a reading of the disk. It is the *conversation* about a
/// reading: opened once, notified about at most once per snooze, answered or
/// not. That distinction is why `latest_used_percent` and the volume figures in
/// `PressureStatusReportDto.current` are separate fields rather than one — the
/// episode's numbers are as of the daemon's last poll, `current` is as of this
/// call, and showing one where the other belongs tells the user about a disk
/// they no longer have.
struct PressureEpisodeReportDto: Decodable, Equatable, Identifiable {
    /// Monotonic per-episode id, never reused. Doubles as the notification
    /// identifier, so re-posting the same id updates the existing banner in
    /// place rather than stacking a second one — which is half of what keeps a
    /// hovering disk from producing a storm.
    let episodeId: UInt64
    var id: UInt64 { episodeId }
    let openedUnixSecs: UInt64
    /// Usage when the threshold was crossed, in percent USED.
    let openedUsedPercent: Double
    /// The worst reading seen in this episode, in percent USED. Reported
    /// separately from `latestUsedPercent` because a disk that peaked at 96% and
    /// is now at 88% is a different story from one that has sat at 88%.
    let peakUsedPercent: Double
    /// The most recent reading the daemon took, in percent USED.
    let latestUsedPercent: Double
    let latestFreeBytes: UInt64
    let latestFreeHuman: String
    /// When that reading was taken. Worth showing: a stale episode from a
    /// stopped daemon must not read as live.
    let latestUnixSecs: UInt64
    /// Whether a banner is owed *right now*. The one field a notifier acts on,
    /// and it is Rust's answer, not a condition to re-derive: a client
    /// recomputing it from a threshold and a percentage would get the snoozed
    /// and already-raised cases wrong, and the symptom is the notification storm
    /// HORO-1508 AC5 forbids.
    let notificationDue: Bool
    /// Banners the user could actually have seen, counted only when delivery was
    /// reported back through `pressure notified`.
    let notificationsRaised: UInt32
    /// Absent until the first banner has been reported as raised.
    let lastNotifiedUnixSecs: UInt64?
    /// Stable snake_case tag from `monitor::episode::EpisodeResponse::as_str`.
    /// Turned into words by `GlomerisVocabulary.episodeResponse`, which
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` keeps covering every
    /// token Rust can emit. A plain `String` for the same forward-compatibility
    /// reason as `stopReason`.
    ///
    /// Absent — not `"none"` — while the question is still open. The difference
    /// matters: one means the user has not answered, the other would claim they
    /// chose to do nothing.
    let response: String?
    let respondedUnixSecs: UInt64?
    /// When a "Remind me later" reminder comes due. Absent for the other two
    /// answers, which is the shape of "this one has a deadline and those do
    /// not".
    let snoozedUntilUnixSecs: UInt64?
    /// Whether the snooze is still running, *derived by the CLI against its own
    /// clock*. Read rather than computed from `snoozedUntilUnixSecs`, because
    /// comparing a stored deadline against this app's clock is how a snooze
    /// silently comes back early on a machine that slept.
    let isSnoozed: Bool

    enum CodingKeys: String, CodingKey {
        case episodeId = "episode_id"
        case openedUnixSecs = "opened_unix_secs"
        case openedUsedPercent = "opened_used_percent"
        case peakUsedPercent = "peak_used_percent"
        case latestUsedPercent = "latest_used_percent"
        case latestFreeBytes = "latest_free_bytes"
        case latestFreeHuman = "latest_free_human"
        case latestUnixSecs = "latest_unix_secs"
        case notificationDue = "notification_due"
        case notificationsRaised = "notifications_raised"
        case lastNotifiedUnixSecs = "last_notified_unix_secs"
        case response
        case respondedUnixSecs = "responded_unix_secs"
        case snoozedUntilUnixSecs = "snoozed_until_unix_secs"
        case isSnoozed = "is_snoozed"
    }
}

/// Mirrors `reporting::dto::PressureStatusReport` (HORO-1508) — everything the
/// app needs to decide whether to raise a pressure notification, and what to
/// put in it.
///
/// This DTO exists because the process that notices disk pressure cannot ask
/// the user about it. An actionable banner needs `UNUserNotificationCenter`,
/// which needs an app bundle; the monitor daemon is a bare launchd process. So
/// the daemon records that a notification is *owed* and this app raises it —
/// with every rule (when an episode opens, when hysteresis closes it, how long
/// a snooze lasts, whether a banner is owed) staying in Rust.
///
/// Four percentages arrive together here and every one of them names its axis.
/// They are four different things about the same disk: what it is now, when to
/// speak up, when the episode is over, and where recovery should stop.
struct PressureStatusReportDto: Decodable, Equatable {
    /// When to call attention, in percent USED — the user's monitoring setting.
    let notifyAtUsedPercent: Double
    /// e.g. `"85% used"`. Shown as-is, never reassembled from the number above.
    let notifyAtDescription: String
    /// The hysteresis boundary, in percent USED: at or below this the episode
    /// closes and a later crossing is a new episode. Reported so a user can be
    /// told why a disk at 83% is still in an episode when their threshold is
    /// 85%.
    let clearAtUsedPercent: Double
    /// How long "Remind me later" lasts.
    let snoozeSecs: UInt64
    /// The volume as measured by *this* call, not by the daemon's last poll.
    let current: StatusReportDto
    /// Whether `current` is at or above the threshold. Separate from
    /// `notificationDue` on purpose: a disk can be over the threshold with
    /// nothing owed (already raised, or snoozed), and inside the hysteresis gap
    /// this is `false` while an episode is still open.
    let thresholdCrossed: Bool
    /// Whether a banner is owed right now — the same field as on the episode,
    /// surfaced at the top level so a notifier need not reason about presence.
    /// `false` whenever there is no episode at all.
    let notificationDue: Bool
    /// Absent when disk usage has never crossed the threshold, or the last
    /// episode has cleared. Absent rather than a zeroed record, because "no
    /// episode" is a state with no numbers in it.
    let episode: PressureEpisodeReportDto?
    /// The goal a "Review & recover" deep link should open Recovery at, carried
    /// here so the notification and the Recovery card cannot disagree about
    /// where recovery is heading.
    let defaultGoal: RecoveryGoalReportDto
    /// The answers `pressure respond` accepts, in the order they should be
    /// offered. Read rather than hard-coded so the buttons on the banner are
    /// exactly what the CLI will take back; `GlomerisVocabulary.episodeResponse`
    /// supplies the wording for each.
    let responses: [String]
    /// Where the episode is persisted. Absent when the CLI could not resolve
    /// `$HOME` — which must not be read as "monitoring is off": the threshold
    /// and the answer set are still reported.
    let statePath: String?

    enum CodingKeys: String, CodingKey {
        case notifyAtUsedPercent = "notify_at_used_percent"
        case notifyAtDescription = "notify_at_description"
        case clearAtUsedPercent = "clear_at_used_percent"
        case snoozeSecs = "snooze_secs"
        case current
        case thresholdCrossed = "threshold_crossed"
        case notificationDue = "notification_due"
        case episode
        case defaultGoal = "default_goal"
        case responses
        case statePath = "state_path"
    }
}

/// Mirrors `reporting::dto::PressureRejectionReport` (HORO-1508) — an
/// acknowledgement or an answer the CLI would not record.
///
/// Printed on stdout with exit code 3, so the app reads it through
/// `GlomerisClient.runRaw` for the reason the Recovery card and the settings
/// pane do: `run` throws on a non-zero exit and discards stdout, losing the
/// explanation exactly when it is needed.
///
/// Neither refusal is a malfunction. Both mean the disk recovered between the
/// banner appearing and the button being pressed, which is good news worded as
/// a refusal — hence `GlomerisVocabulary.episodeRejection` renders them as news
/// about the disk rather than as errors.
struct PressureRejectionReportDto: Decodable, Equatable {
    /// Stable snake_case tag from `monitor::episode::EpisodeRejection::as_str`.
    /// Turned into words by `GlomerisVocabulary.episodeRejection`, which
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` keeps covering every
    /// token Rust can emit. A plain `String` for the same forward-compatibility
    /// reason as `stopReason`.
    let reason: String
    /// The refusal's own text. Rust decided it and Rust words it.
    let message: String

    /// A token Rust never emits, used when the CLI exited 3 without printing a
    /// report this app could decode — an older CLI refusing for a reason it has
    /// not published, or the non-`--json` path having been run by mistake.
    ///
    /// Deliberately not one of the real tokens: claiming `no_open_episode` for an
    /// unknown refusal would put a specific, confident sentence in front of the
    /// user about a state nothing established. Reserved rather than invented ad
    /// hoc at the call site so it can be asserted to stay outside the set the
    /// vocabulary recognises — `GlomerisVocabulary.episodeRejection` answers it
    /// through its `unrecognised` branch, which is the honest wording.
    static let unknownReason = "unrecognized_refusal"

    enum CodingKeys: String, CodingKey {
        case reason
        case message
    }
}

// MARK: - Workspace intelligence (HORO-1550)

/// Mirrors `reporting::dto::WorkspacePlanItemReport` — one row of the
/// contract-version-2 plan.
///
/// The shape worth preserving here is the nesting. `item` is the same
/// `LlmPlanItemReportDto` the version-1 plan carries, holding this machine's
/// own finding and its policy verdict; the four properties beside it are what
/// the model said about that finding. They are siblings rather than one flat
/// row because §17 of the campaign requires a surface to be unable to present
/// a local fact and a model inference as one thing, and a view that reads
/// `dto.item.policyLabel` from one value and `dto.disposition` from another
/// cannot merge them by accident.
///
/// `modelConfidence` is not called `confidence` for the same reason:
/// `item.confidence` is already the *evidence* confidence Rust computed, and
/// two properties of that name on one row is a misattribution waiting to
/// happen.
struct WorkspacePlanItemReportDto: Decodable, Equatable, Identifiable {
    let item: LlmPlanItemReportDto
    /// `"recommend_now"`, `"ask_user"`, `"defer"` or `"keep"` — the model's
    /// recommendation, which is never authority. A `"recommend_now"` on a row
    /// whose `item.candidate.executable` is `false`, or whose action is absent
    /// from `item.candidate.offeredActions`, still may not be acted on: that
    /// list remains the only enablement path, exactly as on a candidates-list
    /// row.
    let disposition: String
    /// `"observed"`, `"inferred"` or `"unknown"` — how the model rated its own
    /// claim. A plain `String` for the usual forward-compatibility reason: a
    /// resolved CLI newer than this app may name a value this build has no word
    /// for, and refusing to decode the whole plan over it would be worse than
    /// rendering it as unrecognised.
    let modelConfidence: String
    /// What the model said it could not settle. Shown rather than dropped —
    /// a recommendation whose caveats were edited out reads more certain than
    /// the model was.
    let uncertainties: [String]
    /// Opaque request-scoped aliases (`resource_1`, `workspace_1`, …) the model
    /// cited. Never a path, a repository name or an issue key — the aliases are
    /// all it was given.
    let evidenceRefs: [String]

    enum CodingKeys: String, CodingKey {
        case item
        case disposition
        case modelConfidence = "model_confidence"
        case uncertainties
        case evidenceRefs = "evidence_refs"
    }

    var id: String { item.resourceId }
}

/// Mirrors `reporting::dto::WorkspaceProfileReport` — the model's reading of
/// the workspace's shape, which is not the same thing as
/// `WorkflowProfileReportDto`'s locally measured baseline and must not be
/// rendered as though it were.
struct WorkspaceProfileReportDto: Decodable, Equatable {
    /// A `workspace::WorkflowMode` tag.
    let mode: String
    /// `"observed"`, `"inferred"` or `"unknown"`.
    let confidence: String
    let evidenceRefs: [String]
    let summary: String?

    enum CodingKeys: String, CodingKey {
        case mode
        case confidence
        case evidenceRefs = "evidence_refs"
        case summary
    }
}

/// Mirrors `reporting::dto::WorkspaceObservationReport` — something the model
/// noticed about the evidence as a whole rather than about one resource.
///
/// `kind` matters more than `detail` here: `"conflicting_evidence"` and
/// `"missing_evidence"` are the two AC 1 asks a user to be able to see, and
/// `detail` is `nil` whenever the model offered a kind without wording.
struct WorkspaceObservationReportDto: Decodable, Equatable {
    let kind: String
    let evidenceRefs: [String]
    let detail: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case evidenceRefs = "evidence_refs"
        case detail
    }
}

/// Mirrors `reporting::dto::WorkspaceEvidenceRequestReport` — a read-only probe
/// the model asked for and did not get to run itself.
///
/// `probeId` is a tag from a compiled registry and `subjectRef` is an alias
/// that already existed in the request. Neither is a command, a path or a URL,
/// and this app builds nothing from either: it renders them so a user can see
/// what the model wanted to know.
struct WorkspaceEvidenceRequestReportDto: Decodable, Equatable {
    let probeId: String
    let subjectRef: String
    let reason: String?

    enum CodingKeys: String, CodingKey {
        case probeId = "probe_id"
        case subjectRef = "subject_ref"
        case reason
    }
}

/// Mirrors `reporting::dto::WorkspacePlanDroppedReport` — every way a provider
/// response was refused, counted.
///
/// Surfaced rather than swallowed, for the reason `LlmPlanReportDto`'s two
/// counts are: a two-row plan built from a response whose other five rows were
/// rejected is not a two-row plan, and the rejections are the evidence that the
/// validation in Rust is the thing deciding what may be shown.
struct WorkspacePlanDroppedReportDto: Decodable, Equatable {
    let unknownResource: UInt32
    let unofferedAction: UInt32
    let unknownDisposition: UInt32
    let duplicateItem: UInt32
    let unknownObservationKind: UInt32
    let unknownProbe: UInt32
    let unknownProbeSubject: UInt32
    let incompatibleProbeSubject: UInt32
    let duplicateEvidenceRequest: UInt32
    let uncitedEvidenceRef: UInt32
    let degradedUnknownConfidence: UInt32
    let degradedUnknownWorkflowMode: UInt32
    let truncatedItems: UInt32
    let truncatedObservations: UInt32
    let truncatedEvidenceRequests: UInt32
    let truncatedUncertainties: UInt32
    let truncatedEvidenceRefs: UInt32

    /// `true` when the provider said anything Rust refused. The card shows the
    /// breakdown only then, because seventeen zeroes is noise.
    var anyDropped: Bool {
        unknownResource > 0 || unofferedAction > 0 || unknownDisposition > 0
            || duplicateItem > 0 || unknownObservationKind > 0 || unknownProbe > 0
            || unknownProbeSubject > 0 || incompatibleProbeSubject > 0
            || duplicateEvidenceRequest > 0 || uncitedEvidenceRef > 0
            || degradedUnknownConfidence > 0 || degradedUnknownWorkflowMode > 0
            || truncatedItems > 0 || truncatedObservations > 0
            || truncatedEvidenceRequests > 0 || truncatedUncertainties > 0
            || truncatedEvidenceRefs > 0
    }

    enum CodingKeys: String, CodingKey {
        case unknownResource = "unknown_resource"
        case unofferedAction = "unoffered_action"
        case unknownDisposition = "unknown_disposition"
        case duplicateItem = "duplicate_item"
        case unknownObservationKind = "unknown_observation_kind"
        case unknownProbe = "unknown_probe"
        case unknownProbeSubject = "unknown_probe_subject"
        case incompatibleProbeSubject = "incompatible_probe_subject"
        case duplicateEvidenceRequest = "duplicate_evidence_request"
        case uncitedEvidenceRef = "uncited_evidence_ref"
        case degradedUnknownConfidence = "degraded_unknown_confidence"
        case degradedUnknownWorkflowMode = "degraded_unknown_workflow_mode"
        case truncatedItems = "truncated_items"
        case truncatedObservations = "truncated_observations"
        case truncatedEvidenceRequests = "truncated_evidence_requests"
        case truncatedUncertainties = "truncated_uncertainties"
        case truncatedEvidenceRefs = "truncated_evidence_refs"
    }
}

/// Mirrors `reporting::dto::WorkspaceProbeFindingReport` — one probe that ran,
/// and what came back.
///
/// `unavailableReason` is why this type exists as more than a tag pair: a probe
/// that ran and found nothing and a probe that could not run are two different
/// facts, and `finding: "unavailable"` with a `tool_absent` reason must never be
/// rendered as the quiet former.
struct WorkspaceProbeFindingReportDto: Decodable, Equatable {
    let round: UInt32
    let probeId: String
    let subjectRef: String
    /// A `planner::contract::ProbeFindingView` tag, or `"unavailable"`.
    let finding: String
    /// An `evidence::ProbeReason` tag, set only for `"unavailable"`.
    let unavailableReason: String?

    enum CodingKeys: String, CodingKey {
        case round
        case probeId = "probe_id"
        case subjectRef = "subject_ref"
        case finding
        case unavailableReason = "unavailable_reason"
    }
}

/// Mirrors `reporting::dto::WorkspaceExpansionReport` — what the bounded
/// evidence loop actually did.
///
/// `converged` is the honest end state and `stoppedBecause` is why. A plan that
/// stopped at `"round_limit"` has open questions the model asked and nobody
/// answered, which a surface claiming completeness would hide.
struct WorkspaceExpansionReportDto: Decodable, Equatable {
    let roundsRun: UInt32
    let roundsAllowed: UInt32
    let probesRun: Int
    let probesAllowed: Int
    /// A `planner::expansion::StopReason` tag.
    let stoppedBecause: String
    /// `true` only for `"nothing_more_asked"` — the model ran out of questions
    /// rather than out of budget.
    let converged: Bool
    let findings: [WorkspaceProbeFindingReportDto]

    enum CodingKeys: String, CodingKey {
        case roundsRun = "rounds_run"
        case roundsAllowed = "rounds_allowed"
        case probesRun = "probes_run"
        case probesAllowed = "probes_allowed"
        case stoppedBecause = "stopped_because"
        case converged
        case findings
    }
}

/// Mirrors `reporting::dto::WorkspacePlanReport` — what
/// `llm-plan --contract-version 2 --json` prints.
///
/// `contractDeclared` is separate from `contractVersion` because a provider
/// that answered without naming the contract it was answering is a different
/// situation from one that named it: the response was still validated against
/// version 2, and the difference is worth showing rather than assuming.
///
/// Every key in this family is always present on the wire — the Rust side
/// carries no `skip_serializing_if` here — so an absent one is a mismatch this
/// app should fail on rather than quietly read as `nil`. The optionals below are
/// optional because Rust sends `null`, not because the key may be missing.
struct WorkspacePlanReportDto: Decodable, Equatable {
    let contractVersion: UInt32
    let contractDeclared: Bool
    let profile: WorkspaceProfileReportDto?
    let items: [WorkspacePlanItemReportDto]
    let observations: [WorkspaceObservationReportDto]
    let evidenceRequests: [WorkspaceEvidenceRequestReportDto]
    let dropped: WorkspacePlanDroppedReportDto
    /// `nil` when the plan ran in a single round with no expansion, which is
    /// the default. Not the same as an expansion that ran and found nothing.
    let expansion: WorkspaceExpansionReportDto?
    /// Non-`nil` when the provider call itself failed. The CLI still prints the
    /// whole report and exits 1, so the card reads it through `runRaw` for the
    /// same reason `LlmPlanReportDto` does.
    let providerError: String?

    enum CodingKeys: String, CodingKey {
        case contractVersion = "contract_version"
        case contractDeclared = "contract_declared"
        case profile
        case items
        case observations
        case evidenceRequests = "evidence_requests"
        case dropped
        case expansion
        case providerError = "provider_error"
    }

    /// Whether the reply carried anything at all besides suggestions.
    ///
    /// The version 1 contract had only items, so "no items" and "no answer"
    /// were one fact and the card said "No suggestions". Here they are two: a
    /// reply can propose nothing and still report a conflict, name a workflow
    /// shape, or ask for a branch probe — and each of those is the kind of thing
    /// HORO-1550 exists to put on screen. `dropped` is deliberately NOT counted,
    /// because a reply whose every suggestion was discarded proposed nothing
    /// usable and saying otherwise would dress validation failures up as
    /// findings.
    var saidSomethingBesidesItems: Bool {
        profile != nil
            || !observations.isEmpty
            || !evidenceRequests.isEmpty
            || expansion != nil
    }
}

/// Mirrors `reporting::dto::ExternalEgressFieldReport` — one field that may
/// reach a model, and every value it may carry.
struct ExternalEgressFieldReportDto: Decodable, Equatable, Identifiable {
    let source: String
    let field: String
    /// `"token"`, `"days"` or `"status"`.
    let shape: String
    /// Empty for `"days"`, whose range is the numbers rather than a set.
    let vocabulary: [String]

    var id: String { field }
}

/// Mirrors `reporting::dto::ExternalProviderPreviewReport` — one provider's
/// local setup state.
///
/// `configured`, `ready` and `refusal` are three properties rather than one
/// status because §10 of the campaign forbids folding them: a provider nobody
/// set up and a provider whose credential a service rejected must not present
/// as the same thing, and neither implies that no pull request or task exists.
struct ExternalProviderPreviewReportDto: Decodable, Equatable, Identifiable {
    let source: String
    /// Whether the configuration file names this provider at all.
    let configured: Bool
    /// Configured *and* its credential variable set — the only state in which
    /// anything is ever asked of it.
    let ready: Bool
    /// The name of the variable a credential is read from. Never its value.
    let credentialEnv: String?
    let endpoint: String?
    /// The git remote host whose working trees are in scope for this provider,
    /// or `nil` for one keyed by an explicit issue key instead.
    let repositoryHost: String?
    /// Why a configured provider is not ready, worded by Rust.
    let refusal: String?

    var id: String { source }

    enum CodingKeys: String, CodingKey {
        case source
        case configured
        case ready
        case credentialEnv = "credential_env"
        case endpoint
        case repositoryHost = "repository_host"
        case refusal
    }
}

/// Mirrors `reporting::dto::ExternalContextPreviewReport` — what
/// `external-context --json` prints.
///
/// Nothing in it was fetched. `neverSent` is the explicit negative and the
/// reason this is worth a screen at all: a list of what travels does not answer
/// "did you send my branch name", and only naming the absence does.
struct ExternalContextPreviewReportDto: Decodable, Equatable {
    /// `true` when at least one provider is configured. `false` is the state the
    /// product ships in, and then `egressFields` is empty because nothing is
    /// asked or sent.
    let enabled: Bool
    let configPath: String?
    /// Whether a file exists there — distinct from `enabled`, because a file
    /// that parses to nothing configured is not the same as no file.
    let configExists: Bool
    let configError: String?
    /// Every provider this product can have, configured or not, so the screen
    /// can say "not configured" rather than omit it.
    let providers: [ExternalProviderPreviewReportDto]
    let egressFields: [ExternalEgressFieldReportDto]
    let neverSent: [String]

    enum CodingKeys: String, CodingKey {
        case enabled
        case configPath = "config_path"
        case configExists = "config_exists"
        case configError = "config_error"
        case providers
        case egressFields = "egress_fields"
        case neverSent = "never_sent"
    }
}

/// Mirrors `reporting::dto::WorkflowSupportReport` — the counts a
/// `WorkflowProfileReportDto`'s mode was read off.
struct WorkflowSupportReportDto: Decodable, Equatable {
    let spanningDays: UInt32
    let repositoriesObserved: UInt32
    let parallelObservations: UInt32
    let serialObservations: UInt32
    let mixedObservations: UInt32
    let singleCheckoutBranchChanges: UInt32
    let mostWorktreesSeenAtOnce: UInt32

    enum CodingKeys: String, CodingKey {
        case spanningDays = "spanning_days"
        case repositoriesObserved = "repositories_observed"
        case parallelObservations = "parallel_observations"
        case serialObservations = "serial_observations"
        case mixedObservations = "mixed_observations"
        case singleCheckoutBranchChanges = "single_checkout_branch_changes"
        case mostWorktreesSeenAtOnce = "most_worktrees_seen_at_once"
    }
}

/// Mirrors `reporting::dto::WorkflowProfileReport` — what
/// `workflow-profile --json` prints: this machine's own measured baseline.
///
/// This is the historical half of AC 2, and the distinction the screen has to
/// keep is between it and current evidence. `mode: "unknown"` with
/// `confidence: "insufficient"` and `observationsStillNeeded: 1` is a
/// measurement that was taken and has an answer pending — a surface that
/// rendered it as "workflow: unknown" would be reporting it as though nothing
/// had been looked at.
///
/// `state` is a word rather than a bool for the same reason: `never_collected`,
/// `unreadable` and `collected` have three different next steps, and a
/// `hasHistory: Bool` would send somebody to re-run a recorder that is running
/// fine.
struct WorkflowProfileReportDto: Decodable, Equatable {
    /// `"never_collected"`, `"unreadable"` or `"collected"`.
    let state: String
    /// An `evidence::ProbeReason` tag, set only for `"unreadable"`.
    let unreadableReason: String?
    let storedAt: String?
    /// A `workspace::WorkflowMode` tag.
    let mode: String
    /// `"observed"` or `"insufficient"`.
    let confidence: String
    let observationCount: UInt32
    /// `0` once the minimum is met — present rather than omitted, because "you
    /// need none more" is what somebody who just met it is looking for.
    let observationsStillNeeded: UInt32
    let minimumIntervalSecs: UInt64
    let retentionDays: UInt64
    let support: WorkflowSupportReportDto
    /// What this baseline may and may not do, worded by Rust. Rendered verbatim:
    /// the app does not get to paraphrase a limit on its own authority.
    let authority: String

    enum CodingKeys: String, CodingKey {
        case state
        case unreadableReason = "unreadable_reason"
        case storedAt = "stored_at"
        case mode
        case confidence
        case observationCount = "observation_count"
        case observationsStillNeeded = "observations_still_needed"
        case minimumIntervalSecs = "minimum_interval_secs"
        case retentionDays = "retention_days"
        case support
        case authority
    }
}
