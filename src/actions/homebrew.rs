//! `homebrew.cleanup.cache`: run Homebrew's own `brew cleanup -s` rather
//! than deleting the cache directory directly, so Homebrew's own
//! retention logic (e.g. keeping the most recent downloads) is respected.

use crate::evidence::model::{Evidence, EvidenceField, Recoverability, ResourceKind};

use super::{Action, ActionError, ActionId, ActionPlan, ActionStep, ToolBinary};

pub const ID: ActionId = ActionId("homebrew.cleanup.cache");

pub struct HomebrewCleanupCache;

impl Action for HomebrewCleanupCache {
    fn id(&self) -> ActionId {
        ID
    }

    fn applies_to(&self) -> &'static [ResourceKind] {
        &[ResourceKind::HomebrewCache]
    }

    fn required_evidence(&self) -> &'static [EvidenceField] {
        &[EvidenceField::ReclaimableBytes]
    }

    fn recoverability(&self) -> Recoverability {
        Recoverability::RegenerableByTool
    }

    fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError> {
        if ev.resource.kind != ResourceKind::HomebrewCache {
            return Err(ActionError::ResourceMismatch);
        }

        Ok(ActionPlan {
            action: ID,
            resource: ev.resource.clone(),
            steps: vec![ActionStep::RunTool {
                tool: ToolBinary::Brew,
                args: vec!["cleanup".to_string(), "-s".to_string()],
            }],
            expected_reclaimed_bytes: ev.reclaimable_bytes.clone(),
            explain: "Run `brew cleanup -s` to let Homebrew prune its own cache under its own retention policy".to_string(),
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
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn evidence_for(resource_path: PathBuf, kind: RK) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, RL::Path(resource_path)),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByTool,
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
        let ev = evidence_for(PathBuf::from("/tmp/x/cache"), RK::CargoTargetDir);
        assert_eq!(
            HomebrewCleanupCache.plan(&ev).unwrap_err(),
            ActionError::ResourceMismatch
        );
    }

    /// Plan-only test — never spawns real `brew`. Real execution of this
    /// action is intentionally untested in CI (see PR known limitations).
    #[test]
    fn plan_renders_expected_run_tool_step_without_touching_real_brew() {
        let cache = PathBuf::from("/tmp/x/homebrew-cache");
        let ev = evidence_for(cache, RK::HomebrewCache);

        let plan = HomebrewCleanupCache.plan(&ev).expect("plan should succeed");
        assert_eq!(plan.action, ID);
        assert_eq!(plan.steps.len(), 1);
        match &plan.steps[0] {
            ActionStep::RunTool { tool, args } => {
                assert_eq!(*tool, ToolBinary::Brew);
                assert_eq!(args, &vec!["cleanup".to_string(), "-s".to_string()]);
            }
            other => panic!("expected RunTool, got {other:?}"),
        }
    }
}
