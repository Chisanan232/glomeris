//! Pressure threshold configuration.
//!
//! Each non-healthy [`PressureState`] is guarded by a [`Threshold`]: the
//! state is entered when *either* the used-percentage bound or the
//! absolute-free-bytes bound is crossed (whichever is stricter for a given
//! filesystem size). Percentage alone under-warns on huge volumes; absolute
//! bytes alone over-warns on tiny ones. Using both, independently
//! configurable, covers both cases.

use crate::monitor::pressure::PressureState;

/// A single boundary: crossed when used percentage reaches `used_percent`
/// *or* free bytes drops to or below `free_bytes`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    /// Percentage of capacity used, 0.0..=100.0.
    pub used_percent: f64,
    /// Absolute free bytes remaining.
    pub free_bytes: u64,
}

impl Threshold {
    pub fn new(used_percent: f64, free_bytes: u64) -> Self {
        Self {
            used_percent,
            free_bytes,
        }
    }

    /// True if the observed usage crosses this boundary.
    pub fn is_crossed(&self, used_percent: f64, free_bytes: u64) -> bool {
        used_percent >= self.used_percent || free_bytes <= self.free_bytes
    }
}

/// Threshold configuration for every non-healthy pressure state.
#[derive(Debug, Clone, PartialEq)]
pub struct ThresholdConfig {
    pub warn: Threshold,
    pub pressured: Threshold,
    pub critical: Threshold,
    pub emergency: Threshold,
}

impl ThresholdConfig {
    /// Classify an observation into the most urgent state whose threshold
    /// is crossed, checked from most to least urgent so a single reading
    /// lands on the correct state even if it crosses several boundaries.
    pub fn classify(&self, used_percent: f64, free_bytes: u64) -> PressureState {
        if self.emergency.is_crossed(used_percent, free_bytes) {
            PressureState::Emergency
        } else if self.critical.is_crossed(used_percent, free_bytes) {
            PressureState::Critical
        } else if self.pressured.is_crossed(used_percent, free_bytes) {
            PressureState::Pressured
        } else if self.warn.is_crossed(used_percent, free_bytes) {
            PressureState::Warn
        } else {
            PressureState::Healthy
        }
    }
}

const GIB: u64 = 1024 * 1024 * 1024;

impl Default for ThresholdConfig {
    /// Sensible defaults for a typical developer laptop volume.
    fn default() -> Self {
        Self {
            warn: Threshold::new(75.0, 50 * GIB),
            pressured: Threshold::new(85.0, 20 * GIB),
            critical: Threshold::new(92.0, 10 * GIB),
            emergency: Threshold::new(97.0, 3 * GIB),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_threshold_triggers_independently_of_bytes() {
        let t = Threshold::new(80.0, 0);
        assert!(t.is_crossed(80.0, u64::MAX));
        assert!(t.is_crossed(99.0, u64::MAX));
        assert!(!t.is_crossed(79.9, u64::MAX));
    }

    #[test]
    fn absolute_bytes_threshold_triggers_independently_of_percent() {
        let t = Threshold::new(100.0, 10 * GIB);
        assert!(t.is_crossed(0.0, 10 * GIB));
        assert!(t.is_crossed(0.0, GIB));
        assert!(!t.is_crossed(0.0, 11 * GIB));
    }

    #[test]
    fn classify_picks_most_urgent_crossed_state() {
        let cfg = ThresholdConfig::default();
        assert_eq!(cfg.classify(10.0, 500 * GIB), PressureState::Healthy);
        assert_eq!(cfg.classify(76.0, 500 * GIB), PressureState::Warn);
        assert_eq!(cfg.classify(86.0, 500 * GIB), PressureState::Pressured);
        assert_eq!(cfg.classify(93.0, 500 * GIB), PressureState::Critical);
        assert_eq!(cfg.classify(98.0, 500 * GIB), PressureState::Emergency);
    }

    #[test]
    fn classify_via_absolute_bytes_alone() {
        let cfg = ThresholdConfig::default();
        // High free percent headroom but nearly no absolute bytes left.
        assert_eq!(cfg.classify(1.0, 2 * GIB), PressureState::Emergency);
    }
}
