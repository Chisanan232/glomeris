//! Detector registry (HORO-948).
//!
//! Each detector is a bounded, shallow probe of a known root for one
//! family of tool-owned resources — NOT a full filesystem walk (that's
//! [`crate::scanner`]'s job; detectors deliberately do not reuse
//! `scanner::walker`). A detector's tool being absent from the machine is
//! normal, expected state, never an error.

mod cargo;
mod docker;
mod homebrew;
mod node;
mod xcode;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::evidence::{
    Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, Regenerability,
    ResourceFingerprint, ResourceId,
};

/// Stable identifier for one detector, e.g. `"cargo_target_dir"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DetectorId(pub &'static str);

/// Outcome of running one detector's discovery pass.
///
/// `ToolAbsent` is normal, expected state — not every detector's tool is
/// installed on every machine. `Failed` must never be silently converted
/// to an empty/safe result by an upstream caller: a failed probe is not
/// evidence of "nothing to clean up", it is evidence of "we don't know."
/// This ticket's code only constructs the variant; enforcing that
/// distinction end-to-end is the future policy layer's job.
#[derive(Debug, PartialEq)]
pub enum DetectorStatus {
    Found(Vec<Evidence>),
    ToolAbsent,
    Failed(String),
}

/// Shared context passed to every detector's `discover` call.
pub struct DiscoveryContext {
    pub home_dir: PathBuf,
    /// Project roots to check for build/dependency directories (e.g.
    /// `<root>/target`, `<root>/node_modules`). Empty by default —
    /// detectors that need this do nothing when it's empty rather than
    /// guessing at project locations.
    pub known_project_roots: Vec<PathBuf>,
}

impl DiscoveryContext {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            known_project_roots: Vec::new(),
        }
    }

    pub fn with_known_project_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.known_project_roots = roots;
        self
    }
}

/// One detector: a bounded, shallow probe for one family of resources.
pub trait Detector: Send + Sync {
    fn id(&self) -> DetectorId;
    fn resource_kinds(&self) -> &'static [crate::evidence::ResourceKind];

    /// Discover candidates. MUST NOT panic on tool-absent or permission
    /// errors — return [`DetectorStatus::ToolAbsent`] /
    /// [`DetectorStatus::Failed`] instead.
    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus;
}

/// Shallow, non-recursive best-effort size probe: sums the `st_size` of
/// `path`'s immediate directory entries only (no subtree recursion — that
/// full-walk job belongs to [`crate::scanner`], not to detectors). For a
/// directory whose children are themselves directories, this
/// under-counts real content size; it is a deliberately bounded MVP
/// approximation, not a reclaimable-bytes guarantee.
pub(crate) fn shallow_logical_bytes(path: &Path) -> ProbeOutcome<u64> {
    let read_dir = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return ProbeOutcome::Unavailable(ProbeReason::PermissionDenied)
        }
        Err(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    let mut total: u64 = 0;
    for entry in read_dir {
        match entry {
            Ok(e) => {
                if let Ok(meta) = e.metadata() {
                    total += meta.len();
                }
            }
            Err(_) => continue,
        }
    }
    ProbeOutcome::Observed(total)
}

/// Best-effort mtime probe for `path` itself.
pub(crate) fn probe_mtime(path: &Path) -> ProbeOutcome<SystemTime> {
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => ProbeOutcome::Observed(mtime),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied)
        }
        Err(_) => ProbeOutcome::Unavailable(ProbeReason::Failed),
    }
}

/// Best-effort dev/inode fingerprint for `path`.
#[cfg(unix)]
pub(crate) fn dev_ino_fingerprint(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Build a discovery-stage [`Evidence`]: the four correlation fields
/// (`open_by_process`, `process_cwd_match`, `git_state`, `tool_liveness`)
/// are ALWAYS `Unavailable(NotAttempted)` here — HORO-948 never attempts
/// process/git/lsof correlation. HORO-949 is responsible for filling
/// those in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn discovery_evidence(
    resource: ResourceId,
    detector: DetectorId,
    path_for_fingerprint: &Path,
    logical_bytes: ProbeOutcome<u64>,
    last_modified: ProbeOutcome<SystemTime>,
    regenerability: Regenerability,
    recoverability: Recoverability,
    native_cleanup: NativeCleanup,
) -> Evidence {
    Evidence {
        resource,
        fingerprint: ResourceFingerprint {
            dev_ino: dev_ino_fingerprint(path_for_fingerprint),
            mtime: last_modified.observed().copied(),
            tool_revision: None,
        },
        detector,
        logical_bytes,
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        last_modified,
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability,
        recoverability,
        native_cleanup,
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        collected_at: SystemTime::now(),
        sources: Vec::new(),
    }
}

/// Plain compile-time list of the built-in detectors — deliberately NOT a
/// plugin/inventory registration system. Five detectors don't earn that
/// complexity.
pub struct DetectorRegistry {
    detectors: Vec<Box<dyn Detector>>,
}

impl DetectorRegistry {
    /// Registers all five built-in detectors (xcode, homebrew, cargo,
    /// node, docker).
    pub fn builtin() -> Self {
        Self {
            detectors: vec![
                Box::new(xcode::XcodeDetector),
                Box::new(homebrew::HomebrewDetector),
                Box::new(cargo::CargoDetector),
                Box::new(node::NodeDetector),
                Box::new(docker::DockerDetector),
            ],
        }
    }

    pub fn discover_all(&self, ctx: &DiscoveryContext) -> Vec<(DetectorId, DetectorStatus)> {
        self.detectors
            .iter()
            .map(|detector| (detector.id(), detector.discover(ctx)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_registers_all_five_detectors() {
        let registry = DetectorRegistry::builtin();
        assert_eq!(registry.detectors.len(), 5);
    }

    #[test]
    fn discover_all_returns_one_status_per_detector() {
        let registry = DetectorRegistry::builtin();
        let ctx = DiscoveryContext::new("/nonexistent-home-for-test");
        let results = registry.discover_all(&ctx);
        assert_eq!(results.len(), 5);
    }
}
