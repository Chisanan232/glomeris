//! Go build-cache and module-cache detectors (HORO-1543).
//!
//! Two detectors rather than one, even though a single `go env GOCACHE
//! GOMODCACHE` would answer both in one spawn: [`DetectorStatus`] is
//! per-detector, so a detector covering two roots would have to choose
//! between reporting `Failed` (discarding the root that *did* answer) and
//! reporting `Found` (letting the root whose probe failed appear as
//! "nothing found", which HORO-1543 AC 3 forbids). Splitting them keeps
//! each root's outcome exact at the cost of one extra `go env` call.
//!
//! Neither location is a hardcoded `~/Library/Caches/go-build` or
//! `~/go/pkg/mod` guess: both move with `GOCACHE`, `GOMODCACHE`, `GOPATH` and
//! `GOENV`, and a path assembled from `$HOME` alone would be wrong for anyone
//! using a non-default `GOPATH`.
//!
//! What is not a guess is go's own documented configuration, and consulting
//! that costs no subprocess (HORO-1560 AC 2). Each detector reads the
//! variables go documents, falls back to the location go documents as the
//! default, and asks `go env` only when neither names a directory that is
//! there — see [`super::documented_cache_root`] for why an absent directory is
//! not an answer, and for the limitation both routes share: a `GOENV` file can
//! set these variables where no environment variable is visible, which is
//! exactly the case the fallback spawn still covers.
//!
//! The two roots hold different things and are classified differently. The
//! build cache holds compiled artifacts, so it comes back only by
//! recompiling ([`Regenerability::RegenerableByRebuild`], matching the
//! existing Cargo/Xcode detectors). The module cache holds downloaded
//! module zips and their extracted trees, which `go mod download` refetches
//! ([`Regenerability::RegenerableByTool`]) — given network access and, for
//! a private module, credentials.

use std::path::{Path, PathBuf};

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, documented_cache_root, query_tool_single_path, user_caches_dir, CacheRoute,
    Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence, ToolEnvVar, ToolQuery,
    DOCUMENTED_ROUTE_ABSENCE,
};

pub struct GoBuildCacheDetector;
pub struct GoModuleCacheDetector;

const BUILD_KINDS: &[ResourceKind] = &[ResourceKind::GoBuildCache];
const MODULE_KINDS: &[ResourceKind] = &[ResourceKind::GoModuleCache];

/// `go env GOCACHE`'s answer when the user has disabled the build cache
/// outright. Not a path, and deliberately not reported as a failed probe:
/// the probe worked perfectly and its answer is "there is no build cache".
const GOCACHE_DISABLED: &str = "off";

/// The program these detectors run. Named here rather than inline so
/// [`super::SPAWNED_PROGRAMS`] can be built from the detectors' own
/// declarations instead of a second list that could drift from them.
pub(super) const GO_PROGRAM: &str = "go";

/// What one line of `go env` output means.
#[derive(Debug, PartialEq)]
enum GoEnvAnswer<'a> {
    /// The user set the variable to `off`. A real answer, not a path.
    CacheDisabled,
    Root(&'a str),
}

/// Pure interpretation of `go env <var>`'s single line, separated from the
/// spawn so the `off` case is testable on a machine where Go is installed
/// normally — a test that re-derived this branch instead of calling it
/// would prove nothing about `discover`.
fn interpret_go_env_answer(answer: &str) -> GoEnvAnswer<'_> {
    if answer == GOCACHE_DISABLED {
        GoEnvAnswer::CacheDisabled
    } else {
        GoEnvAnswer::Root(answer)
    }
}

/// go's documented default build-cache directory, inside the platform's own
/// cache directory. Go resolves this through its equivalent of
/// [`user_caches_dir`], which on macOS is `~/Library/Caches` and — unlike
/// `uv`'s — does *not* consult `XDG_CACHE_HOME`. Checked against the installed
/// toolchain rather than carried over from go's behaviour on Linux, where that
/// variable does move it.
const BUILD_CACHE_SUBDIR: &str = "go-build";

/// go's documented default `GOPATH`, relative to the user's home.
const GOPATH_DEFAULT_SUBDIR: &str = "go";

/// Where the module cache sits inside a `GOPATH` entry.
const MODULE_CACHE_SUBDIR: &str = "pkg/mod";

/// Where go's own documented configuration puts the build cache, when that
/// answers without running `go`. `GOCACHE` first, then the documented default.
///
/// `GOCACHE=off` is deliberately *not* handled here: it is not a location, so
/// it is not this function's answer to give. [`GoBuildCacheDetector::discover`]
/// reads it before asking about locations at all.
fn documented_build_cache_root(ctx: &DiscoveryContext) -> Option<(PathBuf, CacheRoute)> {
    documented_cache_root(
        ctx,
        ToolEnvVar::GoCache,
        user_caches_dir(&ctx.home_dir).map(|caches| caches.join(BUILD_CACHE_SUBDIR)),
    )
}

/// go's documented default module cache: `pkg/mod` inside the **first**
/// `GOPATH` entry, or inside `~/go` when `GOPATH` is unset.
///
/// The first entry, not the whole list: `GOPATH` is an OS-separated list and go
/// documents the module cache as living in its first element, which was
/// confirmed against the installed toolchain (`GOPATH=/a:/b` reports
/// `/a/pkg/mod`). Taking the string whole would have produced one nonsensical
/// path containing a separator.
///
/// `None` for a relative first entry. That is not a path to resolve against
/// the scan's working directory and not a path to hand to go either — go
/// refuses a relative `GOPATH` entry outright — so the caller falls through to
/// the spawn, where go reports the refusal as the failed probe it is.
fn default_module_cache_dir(ctx: &DiscoveryContext) -> Option<PathBuf> {
    // `GOPATH` has a single documented spelling, so the ambiguous case
    // `tool_env` also answers `None` for cannot arise here.
    let gopath = match ctx.tool_env(ToolEnvVar::GoPath) {
        // `split_paths` reads nothing from the environment; it applies the
        // platform's list-separator rule, which is the rule `GOPATH` uses.
        Some(value) => {
            let first = std::env::split_paths(value).next()?;
            if !first.is_absolute() {
                return None;
            }
            first
        }
        None => ctx.home_dir.join(GOPATH_DEFAULT_SUBDIR),
    };
    Some(gopath.join(MODULE_CACHE_SUBDIR))
}

/// Where go's own documented configuration puts the module cache, when that
/// answers without running `go`. `GOMODCACHE` first, then the `GOPATH` chain in
/// [`default_module_cache_dir`].
fn documented_module_cache_root(ctx: &DiscoveryContext) -> Option<(PathBuf, CacheRoute)> {
    documented_cache_root(ctx, ToolEnvVar::GoModCache, default_module_cache_dir(ctx))
}

/// Asks `go env <var>` for one absolute cache root and builds its evidence.
///
/// Why go has to be asked (HORO-1560 AC 3): `go env -w` persists `GOCACHE`,
/// `GOMODCACHE` and `GOPATH` into go's own env file
/// (`os.UserConfigDir()/go/env`), where no environment variable is visible. A
/// machine configured that way has a cache the documented route cannot find,
/// and go is the only thing that can read that file's precedence correctly.
///
/// What go does when asked: `go env <var>` prints the resolved value on one
/// line. It reads; only `go env -w`/`-u` write, and neither appears here.
fn go_env_cache_root(
    detector: DetectorId,
    kind: ResourceKind,
    var: &str,
    regenerability: Regenerability,
    recoverability: Recoverability,
) -> DetectorStatus {
    match query_tool_single_path(GO_PROGRAM, &["env", var]) {
        ToolQuery::Lines(lines) => match interpret_go_env_answer(&lines[0]) {
            GoEnvAnswer::CacheDisabled => DetectorStatus::Found(Vec::new()),
            GoEnvAnswer::Root(root) => cache_root_status(
                detector,
                kind,
                Path::new(root),
                regenerability,
                recoverability,
                &format!("path reported by `go env {var}`"),
                RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
            ),
        },
        ToolQuery::ToolAbsent => DetectorStatus::ToolAbsent,
        // A `go env` that had to be abandoned tells us nothing about whether
        // the cache exists, so it must not reach the report as `ToolAbsent`
        // or as a zero-byte resource (HORO-1559).
        ToolQuery::Failed(msg) | ToolQuery::TimedOut(msg) => DetectorStatus::Failed(msg),
    }
}

impl Detector for GoBuildCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("go_build_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        BUILD_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        // `GOCACHE=off` is the same answer `go env GOCACHE` would print, and
        // reading it here spares the spawn entirely. Interpreted by the one
        // function that interprets that word, so the environment route and the
        // `go env` route cannot come to disagree about what `off` means.
        if let Some(value) = ctx.tool_env(ToolEnvVar::GoCache) {
            if interpret_go_env_answer(value) == GoEnvAnswer::CacheDisabled {
                return DetectorStatus::Found(Vec::new());
            }
        }

        if let Some((root, route)) = documented_build_cache_root(ctx) {
            return cache_root_status(
                self.id(),
                ResourceKind::GoBuildCache,
                &root,
                Regenerability::RegenerableByRebuild,
                Recoverability::RegenerableByRebuild,
                &route.provenance("go's build cache"),
                DOCUMENTED_ROUTE_ABSENCE,
            );
        }

        go_env_cache_root(
            self.id(),
            ResourceKind::GoBuildCache,
            "GOCACHE",
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
        )
    }
}

impl Detector for GoModuleCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("go_module_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        MODULE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        if let Some((root, route)) = documented_module_cache_root(ctx) {
            return cache_root_status(
                self.id(),
                ResourceKind::GoModuleCache,
                &root,
                Regenerability::RegenerableByTool,
                Recoverability::RegenerableByTool,
                &route.provenance("go's module cache"),
                DOCUMENTED_ROUTE_ABSENCE,
            );
        }

        go_env_cache_root(
            self.id(),
            ResourceKind::GoModuleCache,
            "GOMODCACHE",
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_kinds_are_stable() {
        assert_eq!(GoBuildCacheDetector.id(), DetectorId("go_build_cache"));
        assert_eq!(
            GoBuildCacheDetector.resource_kinds(),
            &[ResourceKind::GoBuildCache]
        );
        assert_eq!(GoModuleCacheDetector.id(), DetectorId("go_module_cache"));
        assert_eq!(
            GoModuleCacheDetector.resource_kinds(),
            &[ResourceKind::GoModuleCache]
        );
    }

    /// The build cache and the module cache must not be described the same
    /// way. A cache of compiled output that only `go build` can reproduce is
    /// not the same risk as a cache of downloads `go mod download` refetches,
    /// and collapsing them would hand the module cache the build cache's
    /// rebuild-cost reasoning (or vice versa).
    #[test]
    fn the_two_roots_are_classified_differently() {
        let root = crate::detectors::test_support::make_temp_dir("go-cache-fixture");
        std::fs::create_dir_all(root.join("ab")).unwrap();
        std::fs::write(root.join("ab/abcdef-d"), vec![0u8; 4096]).unwrap();

        let build = go_root_evidence(
            &root,
            ResourceKind::GoBuildCache,
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
        );
        assert_eq!(build.regenerability, Regenerability::RegenerableByRebuild);
        assert_eq!(build.recoverability, Recoverability::RegenerableByRebuild);

        let module = go_root_evidence(
            &root,
            ResourceKind::GoModuleCache,
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
        );
        assert_eq!(module.regenerability, Regenerability::RegenerableByTool);
        assert_eq!(module.recoverability, Recoverability::RegenerableByTool);

        // Both still measured, not assumed (AC 8).
        assert_eq!(
            build.logical_bytes,
            crate::evidence::ProbeOutcome::Observed(4096)
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `GOCACHE=off` is an answer, not a broken probe: the user has turned
    /// the build cache off, so there is no resource — and reporting `Failed`
    /// would claim we do not know what is on disk when we do.
    #[test]
    fn a_disabled_build_cache_is_no_resource_not_a_failure() {
        assert_eq!(interpret_go_env_answer("off"), GoEnvAnswer::CacheDisabled);
    }

    /// The `off` check must not swallow a real path that merely mentions it.
    #[test]
    fn a_path_is_not_mistaken_for_the_disabled_sentinel() {
        assert_eq!(
            interpret_go_env_answer("/Users/dev/off"),
            GoEnvAnswer::Root("/Users/dev/off")
        );
        assert_eq!(
            interpret_go_env_answer("/Users/dev/Library/Caches/go-build"),
            GoEnvAnswer::Root("/Users/dev/Library/Caches/go-build")
        );
    }

    /// A relative answer is refused rather than resolved against whatever
    /// the scan's working directory happens to be.
    #[test]
    fn a_relative_answer_is_a_failure_not_a_resource() {
        let status = cache_root_status(
            GoModuleCacheDetector.id(),
            ResourceKind::GoModuleCache,
            Path::new("pkg/mod"),
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "path reported by `go env GOMODCACHE`",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        );
        match status {
            DetectorStatus::Failed(msg) => assert!(msg.contains("absolute")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// HORO-1560 AC 2 for the build cache, end to end through `discover`: a
    /// cache at go's documented default location is measured and the evidence
    /// says no tool was asked.
    ///
    /// Also the mutation test for that branch. Delete it and `discover` falls
    /// through to the real `go env GOCACHE`, which either names this
    /// workstation's own build cache (wrong bytes) or reports `ToolAbsent` on a
    /// machine without Go — failing either way, never silently passing.
    ///
    /// macOS only: [`user_caches_dir`] declines to invent a default location on
    /// a platform where it does not know go's, and this fixture is that
    /// location.
    #[cfg(target_os = "macos")]
    #[test]
    fn gos_documented_default_build_cache_is_measured_without_running_go() {
        let home = crate::detectors::test_support::make_temp_dir("go-documented-home");
        let cache = home.join("Library/Caches/go-build");
        std::fs::create_dir_all(cache.join("ab")).unwrap();
        std::fs::write(cache.join("ab/abcdef-d"), vec![0u8; 4_096]).unwrap();

        match GoBuildCacheDetector.discover(&DiscoveryContext::new(&home)) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(4_096)
                );
                assert!(
                    ev.sources
                        .iter()
                        .any(|s| s.contains("the tool was not run")),
                    "the evidence must record that no tool was asked: {:?}",
                    ev.sources
                );
                assert!(
                    !ev.sources.iter().any(|s| s.contains("go env")),
                    "nothing may claim `go env` reported this: {:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// `GOCACHE` outranks the default location, and the evidence names the
    /// variable that answered.
    #[cfg(target_os = "macos")]
    #[test]
    fn gocache_outranks_the_default_location() {
        let home = crate::detectors::test_support::make_temp_dir("go-relocated-home");
        let decoy = home.join("Library/Caches/go-build");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("decoy-d"), vec![0u8; 8_192]).unwrap();
        let relocated = home.join("elsewhere");
        std::fs::create_dir_all(&relocated).unwrap();
        std::fs::write(relocated.join("real-d"), vec![0u8; 1_024]).unwrap();

        let ctx = DiscoveryContext::new(&home)
            .with_tool_env(ToolEnvVar::GoCache, relocated.to_str().unwrap());

        match GoBuildCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(1_024),
                    "the relocated cache, not the stale default one"
                );
                assert!(
                    ev.sources.iter().any(|s| s.contains("GOCACHE")),
                    "{:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// `GOCACHE=off` is answered from the environment, without a spawn — and
    /// still means "no resource", not "failed probe".
    ///
    /// This is the mutation test for that branch too: `off` is not an absolute
    /// path, so without the branch the documented route declines and `discover`
    /// runs the real `go env GOCACHE`, whose answer on this workstation is a
    /// populated cache directory rather than an empty result.
    #[test]
    fn a_disabled_build_cache_is_read_from_the_environment() {
        let home = crate::detectors::test_support::make_temp_dir("go-disabled-home");

        let ctx = DiscoveryContext::new(&home).with_tool_env(ToolEnvVar::GoCache, GOCACHE_DISABLED);

        assert_eq!(
            GoBuildCacheDetector.discover(&ctx),
            DetectorStatus::Found(Vec::new())
        );

        std::fs::remove_dir_all(&home).ok();
    }

    /// The module cache's documented default sits under the default `GOPATH`,
    /// which is `~/go` — not under the platform cache directory the build cache
    /// uses. The two defaults are different shapes and must not be conflated.
    #[test]
    fn the_module_caches_documented_default_lives_under_the_default_gopath() {
        let home = crate::detectors::test_support::make_temp_dir("go-modcache-home");
        let cache = home.join("go/pkg/mod");
        std::fs::create_dir_all(cache.join("cache/download")).unwrap();
        std::fs::write(cache.join("cache/download/mod.zip"), vec![0u8; 2_048]).unwrap();

        match GoModuleCacheDetector.discover(&DiscoveryContext::new(&home)) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(2_048)
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// `GOPATH` relocates the module cache, and only its *first* entry owns it
    /// — go's documented rule, confirmed against the installed toolchain. A
    /// resolver that took the whole variable would build one path with a
    /// separator inside it and find nothing.
    #[test]
    fn the_first_gopath_entry_owns_the_module_cache() {
        let home = crate::detectors::test_support::make_temp_dir("go-gopath-home");
        let first = home.join("first");
        std::fs::create_dir_all(first.join("pkg/mod")).unwrap();
        std::fs::write(first.join("pkg/mod/real.zip"), vec![0u8; 1_024]).unwrap();
        let second = home.join("second");
        std::fs::create_dir_all(second.join("pkg/mod")).unwrap();
        std::fs::write(second.join("pkg/mod/decoy.zip"), vec![0u8; 4_096]).unwrap();

        let gopath = format!("{}:{}", first.display(), second.display());
        let ctx = DiscoveryContext::new(&home).with_tool_env(ToolEnvVar::GoPath, gopath);

        match GoModuleCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => assert_eq!(
                evidence[0].logical_bytes,
                crate::evidence::ProbeOutcome::Observed(1_024),
                "the first GOPATH entry's cache, not the second's"
            ),
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// `GOMODCACHE` outranks the whole `GOPATH` chain.
    #[test]
    fn gomodcache_outranks_the_gopath_chain() {
        let home = crate::detectors::test_support::make_temp_dir("go-modcache-direct-home");
        let decoy = home.join("go/pkg/mod");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("decoy.zip"), vec![0u8; 8_192]).unwrap();
        let relocated = home.join("elsewhere");
        std::fs::create_dir_all(&relocated).unwrap();
        std::fs::write(relocated.join("real.zip"), vec![0u8; 512]).unwrap();

        let ctx = DiscoveryContext::new(&home)
            .with_tool_env(ToolEnvVar::GoModCache, relocated.to_str().unwrap());

        match GoModuleCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(512)
                );
                assert!(
                    ev.sources.iter().any(|s| s.contains("GOMODCACHE")),
                    "{:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// A relative `GOPATH` entry yields no documented default. go refuses such
    /// an entry outright, so there is nothing here to resolve and the caller
    /// must fall through to the spawn — asserted on the resolver rather than on
    /// `discover`, which would run the real `go` at that point.
    #[test]
    fn a_relative_gopath_entry_yields_no_documented_default() {
        let home = crate::detectors::test_support::make_temp_dir("go-relative-gopath-home");

        for value in ["relgp", "", "relgp:/tmp/absolute"] {
            let ctx = DiscoveryContext::new(&home).with_tool_env(ToolEnvVar::GoPath, value);
            assert!(
                default_module_cache_dir(&ctx).is_none(),
                "a GOPATH of {value:?} must not produce a documented default"
            );
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// An empty home leaves both documented routes silent, so neither detector
    /// short-circuits the spawn on a machine where nothing is at the documented
    /// locations. Without this, the two tests above could pass while the
    /// fallback probes had become unreachable (HORO-1560 AC 6).
    #[test]
    fn an_empty_home_leaves_both_documented_routes_with_no_answer() {
        let home = crate::detectors::test_support::make_temp_dir("go-empty-home");
        let ctx = DiscoveryContext::new(&home);

        assert!(documented_build_cache_root(&ctx).is_none());
        assert!(documented_module_cache_root(&ctx).is_none());

        std::fs::remove_dir_all(&home).ok();
    }

    fn go_root_evidence(
        root: &Path,
        kind: ResourceKind,
        regenerability: Regenerability,
        recoverability: Recoverability,
    ) -> crate::evidence::Evidence {
        match cache_root_status(
            DetectorId("go_test"),
            kind,
            root,
            regenerability,
            recoverability,
            "test fixture",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        ) {
            DetectorStatus::Found(mut evidence) if evidence.len() == 1 => evidence.remove(0),
            other => panic!("expected one Found evidence, got {other:?}"),
        }
    }
}
