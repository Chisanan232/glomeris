//! Runtime/project correlation for [`Evidence`]'s four correlation
//! fields (HORO-949), via already-installed system tools (`lsof`, `git`,
//! `pgrep`) — no new dependency, consistent with HORO-948.
//!
//! [`EvidenceCollector::collect`] is deliberately single-resource, not
//! batch: a future ticket (HORO-951) calls exactly this same method at
//! deletion time to re-check liveness immediately before executing a
//! destructive action. If this were batch-only, that ticket would have
//! to reimplement correlation instead of reusing it.

mod open_files;
mod timeout;

use std::time::Duration;

use super::model::ResourceId;
use super::probe::ProbeOutcome;

pub use super::model::{GitState, ProcessRef};
pub use open_files::{LsofOpenFileProbe, OpenFileProbe};

/// Time budget for one [`EvidenceCollector::collect`] call. Applied *per*
/// underlying subprocess call (e.g. each of the up-to-four lsof/git/pgrep
/// invocations a single `collect()` may make) — a single call can spend
/// up to roughly `timeout * 4` wall time in the worst case, not a shared
/// budget across all four.
pub struct ProbeBudget {
    pub timeout: Duration,
}

/// The four correlation fields as one bundle — a caller (a
/// detector-refresh pass, or HORO-951's deletion-time revalidation)
/// merges these into an existing [`Evidence`] via [`merge_into`] rather
/// than replacing the whole struct.
pub struct CorrelationResult {
    pub open_by_process: ProbeOutcome<Vec<ProcessRef>>,
    pub process_cwd_match: ProbeOutcome<Vec<ProcessRef>>,
    pub git_state: ProbeOutcome<Option<GitState>>,
    pub tool_liveness: ProbeOutcome<bool>,
}

/// A single-resource correlation collector. See the module docs for why
/// this is single-resource rather than batch.
pub trait EvidenceCollector {
    fn collect(&self, id: &ResourceId, budget: ProbeBudget) -> CorrelationResult;
}
