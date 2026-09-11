//! Homebrew cache detector.
//!
//! Uses `brew --cache` (argument-array `Command`, never shell string
//! interpolation) to find Homebrew's cache directory rather than
//! hardcoding a path — the location can be overridden by `HOMEBREW_CACHE`
//! and differs between Intel/Apple Silicon default prefixes.
//!
//! `reclaimable_bytes` (HORO-992): rather than parsing `brew cleanup -n`'s
//! dry-run output (its exact wording/format is not a stable contract
//! across Homebrew versions, so parsing it reliably is real ongoing
//! maintenance risk), this detector reuses the same [`shallow_logical_bytes`]
//! estimate it already computes for `logical_bytes` against `brew --cache`'s
//! path. The cache directory holds only downloaded bottles/sources that
//! Homebrew fully owns and can re-download on demand, so
//! `reclaimable_bytes == logical_bytes` is an honest equivalence of
//! meaning here. Because the underlying probe is shallow/non-recursive
//! (see [`shallow_logical_bytes`]) and the cache directory does have
//! nested subdirectories (e.g. `Cask/`), both figures are an honest
//! *lower bound* on the real cache size, not a precise one.

use std::path::PathBuf;
use std::process::Command;

use crate::evidence::{
    NativeCleanup, Recoverability, Regenerability, ResourceId, ResourceKind, ResourceLocator,
};

use super::{
    discovery_evidence, probe_mtime, shallow_logical_bytes, Detector, DetectorId, DetectorStatus,
    DiscoveryContext,
};

pub struct HomebrewDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::HomebrewCache];

impl Detector for HomebrewDetector {
    fn id(&self) -> DetectorId {
        DetectorId("homebrew_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
        let output = match Command::new("brew").arg("--cache").output() {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return DetectorStatus::ToolAbsent;
            }
            Err(e) => return DetectorStatus::Failed(format!("failed to spawn brew: {e}")),
        };

        if !output.status.success() {
            return DetectorStatus::Failed(format!(
                "brew --cache exited with status {}",
                output.status
            ));
        }

        let raw_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw_path.is_empty() {
            return DetectorStatus::Failed("brew --cache returned an empty path".to_string());
        }

        let cache_path = PathBuf::from(&raw_path);
        let canonical = match cache_path.canonicalize() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // brew is installed but has never populated a cache yet.
                return DetectorStatus::ToolAbsent;
            }
            Err(e) => {
                return DetectorStatus::Failed(format!("failed to canonicalize {raw_path}: {e}"))
            }
        };

        let resource = ResourceId::new(
            ResourceKind::HomebrewCache,
            ResourceLocator::Path(canonical.clone()),
        );
        let logical_bytes = shallow_logical_bytes(&canonical);
        let evidence = discovery_evidence(
            resource,
            self.id(),
            &canonical,
            logical_bytes.clone(),
            logical_bytes,
            probe_mtime(&canonical),
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            // Registering the actual `brew cleanup` action as a typed
            // NativeCleanup::Available(ActionId) is HORO-951's job (the
            // actions module doesn't exist yet); this ticket only detects
            // the resource.
            NativeCleanup::Unsupported,
        );

        DetectorStatus::Found(vec![evidence])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_kinds_reports_homebrew_cache() {
        assert_eq!(
            HomebrewDetector.resource_kinds(),
            &[ResourceKind::HomebrewCache]
        );
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(HomebrewDetector.id(), DetectorId("homebrew_cache"));
    }

    // No integration test spawning a real `brew` process here: whether
    // `brew` is installed/absent is environment-dependent (and CI must
    // not depend on it), so behavior is covered structurally by the
    // ToolAbsent/Failed/Found branches' shared helpers, exercised directly
    // in the other detectors' tests plus the discover_all smoke test in
    // `detectors::tests`.
}
