//! Cargo `target/` directory detector.
//!
//! MVP scope: this detector does not discover arbitrary project roots on
//! disk (that would require a full-disk walk, which is the scanner's job,
//! not a detector's). It only checks each root in
//! [`DiscoveryContext::known_project_roots`] for a `target/` directory —
//! callers (CLI wiring, future config) are responsible for supplying that
//! list. With an empty list, this detector reports [`DetectorStatus::ToolAbsent`].
//!
//! `reclaimable_bytes` reuses the same [`shallow_logical_bytes`] estimate
//! as `logical_bytes` (HORO-992): a `target/` directory is fully owned,
//! regenerable build output with no partial-retention concept, so
//! `reclaimable_bytes == logical_bytes` is an honest equivalence of
//! meaning here. Because the underlying probe is shallow/non-recursive,
//! both figures are an honest *lower bound* on the real subtree size, not
//! a precise one.

use std::path::PathBuf;

use crate::evidence::{
    NativeCleanup, Recoverability, Regenerability, ResourceId, ResourceKind, ResourceLocator,
};

use super::{
    discovery_evidence, probe_mtime, shallow_logical_bytes, Detector, DetectorId, DetectorStatus,
    DiscoveryContext,
};

pub struct CargoDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::CargoTargetDir];

impl Detector for CargoDetector {
    fn id(&self) -> DetectorId {
        DetectorId("cargo_target_dir")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let mut evidence = Vec::new();
        let mut saw_permission_error = false;

        for root in &ctx.known_project_roots {
            let target_dir: PathBuf = root.join("target");
            match target_dir.canonicalize() {
                Ok(canonical) => {
                    let resource = ResourceId::new(
                        ResourceKind::CargoTargetDir,
                        ResourceLocator::Path(canonical.clone()),
                    )
                    .with_source_project_root(root.clone());
                    let logical_bytes = shallow_logical_bytes(&canonical);
                    evidence.push(discovery_evidence(
                        resource,
                        self.id(),
                        &canonical,
                        logical_bytes.clone(),
                        logical_bytes,
                        probe_mtime(&canonical),
                        Regenerability::RegenerableByRebuild,
                        Recoverability::RegenerableByRebuild,
                        NativeCleanup::Unsupported,
                    ));
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
        assert_eq!(CargoDetector.discover(&ctx), DetectorStatus::ToolAbsent);
    }

    #[test]
    fn project_root_without_target_dir_is_tool_absent() {
        let root = make_temp_dir("cargo-no-target");
        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);

        assert_eq!(CargoDetector.discover(&ctx), DetectorStatus::ToolAbsent);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn project_root_with_target_dir_yields_evidence() {
        let root = make_temp_dir("cargo-with-target");
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/build_output.bin"), vec![0u8; 4096]).unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);
        let status = CargoDetector.discover(&ctx);

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].resource.kind, ResourceKind::CargoTargetDir);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1017: the discovered `Evidence`'s `resource.source_project_root`
    /// must carry the exact `known_project_roots` entry the target dir was
    /// found under, so a later action can bind manifest lookup to the
    /// real discovered root instead of re-deriving it from the
    /// (possibly symlink-resolved) canonicalized target path.
    #[test]
    fn evidence_resource_carries_the_discovered_project_root() {
        let root = make_temp_dir("cargo-project-root-carried");
        fs::create_dir_all(root.join("target")).unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);
        let status = CargoDetector.discover(&ctx);

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].resource.source_project_root.as_deref(),
                    Some(root.as_path())
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }
}
