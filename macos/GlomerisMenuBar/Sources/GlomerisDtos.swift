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
//  output exactly (both sides use plain snake_case field names with no
//  `#[serde(rename...)]` on the Rust side, so no `CodingKeys` remapping is
//  needed here either).
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

/// Mirrors `reporting::dto::DetectReport`.
struct DetectReportDto: Decodable, Equatable {
    let candidates: [DetectCandidateReportDto]
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
    case detectorFinished(detector: String, candidatesFound: Int)

    private enum CodingKeys: String, CodingKey {
        case phase
        case detector
        case candidatesFound = "candidates_found"
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
            self = .detectorFinished(detector: detector, candidatesFound: candidatesFound)
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
