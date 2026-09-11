//! [`ProbeOutcome`]: the structural guard against "a failed probe becomes
//! a safe default".
//!
//! Deliberately NOT `Result<T, E>` — no `Default` impl, no
//! `unwrap_or_default()` escape hatch. A caller that wants the observed
//! value must explicitly branch on this enum; there is no way to silently
//! coerce an unavailable probe into a zero/empty/false value.

/// The outcome of one attempt to observe something about a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome<T> {
    Observed(T),
    Unavailable(ProbeReason),
}

/// Why a probe did not produce an observed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeReason {
    ToolAbsent,
    ToolNotRunning,
    PermissionDenied,
    TimedOut,
    Failed,
    NotAttempted,
}

impl<T> ProbeOutcome<T> {
    pub fn observed(&self) -> Option<&T> {
        match self {
            Self::Observed(t) => Some(t),
            Self::Unavailable(_) => None,
        }
    }

    pub fn is_observed(&self) -> bool {
        self.observed().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_returns_some_for_observed_variant() {
        let outcome = ProbeOutcome::Observed(42u64);
        assert_eq!(outcome.observed(), Some(&42));
        assert!(outcome.is_observed());
    }

    #[test]
    fn observed_returns_none_for_unavailable_variant() {
        let outcome: ProbeOutcome<u64> = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);
        assert_eq!(outcome.observed(), None);
        assert!(!outcome.is_observed());
    }
}
