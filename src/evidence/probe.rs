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

impl ProbeReason {
    /// Stable snake_case token for this reason.
    ///
    /// Until HORO-1542 nothing surfaced *why* a probe came back empty —
    /// reports projected `Unavailable` to an absent field and the reason was
    /// dropped. That is what makes "the process probe failed" and "no
    /// process is using it" read alike downstream, so anything that
    /// explains an unknown needs the reason as a value it can print.
    pub fn tag(self) -> &'static str {
        match self {
            Self::ToolAbsent => "tool_absent",
            Self::ToolNotRunning => "tool_not_running",
            Self::PermissionDenied => "permission_denied",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
            Self::NotAttempted => "not_attempted",
        }
    }

    /// Every reason, in declaration order. Lets a test cover the enum
    /// without being edited when a variant is added.
    pub const ALL: [ProbeReason; 6] = [
        Self::ToolAbsent,
        Self::ToolNotRunning,
        Self::PermissionDenied,
        Self::TimedOut,
        Self::Failed,
        Self::NotAttempted,
    ];
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

    /// The tokens are what a reader — or a model — sees instead of a value,
    /// so two reasons sharing one would make a missing tool and a denied
    /// permission indistinguishable at exactly the point the difference
    /// matters. Driven from `ALL` so a new variant is covered unedited.
    #[test]
    fn every_reason_has_a_distinct_non_empty_tag() {
        let mut tags: Vec<&str> = ProbeReason::ALL.iter().map(|r| r.tag()).collect();
        assert_eq!(tags.len(), 6, "ALL must list every variant");
        for tag in &tags {
            assert!(!tag.is_empty());
        }
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), ProbeReason::ALL.len());
    }
}
