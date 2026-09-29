//! Report building for the workflow-profile surface (HORO-1547).
//!
//! The projection layer between [`crate::workspace::history`] and what
//! `glomeris workflow-profile` prints, in prose and `--json`. Same division of
//! labour as [`crate::cli::external_context`]: the domain owns the rules, this
//! module owns shape, and `main.rs` owns exit codes.
//!
//! # The wording rule this module is where somebody would break
//!
//! HORO-1547's seventh acceptance criterion: the report never labels the user by
//! skill or competence. [`crate::workspace::history::classify`] cannot break it,
//! because its vocabulary has no such word in it — but prose can, and this is
//! the only place that writes prose about the baseline. So the sentences below
//! describe layouts and counts, never the person operating them, and
//! `scripts/check-workflow-history-labels-no-one.sh` checks mechanically that
//! no banned word appears in this file or the ones around it.
//!
//! The temptation is real and worth naming: "parallel_multi_worktree" is the
//! shape of somebody juggling six checkouts, and calling that "advanced" would
//! read as a compliment. It would also make "serial_single_checkout" a
//! judgement, and then an `unknown` baseline a verdict on somebody the product
//! has barely observed.

use crate::evidence::probe::ProbeOutcome;
use crate::reporting::dto::{WorkflowProfileReport, WorkflowRecordReport, WorkflowSupportReport};
use crate::workspace::history::store::{
    Admission, StoreState, MIN_ADMISSION_INTERVAL_SECS, RETENTION_WINDOW_SECS,
};
use crate::workspace::history::WorkspaceObservation;
use crate::workspace::{WorkflowSupport, MIN_OBSERVATIONS_FOR_A_PATTERN};

/// The one sentence every rendering of this baseline carries.
///
/// Kept as a constant so the JSON, the prose and any future surface say the
/// same thing, and so a change to it is a visible one-line diff rather than
/// three drifting paraphrases.
pub const AUTHORITY_NOTE: &str =
    "This baseline explains and orders. It is never permission: a working tree that is dirty, \
     in use, or holds commits that exist nowhere else stays protected by its own evidence, \
     whatever the pattern here says.";

/// Project a store state into the shape both renderings read.
///
/// Pure. `stored_at` and `now_unix_secs` are supplied by the caller for the same
/// reason [`crate::cli::settings::build_settings_report`] takes its `stored_at`:
/// a function that reports on a file must not be the one resolving it, and a
/// function whose output depends on the clock must not be the one reading it.
pub fn build_workflow_profile_report(
    state: &StoreState,
    stored_at: Option<String>,
    now_unix_secs: u64,
) -> WorkflowProfileReport {
    let summary = crate::workspace::history::classify(state, now_unix_secs);

    // Deliberately read off the store state rather than off the classified
    // outcome. `Unavailable(NotAttempted)` is what a never-collected store
    // produces *and* what an unreadable one could produce if a future reason
    // were added carelessly, so the three-way distinction is taken from the
    // value that actually holds it.
    let (state_tag, unreadable_reason) = match state {
        StoreState::NeverCollected => ("never_collected", None),
        StoreState::Unreadable(reason) => ("unreadable", Some(reason.tag())),
        StoreState::Collected(_) => ("collected", None),
    };

    let (mode, confidence, observation_count, support) = match &summary {
        ProbeOutcome::Observed(summary) => (
            summary.mode.tag(),
            summary.confidence.tag(),
            summary.observation_count,
            summary.support,
        ),
        // No summary to report. The mode is still the honest `unknown` rather
        // than a placeholder, and the counts are zeros beside a `state` that
        // says why there are none.
        ProbeOutcome::Unavailable(_) => (
            crate::workspace::WorkflowMode::Unknown.tag(),
            crate::workspace::HistoryConfidence::Insufficient.tag(),
            0,
            WorkflowSupport::nothing_observed(),
        ),
    };

    WorkflowProfileReport {
        state: state_tag,
        unreadable_reason,
        stored_at,
        mode,
        confidence,
        observation_count,
        observations_still_needed: MIN_OBSERVATIONS_FOR_A_PATTERN.saturating_sub(observation_count),
        minimum_interval_secs: MIN_ADMISSION_INTERVAL_SECS,
        retention_days: RETENTION_WINDOW_SECS / (24 * 60 * 60),
        support: support_report(&support),
        authority: AUTHORITY_NOTE,
    }
}

/// Project the outcome of a recording run.
pub fn build_workflow_record_report(
    admission: Admission,
    observation: &WorkspaceObservation,
    stored: &[WorkspaceObservation],
    stored_at: Option<String>,
    now_unix_secs: u64,
) -> WorkflowRecordReport {
    let (tag, seconds_until_eligible) = match admission {
        Admission::Admitted => ("admitted", None),
        Admission::TooSoon {
            seconds_until_eligible,
        } => ("too_soon", Some(seconds_until_eligible)),
        Admission::ClockWentBackwards => ("clock_went_backwards", None),
    };

    WorkflowRecordReport {
        admission: tag,
        seconds_until_eligible,
        repositories_seen: observation.repositories.len() as u32,
        observations_stored: stored.len() as u32,
        profile: build_workflow_profile_report(
            &StoreState::Collected(stored.to_vec()),
            stored_at,
            now_unix_secs,
        ),
    }
}

fn support_report(support: &WorkflowSupport) -> WorkflowSupportReport {
    WorkflowSupportReport {
        spanning_days: support.spanning_days,
        repositories_observed: support.repositories_observed,
        parallel_observations: support.parallel_observations,
        serial_observations: support.serial_observations,
        mixed_observations: support.mixed_observations,
        single_checkout_branch_changes: support.single_checkout_branch_changes,
        most_worktrees_seen_at_once: support.most_worktrees_seen_at_once,
    }
}

/// What the mode word means, in a clause a reader does not have to look up.
///
/// Descriptions of a layout. None of them is the good one, and there is
/// deliberately no ordering between them — see the module header.
///
/// `unknown` is described without a cause on purpose. It has two, they have
/// different remedies, and a clause here can only name one of them: this
/// function is given a word, not the counts behind it. It used to say "not
/// enough observations to say", which beside `Confidence: observed (5
/// observations)` reads as a contradiction — five looks that each found no
/// repository are enough looks and still no shape. Both causes are stated as
/// their own lines in [`describe_workflow_profile`], where the counts are in
/// hand.
pub fn describe_mode(mode: &str) -> &'static str {
    match mode {
        "serial_single_checkout" => "one checkout at a time, staying on one branch",
        "serial_multi_branch" => "one checkout at a time, moving between branches",
        "parallel_multi_worktree" => "several working trees of a repository at once",
        "mixed" => "some repositories with several working trees, some with one",
        _ => "no layout claimed from what has been seen so far",
    }
}

/// The prose `workflow-profile` prints, one line per element.
pub fn describe_workflow_profile(report: &WorkflowProfileReport) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("WORKFLOW BASELINE".to_string());

    match report.state {
        "never_collected" => {
            lines.push(
                "  No observations recorded yet. Run `glomeris workflow-profile record` to take \
                 the first one."
                    .to_string(),
            );
        }
        "unreadable" => {
            // Named as a read failure, not as an empty baseline. The two have
            // different next steps, and this is the line that tells them apart.
            lines.push(format!(
                "  The stored baseline could not be read ({}). Nothing is assumed from its \
                 absence.",
                report.unreadable_reason.unwrap_or("unknown")
            ));
        }
        _ => {
            lines.push(format!(
                "  Shape: {} — {}",
                report.mode,
                describe_mode(report.mode)
            ));
            lines.push(format!(
                "  Confidence: {} ({} observation{})",
                report.confidence,
                report.observation_count,
                if report.observation_count == 1 {
                    ""
                } else {
                    "s"
                }
            ));
            if report.observations_still_needed > 0 {
                lines.push(format!(
                    "  {} more observation{} needed before any shape is claimed.",
                    report.observations_still_needed,
                    if report.observations_still_needed == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
            }
            // The other reason a shape is `unknown`, and the one a count of
            // observations hides: looks were taken and none of them found a
            // repository. More looks in the same place will not produce a shape
            // either, so leaving this to be inferred from `repositories: 0`
            // further down sends a reader off to record again for nothing.
            //
            // `repositories_observed == 0` with at least one observation is
            // exactly that case: a look that sees a repository contributes to
            // the serial, parallel or mixed count, so a mode stays `unknown`
            // only while every look saw none.
            if report.observation_count > 0 && report.support.repositories_observed == 0 {
                lines.push(
                    "  No observation found a repository, so there is no layout to describe. \
                     Recording from a directory that holds one is what would change this."
                        .to_string(),
                );
            }
        }
    }

    if report.state == "collected" {
        let s = &report.support;
        lines.push("  Observed:".to_string());
        lines.push(format!("    spanning days: {}", s.spanning_days));
        lines.push(format!("    repositories: {}", s.repositories_observed));
        lines.push(format!(
            "    looks with one working tree each: {}",
            s.serial_observations
        ));
        lines.push(format!(
            "    looks with several working trees: {}",
            s.parallel_observations
        ));
        lines.push(format!("    looks with both: {}", s.mixed_observations));
        lines.push(format!(
            "    branch changes in a single checkout: {}",
            s.single_checkout_branch_changes
        ));
        lines.push(format!(
            "    most working trees seen at once: {}",
            s.most_worktrees_seen_at_once
        ));
    }

    lines.push(format!(
        "  Recording: at most one observation every {} minutes; records kept {} days.",
        report.minimum_interval_secs / 60,
        report.retention_days
    ));
    if let Some(path) = &report.stored_at {
        lines.push(format!("  Stored at: {path}"));
    }
    lines.push("  Local only. No path, branch name or repository name is stored.".to_string());
    lines.push(format!("  {}", report.authority));
    lines
}

/// The prose `workflow-profile record` prints.
pub fn describe_workflow_record(report: &WorkflowRecordReport) -> Vec<String> {
    let mut lines = Vec::new();
    match report.admission {
        "admitted" => lines.push(format!(
            "Recorded one observation of {} repositor{}. The baseline now holds {}.",
            report.repositories_seen,
            if report.repositories_seen == 1 {
                "y"
            } else {
                "ies"
            },
            report.observations_stored
        )),
        // Phrased as the rule doing its job, because it is. A user who reads
        // this as a failure will run it again in a loop, which is the exact
        // thing the interval exists to make useless.
        "too_soon" => lines.push(format!(
            "Not recorded: the last observation is too recent. One more would be admitted in \
             about {} minutes. Spacing is what stops repeated runs from looking like a habit.",
            report.seconds_until_eligible.unwrap_or(0).div_ceil(60)
        )),
        _ => lines.push(
            "Not recorded: the newest stored observation is stamped later than now. The clock \
             moved backwards, and guessing which stamp to trust would be worse than declining."
                .to_string(),
        ),
    }
    lines.push(String::new());
    lines.extend(describe_workflow_profile(&report.profile));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests need these two; at file level they would be unused in the
    // non-test build of the library, which `-D warnings` makes a hard failure.
    use crate::evidence::probe::ProbeReason;
    use crate::workspace::history::alias::LocalAlias;
    use crate::workspace::history::observation::RepositoryObservation;
    use crate::workspace::WorkflowHistorySummary;

    const DAY: u64 = 24 * 60 * 60;
    const NOW: u64 = 1_800_000_000;

    fn observation(days_ago: u64, worktrees: u32) -> WorkspaceObservation {
        WorkspaceObservation {
            at_unix_secs: NOW - days_ago * DAY,
            repositories: vec![RepositoryObservation {
                repository: LocalAlias::of_name("app"),
                worktree_count: worktrees,
                linked_worktree_count: worktrees.saturating_sub(1),
                detached_worktree_count: 0,
                single_checkout_branch: (worktrees == 1).then(|| LocalAlias::of_name("trunk")),
            }],
        }
    }

    fn settled() -> StoreState {
        StoreState::Collected(vec![
            observation(3, 4),
            observation(2, 4),
            observation(1, 4),
        ])
    }

    #[test]
    fn a_settled_baseline_reports_its_shape_and_the_counts_behind_it() {
        let report = build_workflow_profile_report(&settled(), None, NOW);
        assert_eq!(report.state, "collected");
        assert_eq!(report.mode, "parallel_multi_worktree");
        assert_eq!(report.confidence, "observed");
        assert_eq!(report.observation_count, 3);
        assert_eq!(report.observations_still_needed, 0);
        assert_eq!(report.support.most_worktrees_seen_at_once, 4);
    }

    /// The three states stay three in the report, not just in the domain.
    #[test]
    fn the_three_store_states_are_three_different_reports() {
        let never = build_workflow_profile_report(&StoreState::NeverCollected, None, NOW);
        let unreadable = build_workflow_profile_report(
            &StoreState::Unreadable(ProbeReason::PermissionDenied),
            None,
            NOW,
        );
        let empty = build_workflow_profile_report(&StoreState::Collected(Vec::new()), None, NOW);

        assert_eq!(never.state, "never_collected");
        assert_eq!(unreadable.state, "unreadable");
        assert_eq!(unreadable.unreadable_reason, Some("permission_denied"));
        assert_eq!(empty.state, "collected");
        assert_eq!(never.unreadable_reason, None);

        for report in [&never, &unreadable, &empty] {
            assert_eq!(
                report.mode, "unknown",
                "a report with no observations named a shape"
            );
            assert_eq!(report.confidence, "insufficient");
        }
    }

    #[test]
    fn a_thin_baseline_says_how_many_more_are_needed() {
        let report = build_workflow_profile_report(
            &StoreState::Collected(vec![observation(1, 9)]),
            None,
            NOW,
        );
        assert_eq!(report.confidence, "insufficient");
        assert_eq!(report.mode, "unknown");
        assert_eq!(
            report.observations_still_needed,
            MIN_OBSERVATIONS_FOR_A_PATTERN - 1
        );
        assert_eq!(
            report.support.parallel_observations, 1,
            "the counts must survive an insufficient verdict, or the verdict is unexplained"
        );
    }

    /// A look that found no repository at all.
    fn nothing_seen(days_ago: u64) -> WorkspaceObservation {
        WorkspaceObservation {
            at_unix_secs: NOW - days_ago * DAY,
            repositories: Vec::new(),
        }
    }

    /// Five looks that each found no repository are enough looks, so the shape
    /// line must not blame the number of them.
    ///
    /// Found by running the command: the live output read `Shape: unknown — not
    /// enough observations to say` directly above `Confidence: observed (5
    /// observations)`. One line said there were too few and the next said there
    /// were enough, because both causes of `unknown` had collapsed into the only
    /// clause a mode word can carry.
    #[test]
    fn enough_looks_that_saw_nothing_are_not_described_as_too_few_looks() {
        let report = build_workflow_profile_report(
            &StoreState::Collected((0..5).map(nothing_seen).collect()),
            None,
            NOW,
        );
        // The pairing that made the contradiction visible, pinned so this test
        // fails if either half of it stops holding.
        assert_eq!(report.mode, "unknown");
        assert_eq!(report.confidence, "observed");
        assert_eq!(report.observation_count, 5);
        assert_eq!(report.observations_still_needed, 0);
        assert_eq!(report.support.repositories_observed, 0);

        let text = describe_workflow_profile(&report).join("\n");
        assert!(
            !text.contains("not enough observations") && !text.contains("more observation"),
            "five observations were described as too few: {text}"
        );
        assert!(
            text.contains("No observation found a repository"),
            "the shape is unknown and nothing says why: {text}"
        );
    }

    /// One look that found no repository has both causes at once, and says both.
    ///
    /// The two lines are not alternatives and must not be made exclusive: a
    /// second and third look would clear the count, and would still produce no
    /// shape while they keep finding nothing.
    #[test]
    fn a_thin_baseline_that_saw_nothing_gives_both_reasons() {
        let report =
            build_workflow_profile_report(&StoreState::Collected(vec![nothing_seen(0)]), None, NOW);
        assert_eq!(report.confidence, "insufficient");
        let text = describe_workflow_profile(&report).join("\n");
        assert!(
            text.contains("2 more observations needed"),
            "a single look was not described as a thin baseline: {text}"
        );
        assert!(
            text.contains("No observation found a repository"),
            "a look that saw nothing was described only as a thin baseline, so \
             recording twice more reads as the remedy: {text}"
        );
    }

    /// The prose for a store that could not be read must not read like a store
    /// with nothing in it. This is AC 5 at the surface a person actually sees.
    #[test]
    fn unreadable_prose_does_not_read_as_an_empty_baseline() {
        let lines = describe_workflow_profile(&build_workflow_profile_report(
            &StoreState::Unreadable(ProbeReason::Failed),
            None,
            NOW,
        ));
        let text = lines.join("\n");
        assert!(text.contains("could not be read"), "{text}");
        assert!(
            !text.contains("No observations recorded yet"),
            "an unreadable baseline was described as never collected: {text}"
        );
    }

    #[test]
    fn a_never_collected_baseline_says_how_to_start_one() {
        let lines = describe_workflow_profile(&build_workflow_profile_report(
            &StoreState::NeverCollected,
            None,
            NOW,
        ));
        let text = lines.join("\n");
        assert!(text.contains("workflow-profile record"), "{text}");
    }

    /// Every rendering carries the no-authority sentence. A report pasted into
    /// a bug thread without it invites exactly the reading it denies.
    #[test]
    fn every_rendering_says_the_baseline_is_not_permission() {
        for state in [
            StoreState::NeverCollected,
            StoreState::Unreadable(ProbeReason::Failed),
            settled(),
        ] {
            let report = build_workflow_profile_report(&state, None, NOW);
            assert_eq!(report.authority, AUTHORITY_NOTE);
            let text = describe_workflow_profile(&report).join("\n");
            assert!(text.contains("never permission"), "{text}");
        }
    }

    #[test]
    fn a_refused_recording_says_when_one_would_be_admitted() {
        let report = build_workflow_record_report(
            Admission::TooSoon {
                seconds_until_eligible: 1_500,
            },
            &observation(0, 4),
            &[observation(1, 4)],
            None,
            NOW,
        );
        assert_eq!(report.admission, "too_soon");
        assert_eq!(report.seconds_until_eligible, Some(1_500));
        let text = describe_workflow_record(&report).join("\n");
        assert!(text.contains("25 minutes"), "{text}");
        assert!(
            !text.contains("failed") && !text.contains("error"),
            "a working rule was described as a failure: {text}"
        );
    }

    #[test]
    fn an_admitted_recording_reports_what_it_saw_and_what_is_stored() {
        let report = build_workflow_record_report(
            Admission::Admitted,
            &observation(0, 4),
            &[observation(2, 4), observation(1, 4), observation(0, 4)],
            Some("/somewhere/workflow-history.json".to_string()),
            NOW,
        );
        assert_eq!(report.admission, "admitted");
        assert_eq!(report.repositories_seen, 1);
        assert_eq!(report.observations_stored, 3);
        assert_eq!(report.profile.mode, "parallel_multi_worktree");
        let text = describe_workflow_record(&report).join("\n");
        assert!(text.contains("Recorded one observation"), "{text}");
        assert!(text.contains("/somewhere/workflow-history.json"), "{text}");
    }

    /// A clock that moved backwards is declined, and the prose says which of
    /// the two refusals happened — they have different remedies.
    #[test]
    fn a_backwards_clock_is_its_own_refusal() {
        let report = build_workflow_record_report(
            Admission::ClockWentBackwards,
            &observation(0, 1),
            &[observation(0, 1)],
            None,
            NOW,
        );
        assert_eq!(report.admission, "clock_went_backwards");
        assert_eq!(report.seconds_until_eligible, None);
        let text = describe_workflow_record(&report).join("\n");
        assert!(text.contains("clock moved backwards"), "{text}");
    }

    /// AC 7, at the one place prose about the baseline is written.
    ///
    /// The guard script checks the source; this checks the rendered output for
    /// every state, which is what a user and a bug report actually see.
    #[test]
    fn no_rendering_labels_the_person_operating_the_machine() {
        const BANNED: &[&str] = &[
            "advanced",
            "beginner",
            "novice",
            "expert",
            "power user",
            "proficient",
            "sophisticated",
            "competent",
            "skilled",
            "amateur",
            "professional",
        ];
        let mut text = String::new();
        for state in [
            StoreState::NeverCollected,
            StoreState::Unreadable(ProbeReason::Failed),
            StoreState::Collected(Vec::new()),
            settled(),
            StoreState::Collected(vec![
                observation(1, 1),
                observation(2, 1),
                observation(3, 1),
            ]),
        ] {
            let report = build_workflow_profile_report(&state, None, NOW);
            text.push_str(&describe_workflow_profile(&report).join("\n"));
            text.push('\n');
        }
        for admission in [
            Admission::Admitted,
            Admission::TooSoon {
                seconds_until_eligible: 60,
            },
            Admission::ClockWentBackwards,
        ] {
            let report = build_workflow_record_report(
                admission,
                &observation(0, 4),
                &[observation(0, 4)],
                None,
                NOW,
            );
            text.push_str(&describe_workflow_record(&report).join("\n"));
            text.push('\n');
        }
        // Every mode word's own description too, including the ones no state
        // above happened to produce.
        for mode in [
            "serial_single_checkout",
            "serial_multi_branch",
            "parallel_multi_worktree",
            "mixed",
            "unknown",
        ] {
            text.push_str(describe_mode(mode));
            text.push('\n');
        }

        let lowered = text.to_lowercase();
        for word in BANNED {
            assert!(
                !lowered.contains(word),
                "the baseline's own wording called somebody `{word}`:\n{text}"
            );
        }
    }

    /// Each of the seven counts arrives under its own name.
    ///
    /// Seven same-typed fields copied by hand is exactly the shape where two get
    /// crossed, and every value below is distinct so a swap cannot pass.
    #[test]
    fn all_seven_counts_reach_the_report_under_the_right_name() {
        let report = support_report(&WorkflowSupport {
            spanning_days: 30,
            repositories_observed: 4,
            parallel_observations: 5,
            serial_observations: 6,
            mixed_observations: 7,
            single_checkout_branch_changes: 8,
            most_worktrees_seen_at_once: 9,
        });
        assert_eq!(report.spanning_days, 30);
        assert_eq!(report.repositories_observed, 4);
        assert_eq!(report.parallel_observations, 5);
        assert_eq!(report.serial_observations, 6);
        assert_eq!(report.mixed_observations, 7);
        assert_eq!(report.single_checkout_branch_changes, 8);
        assert_eq!(report.most_worktrees_seen_at_once, 9);
    }

    /// A summary that was built rather than classified still projects its
    /// counts — the path Planner v2 and the graph take.
    #[test]
    fn a_summary_projects_the_counts_it_was_constructed_with() {
        let summary = WorkflowHistorySummary::from_observations(
            crate::workspace::WorkflowMode::Mixed,
            18,
            WorkflowSupport {
                spanning_days: 30,
                ..WorkflowSupport::nothing_observed()
            },
        );
        assert_eq!(support_report(&summary.support).spanning_days, 30);
    }
}
