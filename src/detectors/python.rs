//! Python package-manager cache detectors: pip and uv (HORO-1543).
//!
//! One detector per *tool*, not one per ecosystem. pip and uv keep
//! separate caches in separate places, either can be installed without the
//! other, and [`ResourceKind::PipCache`]/[`ResourceKind::UvCache`] report
//! different owning tools — so folding them together would make one tool's
//! absence look like the whole Python ecosystem's absence.
//!
//! Neither location is a hardcoded `~/.cache/...` guess: pip's cache moves
//! with `PIP_CACHE_DIR` and the platform, uv's with `UV_CACHE_DIR`, and a
//! guess that is wrong either reports nothing while gigabytes sit elsewhere,
//! or names a directory belonging to something else entirely.
//!
//! What is *not* a guess is the tool's own documented configuration, and
//! consulting that costs no subprocess (HORO-1560 AC 2): each detector reads
//! the variable its tool documents, falls back to the location its tool
//! documents as the default, and only asks the tool when neither of those
//! names a directory that is there. See [`super::documented_cache_root`] for
//! why an absent directory is not an answer.
//!
//! Adding Poetry or PDM later means adding a detector beside these two,
//! each asking its own tool where its own cache is. Nothing in this module
//! hardcodes a deletable path, so that extension needs no new machinery
//! (HORO-1543: "architecture should permit future Poetry/PDM integration
//! without hardcoded path deletion").

use std::path::{Path, PathBuf};

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, documented_cache_root, query_tool_single_path, user_caches_dir, CacheRoute,
    Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence, ToolEnvVar, ToolQuery,
};

pub struct PipCacheDetector;
pub struct UvCacheDetector;

const PIP_KINDS: &[ResourceKind] = &[ResourceKind::PipCache];
const UV_KINDS: &[ResourceKind] = &[ResourceKind::UvCache];

/// The programs `pip cache dir` may be installed as, in the order tried.
///
/// A machine can have `pip3` without `pip` (the usual shape once the
/// Python 2 era's bare `pip` stopped being installed), so trying only one
/// name would report an absent ecosystem on a machine that has a populated
/// pip cache. `ToolAbsent` is reported only when *no* name resolves.
const PIP_PROGRAMS: &[&str] = &[PIP_PROGRAM, PIP3_PROGRAM];

/// The programs these detectors run. Named here rather than inline so
/// [`super::SPAWNED_PROGRAMS`] can be built from the detectors' own
/// declarations instead of a second list that could drift from them.
pub(super) const PIP_PROGRAM: &str = "pip";
pub(super) const PIP3_PROGRAM: &str = "pip3";
pub(super) const UV_PROGRAM: &str = "uv";

/// Where pip's own documented configuration puts its cache, when that
/// answers without running pip.
///
/// `PIP_CACHE_DIR` first, then the documented default. On macOS that default
/// is `~/Library/Caches/pip`, the platform's own cache directory — pip does
/// *not* honour `XDG_CACHE_HOME` here, which was checked against the
/// installed pip rather than inferred from pip's behaviour on Linux, where it
/// does.
fn pip_documented_cache_root(ctx: &DiscoveryContext) -> Option<(PathBuf, CacheRoute)> {
    documented_cache_root(
        ctx,
        ToolEnvVar::PipCacheDir,
        user_caches_dir(&ctx.home_dir).map(|caches| caches.join("pip")),
    )
}

/// What a cache root reached without running the tool means when it turns out
/// not to be there after all.
///
/// [`documented_cache_root`] only answers for a directory that exists, so this
/// is reached only if the directory disappears between that check and the
/// probe — a real race on a cache a tool is free to clear at any moment.
/// Nothing here ran the tool, so nothing here is entitled to say whether the
/// tool is installed: [`RootAbsence::InferredUnderSharedParent`] is the
/// variant that makes no claim about it (HORO-1575).
const DOCUMENTED_ROUTE_ABSENCE: RootAbsence = RootAbsence::InferredUnderSharedParent;

impl Detector for PipCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("pip_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        PIP_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        if let Some((root, route)) = pip_documented_cache_root(ctx) {
            return cache_root_status(
                self.id(),
                ResourceKind::PipCache,
                &root,
                Regenerability::RegenerableByTool,
                Recoverability::RegenerableByTool,
                &route.provenance("pip's cache"),
                DOCUMENTED_ROUTE_ABSENCE,
            );
        }

        let mut last_failure: Option<String> = None;

        for program in PIP_PROGRAMS {
            match query_tool_single_path(program, &["cache", "dir"]) {
                ToolQuery::Lines(lines) => {
                    return cache_root_status(
                        self.id(),
                        ResourceKind::PipCache,
                        Path::new(&lines[0]),
                        Regenerability::RegenerableByTool,
                        Recoverability::RegenerableByTool,
                        &format!("path reported by `{program} cache dir`"),
                        RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
                    );
                }
                ToolQuery::ToolAbsent => continue,
                // A pip that is installed but could not answer is a failed
                // probe, not an absent tool — but a *later* name in
                // PIP_PROGRAMS may still answer, so keep looking and only
                // report the failure if none of them does. Never discarded:
                // if nothing answers, this is what gets reported, so the
                // failure cannot end up presented as "no pip cache".
                ToolQuery::Failed(msg) => last_failure = Some(msg),
                // Unlike `Failed`, a timeout stops the loop. The remaining
                // name resolves through the same shim and version-manager
                // machinery that just stalled, so trying it would pay a
                // second PROBE_DEADLINE for an answer from the component that
                // is already known to be stuck — doubling this detector's
                // worst case for no independent information (HORO-1559).
                ToolQuery::TimedOut(msg) => {
                    last_failure = Some(msg);
                    break;
                }
            }
        }

        match last_failure {
            Some(msg) => DetectorStatus::Failed(msg),
            None => DetectorStatus::ToolAbsent,
        }
    }
}

impl Detector for UvCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("uv_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        UV_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
        match query_tool_single_path(UV_PROGRAM, &["cache", "dir"]) {
            ToolQuery::Lines(lines) => cache_root_status(
                self.id(),
                ResourceKind::UvCache,
                Path::new(&lines[0]),
                Regenerability::RegenerableByTool,
                Recoverability::RegenerableByTool,
                "path reported by `uv cache dir`",
                RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
            ),
            ToolQuery::ToolAbsent => DetectorStatus::ToolAbsent,
            // Abandoning the probe is not evidence that uv is missing: it was
            // spawned successfully, it just did not finish (HORO-1559).
            ToolQuery::Failed(msg) | ToolQuery::TimedOut(msg) => DetectorStatus::Failed(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_kinds_are_stable() {
        assert_eq!(PipCacheDetector.id(), DetectorId("pip_cache"));
        assert_eq!(PipCacheDetector.resource_kinds(), &[ResourceKind::PipCache]);
        assert_eq!(UvCacheDetector.id(), DetectorId("uv_cache"));
        assert_eq!(UvCacheDetector.resource_kinds(), &[ResourceKind::UvCache]);
    }

    /// Positive control for HORO-1543 AC 8, exercising the same
    /// `cache_dir_evidence` path a real `pip cache dir` answer takes — with
    /// a fixture directory standing in for the answer, so the assertion
    /// holds on a machine with no Python at all.
    #[test]
    fn a_populated_cache_root_is_observed_not_estimated_at_zero() {
        let root = crate::detectors::test_support::make_temp_dir("pip-cache-fixture");
        std::fs::create_dir_all(root.join("wheels/ab")).unwrap();
        std::fs::write(root.join("wheels/ab/some_pkg.whl"), vec![0u8; 8192]).unwrap();

        let status = cache_root_status(
            PipCacheDetector.id(),
            ResourceKind::PipCache,
            &root,
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "path reported by `pip cache dir`",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        );

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(ev.resource.kind, ResourceKind::PipCache);
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(8192)
                );
                assert_eq!(ev.reclaimable_bytes, ev.logical_bytes);
                assert!(!ev.reclaimable_bytes_is_lower_bound);
                assert!(ev.sources.iter().any(|s| s.contains("pip cache dir")));
                // A global cache belongs to no project (AC 4).
                assert_eq!(ev.resource.source_project_root, None);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// A tool that answers with a directory it has not created yet yields
    /// no resource at all — never a zero-byte one (AC 3). It is also not
    /// `ToolAbsent`: `uv` answered, so `uv` is installed.
    #[test]
    fn an_unpopulated_cache_root_is_no_resource_not_zero_bytes() {
        let root = crate::detectors::test_support::make_temp_dir("uv-cache-absent");
        let missing = root.join("never-created");

        let status = cache_root_status(
            UvCacheDetector.id(),
            ResourceKind::UvCache,
            &missing,
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "path reported by `uv cache dir`",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        );

        assert_eq!(status, DetectorStatus::Found(Vec::new()));

        std::fs::remove_dir_all(&root).ok();
    }

    /// HORO-1560 AC 2 for pip, end to end through `discover`: a cache sitting
    /// at the documented default location is measured, and the evidence says
    /// so — no `pip` was run to find it. Asserted through `discover` rather
    /// than through the resolver alone, because "no child process is spawned"
    /// is a property of the whole call, not of one helper.
    ///
    /// macOS only: [`user_caches_dir`] declines to invent a default location
    /// on a platform where it does not know pip's, and this fixture is that
    /// location.
    #[cfg(target_os = "macos")]
    #[test]
    fn pips_documented_default_location_is_measured_without_running_pip() {
        let home = crate::detectors::test_support::make_temp_dir("pip-documented-home");
        let cache = home.join("Library/Caches/pip");
        std::fs::create_dir_all(cache.join("wheels")).unwrap();
        std::fs::write(cache.join("wheels/some_pkg.whl"), vec![0u8; 4_096]).unwrap();

        match PipCacheDetector.discover(&DiscoveryContext::new(&home)) {
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
                    !ev.sources.iter().any(|s| s.contains("cache dir")),
                    "nothing may claim `pip cache dir` reported this: {:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// `PIP_CACHE_DIR` outranks the default location, and the evidence names
    /// the variable that answered.
    #[cfg(target_os = "macos")]
    #[test]
    fn pip_cache_dir_outranks_the_default_location() {
        let home = crate::detectors::test_support::make_temp_dir("pip-relocated-home");
        let decoy = home.join("Library/Caches/pip");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("decoy.whl"), vec![0u8; 8_192]).unwrap();
        let relocated = home.join("elsewhere");
        std::fs::create_dir_all(&relocated).unwrap();
        std::fs::write(relocated.join("real.whl"), vec![0u8; 1_024]).unwrap();

        let ctx = DiscoveryContext::new(&home)
            .with_tool_env(ToolEnvVar::PipCacheDir, relocated.to_str().unwrap());

        match PipCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(1_024),
                    "the relocated cache, not the stale default one"
                );
                assert!(
                    ev.sources.iter().any(|s| s.contains("PIP_CACHE_DIR")),
                    "{:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// The anti-vacuity half of the two above: with nothing at either
    /// documented location, the documented route declines to answer, so the
    /// detector still falls through to asking pip. Removing the
    /// existence check in [`documented_cache_root`] would make this return
    /// `Some` for a directory that is not there.
    #[test]
    fn an_empty_home_leaves_the_documented_route_with_no_answer() {
        let home = crate::detectors::test_support::make_temp_dir("pip-empty-home");

        assert!(pip_documented_cache_root(&DiscoveryContext::new(&home)).is_none());

        std::fs::remove_dir_all(&home).ok();
    }

    /// A relative answer is refused rather than resolved against whatever
    /// the scan's working directory happens to be.
    #[test]
    fn a_relative_answer_is_a_failure_not_a_resource() {
        let status = cache_root_status(
            PipCacheDetector.id(),
            ResourceKind::PipCache,
            Path::new("relative/cache"),
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "path reported by `pip cache dir`",
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        );

        match status {
            DetectorStatus::Failed(msg) => assert!(msg.contains("absolute")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
