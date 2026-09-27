//! The one explicit used-percent ↔ free-percent conversion (HORO-1506).
//!
//! The recovery loop's [`FreeTarget::Percentage`] means *percent of capacity
//! FREE*: `FreeTarget::Percentage(20.0)` is satisfied when 20% of the volume
//! is free, i.e. when it is 80% used. That is the right primitive for a
//! floor-style CLI flag, and it is what `glomeris free --target 20%` has
//! always meant, but it is the wrong number to put in front of a human. A
//! person reasons about a disk in *percent used* — "this thing is 94% full,
//! get it down to 60%" — and a bare `Target: 50%` on a screen is ambiguous
//! about which of the two it is.
//!
//! [`RecoveryGoal`] is therefore the product-facing type: it stores **used
//! percent**, it is the only thing the GUI and the `--goal-used-percent`
//! flag ever construct, and [`RecoveryGoal::to_free_target`] is the single
//! place in the codebase where the inversion happens. Nothing else may
//! subtract from 100. If a second conversion appears, the two can disagree,
//! and a disagreement here silently changes how much of a user's disk gets
//! deleted.
//!
//! The conversion is proven equivalent to the loop's own predicate rather
//! than asserted: see `converted_goal_agrees_with_target_met_predicate` in
//! this module's tests, which checks
//! `target_met(usage, goal.to_free_target()) == (usage.used_percent() <=
//! goal.used_percent())` across a grid of volumes.

use crate::executor::recovery_loop::FreeTarget;
use crate::monitor::fs_stat::FsUsage;
use std::fmt;

/// A recovery goal, always expressed as **target disk usage percentage** —
/// the number a human reads.
///
/// The field is private and the only constructor validates, so an
/// out-of-range or non-finite goal cannot exist. Read it back with
/// [`RecoveryGoal::used_percent`] or [`RecoveryGoal::free_percent`], both of
/// which name their axis; there is deliberately no bare `percent()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecoveryGoal {
    used_percent: f64,
}

/// Why a goal was refused. Each variant's [`fmt::Display`] is written to be
/// shown verbatim to a user, and always names the axis ("% used") so no
/// message can be read the wrong way round.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GoalRejection {
    /// NaN or an infinity. Cannot be compared, so cannot be honoured.
    NotFinite,
    /// Outside `0..=100`.
    OutOfRange { used_percent: f64 },
    /// The goal asks for the same usage or more than the volume already has,
    /// so recovering toward it would be a no-op or a regression. Only
    /// checked when a goal is validated against an observation — see
    /// [`RecoveryGoal::progress_toward`].
    NotAnImprovement {
        goal_used_percent: f64,
        current_used_percent: f64,
    },
}

impl fmt::Display for GoalRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFinite => {
                write!(f, "recovery goal must be a finite percentage of disk used")
            }
            Self::OutOfRange { used_percent } => write!(
                f,
                "recovery goal {used_percent}% used is out of range: must be between 0 and 100 percent used"
            ),
            Self::NotAnImprovement {
                goal_used_percent,
                current_used_percent,
            } => write!(
                f,
                "recovery goal {goal_used_percent}% used is not an improvement on the current {current_used_percent:.1}% used: choose a goal below current usage"
            ),
        }
    }
}

/// A stable machine tag per rejection, for clients that branch on the reason
/// rather than display it. Mirrors the convention used by
/// [`crate::autopilot::gate::RefusalReason::as_str`].
impl GoalRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotFinite => "not_finite",
            Self::OutOfRange { .. } => "out_of_range",
            Self::NotAnImprovement { .. } => "not_an_improvement",
        }
    }
}

/// How far an observed volume is from a goal. Every byte field here is
/// derived from a real [`FsUsage`] reading, never from a candidate estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoalProgress {
    pub goal_used_percent: f64,
    pub goal_free_percent: f64,
    pub current_used_percent: f64,
    pub current_free_bytes: u64,
    pub total_bytes: u64,
    /// Free bytes the volume must reach for the goal to be met.
    pub required_free_bytes: u64,
    /// Additional free bytes still needed, `0` once the goal is met.
    pub bytes_needed: u64,
}

impl RecoveryGoal {
    /// The only constructor. Rejects non-finite values and anything outside
    /// `0..=100`.
    pub fn from_used_percent(used_percent: f64) -> Result<Self, GoalRejection> {
        if !used_percent.is_finite() {
            return Err(GoalRejection::NotFinite);
        }
        if !(0.0..=100.0).contains(&used_percent) {
            return Err(GoalRejection::OutOfRange { used_percent });
        }
        Ok(Self { used_percent })
    }

    /// Target usage, the axis the GUI shows.
    pub fn used_percent(&self) -> f64 {
        self.used_percent
    }

    /// The same goal on the free axis — the axis the recovery loop speaks.
    pub fn free_percent(&self) -> f64 {
        100.0 - self.used_percent
    }

    /// Convert to the loop's target. This is the only inversion in the
    /// codebase.
    pub fn to_free_target(&self) -> FreeTarget {
        FreeTarget::Percentage(self.free_percent())
    }

    /// Free bytes required on a volume of `total_bytes` for this goal to be
    /// met, rounded up so the returned figure genuinely satisfies
    /// [`crate::executor::recovery_loop::target_met`] rather than landing one
    /// byte short of it.
    pub fn required_free_bytes(&self, total_bytes: u64) -> u64 {
        if total_bytes == 0 {
            return 0;
        }
        let required = (self.free_percent() / 100.0) * total_bytes as f64;
        required.ceil().max(0.0) as u64
    }

    /// Measure `usage` against this goal.
    ///
    /// Returns [`GoalRejection::NotAnImprovement`] when the goal is at or
    /// above current usage. Recovery toward such a goal would delete nothing
    /// and report success, which reads as "Glomeris cleaned up for you" when
    /// nothing happened — so it is refused at the edge instead, with the two
    /// numbers in the message.
    ///
    /// Note the asymmetry with `free --target`, which deliberately does *not*
    /// apply this check: a raw free-space floor you already exceed is a
    /// legitimate no-op probe (and is used as one in
    /// `tests/execution_lock_wiring.rs`), whereas a *product goal* asking for
    /// more usage than you have is a user error worth naming.
    pub fn progress_toward(&self, usage: &FsUsage) -> Result<GoalProgress, GoalRejection> {
        let current_used_percent = usage.used_percent();
        if self.used_percent >= current_used_percent {
            return Err(GoalRejection::NotAnImprovement {
                goal_used_percent: self.used_percent,
                current_used_percent,
            });
        }
        let required_free_bytes = self.required_free_bytes(usage.total_bytes);
        Ok(GoalProgress {
            goal_used_percent: self.used_percent,
            goal_free_percent: self.free_percent(),
            current_used_percent,
            current_free_bytes: usage.free_bytes,
            total_bytes: usage.total_bytes,
            required_free_bytes,
            bytes_needed: required_free_bytes.saturating_sub(usage.free_bytes),
        })
    }

    /// One-line unambiguous rendering, e.g. `60% used (40% free)`. Produced
    /// in Rust so every surface — CLI prose, JSON, GUI label, spoken
    /// accessibility string — reads the same words, the same reason
    /// `ExecuteReport` emits its own human byte strings (HORO-1312).
    pub fn describe(&self) -> String {
        format!(
            "{}% used ({}% free)",
            trim_percent(self.used_percent),
            trim_percent(self.free_percent())
        )
    }
}

/// Render a percentage without a trailing `.0`, so a whole number reads as
/// `60%` and a fractional one keeps its precision.
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
    use crate::executor::recovery_loop::target_met;

    #[test]
    fn used_percent_inverts_to_free_percent() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        assert_eq!(goal.used_percent(), 60.0);
        assert_eq!(goal.free_percent(), 40.0);
        assert_eq!(goal.to_free_target(), FreeTarget::Percentage(40.0));
    }

    #[test]
    fn bounds_are_inclusive_at_both_ends() {
        assert!(RecoveryGoal::from_used_percent(0.0).is_ok());
        assert!(RecoveryGoal::from_used_percent(100.0).is_ok());
    }

    #[test]
    fn out_of_range_is_rejected_with_the_offending_value() {
        assert_eq!(
            RecoveryGoal::from_used_percent(101.0),
            Err(GoalRejection::OutOfRange {
                used_percent: 101.0
            })
        );
        assert_eq!(
            RecoveryGoal::from_used_percent(-0.5),
            Err(GoalRejection::OutOfRange {
                used_percent: -0.5
            })
        );
    }

    #[test]
    fn non_finite_is_rejected() {
        assert_eq!(
            RecoveryGoal::from_used_percent(f64::NAN),
            Err(GoalRejection::NotFinite)
        );
        assert_eq!(
            RecoveryGoal::from_used_percent(f64::INFINITY),
            Err(GoalRejection::NotFinite)
        );
    }

    /// The load-bearing test for this module: the conversion must agree with
    /// the recovery loop's own satisfaction predicate, for every volume — not
    /// just for the one example a reviewer happens to try.
    #[test]
    fn converted_goal_agrees_with_target_met_predicate() {
        let goals = [0.0, 1.0, 12.5, 50.0, 60.0, 80.0, 94.0, 99.9, 100.0];
        let volumes: [(u64, u64); 8] = [
            (1000, 0),
            (1000, 1),
            (1000, 60),
            (1000, 400),
            (1000, 999),
            (1000, 1000),
            (500 * 1024 * 1024 * 1024, 27 * 1024 * 1024 * 1024),
            (u64::MAX / 2, u64::MAX / 4),
        ];
        for goal_used in goals {
            let goal = RecoveryGoal::from_used_percent(goal_used).unwrap();
            let target = goal.to_free_target();
            for (total, free) in volumes {
                let usage = FsUsage::new(total, free);
                let by_loop = target_met(&usage, &target);
                let by_goal = usage.used_percent() <= goal.used_percent();
                assert_eq!(
                    by_loop, by_goal,
                    "goal {goal_used}% used on volume total={total} free={free}: \
                     recovery loop says met={by_loop} but the used-percent reading \
                     says met={by_goal}"
                );
            }
        }
    }

    #[test]
    fn degenerate_zero_capacity_volume_does_not_divide_by_zero() {
        let goal = RecoveryGoal::from_used_percent(50.0).unwrap();
        assert_eq!(goal.required_free_bytes(0), 0);
        // `used_percent()` of a zero-capacity volume is 0.0, so a 50% goal is
        // already met and therefore not an improvement.
        let err = goal.progress_toward(&FsUsage::new(0, 0)).unwrap_err();
        assert_eq!(err.as_str(), "not_an_improvement");
    }

    #[test]
    fn required_free_bytes_rounds_up_so_the_figure_actually_satisfies_the_goal() {
        // 40% free of 1001 bytes is 400.4 — 400 would leave the goal unmet.
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        let required = goal.required_free_bytes(1001);
        assert_eq!(required, 401);
        assert!(target_met(
            &FsUsage::new(1001, required),
            &goal.to_free_target()
        ));
        assert!(!target_met(
            &FsUsage::new(1001, required - 1),
            &goal.to_free_target()
        ));
    }

    #[test]
    fn progress_reports_measured_shortfall() {
        let goal = RecoveryGoal::from_used_percent(60.0).unwrap();
        // 1000-byte volume, 60 free => 94% used.
        let progress = goal.progress_toward(&FsUsage::new(1000, 60)).unwrap();
        assert_eq!(progress.goal_used_percent, 60.0);
        assert_eq!(progress.goal_free_percent, 40.0);
        assert_eq!(progress.current_used_percent, 94.0);
        assert_eq!(progress.required_free_bytes, 400);
        assert_eq!(progress.bytes_needed, 340);
    }

    #[test]
    fn a_goal_at_or_above_current_usage_is_not_an_improvement() {
        // 1000-byte volume, 400 free => 60% used.
        let usage = FsUsage::new(1000, 400);
        for goal_used in [60.0, 60.1, 80.0, 100.0] {
            let goal = RecoveryGoal::from_used_percent(goal_used).unwrap();
            let err = goal.progress_toward(&usage).unwrap_err();
            assert_eq!(
                err,
                GoalRejection::NotAnImprovement {
                    goal_used_percent: goal_used,
                    current_used_percent: 60.0,
                }
            );
        }
        // Strictly below is accepted.
        assert!(RecoveryGoal::from_used_percent(59.9)
            .unwrap()
            .progress_toward(&usage)
            .is_ok());
    }

    #[test]
    fn rejection_messages_name_the_axis_and_never_read_as_free_percent() {
        let out_of_range = GoalRejection::OutOfRange {
            used_percent: 150.0,
        }
        .to_string();
        assert!(out_of_range.contains("150% used"), "{out_of_range}");
        assert!(out_of_range.contains("percent used"), "{out_of_range}");

        let not_improvement = GoalRejection::NotAnImprovement {
            goal_used_percent: 80.0,
            current_used_percent: 60.0,
        }
        .to_string();
        assert!(not_improvement.contains("80% used"), "{not_improvement}");
        assert!(not_improvement.contains("60.0% used"), "{not_improvement}");

        // No message may say "free": these are used-axis messages, and a
        // stray "free" is exactly the ambiguity HORO-1506 forbids.
        for message in [
            GoalRejection::NotFinite.to_string(),
            out_of_range,
            not_improvement,
        ] {
            assert!(
                !message.contains("free"),
                "used-axis rejection must not mention free space: {message}"
            );
        }
    }

    #[test]
    fn describe_states_both_axes_explicitly() {
        assert_eq!(
            RecoveryGoal::from_used_percent(60.0).unwrap().describe(),
            "60% used (40% free)"
        );
        assert_eq!(
            RecoveryGoal::from_used_percent(87.5).unwrap().describe(),
            "87.5% used (12.5% free)"
        );
        assert_eq!(
            RecoveryGoal::from_used_percent(0.0).unwrap().describe(),
            "0% used (100% free)"
        );
    }

    #[test]
    fn as_str_tags_are_stable_snake_case() {
        assert_eq!(GoalRejection::NotFinite.as_str(), "not_finite");
        assert_eq!(
            GoalRejection::OutOfRange { used_percent: 0.0 }.as_str(),
            "out_of_range"
        );
        assert_eq!(
            GoalRejection::NotAnImprovement {
                goal_used_percent: 0.0,
                current_used_percent: 0.0
            }
            .as_str(),
            "not_an_improvement"
        );
    }
}
