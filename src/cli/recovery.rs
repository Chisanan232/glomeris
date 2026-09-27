//! Report building for the recovery-goal surface (HORO-1506).
//!
//! `glomeris free` printed prose and nothing else, so a GUI wanting to offer
//! target-based recovery had only terminal output to scrape. This module is
//! the projection layer that fixes that: it turns a
//! [`RecoveryGoal`](crate::executor::goal::RecoveryGoal), an
//! [`FsUsage`] reading and a discovery pass into the
//! [`RecoveryPreviewReport`] shown *before* anything is mutated, and a
//! finished [`RecoveryReport`] into the [`RecoveryRunReport`] shown after.
//!
//! Two rules shape everything here.
//!
//! First, **no ambiguous percentages**. Every percentage that reaches a
//! report names its axis, and the used↔free inversion happens only in
//! [`RecoveryGoal`](crate::executor::goal::RecoveryGoal).
//!
//! Second, **estimates and measurements are never the same number**. The
//! preview's opportunity figures are detector estimates and are split by what
//! policy would actually permit, so confirmation-gated and protected space
//! cannot be read as space the user is about to get back. The run report's
//! byte figures are measured, and `target_met` is derived from a re-measured
//! free-space reading rather than from the sum of what was deleted — which is
//! the campaign's "filesystem reality wins" rule stated as code.

use crate::executor::goal::RecoveryGoal;
use crate::executor::recovery_loop::{target_met, FreeTarget, RecoveryReport, StopReason};
use crate::monitor::{FsUsage, ThresholdConfig};
use crate::reporting::dto::{
    stop_reason_tag, DetectCandidateReport, DetectReport, RecoveryGoalReport,
    RecoveryOpportunityReport, RecoveryPreviewReport, RecoveryRunReport,
};
use crate::reporting::human_bytes;

/// The sentence every estimate-bearing surface carries. One producer, so the
/// CLI, the JSON and the GUI cannot soften it differently.
const ESTIMATES_CAVEAT: &str = "Reclaimable figures are detector estimates. \
     Actual progress is measured from the filesystem after each action.";

/// Project a goal onto both axes plus one unambiguous sentence.
pub fn build_recovery_goal_report(goal: &RecoveryGoal) -> RecoveryGoalReport {
    RecoveryGoalReport {
        used_percent: goal.used_percent(),
        free_percent: goal.free_percent(),
        description: goal.describe(),
    }
}

/// Describe a raw [`FreeTarget`] in words that name its axis.
///
/// `FreeTarget` is a free-space floor in both of its forms, so both arms say
/// "free". A reader who sees `20% free` cannot mistake it for 20% used.
pub fn describe_free_target(target: &FreeTarget) -> String {
    match target {
        FreeTarget::Percentage(pct) => format!("{pct}% free"),
        FreeTarget::AbsoluteBytes(bytes) => format!("{} free", human_bytes(*bytes)),
    }
}

/// One sentence per stop reason, in the terms a user cares about.
///
/// Deliberately never the bare word "Done". A run that stopped because
/// nothing safe was left has not achieved what the user asked for, and
/// collapsing that into a success word is the specific failure HORO-1509
/// prohibits — this is the wording contract it will extend, not replace.
pub fn stop_reason_detail(reason: &StopReason) -> String {
    match reason {
        StopReason::TargetReached => {
            "The recovery goal was reached: re-measured free space satisfies it.".to_string()
        }
        StopReason::SafeExhausted => "The run stopped before reaching the goal because no \
             safe candidate remained among the detectors that answered."
            .to_string(),
        StopReason::BudgetExceeded => "The run stopped before reaching the goal because a run \
             limit was reached (iterations, actions, or elapsed time)."
            .to_string(),
        StopReason::NoProgress => "The run stopped before reaching the goal because recent \
             actions reclaimed no measurable space."
            .to_string(),
        StopReason::Error(message) => {
            format!("The run could not continue: {message}")
        }
    }
}

/// Sum and split the estimated opportunity across a discovery pass's
/// candidates.
///
/// The split is the product of this function, not a convenience. A single
/// total would let `PROTECTED` and confirmation-gated space read as space a
/// run is about to reclaim; the counts for those are reported without byte
/// sums for `not_executable` precisely so nothing presents unreachable space
/// as an opportunity.
pub fn build_recovery_opportunity(
    candidates: &[DetectCandidateReport],
) -> RecoveryOpportunityReport {
    let mut out = RecoveryOpportunityReport::default();

    for candidate in candidates {
        if !candidate.executable {
            out.not_executable_count += 1;
            if candidate.policy_label == "PROTECTED" {
                out.protected_count += 1;
            }
            continue;
        }

        // `executable` guarantees exactly one offered action (see
        // `DetectCandidateReport::offered_actions`), but read it defensively
        // rather than indexing: a future change that breaks that invariant
        // should under-report an opportunity, not panic.
        let requires_confirmation = candidate
            .offered_actions
            .iter()
            .any(|action| action.requires_confirmation);
        let bytes = candidate.reclaimable_bytes.unwrap_or(0);

        if requires_confirmation {
            out.requires_confirmation_count += 1;
            out.requires_confirmation_bytes = out.requires_confirmation_bytes.saturating_add(bytes);
        } else {
            out.actionable_now_count += 1;
            out.actionable_now_bytes = out.actionable_now_bytes.saturating_add(bytes);
        }

        if candidate.reclaimable_bytes.is_some() && candidate.reclaimable_bytes_is_lower_bound {
            out.is_lower_bound = true;
        }
    }

    out.actionable_now_human = human_bytes(out.actionable_now_bytes);
    out.requires_confirmation_human = human_bytes(out.requires_confirmation_bytes);
    out
}

/// Build the pre-execution preview for `goal` against a real `usage`
/// reading and an already-completed discovery pass.
///
/// Pure: every input is passed in, nothing here touches the filesystem, so
/// the whole surface is unit-testable without a disk in a particular state.
pub fn build_recovery_preview_report(
    goal: &RecoveryGoal,
    usage: &FsUsage,
    thresholds: &ThresholdConfig,
    detect: DetectReport,
) -> RecoveryPreviewReport {
    let required_free_bytes = goal.required_free_bytes(usage.total_bytes);
    let bytes_needed = required_free_bytes.saturating_sub(usage.free_bytes);
    let opportunity = build_recovery_opportunity(&detect.candidates);

    let mut caveats = vec![ESTIMATES_CAVEAT.to_string()];
    if !detect.discovery_complete {
        let failed: Vec<&str> = detect
            .detectors
            .iter()
            .filter(|d| d.status == "failed")
            .map(|d| d.detector.as_str())
            .collect();
        caveats.push(format!(
            "Discovery was incomplete: {} detector(s) failed ({}). \
             The real opportunity may be larger than shown.",
            failed.len(),
            failed.join(", ")
        ));
    }
    if opportunity.is_lower_bound {
        caveats.push(
            "At least one estimate is a lower bound, so the real opportunity may be larger."
                .to_string(),
        );
    }
    if opportunity.requires_confirmation_count > 0 {
        caveats.push(format!(
            "{} candidate(s) need your confirmation and will not be reclaimed automatically.",
            opportunity.requires_confirmation_count
        ));
    }
    if opportunity.protected_count > 0 {
        caveats.push(format!(
            "{} candidate(s) are protected and cannot be reclaimed by any run.",
            opportunity.protected_count
        ));
    }

    RecoveryPreviewReport {
        goal: build_recovery_goal_report(goal),
        current: super::build_status_report(usage, thresholds),
        required_free_bytes,
        required_free_human: human_bytes(required_free_bytes),
        bytes_needed,
        bytes_needed_human: human_bytes(bytes_needed),
        // Planning only. See the field's own doc comment for why summing
        // estimates is permitted here and nowhere else.
        goal_appears_reachable: opportunity.actionable_now_bytes >= bytes_needed,
        opportunity,
        discovery_complete: detect.discovery_complete,
        caveats,
        detectors: detect.detectors,
        candidates: detect.candidates,
    }
}

/// Project a finished run.
///
/// `total_bytes` is required because [`RecoveryReport`] carries free bytes
/// only, and deciding `target_met` needs capacity. That decision is made
/// here from the **re-measured** `final_free_bytes`, using the loop's own
/// [`target_met`] predicate rather than a second copy of the comparison, and
/// never from `bytes_freed_measured` — a run can free a great deal and still
/// not reach a goal, and must say so.
pub fn build_recovery_run_report(
    goal: Option<&RecoveryGoal>,
    target: &FreeTarget,
    total_bytes: u64,
    report: &RecoveryReport,
) -> RecoveryRunReport {
    let final_usage = FsUsage::new(total_bytes, report.final_free_bytes);
    let error = match &report.stop_reason {
        StopReason::Error(message) => Some(message.clone()),
        _ => None,
    };

    RecoveryRunReport {
        goal: goal.map(build_recovery_goal_report),
        target: describe_free_target(target),
        stop_reason: stop_reason_tag(&report.stop_reason),
        stop_reason_detail: stop_reason_detail(&report.stop_reason),
        error,
        iterations_run: report.iterations_run,
        actions_executed: report.actions_executed,
        actions_declined_or_skipped: report.actions_declined_or_skipped,
        bytes_freed_measured: report.total_bytes_freed,
        bytes_freed_measured_human: human_bytes(report.total_bytes_freed),
        started_free_bytes: report.started_free_bytes,
        started_free_human: human_bytes(report.started_free_bytes),
        final_free_bytes: report.final_free_bytes,
        final_free_human: human_bytes(report.final_free_bytes),
        target_met: target_met(&final_usage, target),
        detector_failures: report.detector_failures.clone(),
        discovery_complete: report.detector_failures.is_empty(),
        caveats: report.discovery_caveat_lines(),
    }
}

/// Print the preview as prose, for a terminal user.
pub fn print_recovery_preview_report(report: &RecoveryPreviewReport) {
    println!("recovery goal:          {}", report.goal.description);
    println!(
        "current usage:          {:.1}% used ({} free of {})",
        report.current.used_percent, report.current.free_human, report.current.total_human
    );
    println!("pressure state:         {}", report.current.pressure_state);
    println!(
        "free needed for goal:   {} ({} bytes)",
        report.bytes_needed_human, report.bytes_needed
    );
    println!(
        "reclaimable now (est):  {} across {} candidate(s)",
        report.opportunity.actionable_now_human, report.opportunity.actionable_now_count
    );
    println!(
        "needs confirmation:     {} across {} candidate(s)",
        report.opportunity.requires_confirmation_human,
        report.opportunity.requires_confirmation_count
    );
    println!(
        "not executable:         {} candidate(s), {} protected",
        report.opportunity.not_executable_count, report.opportunity.protected_count
    );
    println!(
        "goal appears reachable: {} (estimate, not a measurement)",
        report.goal_appears_reachable
    );
    println!("nothing was modified:   this is a preview");
    for caveat in &report.caveats {
        println!("note: {caveat}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::dto::{DetectorHealthReport, OfferedAction};

    fn candidate(
        id: &str,
        label: &'static str,
        executable: bool,
        requires_confirmation: bool,
        bytes: Option<u64>,
        lower_bound: bool,
    ) -> DetectCandidateReport {
        DetectCandidateReport {
            resource_id: id.to_string(),
            kind: "cargo_target_dir",
            reclaimable_bytes: bytes,
            reclaimable_human: bytes.map(human_bytes),
            reclaimable_bytes_is_lower_bound: lower_bound,
            impact_tier: "normal",
            policy_label: label,
            reasons: vec![],
            executable,
            offered_actions: if executable {
                vec![OfferedAction {
                    action_id: "delete_cargo_target_dir".to_string(),
                    requires_confirmation,
                }]
            } else {
                vec![]
            },
            refusal_reason: if executable {
                None
            } else {
                Some("protected".to_string())
            },
        }
    }

    fn detect_report(candidates: Vec<DetectCandidateReport>, complete: bool) -> DetectReport {
        DetectReport {
            candidates,
            detectors: vec![DetectorHealthReport {
                detector: "cargo_target_dir".to_string(),
                status: if complete { "found" } else { "failed" },
                candidates_found: 0,
                reason: if complete {
                    None
                } else {
                    Some("probe exploded".to_string())
                },
            }],
            discovery_complete: complete,
        }
    }

    #[test]
    fn goal_report_carries_both_axes_and_the_sentence() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let report = build_recovery_goal_report(&goal);
        assert_eq!(report.used_percent, 60.0);
        assert_eq!(report.free_percent, 40.0);
        assert_eq!(report.description, "60% used (40% free)");
    }

    #[test]
    fn free_target_description_always_names_the_free_axis() {
        assert_eq!(
            describe_free_target(&FreeTarget::Percentage(20.0)),
            "20% free"
        );
        let bytes = describe_free_target(&FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024));
        assert!(bytes.ends_with(" free"), "{bytes}");
        // A raw target is a free-space floor in both forms, so neither may
        // ever be rendered as a used-percentage.
        assert!(!bytes.contains("used"), "{bytes}");
        assert!(
            !describe_free_target(&FreeTarget::Percentage(20.0)).contains("used"),
            "a percentage target must not read as percent used"
        );
    }

    #[test]
    fn opportunity_splits_actionable_from_confirmation_and_protected() {
        let candidates = vec![
            candidate("a", "AUTO_SAFE", true, false, Some(100), false),
            candidate("b", "AUTO_SAFE", true, false, Some(50), false),
            candidate("c", "ASK", true, true, Some(1000), false),
            candidate("d", "PROTECTED", false, false, Some(9999), false),
            candidate("e", "UNKNOWN_INCOMPLETE", false, false, None, false),
        ];
        let opportunity = build_recovery_opportunity(&candidates);
        assert_eq!(opportunity.actionable_now_count, 2);
        assert_eq!(opportunity.actionable_now_bytes, 150);
        assert_eq!(opportunity.requires_confirmation_count, 1);
        assert_eq!(opportunity.requires_confirmation_bytes, 1000);
        assert_eq!(opportunity.not_executable_count, 2);
        assert_eq!(opportunity.protected_count, 1);
        assert!(!opportunity.is_lower_bound);
    }

    /// The misread this split exists to prevent: protected space must never
    /// land in any byte total a user could read as reclaimable.
    #[test]
    fn protected_bytes_never_enter_any_opportunity_total() {
        let candidates = vec![candidate(
            "huge_protected",
            "PROTECTED",
            false,
            false,
            Some(900 * 1024 * 1024 * 1024),
            false,
        )];
        let opportunity = build_recovery_opportunity(&candidates);
        assert_eq!(opportunity.actionable_now_bytes, 0);
        assert_eq!(opportunity.requires_confirmation_bytes, 0);
        assert_eq!(opportunity.protected_count, 1);
    }

    #[test]
    fn lower_bound_flag_propagates_from_any_contributing_candidate() {
        let candidates = vec![
            candidate("a", "AUTO_SAFE", true, false, Some(100), false),
            candidate("b", "AUTO_SAFE", true, false, Some(100), true),
        ];
        assert!(build_recovery_opportunity(&candidates).is_lower_bound);
    }

    #[test]
    fn preview_reports_measured_shortfall_and_flags_reachability_as_an_estimate() {
        // 1000-byte volume, 60 free => 94% used. Goal 60% used needs 400 free.
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let usage = FsUsage::new(1000, 60);
        let detect = detect_report(
            vec![candidate("a", "AUTO_SAFE", true, false, Some(500), false)],
            true,
        );
        let preview = build_recovery_preview_report(
            &goal,
            &usage,
            &ThresholdConfig::default(),
            detect,
        );

        assert_eq!(preview.required_free_bytes, 400);
        assert_eq!(preview.bytes_needed, 340);
        assert!(preview.goal_appears_reachable);
        assert_eq!(preview.current.used_percent, 94.0);
        assert!(preview.discovery_complete);
        assert!(
            preview.caveats.iter().any(|c| c.contains("estimates")),
            "the estimates caveat must always be present: {:?}",
            preview.caveats
        );
    }

    #[test]
    fn confirmation_gated_space_does_not_make_a_goal_look_reachable() {
        // The only opportunity is confirmation-gated, so a run started now
        // would not automatically reclaim it.
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let usage = FsUsage::new(1000, 60);
        let detect = detect_report(
            vec![candidate("gated", "ASK", true, true, Some(10_000), false)],
            true,
        );
        let preview =
            build_recovery_preview_report(&goal, &usage, &ThresholdConfig::default(), detect);
        assert!(!preview.goal_appears_reachable);
        assert!(
            preview
                .caveats
                .iter()
                .any(|c| c.contains("need your confirmation")),
            "{:?}",
            preview.caveats
        );
    }

    #[test]
    fn incomplete_discovery_is_stated_in_the_preview_caveats() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let usage = FsUsage::new(1000, 60);
        let preview = build_recovery_preview_report(
            &goal,
            &usage,
            &ThresholdConfig::default(),
            detect_report(vec![], false),
        );
        assert!(!preview.discovery_complete);
        assert!(
            preview
                .caveats
                .iter()
                .any(|c| c.contains("Discovery was incomplete")
                    && c.contains("cargo_target_dir")),
            "{:?}",
            preview.caveats
        );
    }

    #[test]
    fn already_met_goal_needs_zero_bytes() {
        // 1000-byte volume, 500 free => 50% used, already under a 60% goal.
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let preview = build_recovery_preview_report(
            &goal,
            &FsUsage::new(1000, 500),
            &ThresholdConfig::default(),
            detect_report(vec![], true),
        );
        assert_eq!(preview.bytes_needed, 0);
        assert!(preview.goal_appears_reachable);
    }

    fn run_report(stop_reason: StopReason, final_free: u64, freed: u64) -> RecoveryReport {
        RecoveryReport {
            stop_reason,
            iterations_run: 3,
            actions_executed: 2,
            actions_declined_or_skipped: 1,
            total_bytes_freed: freed,
            started_free_bytes: 60,
            final_free_bytes: final_free,
            detector_failures: vec![],
        }
    }

    #[test]
    fn run_report_decides_target_met_from_remeasured_free_space() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let target = goal.to_free_target();
        // Reached: 400 free of 1000 is exactly 60% used.
        let met = build_recovery_run_report(
            Some(&goal),
            &target,
            1000,
            &run_report(StopReason::TargetReached, 400, 340),
        );
        assert!(met.target_met);
        assert_eq!(met.stop_reason, "target_reached");
        assert_eq!(met.goal.as_ref().unwrap().used_percent, 60.0);
        assert_eq!(met.target, "40% free");
    }

    /// The honesty case: a run can free a lot and still not reach the goal.
    /// `target_met` must follow the filesystem, not the byte total.
    #[test]
    fn a_large_measured_reclaim_that_misses_the_goal_reports_target_not_met() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let target = goal.to_free_target();
        let report = build_recovery_run_report(
            Some(&goal),
            &target,
            1000,
            // Freed 290 bytes, ending at 350 free — still 65% used.
            &run_report(StopReason::SafeExhausted, 350, 290),
        );
        assert!(!report.target_met);
        assert_eq!(report.bytes_freed_measured, 290);
        assert_eq!(report.stop_reason, "safe_exhausted");
    }

    #[test]
    fn no_stop_reason_detail_collapses_into_a_bare_success_word() {
        for reason in [
            StopReason::TargetReached,
            StopReason::SafeExhausted,
            StopReason::BudgetExceeded,
            StopReason::NoProgress,
            StopReason::Error("statfs failed".to_string()),
        ] {
            let detail = stop_reason_detail(&reason);
            assert!(detail.len() > 20, "{reason:?} -> {detail}");
            let lowered = detail.to_lowercase();
            assert!(
                !lowered.contains("done") && !lowered.contains("complete"),
                "{reason:?} renders as a bare completion: {detail}"
            );
            if reason != StopReason::TargetReached {
                assert!(
                    lowered.contains("before reaching the goal") || lowered.contains("could not"),
                    "a non-success stop must say it fell short: {detail}"
                );
            }
        }
    }

    #[test]
    fn error_stop_reason_carries_the_message_in_a_dedicated_field() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let report = build_recovery_run_report(
            Some(&goal),
            &goal.to_free_target(),
            1000,
            &run_report(StopReason::Error("statfs failed".to_string()), 60, 0),
        );
        assert_eq!(report.stop_reason, "error");
        assert_eq!(report.error.as_deref(), Some("statfs failed"));
        assert!(report.stop_reason_detail.contains("statfs failed"));
    }

    #[test]
    fn raw_target_run_has_no_goal_and_reports_the_free_axis() {
        let target = FreeTarget::Percentage(20.0);
        let report =
            build_recovery_run_report(None, &target, 1000, &run_report(StopReason::NoProgress, 60, 0));
        assert!(report.goal.is_none());
        assert_eq!(report.target, "20% free");
    }

    #[test]
    fn detector_failures_make_the_run_report_say_discovery_was_incomplete() {
        let mut inner = run_report(StopReason::SafeExhausted, 60, 0);
        inner.detector_failures = vec!["cargo_target_dir: probe exploded".to_string()];
        let report =
            build_recovery_run_report(None, &FreeTarget::Percentage(20.0), 1000, &inner);
        assert!(!report.discovery_complete);
        assert_eq!(report.detector_failures.len(), 1);
        assert!(
            !report.caveats.is_empty(),
            "a failed detector must produce caveat lines"
        );
    }
}
