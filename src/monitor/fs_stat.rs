//! Filesystem capacity observation seam.
//!
//! `FsStat` is the only place the monitor talks to the real filesystem. The
//! healthy polling path calls this and nothing else — no directory walks,
//! no recursive scans. Production uses a cheap `statfs`/`statvfs` call
//! (see `crate::platform::macos::statfs`); tests inject fixed values.

use std::io;
use std::path::Path;

/// A single capacity observation for one mounted filesystem.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FsUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl FsUsage {
    pub fn new(total_bytes: u64, free_bytes: u64) -> Self {
        Self {
            total_bytes,
            free_bytes,
        }
    }

    /// Percentage of capacity in use, 0.0..=100.0. Returns 0.0 for a
    /// (degenerate) zero-capacity filesystem rather than dividing by zero.
    pub fn used_percent(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        let used = self.total_bytes.saturating_sub(self.free_bytes);
        (used as f64 / self.total_bytes as f64) * 100.0
    }
}

/// Cheap, O(1) filesystem-capacity observation. Must never recurse into the
/// directory tree — that is the scanner's job (a separate, expensive path),
/// not the healthy polling loop's.
pub trait FsStat: Send + Sync {
    fn stat(&self, path: &Path) -> io::Result<FsUsage>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_percent_computes_from_total_and_free() {
        let u = FsUsage::new(100, 25);
        assert!((u.used_percent() - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn used_percent_zero_total_does_not_panic() {
        let u = FsUsage::new(0, 0);
        assert_eq!(u.used_percent(), 0.0);
    }

    #[test]
    fn used_percent_free_exceeding_total_saturates() {
        // Defensive: a bogus reading should never underflow.
        let u = FsUsage::new(10, 20);
        assert_eq!(u.used_percent(), 0.0);
    }
}
