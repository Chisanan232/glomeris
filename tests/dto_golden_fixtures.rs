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
//! `LlmCheckReport` and `LlmPayloadReport` were added in HORO-1309, when
//! `AiProviderPreferencesView.swift` started decoding both.
//! The recovery reports were added in HORO-1506 and the settings reports in
//! HORO-1507, when `RecoverySectionView.swift` and
//! `RecoveryPreferencesView.swift` started decoding them.
//! The pressure reports were added in HORO-1508, when the app became the
//! process that raises the pressure notification the daemon can only record.
//! Three status fixtures rather than one: the shape a client acts on is mostly
//! made of optionals, and "absent" is a meaning of its own on this surface.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use glomeris::actions::llm::{llm_check_outcome, LlmError, API_STYLE_CHAT_COMPLETIONS};
use glomeris::autopilot::RefusalReason;
use glomeris::cli::pressure::{build_pressure_rejection_report, build_pressure_status_report};
use glomeris::cli::recovery::{
    build_goal_rejection_report, build_recovery_preview_report, build_recovery_run_report,
};
use glomeris::cli::settings::{build_settings_rejection_report, build_settings_report};
use glomeris::evidence::{ProbeOutcome, ProbeReason};
use glomeris::executor::goal::RecoveryGoal;
use glomeris::executor::recovery_loop::{
    FreeTarget, RecoveryReport, RemainingCandidates, StopReason,
};
use glomeris::monitor::config::ThresholdConfig;
use glomeris::monitor::episode::{EpisodeConfig, EpisodeResponse, EpisodeTracker};
use glomeris::monitor::fs_stat::FsUsage;
use glomeris::reporting::dto::{
    ActionHistoryEventReport, ActionHistoryReport, AutopilotAskPreauthorizationReport,
    AutopilotCeilingsReport, AutopilotEnvelopeReport, DaemonStatusReport, DetectCandidateReport,
    DetectReport, DetectorHealthReport, ExecuteReport, HistoryEventReport, HistoryReport,
    LlmCheckReport, LlmPayloadReport, LlmPayloadResourceAlias, LlmPlanItemReport, LlmPlanReport,
    OfferedAction, StatusReport, WorkspaceEvidenceRequestReport, WorkspaceExpansionReport,
    WorkspaceFamilyReport, WorkspaceObservationReport, WorkspacePlanDroppedReport,
    WorkspacePlanItemReport, WorkspacePlanReport, WorkspaceProbeFindingReport,
    WorkspaceProfileReport,
};
use glomeris::reporting::PolicyLabel;
use glomeris::settings::RecoverySettings;
use glomeris::workspace::{
    ActivityState, Divergence, EquivalenceMethod, IntegrationEvidence, MergedState,
    PatchEquivalence, UpstreamState, WorkspaceFamily, WorkspaceMember, WorkspaceWorktree,
    WorktreeBranchState,
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
        // All four outcomes in one fixture (HORO-1484, extended by
        // HORO-1544), because the shape that matters is the one a consumer
        // has to tell apart: a detector that found nothing and a detector
        // that failed differ only in `status`, and `reason` is the field that
        // says why. Pinning them together is what stops `failed` from quietly
        // serializing as something a caller would read as a clean result.
        //
        // `tool_not_running` is the fourth, and the one a client is most
        // likely to get wrong: it carries no reason, exactly like
        // `tool_absent`, so the two are distinguishable by `status` alone.
        // A client collapsing them would tell a developer Docker is not
        // installed while gigabytes of its images sit on the disk unseen.
        detectors: vec![
            DetectorHealthReport {
                detector: "cargo_target_dir".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "docker_build_cache".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "node_modules".to_string(),
                status: "tool_absent",
                candidates_found: 0,
                reason: None,
            },
            DetectorHealthReport {
                detector: "docker_objects".to_string(),
                status: "tool_not_running",
                candidates_found: 0,
                reason: None,
            },
            DetectorHealthReport {
                detector: "project_roots".to_string(),
                status: "failed",
                candidates_found: 0,
                reason: Some("permission denied reading /Users/dev/private".to_string()),
            },
        ],
        // False because of `project_roots` above: two candidates were
        // found, and the list they are in is still not the whole picture.
        discovery_complete: false,
        // Empty on purpose, and the fixture has no `workspaces` key at all:
        // the field is `skip_serializing_if = "Vec::is_empty"`, so this
        // pins the shape a caller that did not group produces. The
        // populated shape is a fixture of its own — see
        // `detect_report_with_workspaces_matches_golden_fixture`, and the
        // note above about "absent" being a meaning of its own here.
        workspaces: vec![],
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
        // Via `human_bytes` rather than a literal, so this fixture cannot
        // assert a conversion the product does not perform — which is the
        // mistake the history fixture made ("524.3 MB" for a 1024-based
        // 524_288_000, HORO-1310).
        expected_reclaimed_human: Some(glomeris::reporting::human_bytes(2_147_483_648)),
        actual_reclaimed_human: Some(glomeris::reporting::human_bytes(2_147_483_648)),
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
        expected_reclaimed_human: Some(glomeris::reporting::human_bytes(2_147_483_648)),
        // An abort reclaimed nothing because it deleted nothing, which is not
        // the same claim as "0 B". Pinned as null so a client rendering this
        // row cannot report a successful zero-byte cleanup.
        actual_reclaimed_human: None,
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
                // The item-level sentence `build_llm_plan_report` actually
                // emits for a PROTECTED resource, verbatim — distinct from the
                // candidate's own `refusal_reason` below, which is
                // `executable_fields`' `PROTECTED: <reason code>` form. A
                // fixture holding a string its producer cannot produce is not a
                // contract, so both are quoted from the source rather than
                // paraphrased.
                skip_reason: Some(
                    "PROTECTED — no cleanup action is ever rendered for this resource".to_string(),
                ),
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

/// HORO-1550. The version 2 plan, whose rows the Workspace Intelligence surface
/// has to render without ever merging the model's request into this machine's
/// verdict.
///
/// The two rows are paired the awkward way round on purpose, and it is a
/// different awkwardness from the version 1 fixture's:
///
/// 1. a row this machine classified `AUTO_SAFE` and the model asked to
///    `defer` — so a surface cannot read `disposition` as a stronger form of
///    `policy_label`, because here it is a weaker one;
/// 2. a row this machine classified `ASK`, whose only offered action
///    `requires_confirmation`, and which the model asked to `recommend_now`
///    with `model_confidence: "unknown"` — so a surface cannot read
///    `recommend_now` as authority to offer one click, and cannot read the
///    model's own admission of ignorance as an absence of doubt.
///
/// There is deliberately **no `PROTECTED` row**, which is the version 1
/// fixture's item 3. A version 2 response naming an action for a protected
/// resource is dropped before a row is built — `cli::tests::
/// build_workspace_plan_report_never_renders_a_row_for_a_protected_resource` —
/// so a fixture carrying one would be a shape its producer cannot produce. What
/// happened to it is `dropped.unoffered_action: 1` instead, and that is the
/// point of the counters: the protected resource's absence from `items` is only
/// readable there.
///
/// `evidence_refs` hold wire aliases (`resource_1`) while every `resource_id`
/// holds the real local id. Both are true at once and the asymmetry is load
/// bearing: the model cited the only names it was given, and the row names the
/// resource on this disk. A Swift mirror that tried to join the two directly
/// would find nothing.
///
/// `expansion` is present, reports a run that stopped at its round ceiling, and
/// includes one `unavailable` finding — the three facts a surface must not round
/// off. `converged: false` beside `rounds_run == rounds_allowed` is the shape of
/// a plan formed without evidence somebody asked for, and the `unavailable`
/// finding carries a `reason` rather than an answer of `false`.
#[test]
fn workspace_plan_report_matches_golden_fixture() {
    let report = WorkspacePlanReport {
        contract_version: 2,
        contract_declared: true,
        profile: Some(WorkspaceProfileReport {
            mode: "mixed",
            confidence: "inferred",
            evidence_refs: vec!["workflow_history".to_string()],
            summary: Some(
                "Some repositories are used one branch at a time and one keeps several \
                 working trees."
                    .to_string(),
            ),
        }),
        items: vec![
            WorkspacePlanItemReport {
                item: LlmPlanItemReport {
                    resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
                    policy_label: "AUTO_SAFE",
                    requested_action_id: Some("cargo.clean.target_dir"),
                    priority: Some(1),
                    model_reason: Some(
                        "Large, but a build may be running and the probe did not say.".to_string(),
                    ),
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
                // Weaker than the policy verdict, not stronger.
                disposition: "defer",
                model_confidence: "inferred",
                uncertainties: vec!["whether a cargo build is running right now".to_string()],
                evidence_refs: vec!["resource_1".to_string()],
            },
            WorkspacePlanItemReport {
                item: LlmPlanItemReport {
                    resource_id: "node_modules:/Users/dev/proj/node_modules".to_string(),
                    policy_label: "ASK",
                    requested_action_id: Some("node.remove.node_modules"),
                    priority: Some(2),
                    model_reason: Some("Regenerable from the lockfile.".to_string()),
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
                // The sharp one: the model wants this done now and says it does
                // not know how well-founded that is.
                disposition: "recommend_now",
                model_confidence: "unknown",
                // Non-empty here and single-element above, because an empty
                // list is the one value a surface must not render as
                // reassurance — see `WorkspacePlanItemReport::uncertainties`.
                uncertainties: vec![
                    "whether the lockfile still resolves".to_string(),
                    "no pull request state was available".to_string(),
                ],
                evidence_refs: vec!["resource_2".to_string(), "workflow_history".to_string()],
            },
        ],
        observations: vec![
            WorkspaceObservationReport {
                kind: "conflicting_evidence",
                evidence_refs: vec!["workspace_1".to_string()],
                detail: Some(
                    "A working tree reports no upstream and also reports commits that are \
                     merged."
                        .to_string(),
                ),
            },
            // `detail: null` on purpose: the model named a kind and said
            // nothing more, and a surface has to show the kind rather than
            // drop a wordless observation.
            WorkspaceObservationReport {
                kind: "missing_evidence",
                evidence_refs: vec!["machine".to_string()],
                detail: None,
            },
        ],
        evidence_requests: vec![WorkspaceEvidenceRequestReport {
            probe_id: "github_pr_state",
            subject_ref: "workspace_1".to_string(),
            reason: Some("is anything open against this branch".to_string()),
        }],
        dropped: WorkspacePlanDroppedReport {
            // The protected resource the model asked to delete.
            unoffered_action: 1,
            // A resource id that was never issued, and a probe this build does
            // not implement: the two halves of "the model asked for things that
            // do not exist", which a surface reporting only `items.len()` hides.
            unknown_resource: 1,
            unknown_probe: 1,
            degraded_unknown_confidence: 1,
            truncated_uncertainties: 2,
            ..WorkspacePlanDroppedReport::default()
        },
        expansion: Some(WorkspaceExpansionReport {
            rounds_run: 3,
            rounds_allowed: 3,
            probes_run: 2,
            probes_allowed: 12,
            stopped_because: "round_limit",
            // A ceiling, and never to be rendered as agreement.
            converged: false,
            findings: vec![
                WorkspaceProbeFindingReport {
                    round: 2,
                    probe_id: "git_branch_state",
                    subject_ref: "workspace_1".to_string(),
                    finding: "branch_state",
                    unavailable_reason: None,
                },
                WorkspaceProbeFindingReport {
                    round: 3,
                    probe_id: "github_pr_state",
                    subject_ref: "workspace_1".to_string(),
                    finding: "unavailable",
                    // Not an answer of "no pull request": the provider was
                    // never configured, which is a different fact and the one
                    // §10 forbids collapsing.
                    unavailable_reason: Some("not_attempted"),
                },
            ],
        }),
        provider_error: None,
    };
    assert_matches_fixture(&report, "workspace_plan_report.json");
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
                model_rank: None,
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
                model_rank: None,
            },
            // HORO-1310. A third event only because this is the one shape the
            // other two cannot express: `source` naming which authority ran
            // the action (an Autopilot run, not a command the user typed) and
            // `model_rank` recording that a model put this resource first.
            // Both are separate axes from `policy_label` — a model's
            // suggestion never reaches `classify`, so AUTO_SAFE here is still
            // policy's own verdict.
            ActionHistoryEventReport {
                timestamp: 1_700_001_200,
                action_id: "node.clean.node_modules".to_string(),
                resource_id: "node_modules:/Users/dev/proj/node_modules".to_string(),
                policy_label: "AUTO_SAFE".to_string(),
                outcome: "succeeded".to_string(),
                abort_reason: None,
                actual_reclaimed_bytes: Some(524_288_000),
                // `human_bytes` is 1024-based, so 524_288_000 bytes is
                // "500.0 MB", not the 1000-based "524.3 MB" this fixture
                // carried first. A golden fixture is a claim about what the
                // product emits; a pair of fields that disagree teaches a
                // client reading it the wrong conversion.
                actual_reclaimed_human: Some(glomeris::reporting::human_bytes(524_288_000)),
                source: "autopilot_auto_safe".to_string(),
                model_rank: Some(1),
            },
        ],
    };
    assert_matches_fixture(&report, "action_history_report.json");
}

/// HORO-1309. Two fixtures rather than one, for the same reason
/// `ExecuteReport` has two: the type's interest is entirely in which of its
/// optionals is populated. `"ok"` carries a `response_excerpt` and no
/// `error`; every other outcome carries an `error` and no excerpt, and a
/// single fixture would leave one of those two directions unasserted on both
/// sides of the contract.
#[test]
fn llm_check_report_ok_matches_golden_fixture() {
    let report = LlmCheckReport {
        outcome: llm_check_outcome(None),
        model: "gpt-4o-mini".to_string(),
        endpoint_path: "/v1/chat/completions".to_string(),
        error: None,
        response_excerpt: Some("ok".to_string()),
    };
    assert_matches_fixture(&report, "llm_check_report_ok.json");
}

#[test]
fn llm_check_report_rejected_matches_golden_fixture() {
    // The error sentence is `LlmError`'s own `Display`, produced here rather
    // than hand-written, so the fixture cannot drift into holding a string
    // its producer could never emit — and so the `outcome` token comes from
    // `llm_check_outcome`, the same sole producer the macOS app's wording is
    // diffed against by `scripts/check-vocabulary-covers-cli-tokens.sh`.
    let error = LlmError::ProviderStatus {
        status: 401,
        api_style: API_STYLE_CHAT_COMPLETIONS.to_string(),
        endpoint_path: "/v1/chat/completions".to_string(),
        request_id: Some("req_abc123".to_string()),
        body_excerpt:
            "{\"error\":{\"message\":\"Incorrect API key provided.\",\"type\":\"invalid_request_error\"}}"
                .to_string(),
    };
    let report = LlmCheckReport {
        outcome: llm_check_outcome(Some(&error)),
        model: "gpt-4o-mini".to_string(),
        endpoint_path: "/v1/chat/completions".to_string(),
        error: Some(error.to_string()),
        response_excerpt: None,
    };

    assert_eq!(report.outcome, "rejected");
    assert!(
        !report.error.as_deref().unwrap().contains("://"),
        "the rejection sentence must carry no scheme or host — only the path"
    );
    assert_matches_fixture(&report, "llm_check_report_rejected.json");
}

/// HORO-1298's report, surfaced in the GUI by HORO-1309. The fixture's
/// `system_prompt` and `user_prompt` are what actually leave the machine and
/// the `resource_aliases` are what deliberately do not, so this asserts that
/// separation as well as the field names: no alias's real, absolute
/// `local_resource_id` may appear anywhere in either prompt.
#[test]
fn llm_payload_report_matches_golden_fixture() {
    let aliases = vec![
        LlmPayloadResourceAlias {
            wire_resource_id: "resource_1".to_string(),
            local_resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
        },
        LlmPayloadResourceAlias {
            wire_resource_id: "resource_2".to_string(),
            local_resource_id: "node_modules:/Users/dev/proj/node_modules".to_string(),
        },
    ];
    let report = LlmPayloadReport {
        // Quoted from `actions::llm`'s private `SYSTEM_PROMPT`. Not read back
        // out of the fixture, which would make this field assert nothing, and
        // not reachable by reference either — the const is deliberately
        // private so that only `build_request_payload` can put it on a wire.
        system_prompt: "You are a storage cleanup ranking assistant. You will receive a JSON \
             array of resource views. Respond with ONLY a JSON object of the shape \
             {\"items\": [{\"resource_id\": string, \"action_id\": string, \"priority\": \
             number, \"reason\": string}]}, choosing resource_id and action_id only from the \
             values you were given."
            .to_string(),
        user_prompt: "[{\"resource_id\":\"resource_1\",\"kind\":\"cargo_target_dir\",\
             \"reclaimable_bytes\":2147483648,\"age_days\":31,\
             \"regenerability\":\"regenerable_by_rebuild\",\"completeness\":\"complete\",\
             \"offered_action_ids\":[\"cargo.clean.target_dir\"]},\
             {\"resource_id\":\"resource_2\",\"kind\":\"node_modules\",\
             \"reclaimable_bytes\":536870912,\"age_days\":null,\
             \"regenerability\":\"regenerable_by_tool\",\"completeness\":\"partial\",\
             \"offered_action_ids\":[\"node.remove.node_modules\"]}]"
            .to_string(),
        resource_aliases: aliases,
    };

    for alias in &report.resource_aliases {
        assert!(
            !report.system_prompt.contains(&alias.local_resource_id)
                && !report.user_prompt.contains(&alias.local_resource_id),
            "{} is in the outbound prompts; the alias table exists so that it is not",
            alias.local_resource_id
        );
    }
    assert_matches_fixture(&report, "llm_payload_report.json");
}

/// HORO-1310's envelope report, surfaced in the GUI by the Autopilot settings
/// screen. Constructed literally rather than through
/// `autopilot::envelope_report`, like every other fixture here: the contract
/// under test is the field names and JSON types the Swift mirror decodes, and
/// routing it through the producer would make this fail whenever a sentence
/// in `ai_authority` is reworded — a change that breaks nothing.
///
/// `autopilot::report`'s own unit tests are what assert the producer fills
/// these fields from the right functions.
///
/// The fixture carries a grant rather than a revoked envelope so that every
/// optional and every list is populated: `min_pressure` set, one ASK
/// pre-authorization, and both refusal lists non-empty. A fixture of empty
/// arrays would decode successfully on the Swift side no matter what the
/// element types were.
#[test]
fn autopilot_envelope_report_matches_golden_fixture() {
    let report = AutopilotEnvelopeReport {
        enabled: true,
        allowed_kinds: vec!["node_modules", "cargo_target_dir"],
        ask_preauthorizations: vec![AutopilotAskPreauthorizationReport {
            kind: "node_modules",
            reason: "rebuild_cost_high",
        }],
        max_actions: 2,
        max_bytes: 2_147_483_648,
        max_bytes_human: "2.0 GB".to_string(),
        max_duration_secs: 120,
        min_pressure: Some("PRESSURED"),
        respond_to_alerts: true,
        starts_unprompted: true,
        ceilings: AutopilotCeilingsReport {
            max_actions: 25,
            max_bytes: 68_719_476_736,
            max_bytes_human: "64.0 GB".to_string(),
            max_duration_secs: 900,
        },
        allowlistable_kinds: vec![
            "xcode_derived_data",
            "homebrew_cache",
            "cargo_target_dir",
            "cargo_registry_cache",
            "node_modules",
            "node_package_manager_cache",
            "docker_build_cache",
            "docker_image",
            "pip_cache",
            "uv_cache",
            "go_build_cache",
            "go_module_cache",
            "gradle_cache",
            "maven_local_repository",
            "swiftpm_cache",
            "swiftpm_build_dir",
        ],
        never_allowlistable_kinds: vec!["unknown"],
        preauthorizable_reasons: vec!["rebuild_cost_high"],
        never_preauthorizable_reasons: vec![
            "protected_credential_material",
            "protected_git_internals",
            "protected_infra_state",
            "protected_persistent_volume",
            "protected_user_documents",
            "protected_system_path",
            "protected_unsafe_mount_or_symlink",
            "protected_unknown_resource_kind",
            "evidence_incomplete",
            "evidence_stale",
            "evidence_probe_failed",
            "resource_in_active_use",
            "git_worktree_dirty",
            "regenerability_unknown",
            "recoverability_irreversible",
            "owning_tool_live",
        ],
        pressure_states: vec!["HEALTHY", "WARN", "PRESSURED", "CRITICAL", "EMERGENCY"],
        never_executable_labels: vec!["PROTECTED", "UNKNOWN_INCOMPLETE"],
        ai_authority: vec![
            "AI can recommend. Policy decides. Executor verifies. Filesystem reality wins.",
            "The model's only influence is the order candidates are considered in.",
        ],
        stored_at: Some(
            "/Users/dev/Library/Application Support/Glomeris/autopilot.conf".to_string(),
        ),
    };

    // The one cross-field invariant a client would be wrong to assume it can
    // derive: a kind on the refused list must never also be offered.
    for refused in &report.never_allowlistable_kinds {
        assert!(!report.allowlistable_kinds.contains(refused));
        assert!(!report.allowed_kinds.contains(refused));
    }

    assert_matches_fixture(&report, "autopilot_envelope_report.json");
}

// ---------------------------------------------------------------------------
// Developer-workspace aggregation (HORO-1511)
// ---------------------------------------------------------------------------

/// One repository's three checkouts, built through
/// [`WorkspaceFamilyReport::from_family`] rather than as a DTO literal.
///
/// The builder is the point. This report's whole job is to let a client say
/// "this project accounts for 7.5 GB across three worktrees" *without* that
/// sentence becoming permission, and the two ways it could fail are both in
/// the projection rather than in the shape: protected bulk being summed into
/// the reclaimable total, and an unmeasured member being totalled as a
/// confident zero. A hand-typed literal would agree with a fixture that
/// asserts neither.
///
/// The family is deliberately mixed (AC 6): a clean merged main checkout, a
/// dirty linked worktree with unpushed commits and an unmeasured member, and
/// a linked worktree that is in use and whose branch could not be read at
/// all. Two of the three hold work in progress, which is the number that
/// makes the family total unusable as delete authority — the bulk and the
/// outstanding work are in the same group.
fn at(unix_secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs)
}

fn one_repositorys_three_checkouts() -> WorkspaceFamily {
    WorkspaceFamily {
        common_dir: PathBuf::from("/Users/dev/proj/.git"),
        worktrees: vec![
            // The main checkout: nothing outstanding anywhere, and the only
            // worktree whose `holds_work_in_progress` is false.
            WorkspaceWorktree {
                root: PathBuf::from("/Users/dev/proj"),
                linked: false,
                dirty: false,
                untracked: false,
                activity: ActivityState::Idle,
                newest_member_modified_at: None,
                branch: ProbeOutcome::Observed(WorktreeBranchState {
                    branch: Some("main".to_string()),
                    upstream: UpstreamState::Tracking {
                        ahead: 0,
                        behind: 12,
                    },
                    merged: MergedState::Merged {
                        into: "origin/main".to_string(),
                    },
                    // Ancestry already contains HEAD, so there was no
                    // equivalence question — `not_applicable`, and three
                    // observed zeroes rather than three absences.
                    integration: IntegrationEvidence {
                        divergence: ProbeOutcome::Observed(Divergence {
                            unique_commits: 0,
                            equivalent_commits: 0,
                            unclassified_commits: 0,
                        }),
                        equivalence: PatchEquivalence::NotApplicable,
                        tip_committed_at: ProbeOutcome::Observed(at(1_725_000_000)),
                        comparison_tip_committed_at: ProbeOutcome::Observed(at(1_725_000_000)),
                    },
                }),
                members: vec![WorkspaceMember {
                    resource_id: "cargo_target_dir:/Users/dev/proj/target".to_string(),
                    reclaimable_bytes: Some(2_147_483_648),
                    reclaimable_bytes_is_lower_bound: false,
                    label: PolicyLabel::AutoSafe,
                }],
            },
            // Dirty, three commits no remote has, and one member nothing
            // measured. Its `ASK` member is the lower-bound estimate, so
            // `is_lower_bound` on the family comes from a member that
            // actually contributes to a byte total.
            WorkspaceWorktree {
                root: PathBuf::from("/Users/dev/proj-feature"),
                linked: true,
                dirty: true,
                untracked: false,
                activity: ActivityState::Idle,
                newest_member_modified_at: None,
                branch: ProbeOutcome::Observed(WorktreeBranchState {
                    branch: Some("feature/recovery-goal".to_string()),
                    upstream: UpstreamState::Tracking {
                        ahead: 3,
                        behind: 0,
                    },
                    merged: MergedState::NotMerged {
                        into: "origin/main".to_string(),
                    },
                    // The shape HORO-1545 exists for: three commits ancestry
                    // does not have, whose files are nonetheless identical to
                    // the default branch's — a squash landed. And the
                    // worktree is *still* dirty, so
                    // `holds_work_in_progress` stays true beside it.
                    integration: IntegrationEvidence {
                        divergence: ProbeOutcome::Observed(Divergence {
                            unique_commits: 3,
                            equivalent_commits: 0,
                            unclassified_commits: 0,
                        }),
                        equivalence: PatchEquivalence::Equivalent(
                            EquivalenceMethod::ContentIdentical,
                        ),
                        tip_committed_at: ProbeOutcome::Observed(at(1_726_500_000)),
                        comparison_tip_committed_at: ProbeOutcome::Observed(at(1_725_000_000)),
                    },
                }),
                members: vec![
                    WorkspaceMember {
                        resource_id: "node_modules:/Users/dev/proj-feature/node_modules"
                            .to_string(),
                        reclaimable_bytes: Some(5_368_709_120),
                        reclaimable_bytes_is_lower_bound: true,
                        label: PolicyLabel::Ask,
                    },
                    WorkspaceMember {
                        resource_id: "docker_build_cache:/Users/dev/proj-feature/.docker"
                            .to_string(),
                        reclaimable_bytes: None,
                        reclaimable_bytes_is_lower_bound: false,
                        label: PolicyLabel::UnknownIncomplete,
                    },
                ],
            },
            // In use, untracked files, and no branch answer at all: three
            // separate reasons, and the `"unknown"` tags are what an unread
            // branch produces rather than a tidied-up detached-HEAD guess.
            WorkspaceWorktree {
                root: PathBuf::from("/Users/dev/proj-hotfix"),
                linked: true,
                dirty: false,
                untracked: true,
                activity: ActivityState::InUse,
                newest_member_modified_at: None,
                branch: ProbeOutcome::Unavailable(ProbeReason::TimedOut),
                members: vec![WorkspaceMember {
                    resource_id:
                        "xcode_derived_data:/Users/dev/Library/Developer/Xcode/DerivedData/App-h"
                            .to_string(),
                    reclaimable_bytes: Some(1_073_741_824),
                    reclaimable_bytes_is_lower_bound: false,
                    label: PolicyLabel::Protected,
                }],
            },
        ],
    }
}

#[test]
fn workspace_family_report_matches_golden_fixture() {
    let report = WorkspaceFamilyReport::from_family(&one_repositorys_three_checkouts());
    assert_matches_fixture(&report, "workspace_family_report.json");
}

/// The join AC 5 rests on: a family total a person can read beside the
/// candidate lines that carry the actual action and refusal evidence.
///
/// Every `member_resource_ids` entry resolves to a candidate, and the
/// candidate is where `executable`, `offered_actions` and `refusal_reason`
/// are — the family report has none of the three. The fixture also carries
/// one candidate belonging to no family (`homebrew_cache`, outside any git
/// working tree), because "grouped" and "discovered" are different lists and
/// a client must not assume the families account for everything.
#[test]
fn detect_report_with_workspaces_matches_golden_fixture() {
    let report = DetectReport {
        candidates: vec![
            DetectCandidateReport {
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
            DetectCandidateReport {
                resource_id: "node_modules:/Users/dev/proj-feature/node_modules".to_string(),
                kind: "node_modules",
                reclaimable_bytes: Some(5_368_709_120),
                reclaimable_human: Some("5.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: true,
                impact_tier: "notable",
                policy_label: "ASK",
                reasons: vec!["rebuild_cost_high"],
                executable: true,
                offered_actions: vec![OfferedAction {
                    action_id: "node.remove.node_modules".to_string(),
                    requires_confirmation: true,
                }],
                refusal_reason: None,
            },
            DetectCandidateReport {
                resource_id: "docker_build_cache:/Users/dev/proj-feature/.docker".to_string(),
                kind: "docker_build_cache",
                reclaimable_bytes: None,
                reclaimable_human: None,
                reclaimable_bytes_is_lower_bound: false,
                impact_tier: "unknown",
                policy_label: "UNKNOWN_INCOMPLETE",
                reasons: vec!["evidence_incomplete"],
                executable: false,
                offered_actions: vec![],
                refusal_reason: Some("size could not be measured".to_string()),
            },
            DetectCandidateReport {
                resource_id:
                    "xcode_derived_data:/Users/dev/Library/Developer/Xcode/DerivedData/App-h"
                        .to_string(),
                kind: "xcode_derived_data",
                reclaimable_bytes: Some(1_073_741_824),
                reclaimable_human: Some("1.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: false,
                impact_tier: "notable",
                policy_label: "PROTECTED",
                reasons: vec!["active_process_using_path"],
                executable: false,
                offered_actions: vec![],
                refusal_reason: Some("an active process is using this path".to_string()),
            },
            // Discovered, and in no family: not inside any git working tree.
            DetectCandidateReport {
                resource_id: "homebrew_cache:/Users/dev/Library/Caches/Homebrew".to_string(),
                kind: "homebrew_cache",
                reclaimable_bytes: Some(3_221_225_472),
                reclaimable_human: Some("3.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: false,
                impact_tier: "notable",
                policy_label: "AUTO_SAFE",
                reasons: vec!["no_active_use_observed"],
                executable: true,
                offered_actions: vec![OfferedAction {
                    action_id: "homebrew.cleanup.cache".to_string(),
                    requires_confirmation: false,
                }],
                refusal_reason: None,
            },
        ],
        detectors: vec![
            DetectorHealthReport {
                detector: "cargo_target_dir".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "node_modules".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "docker_build_cache".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "xcode_derived_data".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "homebrew_cache".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
        ],
        discovery_complete: true,
        workspaces: vec![WorkspaceFamilyReport::from_family(
            &one_repositorys_three_checkouts(),
        )],
    };
    assert_matches_fixture(&report, "detect_report_with_workspaces.json");
}

/// Every id a family publishes has to name a candidate in the same report.
///
/// Not a fixture assertion — a statement about the two lists that the
/// fixture above would still satisfy if someone renamed an id on one side
/// only, because `assert_matches_fixture` compares each report against the
/// JSON rather than against the other list.
#[test]
fn every_workspace_member_id_names_a_candidate_in_the_same_report() {
    let fixture = read_fixture("detect_report_with_workspaces.json");
    let value: serde_json::Value = serde_json::from_str(&fixture).expect("parse fixture");

    let candidate_ids: Vec<&str> = value["candidates"]
        .as_array()
        .expect("candidates array")
        .iter()
        .map(|c| c["resource_id"].as_str().expect("resource_id string"))
        .collect();

    let mut grouped = 0usize;
    for family in value["workspaces"].as_array().expect("workspaces array") {
        for worktree in family["worktrees"].as_array().expect("worktrees array") {
            // The aggregate explains; it must not be able to authorize. A
            // worktree carries ids and no executability of its own.
            for forbidden in ["executable", "offered_actions", "action_id"] {
                assert!(
                    worktree.get(forbidden).is_none(),
                    "a worktree report must not carry `{forbidden}`"
                );
            }
            for id in worktree["member_resource_ids"]
                .as_array()
                .expect("member_resource_ids array")
            {
                let id = id.as_str().expect("resource id string");
                assert!(
                    candidate_ids.contains(&id),
                    "family member {id} names no candidate in the same report"
                );
                grouped += 1;
            }
        }
    }

    assert_eq!(grouped, 4, "the fixture's family has four members");
    assert_eq!(
        candidate_ids.len(),
        5,
        "one candidate belongs to no family, which is the case being pinned"
    );
}

// ---------------------------------------------------------------------------
// Recovery goal reports (HORO-1506)
// ---------------------------------------------------------------------------
//
// These four are built through `cli::recovery`'s own builders rather than as
// struct literals, which is a deliberate departure from every fixture above.
// The reason is the caveat sentences: they are the report's defence against
// being misread — estimates are not measurements, an incomplete search is not
// a complete one, confirmation-gated space is not automatic — and a literal
// would let the fixture agree with a hand-typed copy of them while the CLI
// emitted something else entirely. Going through the builder means the fixture
// pins what `glomeris free` actually prints.

/// The discovery pass both preview fixtures are built from.
///
/// Four candidates, chosen so the opportunity split has something in every
/// bucket: one actionable now, one executable but confirmation-gated, one
/// `PROTECTED`, one not executable for want of an action. The
/// confirmation-gated one is also the lower-bound estimate, because
/// `build_recovery_opportunity` only raises `is_lower_bound` for candidates
/// that contribute to a byte total — pinning that on a non-executable
/// candidate would pin nothing.
fn recovery_discovery_pass() -> DetectReport {
    DetectReport {
        candidates: vec![
            DetectCandidateReport {
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
            DetectCandidateReport {
                resource_id: "node_modules:/Users/dev/proj/node_modules".to_string(),
                kind: "node_modules",
                reclaimable_bytes: Some(5_368_709_120),
                reclaimable_human: Some("5.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: true,
                impact_tier: "notable",
                policy_label: "ASK",
                reasons: vec!["rebuild_cost_high"],
                executable: true,
                offered_actions: vec![OfferedAction {
                    action_id: "node.remove.node_modules".to_string(),
                    requires_confirmation: true,
                }],
                refusal_reason: None,
            },
            DetectCandidateReport {
                resource_id:
                    "xcode_derived_data:/Users/dev/Library/Developer/Xcode/DerivedData/App-a"
                        .to_string(),
                kind: "xcode_derived_data",
                reclaimable_bytes: Some(1_073_741_824),
                reclaimable_human: Some("1.0 GB".to_string()),
                reclaimable_bytes_is_lower_bound: false,
                impact_tier: "notable",
                policy_label: "PROTECTED",
                reasons: vec!["active_process_using_path"],
                executable: false,
                offered_actions: vec![],
                refusal_reason: Some("an active process is using this path".to_string()),
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
        detectors: vec![
            DetectorHealthReport {
                detector: "cargo_target_dir".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "node_modules".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "xcode_derived_data".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "docker_build_cache".to_string(),
                status: "found",
                candidates_found: 1,
                reason: None,
            },
            DetectorHealthReport {
                detector: "homebrew_cache".to_string(),
                status: "failed",
                candidates_found: 0,
                reason: Some("brew --cache exited 1".to_string()),
            },
        ],
        discovery_complete: false,
        // The recovery previews are about a volume and a goal, not about
        // where the bulk sits; grouping would add a second thing for those
        // fixtures to pin. HORO-1511's own fixture covers it.
        workspaces: vec![],
    }
}

#[test]
fn recovery_preview_report_matches_golden_fixture() {
    // 440 GB of 500 GB used — 88.0% — against a 60%-used goal, so the goal is
    // a real improvement and `bytes_needed` is non-zero. The estimated 2.0 GB
    // actionable now is nowhere near it, which is the point: a preview has to
    // be able to say "this will not get you there" without that being a
    // refusal.
    let usage = FsUsage::new(500_000_000_000, 60_000_000_000);
    let goal = RecoveryGoal::from_used_percent(60.0).expect("60% used is a valid goal");
    let report = build_recovery_preview_report(
        &goal,
        &usage,
        &ThresholdConfig::default(),
        recovery_discovery_pass(),
    );
    assert_matches_fixture(&report, "recovery_preview_report.json");
}

/// The preview of a goal that is already met.
///
/// The same volume and the same discovery pass as above, against a 95%-used
/// goal it is already below. Three things are pinned that the other preview
/// fixture cannot pin, because there `bytes_needed` is non-zero:
///
///   * `bytes_needed` is `0` and its human form is a real formatted zero rather
///     than an omission — a client that treated a missing figure as "unknown"
///     would have nothing to distinguish "already there" from "not measured".
///   * the caveat saying a real run would be refused. `free --dry-run` reports
///     this state instead of refusing it, so the sentence is the only thing
///     standing between a user and pressing a button that cannot work.
///   * `goal_appears_reachable` is `true` here for the trivial reason — there is
///     nothing left to reach — which is worth having in a fixture so nobody
///     reads that flag as "there is enough to reclaim".
#[test]
fn recovery_preview_report_for_an_already_met_goal_matches_golden_fixture() {
    let usage = FsUsage::new(500_000_000_000, 60_000_000_000);
    let goal = RecoveryGoal::from_used_percent(95.0).expect("95% used is a valid goal");
    let report = build_recovery_preview_report(
        &goal,
        &usage,
        &ThresholdConfig::default(),
        recovery_discovery_pass(),
    );
    assert_matches_fixture(&report, "recovery_preview_report_goal_already_met.json");
}

#[test]
fn recovery_run_report_matches_golden_fixture() {
    let goal = RecoveryGoal::from_used_percent(60.0).expect("60% used is a valid goal");
    let inner = RecoveryReport {
        stop_reason: StopReason::TargetReached,
        iterations_run: 3,
        actions_executed: 4,
        actions_declined_or_skipped: 2,
        total_bytes_freed: 150_000_000_000,
        started_free_bytes: 60_000_000_000,
        final_free_bytes: 210_000_000_000,
        detector_failures: vec![],
    };
    // 210 GB free of 500 GB is 42% free, which clears the goal's 40% floor —
    // so `target_met` is decided by the re-measured reading, not by the
    // 150 GB that was deleted.
    let report =
        build_recovery_run_report(Some(&goal), &goal.to_free_target(), 500_000_000_000, &inner);
    assert_matches_fixture(&report, "recovery_run_report.json");
}

/// The raw `--target` form, and the run that did not get there.
///
/// Two absences are the fixture's whole purpose: `goal` is omitted because a
/// free-space floor is not a used-axis goal, and `error` is omitted because
/// this run did not fail. A client that treated either absence as a zero or a
/// blank would be inventing a fact, so both are pinned here rather than left to
/// whichever fixture happened to have them.
///
/// It is also the one fixture carrying `remaining` (HORO-1509), because it is
/// the one that stopped for want of safe work. Three different counts, so a
/// client that decoded them in the wrong order fails here rather than telling a
/// user to confirm something protected. `recovery_run_report.json` reached its
/// goal and omits the key entirely — the pair pins both halves of that rule.
#[test]
fn recovery_run_report_for_a_raw_target_matches_golden_fixture() {
    let target = FreeTarget::AbsoluteBytes(250_000_000_000);
    let inner = RecoveryReport {
        stop_reason: StopReason::SafeExhausted(RemainingCandidates {
            requires_confirmation: 2,
            protected: 1,
            not_executable: 3,
            // A human-started run, which is what this fixture is: no Autopilot
            // envelope, so nothing can have been refused by one (HORO-1510).
            not_permitted_by_autopilot: 0,
        }),
        iterations_run: 2,
        actions_executed: 1,
        actions_declined_or_skipped: 3,
        total_bytes_freed: 2_147_483_648,
        started_free_bytes: 60_000_000_000,
        final_free_bytes: 62_147_483_648,
        detector_failures: vec!["homebrew_cache: brew --cache exited 1".to_string()],
    };
    let report = build_recovery_run_report(None, &target, 500_000_000_000, &inner);
    assert_matches_fixture(&report, "recovery_run_report_raw_target.json");
}

/// The automatic run that ran out of *authorization*, not of safe work
/// (HORO-1510).
///
/// The pair with `recovery_run_report_raw_target.json` is the point. Both
/// stopped without reaching the goal and both carry a `remaining` breakdown, so
/// the only thing telling a client which sentence to show is `stop_reason` plus
/// the `envelope_refusal` token — and a client that ignored the difference would
/// tell a user their disk has nothing safe left on it when four candidates are
/// sitting there waiting for a run the user starts themselves. That is why
/// `not_permitted_by_autopilot_count` is the largest count here and `0` there:
/// decoded in the wrong order, or folded into `protected_count`, it fails here.
///
/// `envelope_refusal` carries the gate's own snake_case token rather than
/// leaving a client to substring-match `stop_reason_detail`, whose wording is
/// prose and will be reworded.
#[test]
fn recovery_run_report_for_an_envelope_refusal_matches_golden_fixture() {
    let goal = RecoveryGoal::from_used_percent(60.0).expect("60% used is a valid goal");
    let inner = RecoveryReport {
        stop_reason: StopReason::EnvelopeRefused {
            refusal: RefusalReason::ActionBudgetExhausted { max_actions: 3 },
            // `Some`, because this refusal came from a pass that did look. The
            // pre-discovery refusals carry `None` and drop the key entirely —
            // see the companion fixture.
            remaining: Some(RemainingCandidates {
                requires_confirmation: 1,
                protected: 2,
                not_executable: 0,
                not_permitted_by_autopilot: 4,
            }),
        },
        iterations_run: 4,
        actions_executed: 3,
        actions_declined_or_skipped: 7,
        total_bytes_freed: 5_368_709_120,
        started_free_bytes: 60_000_000_000,
        final_free_bytes: 65_368_709_120,
        detector_failures: vec![],
    };
    // 65.4 GB free of 500 GB is 13% free, well short of the goal's 40% floor:
    // the run really did stop early, and `target_met` says so from the
    // re-measured reading rather than from the 5 GB it did free.
    let report =
        build_recovery_run_report(Some(&goal), &goal.to_free_target(), 500_000_000_000, &inner);
    assert_matches_fixture(&report, "recovery_run_report_envelope_refused.json");
}

/// The envelope that refused *before* discovery ran (HORO-1510).
///
/// Asserted structurally rather than against a fixture file, because the whole
/// claim is about a key that must not be there: a run stopped by a revoked
/// Autopilot switch never looked at the disk, so it has no breakdown to report,
/// and emitting one made of zeros would read as "we searched and found
/// nothing" — the HORO-1484 failure this contract exists to avoid. The token
/// still ships, so a client can explain the stop without a breakdown.
#[test]
fn a_pre_discovery_envelope_refusal_omits_the_remaining_breakdown() {
    let inner = RecoveryReport {
        stop_reason: StopReason::EnvelopeRefused {
            refusal: RefusalReason::AutopilotRevoked,
            remaining: None,
        },
        iterations_run: 1,
        actions_executed: 0,
        actions_declined_or_skipped: 0,
        total_bytes_freed: 0,
        started_free_bytes: 60_000_000_000,
        final_free_bytes: 60_000_000_000,
        detector_failures: vec![],
    };
    let report = build_recovery_run_report(
        None,
        &FreeTarget::AbsoluteBytes(250_000_000_000),
        500_000_000_000,
        &inner,
    );
    let json = serde_json::to_value(&report).expect("the report serializes");
    assert_eq!(
        json.get("stop_reason").and_then(|v| v.as_str()),
        Some("envelope_refused")
    );
    assert_eq!(
        json.get("envelope_refusal").and_then(|v| v.as_str()),
        Some("autopilot_revoked"),
        "the refusal token ships even with no breakdown to attach it to"
    );
    assert!(
        json.get("remaining").is_none(),
        "a run that never reached discovery must not publish a breakdown, not even one of zeros: {json:#}"
    );
}

#[test]
fn recovery_goal_rejection_report_matches_golden_fixture() {
    let usage = FsUsage::new(500_000_000_000, 60_000_000_000);
    let goal = RecoveryGoal::from_used_percent(95.0).expect("95% used is a valid goal");
    // 88.0% used already, so a 95%-used goal would reclaim nothing. Obtained
    // from `progress_toward` rather than constructed, so the fixture carries
    // the rejection the CLI really produces.
    let rejection = goal
        .progress_toward(&usage)
        .expect_err("a goal above current usage is not an improvement");
    let report = build_goal_rejection_report(&rejection);
    assert_matches_fixture(&report, "recovery_goal_rejection_report.json");
}

/// The rejection with both optional figures absent.
///
/// `not_finite` is the one rejection with no number to report, and reporting
/// `0.0` for it would be a fabricated measurement — so the fields are omitted,
/// and that omission is what this fixture pins.
#[test]
fn recovery_goal_rejection_report_with_no_figures_matches_golden_fixture() {
    let rejection =
        RecoveryGoal::from_used_percent(f64::NAN).expect_err("NaN is not a usable goal");
    let report = build_goal_rejection_report(&rejection);
    assert_matches_fixture(&report, "recovery_goal_rejection_report_not_finite.json");
}

// ---------------------------------------------------------------------------
// Settings reports (HORO-1507)
// ---------------------------------------------------------------------------
//
// Built through the validators for the same reason the recovery fixtures go
// through their builders, plus one of their own: the two numbers here are
// constrained *against each other*, so a struct literal could pin a pair the
// CLI would refuse and the settings pane would then be tested against a state
// no user can reach.

/// The settings report, with a stored pair (HORO-1507).
///
/// Built through `with_changes` rather than as a struct literal, so the fixture
/// carries a pair the validator actually accepted — a hand-written literal could
/// pin a combination `settings set` would refuse, and the app would then be
/// tested against a state it can never be in.
#[test]
fn recovery_settings_report_matches_golden_fixture() {
    let settings = RecoverySettings::default()
        .with_changes(Some(85.0), Some(60.0))
        .expect("85% threshold with a 60% goal is a valid pair");
    let report = build_settings_report(
        &settings,
        Some("/Users/dev/Library/Application Support/Glomeris/settings.conf".to_string()),
        true,
    );
    assert_matches_fixture(&report, "recovery_settings_report.json");
}

/// The same report before anything has been stored.
///
/// Two things are pinned that the populated fixture cannot pin: `stored_at`
/// omitted rather than null — the one case where `$HOME` could not be resolved —
/// and `loaded_from_file: false`, which is the flag a pane needs to tell "these
/// are the built-in numbers" from "these were chosen". A client that read the
/// absence of the path as "not configured" would be conflating two different
/// facts.
#[test]
fn recovery_settings_report_for_the_built_in_defaults_matches_golden_fixture() {
    let report = build_settings_report(&RecoverySettings::default(), None, false);
    assert_matches_fixture(&report, "recovery_settings_report_defaults.json");
}

/// A refused change (HORO-1507).
///
/// The cross-field refusal, chosen because it is the only one carrying *both*
/// figures: it is about neither number on its own, and a client showing only one
/// of them would send the user to the field they may not have wanted to change.
/// Obtained from the validator, so the message is the one the CLI really prints.
#[test]
fn settings_rejection_report_matches_golden_fixture() {
    let rejection = RecoverySettings::default()
        .with_changes(Some(85.0), Some(90.0))
        .expect_err("a goal above the alert threshold is refused");
    let report = build_settings_rejection_report(&rejection);
    assert_matches_fixture(&report, "settings_rejection_report.json");
}

// ---------------------------------------------------------------------------
// Pressure-episode reports (HORO-1508)
// ---------------------------------------------------------------------------
//
// Driven through `EpisodeTracker` rather than written as struct literals, for
// the reason the settings fixtures give and one more of their own: these fields
// are a *state machine's* observable state. A literal could pin
// `notification_due: true` next to `notifications_raised: 1`, a combination the
// tracker will not produce, and the notifier would then be tested against a
// state that cannot occur — while the storm AC5 forbids would be tested against
// nothing at all.

/// The state the menu-bar app acts on: pressure crossed, banner owed, nobody
/// has answered anything yet.
///
/// Every optional is absent rather than null. That is the half of the contract a
/// mis-mapped `CodingKey` on the Swift side would pass silently, so the sibling
/// fixture below populates all four.
#[test]
fn pressure_status_report_matches_golden_fixture() {
    let settings = RecoverySettings::default()
        .with_changes(Some(85.0), Some(60.0))
        .expect("85% threshold with a 60% goal is a valid pair");
    let mut tracker = EpisodeTracker::new(EpisodeConfig::new(settings.notify_at_used_percent()));
    tracker.observe(91.0, 45_000_000_000, 1_700_000_000);

    let report = build_pressure_status_report(
        &tracker,
        &settings,
        &FsUsage::new(500_000_000_000, 45_000_000_000),
        &ThresholdConfig::default(),
        1_700_000_000,
        Some("/Users/dev/Library/Application Support/Glomeris/pressure-episode.json".to_string()),
    );
    assert_matches_fixture(&report, "pressure_status_report.json");
}

/// The same episode after it was raised, snoozed, and then measured again on a
/// disk that had recovered somewhat.
///
/// Three things are pinned here that the fixture above cannot pin. All four
/// optionals are populated. `is_snoozed` is a *derived* flag rather than stored
/// state, so a client must not compute it from a stale reading. And
/// `current.used_percent` (88) differs from `episode.peak_used_percent` (94):
/// a surface conflating the two would tell the user their disk is fuller than
/// it now is, which is the number they would be deciding on.
#[test]
fn pressure_status_report_for_a_snoozed_episode_matches_golden_fixture() {
    let settings = RecoverySettings::default()
        .with_changes(Some(85.0), Some(60.0))
        .expect("85% threshold with a 60% goal is a valid pair");
    let mut tracker = EpisodeTracker::new(EpisodeConfig::new(settings.notify_at_used_percent()));
    tracker.observe(94.0, 30_000_000_000, 1_700_000_000);
    tracker
        .mark_notified(1_700_000_060)
        .expect("a notification was owed");
    tracker
        .respond(EpisodeResponse::RemindLater, 1_700_000_120)
        .expect("an open episode can be answered");
    // Still above the clear boundary, so the episode does not close — and still
    // inside the snooze, so nothing is owed.
    tracker.observe(88.0, 60_000_000_000, 1_700_002_000);

    let report = build_pressure_status_report(
        &tracker,
        &settings,
        &FsUsage::new(500_000_000_000, 60_000_000_000),
        &ThresholdConfig::default(),
        1_700_003_000,
        Some("/Users/dev/Library/Application Support/Glomeris/pressure-episode.json".to_string()),
    );
    assert_matches_fixture(&report, "pressure_status_report_snoozed.json");
}

/// A quiet disk.
///
/// `episode` is omitted, not null — and so is `state_path`, for the case where
/// `$HOME` could not be resolved. Both absences say "there is nothing here",
/// and neither may be read as "monitoring is off": `notify_at_used_percent` and
/// `responses` are still reported, which is how a pane says what it is watching
/// for before anything has happened.
#[test]
fn pressure_status_report_with_no_episode_matches_golden_fixture() {
    let settings = RecoverySettings::default()
        .with_changes(Some(85.0), Some(60.0))
        .expect("85% threshold with a 60% goal is a valid pair");
    let tracker = EpisodeTracker::new(EpisodeConfig::new(settings.notify_at_used_percent()));

    let report = build_pressure_status_report(
        &tracker,
        &settings,
        &FsUsage::new(500_000_000_000, 200_000_000_000),
        &ThresholdConfig::default(),
        1_700_000_000,
        None,
    );
    assert_matches_fixture(&report, "pressure_status_report_no_episode.json");
}

/// A refused acknowledgement (HORO-1508).
///
/// `no_notification_due` rather than `no_open_episode` because it is the one an
/// app reaches by doing the right thing twice — two polls racing to raise the
/// same banner. Obtained from the tracker, so the message is the one the CLI
/// really prints, and the app cannot be built against a friendlier wording than
/// the user will see.
#[test]
fn pressure_rejection_report_matches_golden_fixture() {
    let mut tracker = EpisodeTracker::new(EpisodeConfig::new(85.0));
    tracker.observe(91.0, 45_000_000_000, 1_700_000_000);
    tracker
        .mark_notified(1_700_000_060)
        .expect("the first acknowledgement is owed");
    let rejection = tracker
        .mark_notified(1_700_000_070)
        .expect_err("the second acknowledgement is not");

    let report = build_pressure_rejection_report(&rejection);
    assert_matches_fixture(&report, "pressure_rejection_report.json");
}
