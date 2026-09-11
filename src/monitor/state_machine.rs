//! Pure pressure state-transition logic with debounce/hysteresis.
//!
//! [`PressureStateMachine`] is deliberately free of any I/O: it consumes
//! already-classified [`PressureState`] observations and decides whether a
//! *confirmed* transition has happened. It requires a candidate state to be
//! observed `confirm_after` consecutive times before confirming the
//! transition, which is what prevents a single noisy sample (or a value
//! oscillating right at a boundary) from producing a notification per poll.

use crate::monitor::pressure::PressureState;

/// A confirmed state transition, returned at most once per boundary
/// crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: PressureState,
    pub to: PressureState,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    candidate: PressureState,
    consecutive: u32,
}

/// Debounced pressure state machine.
#[derive(Debug, Clone)]
pub struct PressureStateMachine {
    current: PressureState,
    pending: Option<Pending>,
    /// Number of consecutive matching observations required before a
    /// candidate state is confirmed and a [`Transition`] is emitted.
    confirm_after: u32,
}

impl PressureStateMachine {
    /// New machine starting from `initial`, requiring `confirm_after`
    /// consecutive matching observations to confirm any transition.
    /// `confirm_after` is clamped to a minimum of 1.
    pub fn new(initial: PressureState, confirm_after: u32) -> Self {
        Self {
            current: initial,
            pending: None,
            confirm_after: confirm_after.max(1),
        }
    }

    /// Current confirmed state.
    pub fn current(&self) -> PressureState {
        self.current
    }

    /// Feed one classified observation. Returns `Some(Transition)` exactly
    /// once per confirmed boundary crossing; returns `None` otherwise,
    /// including while a candidate state is still accumulating consecutive
    /// observations.
    pub fn observe(&mut self, observed: PressureState) -> Option<Transition> {
        if observed == self.current {
            // Back to the confirmed state — any in-flight candidate is
            // stale noise, discard it.
            self.pending = None;
            return None;
        }

        let consecutive = match self.pending {
            Some(p) if p.candidate == observed => p.consecutive + 1,
            _ => 1,
        };

        if consecutive >= self.confirm_after {
            let from = self.current;
            self.current = observed;
            self.pending = None;
            Some(Transition { from, to: observed })
        } else {
            self.pending = Some(Pending {
                candidate: observed,
                consecutive,
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PressureState::*;

    #[test]
    fn single_sample_at_new_state_does_not_transition_by_default_confirm_2() {
        let mut m = PressureStateMachine::new(Healthy, 2);
        assert_eq!(m.observe(Warn), None);
        assert_eq!(m.current(), Healthy);
    }

    #[test]
    fn two_consecutive_samples_confirm_transition() {
        let mut m = PressureStateMachine::new(Healthy, 2);
        assert_eq!(m.observe(Warn), None);
        assert_eq!(
            m.observe(Warn),
            Some(Transition {
                from: Healthy,
                to: Warn
            })
        );
        assert_eq!(m.current(), Warn);
    }

    #[test]
    fn transition_is_emitted_exactly_once() {
        let mut m = PressureStateMachine::new(Healthy, 1);
        assert_eq!(
            m.observe(Warn),
            Some(Transition {
                from: Healthy,
                to: Warn
            })
        );
        // Staying at Warn never re-emits.
        assert_eq!(m.observe(Warn), None);
        assert_eq!(m.observe(Warn), None);
    }

    #[test]
    fn flapping_at_boundary_does_not_spam_transitions() {
        let mut m = PressureStateMachine::new(Healthy, 3);
        // Oscillates between Healthy and Warn without ever holding Warn for
        // 3 consecutive samples: never confirms.
        for _ in 0..10 {
            assert_eq!(m.observe(Warn), None);
            assert_eq!(m.observe(Healthy), None);
        }
        assert_eq!(m.current(), Healthy);
    }

    #[test]
    fn candidate_resets_when_a_different_candidate_appears() {
        let mut m = PressureStateMachine::new(Healthy, 2);
        assert_eq!(m.observe(Warn), None);
        // A different candidate interrupts the run — count restarts.
        assert_eq!(m.observe(Pressured), None);
        assert_eq!(
            m.observe(Pressured),
            Some(Transition {
                from: Healthy,
                to: Pressured
            })
        );
    }

    #[test]
    fn confirm_after_zero_is_clamped_to_one() {
        let mut m = PressureStateMachine::new(Healthy, 0);
        assert_eq!(
            m.observe(Critical),
            Some(Transition {
                from: Healthy,
                to: Critical
            })
        );
    }

    #[test]
    fn escalation_then_recovery_round_trip() {
        let mut m = PressureStateMachine::new(Healthy, 1);
        assert_eq!(m.observe(Emergency).unwrap().to, Emergency);
        assert_eq!(m.observe(Warn).unwrap().to, Warn);
        assert_eq!(m.observe(Healthy).unwrap().to, Healthy);
    }
}
