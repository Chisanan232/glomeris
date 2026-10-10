//! Honest 1-TiB-free feasibility arithmetic (HORO-1827 / ADR-0001 §4.4).
//!
//! This module computes nothing about *which* resources are AUTO_SAFE,
//! ASK, PROTECTED or UNKNOWN — that classification is
//! `src/policy/engine.rs::classify`'s job, and it is explicitly out of this
//! ticket's file scope (ADR-0001 §14 lists only `src/monitor/*` and
//! `src/platform/macos/statfs.rs` for HORO-1827). Instead, this module
//! takes the already-classified byte totals as plain inputs and answers one
//! narrow question honestly: given a stability verdict and those totals, is
//! the user's target free-space goal reachable, and by what means?
//!
//! The one invariant every variant of [`FeasibilityVerdict`] preserves:
//! **measured bytes and estimated bytes are never added together.**
//! [`FeasibilityInputs::measured_actual_reclaimed_bytes`] (a fact — the sum
//! of `actual_reclaimed_bytes` from real, df-confirmed executions) stays in
//! its own field in every verdict that carries it, never folded into
//! `auto_safe_estimate_bytes` or `ask_protected_unknown_estimate_bytes` (both
//! upper-bound predictions). A caller that wants one number has to choose
//! which kind it means; this module will not choose for it.

use super::stability::StabilityVerdict;

/// The byte totals and context a feasibility assessment needs, all
/// supplied by the caller (see this module's doc comment for why none of
/// them are computed here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeasibilityInputs {
    pub target_free_bytes: u64,
    pub current_free_bytes: u64,
    /// Upper-bound estimate of AUTO_SAFE-reclaimable bytes, inode-
    /// deduplicated by the caller (ADR-0001 §4.4: each inode with
    /// `st_nlink > 1` counted once) so a hardlinked Cargo binary is not
    /// counted twice.
    pub auto_safe_estimate_bytes: u64,
    /// Upper-bound estimate of ASK/PROTECTED/UNKNOWN bytes, kept separate
    /// from `auto_safe_estimate_bytes` so a verdict can never collapse
    /// "safe without asking" into "safe only with consent".
    pub ask_protected_unknown_estimate_bytes: u64,
    /// Sum of `actual_reclaimed_bytes` from real, df-confirmed executions
    /// recorded in `actions.jsonl` — a measured fact, not a prediction. See
    /// this module's doc comment: never summed into either estimate field.
    pub measured_actual_reclaimed_bytes: u64,
    /// True when any part of `auto_safe_estimate_bytes` sits inside a
    /// shared or `Redirected` Cargo target with other active sharers
    /// (ADR-0001 §9). Bytes freed there are temporary when sharers are
    /// active — the next build in each sharer is a cold rebuild that
    /// regrows the target. Surfaced as a caveat, never used to suppress a
    /// verdict.
    pub shared_target_regrowth_risk: bool,
    /// True when `tmutil listlocalsnapshots /` observed at least one local
    /// snapshot (ADR-0001 §4.4). Freed bytes may not return to `df` until
    /// macOS thins them; Glomeris never deletes snapshots itself.
    pub local_snapshots_present: bool,
}

/// The outcome of a feasibility assessment.
///
/// [`FeasibilityVerdict::InsufficientObservation`] is reachable from this
/// module alone: [`assess`] returns it whenever `stability` is itself
/// [`StabilityVerdict::InsufficientObservation`], before looking at any
/// byte total — the same "never fabricate" rule as the stability module
/// one level down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeasibilityVerdict {
    /// Fewer than 12 real hours of stable same-volume observation exist
    /// yet; no feasibility claim is made at all.
    InsufficientObservation {
        elapsed_secs: u64,
        required_secs: u64,
    },
    /// The current free-byte reading already meets or exceeds the target;
    /// there is no gap to close.
    AlreadyAtGoal,
    /// The AUTO_SAFE estimate alone covers the gap to the target — reachable
    /// without asking the user anything, modulo the usual caveats.
    ReachableAutoSafe {
        gap_bytes: u64,
        auto_safe_estimate_bytes: u64,
        regrowth_risk: bool,
        snapshots_may_delay_recovery: bool,
    },
    /// The gap is only closeable by also counting ASK/PROTECTED/UNKNOWN
    /// bytes — reaching the target requires the user's consent on at least
    /// one resource.
    ReachableRequiresConsent {
        gap_bytes: u64,
        auto_safe_estimate_bytes: u64,
        ask_protected_unknown_estimate_bytes: u64,
        regrowth_risk: bool,
        snapshots_may_delay_recovery: bool,
    },
    /// Even every known byte, safe or not, does not close the gap. The
    /// honest answer is "not reachable by means Glomeris knows about
    /// today", not a fabricated partial win.
    Unreachable {
        gap_bytes: u64,
        auto_safe_estimate_bytes: u64,
        ask_protected_unknown_estimate_bytes: u64,
        measured_actual_reclaimed_bytes: u64,
        regrowth_risk: bool,
    },
}

/// Assesses feasibility. See this module's doc comment for the invariants
/// every variant preserves.
pub fn assess(stability: &StabilityVerdict, inputs: FeasibilityInputs) -> FeasibilityVerdict {
    if let StabilityVerdict::InsufficientObservation {
        elapsed_secs,
        required_secs,
    } = stability
    {
        return FeasibilityVerdict::InsufficientObservation {
            elapsed_secs: *elapsed_secs,
            required_secs: *required_secs,
        };
    }

    let gap_bytes = inputs
        .target_free_bytes
        .saturating_sub(inputs.current_free_bytes);
    if gap_bytes == 0 {
        return FeasibilityVerdict::AlreadyAtGoal;
    }

    if inputs.auto_safe_estimate_bytes >= gap_bytes {
        return FeasibilityVerdict::ReachableAutoSafe {
            gap_bytes,
            auto_safe_estimate_bytes: inputs.auto_safe_estimate_bytes,
            regrowth_risk: inputs.shared_target_regrowth_risk,
            snapshots_may_delay_recovery: inputs.local_snapshots_present,
        };
    }

    let combined_estimate = inputs
        .auto_safe_estimate_bytes
        .saturating_add(inputs.ask_protected_unknown_estimate_bytes);
    if combined_estimate >= gap_bytes {
        return FeasibilityVerdict::ReachableRequiresConsent {
            gap_bytes,
            auto_safe_estimate_bytes: inputs.auto_safe_estimate_bytes,
            ask_protected_unknown_estimate_bytes: inputs.ask_protected_unknown_estimate_bytes,
            regrowth_risk: inputs.shared_target_regrowth_risk,
            snapshots_may_delay_recovery: inputs.local_snapshots_present,
        };
    }

    FeasibilityVerdict::Unreachable {
        gap_bytes,
        auto_safe_estimate_bytes: inputs.auto_safe_estimate_bytes,
        ask_protected_unknown_estimate_bytes: inputs.ask_protected_unknown_estimate_bytes,
        measured_actual_reclaimed_bytes: inputs.measured_actual_reclaimed_bytes,
        regrowth_risk: inputs.shared_target_regrowth_risk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::stability::MIN_OBSERVATION_SECS;

    fn observed() -> StabilityVerdict {
        StabilityVerdict::Analyzed(crate::monitor::stability::StabilityAnalysis {
            volume_dev: 1,
            window_start_unix_secs: 0,
            window_end_unix_secs: MIN_OBSERVATION_SECS,
            elapsed_secs: MIN_OBSERVATION_SECS,
            min_free_bytes: 0,
            max_free_bytes: 0,
            start_free_bytes: 0,
            end_free_bytes: 0,
            rate_bytes_per_sec: 0.0,
            missing_intervals: 0,
            trend: crate::monitor::stability::Trend::Healthy,
            low_watermark_free_bytes: 0,
        })
    }

    fn base_inputs() -> FeasibilityInputs {
        FeasibilityInputs {
            target_free_bytes: 1_099_511_627_776, // 1 TiB
            current_free_bytes: 300 * 1024 * 1024 * 1024,
            auto_safe_estimate_bytes: 0,
            ask_protected_unknown_estimate_bytes: 0,
            measured_actual_reclaimed_bytes: 0,
            shared_target_regrowth_risk: false,
            local_snapshots_present: false,
        }
    }

    #[test]
    fn insufficient_observation_is_never_bypassed_regardless_of_byte_totals() {
        let insufficient = StabilityVerdict::insufficient(60);
        let mut inputs = base_inputs();
        inputs.auto_safe_estimate_bytes = u64::MAX;
        let verdict = assess(&insufficient, inputs);
        assert_eq!(
            verdict,
            FeasibilityVerdict::InsufficientObservation {
                elapsed_secs: 60,
                required_secs: MIN_OBSERVATION_SECS,
            }
        );
    }

    #[test]
    fn already_at_goal_when_current_meets_target() {
        let mut inputs = base_inputs();
        inputs.current_free_bytes = inputs.target_free_bytes;
        assert_eq!(
            assess(&observed(), inputs),
            FeasibilityVerdict::AlreadyAtGoal
        );
    }

    #[test]
    fn reachable_auto_safe_when_the_safe_estimate_alone_covers_the_gap() {
        let mut inputs = base_inputs();
        let gap = inputs.target_free_bytes - inputs.current_free_bytes;
        inputs.auto_safe_estimate_bytes = gap;
        match assess(&observed(), inputs) {
            FeasibilityVerdict::ReachableAutoSafe { gap_bytes, .. } => {
                assert_eq!(gap_bytes, gap);
            }
            other => panic!("expected ReachableAutoSafe, got {other:?}"),
        }
    }

    #[test]
    fn reachable_requires_consent_when_ask_bytes_are_needed() {
        let mut inputs = base_inputs();
        let gap = inputs.target_free_bytes - inputs.current_free_bytes;
        inputs.auto_safe_estimate_bytes = gap / 2;
        inputs.ask_protected_unknown_estimate_bytes = gap; // combined covers it
        match assess(&observed(), inputs) {
            FeasibilityVerdict::ReachableRequiresConsent { gap_bytes, .. } => {
                assert_eq!(gap_bytes, gap);
            }
            other => panic!("expected ReachableRequiresConsent, got {other:?}"),
        }
    }

    /// Mirrors the ADR's own worked example: target unreachable by safe
    /// means because the known Cargo totals (109 + 525 GiB) sit below the
    /// ~728 GiB gap even before any discounting for the shared target being
    /// PROTECTED.
    #[test]
    fn target_unreachable_when_every_known_byte_is_insufficient() {
        let mut inputs = base_inputs();
        inputs.auto_safe_estimate_bytes = 109u64 * 1024 * 1024 * 1024;
        inputs.ask_protected_unknown_estimate_bytes = 525u64 * 1024 * 1024 * 1024;
        inputs.measured_actual_reclaimed_bytes = 10u64 * 1024 * 1024 * 1024;
        inputs.shared_target_regrowth_risk = true;

        match assess(&observed(), inputs) {
            FeasibilityVerdict::Unreachable {
                gap_bytes,
                auto_safe_estimate_bytes,
                ask_protected_unknown_estimate_bytes,
                measured_actual_reclaimed_bytes,
                regrowth_risk,
            } => {
                assert_eq!(
                    gap_bytes,
                    inputs.target_free_bytes - inputs.current_free_bytes
                );
                assert_eq!(auto_safe_estimate_bytes, inputs.auto_safe_estimate_bytes);
                assert_eq!(
                    ask_protected_unknown_estimate_bytes,
                    inputs.ask_protected_unknown_estimate_bytes
                );
                assert_eq!(
                    measured_actual_reclaimed_bytes,
                    inputs.measured_actual_reclaimed_bytes
                );
                assert!(regrowth_risk);
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    /// No double counting: the measured, df-confirmed figure must never be
    /// added into either estimate field, even when summing them would
    /// change which verdict variant is returned.
    #[test]
    fn measured_bytes_are_never_summed_into_the_estimates() {
        let mut inputs = base_inputs();
        let gap = inputs.target_free_bytes - inputs.current_free_bytes;
        // Estimates alone fall short of the gap...
        inputs.auto_safe_estimate_bytes = gap / 4;
        inputs.ask_protected_unknown_estimate_bytes = gap / 4;
        // ...but the measured figure, if wrongly added in, would cover it.
        inputs.measured_actual_reclaimed_bytes = gap;

        match assess(&observed(), inputs) {
            FeasibilityVerdict::Unreachable { .. } => {}
            other => {
                panic!("measured bytes must not have been summed into the estimate, got {other:?}")
            }
        }
    }
}
