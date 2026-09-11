//! Composes the four real probes into one [`EvidenceCollector`].

use super::git::{GitCliProbe, GitProbe};
use super::open_files::{LsofOpenFileProbe, OpenFileProbe};
use super::process::{LsofProcessCwdProbe, ProcessCwdProbe};
use super::tool_liveness::{PgrepToolLivenessProbe, ToolLivenessProbe};
use super::{CorrelationResult, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{ResourceId, ResourceLocator};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

/// Composes [`OpenFileProbe`], [`ProcessCwdProbe`], [`GitProbe`], and
/// [`ToolLivenessProbe`] into one [`EvidenceCollector`].
///
/// The three path-based probes (open files, process cwd, git) only run
/// when the resource's [`ResourceLocator`] is a [`ResourceLocator::Path`]
/// — a [`ResourceLocator::Tool`] resource (no single canonical path) gets
/// `Unavailable(NotAttempted)` for those three fields, since there is no
/// path to probe against. `tool_liveness` always runs regardless of
/// locator kind, since it depends only on the resource's owning tool.
pub struct DefaultEvidenceCollector {
    open_files: Box<dyn OpenFileProbe + Send + Sync>,
    process_cwd: Box<dyn ProcessCwdProbe + Send + Sync>,
    git: Box<dyn GitProbe + Send + Sync>,
    tool_liveness: Box<dyn ToolLivenessProbe + Send + Sync>,
}

impl DefaultEvidenceCollector {
    /// Builds a collector from injected probes — used by tests to supply
    /// fakes instead of the real subprocess-backed implementations.
    pub fn new(
        open_files: Box<dyn OpenFileProbe + Send + Sync>,
        process_cwd: Box<dyn ProcessCwdProbe + Send + Sync>,
        git: Box<dyn GitProbe + Send + Sync>,
        tool_liveness: Box<dyn ToolLivenessProbe + Send + Sync>,
    ) -> Self {
        Self {
            open_files,
            process_cwd,
            git,
            tool_liveness,
        }
    }
}

impl Default for DefaultEvidenceCollector {
    fn default() -> Self {
        Self::new(
            Box::new(LsofOpenFileProbe),
            Box::new(LsofProcessCwdProbe),
            Box::new(GitCliProbe),
            Box::new(PgrepToolLivenessProbe),
        )
    }
}

impl EvidenceCollector for DefaultEvidenceCollector {
    fn collect(&self, id: &ResourceId, budget: ProbeBudget) -> CorrelationResult {
        let path = match &id.locator {
            ResourceLocator::Path(p) => Some(p.as_path()),
            ResourceLocator::Tool { .. } => None,
        };

        let (open_by_process, process_cwd_match, git_state) = match path {
            Some(path) => (
                self.open_files
                    .processes_with_open_files_under(path, budget.timeout),
                self.process_cwd
                    .processes_with_cwd_under(path, budget.timeout),
                self.git.state_of(path, budget.timeout),
            ),
            None => (
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            ),
        };

        let tool_liveness = self
            .tool_liveness
            .is_running(id.kind.owning_tool(), budget.timeout);

        CorrelationResult {
            open_by_process,
            process_cwd_match,
            git_state,
            tool_liveness,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::model::{OwningTool, ProcessRef, ResourceKind};
    use crate::evidence::GitState;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    struct FakeOpenFiles;
    impl OpenFileProbe for FakeOpenFiles {
        fn processes_with_open_files_under(
            &self,
            _path: &Path,
            _timeout: Duration,
        ) -> ProbeOutcome<Vec<ProcessRef>> {
            ProbeOutcome::Observed(vec![ProcessRef {
                pid: 1,
                command: "open_files".to_string(),
            }])
        }
    }

    struct FakeProcessCwd;
    impl ProcessCwdProbe for FakeProcessCwd {
        fn processes_with_cwd_under(
            &self,
            _path: &Path,
            _timeout: Duration,
        ) -> ProbeOutcome<Vec<ProcessRef>> {
            ProbeOutcome::Observed(vec![ProcessRef {
                pid: 2,
                command: "cwd".to_string(),
            }])
        }
    }

    struct FakeGit;
    impl GitProbe for FakeGit {
        fn state_of(&self, path: &Path, _timeout: Duration) -> ProbeOutcome<Option<GitState>> {
            ProbeOutcome::Observed(Some(GitState {
                repo_root: path.to_path_buf(),
                dirty: false,
                untracked: false,
                worktree: false,
            }))
        }
    }

    struct FakeToolLiveness;
    impl ToolLivenessProbe for FakeToolLiveness {
        fn is_running(&self, _tool: OwningTool, _timeout: Duration) -> ProbeOutcome<bool> {
            ProbeOutcome::Observed(true)
        }
    }

    fn fake_collector() -> DefaultEvidenceCollector {
        DefaultEvidenceCollector::new(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
        )
    }

    #[test]
    fn path_locator_runs_all_four_probes() {
        let collector = fake_collector();
        let id = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/tmp/example")),
        );

        let result = collector.collect(
            &id,
            ProbeBudget {
                timeout: Duration::from_secs(1),
            },
        );

        assert_eq!(
            result.open_by_process,
            ProbeOutcome::Observed(vec![ProcessRef {
                pid: 1,
                command: "open_files".to_string()
            }])
        );
        assert_eq!(
            result.process_cwd_match,
            ProbeOutcome::Observed(vec![ProcessRef {
                pid: 2,
                command: "cwd".to_string()
            }])
        );
        assert!(matches!(result.git_state, ProbeOutcome::Observed(Some(_))));
        assert_eq!(result.tool_liveness, ProbeOutcome::Observed(true));
    }

    #[test]
    fn tool_locator_skips_path_based_probes_but_still_checks_liveness() {
        let collector = fake_collector();
        let id = ResourceId::new(
            ResourceKind::DockerBuildCache,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: "abc123".to_string(),
            },
        );

        let result = collector.collect(
            &id,
            ProbeBudget {
                timeout: Duration::from_secs(1),
            },
        );

        assert_eq!(
            result.open_by_process,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            result.process_cwd_match,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(
            result.git_state,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
        assert_eq!(result.tool_liveness, ProbeOutcome::Observed(true));
    }
}
