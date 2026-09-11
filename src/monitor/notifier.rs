//! Notification seam.
//!
//! Notification content is fixed/templated from the [`PressureState`] enum
//! only — never built from arbitrary or externally-supplied strings — so
//! there is nothing here for untrusted input to interpolate into.

use crate::monitor::pressure::PressureState;
use std::io;

/// Sends a user-facing notification for a confirmed pressure transition.
/// Implementations must not panic on failure — a failed notification must
/// never take down the monitor loop.
pub trait Notifier: Send + Sync {
    fn notify(&self, from: PressureState, to: PressureState) -> io::Result<()>;
}

/// Fixed, templated notification title/body for a transition. No caller
/// input is ever interpolated here beyond the enum's own `Display`.
pub fn notification_text(from: PressureState, to: PressureState) -> (String, String) {
    let title = format!("Glomeris: disk pressure {to}");
    let body = format!("Disk pressure changed from {from} to {to}.");
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_is_fixed_and_templated() {
        let (title, body) = notification_text(PressureState::Healthy, PressureState::Warn);
        assert_eq!(title, "Glomeris: disk pressure WARN");
        assert_eq!(body, "Disk pressure changed from HEALTHY to WARN.");
    }
}
