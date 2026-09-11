//! Cancellation / resource budget for a scan.
//!
//! A scan is bounded by wall-clock time and/or the number of filesystem
//! entries visited, so a single scan can never run forever against an
//! arbitrarily large or pathological tree.

use std::time::{Duration, Instant};

/// Configured limits for one scan invocation. `None` on either field means
/// "no limit" for that dimension. The default is unlimited.
#[derive(Debug, Clone, Copy)]
pub struct ScanBudget {
    max_duration: Option<Duration>,
    max_files_visited: Option<u64>,
}

impl ScanBudget {
    /// No time or file-count limit.
    pub fn unlimited() -> Self {
        Self {
            max_duration: None,
            max_files_visited: None,
        }
    }

    /// Set a wall-clock duration budget.
    pub fn with_max_duration(mut self, max_duration: Duration) -> Self {
        self.max_duration = Some(max_duration);
        self
    }

    /// Set a maximum number of filesystem entries to visit.
    pub fn with_max_files_visited(mut self, max_files_visited: u64) -> Self {
        self.max_files_visited = Some(max_files_visited);
        self
    }

    pub(crate) fn tracker(&self) -> BudgetTracker {
        BudgetTracker {
            budget: *self,
            started_at: Instant::now(),
            files_visited: 0,
        }
    }
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Why a scan stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The walk visited every reachable entry within scope.
    Exhausted,
    /// The configured time budget was reached.
    TimeBudget,
    /// The configured file-count budget was reached.
    FileCountBudget,
}

/// Tracks elapsed time and visited-entry count against a [`ScanBudget`]
/// for the lifetime of one scan.
pub(crate) struct BudgetTracker {
    budget: ScanBudget,
    started_at: Instant,
    files_visited: u64,
}

impl BudgetTracker {
    /// Record one visited filesystem entry and return `Some(reason)` if
    /// the budget has just been exceeded.
    pub(crate) fn record_visit(&mut self) -> Option<StopReason> {
        self.files_visited += 1;
        self.check()
    }

    /// Check the budget without recording a visit (e.g. before entering a
    /// new directory).
    pub(crate) fn check(&self) -> Option<StopReason> {
        if let Some(max) = self.budget.max_files_visited {
            if self.files_visited >= max {
                return Some(StopReason::FileCountBudget);
            }
        }
        if let Some(max_duration) = self.budget.max_duration {
            if self.started_at.elapsed() >= max_duration {
                return Some(StopReason::TimeBudget);
            }
        }
        None
    }

    pub(crate) fn files_visited(&self) -> u64 {
        self.files_visited
    }
}
