//! What the evidence-expansion loop is allowed to cost (HORO-1549).
//!
//! # Why a type rather than four constants
//!
//! The loop is the one part of planning whose work a *provider* influences: it
//! asks for more evidence, and asking again is how a round becomes two. Campaign
//! section 16 requires that bounded, and four `const`s scattered through the
//! loop would be four places to forget one. Held together, and passed in, the
//! tests can shrink every one of them to something a unit test can exhaust —
//! which is the only way the bounds get exercised at all, because the default
//! ceilings are deliberately higher than any honest round reaches.
//!
//! # Every bound is a ceiling, and none of them is a target
//!
//! A loop that stops early because no probe was requested is the normal case,
//! not a degraded one. These numbers exist for the abnormal case: a provider
//! that answers every round with another question. See
//! [`super::expand::StopReason`] for how the two are told apart in the report,
//! which matters because "nothing more was needed" and "we ran out of rounds"
//! are different facts about how much the plan is worth.

use std::time::Duration;

/// Ceilings on one bounded evidence-expansion run.
///
/// `Copy` because every field is, and because the loop reads them repeatedly
/// while threading nothing else through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceBounds {
    /// How many planning rounds may run in total, the first one included.
    ///
    /// Counted this way, rather than as "expansion rounds after the first", so
    /// that `max_rounds: 1` is exactly today's single-shot behaviour and needs
    /// no separate disabled flag. A value of 0 is treated as 1 by
    /// [`Self::rounds_allowed`]: a run that sends nothing has no plan to
    /// report, and silently doing nothing is worse than a caller's typo.
    pub max_rounds: u32,
    /// How many probes may run across the whole run, all rounds together.
    ///
    /// Separate from `max_rounds` because a per-response cap of
    /// [`super::validate::MAX_EVIDENCE_REQUESTS`] and a round cap of N would
    /// otherwise multiply into N × 8 probes — each one a `git` or `lsof`
    /// process — and the product of two limits nobody wrote down is not a
    /// limit.
    pub max_probes_total: u32,
    /// How long any one probe may take before it reports as unavailable.
    ///
    /// Handed to the probes themselves; a provider never influences it.
    pub per_probe_timeout: Duration,
    /// How long the whole run may take before no further round starts.
    ///
    /// Checked between rounds and between probes rather than enforced by
    /// killing work in flight: a probe that is interrupted mid-read would have
    /// to report something about a tree it only partly looked at, and there is
    /// no honest value for that. So this bounds when the *next* thing starts,
    /// and the worst case is this plus one `per_probe_timeout`.
    pub max_total_duration: Duration,
}

impl EvidenceBounds {
    /// Today's behaviour: plan once, run no probes.
    ///
    /// The default for every caller that has not opted in, so adding this
    /// module changes what `glomeris` does to nobody. It is also the
    /// deterministic path campaign section 16's AC 8 requires stay available —
    /// one round is one provider call and zero local probes.
    pub const SINGLE_ROUND: Self = Self {
        max_rounds: 1,
        max_probes_total: 0,
        per_probe_timeout: Duration::from_secs(5),
        max_total_duration: Duration::from_secs(30),
    };

    /// The ceilings for a run that is allowed to expand.
    ///
    /// Three rounds because the loop campaign section 16 describes — observe,
    /// identify what is missing, re-evaluate — needs two, and a third leaves
    /// room for one follow-up that the second round's answer raised. Not more:
    /// a fourth round means the provider is not converging, and more turns will
    /// not make it converge, they will only make a human wait.
    ///
    /// Twelve probes because three rounds of the four or so genuinely distinct
    /// questions a workspace raises is twelve, while 3 × the per-response cap
    /// of 8 would be 24 — more `git` processes than a question asked while
    /// someone waits can justify.
    pub const DEFAULT: Self = Self {
        max_rounds: 3,
        max_probes_total: 12,
        per_probe_timeout: Duration::from_secs(5),
        max_total_duration: Duration::from_secs(30),
    };

    /// The most rounds a caller may ask for.
    ///
    /// Not a safety bound — [`Self::max_probes_total`] and
    /// [`Self::max_total_duration`] are what actually stop the loop, and they
    /// stop it whatever this says. This is a sanity bound on the *request*: a
    /// caller who typed `--evidence-rounds 500` has almost certainly made a
    /// mistake, and finding out is better than watching a run end on a
    /// duration ceiling thirty seconds later and wondering which ceiling it
    /// was. Eight because [`Self::DEFAULT`] allows twelve probes, and a run
    /// asking fewer than two questions per round has stopped learning.
    pub const MAX_ROUNDS: u32 = 8;

    /// `max_rounds`, floored at 1. See the field doc for why 0 is not honoured.
    pub fn rounds_allowed(self) -> u32 {
        self.max_rounds.max(1)
    }

    /// The bounds for `rounds` rounds, everything else defaulted.
    ///
    /// The shape the CLI needs: one number a user chose, with the costs that
    /// number implies left to this module rather than to an argument parser.
    pub fn for_rounds(rounds: u32) -> Self {
        Self {
            max_rounds: rounds,
            ..Self::DEFAULT
        }
    }
}

impl Default for EvidenceBounds {
    /// [`Self::SINGLE_ROUND`], not [`Self::DEFAULT`].
    ///
    /// Deliberately the conservative one, so a caller that constructs these
    /// with `..Default::default()` gets the behaviour that runs no probes.
    /// Expansion is something a caller asks for.
    fn default() -> Self {
        Self::SINGLE_ROUND
    }
}

/// How much of `max_total_duration` has gone.
///
/// A trait because the alternative is [`std::time::Instant::now`] inside the
/// loop, and then the only way to test the time bound is a test that sleeps.
/// Tests that sleep are tests that are slow enough to get marked `#[ignore]`,
/// and a bound whose test is ignored is not a bound.
pub trait ElapsedClock {
    fn elapsed(&self) -> Duration;
}

/// The real one.
pub struct MonotonicClock {
    started: std::time::Instant,
}

impl MonotonicClock {
    pub fn started_now() -> Self {
        Self {
            started: std::time::Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::started_now()
    }
}

impl ElapsedClock for MonotonicClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_for_a_caller_that_asked_for_nothing_runs_no_probes() {
        assert_eq!(EvidenceBounds::default(), EvidenceBounds::SINGLE_ROUND);
        assert_eq!(EvidenceBounds::default().max_probes_total, 0);
        assert_eq!(EvidenceBounds::default().rounds_allowed(), 1);
    }

    /// A zero is a caller's mistake, and the plan a zero-round run would
    /// produce is no plan at all.
    /// The request bound is not the safety bound, and the test says so: asking
    /// for the maximum leaves the probe and duration ceilings exactly where
    /// they were.
    #[test]
    fn the_most_rounds_a_caller_may_ask_for_changes_no_other_ceiling() {
        let most = EvidenceBounds::for_rounds(EvidenceBounds::MAX_ROUNDS);
        assert_eq!(most.rounds_allowed(), EvidenceBounds::MAX_ROUNDS);
        assert_eq!(
            most.max_probes_total,
            EvidenceBounds::DEFAULT.max_probes_total
        );
        assert_eq!(
            most.max_total_duration,
            EvidenceBounds::DEFAULT.max_total_duration
        );
        assert_eq!(
            most.per_probe_timeout,
            EvidenceBounds::DEFAULT.per_probe_timeout
        );
    }

    #[test]
    fn zero_rounds_still_plans_once() {
        assert_eq!(EvidenceBounds::for_rounds(0).rounds_allowed(), 1);
        assert_eq!(EvidenceBounds::for_rounds(1).rounds_allowed(), 1);
        assert_eq!(EvidenceBounds::for_rounds(9).rounds_allowed(), 9);
    }

    /// The probe ceiling is below the product of the round ceiling and the
    /// per-response cap, which is the whole reason it is a separate number.
    #[test]
    fn the_probe_ceiling_is_lower_than_the_rounds_could_ask_for() {
        let bounds = EvidenceBounds::DEFAULT;
        let could_ask = bounds.max_rounds * super::super::validate::MAX_EVIDENCE_REQUESTS as u32;
        assert!(
            bounds.max_probes_total < could_ask,
            "the probe ceiling {} does not bind: {} rounds could ask for {could_ask}",
            bounds.max_probes_total,
            bounds.max_rounds
        );
    }

    /// Choosing a round count leaves every cost bound at its default rather
    /// than unbounded.
    #[test]
    fn asking_for_more_rounds_does_not_lift_the_other_ceilings() {
        let many = EvidenceBounds::for_rounds(99);
        assert_eq!(
            many.max_probes_total,
            EvidenceBounds::DEFAULT.max_probes_total
        );
        assert_eq!(
            many.per_probe_timeout,
            EvidenceBounds::DEFAULT.per_probe_timeout
        );
        assert_eq!(
            many.max_total_duration,
            EvidenceBounds::DEFAULT.max_total_duration
        );
    }

    #[test]
    fn the_real_clock_measures_forward() {
        let clock = MonotonicClock::started_now();
        let first = clock.elapsed();
        let second = clock.elapsed();
        assert!(second >= first, "a monotonic clock went backwards");
    }
}
