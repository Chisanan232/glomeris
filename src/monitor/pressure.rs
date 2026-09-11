//! Pressure state definitions.
//!
//! `PressureState` is the coarse-grained disk-pressure classification the
//! rest of the monitor operates on. Ordered from least to most urgent so
//! comparisons (`>=`) can express "at least as urgent as WARN".

/// Disk-pressure classification, ordered from least to most urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PressureState {
    Healthy,
    Warn,
    Pressured,
    Critical,
    Emergency,
}

impl PressureState {
    /// All states in ascending urgency order.
    pub const ALL: [PressureState; 5] = [
        PressureState::Healthy,
        PressureState::Warn,
        PressureState::Pressured,
        PressureState::Critical,
        PressureState::Emergency,
    ];

    /// Short machine-stable name, used in logs/persistence/notifications.
    pub fn as_str(&self) -> &'static str {
        match self {
            PressureState::Healthy => "HEALTHY",
            PressureState::Warn => "WARN",
            PressureState::Pressured => "PRESSURED",
            PressureState::Critical => "CRITICAL",
            PressureState::Emergency => "EMERGENCY",
        }
    }
}

impl std::fmt::Display for PressureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_ascending_urgency() {
        assert!(PressureState::Healthy < PressureState::Warn);
        assert!(PressureState::Warn < PressureState::Pressured);
        assert!(PressureState::Pressured < PressureState::Critical);
        assert!(PressureState::Critical < PressureState::Emergency);
    }

    #[test]
    fn as_str_round_trips_display() {
        for state in PressureState::ALL {
            assert_eq!(state.as_str(), state.to_string());
        }
    }
}
