//! Only the ownership core's validated source-free generated capability can reach this action.
use super::{Action, ActionError, ActionId, ActionPlan, ActionStep};
use crate::evidence::model::{
    Evidence, EvidenceField, Recoverability, ResourceKind, ResourceLocator,
};
pub struct ManagedGeneratedClean;
pub struct ManagedCargoLibraryClean;

fn plan_managed_generation(
    ev: &Evidence,
    kind: ResourceKind,
    action: ActionId,
) -> Result<ActionPlan, ActionError> {
    if ev.resource.kind != kind {
        return Err(ActionError::ResourceMismatch);
    }
    let ResourceLocator::Path(path) = &ev.resource.locator else {
        return Err(ActionError::ResourceMismatch);
    };
    if !ev
        .managed_ownership
        .as_ref()
        .and_then(|p| p.observed())
        .is_some_and(|v| v.reclaimable)
    {
        return Err(ActionError::Unsupported(
            "managed ownership/reconstruction evidence incomplete".into(),
        ));
    }
    Ok(ActionPlan {
        action,
        resource: ev.resource.clone(),
        steps: vec![ActionStep::DeletePath { path: path.clone() }],
        expected_reclaimed_bytes: ev.reclaimable_bytes.clone(),
        explain: "Retire one enrolled, sealed generation under its exclusive ownership barrier"
            .into(),
    })
}

impl Action for ManagedGeneratedClean {
    fn id(&self) -> ActionId {
        ActionId("managed.clean.generated")
    }
    fn applies_to(&self) -> &'static [ResourceKind] {
        &[ResourceKind::ManagedGenerated]
    }
    fn required_evidence(&self) -> &'static [EvidenceField] {
        ResourceKind::ManagedGenerated.required_evidence()
    }
    fn recoverability(&self) -> Recoverability {
        Recoverability::RegenerableByRebuild
    }
    fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError> {
        plan_managed_generation(ev, ResourceKind::ManagedGenerated, self.id())
    }
}

impl Action for ManagedCargoLibraryClean {
    fn id(&self) -> ActionId {
        ActionId("managed.clean.cargo-library")
    }
    fn applies_to(&self) -> &'static [ResourceKind] {
        &[ResourceKind::ManagedCargoLibrary]
    }
    fn required_evidence(&self) -> &'static [EvidenceField] {
        ResourceKind::ManagedCargoLibrary.required_evidence()
    }
    fn recoverability(&self) -> Recoverability {
        Recoverability::RegenerableByRebuild
    }
    fn plan(&self, ev: &Evidence) -> Result<ActionPlan, ActionError> {
        plan_managed_generation(ev, ResourceKind::ManagedCargoLibrary, self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{
        NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint, ResourceId,
    };

    fn evidence(kind: ResourceKind) -> Evidence {
        Evidence {
            resource: ResourceId::new(
                kind,
                ResourceLocator::Path(std::path::PathBuf::from("/managed/generation")),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: crate::detectors::DetectorId("managed_test"),
            logical_bytes: ProbeOutcome::Observed(64),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(64),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(std::time::SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Available(ActionId("managed.clean.cargo-library")),
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            docker_lifecycle: None,
            executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            managed_ownership: Some(ProbeOutcome::Observed(
                crate::ownership::OwnershipEvidence {
                    reclaimable: true,
                    blockers: Vec::new(),
                },
            )),
            cargo_target_scope: None,
            collected_at: std::time::SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn cargo_library_action_has_distinct_identity_and_exact_kind_scope() {
        let action = ManagedCargoLibraryClean;
        assert_eq!(action.id(), ActionId("managed.clean.cargo-library"));
        assert_eq!(action.applies_to(), &[ResourceKind::ManagedCargoLibrary]);
        let registered = crate::actions::ActionRegistry::builtin();
        assert_eq!(
            registered
                .find_for_kind(ResourceKind::ManagedCargoLibrary)
                .expect("cargo library action is registered")
                .id(),
            action.id()
        );
        let plan = action
            .plan(&evidence(ResourceKind::ManagedCargoLibrary))
            .unwrap();
        assert_eq!(plan.action, action.id());
        assert!(matches!(
            plan.steps.as_slice(),
            [ActionStep::DeletePath { path }] if path == std::path::Path::new("/managed/generation")
        ));
        assert_eq!(
            action
                .plan(&evidence(ResourceKind::ManagedGenerated))
                .unwrap_err(),
            ActionError::ResourceMismatch
        );
        assert_eq!(
            action
                .plan(&evidence(ResourceKind::CargoTargetDir))
                .unwrap_err(),
            ActionError::ResourceMismatch
        );
    }

    #[test]
    fn cargo_library_action_refuses_missing_or_nonreclaimable_ownership() {
        let action = ManagedCargoLibraryClean;
        let mut ev = evidence(ResourceKind::ManagedCargoLibrary);
        ev.managed_ownership = None;
        assert!(matches!(action.plan(&ev), Err(ActionError::Unsupported(_))));

        ev.managed_ownership = Some(ProbeOutcome::Observed(
            crate::ownership::OwnershipEvidence {
                reclaimable: false,
                blockers: vec!["not sealed".into()],
            },
        ));
        assert!(matches!(action.plan(&ev), Err(ActionError::Unsupported(_))));
    }
}
