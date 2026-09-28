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
//! Both locations come from `go env` rather than from `~/Library/Caches/go-build`
//! and `~/go/pkg/mod`: those defaults move with `GOCACHE`, `GOMODCACHE`,
//! `GOPATH` and `GOENV`, and on this machine a path guessed from `$HOME`
//! would be wrong for anyone using a non-default `GOPATH`.
//!
//! The two roots hold different things and are classified differently. The
//! build cache holds compiled artifacts, so it comes back only by
//! recompiling ([`Regenerability::RegenerableByRebuild`], matching the
//! existing Cargo/Xcode detectors). The module cache holds downloaded
//! module zips and their extracted trees, which `go mod download` refetches
//! ([`Regenerability::RegenerableByTool`]) — given network access and, for
//! a private module, credentials.

use std::path::Path;

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, query_tool_single_line, Detector, DetectorId, DetectorStatus,
    DiscoveryContext, RootAbsence, ToolQuery,
};

pub struct GoBuildCacheDetector;
pub struct GoModuleCacheDetector;

const BUILD_KINDS: &[ResourceKind] = &[ResourceKind::GoBuildCache];
const MODULE_KINDS: &[ResourceKind] = &[ResourceKind::GoModuleCache];

/// `go env GOCACHE`'s answer when the user has disabled the build cache
/// outright. Not a path, and deliberately not reported as a failed probe:
/// the probe worked perfectly and its answer is "there is no build cache".
const GOCACHE_DISABLED: &str = "off";

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

/// Asks `go env <var>` for one absolute cache root and builds its evidence.
fn go_env_cache_root(
    detector: DetectorId,
    kind: ResourceKind,
    var: &str,
    regenerability: Regenerability,
    recoverability: Recoverability,
) -> DetectorStatus {
    match query_tool_single_line("go", &["env", var]) {
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
        ToolQuery::Failed(msg) => DetectorStatus::Failed(msg),
    }
}

impl Detector for GoBuildCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("go_build_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        BUILD_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
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

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
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
