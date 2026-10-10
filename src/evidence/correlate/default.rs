//! Composes the four real probes into one [`EvidenceCollector`].

use std::collections::HashMap;

use super::git::{GitCliProbe, GitProbe};
use super::host_dependency::{HostDependencyProbe, HostDependencyRoots, LiveHostDependencyProbe};
use super::open_files::{LsofOpenFileProbe, OpenFileProbe};
use super::process::{LsofProcessCwdProbe, ProcessCwdProbe};
use super::process_identity::{LiveProcessIdentityProbe, ProcessIdentityProbe};
use super::tool_liveness::{SystemToolLivenessProbe, ToolLivenessProbe};
use super::{CorrelationResult, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{ProcessIdentity, ProcessRef, ResourceId, ResourceLocator};
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
    /// HORO-1823 §4.1. Enriches every `ProcessRef` already discovered by
    /// the probes above with bounded identity (never discovers holders on
    /// its own — see [`ProcessRef`]'s doc comment).
    process_identity: Box<dyn ProcessIdentityProbe + Send + Sync>,
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
        Self::with_process_identity(
            open_files,
            process_cwd,
            git,
            tool_liveness,
            host_dependency,
            Box::new(LiveProcessIdentityProbe::default()),
        )
    }

    /// Same as [`Self::new`], plus an injected identity probe — used by
    /// tests that need to exercise identity enrichment (missing pid,
    /// zombie, tuple mismatch) without depending on real `ps`/`launchctl`.
    pub fn with_process_identity(
        open_files: Box<dyn OpenFileProbe + Send + Sync>,
        process_cwd: Box<dyn ProcessCwdProbe + Send + Sync>,
        git: Box<dyn GitProbe + Send + Sync>,
        tool_liveness: Box<dyn ToolLivenessProbe + Send + Sync>,
        host_dependency: Option<Box<dyn HostDependencyProbe + Send + Sync>>,
        process_identity: Box<dyn ProcessIdentityProbe + Send + Sync>,
    ) -> Self {
        Self {
            open_files,
            process_cwd,
            git,
            tool_liveness,
            host_dependency,
            process_identity,
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

/// Applies an already-collected identity map to every [`ProcessRef`] in
/// `refs`, in place. The holder itself is NEVER removed or filtered by
/// this step; only its `identity` field changes, and it always changes
/// to SOME `Unavailable`/`Observed` value — enrichment was attempted for
/// every pid passed into the identity probe that produced `identities`,
/// so a pid missing from the result map (exited, a transient gap, or the
/// whole probe call failing) is set to `Unavailable(Failed)` here rather
/// than left at its prior `Unavailable(NotAttempted)` default, which
/// would misreport "never even tried".
///
/// Deliberately takes an already-collected `&HashMap`, not a probe to
/// call — see [`collect`]'s single call to
/// [`ProcessIdentityProbe::identities_for`], covering every pid across
/// `open_by_process`, `process_cwd_match` and `running_inside` combined.
/// Calling the underlying probe once per list (as an earlier version of
/// this code did) would multiply the identity probe's own subprocess
/// timeout budget by up to 3x against a single `collect()` call's
/// `ProbeBudget` — exactly the risk the module doc comment on
/// `process_identity.rs` says the batched-per-pid design exists to
/// avoid.
fn apply_identities(
    refs: &mut [ProcessRef],
    identities: &HashMap<u32, ProbeOutcome<ProcessIdentity>>,
) {
    for r in refs.iter_mut() {
        r.identity = identities
            .get(&r.pid)
            .cloned()
            .unwrap_or(ProbeOutcome::Unavailable(ProbeReason::Failed));
    }
}

impl EvidenceCollector for DefaultEvidenceCollector {
    fn collect(&self, id: &ResourceId, budget: ProbeBudget) -> CorrelationResult {
        let path = match &id.locator {
            ResourceLocator::Path(p) => Some(p.as_path()),
            ResourceLocator::Tool { .. } => None,
        };

        let (mut open_by_process, mut process_cwd_match, git_state, mut executable_dependency) =
            match path {
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

        // HORO-1823 §4.1 identity enrichment. Runs on every `ProcessRef`
        // already produced above — each probe's own holder-discovery
        // result is already finalized at this point; this step only adds
        // `identity`, never changes which pids are present.
        //
        // The identity probe itself is called exactly ONCE per
        // `collect()`, over the union of pids across all three lists —
        // not once per list. Each of the probe's own subprocess calls
        // (`ps`/`launchctl`/`lsof`) is already batched across pids
        // internally; calling it three separate times here would still
        // multiply ITS timeout budget by up to 3x against this single
        // `collect()` call's `budget.timeout` (worst case ~3x
        // `REVALIDATION_TIMEOUT`), which is exactly what the batching
        // exists to avoid.
        let mut pids: Vec<u32> = Vec::new();
        if let ProbeOutcome::Observed(procs) = &open_by_process {
            pids.extend(procs.iter().map(|p| p.pid));
        }
        if let ProbeOutcome::Observed(procs) = &process_cwd_match {
            pids.extend(procs.iter().map(|p| p.pid));
        }
        if let ProbeOutcome::Observed(report) = &executable_dependency {
            pids.extend(report.running_inside.iter().map(|p| p.pid));
        }
        pids.sort_unstable();
        pids.dedup();

        if !pids.is_empty() {
            let identities = self.process_identity.identities_for(&pids, budget.timeout);
            if let ProbeOutcome::Observed(procs) = &mut open_by_process {
                apply_identities(procs, &identities);
            }
            if let ProbeOutcome::Observed(procs) = &mut process_cwd_match {
                apply_identities(procs, &identities);
            }
            if let ProbeOutcome::Observed(report) = &mut executable_dependency {
                apply_identities(&mut report.running_inside, &identities);
            }
        }

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
            ProbeOutcome::Observed(vec![ProcessRef::new(1, "open_files".to_string())])
        }
    }

    struct FakeProcessCwd;
    impl ProcessCwdProbe for FakeProcessCwd {
        fn processes_with_cwd_under(
            &self,
            _path: &Path,
            _timeout: Duration,
        ) -> ProbeOutcome<Vec<ProcessRef>> {
            ProbeOutcome::Observed(vec![ProcessRef::new(2, "cwd".to_string())])
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

    /// Never discovers anything; every pid is simply absent from the
    /// returned map, matching a real identity probe's behavior when it
    /// cannot run or when a pid has already exited.
    struct NoOpProcessIdentity;
    impl ProcessIdentityProbe for NoOpProcessIdentity {
        fn identities_for(
            &self,
            _pids: &[u32],
            _timeout: Duration,
        ) -> HashMap<u32, ProbeOutcome<ProcessIdentity>> {
            HashMap::new()
        }
    }

    fn fake_collector() -> DefaultEvidenceCollector {
        DefaultEvidenceCollector::with_process_identity(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
            Some(Box::new(FakeHostDependency)),
            Box::new(NoOpProcessIdentity),
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

        // `NoOpProcessIdentity` returns an empty map, so identity
        // enrichment (which still runs) sets `Unavailable(Failed)` for
        // both holders — "attempted, unresolved", never `NotAttempted`.
        assert_eq!(
            result.open_by_process,
            ProbeOutcome::Observed(vec![ProcessRef::new(1, "open_files".to_string())
                .with_identity(ProbeOutcome::Unavailable(ProbeReason::Failed))])
        );
        assert_eq!(
            result.process_cwd_match,
            ProbeOutcome::Observed(vec![ProcessRef::new(2, "cwd".to_string())
                .with_identity(ProbeOutcome::Unavailable(ProbeReason::Failed))])
        );
        assert!(matches!(result.git_state, ProbeOutcome::Observed(Some(_))));
        assert_eq!(result.tool_liveness, ProbeOutcome::Observed(true));
        assert!(result.executable_dependency.is_observed());
    }

    /// `host_dependency: None` (e.g. no resolvable `$HOME`) must report
    /// `Unavailable(ToolAbsent)`, never a silent clean negative.
    #[test]
    fn missing_host_dependency_probe_is_unavailable_not_a_clean_negative() {
        let collector = DefaultEvidenceCollector::with_process_identity(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
            None,
            Box::new(NoOpProcessIdentity),
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

    /// Identity enrichment adds `identity` without ever filtering the
    /// holder list: pid 1 ("open_files") gets a real-looking identity from
    /// the fake probe; pid 2 ("cwd") has no entry in the fake probe's map
    /// at all (simulating a pid that exited between discovery and
    /// enrichment, or a transient `ps` gap) and must still be present in
    /// `process_cwd_match`, with its identity set to `Unavailable(Failed)`
    /// — enrichment was attempted for it, so it is never left at (or
    /// reported as) `NotAttempted`, and it is never dropped or defaulted
    /// to a synthesized value.
    #[test]
    fn identity_enrichment_never_removes_a_holder_it_cannot_identify() {
        struct PartialProcessIdentity;
        impl ProcessIdentityProbe for PartialProcessIdentity {
            fn identities_for(
                &self,
                pids: &[u32],
                _timeout: Duration,
            ) -> HashMap<u32, ProbeOutcome<ProcessIdentity>> {
                let mut out = HashMap::new();
                if pids.contains(&1) {
                    out.insert(
                        1,
                        ProbeOutcome::Observed(ProcessIdentity {
                            start_time: std::time::SystemTime::UNIX_EPOCH,
                            uid: 501,
                            ppid: 1,
                            pgid: 1,
                            state_zombie: false,
                            exe: ProbeOutcome::Unavailable(ProbeReason::Failed),
                            supervisor: crate::evidence::model::Supervisor::Unknown,
                        }),
                    );
                }
                // pid 2 is deliberately absent.
                out
            }
        }

        let collector = DefaultEvidenceCollector::with_process_identity(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
            Some(Box::new(FakeHostDependency)),
            Box::new(PartialProcessIdentity),
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

        let open_by_process = result.open_by_process.observed().unwrap();
        assert_eq!(open_by_process.len(), 1);
        assert!(open_by_process[0].identity.is_observed());

        // pid 2 must still be present — identity enrichment is additive
        // only, never a filter.
        let process_cwd_match = result.process_cwd_match.observed().unwrap();
        assert_eq!(process_cwd_match.len(), 1);
        assert_eq!(process_cwd_match[0].pid, 2);
        assert_eq!(
            process_cwd_match[0].identity,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }

    /// The identity probe must be called exactly ONCE per `collect()`,
    /// over the union of pids across `open_by_process`,
    /// `process_cwd_match` AND `executable_dependency.running_inside` —
    /// never once per list. `FakeHostDependencyWithRunning` below
    /// reports pid 1 as `running_inside` too (the same pid
    /// `FakeOpenFiles` already reports), so this also proves a pid
    /// appearing in more than one list is still only looked up once.
    #[test]
    fn identity_probe_is_called_exactly_once_per_collect_over_the_union_of_pids() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct FakeHostDependencyWithRunning;
        impl HostDependencyProbe for FakeHostDependencyWithRunning {
            fn probe(
                &self,
                _resource_path: &Path,
                _timeout: Duration,
            ) -> ProbeOutcome<crate::evidence::ExecutableDependencyReport> {
                let mut report = crate::evidence::ExecutableDependencyReport::empty();
                // pid 1 overlaps with FakeOpenFiles's pid 1; pid 3 is
                // unique to this list.
                report.running_inside = vec![
                    ProcessRef::new(1, "open_files".to_string()),
                    ProcessRef::new(3, "running".to_string()),
                ];
                ProbeOutcome::Observed(report)
            }
        }

        struct CountingProcessIdentity {
            calls: Arc<AtomicUsize>,
            seen_pids: Arc<std::sync::Mutex<Vec<u32>>>,
        }
        impl ProcessIdentityProbe for CountingProcessIdentity {
            fn identities_for(
                &self,
                pids: &[u32],
                _timeout: Duration,
            ) -> HashMap<u32, ProbeOutcome<ProcessIdentity>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.seen_pids.lock().unwrap().extend_from_slice(pids);
                HashMap::new()
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let seen_pids = Arc::new(std::sync::Mutex::new(Vec::new()));
        let collector = DefaultEvidenceCollector::with_process_identity(
            Box::new(FakeOpenFiles),
            Box::new(FakeProcessCwd),
            Box::new(FakeGit),
            Box::new(FakeToolLiveness),
            Some(Box::new(FakeHostDependencyWithRunning)),
            Box::new(CountingProcessIdentity {
                calls: calls.clone(),
                seen_pids: seen_pids.clone(),
            }),
        );
        let id = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/tmp/example")),
        );

        collector.collect(
            &id,
            ProbeBudget {
                timeout: Duration::from_secs(1),
            },
        );

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "identity probe must be called exactly once per collect(), not once per list"
        );
        let mut seen = seen_pids.lock().unwrap().clone();
        seen.sort_unstable();
        seen.dedup();
        // pid 1: open_files AND running_inside; pid 2: cwd; pid 3: running_inside only.
        assert_eq!(seen, vec![1, 2, 3]);
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
