//! Python package-manager cache detectors: pip and uv (HORO-1543).
//!
//! One detector per *tool*, not one per ecosystem. pip and uv keep
//! separate caches in separate places, either can be installed without the
//! other, and [`ResourceKind::PipCache`]/[`ResourceKind::UvCache`] report
//! different owning tools — so folding them together would make one tool's
//! absence look like the whole Python ecosystem's absence.
//!
//! Both locations come from the tool itself (`pip cache dir`, `uv cache
//! dir`) rather than from a hardcoded `~/.cache/...` guess: pip's cache
//! moves with `PIP_CACHE_DIR`, `XDG_CACHE_HOME` and the platform, uv's with
//! `UV_CACHE_DIR`, and a guess that is wrong either reports nothing while
//! gigabytes sit elsewhere, or names a directory belonging to something
//! else entirely.
//!
//! Adding Poetry or PDM later means adding a detector beside these two,
//! each asking its own tool where its own cache is. Nothing in this module
//! hardcodes a deletable path, so that extension needs no new machinery
//! (HORO-1543: "architecture should permit future Poetry/PDM integration
//! without hardcoded path deletion").

use std::path::Path;

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, query_tool_single_path, Detector, DetectorId, DetectorStatus,
    DiscoveryContext, RootAbsence, ToolQuery,
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
const PIP_PROGRAMS: &[&str] = &["pip", "pip3"];

impl Detector for PipCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("pip_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        PIP_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
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
        match query_tool_single_path("uv", &["cache", "dir"]) {
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
            ToolQuery::Failed(msg) => DetectorStatus::Failed(msg),
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
