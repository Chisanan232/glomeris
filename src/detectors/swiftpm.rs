//! Swift Package Manager detectors (HORO-1543): the shared cache, and
//! per-project `.build` directories.
//!
//! Two detectors because the two resources are found in completely
//! different ways and answer differently when absent. The cache is one
//! machine-global directory; a `.build` directory belongs to one project
//! and is only ever looked for under a known project root.
//!
//! ## What is deliberately NOT named
//!
//! SwiftPM keeps three things under similar-looking paths, and only one of
//! them is a cache:
//!
//! - `~/Library/Caches/org.swift.swiftpm` — the cache. Manifest build
//!   results and cloned package repositories that SwiftPM refetches.
//! - `~/Library/org.swift.swiftpm` — *security and configuration*: trusted
//!   root certificates, registry configuration, package collections. Not a
//!   cache in any sense, and never named here.
//! - `~/.swiftpm` — user configuration, including mirrors. Also never named.
//!
//! The one path this module contains is asserted by a test to live under
//! `Library/Caches`, so a future edit cannot quietly promote one of the
//! other two into a reclaimable resource.
//!
//! That shared parent is also why a missing cache is
//! [`RootAbsence::InferredUnderSharedParent`] and not a `tool_absent` claim:
//! `~/Library/Caches` belongs to everything, so its state is no evidence
//! about SwiftPM (HORO-1575).
//!
//! ## Ownership of a `.build` directory
//!
//! `.build` is an unremarkable directory name that other tools also use, so
//! its presence alone does not establish that SwiftPM owns it. This
//! detector reports one only where the project root also contains a
//! `Package.swift` — the manifest that makes the directory SwiftPM's build
//! output. Without that, the directory is skipped rather than guessed at.
//!
//! A root skipped for want of a manifest was still *examined*, which is why
//! the HORO-1576 rule is about the root list and not about the evidence
//! count: no roots at all is [`DetectorStatus::NotConfigured`], while roots
//! that turned out to hold no Swift package are an empty `Found`. Neither is
//! a claim that Swift is missing — on a Mac with the command-line tools it
//! never is, and this detector would have no way to tell.

use std::path::PathBuf;

use crate::evidence::{
    NativeCleanup, Recoverability, Regenerability, ResourceId, ResourceKind, ResourceLocator,
};

use super::{
    cache_root_status, discovery_evidence, estimate_logical_bytes, probe_mtime,
    size_estimate_budget, Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence,
};

pub struct SwiftPmCacheDetector;
pub struct SwiftPmBuildDirDetector;

const CACHE_KINDS: &[ResourceKind] = &[ResourceKind::SwiftPackageManagerCache];
const BUILD_KINDS: &[ResourceKind] = &[ResourceKind::SwiftPackageManagerBuildDir];

/// The cache directory, relative to `$HOME`. Under `Library/Caches` — see
/// the module doc for the two sibling directories that are not caches, and
/// `the_cache_path_is_under_library_caches` for the assertion that keeps
/// this one honest.
const CACHE_RELATIVE_PATH: &str = "Library/Caches/org.swift.swiftpm";

/// The manifest whose presence establishes that SwiftPM owns a `.build`.
const MANIFEST_FILE: &str = "Package.swift";

/// SwiftPM's build output directory name.
const BUILD_DIR: &str = ".build";

impl Detector for SwiftPmCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("swiftpm_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        CACHE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        cache_root_status(
            self.id(),
            ResourceKind::SwiftPackageManagerCache,
            &ctx.home_dir.join(CACHE_RELATIVE_PATH),
            // Manifest build results and cloned package repositories:
            // SwiftPM refetches and rebuilds these itself on the next
            // resolve.
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "SwiftPM's shared cache directory (~/Library/Caches/org.swift.swiftpm)",
            // The parent here is `~/Library/Caches`, which holds entries for
            // most of the software on a Mac and so says nothing whatever
            // about SwiftPM. There is therefore no observation on which to
            // base a `tool_absent` claim, and a missing cache reports that
            // nothing was found rather than that Swift is not installed —
            // which on a Mac with the command-line tools it always is
            // (HORO-1575).
            RootAbsence::InferredUnderSharedParent,
        )
    }
}

impl Detector for SwiftPmBuildDirDetector {
    fn id(&self) -> DetectorId {
        DetectorId("swiftpm_build_dir")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        BUILD_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        // Before any read, and deliberately a test of the root list rather
        // than of the evidence: a root skipped for want of a manifest was
        // still examined (HORO-1576).
        if ctx.known_project_roots.is_empty() {
            return DetectorStatus::NotConfigured;
        }

        let mut evidence = Vec::new();
        let mut saw_permission_error = false;

        for root in &ctx.known_project_roots {
            // Ownership first: no manifest, no claim. Checked before the
            // build directory is even canonicalized, so a `.build` beside
            // something that is not a Swift package is never named at all.
            if !root.join(MANIFEST_FILE).is_file() {
                continue;
            }

            let build_dir: PathBuf = root.join(BUILD_DIR);
            match build_dir.canonicalize() {
                Ok(canonical) => {
                    let resource = ResourceId::new(
                        ResourceKind::SwiftPackageManagerBuildDir,
                        ResourceLocator::Path(canonical.clone()),
                    )
                    // The same reasoning as the Cargo detector's HORO-1017
                    // change: a later action must be able to find the
                    // manifest through the root it was discovered under,
                    // not by re-deriving it from a canonicalized (possibly
                    // symlink-resolved) build path.
                    .with_source_project_root(root.clone());
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
                    ev.push_source(format!(
                        "SwiftPM build directory beside a {MANIFEST_FILE} manifest"
                    ));
                    evidence.push(ev);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    saw_permission_error = true;
                }
                Err(_) => continue,
            }
        }

        if evidence.is_empty() && saw_permission_error {
            return DetectorStatus::Failed(
                "permission denied probing one or more known project roots for a \
                 SwiftPM build directory"
                    .to_string(),
            );
        }

        // Every configured root was examined; none held a SwiftPM build
        // directory. The search ran, so this is an empty result — the same
        // answer `SwiftPmCacheDetector` above already gives, for the same
        // reason (HORO-1575/1576).
        DetectorStatus::Found(evidence)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::evidence::ProbeOutcome;

    fn temp_dir(prefix: &str) -> PathBuf {
        crate::detectors::test_support::make_temp_dir(prefix)
    }

    #[test]
    fn ids_and_kinds_are_stable() {
        assert_eq!(SwiftPmCacheDetector.id(), DetectorId("swiftpm_cache"));
        assert_eq!(
            SwiftPmCacheDetector.resource_kinds(),
            &[ResourceKind::SwiftPackageManagerCache]
        );
        assert_eq!(
            SwiftPmBuildDirDetector.id(),
            DetectorId("swiftpm_build_dir")
        );
        assert_eq!(
            SwiftPmBuildDirDetector.resource_kinds(),
            &[ResourceKind::SwiftPackageManagerBuildDir]
        );
    }

    /// The security/configuration directories `~/Library/org.swift.swiftpm`
    /// and `~/.swiftpm` must stay out of reach. Pinning the cache path's
    /// shape is what makes that structural instead of a comment: neither of
    /// those two is under `Library/Caches`.
    #[test]
    fn the_cache_path_is_under_library_caches() {
        let path = Path::new(CACHE_RELATIVE_PATH);
        assert!(path.starts_with("Library/Caches"));
        assert_ne!(path, Path::new("Library/org.swift.swiftpm"));
        assert_ne!(path, Path::new(".swiftpm"));
    }

    #[test]
    fn a_populated_cache_is_measured() {
        let home = temp_dir("swiftpm-home");
        let cache = home.join(CACHE_RELATIVE_PATH);
        std::fs::create_dir_all(cache.join("manifest")).unwrap();
        std::fs::write(cache.join("manifest/entry"), vec![0u8; 3_072]).unwrap();

        match SwiftPmCacheDetector.discover(&DiscoveryContext::new(&home)) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].logical_bytes, ProbeOutcome::Observed(3_072));
                assert_eq!(
                    evidence[0].resource.kind,
                    ResourceKind::SwiftPackageManagerCache
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// SwiftPM's cache sits under `~/Library/Caches`, shared with most of
    /// the software on the machine, so neither its presence nor its absence
    /// is evidence about SwiftPM. A missing cache therefore reports that
    /// nothing was found — not that Swift is absent, which on a Mac with the
    /// command-line tools it is not (HORO-1575).
    ///
    /// Both halves here are the shared-parent rule rather than a pair: with
    /// `~/Library/Caches` present and with it absent, the answer is the same,
    /// which is the whole point. A detector that judged the parent would give
    /// two different answers and fail the second half.
    #[test]
    fn a_missing_cache_reports_nothing_found_rather_than_a_missing_swift() {
        for (label, create_caches_parent) in [("no-caches-parent", false), ("caches-parent", true)]
        {
            let home = temp_dir(&format!("swiftpm-home-{label}"));
            if create_caches_parent {
                std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
            }

            let status = SwiftPmCacheDetector.discover(&DiscoveryContext::new(&home));
            std::fs::remove_dir_all(&home).ok();

            match status {
                DetectorStatus::Found(evidence) => assert!(
                    evidence.is_empty(),
                    "{label}: a missing cache must not become a zero-byte resource"
                ),
                DetectorStatus::ToolAbsent => panic!(
                    "{label}: `~/Library/Caches` says nothing about SwiftPM, so there is \
                     no observation on which to claim Swift is not installed"
                ),
                other => panic!("{label}: expected Found(empty), got {other:?}"),
            }
        }
    }

    /// The ownership rule, as a positive control: the same `.build`
    /// directory is reported with a manifest beside it and not without one.
    ///
    /// Mutation control: deleting the `Package.swift` check in `discover`
    /// makes the second half of this fail with `Found`, because the
    /// directory itself is identical in both halves.
    #[test]
    fn a_build_dir_is_only_swiftpms_when_a_manifest_is_beside_it() {
        let root = temp_dir("swiftpm-build-ownership");
        std::fs::create_dir_all(root.join(".build/debug")).unwrap();
        std::fs::write(root.join(".build/debug/artifact.o"), vec![0u8; 5_120]).unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);

        // No manifest yet: the directory exists, and is still not claimed. The
        // root WAS examined, so the answer is an empty search result — not
        // `NotConfigured`, which would say nothing was looked at, and not
        // `ToolAbsent`, which would claim Swift is missing (HORO-1576).
        match SwiftPmBuildDirDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => assert!(
                evidence.is_empty(),
                "a .build directory alone does not establish that SwiftPM owns it"
            ),
            other => panic!("expected Found(empty) for an examined root, got {other:?}"),
        }

        std::fs::write(root.join(MANIFEST_FILE), b"// swift-tools-version:5.9\n").unwrap();

        match SwiftPmBuildDirDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(ev.resource.kind, ResourceKind::SwiftPackageManagerBuildDir);
                assert_eq!(ev.logical_bytes, ProbeOutcome::Observed(5_120));
                assert_eq!(
                    ev.resource.source_project_root.as_deref(),
                    Some(root.as_path()),
                    "the discovered root must be carried, not re-derived later"
                );
            }
            other => panic!("expected Found once a manifest is present, got {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// Mutation control: restoring `ToolAbsent` fails here. A Mac with the
    /// command-line tools always has Swift, and this detector never asks
    /// (HORO-1576).
    #[test]
    fn no_known_project_roots_examined_nothing() {
        let ctx = DiscoveryContext::new("/tmp");
        assert_eq!(
            SwiftPmBuildDirDetector.discover(&ctx),
            DetectorStatus::NotConfigured
        );
    }

    /// A manifest with no `.build` yet is not a zero-byte resource — and, as
    /// the discriminating half of the pair above, not "nothing was examined"
    /// either: this root was read, and it is a Swift package.
    #[test]
    fn a_manifest_without_a_build_dir_reports_nothing() {
        let root = temp_dir("swiftpm-manifest-only");
        std::fs::write(root.join(MANIFEST_FILE), b"// swift-tools-version:5.9\n").unwrap();

        let ctx = DiscoveryContext::new("/tmp").with_known_project_roots(vec![root.clone()]);
        let status = SwiftPmBuildDirDetector.discover(&ctx);
        std::fs::remove_dir_all(&root).ok();

        match status {
            DetectorStatus::Found(evidence) => assert!(evidence.is_empty()),
            DetectorStatus::NotConfigured => {
                panic!("a root WAS configured and examined, so this search did happen")
            }
            other => panic!("expected Found(empty), got {other:?}"),
        }
    }
}
