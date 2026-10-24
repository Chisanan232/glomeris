//! Discover only existing core enrollment; never adopt directory-name guesses.
use super::{
    discovery_evidence, estimate_logical_bytes, probe_mtime, size_estimate_budget, Detector,
    DetectorId, DetectorStatus, DiscoveryContext,
};
use crate::evidence::{
    NativeCleanup, ProbeOutcome, Recoverability, Regenerability, ResourceId, ResourceKind,
    ResourceLocator,
};
pub struct ManagedGeneratedDetector;
impl Detector for ManagedGeneratedDetector {
    fn id(&self) -> DetectorId {
        DetectorId("managed_generated")
    }
    fn resource_kinds(&self) -> &'static [ResourceKind] {
        &[
            ResourceKind::ManagedGenerated,
            ResourceKind::ManagedCargoLibrary,
        ]
    }
    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let store = crate::ownership::Store {
            root: ctx
                .home_dir
                .join("Library/Application Support/Glomeris/resources"),
        };
        let ids = match store.batch_ids("sweep") {
            Ok(ids) => ids,
            Err(e) => return DetectorStatus::Failed(e.to_string()),
        };
        let mut all = vec![];
        for id in ids.into_iter().take(64) {
            let record = match store.read(&id) {
                Ok(r) => r,
                Err(e) => return DetectorStatus::Failed(e.to_string()),
            };
            if record.reclaimed {
                continue;
            }
            let kind = if record.class == "managed_cargo_library" {
                ResourceKind::ManagedCargoLibrary
            } else {
                ResourceKind::ManagedGenerated
            };
            let action = if kind == ResourceKind::ManagedCargoLibrary {
                "managed.clean.cargo-library"
            } else {
                "managed.clean.generated"
            };
            let path = store.path(&record);
            let size = estimate_logical_bytes(&path, size_estimate_budget());
            let mut evidence = discovery_evidence(
                ResourceId::new(kind, ResourceLocator::Path(path.clone())),
                self.id(),
                &path,
                size.bytes.clone(),
                size.bytes.clone(),
                size.is_lower_bound(),
                probe_mtime(&path),
                Regenerability::RegenerableByRebuild,
                Recoverability::RegenerableByRebuild,
                NativeCleanup::Available(crate::actions::ActionId(action)),
            );
            let view = store.inspect(
                &record,
                evidence
                    .collected_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
            evidence.managed_ownership = Some(ProbeOutcome::Observed(
                crate::ownership::OwnershipEvidence {
                    reclaimable: view.eligible,
                    blockers: view.blockers,
                },
            ));
            all.push(evidence);
        }
        DetectorStatus::Found(all)
    }
}
