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

use std::path::{Path, PathBuf};

use crate::evidence::{
    NativeCleanup, Recoverability, Regenerability, ResourceId, ResourceKind, ResourceLocator,
};

use super::{
    cache_root_status, discovery_evidence, estimate_logical_bytes, probe_mtime,
    query_tool_single_line, size_estimate_budget, Detector, DetectorId, DetectorStatus,
    DiscoveryContext, RootAbsence, ToolQuery,
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
                        estimate.is_lower_bound(),
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

/// Detector for npm's content-addressable package cache (HORO-1543).
///
/// The location comes from `npm config get cache`, not from `~/.npm`: npm
/// resolves it through `.npmrc` files, `npm_config_cache` and
/// `$XDG_CACHE_HOME`, and a path guessed from `$HOME` would be wrong for
/// anyone who has moved it.
///
/// The resource is `<cache>/_cacache` rather than the cache directory npm
/// names. That directory also holds `_logs` — npm's debug logs, which are
/// diagnostic history rather than a cache — and
/// `_update-notifier-last-checked`. `_cacache` is what `npm cache clean`
/// itself operates on.
///
/// ## Why pnpm and yarn are not covered here
///
/// [`ResourceKind::NodePackageManagerCache`]'s
/// [`owning_tool`](ResourceKind::owning_tool) is statically
/// [`OwningTool::Npm`](crate::evidence::OwningTool::Npm). Reporting a pnpm
/// store or a yarn cache under this kind would therefore attribute it to npm
/// — a false statement about who owns a directory, and one that would flow
/// straight into the tool-liveness reasoning and the GUI.
///
/// Covering them honestly needs either their own `ResourceKind` variants or a
/// per-instance owning tool on `Evidence`, both of which are changes to the
/// shared evidence model rather than to a detector. That is deliberately
/// deferred rather than approximated: pnpm's store is also structurally
/// different (content-addressed and *hard-linked into* every
/// `node_modules` that references it), so deleting it is not the same
/// operation as deleting a download cache, and it needs its own reasoning
/// about reclaimable bytes before anything claims a figure for it.
pub struct NodePackageManagerCacheDetector;

const CACHE_KINDS: &[ResourceKind] = &[ResourceKind::NodePackageManagerCache];

/// The one subdirectory of npm's cache directory this detector will name.
/// See [`NodePackageManagerCacheDetector`] for the siblings it must not.
const CACACHE_SUBDIR: &str = "_cacache";

/// Asking npm where its cache is, without npm writing a log about it
/// (HORO-1556).
///
/// `npm` writes `<cache>/_logs/<timestamp>-debug-0.log` on every invocation,
/// including one that only reads a config value. That put a new file inside the
/// directory this detector measures each time `glomeris detect` ran — growing
/// the resource being reported on, and writing to the user's disk from a
/// command whose help says "Read-only — changes nothing." `--logs-max=0` is
/// npm's own documented control for retaining no log files, and it applies to
/// the invocation that would create one.
const NPM_CACHE_QUERY: &[&str] = &["config", "get", "cache", "--logs-max=0"];

/// Narrows the directory `npm config get cache` reports to the cache proper.
///
/// A separate function so the boundary is testable: a test that joined
/// `_cacache` itself would be asserting a property of
/// [`estimate_logical_bytes`], not of this detector's choice of resource.
fn npm_cacache_dir(reported_cache_dir: &str) -> PathBuf {
    Path::new(reported_cache_dir.trim()).join(CACACHE_SUBDIR)
}

impl Detector for NodePackageManagerCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("npm_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        CACHE_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
        match query_tool_single_line("npm", NPM_CACHE_QUERY) {
            ToolQuery::Lines(lines) => cache_root_status(
                self.id(),
                ResourceKind::NodePackageManagerCache,
                &npm_cacache_dir(&lines[0]),
                // Downloaded package tarballs and their metadata, which npm
                // refetches on the next install given network access and,
                // for a private registry, credentials.
                Regenerability::RegenerableByTool,
                Recoverability::RegenerableByTool,
                "npm's package cache, under the directory reported by \
                 `npm config get cache`",
                RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
            ),
            ToolQuery::ToolAbsent => DetectorStatus::ToolAbsent,
            ToolQuery::Failed(msg) => DetectorStatus::Failed(msg),
        }
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

    #[test]
    fn npm_cache_detector_id_and_kinds_are_stable() {
        assert_eq!(
            NodePackageManagerCacheDetector.id(),
            DetectorId("npm_cache")
        );
        assert_eq!(
            NodePackageManagerCacheDetector.resource_kinds(),
            &[ResourceKind::NodePackageManagerCache]
        );
    }

    /// Positive control for the `_cacache` boundary: an npm cache directory
    /// holding both `_logs` and `_cacache` yields evidence measuring only the
    /// latter. If the detector ever named the directory npm reports, the
    /// observed byte count would include the debug logs and this would fail.
    #[test]
    fn npm_debug_logs_are_not_part_of_the_cache_resource() {
        // `cache_root_status` with the path the detector would build, rather
        // than `discover`: `discover` runs the real `npm`, which answers with
        // this machine's own cache.
        let npm_cache = make_temp_dir("npm-cache-fixture");
        fs::create_dir_all(npm_cache.join("_cacache/content-v2")).unwrap();
        fs::create_dir_all(npm_cache.join("_logs")).unwrap();
        fs::write(
            npm_cache.join("_logs/2026-01-01T00_00_00_000Z-debug.log"),
            vec![b'x'; 3_000],
        )
        .unwrap();
        fs::write(npm_cache.join("_cacache/content-v2/blob"), vec![0u8; 6_144]).unwrap();

        let status = cache_root_status(
            NodePackageManagerCacheDetector.id(),
            ResourceKind::NodePackageManagerCache,
            &npm_cacache_dir(npm_cache.to_str().unwrap()),
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "test fixture",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        );

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(6_144),
                    "only the _cacache subtree may be measured"
                );
                match &ev.resource.locator {
                    ResourceLocator::Path(named) => assert!(named.ends_with(CACACHE_SUBDIR)),
                    other => panic!("expected a path locator, got {other:?}"),
                }
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&npm_cache).ok();
    }

    /// An npm that answers with a cache directory it has not populated yet is
    /// installed: that is `Found(vec![])`, never `ToolAbsent`, and never a
    /// zero-byte resource.
    #[test]
    fn an_unpopulated_npm_cache_is_no_resource_and_not_tool_absent() {
        let npm_cache = make_temp_dir("npm-cache-empty");
        let status = cache_root_status(
            NodePackageManagerCacheDetector.id(),
            ResourceKind::NodePackageManagerCache,
            &npm_cacache_dir(npm_cache.to_str().unwrap()),
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "test fixture",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        );
        assert_eq!(status, DetectorStatus::Found(Vec::new()));

        fs::remove_dir_all(&npm_cache).ok();
    }
}
