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

/// Mirrors `reporting::dto::DetectorHealthReport` (HORO-1484) — one
/// detector's outcome from the discovery pass that produced this report.
///
/// `status` is one of `"found"`, `"tool_absent"`, `"failed"`, produced by
/// `cli::DetectorOutcome::tag()`. The three are NOT interchangeable and the
/// app must not collapse them: `tool_absent` means the tool that would
/// produce candidates is not installed, which is normal state and a real
/// answer; `failed` means the probe did not answer, so whatever that
/// detector would have found is unknown. `reason` is present only for
/// `failed`.
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
    var failedDetectors: [DetectorHealthReportDto] {
        detectors.filter(\.didFail)
    }

    enum CodingKeys: String, CodingKey {
        case candidates
        case detectors
        case discoveryComplete = "discovery_complete"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        candidates = try container.decode([DetectCandidateReportDto].self, forKey: .candidates)
        detectors =
            try container.decodeIfPresent([DetectorHealthReportDto].self, forKey: .detectors) ?? []
        discoveryComplete =
            try container.decodeIfPresent(Bool.self, forKey: .discoveryComplete) ?? true
    }

    /// Non-decoding initializer for tests and previews.
    init(
        candidates: [DetectCandidateReportDto],
        detectors: [DetectorHealthReportDto] = [],
        discoveryComplete: Bool = true
    ) {
        self.candidates = candidates
        self.detectors = detectors
        self.discoveryComplete = discoveryComplete
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
    /// said about either. `outcome` is `"found"`/`"tool_absent"`/`"failed"`;
    /// `reason` is present only for `"failed"`.
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
