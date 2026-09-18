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
//! `ExplainReport`, `ActionListReport`, `LlmPlanReport`,
//! `CleanDryRunReport`, and `ProgressEvent` remain out of scope for this
//! ticket — none of them back a currently-planned screen yet.
//! `ActionHistoryReport` was added in HORO-1066, when the recent-history +
//! action-audit popover section actually needed it — see that ticket's
//! `HistoryAuditSectionView.swift`.

use std::path::PathBuf;

use glomeris::reporting::dto::{
    ActionHistoryEventReport, ActionHistoryReport, DaemonStatusReport, DetectCandidateReport,
    DetectReport, ExecuteReport, HistoryEventReport, HistoryReport, OfferedAction, StatusReport,
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
