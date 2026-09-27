//! Report building for the settings surface (HORO-1507).
//!
//! The projection layer between [`crate::settings`] and what `glomeris
//! settings show|set` prints, in both prose and `--json`. Same division of
//! labour as [`crate::cli::recovery`]: the domain type owns validation and
//! wording, this module owns shape, and `main.rs` owns exit codes.
//!
//! One rule carries over unchanged, and it matters more here than anywhere
//! else in the product: **no ambiguous percentages**. Two numbers on one
//! screen, both percentages, both about the same disk, meaning different
//! things — a bare `75` and `70` beside each other is the single most
//! misreadable thing this feature could ship. So every field names its axis,
//! every prose line names its axis, and the used↔free inversion still happens
//! only inside [`RecoveryGoal`](crate::executor::goal::RecoveryGoal).

use crate::executor::goal::RecoveryGoal;
use crate::reporting::dto::{
    RecoverySettingsBoundsReport, RecoverySettingsReport, SettingsRejectionReport,
};
use crate::settings::{
    RecoverySettings, SettingsRejection, MAX_NOTIFY_AT_USED_PERCENT, MIN_NOTIFY_AT_USED_PERCENT,
};

use super::recovery::build_recovery_goal_report;

/// Project settings into the shape a `--json` client parses.
///
/// `stored_at` and `loaded_from_file` are supplied by the caller rather than
/// resolved here, because this function must stay pure — the same reason
/// [`crate::cli::recovery`]'s builders take an [`crate::monitor::FsUsage`]
/// instead of reading the filesystem.
pub fn build_settings_report(
    settings: &RecoverySettings,
    stored_at: Option<String>,
    loaded_from_file: bool,
) -> RecoverySettingsReport {
    RecoverySettingsReport {
        notify_at_used_percent: settings.notify_at_used_percent(),
        notify_at_description: settings.describe_notify_at(),
        default_goal: build_recovery_goal_report(&settings.default_goal()),
        bounds: bounds_report(),
        stored_at,
        loaded_from_file,
    }
}

/// The bounds, read from the constants the validators themselves read.
///
/// Not a `const` in the DTO module: that would put the numbers somewhere a
/// client reads and nothing checks, which is exactly the drift this field
/// exists to prevent.
fn bounds_report() -> RecoverySettingsBoundsReport {
    RecoverySettingsBoundsReport {
        notify_at_minimum_used_percent: MIN_NOTIFY_AT_USED_PERCENT,
        notify_at_maximum_used_percent: MAX_NOTIFY_AT_USED_PERCENT,
        goal_minimum_used_percent: RecoveryGoal::MINIMUM_USED_PERCENT,
        goal_maximum_used_percent: RecoveryGoal::MAXIMUM_USED_PERCENT,
    }
}

/// Project a refused settings change into the shape a `--json` client parses.
///
/// The optional fields carry whichever figures the refusal actually involved
/// and are absent otherwise, rather than zero-filled: reporting
/// `notify_at_used_percent: 0.0` for a threshold that was `NaN` would be a
/// number this code invented.
pub fn build_settings_rejection_report(rejection: &SettingsRejection) -> SettingsRejectionReport {
    let (notify_at_used_percent, goal_used_percent) = match rejection {
        SettingsRejection::NotifyThresholdNotFinite | SettingsRejection::GoalNotFinite => {
            (None, None)
        }
        SettingsRejection::NotifyThresholdOutOfRange { used_percent } => {
            (Some(*used_percent), None)
        }
        SettingsRejection::GoalOutOfRange { used_percent }
        | SettingsRejection::GoalRefused {
            reason: _,
            used_percent,
        } => (None, Some(*used_percent)),
        SettingsRejection::GoalNotBelowNotifyThreshold {
            goal_used_percent,
            notify_at_used_percent,
        } => (Some(*notify_at_used_percent), Some(*goal_used_percent)),
    };
    SettingsRejectionReport {
        reason: rejection.as_str(),
        message: rejection.to_string(),
        notify_at_used_percent,
        goal_used_percent,
    }
}

/// The prose `settings show` prints, one line per element.
///
/// Returned as lines rather than printed so the wording is testable without
/// capturing stdout — the same shape as
/// [`crate::autopilot::AutopilotEnvelope::describe`].
///
/// The two settings are labelled with what they *do*, not with their field
/// names. "Notify me when disk usage reaches" and "Default recovery goal" are
/// a question and a destination; `notify_at_used_percent = 75` and
/// `default_goal_used_percent = 70` are two similar-looking numbers whose
/// difference the reader has to work out.
pub fn describe_settings(settings: &RecoverySettings) -> Vec<String> {
    vec![
        format!(
            "notify me at:      {} disk usage",
            settings.describe_notify_at()
        ),
        format!("recovery goal:     {}", settings.default_goal().describe()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(notify_at: f64, goal: f64) -> RecoverySettings {
        RecoverySettings::default()
            .with_changes(Some(notify_at), Some(goal))
            .expect("test pair must be valid")
    }

    #[test]
    fn the_report_names_both_axes_on_both_settings() {
        let report = build_settings_report(
            &settings(88.0, 55.0),
            Some("/somewhere/settings.conf".to_string()),
            true,
        );
        assert_eq!(report.notify_at_used_percent, 88.0);
        assert_eq!(report.notify_at_description, "88% used");
        assert_eq!(report.default_goal.used_percent, 55.0);
        assert_eq!(report.default_goal.free_percent, 45.0);
        assert_eq!(report.default_goal.description, "55% used (45% free)");
        assert_eq!(
            report.stored_at.as_deref(),
            Some("/somewhere/settings.conf")
        );
        assert!(report.loaded_from_file);
    }

    /// The threshold and the goal must never be reported under one name. This
    /// is the misread the whole surface is shaped to prevent.
    #[test]
    fn the_report_keeps_the_threshold_and_the_goal_apart() {
        let json =
            serde_json::to_string(&build_settings_report(&settings(75.0, 70.0), None, false))
                .unwrap();
        assert!(json.contains("\"notify_at_used_percent\":75"), "{json}");
        assert!(json.contains("\"used_percent\":70"), "{json}");
        // An absent path is omitted rather than serialized as null, matching
        // every other optional field in the DTO module.
        assert!(!json.contains("stored_at"), "{json}");
        assert!(json.contains("\"loaded_from_file\":false"), "{json}");
    }

    /// Every rejection reports whichever figures it actually involved, and
    /// invents none.
    #[test]
    fn rejection_reports_carry_only_the_figures_the_refusal_involved() {
        let cases = [
            (
                SettingsRejection::NotifyThresholdNotFinite,
                "notify_threshold_not_finite",
                None,
                None,
            ),
            (
                SettingsRejection::NotifyThresholdOutOfRange {
                    used_percent: 120.0,
                },
                "notify_threshold_out_of_range",
                Some(120.0),
                None,
            ),
            (
                SettingsRejection::GoalNotFinite,
                "goal_not_finite",
                None,
                None,
            ),
            (
                SettingsRejection::GoalOutOfRange {
                    used_percent: 101.0,
                },
                "goal_out_of_range",
                None,
                Some(101.0),
            ),
            (
                SettingsRejection::GoalNotBelowNotifyThreshold {
                    goal_used_percent: 85.0,
                    notify_at_used_percent: 80.0,
                },
                "goal_not_below_notify_threshold",
                Some(80.0),
                Some(85.0),
            ),
            (
                SettingsRejection::GoalRefused {
                    reason: "not_an_improvement",
                    used_percent: 42.0,
                },
                "goal_refused",
                None,
                Some(42.0),
            ),
        ];
        for (rejection, reason, notify_at, goal) in cases {
            let report = build_settings_rejection_report(&rejection);
            assert_eq!(report.reason, reason);
            assert_eq!(report.notify_at_used_percent, notify_at, "{reason}");
            assert_eq!(report.goal_used_percent, goal, "{reason}");
            // The message is the domain type's own, not a second wording.
            assert_eq!(report.message, rejection.to_string(), "{reason}");
        }
    }

    /// A non-finite value has no figure to report, so the field is omitted
    /// rather than serialized as a number or as `null`.
    #[test]
    fn a_non_finite_rejection_serializes_no_percentage_at_all() {
        let json = serde_json::to_string(&build_settings_rejection_report(
            &SettingsRejection::NotifyThresholdNotFinite,
        ))
        .unwrap();
        assert!(!json.contains("used_percent"), "{json}");
        assert!(json.contains("notify_threshold_not_finite"), "{json}");
    }

    /// The reported bounds are proven against the validators rather than
    /// compared to the constants they were built from.
    ///
    /// Mirroring the constants would pass even if the validators enforced
    /// something else entirely, which is the whole failure this field exists to
    /// prevent — a pane built to these numbers would then offer a value the CLI
    /// refuses. So each reported edge is *accepted*, and a value just outside
    /// each one is *refused*.
    #[test]
    fn the_reported_bounds_are_the_ones_actually_enforced() {
        let bounds = build_settings_report(&RecoverySettings::default(), None, false).bounds;
        let base = RecoverySettings::default();

        // A goal low enough to stay below either threshold edge, so these
        // cases test the threshold and nothing else.
        let low_goal = Some(0.5);
        assert!(base
            .with_changes(Some(bounds.notify_at_minimum_used_percent), low_goal)
            .is_ok());
        assert!(base
            .with_changes(Some(bounds.notify_at_maximum_used_percent), low_goal)
            .is_ok());
        for outside in [
            bounds.notify_at_minimum_used_percent - 0.5,
            bounds.notify_at_maximum_used_percent + 0.5,
        ] {
            assert_eq!(
                base.with_changes(Some(outside), low_goal).map(|_| ()),
                Err(SettingsRejection::NotifyThresholdOutOfRange {
                    used_percent: outside
                }),
                "{outside} should be outside the reported threshold bounds"
            );
        }

        // The goal's own edges, each paired with a threshold above it so the
        // cross-field rule is not what is being measured. The maximum goal
        // (100% used) cannot be below any threshold, so it is checked through
        // `RecoveryGoal` directly — the constructor is what the bound is a
        // bound on.
        assert!(base
            .with_changes(Some(1.0), Some(bounds.goal_minimum_used_percent))
            .is_ok());
        assert!(RecoveryGoal::from_used_percent(bounds.goal_maximum_used_percent).is_ok());
        for outside in [
            bounds.goal_minimum_used_percent - 0.5,
            bounds.goal_maximum_used_percent + 0.5,
        ] {
            assert_eq!(
                RecoveryGoal::from_used_percent(outside).map(|_| ()),
                Err(crate::executor::goal::GoalRejection::OutOfRange {
                    used_percent: outside
                }),
                "{outside} should be outside the reported goal bounds"
            );
        }
    }

    #[test]
    fn the_prose_labels_say_what_each_setting_does() {
        let lines = describe_settings(&settings(75.0, 70.0));
        assert_eq!(
            lines,
            vec![
                "notify me at:      75% used disk usage".to_string(),
                "recovery goal:     70% used (30% free)".to_string(),
            ]
        );
    }

    /// Neither prose line may print a percentage without saying which axis it
    /// is on, and neither may be confusable with the other.
    #[test]
    fn every_prose_line_names_its_axis() {
        for line in describe_settings(&settings(92.5, 41.0)) {
            assert!(line.contains("used"), "{line}");
        }
        let lines = describe_settings(&settings(92.5, 41.0));
        assert!(lines[0].contains("92.5% used"), "{:?}", lines[0]);
        assert!(lines[1].contains("41% used (59% free)"), "{:?}", lines[1]);
    }
}
