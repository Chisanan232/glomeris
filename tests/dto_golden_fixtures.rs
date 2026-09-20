//! HORO-1061: golden DTO JSON fixtures shared between the Rust CLI and the
//! Swift `GlomerisMenuBar` app's `Codable` mirrors.
//!
//! Each fixture under `tests/fixtures/dto/<name>.json` is hand-constructed
//! (not dumped from a live run) to match a `reporting::dto` type's
//! `Serialize` output field-for-field. This file asserts the Rust side of
//! the contract: constructing a real DTO instance and serializing it
//! reproduces the fixture byte-for-byte (after key-order-insensitive JSON
//! comparison — see the comment on `assert_matches_fixture` for why).
//!
//! `macos/GlomerisMenuBar/Tests/DtoGoldenFixturesTests.swift` asserts the
//! other half: decoding the same fixture files via hand-written Swift
//! `Codable` mirrors of these same DTOs.
//!
//! AC (HORO-1061): renaming/removing a Rust DTO field without updating the
//! fixture must fail here; renaming/removing a Swift field without
//! updating the fixture must fail there. See the PR description for the
//! scratch experiment that proved both directions.
//!
//! Scope: not every DTO in `reporting::dto` has a fixture yet — only the
//! ones covering what the Swift app already needs to decode (nothing
//! concrete yet — `GlomerisClient` is fully generic, see HORO-1060) plus
//! the structurally interesting shapes the upcoming C4-C8 screens will
//! need first: `StatusReport` (flat scalars), `DaemonStatusReport`
//! (`Option<u64>`), `HistoryReport` (nested array), `DetectReport` (nested
//! array of a DTO with the `executable`/`offered_actions`/`refusal_reason`
//! triple, mixing populated and empty/null variants), and `ExecuteReport`
//! (the `outcome` enum-as-`&'static str` field plus several optionals).
//! `ExplainReport`, `ActionListReport`, `CleanDryRunReport`, and
//! `ProgressEvent` remain out of scope for this ticket — none of them back a
//! currently-planned screen yet.
//! `ActionHistoryReport` was added in HORO-1066, when the recent-history +
//! action-audit popover section actually needed it — see that ticket's
//! `HistoryAuditSectionView.swift`. `LlmPlanReport` was added in HORO-1308,
//! for the same reason: `AiPlanSectionView.swift` decodes it.

use std::path::PathBuf;

use glomeris::reporting::dto::{
    ActionHistoryEventReport, ActionHistoryReport, DaemonStatusReport, DetectCandidateReport,
    DetectReport, ExecuteReport, HistoryEventReport, HistoryReport, LlmPlanItemReport,
    LlmPlanReport, OfferedAction, StatusReport,
};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dto")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Compares two JSON documents by parsed value rather than raw text.
/// `serde_json::to_string_pretty` on a plain `struct` preserves field
/// declaration order (not alphabetical), so byte-for-byte comparison
/// against a hand-authored fixture would silently depend on remembering
/// that ordering exactly whenever a fixture is hand-edited. Comparing
/// parsed `serde_json::Value`s instead is robust to whitespace/formatting
/// differences and key order, while still failing on any renamed, added,
/// missing, or differently-typed/valued field — exactly what the AC
/// requires ("changing a DTO field without updating the fixture fails the
/// Rust test").
fn assert_matches_fixture(actual: &impl serde::Serialize, fixture_name: &str) {
    let actual_value = serde_json::to_value(actual).expect("serialize DTO to Value");
    let fixture_text = read_fixture(fixture_name);
    let expected_value: serde_json::Value = serde_json::from_str(&fixture_text)
        .unwrap_or_else(|e| panic!("parse fixture {fixture_name}: {e}"));
    assert_eq!(
        actual_value, expected_value,
        "{fixture_name}: serialized DTO does not match the golden fixture"
    );
}

#[test]
fn status_report_matches_golden_fixture() {
    let report = StatusReport {
        total_bytes: 500_000_000_000,
        free_bytes: 125_000_000_000,
        used_percent: 75.0,
        free_human: "116.4 GB".to_string(),
        total_human: "465.7 GB".to_string(),
        pressure_state: "WARN",
    };
    assert_matches_fixture(&report, "status_report.json");
}

#[test]
fn daemon_status_report_matches_golden_fixture() {
    let report = DaemonStatusReport {
        plist_installed: true,
        plist_path: "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist".to_string(),
        loaded: true,
        heartbeat_age_secs: Some(42),
    };
    assert_matches_fixture(&report, "daemon_status_report.json");
}

#[test]
fn daemon_status_report_with_no_heartbeat_matches_golden_fixture() {
    let report = DaemonStatusReport {
        plist_installed: false,
        plist_path: "/Users/dev/Library/LaunchAgents/dev.glomeris.daemon.plist".to_string(),
        loaded: false,
        heartbeat_age_secs: None,
    };
    assert_matches_fixture(&report, "daemon_status_report_no_heartbeat.json");
}

#[test]
fn history_report_matches_golden_fixture() {
    let report = HistoryReport {
        events: vec![
            HistoryEventReport {
                unix_time_secs: 1_700_000_000,
                from: "OK".to_string(),
                to: "WARN".to_string(),
                used_percent: 82.5,
                free_bytes: 80_000_000_000,
                free_human: "74.5 GB".to_string(),
            },
            HistoryEventReport {
                unix_time_secs: 1_700_000_600,
                from: "WARN".to_string(),
                to: "CRITICAL".to_string(),
                used_percent: 95.1,
                free_bytes: 20_000_000_000,
                free_human: "18.6 GB".to_string(),
            },
        ],
    };
    assert_matches_fixture(&report, "history_report.json");
}

#[test]
fn detect_report_matches_golden_fixture() {
    let report = DetectReport {
        candidates: vec![
            DetectCandidateReport {
                resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
                kind: "cargo_target_dir",
                reclaimable_bytes: Some(2_147_483_648),
                reclaimable_human: Some("2.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: false,
                // 2 GiB clears the 1 GiB notable floor but not the 10 GiB
                // large one. Deliberately paired with AUTO_SAFE here, and
                // with UNKNOWN_INCOMPLETE below, so the fixture exercises
                // the fact that the impact and safety axes vary
                // independently (HORO-1307).
                impact_tier: "notable",
                policy_label: "AUTO_SAFE",
                reasons: vec!["no_active_use_observed"],
                executable: true,
                offered_actions: vec![OfferedAction {
                    action_id: "cargo.clean.target_dir".to_string(),
                    requires_confirmation: false,
                }],
                refusal_reason: None,
            },
            DetectCandidateReport {
                resource_id: "docker_build_cache:docker".to_string(),
                kind: "docker_build_cache",
                reclaimable_bytes: Some(10_737_418_240),
                reclaimable_human: Some("10.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: true,
                impact_tier: "large",
                policy_label: "UNKNOWN_INCOMPLETE",
                reasons: vec!["evidence_incomplete"],
                executable: false,
                offered_actions: vec![],
                refusal_reason: Some(
                    "no registered cleanup action for this resource kind".to_string(),
                ),
            },
        ],
    };
    assert_matches_fixture(&report, "detect_report.json");
}

#[test]
fn execute_report_succeeded_matches_golden_fixture() {
    let report = ExecuteReport {
        action_id: "cargo.clean.target_dir",
        resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
        outcome: "succeeded",
        failure_message: None,
        abort_reason: None,
        expected_reclaimed_bytes: Some(2_147_483_648),
        actual_reclaimed_bytes: Some(2_147_483_648),
    };
    assert_matches_fixture(&report, "execute_report_succeeded.json");
}

#[test]
fn execute_report_aborted_by_revalidation_matches_golden_fixture() {
    let report = ExecuteReport {
        action_id: "cargo.clean.target_dir",
        resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
        outcome: "aborted_by_revalidation",
        failure_message: None,
        abort_reason: Some("ResourceIdentityChanged".to_string()),
        expected_reclaimed_bytes: Some(2_147_483_648),
        actual_reclaimed_bytes: None,
    };
    assert_matches_fixture(&report, "execute_report_aborted.json");
}

/// HORO-1308. The fixture deliberately holds all three shapes the AI Plan
/// card has to render, and pairs them the awkward way round:
///
/// 1. an AUTO_SAFE item the model explained and that really is executable;
/// 2. an ASK item the model gave NO rationale for, whose offered action
///    still requires confirmation — so the card cannot use "the model
///    explained it" as a proxy for "this is fine";
/// 3. a PROTECTED item the model confidently recommended deleting, with a
///    plausible-sounding rationale, no `requested_action_id`, an empty
///    `offered_actions` and `executable: false`.
///
/// Item 3 is the one that matters: it is the wire-level form of the
/// end-to-end proof in `tests/golden_llm_plan_protected_refusal.rs`, and it
/// is what `DtoGoldenFixturesTests` asserts the Swift mirror decodes without
/// losing the refusal. A Swift model that dropped `refusal_reason`, or typed
/// `executable` as an Optional defaulting to `true`, fails there.
///
/// The `dropped_*` counts are non-zero on purpose: they are the only record
/// that the model asked for things that do not exist, and a surface that
/// silently ignores them tells the user a plan was complete when it was not.
#[test]
fn llm_plan_report_matches_golden_fixture() {
    let report = LlmPlanReport {
        items: vec![
            LlmPlanItemReport {
                resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
                policy_label: "AUTO_SAFE",
                requested_action_id: Some("cargo.clean.target_dir"),
                priority: Some(1),
                model_reason: Some("Largest build output and nothing is using it.".to_string()),
                explain: Some("remove the Cargo target directory for this project".to_string()),
                skip_reason: None,
                candidate: DetectCandidateReport {
                    resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
                    kind: "cargo_target_dir",
                    reclaimable_bytes: Some(2_147_483_648),
                    reclaimable_human: Some("2.0 GB".to_string()),
                    reclaimable_bytes_is_lower_bound: false,
                    impact_tier: "notable",
                    policy_label: "AUTO_SAFE",
                    reasons: vec!["no_active_use_observed"],
                    executable: true,
                    offered_actions: vec![OfferedAction {
                        action_id: "cargo.clean.target_dir".to_string(),
                        requires_confirmation: false,
                    }],
                    refusal_reason: None,
                },
                completeness: "complete",
                confidence: "high",
            },
            LlmPlanItemReport {
                resource_id: "node_modules:/Users/dev/proj/node_modules".to_string(),
                policy_label: "ASK",
                requested_action_id: Some("node.remove.node_modules"),
                priority: Some(2),
                model_reason: None,
                explain: Some("remove node_modules for this project".to_string()),
                skip_reason: None,
                candidate: DetectCandidateReport {
                    resource_id: "node_modules:/Users/dev/proj/node_modules".to_string(),
                    kind: "node_modules",
                    reclaimable_bytes: Some(536_870_912),
                    reclaimable_human: Some("512.0 MB".to_string()),
                    reclaimable_bytes_is_lower_bound: false,
                    impact_tier: "normal",
                    policy_label: "ASK",
                    reasons: vec!["regenerable_by_tool"],
                    executable: true,
                    offered_actions: vec![OfferedAction {
                        action_id: "node.remove.node_modules".to_string(),
                        requires_confirmation: true,
                    }],
                    refusal_reason: None,
                },
                completeness: "partial",
                confidence: "medium",
            },
            LlmPlanItemReport {
                resource_id: "cargo_target_dir:/Users/dev/.ssh/id_ed25519".to_string(),
                policy_label: "PROTECTED",
                requested_action_id: None,
                priority: Some(3),
                model_reason: Some("looks like a stale build directory".to_string()),
                explain: None,
                skip_reason: Some("PROTECTED: protected_credential_material".to_string()),
                candidate: DetectCandidateReport {
                    resource_id: "cargo_target_dir:/Users/dev/.ssh/id_ed25519".to_string(),
                    kind: "cargo_target_dir",
                    reclaimable_bytes: Some(1024),
                    reclaimable_human: Some("1.0 KB".to_string()),
                    reclaimable_bytes_is_lower_bound: false,
                    impact_tier: "normal",
                    policy_label: "PROTECTED",
                    reasons: vec!["protected_credential_material"],
                    executable: false,
                    offered_actions: vec![],
                    refusal_reason: Some("PROTECTED: protected_credential_material".to_string()),
                },
                completeness: "complete",
                confidence: "high",
            },
        ],
        dropped_unknown_resource: 1,
        dropped_unknown_action: 2,
        provider_error: None,
    };
    assert_matches_fixture(&report, "llm_plan_report.json");
}

#[test]
fn action_history_report_matches_golden_fixture() {
    let report = ActionHistoryReport {
        events: vec![
            ActionHistoryEventReport {
                timestamp: 1_700_000_000,
                action_id: "cargo.clean.target_dir".to_string(),
                resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
                policy_label: "AUTO_SAFE".to_string(),
                outcome: "succeeded".to_string(),
                abort_reason: None,
                actual_reclaimed_bytes: Some(2_147_483_648),
                actual_reclaimed_human: Some("2.0 GB".to_string()),
                source: "execute".to_string(),
            },
            ActionHistoryEventReport {
                timestamp: 1_700_000_600,
                action_id: "docker.clean.build_cache".to_string(),
                resource_id: "docker_build_cache:docker".to_string(),
                policy_label: "ASK".to_string(),
                outcome: "aborted_by_revalidation".to_string(),
                abort_reason: Some("ResourceIdentityChanged".to_string()),
                actual_reclaimed_bytes: None,
                actual_reclaimed_human: None,
                source: "free".to_string(),
            },
        ],
    };
    assert_matches_fixture(&report, "action_history_report.json");
}
