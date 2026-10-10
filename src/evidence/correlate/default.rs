//! Composes the four real probes into one [`EvidenceCollector`].

use super::git::{GitCliProbe, GitProbe};
use super::host_dependency::{HostDependencyProbe, HostDependencyRoots, LiveHostDependencyProbe};
use super::open_files::{LsofOpenFileProbe, OpenFileProbe};
use super::process::{LsofProcessCwdProbe, ProcessCwdProbe};
use super::tool_liveness::{SystemToolLivenessProbe, ToolLivenessProbe};
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
    /// HORO-1825 §4.3. `None` when [`HostDependencyRoots::from_env`]
    /// could not resolve a real `$HOME` (e.g. a sandboxed/CI environment
    /// with no home directory at all) — in that case every Path-locator
    /// resource reports this field `Unavailable(ToolAbsent)` rather than
    /// guessing at roots that don't exist. Never defaults to `$HOME`
    /// inside `collect()` itself; see the module header on
    /// `host_dependency.rs` for why.
    host_dependency: Option<Box<dyn HostDependencyProbe + Send + Sync>>,
}

impl DefaultEvidenceCollector {
    /// Builds a collector from injected probes — used by tests to supply
    /// fakes instead of the real subprocess-backed implementations.
    pub fn new(
        open_files: Box<dyn OpenFileProbe + Send + Sync>,
        process_cwd: Box<dyn ProcessCwdProbe + Send + Sync>,
        git: Box<dyn GitProbe + Send + Sync>,
        tool_liveness: Box<dyn ToolLivenessProbe + Send + Sync>,
        host_dependency: Option<Box<dyn HostDependencyProbe + Send + Sync>>,
    ) -> Self {
        Self {
            open_files,
            process_cwd,
            git,
            tool_liveness,
            host_dependency,
        }
    }
}

impl Default for DefaultEvidenceCollector {
    fn default() -> Self {
        let host_dependency: Option<Box<dyn HostDependencyProbe + Send + Sync>> =
            HostDependencyRoots::from_env()
                .map(|roots| Box::new(LiveHostDependencyProbe::new(roots)) as Box<_>);
        Self::new(
            Box::new(LsofOpenFileProbe),
            Box::new(LsofProcessCwdProbe),
            Box::new(GitCliProbe),
            Box::new(SystemToolLivenessProbe),
            host_dependency,
        )
    }
}

impl EvidenceCollector for DefaultEvidenceCollector {
    fn collect(&self, id: &ResourceId, budget: ProbeBudget) -> CorrelationResult {
        let path = match &id.locator {
            ResourceLocator::Path(p) => Some(p.as_path()),
            ResourceLocator::Tool { .. } => None,
        };

        let (open_by_process, process_cwd_match, git_state, executable_dependency) = match path {
            Some(path) => (
                self.open_files
                    .processes_with_open_files_under(path, budget.timeout),
                self.process_cwd
                    .processes_with_cwd_under(path, budget.timeout),
                self.git.state_of(path, budget.timeout),
                match &self.host_dependency {
                    Some(probe) => probe.probe(path, budget.timeout),
                    None => ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
                },
            ),
            None => (
                ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
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
            executable_dependency,
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
                common_dir: path.join(".git"),
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

    struct FakeHostDependency;
    impl HostDependencyProbe for FakeHostDependency {
        fn probe(
            &self,
            _resource_path: &Path,
            _timeout: Duration,
        ) -> ProbeOutcome<crate::evidence::ExecutableDependencyReport> {
            ProbeOutcome::Observed(crate::evidence::ExecutableDependencyReport::empty())
        }
    }

    fn fake_collector() -> DefaultEvidenceCollector {
        DefaultEvidenceCollector::new(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
            Some(Box::new(FakeHostDependency)),
        )
    }

    #[test]
    fn path_locator_runs_all_five_probes() {
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
        assert!(result.executable_dependency.is_observed());
    }

    /// `host_dependency: None` (e.g. no resolvable `$HOME`) must report
    /// `Unavailable(ToolAbsent)`, never a silent clean negative.
    #[test]
    fn missing_host_dependency_probe_is_unavailable_not_a_clean_negative() {
        let collector = DefaultEvidenceCollector::new(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
            None,
        );
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
            result.executable_dependency,
            ProbeOutcome::Unavailable(ProbeReason::ToolAbsent)
        );
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
        assert_eq!(
            result.executable_dependency,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }
}
