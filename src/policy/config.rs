//! [`PolicyConfig`]: tunable thresholds for [`crate::policy::engine::classify`]
//! (HORO-950).

use std::time::Duration;

/// Thresholds `classify` consults. Kept minimal for the MVP — extend with
/// new fields as later tickets need more tunables, rather than hardcoding
/// new constants inside `classify`.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyConfig {
    /// Evidence older than this (relative to the `now` passed to
    /// `classify`) is treated as stale -> `Ask` + `EvidenceStale`.
    pub max_evidence_age: Duration,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            max_evidence_age: Duration::from_secs(5 * 60),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_max_evidence_age_is_five_minutes() {
        assert_eq!(
            PolicyConfig::default().max_evidence_age,
            Duration::from_secs(300)
        );
    }
}
