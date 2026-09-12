//! `node_modules` directory detector.
//!
//! Same shallow, config-driven shape as [`super::cargo::CargoDetector`]:
//! checks each [`DiscoveryContext::known_project_roots`] entry for a
//! `node_modules/` directory rather than discovering project roots on
//! disk itself.
//!
//! `reclaimable_bytes` reuses the same [`estimate_logical_bytes`] estimate
//! as `logical_bytes` (HORO-992): `node_modules/` is fully owned,
//! regenerable dependency output with no partial-retention concept, so
//! `reclaimable_bytes == logical_bytes` is an honest equivalence of
//! meaning here. The estimate recurses through the full subtree, bounded
//! by a size/time budget (HORO-1016) — see [`estimate_logical_bytes`]'s
//! own doc comment for what happens if that budget is hit before the walk
//! finishes (a truthful lower bound, never a precision guarantee).

use std::path::PathBuf;

use crate::evidence::{
    NativeCleanup, Recoverability, Regenerability, ResourceId, ResourceKind, ResourceLocator,
};

use super::{
    discovery_evidence, estimate_logical_bytes, probe_mtime, size_estimate_budget, Detector,
    DetectorId, DetectorStatus, DiscoveryContext,
};

pub struct NodeDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::NodeModules];

impl Detector for NodeDetector {
    fn id(&self) -> DetectorId {
        DetectorId("node_modules")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let mut evidence = Vec::new();
        let mut saw_permission_error = false;

        for root in &ctx.known_project_roots {
            let node_modules: PathBuf = root.join("node_modules");
            match node_modules.canonicalize() {
                Ok(canonical) => {
                    let resource = ResourceId::new(
                        ResourceKind::NodeModules,
                        ResourceLocator::Path(canonical.clone()),
                    );
                    let estimate = estimate_logical_bytes(&canonical, size_estimate_budget());
                    let logical_bytes = estimate.bytes.clone();
                    let mut ev = discovery_evidence(
                        resource,
                        self.id(),
                        &canonical,
                        logical_bytes.clone(),
                        logical_bytes,
                        probe_mtime(&canonical),
                        Regenerability::RegenerableByRebuild,
                        Recoverability::RegenerableByRebuild,
                        NativeCleanup::Unsupported,
                    );
                    if let Some(note) = estimate.lower_bound_note() {
                        ev.push_source(note);
                    }
                    evidence.push(ev);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    saw_permission_error = true;
                }
                Err(_) => continue,
            }
        }

        if evidence.is_empty() {
            if saw_permission_error {
                return DetectorStatus::Failed(
                    "permission denied probing one or more known project roots".to_string(),
                );
            }
            return DetectorStatus::ToolAbsent;
        }

        DetectorStatus::Found(evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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

    #[test]
    fn no_known_project_roots_is_tool_absent() {
        let ctx = DiscoveryContext::new("/tmp");
        assert_eq!(NodeDetector.discover(&ctx), DetectorStatus::ToolAbsent);
    }

    #[test]
    fn project_root_without_node_modules_is_tool_absent() {
        let root = make_temp_dir("node-no-modules");
        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);

        assert_eq!(NodeDetector.discover(&ctx), DetectorStatus::ToolAbsent);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn project_root_with_node_modules_yields_evidence() {
        let root = make_temp_dir("node-with-modules");
        fs::create_dir_all(root.join("node_modules/some-pkg")).unwrap();
        fs::write(root.join("node_modules/some-pkg/index.js"), vec![0u8; 2048]).unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);
        let status = NodeDetector.discover(&ctx);

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].resource.kind, ResourceKind::NodeModules);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }
}
