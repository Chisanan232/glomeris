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
    /// The thing being asked answered, and refused because it is being asked
    /// too often. Distinct from [`Self::Failed`] and from
    /// [`Self::PermissionDenied`] because the three imply different next
    /// steps and, more importantly, different amounts of trust: a quota
    /// refusal means the tool is present, reachable and willing — it has
    /// simply said "later". Folded into `Failed` it would read as a broken
    /// setup, and folded into `PermissionDenied` as a bad credential, and a
    /// user would go and change something that was never wrong.
    ///
    /// Added by HORO-1546 for the external providers, which are the first
    /// probes on a metered interface, but not restricted to them: any
    /// rate-limited tool may use it.
    RateLimited,
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
            Self::RateLimited => "rate_limited",
            Self::Failed => "failed",
            Self::NotAttempted => "not_attempted",
        }
    }

    /// Every reason, in declaration order. Lets a test cover the enum
    /// without being edited when a variant is added.
    pub const ALL: [ProbeReason; 7] = [
        Self::ToolAbsent,
        Self::ToolNotRunning,
        Self::PermissionDenied,
        Self::TimedOut,
        Self::RateLimited,
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
        assert_eq!(tags.len(), 7, "ALL must list every variant");
        for tag in &tags {
            assert!(!tag.is_empty());
        }
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), ProbeReason::ALL.len());
    }

    /// A quota refusal is still an absence of an answer. The reason exists so
    /// a *reader* can tell "later" from "broken"; it must not become a third
    /// thing `ProbeOutcome` treats as an observation, because the provider
    /// said nothing about the subject at all.
    #[test]
    fn a_rate_limited_probe_yields_no_value() {
        let outcome: ProbeOutcome<bool> = ProbeOutcome::Unavailable(ProbeReason::RateLimited);
        assert_eq!(outcome.observed(), None);
        assert!(!outcome.is_observed());
        assert_eq!(ProbeReason::RateLimited.tag(), "rate_limited");
    }
}
