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

use crate::executor::goal::{GoalRejection, RecoveryGoal};
use std::io::Write;
use std::sync::Mutex;

use crate::executor::recovery_loop::{
    target_met, FreeTarget, RecoveryObserver, RecoveryProgress, RecoveryReport, StopReason,
};
use crate::monitor::{FsUsage, ThresholdConfig};
use crate::reporting::dto::{
    stop_reason_tag, DetectCandidateReport, DetectReport, RecoveryGoalRejectionReport,
    RecoveryGoalReport, RecoveryOpportunityReport, RecoveryPreviewReport, RecoveryProgressEvent,
    RecoveryRemainingReport, RecoveryRunReport,
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

/// Project a refused goal into the shape a `--json` client parses.
///
/// Exists so a rejection is machine-readable: the alternative is a client
/// scraping the rejection sentence out of stderr, which is exactly the
/// "parse terminal prose" coupling the structured contract is meant to remove.
/// The optional fields are absent rather than zero-filled — `0.0` for a value
/// that was `NaN` would be a fabricated number.
pub fn build_goal_rejection_report(rejection: &GoalRejection) -> RecoveryGoalRejectionReport {
    let (goal_used_percent, current_used_percent) = match rejection {
        GoalRejection::NotFinite => (None, None),
        GoalRejection::OutOfRange { used_percent } => (Some(*used_percent), None),
        GoalRejection::NotAnImprovement {
            goal_used_percent,
            current_used_percent,
        } => (Some(*goal_used_percent), Some(*current_used_percent)),
    };
    RecoveryGoalRejectionReport {
        reason: rejection.as_str(),
        message: rejection.to_string(),
        goal_used_percent,
        current_used_percent,
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
        StopReason::SafeExhausted(_) => "The run stopped before reaching the goal because no \
             safe candidate remained among the detectors that answered."
            .to_string(),
        StopReason::BudgetExceeded => "The run stopped before reaching the goal because a run \
             limit was reached (iterations, actions, or elapsed time)."
            .to_string(),
        StopReason::NoProgress => "The run stopped before reaching the goal because recent \
             actions reclaimed no measurable space."
            .to_string(),
        // Says whose limit it was, because that is the whole point of the
        // variant (HORO-1510): this sentence is read by someone who may
        // otherwise conclude their disk has nothing safe left on it.
        StopReason::EnvelopeRefused(refusal) => format!(
            "The run stopped before reaching the goal because Autopilot reached the end of \
             what you authorized it to do: {refusal}. That is a limit on Autopilot, not a \
             finding about this disk — a recovery you start yourself is bounded only by what \
             you ask for."
        ),
        StopReason::StoppedByUser => "The run stopped before reaching the goal because you \
             asked it to stop; the action that was already running finished first."
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
    // A preview of an already-satisfied goal is a legitimate question to ask,
    // so this reports rather than refuses — but it must say what a real run
    // would do, because `free --goal-used-percent` refuses a goal that is not
    // an improvement (see `RecoveryGoal::progress_toward`). Without this
    // sentence the preview looks like a green light for a run that will exit
    // with a rejection.
    if bytes_needed == 0 {
        caveats.push(
            "This goal is already satisfied, so there is nothing to recover: starting a run \
             would be refused rather than reclaim anything."
                .to_string(),
        );
    }
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

/// The caveat sentences a finished run carries in its structured report.
///
/// Deliberately NOT [`RecoveryReport::discovery_caveat_lines`], which this
/// originally reused. Those lines are written for the terminal: they carry
/// column-alignment padding (`"discovery incomplete:   1 detector(s) failed"`),
/// a `"  - "` bullet prefix and a `"note: "` prefix, because the prose printer
/// puts them straight after a block of aligned label/value rows. A GUI showing
/// them renders the padding as a gap in the middle of a sentence, and a client
/// that stripped the prefixes back off would be parsing terminal formatting —
/// the exact coupling the structured contract exists to remove.
///
/// The failed detectors themselves are not repeated here. They are their own
/// field on [`RecoveryRunReport`], so a surface lists them from there; these
/// sentences say what their failure *means*, which is the part no client should
/// be deciding for itself.
fn build_run_caveats(report: &RecoveryReport) -> Vec<String> {
    if report.detector_failures.is_empty() {
        return Vec::new();
    }

    let mut caveats = vec![format!(
        "Discovery was incomplete: {} detector(s) failed, so what they would have \
         found is unknown and the real opportunity may be larger.",
        report.detector_failures.len()
    )];
    if matches!(report.stop_reason, StopReason::SafeExhausted(_)) {
        caveats.push(
            "This run stopped because no safe candidate remained among the detectors \
             that answered. That is not a finding that nothing safe is left."
                .to_string(),
        );
    }
    caveats
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
    // Projected from the variant that carries it, so the breakdown cannot
    // appear beside a stop reason that concluded nothing about what is left
    // (HORO-1509). The field names change here on purpose: the loop names its
    // own policy classes, the wire names what a user does next, and that is
    // the same vocabulary `RecoveryOpportunityReport` already uses.
    let remaining = match &report.stop_reason {
        StopReason::SafeExhausted(left_behind) => Some(RecoveryRemainingReport {
            requires_confirmation_count: left_behind.requires_confirmation,
            protected_count: left_behind.protected,
            not_executable_count: left_behind.not_executable,
        }),
        _ => None,
    };

    RecoveryRunReport {
        goal: goal.map(build_recovery_goal_report),
        target: describe_free_target(target),
        stop_reason: stop_reason_tag(&report.stop_reason),
        stop_reason_detail: stop_reason_detail(&report.stop_reason),
        error,
        remaining,
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
        caveats: build_run_caveats(report),
    }
}

/// Project one live [`RecoveryProgress`] event onto the wire (HORO-1509).
///
/// A one-for-one mapping with no judgment in it, and that is deliberate: this
/// function may not decide anything, summarise anything, or add a number the
/// loop did not measure. Its only additions are the `human_bytes` renderings,
/// so a client never formats a byte count that could disagree with the final
/// report's — the same reason every report DTO in this module carries its
/// `*_human` twin.
///
/// The optional human strings are built with `map`, so "not measured" stays
/// absent on both fields rather than becoming `"0 B"` — a string that would
/// read as a measurement of zero.
pub fn build_recovery_progress_event(progress: &RecoveryProgress) -> RecoveryProgressEvent {
    match progress {
        RecoveryProgress::Measured {
            iteration,
            usage,
            bytes_freed_so_far,
        } => RecoveryProgressEvent::Measured {
            iteration: *iteration,
            total_bytes: usage.total_bytes,
            free_bytes: usage.free_bytes,
            used_percent: usage.used_percent(),
            free_human: human_bytes(usage.free_bytes),
            bytes_freed_so_far: *bytes_freed_so_far,
            bytes_freed_so_far_human: human_bytes(*bytes_freed_so_far),
        },
        RecoveryProgress::Discovering { iteration } => RecoveryProgressEvent::Discovering {
            iteration: *iteration,
        },
        RecoveryProgress::Discovered {
            iteration,
            candidates,
            detectors_failed,
        } => RecoveryProgressEvent::Discovered {
            iteration: *iteration,
            candidates: *candidates,
            detectors_failed: *detectors_failed,
        },
        RecoveryProgress::Revalidating { iteration } => RecoveryProgressEvent::Revalidating {
            iteration: *iteration,
        },
        RecoveryProgress::ActionStarted {
            iteration,
            resource,
            action,
            policy_label,
            estimated_bytes,
        } => RecoveryProgressEvent::ActionStarted {
            iteration: *iteration,
            resource: resource.clone(),
            action: action.clone(),
            policy_label,
            estimated_bytes: *estimated_bytes,
            estimated_human: estimated_bytes.map(human_bytes),
        },
        RecoveryProgress::ActionFinished {
            iteration,
            resource,
            action,
            outcome,
            reclaimed_bytes,
            bytes_freed_so_far,
        } => RecoveryProgressEvent::ActionFinished {
            iteration: *iteration,
            resource: resource.clone(),
            action: action.clone(),
            outcome,
            reclaimed_bytes: *reclaimed_bytes,
            reclaimed_human: reclaimed_bytes.map(human_bytes),
            bytes_freed_so_far: *bytes_freed_so_far,
            bytes_freed_so_far_human: human_bytes(*bytes_freed_so_far),
        },
        RecoveryProgress::StopRequested { iteration } => RecoveryProgressEvent::StopRequested {
            iteration: *iteration,
        },
    }
}

/// A [`RecoveryObserver`] that writes each event to a sink as one NDJSON line
/// (HORO-1509) — what `glomeris free --progress-json` gives the GUI.
///
/// Generic over the sink so tests can read back exactly what a consumer would
/// receive; production passes `std::io::stderr()`, keeping the progress stream
/// off the stdout a `--json` report owns.
///
/// The [`Mutex`] is what makes "one event per line" true rather than intended:
/// [`RecoveryObserver::observe`] takes `&self`, so without it two events could
/// interleave halfway through a line and hand the consumer invalid JSON.
pub struct NdjsonProgressObserver<W> {
    sink: Mutex<W>,
}

impl<W: Write> NdjsonProgressObserver<W> {
    pub fn new(sink: W) -> Self {
        Self {
            sink: Mutex::new(sink),
        }
    }
}

impl<W: Write> RecoveryObserver for NdjsonProgressObserver<W> {
    /// Drops the line on any failure, and that is a safety decision rather
    /// than laziness.
    ///
    /// This is called from inside a loop that deletes things. The most likely
    /// failure by far is `EPIPE` — the GUI watching the run quit — and a run
    /// that aborted, panicked or exited there would be a run interrupted
    /// partway through a mutation because *nobody was watching*, which is
    /// precisely the ambiguous filesystem state the cooperative
    /// [`crate::executor::recovery_loop::StopSignal`] exists to avoid.
    ///
    /// Nothing is lost that decides anything: progress is advisory, and the
    /// authority on what happened is the audit log plus the final
    /// [`RecoveryRunReport`], neither of which goes through here. Writing a
    /// diagnostic instead would put a non-JSON line into the stream and break
    /// the one consumer still reading it.
    fn observe(&self, event: RecoveryProgress) {
        let Ok(line) = serde_json::to_string(&build_recovery_progress_event(&event)) else {
            return;
        };
        if let Ok(mut sink) = self.sink.lock() {
            // One `write_all` for line and terminator together, so a partial
            // write cannot leave a line without its newline.
            let _ = sink.write_all(format!("{line}\n").as_bytes());
            let _ = sink.flush();
        }
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
    use crate::executor::recovery_loop::RemainingCandidates;
    use crate::reporting::dto::{DetectorHealthReport, OfferedAction};

    /// `SafeExhausted` having left nothing behind — the right fixture wherever
    /// a test is about something other than the breakdown itself. The
    /// breakdown's own projection is pinned by
    /// `safe_exhausted_projects_what_the_run_left_behind`.
    fn safe_exhausted() -> StopReason {
        StopReason::SafeExhausted(RemainingCandidates::default())
    }

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
        let preview =
            build_recovery_preview_report(&goal, &usage, &ThresholdConfig::default(), detect);

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
                .any(|c| c.contains("Discovery was incomplete") && c.contains("cargo_target_dir")),
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
        // `goal_appears_reachable` being true must not read as "go ahead":
        // a real run refuses a goal that is not an improvement, and the
        // preview has to say so or it is a green light for an exit code 2.
        assert!(
            preview
                .caveats
                .iter()
                .any(|c| c.contains("already satisfied") && c.contains("refused")),
            "{:?}",
            preview.caveats
        );
    }

    #[test]
    fn a_goal_that_still_needs_bytes_is_not_labelled_already_satisfied() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let preview = build_recovery_preview_report(
            &goal,
            // 1000-byte volume, 60 free => 94% used, 340 bytes short.
            &FsUsage::new(1000, 60),
            &ThresholdConfig::default(),
            detect_report(vec![], true),
        );
        assert_eq!(preview.bytes_needed, 340);
        assert!(
            !preview
                .caveats
                .iter()
                .any(|c| c.contains("already satisfied")),
            "{:?}",
            preview.caveats
        );
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
            &run_report(safe_exhausted(), 350, 290),
        );
        assert!(!report.target_met);
        assert_eq!(report.bytes_freed_measured, 290);
        assert_eq!(report.stop_reason, "safe_exhausted");
    }

    /// HORO-1509: a client told "no safe candidate remained" must be able to
    /// tell the user what to do next, which means the counts reach the wire.
    ///
    /// The second half is the load-bearing half. Zeros on a `target_reached`
    /// run would read as "we looked and found nothing left", a claim that run
    /// never made, so the field is absent rather than defaulted — and a
    /// `Serialize` DTO makes absence the client's problem to handle only if it
    /// is genuinely absent.
    #[test]
    fn safe_exhausted_projects_what_the_run_left_behind() {
        let target = FreeTarget::Percentage(20.0);
        let exhausted = build_recovery_run_report(
            None,
            &target,
            1000,
            &run_report(
                StopReason::SafeExhausted(RemainingCandidates {
                    requires_confirmation: 2,
                    protected: 3,
                    not_executable: 1,
                    not_permitted_by_autopilot: 0,
                }),
                60,
                0,
            ),
        );
        let remaining = exhausted
            .remaining
            .expect("a safe_exhausted run must say what is still there");
        assert_eq!(remaining.requires_confirmation_count, 2);
        assert_eq!(remaining.protected_count, 3);
        assert_eq!(remaining.not_executable_count, 1);

        for reason in [
            StopReason::TargetReached,
            StopReason::BudgetExceeded,
            StopReason::NoProgress,
            StopReason::StoppedByUser,
            StopReason::Error("statfs failed".to_string()),
        ] {
            let other = build_recovery_run_report(None, &target, 1000, &run_report(reason, 60, 0));
            assert!(
                other.remaining.is_none(),
                "{} concluded nothing about what is left, so it must claim nothing",
                other.stop_reason
            );
        }
    }

    #[test]
    fn no_stop_reason_detail_collapses_into_a_bare_success_word() {
        for reason in [
            StopReason::TargetReached,
            safe_exhausted(),
            StopReason::BudgetExceeded,
            StopReason::NoProgress,
            StopReason::StoppedByUser,
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
        let report = build_recovery_run_report(
            None,
            &target,
            1000,
            &run_report(StopReason::NoProgress, 60, 0),
        );
        assert!(report.goal.is_none());
        assert_eq!(report.target, "20% free");
    }

    #[test]
    fn detector_failures_make_the_run_report_say_discovery_was_incomplete() {
        let mut inner = run_report(safe_exhausted(), 60, 0);
        inner.detector_failures = vec!["cargo_target_dir: probe exploded".to_string()];
        let report = build_recovery_run_report(None, &FreeTarget::Percentage(20.0), 1000, &inner);
        assert!(!report.discovery_complete);
        assert_eq!(report.detector_failures.len(), 1);
        assert!(
            !report.caveats.is_empty(),
            "a failed detector must produce caveat lines"
        );
        assert!(
            report
                .caveats
                .iter()
                .any(|c| c.contains("no safe candidate remained")),
            "SafeExhausted after a failed probe must say it is not a finding that \
             nothing safe is left: {:?}",
            report.caveats
        );
    }

    /// The structured caveats are sentences, not the terminal's lines.
    ///
    /// This is the assertion that keeps `build_run_caveats` from being
    /// "simplified" back into a call to `discovery_caveat_lines()`, which is
    /// where these strings used to come from. Those lines carry alignment
    /// padding and `"  - "`/`"note: "` prefixes for the prose printer, and a
    /// client that stripped them back off would be parsing terminal
    /// formatting.
    #[test]
    fn run_caveats_carry_no_terminal_formatting() {
        let mut inner = run_report(safe_exhausted(), 60, 0);
        inner.detector_failures = vec![
            "cargo_target_dir: probe exploded".to_string(),
            "homebrew_cache: brew --cache exited 1".to_string(),
        ];
        let report = build_recovery_run_report(None, &FreeTarget::Percentage(20.0), 1000, &inner);

        for caveat in &report.caveats {
            assert!(
                !caveat.contains("  "),
                "a caveat must not carry column padding: {caveat:?}"
            );
            assert!(
                !caveat.starts_with('-') && !caveat.starts_with("note:"),
                "a caveat must not carry a prose-printer prefix: {caveat:?}"
            );
            assert!(
                caveat.ends_with('.'),
                "a caveat must be a sentence: {caveat:?}"
            );
        }

        // The names live in `detector_failures`; the caveats explain what their
        // absence means. Repeating them here would make one of the two the
        // place a surface reads, and nothing would say which.
        assert!(
            report
                .caveats
                .iter()
                .all(|c| !c.contains("probe exploded") && !c.contains("brew --cache")),
            "caveats must not duplicate the detector_failures list: {:?}",
            report.caveats
        );
        assert_eq!(report.detector_failures.len(), 2);
    }

    #[test]
    fn a_rejection_report_carries_the_tag_the_sentence_and_both_numbers() {
        let report = build_goal_rejection_report(&GoalRejection::NotAnImprovement {
            goal_used_percent: 80.0,
            current_used_percent: 60.0,
        });
        assert_eq!(report.reason, "not_an_improvement");
        assert_eq!(report.goal_used_percent, Some(80.0));
        assert_eq!(report.current_used_percent, Some(60.0));
        // The sentence is the rejection's own, not a second wording of it.
        assert_eq!(
            report.message,
            GoalRejection::NotAnImprovement {
                goal_used_percent: 80.0,
                current_used_percent: 60.0,
            }
            .to_string()
        );
    }

    #[test]
    fn a_rejection_with_no_usable_number_reports_no_number_rather_than_zero() {
        let not_finite = build_goal_rejection_report(&GoalRejection::NotFinite);
        assert_eq!(not_finite.reason, "not_finite");
        assert_eq!(
            not_finite.goal_used_percent, None,
            "a NaN goal has no finite value to report; 0.0 would be invented"
        );
        assert_eq!(not_finite.current_used_percent, None);

        // An out-of-range goal has a goal figure but was never compared to a
        // volume, so there is no current usage to report either.
        let out_of_range = build_goal_rejection_report(&GoalRejection::OutOfRange {
            used_percent: 150.0,
        });
        assert_eq!(out_of_range.goal_used_percent, Some(150.0));
        assert_eq!(out_of_range.current_used_percent, None);
    }

    #[test]
    fn rejection_json_omits_absent_numbers_and_never_says_free() {
        let json =
            serde_json::to_string(&build_goal_rejection_report(&GoalRejection::NotFinite)).unwrap();
        assert!(!json.contains("goal_used_percent"), "{json}");
        assert!(!json.contains("current_used_percent"), "{json}");
        // Every rejection is a used-axis statement; a stray "free" here is the
        // ambiguity HORO-1506 exists to remove.
        for rejection in [
            GoalRejection::NotFinite,
            GoalRejection::OutOfRange {
                used_percent: 150.0,
            },
            GoalRejection::NotAnImprovement {
                goal_used_percent: 80.0,
                current_used_percent: 60.0,
            },
        ] {
            let json = serde_json::to_string(&build_goal_rejection_report(&rejection)).unwrap();
            assert!(!json.contains("free"), "{json}");
        }
    }

    /// Every [`RecoveryProgress`] the loop can emit, in the order a run emits
    /// them. Written out exhaustively on purpose: a new variant added to the
    /// domain enum fails `build_recovery_progress_event`'s `match` at compile
    /// time, and the tests below make sure it also has to be *named* on the
    /// wire before it can ship.
    fn every_progress_event() -> Vec<RecoveryProgress> {
        vec![
            RecoveryProgress::Measured {
                iteration: 1,
                usage: FsUsage::new(1_000, 250),
                bytes_freed_so_far: 0,
            },
            RecoveryProgress::Discovering { iteration: 1 },
            RecoveryProgress::Discovered {
                iteration: 1,
                candidates: 4,
                detectors_failed: 1,
            },
            RecoveryProgress::Revalidating { iteration: 1 },
            RecoveryProgress::ActionStarted {
                iteration: 1,
                resource: "/tmp/p/node_modules".to_string(),
                action: "node.clean.node_modules".to_string(),
                policy_label: "AUTO_SAFE",
                estimated_bytes: Some(2048),
            },
            RecoveryProgress::ActionFinished {
                iteration: 1,
                resource: "/tmp/p/node_modules".to_string(),
                action: "node.clean.node_modules".to_string(),
                outcome: "success",
                reclaimed_bytes: Some(2000),
                bytes_freed_so_far: 2000,
            },
            RecoveryProgress::StopRequested { iteration: 2 },
        ]
    }

    /// HORO-1509 AC: the loop is watchable, which means a client can tell one
    /// phase from another and always knows which iteration it is looking at.
    ///
    /// The distinctness assertion is the load-bearing one. Two phases sharing a
    /// tag would leave a UI unable to tell "we are scanning" from "we are
    /// deleting" — and `#[serde(tag = "phase")]` would serialize that happily.
    #[test]
    fn every_progress_phase_is_distinctly_named_and_carries_its_iteration() {
        let mut phases = std::collections::BTreeSet::new();
        for progress in every_progress_event() {
            let json = serde_json::to_string(&build_recovery_progress_event(&progress)).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            let phase = parsed["phase"]
                .as_str()
                .unwrap_or_else(|| panic!("no phase tag: {json}"))
                .to_string();
            assert!(
                phase.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "a phase tag is a stable token, not a Rust variant name: {phase}"
            );
            assert!(
                parsed["iteration"].is_u64(),
                "a progress line with no iteration cannot be placed in the run: {json}"
            );
            assert!(phases.insert(phase.clone()), "duplicate phase tag {phase}");
            // NDJSON: one event per line means no event may contain one.
            assert!(
                !json.contains('\n'),
                "an embedded newline splits the stream: {json}"
            );
        }
        assert_eq!(phases.len(), 7, "got: {phases:?}");
    }

    /// The measured reading is the one place a client learns current usage, so
    /// it must arrive on both axes and pre-rendered.
    #[test]
    fn a_measured_event_carries_the_usage_it_read_and_the_bytes_it_has_freed() {
        let event = build_recovery_progress_event(&RecoveryProgress::Measured {
            iteration: 3,
            usage: FsUsage::new(1_000, 250),
            bytes_freed_so_far: 4096,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["phase"], "measured");
        assert_eq!(json["iteration"], 3);
        assert_eq!(json["total_bytes"], 1_000);
        assert_eq!(json["free_bytes"], 250);
        assert_eq!(json["used_percent"], 75.0);
        assert_eq!(json["bytes_freed_so_far"], 4096);
        // Rendered by Rust so a 1000-based client formatter cannot disagree
        // with the final report about the same number.
        assert_eq!(json["free_human"], human_bytes(250));
        assert_eq!(json["bytes_freed_so_far_human"], human_bytes(4096));
    }

    /// HORO-1509 / campaign section 9: an unmeasurable byte count is absent,
    /// never `0`.
    ///
    /// `"0 B"` on an action whose size could not be determined is a measurement
    /// nobody took, and a progress card showing it would be telling the user the
    /// deletion achieved nothing. Both halves of the pair have to disappear
    /// together — a `reclaimed_human` with no `reclaimed_bytes` beside it would
    /// be a number with no provenance.
    #[test]
    fn unmeasured_byte_counts_are_omitted_rather_than_reported_as_zero() {
        let started = serde_json::to_string(&build_recovery_progress_event(
            &RecoveryProgress::ActionStarted {
                iteration: 1,
                resource: "/tmp/p/node_modules".to_string(),
                action: "node.clean.node_modules".to_string(),
                policy_label: "ASK",
                estimated_bytes: None,
            },
        ))
        .unwrap();
        assert!(!started.contains("estimated_bytes"), "{started}");
        assert!(!started.contains("estimated_human"), "{started}");
        assert!(!started.contains("0 B"), "{started}");

        let finished = serde_json::to_string(&build_recovery_progress_event(
            &RecoveryProgress::ActionFinished {
                iteration: 1,
                resource: "/tmp/p/node_modules".to_string(),
                action: "node.clean.node_modules".to_string(),
                outcome: "success",
                reclaimed_bytes: None,
                bytes_freed_so_far: 0,
            },
        ))
        .unwrap();
        assert!(!finished.contains("reclaimed_bytes"), "{finished}");
        assert!(!finished.contains("reclaimed_human"), "{finished}");
        // `bytes_freed_so_far` is a different claim: the run really has freed
        // nothing measurable yet, and saying so is honest.
        assert!(finished.contains("\"bytes_freed_so_far\":0"), "{finished}");
    }

    /// An estimate must be legible as an estimate, and progress must be legible
    /// as measured. The two live in the same stream, so the names are the only
    /// thing keeping a client from accumulating the wrong one.
    #[test]
    fn an_estimate_is_named_as_one_and_progress_is_named_as_measured() {
        let started = serde_json::to_string(&build_recovery_progress_event(
            &RecoveryProgress::ActionStarted {
                iteration: 1,
                resource: "/tmp/p/node_modules".to_string(),
                action: "node.clean.node_modules".to_string(),
                policy_label: "AUTO_SAFE",
                estimated_bytes: Some(2048),
            },
        ))
        .unwrap();
        assert!(started.contains("\"estimated_bytes\":2048"), "{started}");
        assert!(
            !started.contains("bytes_freed_so_far"),
            "an action that has not run yet has freed nothing, and must claim nothing: {started}"
        );

        let finished = serde_json::to_string(&build_recovery_progress_event(
            &RecoveryProgress::ActionFinished {
                iteration: 1,
                resource: "/tmp/p/node_modules".to_string(),
                action: "node.clean.node_modules".to_string(),
                outcome: "success",
                reclaimed_bytes: Some(2000),
                bytes_freed_so_far: 2000,
            },
        ))
        .unwrap();
        assert!(!finished.contains("estimated"), "{finished}");
        assert!(finished.contains("\"reclaimed_bytes\":2000"), "{finished}");
    }

    /// A sink whose bytes stay readable while the observer still owns it —
    /// `NdjsonProgressObserver` takes the writer by value, and a test needs to
    /// see what a consumer would have seen *during* the run, not afterwards.
    #[derive(Clone, Default)]
    struct SharedSink(std::sync::Arc<Mutex<Vec<u8>>>);

    impl SharedSink {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A sink that refuses everything, standing in for the overwhelmingly
    /// likely real failure: the GUI watching a run quit and the pipe closed.
    struct BrokenSink;

    impl Write for BrokenSink {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "nobody is listening",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "nobody is listening",
            ))
        }
    }

    /// HORO-1509 AC: the stream is NDJSON — one complete, independently
    /// parseable object per line, arriving as the run proceeds.
    ///
    /// Parsing each line separately is the whole assertion: a consumer reads
    /// this incrementally and cannot wait for a closing bracket that a run still
    /// deleting things has not written.
    #[test]
    fn the_observer_writes_one_parseable_json_object_per_line() {
        let sink = SharedSink::default();
        let observer = NdjsonProgressObserver::new(sink.clone());
        let events = every_progress_event();
        for event in events.iter().cloned() {
            observer.observe(event);
        }

        let text = sink.text();
        assert!(
            text.ends_with('\n'),
            "a line without its terminator: {text:?}"
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), events.len(), "one line per event: {text:?}");
        for line in lines {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not standalone JSON ({e}): {line}"));
            assert!(parsed["phase"].is_string(), "{line}");
        }
    }

    /// Progress must be visible while the run is still running, so each line is
    /// written the moment its event happens rather than at drop.
    ///
    /// A buffered observer would pass the test above and still leave a UI
    /// showing a spinner for the whole run — the exact failure HORO-1509 exists
    /// to fix.
    #[test]
    fn each_event_reaches_the_sink_before_the_next_one_happens() {
        let sink = SharedSink::default();
        let observer = NdjsonProgressObserver::new(sink.clone());

        observer.observe(RecoveryProgress::Discovering { iteration: 1 });
        assert_eq!(
            sink.text().lines().count(),
            1,
            "the first event was still buffered when the second was emitted"
        );
        observer.observe(RecoveryProgress::Revalidating { iteration: 1 });
        assert_eq!(sink.text().lines().count(), 2);
    }

    /// A closed pipe must not disturb a run that is deleting things.
    ///
    /// The GUI quitting is the likely cause, and a run that aborted there would
    /// be a mutation interrupted because nobody was watching — the ambiguous
    /// state the cooperative stop signal exists to avoid. Progress decides
    /// nothing; the audit log and the final report are the authority.
    #[test]
    fn a_sink_that_refuses_every_write_does_not_disturb_the_run() {
        let observer = NdjsonProgressObserver::new(BrokenSink);
        for event in every_progress_event() {
            observer.observe(event);
        }
    }

    /// The two NDJSON streams share the `phase` key, so a client reading both
    /// (the GUI runs `detect` and `free`) must never mistake one for the other.
    #[test]
    fn recovery_phases_never_collide_with_discovery_phases() {
        use crate::reporting::dto::ProgressEvent;

        let discovery = [
            ProgressEvent::DetectorStarted { detector: "cargo" },
            ProgressEvent::DetectorFinished {
                detector: "cargo",
                candidates_found: 1,
                outcome: "found",
                reason: None,
            },
        ];
        let discovery_phases: Vec<String> = discovery
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["phase"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();

        for progress in every_progress_event() {
            let phase = serde_json::to_value(build_recovery_progress_event(&progress)).unwrap()
                ["phase"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                !discovery_phases.contains(&phase),
                "{phase} means one thing in a discovery scan and another in a run"
            );
        }
    }
}
