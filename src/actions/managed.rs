//! Only the ownership core's validated source-free generated capability can reach this action.
use super::{Action, ActionError, ActionId, ActionPlan, ActionStep};
use crate::evidence::model::{
    Evidence, EvidenceField, Recoverability, ResourceKind, ResourceLocator,
};
pub struct ManagedGeneratedClean;
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
        if ev.resource.kind != ResourceKind::ManagedGenerated {
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
        Ok(ActionPlan{action:self.id(),resource:ev.resource.clone(),steps:vec![ActionStep::DeletePath{path:path.clone()}],expected_reclaimed_bytes:ev.reclaimable_bytes.clone(),explain:"Retire one enrolled, sealed generated-only generation under its exclusive ownership barrier".into()})
    }
}
