//! `node.clean.node_modules`: delete a detected `node_modules/` directory
//! directly.
//!
//! There is no single "npm clean node_modules" command that is simpler or
//! safer across the three package managers this tool may encounter (npm,
//! pnpm, yarn) than deleting the directory itself: `npm prune` only
//! removes packages not listed in `package.json`, `pnpm store prune`
//! cleans pnpm's *content-addressable store*, not a project's
//! `node_modules`, and yarn has no equivalent single-purpose command at
//! all. All three package managers regenerate `node_modules` from their
//! lockfile on the next install regardless of how it was removed, so a
//! plain directory delete is the correct, safe, and simplest action here.

use crate::evidence::model::{Evidence, EvidenceField, Recoverability, ResourceKind};

use super::{Action, ActionError, ActionId, ActionPlan, ActionStep};

pub const ID: ActionId = ActionId("node.clean.node_modules");

pub struct NodeCleanNodeModules;

impl Action for NodeCleanNodeModules {
    fn id(&self) -> ActionId {
        ID
    }

    fn applies_to(&self) -> &'static [ResourceKind] {
        &[ResourceKind::NodeModules]
    }

    fn required_evidence(&self) -> &'static [EvidenceField] {
        &[EvidenceField::ReclaimableBytes]
    }

    fn recoverability(&self) -> Recoverability {
        Recoverability::RegenerableByRebuild
    }

    fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError> {
        if ev.resource.kind != ResourceKind::NodeModules {
            return Err(ActionError::ResourceMismatch);
        }

        let node_modules_path = match &ev.resource.locator {
            crate::evidence::model::ResourceLocator::Path(p) => p.clone(),
            crate::evidence::model::ResourceLocator::Tool { .. } => {
                return Err(ActionError::Unsupported(
                    "node_modules resource must be a Path locator".to_string(),
                ))
            }
        };

        let explain = format!(
            "Delete {} directly (no single safe cleanup command spans npm/pnpm/yarn)",
            node_modules_path.display()
        );

        Ok(ActionPlan {
            action: ID,
            resource: ev.resource.clone(),
            steps: vec![ActionStep::DeletePath {
                path: node_modules_path,
            }],
            expected_reclaimed_bytes: ev.reclaimable_bytes.clone(),
            explain,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::model::{
        NativeCleanup, ResourceFingerprint, ResourceId, ResourceKind as RK, ResourceLocator as RL,
    };
    use crate::evidence::probe::{ProbeOutcome, ProbeReason};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn evidence_for(resource_path: PathBuf, kind: RK) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, RL::Path(resource_path)),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(2048),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
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
    fn resource_mismatch_is_rejected() {
        let ev = evidence_for(PathBuf::from("/tmp/x/node_modules"), RK::CargoTargetDir);
        assert_eq!(
            NodeCleanNodeModules.plan(&ev).unwrap_err(),
            ActionError::ResourceMismatch
        );
    }

    #[test]
    fn plan_renders_expected_delete_path_step() {
        let node_modules = PathBuf::from("/tmp/x/node_modules");
        let ev = evidence_for(node_modules.clone(), RK::NodeModules);

        let plan = NodeCleanNodeModules.plan(&ev).expect("plan should succeed");
        assert_eq!(plan.action, ID);
        assert_eq!(plan.steps.len(), 1);
        match &plan.steps[0] {
            ActionStep::DeletePath { path } => assert_eq!(path, &node_modules),
            other => panic!("expected DeletePath, got {other:?}"),
        }
    }

    /// Real-execution test against a disposable tempdir fixture.
    #[test]
    fn plan_step_actually_removes_node_modules_when_run() {
        let root = make_temp_dir("node-real-exec");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(node_modules.join("some-pkg")).unwrap();
        fs::write(node_modules.join("some-pkg/index.js"), vec![0u8; 1024]).unwrap();

        let ev = evidence_for(node_modules.clone(), RK::NodeModules);
        let plan = NodeCleanNodeModules.plan(&ev).expect("plan should succeed");
        let ActionStep::DeletePath { path } = &plan.steps[0] else {
            panic!("expected a DeletePath step");
        };

        fs::remove_dir_all(path).expect("failed to remove node_modules");
        assert!(!node_modules.exists());

        fs::remove_dir_all(&root).ok();
    }
}
