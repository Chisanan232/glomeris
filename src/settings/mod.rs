//! User-configurable product settings (HORO-1507).
//!
//! Glomeris has three numbers about disk usage that are easy to confuse, and
//! they are genuinely three different things:
//!
//! 1. **Pressure state** — [`crate::monitor::ThresholdConfig`] classifies an
//!    observation into `HEALTHY`/`WARN`/`PRESSURED`/`CRITICAL`/`EMERGENCY`.
//!    This is a *description of the machine*, it is written to the pressure
//!    history, and it is deliberately **not** configurable here. If the WARN
//!    boundary were a user setting, lowering it would change what a WARN
//!    recorded last week meant, and raising it would make the next poll
//!    report a transition the disk never made. HORO-1507's criterion that
//!    "configuration changes do not fabricate pressure transitions" is
//!    therefore satisfied by construction: nothing in this module can reach
//!    the classifier.
//! 2. **Alert threshold** — [`RecoverySettings::notify_at_used_percent`]. When
//!    the user wants their attention drawn. A preference about *being told*,
//!    not about what is true.
//! 3. **Recovery goal** — [`RecoverySettings::default_goal`]. Where a recovery
//!    run should stop. A preference about *where to get to*.
//!
//! 2 and 3 are the two this module owns, and keeping them apart is the whole
//! point: a threshold says *when to speak*, a goal says *where to stop*, and a
//! single "disk percentage" setting that tried to be both would be a worse
//! product than either.
//!
//! ## Why this is a Rust config file and not a Swift preference
//!
//! `ProjectRootsStore.swift` states the rule this codebase already follows:
//! product/UI state lives in the app's `UserDefaults`, "never in a new Rust
//! config file", because the Rust core need not know it exists. Both values
//! here fail that test — the alert threshold is read by the background
//! monitor, which raises notifications from Rust
//! ([`crate::platform::macos::notify`]), and the goal is read by the recovery
//! loop. A value stored only in the app's preference domain would be invisible
//! to the process that has to act on it.
//!
//! So these live in a versioned file next to `autopilot.conf`, and the GUI
//! reaches them through `glomeris settings show|set --json`, exactly as it
//! reaches the Autopilot envelope. No validation is duplicated in Swift, which
//! means a hand-edited file and a file written by the GUI are held to the same
//! rules.
//!
//! ## Why there is one applier and no per-field setters
//!
//! The two values have a cross-field rule — the goal must sit below the
//! threshold — so applying them one at a time makes the result depend on the
//! order. Moving from `(75, 70)` to `(60, 55)` succeeds if the goal is written
//! first and is refused if the threshold is, even though the destination is
//! valid either way. [`RecoverySettings::with_changes`] takes both at once,
//! checks each field's own bounds, and then checks the pair once — so the
//! operation is order-insensitive and there is no intermediate state for a
//! caller to get stuck in. Same reasoning as the two-pass parse in
//! [`crate::autopilot::store`].

use crate::executor::goal::{GoalRejection, RecoveryGoal};
use std::fmt;

/// Lowest configurable alert threshold. Below this the alert would be
/// permanently on for any real volume, which is not a threshold.
pub const MIN_NOTIFY_AT_USED_PERCENT: f64 = 1.0;

/// Highest configurable alert threshold. At 100 the alert fires only on a
/// completely full disk, by which point the emergency path has long since
/// taken over, so the setting would read as "warn me" and behave as "never".
pub const MAX_NOTIFY_AT_USED_PERCENT: f64 = 99.0;

/// Default alert threshold, in percent used.
///
/// Not an arbitrary round number: it is the used-percentage bound of today's
/// WARN boundary in [`crate::monitor::ThresholdConfig::default`], which is the
/// point at which Glomeris first says anything unprompted. A higher default
/// would make the product quieter than it is today — a behaviour change
/// wearing a default's clothes — and HORO-1507 requires that defaults preserve
/// current behaviour for a user who has never configured anything.
pub const DEFAULT_NOTIFY_AT_USED_PERCENT: f64 = 75.0;

/// Default recovery goal, in percent used. The constant HORO-1506 shipped in
/// the Recovery card, whose code comment already named this ticket as the
/// thing that would take it over.
pub const DEFAULT_GOAL_USED_PERCENT: f64 = 70.0;

/// Why a settings change was refused.
///
/// Each [`fmt::Display`] is written to be shown verbatim, and every message
/// that mentions a percentage names its axis, so none of them can be read the
/// wrong way round.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingsRejection {
    /// The alert threshold was NaN or an infinity.
    NotifyThresholdNotFinite,
    /// The alert threshold was outside
    /// [`MIN_NOTIFY_AT_USED_PERCENT`]`..=`[`MAX_NOTIFY_AT_USED_PERCENT`].
    NotifyThresholdOutOfRange { used_percent: f64 },
    /// The goal was NaN or an infinity.
    GoalNotFinite,
    /// The goal was outside `0..=100`.
    GoalOutOfRange { used_percent: f64 },
    /// The goal is at or above the alert threshold, so responding to an alert
    /// by recovering toward it would aim at a disk no emptier than the one
    /// that raised the alert.
    ///
    /// Distinct from [`GoalRejection::NotAnImprovement`] on purpose. That one
    /// compares a goal against a *live reading* at run time; this one compares
    /// it against the *threshold* at configuration time, and the two can
    /// disagree — a goal of 80% used is a real improvement on 94% used right
    /// now and still a useless default response to an alert that fires at 75%
    /// used. One token for both would produce a message that cannot say which
    /// comparison failed.
    GoalNotBelowNotifyThreshold {
        goal_used_percent: f64,
        notify_at_used_percent: f64,
    },
    /// A goal that [`RecoveryGoal::from_used_percent`] refused for a reason
    /// this surface has no wording of its own for.
    ///
    /// Unreachable today — that constructor returns only
    /// [`GoalRejection::NotFinite`] and [`GoalRejection::OutOfRange`], both of
    /// which have their own variants above, and
    /// `no_input_reaches_the_catch_all_rejection` in this module's tests sweeps
    /// a grid to keep that true. It exists so that a future constructor-time
    /// rejection is reported truthfully rather than mapped onto whichever
    /// existing message is closest, which is how a user ends up reading a
    /// message about the wrong field.
    GoalRefused { reason: &'static str },
}

impl fmt::Display for SettingsRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotifyThresholdNotFinite => write!(
                f,
                "alert threshold must be a finite percentage of disk used"
            ),
            Self::NotifyThresholdOutOfRange { used_percent } => write!(
                f,
                "alert threshold {used_percent}% used is out of range: must be between \
                 {MIN_NOTIFY_AT_USED_PERCENT} and {MAX_NOTIFY_AT_USED_PERCENT} percent used"
            ),
            Self::GoalNotFinite => write!(
                f,
                "default recovery goal must be a finite percentage of disk used"
            ),
            Self::GoalOutOfRange { used_percent } => write!(
                f,
                "default recovery goal {used_percent}% used is out of range: must be between \
                 0 and 100 percent used"
            ),
            Self::GoalNotBelowNotifyThreshold {
                goal_used_percent,
                notify_at_used_percent,
            } => write!(
                f,
                "default recovery goal {goal_used_percent}% used is not below the alert \
                 threshold of {notify_at_used_percent}% used: recovery would aim at a disk \
                 no emptier than the one that raised the alert"
            ),
            Self::GoalRefused { reason } => {
                write!(f, "default recovery goal was refused ({reason})")
            }
        }
    }
}

impl std::error::Error for SettingsRejection {}

impl SettingsRejection {
    /// A stable machine tag per rejection, for clients that branch on the
    /// reason rather than display it. Sole producer of these strings, so
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` can diff the set
    /// against the GUI's wording. Same convention as
    /// [`GoalRejection::as_str`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotifyThresholdNotFinite => "notify_threshold_not_finite",
            Self::NotifyThresholdOutOfRange { .. } => "notify_threshold_out_of_range",
            Self::GoalNotFinite => "goal_not_finite",
            Self::GoalOutOfRange { .. } => "goal_out_of_range",
            Self::GoalNotBelowNotifyThreshold { .. } => "goal_not_below_notify_threshold",
            Self::GoalRefused { .. } => "goal_refused",
        }
    }

    /// Re-express a rejection from the shared goal validator as this
    /// surface's own. Delegating the bounds check rather than repeating it is
    /// the point: a settings file and the Recovery card cannot come to
    /// different conclusions about the same number.
    fn from_goal_rejection(reason: GoalRejection) -> Self {
        match reason {
            GoalRejection::NotFinite => Self::GoalNotFinite,
            GoalRejection::OutOfRange { used_percent } => Self::GoalOutOfRange { used_percent },
            GoalRejection::NotAnImprovement { .. } => Self::GoalRefused {
                reason: reason.as_str(),
            },
        }
    }
}

/// The two user-configurable numbers, valid by construction.
///
/// Both fields are private and the only way to change either is
/// [`RecoverySettings::with_changes`], so a pair that breaks the cross-field
/// rule cannot exist — not from a hand-edited file, not from the GUI, and not
/// from a future call site that forgets to check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecoverySettings {
    notify_at_used_percent: f64,
    default_goal: RecoveryGoal,
}

impl Default for RecoverySettings {
    fn default() -> Self {
        let default_goal = RecoveryGoal::from_used_percent(DEFAULT_GOAL_USED_PERCENT)
            .expect("DEFAULT_GOAL_USED_PERCENT is within 0..=100");
        let settings = Self {
            notify_at_used_percent: DEFAULT_NOTIFY_AT_USED_PERCENT,
            default_goal,
        };
        // The defaults must satisfy the same cross-field rule every user
        // change does. Asserted rather than assumed, because the two
        // constants are edited independently and a default pair that the
        // validator would refuse is the one invalid state `with_changes`
        // cannot protect anyone from.
        debug_assert!(
            settings.default_goal.used_percent() < settings.notify_at_used_percent,
            "default goal must sit below the default alert threshold"
        );
        settings
    }
}

impl RecoverySettings {
    /// When the user wants to be told, in percent used.
    pub fn notify_at_used_percent(&self) -> f64 {
        self.notify_at_used_percent
    }

    /// Where a recovery run should stop by default.
    pub fn default_goal(&self) -> RecoveryGoal {
        self.default_goal
    }

    /// Apply zero, one or both changes and validate the result.
    ///
    /// `None` means "leave this one alone", which is what makes
    /// `settings set --default-goal-used-percent 55` legal without the caller
    /// having to restate a threshold it is not changing.
    ///
    /// Each field's own bounds are checked first, then the resulting pair is
    /// checked once. Returning a new value rather than mutating in place means
    /// a refused change leaves the caller's settings exactly as they were —
    /// there is no half-applied state to notice or to forget to roll back.
    pub fn with_changes(
        &self,
        notify_at_used_percent: Option<f64>,
        default_goal_used_percent: Option<f64>,
    ) -> Result<Self, SettingsRejection> {
        let notify_at = match notify_at_used_percent {
            Some(value) => validate_notify_at(value)?,
            None => self.notify_at_used_percent,
        };
        let goal = match default_goal_used_percent {
            Some(value) => RecoveryGoal::from_used_percent(value)
                .map_err(SettingsRejection::from_goal_rejection)?,
            None => self.default_goal,
        };

        if goal.used_percent() >= notify_at {
            return Err(SettingsRejection::GoalNotBelowNotifyThreshold {
                goal_used_percent: goal.used_percent(),
                notify_at_used_percent: notify_at,
            });
        }

        Ok(Self {
            notify_at_used_percent: notify_at,
            default_goal: goal,
        })
    }

    /// One-line unambiguous rendering of the alert threshold, e.g.
    /// `75% used`. Produced here rather than at each surface so the CLI, the
    /// JSON, the GUI label and the spoken accessibility string read the same
    /// words — the same reason [`RecoveryGoal::describe`] exists.
    pub fn describe_notify_at(&self) -> String {
        format!("{}% used", trim_percent(self.notify_at_used_percent))
    }
}

/// Bounds check for the alert threshold. Separate from
/// [`RecoveryGoal::from_used_percent`] because the two ranges differ on
/// purpose: a *goal* of 0% used is a coherent ask (empty the disk), whereas a
/// *threshold* of 0% used is an alarm that never stops.
fn validate_notify_at(used_percent: f64) -> Result<f64, SettingsRejection> {
    if !used_percent.is_finite() {
        return Err(SettingsRejection::NotifyThresholdNotFinite);
    }
    if !(MIN_NOTIFY_AT_USED_PERCENT..=MAX_NOTIFY_AT_USED_PERCENT).contains(&used_percent) {
        return Err(SettingsRejection::NotifyThresholdOutOfRange { used_percent });
    }
    Ok(used_percent)
}

/// Render a percentage without a trailing `.0`. Mirrors the helper in
/// [`crate::executor::goal`] so the two surfaces format the same number the
/// same way.
fn trim_percent(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value.round() as i64)
    } else {
        let s = format!("{value:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_todays_behaviour() {
        let settings = RecoverySettings::default();
        assert_eq!(settings.notify_at_used_percent(), 75.0);
        assert_eq!(settings.default_goal().used_percent(), 70.0);
    }

    /// The default alert threshold is not merely 75; it is *the same* 75 as
    /// the WARN boundary, which is the claim the constant's doc comment
    /// makes. If someone retunes the pressure defaults, this is what notices
    /// that the promise "defaults preserve current behaviour" has quietly
    /// stopped being true.
    #[test]
    fn default_alert_threshold_tracks_the_warn_boundary() {
        assert_eq!(
            DEFAULT_NOTIFY_AT_USED_PERCENT,
            crate::monitor::ThresholdConfig::default().warn.used_percent
        );
    }

    #[test]
    fn defaults_satisfy_the_cross_field_rule() {
        let settings = RecoverySettings::default();
        // Re-applying the defaults through the validator must be accepted —
        // the `debug_assert` in `Default` is a developer check, this is the
        // one that runs in release tests too.
        assert!(settings
            .with_changes(
                Some(DEFAULT_NOTIFY_AT_USED_PERCENT),
                Some(DEFAULT_GOAL_USED_PERCENT)
            )
            .is_ok());
    }

    #[test]
    fn both_fields_are_independently_configurable() {
        let settings = RecoverySettings::default();

        let threshold_only = settings.with_changes(Some(90.0), None).unwrap();
        assert_eq!(threshold_only.notify_at_used_percent(), 90.0);
        assert_eq!(threshold_only.default_goal().used_percent(), 70.0);

        let goal_only = settings.with_changes(None, Some(40.0)).unwrap();
        assert_eq!(goal_only.notify_at_used_percent(), 75.0);
        assert_eq!(goal_only.default_goal().used_percent(), 40.0);
    }

    #[test]
    fn changing_nothing_is_the_identity() {
        let settings = RecoverySettings::default();
        assert_eq!(settings.with_changes(None, None).unwrap(), settings);
    }

    /// The reason `with_changes` takes both at once. Applied one at a time,
    /// this move is legal in one order and refused in the other; applied
    /// together it is simply legal.
    #[test]
    fn a_valid_destination_is_reachable_regardless_of_which_field_moves_first() {
        let settings = RecoverySettings::default(); // (75, 70)

        let together = settings.with_changes(Some(60.0), Some(55.0)).unwrap();
        assert_eq!(together.notify_at_used_percent(), 60.0);
        assert_eq!(together.default_goal().used_percent(), 55.0);

        // Threshold first, on its own, is the order that would have failed.
        assert_eq!(
            settings
                .with_changes(Some(60.0), None)
                .unwrap_err()
                .as_str(),
            "goal_not_below_notify_threshold"
        );
    }

    #[test]
    fn a_refused_change_leaves_the_original_untouched() {
        let settings = RecoverySettings::default();
        assert!(settings.with_changes(Some(10.0), None).is_err());
        assert_eq!(settings.notify_at_used_percent(), 75.0);
        assert_eq!(settings.default_goal().used_percent(), 70.0);
    }

    #[test]
    fn threshold_bounds_are_inclusive_and_explicit() {
        let settings = RecoverySettings::default();
        // Both ends accepted, with a goal low enough not to be the reason.
        let low = settings.with_changes(Some(1.0), Some(0.0)).unwrap();
        assert_eq!(low.notify_at_used_percent(), 1.0);
        let high = settings.with_changes(Some(99.0), None).unwrap();
        assert_eq!(high.notify_at_used_percent(), 99.0);

        // One step past each end refused, and refused for being out of range
        // rather than for anything to do with the goal.
        for out in [0.0, 100.0, -1.0, 150.0] {
            assert_eq!(
                settings
                    .with_changes(Some(out), Some(0.0))
                    .unwrap_err()
                    .as_str(),
                "notify_threshold_out_of_range",
                "{out} should be out of range for an alert threshold"
            );
        }
    }

    #[test]
    fn non_finite_values_are_refused_on_both_fields() {
        let settings = RecoverySettings::default();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                settings.with_changes(Some(bad), None).unwrap_err().as_str(),
                "notify_threshold_not_finite"
            );
            assert_eq!(
                settings.with_changes(None, Some(bad)).unwrap_err().as_str(),
                "goal_not_finite"
            );
        }
    }

    #[test]
    fn goal_bounds_come_from_the_shared_goal_validator() {
        let settings = RecoverySettings::default();
        for out in [-0.1, 100.1, 1000.0] {
            assert_eq!(
                settings.with_changes(None, Some(out)).unwrap_err().as_str(),
                "goal_out_of_range",
                "{out} should be out of range for a goal"
            );
        }
    }

    /// A goal equal to the threshold is refused, not accepted. "At the
    /// threshold" is not an improvement on the threshold.
    #[test]
    fn a_goal_equal_to_the_threshold_is_refused() {
        let settings = RecoverySettings::default();
        let err = settings.with_changes(Some(80.0), Some(80.0)).unwrap_err();
        assert_eq!(err.as_str(), "goal_not_below_notify_threshold");
        assert!(err.to_string().contains("80% used"));
    }

    /// The catch-all exists for a future change, not for any input reachable
    /// today. If this starts failing, `RecoveryGoal::from_used_percent` has
    /// grown a rejection and `SettingsRejection` needs wording for it rather
    /// than a bucket.
    #[test]
    fn no_input_reaches_the_catch_all_rejection() {
        let settings = RecoverySettings::default();
        let mut swept = 0;
        for i in -2000..=12000 {
            let value = i as f64 / 100.0;
            if let Err(e) = settings.with_changes(None, Some(value)) {
                assert_ne!(
                    e.as_str(),
                    "goal_refused",
                    "{value} reached the catch-all rejection"
                );
            }
            swept += 1;
        }
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_ne!(
                settings.with_changes(None, Some(bad)).unwrap_err().as_str(),
                "goal_refused"
            );
        }
        assert_eq!(swept, 14001, "the sweep must actually have run");
    }

    /// Every rejection has a distinct token, or a client that branches on
    /// them cannot tell two different problems apart.
    #[test]
    fn rejection_tokens_are_distinct() {
        let all = [
            SettingsRejection::NotifyThresholdNotFinite,
            SettingsRejection::NotifyThresholdOutOfRange { used_percent: 0.0 },
            SettingsRejection::GoalNotFinite,
            SettingsRejection::GoalOutOfRange {
                used_percent: 101.0,
            },
            SettingsRejection::GoalNotBelowNotifyThreshold {
                goal_used_percent: 80.0,
                notify_at_used_percent: 75.0,
            },
            SettingsRejection::GoalRefused {
                reason: "not_an_improvement",
            },
        ];
        let mut tokens: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        tokens.sort_unstable();
        let count = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), count, "two rejections share a token");
    }

    /// Every message that mentions a percentage says which axis it is on.
    /// The campaign rule is that no surface may show a bare percentage, and
    /// these strings are shown verbatim.
    #[test]
    fn every_message_names_its_axis() {
        let all = [
            SettingsRejection::NotifyThresholdNotFinite,
            SettingsRejection::NotifyThresholdOutOfRange { used_percent: 0.0 },
            SettingsRejection::GoalNotFinite,
            SettingsRejection::GoalOutOfRange {
                used_percent: 101.0,
            },
            SettingsRejection::GoalNotBelowNotifyThreshold {
                goal_used_percent: 80.0,
                notify_at_used_percent: 75.0,
            },
        ];
        for rejection in all {
            let message = rejection.to_string();
            assert!(
                message.contains("used"),
                "{message:?} does not name the axis"
            );
            assert!(
                !message.contains("free"),
                "{message:?} mentions the free axis, which these settings are not on"
            );
        }
    }

    #[test]
    fn describe_notify_at_names_the_axis_and_trims_whole_numbers() {
        let settings = RecoverySettings::default();
        assert_eq!(settings.describe_notify_at(), "75% used");
        let fractional = settings.with_changes(Some(87.5), None).unwrap();
        assert_eq!(fractional.describe_notify_at(), "87.5% used");
    }

    /// The goal's own rendering is not re-implemented here; it is the one
    /// `RecoveryGoal` already produces, which is why both axes appear.
    #[test]
    fn the_goal_describes_itself_on_both_axes() {
        assert_eq!(
            RecoverySettings::default().default_goal().describe(),
            "70% used (30% free)"
        );
    }
}
