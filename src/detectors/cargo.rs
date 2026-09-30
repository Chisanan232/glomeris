//! Cargo `target/` directory detector.
//!
//! MVP scope: this detector does not discover arbitrary project roots on
//! disk (that would require a full-disk walk, which is the scanner's job,
//! not a detector's). It only checks each root in
//! [`DiscoveryContext::known_project_roots`] for a `target/` directory —
//! callers (CLI wiring, future config) are responsible for supplying that
//! list. With an empty list, this detector reports [`DetectorStatus::ToolAbsent`].
//!
//! `reclaimable_bytes` reuses the same [`estimate_logical_bytes`] estimate
//! as `logical_bytes` (HORO-992): a `target/` directory is fully owned,
//! regenerable build output with no partial-retention concept, so
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
    size_estimate_budget, Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence,
    ToolHomeVar,
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

/// Detector for the shared Cargo registry cache (HORO-1543).
///
/// The resource is `<cargo home>/registry`, and deliberately never the Cargo
/// home itself. `$CARGO_HOME` also holds `credentials.toml` — registry
/// publish tokens — and `bin/`, the binaries `cargo install` put on the
/// user's `PATH`. Naming the parent would put both inside a reclaimable
/// resource, and no later policy class could make that safe again.
///
/// `<cargo home>/git` is also deliberately excluded. It holds checkouts of
/// git dependencies, which can pin a commit that has since been force-pushed
/// away — the same unprovable-reproducibility problem as Maven's local
/// repository, and not something this ticket's evidence can settle. It is
/// simply not claimed rather than claimed with a guess.
pub struct CargoRegistryCacheDetector;

const REGISTRY_KINDS: &[ResourceKind] = &[ResourceKind::CargoRegistryCache];

/// The one subdirectory of the Cargo home this detector will ever name. See
/// [`CargoRegistryCacheDetector`] for the two it must not.
const REGISTRY_SUBDIR: &str = "registry";

/// Resolves the registry cache root from `CARGO_HOME` and `$HOME`, applying
/// Cargo's own two rules.
///
/// Pure so it is testable: the `CARGO_HOME` override arrives through
/// [`DiscoveryContext::tool_home`] rather than from the process environment,
/// so a fixture context can exercise both rules. Reading it here directly
/// would make this detector answer from the developer's real registry even
/// under a fixture `home_dir` — see [`super::ToolHomeVar`], which exists
/// because that is exactly what happened.
///
/// An empty or whitespace-only `CARGO_HOME` falls back to `~/.cargo` rather
/// than resolving to a relative `registry` or to `/registry` — an exported
/// but unset variable is a common shell accident, and treating it as an
/// answer would name a directory belonging to something else.
fn cargo_registry_dir(cargo_home: Option<&str>, home: &Path) -> PathBuf {
    let cargo_home = match cargo_home.map(str::trim) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".cargo"),
    };
    cargo_home.join(REGISTRY_SUBDIR)
}

impl Detector for CargoRegistryCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("cargo_registry_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        REGISTRY_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let registry = cargo_registry_dir(ctx.tool_home(ToolHomeVar::CargoHome), &ctx.home_dir);
        // `registry` is always `<cargo home>/registry`, so the parent is the
        // Cargo home — a directory only Cargo (or rustup, installing it)
        // creates.
        let cargo_home = registry
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| registry.clone());

        cache_root_status(
            self.id(),
            ResourceKind::CargoRegistryCache,
            &registry,
            // Downloaded `.crate` files and their extracted sources: cargo
            // refetches these on the next build, given network access and,
            // for a private registry, credentials — the same caveat the Go
            // module cache carries.
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "Cargo registry cache under the Cargo home \
             (CARGO_HOME if set, otherwise ~/.cargo)",
            // A present Cargo home with no `registry` means cargo is
            // installed and has not fetched a dependency yet — not that cargo
            // is missing (HORO-1575).
            RootAbsence::InferredUnderToolOwnedParent(cargo_home),
        )
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

    #[test]
    fn registry_detector_id_and_kinds_are_stable() {
        assert_eq!(
            CargoRegistryCacheDetector.id(),
            DetectorId("cargo_registry_cache")
        );
        assert_eq!(
            CargoRegistryCacheDetector.resource_kinds(),
            &[ResourceKind::CargoRegistryCache]
        );
    }

    #[test]
    fn cargo_home_wins_over_the_default() {
        assert_eq!(
            cargo_registry_dir(Some("/opt/cargo"), Path::new("/Users/dev")),
            PathBuf::from("/opt/cargo/registry")
        );
        assert_eq!(
            cargo_registry_dir(None, Path::new("/Users/dev")),
            PathBuf::from("/Users/dev/.cargo/registry")
        );
    }

    /// An exported-but-empty `CARGO_HOME` must not become an answer: `""`
    /// would otherwise resolve to `registry` (relative) or `/registry`.
    #[test]
    fn an_empty_cargo_home_falls_back_rather_than_naming_a_stray_path() {
        for value in ["", "   ", "\t"] {
            assert_eq!(
                cargo_registry_dir(Some(value), Path::new("/Users/dev")),
                PathBuf::from("/Users/dev/.cargo/registry"),
                "CARGO_HOME={value:?} should have fallen back"
            );
        }
    }

    /// The credential/binary-safety invariant, asserted rather than
    /// commented: the resolved root is never the Cargo home, under either
    /// rule.
    #[test]
    fn the_cargo_home_itself_is_never_the_resource() {
        let from_env = cargo_registry_dir(Some("/opt/cargo"), Path::new("/Users/dev"));
        assert_ne!(from_env, PathBuf::from("/opt/cargo"));
        assert!(from_env.ends_with(REGISTRY_SUBDIR));

        let from_home = cargo_registry_dir(None, Path::new("/Users/dev"));
        assert_ne!(from_home, PathBuf::from("/Users/dev/.cargo"));
        assert!(from_home.ends_with(REGISTRY_SUBDIR));
    }

    /// Positive control for the same invariant end to end: a Cargo home
    /// holding `credentials.toml`, an installed binary, a git-dependency
    /// checkout and `registry/` yields evidence measuring only `registry/`.
    /// If the detector ever named the parent, the observed byte count would
    /// include the publish token and this assertion would fail.
    #[test]
    fn credentials_binaries_and_git_checkouts_are_not_part_of_the_resource() {
        let home = make_temp_dir("cargo-home-fixture");
        let cargo_home = home.join(".cargo");
        fs::create_dir_all(cargo_home.join("registry/cache/index.crates.io-1234")).unwrap();
        fs::create_dir_all(cargo_home.join("bin")).unwrap();
        fs::create_dir_all(cargo_home.join("git/db/some-dep-5678")).unwrap();
        fs::write(cargo_home.join("credentials.toml"), vec![b'x'; 200]).unwrap();
        fs::write(cargo_home.join("bin/some-tool"), vec![0u8; 100_000]).unwrap();
        fs::write(
            cargo_home.join("git/db/some-dep-5678/packed-refs"),
            vec![0u8; 700],
        )
        .unwrap();
        fs::write(
            cargo_home.join("registry/cache/index.crates.io-1234/serde-1.0.0.crate"),
            vec![0u8; 8_192],
        )
        .unwrap();

        // The real `discover`, through a hermetic `DiscoveryContext`: with no
        // `CargoHome` tool home set, the detector resolves from `home_dir`
        // and cannot see this machine's own registry (HORO-1543).
        match CargoRegistryCacheDetector.discover(&DiscoveryContext::new(&home)) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(8_192),
                    "only the registry subtree may be measured"
                );
                match &ev.resource.locator {
                    ResourceLocator::Path(named) => {
                        assert!(named.ends_with(REGISTRY_SUBDIR));
                        assert!(!named.ends_with(".cargo"));
                    }
                    other => panic!("expected a path locator, got {other:?}"),
                }
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&home).ok();
    }

    /// No Cargo home at all reports nothing, never a zero-byte resource.
    #[test]
    fn no_cargo_home_is_tool_absent_not_zero_bytes() {
        let home = make_temp_dir("cargo-home-empty");
        assert_eq!(
            CargoRegistryCacheDetector.discover(&DiscoveryContext::new(&home)),
            DetectorStatus::ToolAbsent
        );
        fs::remove_dir_all(&home).ok();
    }

    /// The discriminating pair to the test above: same missing `registry`,
    /// but `~/.cargo` is there. Cargo is installed, so `tool_absent` would be
    /// false (HORO-1575). The fixtures differ in exactly one thing — whether
    /// `.cargo` exists.
    #[test]
    fn a_cargo_home_with_no_registry_yet_is_not_a_missing_cargo() {
        let home = make_temp_dir("cargo-home-no-registry");
        fs::create_dir_all(home.join(".cargo")).unwrap();
        // `bin/` is what rustup writes when it installs the toolchain, so a
        // Cargo home in this shape is the normal pre-first-build state.
        fs::create_dir_all(home.join(".cargo/bin")).unwrap();

        let status = CargoRegistryCacheDetector.discover(&DiscoveryContext::new(&home));
        fs::remove_dir_all(&home).ok();

        match status {
            DetectorStatus::Found(evidence) => assert!(
                evidence.is_empty(),
                "an unfetched registry must not become a zero-byte resource"
            ),
            DetectorStatus::ToolAbsent => {
                panic!("`~/.cargo` exists, so cargo is installed; tool_absent is false")
            }
            other => panic!("expected Found(empty), got {other:?}"),
        }
    }

    /// The `CargoHome` override has to reach `discover`, not merely
    /// `cargo_registry_dir`: the pure test above would still pass if
    /// `discover` ignored `ctx.tool_home` and always resolved from
    /// `home_dir`. So the fixture registry lives somewhere `home_dir` cannot
    /// reach, and `home_dir` points at a decoy that holds a *different* number
    /// of bytes — a detector reading the wrong one reports 1_024 and fails.
    #[test]
    fn the_cargo_home_override_reaches_the_detector() {
        let relocated = make_temp_dir("cargo-relocated");
        fs::create_dir_all(relocated.join("registry/cache")).unwrap();
        fs::write(
            relocated.join("registry/cache/serde.crate"),
            vec![0u8; 2_048],
        )
        .unwrap();

        let decoy_home = make_temp_dir("cargo-decoy-home");
        fs::create_dir_all(decoy_home.join(".cargo/registry")).unwrap();
        fs::write(
            decoy_home.join(".cargo/registry/decoy.crate"),
            vec![0u8; 1_024],
        )
        .unwrap();

        let ctx = DiscoveryContext::new(&decoy_home)
            .with_tool_home(ToolHomeVar::CargoHome, relocated.to_str().unwrap());

        match CargoRegistryCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(2_048),
                    "the relocated registry is the one CARGO_HOME names; \
                     1024 would mean the override never reached `discover`"
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        fs::remove_dir_all(&relocated).ok();
        fs::remove_dir_all(&decoy_home).ok();
    }
}
