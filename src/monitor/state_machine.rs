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
