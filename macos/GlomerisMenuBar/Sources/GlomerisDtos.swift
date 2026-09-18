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
struct DetectCandidateReportDto: Decodable, Equatable {
    let resourceId: String
    let kind: String
    let reclaimableBytes: UInt64?
    let reclaimableHuman: String?
    let reclaimableBytesIsLowerBound: Bool
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
        case policyLabel = "policy_label"
        case reasons
        case executable
        case offeredActions = "offered_actions"
        case refusalReason = "refusal_reason"
    }
}

/// Mirrors `reporting::dto::DetectReport`.
struct DetectReportDto: Decodable, Equatable {
    let candidates: [DetectCandidateReportDto]
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

    enum CodingKeys: String, CodingKey {
        case actionId = "action_id"
        case resourceId = "resource_id"
        case outcome
        case failureMessage = "failure_message"
        case abortReason = "abort_reason"
        case expectedReclaimedBytes = "expected_reclaimed_bytes"
        case actualReclaimedBytes = "actual_reclaimed_bytes"
    }
}
