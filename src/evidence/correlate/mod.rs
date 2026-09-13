//! Runtime/project correlation for [`Evidence`]'s four correlation
//! fields (HORO-949), via already-installed system tools (`lsof`, `git`,
//! `pgrep`) — no new dependency, consistent with HORO-948.
//!
//! [`EvidenceCollector::collect`] is deliberately single-resource, not
//! batch: a future ticket (HORO-951) calls exactly this same method at
//! deletion time to re-check liveness immediately before executing a
//! destructive action. If this were batch-only, that ticket would have
//! to reimplement correlation instead of reusing it.

mod default;
mod git;
mod open_files;
mod process;
mod timeout;
mod tool_liveness;

use std::time::Duration;

use super::model::{Evidence, ResourceId};
use super::probe::ProbeOutcome;

pub use super::model::{GitState, ProcessRef};
pub use default::DefaultEvidenceCollector;
pub use git::{GitCliProbe, GitProbe};
pub use open_files::{LsofOpenFileProbe, OpenFileProbe};
pub use process::{LsofProcessCwdProbe, ProcessCwdProbe};
pub use tool_liveness::{PgrepToolLivenessProbe, ToolLivenessProbe};

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

/// Merge a freshly-collected [`CorrelationResult`] into an existing
/// [`Evidence`]'s four correlation fields.
///
/// Overwrites all four fields unconditionally, including with a new
/// `Unavailable` — a revalidation (e.g. HORO-951's deletion-time
/// recheck) that finds a probe now failing must downgrade the field,
/// never leave a stale `Observed` value in place.
pub fn merge_into(evidence: &mut Evidence, result: CorrelationResult) {
    evidence.open_by_process = result.open_by_process;
    evidence.process_cwd_match = result.process_cwd_match;
    evidence.git_state = result.git_state;
    evidence.tool_liveness = result.tool_liveness;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::model::{
        NativeCleanup, Recoverability, Regenerability, ResourceFingerprint, ResourceKind,
        ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn base_evidence() -> Evidence {
        Evidence {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from("/tmp/x")),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(1024),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: Regenerability::RegenerableByRebuild,
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn merge_into_fills_all_four_correlation_fields() {
        let mut evidence = base_evidence();
        let result = CorrelationResult {
            open_by_process: ProbeOutcome::Observed(vec![ProcessRef {
                pid: 42,
                command: "cargo".to_string(),
            }]),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
        };

        merge_into(&mut evidence, result);

        assert_eq!(
            evidence.open_by_process,
            ProbeOutcome::Observed(vec![ProcessRef {
                pid: 42,
                command: "cargo".to_string()
            }])
        );
        assert_eq!(
            evidence.process_cwd_match,
            ProbeOutcome::Observed(Vec::new())
        );
        assert_eq!(evidence.git_state, ProbeOutcome::Observed(None));
        assert_eq!(evidence.tool_liveness, ProbeOutcome::Observed(false));
    }

    #[test]
    fn merge_into_downgrades_a_previously_observed_field_when_revalidation_fails() {
        // Simulates a deletion-time revalidation (HORO-951): an Evidence
        // that previously observed open_by_process now gets a
        // CorrelationResult where that probe failed. The stale
        // `Observed` must not survive the merge.
        let mut evidence = base_evidence();
        evidence.open_by_process = ProbeOutcome::Observed(vec![ProcessRef {
            pid: 1,
            command: "stale".to_string(),
        }]);

        let result = CorrelationResult {
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            git_state: ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::TimedOut),
        };

        merge_into(&mut evidence, result);

        assert_eq!(
            evidence.open_by_process,
            ProbeOutcome::Unavailable(ProbeReason::TimedOut)
        );
    }
}
