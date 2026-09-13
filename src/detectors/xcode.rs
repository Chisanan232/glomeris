//! Xcode DerivedData detector.
//!
//! Design choice: this detector emits ONE aggregate [`Evidence`] for the
//! whole `DerivedData` directory rather than one per top-level project
//! subdirectory. DerivedData subdirectories are keyed by a hash suffix
//! that changes across Xcode versions/project moves, so per-subdirectory
//! identity is not stable enough to be a useful resource handle yet;
//! aggregate-level evidence is what a later ticket's policy can reason
//! about safely today. Splitting into per-project evidence can be
//! revisited once there's a stable way to correlate a subdirectory back
//! to its owning `.xcodeproj`.
//!
//! `reclaimable_bytes` reuses the exact same [`estimate_logical_bytes`]
//! estimate as `logical_bytes` (HORO-992): DerivedData is fully owned,
//! regenerable build output with no partial-retention concept — deleting
//! it never leaves behind a smaller-but-still-useful remainder, so
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

pub struct XcodeDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::XcodeDerivedData];

impl Detector for XcodeDetector {
    fn id(&self) -> DetectorId {
        DetectorId("xcode_derived_data")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let derived_data = ctx.home_dir.join("Library/Developer/Xcode/DerivedData");

        let canonical: PathBuf = match derived_data.canonicalize() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Xcode never used / no DerivedData yet — expected, not an
                // error.
                return DetectorStatus::ToolAbsent;
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return DetectorStatus::Failed(format!(
                    "permission denied canonicalizing {}: {e}",
                    derived_data.display()
                ));
            }
            Err(e) => {
                return DetectorStatus::Failed(format!(
                    "failed to canonicalize {}: {e}",
                    derived_data.display()
                ))
            }
        };

        let resource = ResourceId::new(
            ResourceKind::XcodeDerivedData,
            ResourceLocator::Path(canonical.clone()),
        );
        let estimate = estimate_logical_bytes(&canonical, size_estimate_budget());
        let logical_bytes = estimate.bytes.clone();
        let mut evidence = discovery_evidence(
            resource,
            self.id(),
            &canonical,
            logical_bytes.clone(),
            logical_bytes,
            estimate.is_lower_bound(),
            probe_mtime(&canonical),
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            // No single safe native command deletes DerivedData subtrees
            // generically (Xcode itself only offers "Clean Build Folder"
            // per-project via the IDE, not a CLI-safe bulk delete) —
            // native cleanup support is left unregistered for MVP.
            NativeCleanup::Unsupported,
        );
        if let Some(note) = estimate.lower_bound_note() {
            evidence.push_source(note);
        }

        DetectorStatus::Found(vec![evidence])
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
    fn missing_derived_data_dir_is_tool_absent() {
        let home = make_temp_dir("xcode-absent");
        let ctx = DiscoveryContext::new(&home);

        let status = XcodeDetector.discover(&ctx);
        assert_eq!(status, DetectorStatus::ToolAbsent);

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn existing_derived_data_dir_yields_one_aggregate_evidence() {
        let home = make_temp_dir("xcode-present");
        let derived_data = home.join("Library/Developer/Xcode/DerivedData");
        fs::create_dir_all(derived_data.join("MyApp-abcdef")).unwrap();
        fs::write(derived_data.join("MyApp-abcdef/Info.plist"), vec![0u8; 128]).unwrap();

        let ctx = DiscoveryContext::new(&home);
        let status = XcodeDetector.discover(&ctx);

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].resource.kind, ResourceKind::XcodeDerivedData);
                assert_eq!(
                    evidence[0].regenerability,
                    Regenerability::RegenerableByRebuild
                );
                assert_eq!(evidence[0].native_cleanup, NativeCleanup::Unsupported);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&home).ok();
    }
}
