//! Homebrew cache detector.
//!
//! Asks `brew --cache` where the cache is rather than hardcoding a path — the
//! location can be overridden by `HOMEBREW_CACHE` and differs between
//! Intel/Apple Silicon default prefixes.
//!
//! `reclaimable_bytes` (HORO-992): rather than parsing `brew cleanup -n`'s
//! dry-run output (its exact wording/format is not a stable contract
//! across Homebrew versions, so parsing it reliably is real ongoing
//! maintenance risk), this detector reports the same measured estimate for
//! both figures. The cache directory holds only downloaded bottles/sources
//! that Homebrew fully owns and can re-download on demand, so
//! `reclaimable_bytes == logical_bytes` is an honest equivalence of meaning
//! here. The estimate recurses through the full cache tree (including nested
//! subdirectories like `Cask/`), bounded by a size/time budget (HORO-1016) —
//! see [`super::estimate_logical_bytes`]'s own doc comment for what happens
//! if that budget is hit before the walk finishes (a truthful lower bound,
//! never a precision guarantee).
//!
//! ## Why the shared helpers, and not a probe of its own (HORO-1558)
//!
//! This detector predates both [`query_tool_single_path`] and
//! [`cache_root_status`], and its own subprocess handling had drifted away from
//! what the rest of the detectors do in two ways that were visible to the user:
//!
//! - A `brew` that answered with a cache directory it has not created yet —
//!   the ordinary state of a fresh install, and of this workstation — was
//!   reported as `ToolAbsent`, i.e. "Homebrew is not installed". That status is
//!   carried all the way out to the GUI, so the report stated something false
//!   about the machine. The distinction
//!   [`RootAbsence::ToolAnsweredWithAPathItHasNotWritten`] exists for exactly
//!   this case: the tool answered, so it is installed, and there is simply no
//!   resource to report.
//! - It treated all of stdout as the path. Any extra line — a `brew` notice, a
//!   shim banner — produced a multi-line path that could not be canonicalized,
//!   reaching the same false `ToolAbsent` silently. [`query_tool_single_path`]
//!   selects by shape and fails closed on genuine ambiguity (HORO-1557).

use std::path::Path;

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, query_tool_single_path, Detector, DetectorId, DetectorStatus,
    DiscoveryContext, RootAbsence, ToolQuery,
};

pub struct HomebrewDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::HomebrewCache];

/// Asking Homebrew where its cache is. An argument array, never a shell
/// string, and a read-only query: `brew --cache` with no formula argument
/// prints the directory and downloads nothing.
const CACHE_QUERY: &[&str] = &["--cache"];

impl Detector for HomebrewDetector {
    fn id(&self) -> DetectorId {
        DetectorId("homebrew_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
        status_for(query_tool_single_path("brew", CACHE_QUERY))
    }
}

/// Maps what `brew --cache` answered onto a detector status.
///
/// Separate from [`Detector::discover`] so the mapping is testable: whether
/// Homebrew is installed, and whether it has downloaded anything yet, are
/// properties of the machine running the tests, and the statuses this detector
/// reports must not be. A test that called `discover` would assert whichever
/// answer this workstation happens to give.
fn status_for(query: ToolQuery) -> DetectorStatus {
    match query {
        ToolQuery::Lines(lines) => cache_root_status(
            HomebrewDetector.id(),
            ResourceKind::HomebrewCache,
            Path::new(&lines[0]),
            // Downloaded bottles and sources, which Homebrew re-downloads on
            // demand given network access.
            Regenerability::RegenerableByTool,
            Recoverability::RegenerableByTool,
            "Homebrew's cache, under the directory reported by `brew --cache`",
            // `brew` answered, so Homebrew is installed. A directory it has
            // not written yet means there is no resource — not that the tool
            // is missing (HORO-1558).
            RootAbsence::ToolAnsweredWithAPathItHasNotWritten,
        ),
        ToolQuery::ToolAbsent => DetectorStatus::ToolAbsent,
        ToolQuery::Failed(msg) => DetectorStatus::Failed(msg),
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

    fn answered(path: &Path) -> DetectorStatus {
        status_for(ToolQuery::Lines(vec![path.to_str().unwrap().to_string()]))
    }

    /// The defect this detector was rewritten for (HORO-1558): `brew --cache`
    /// on a working Homebrew install prints a directory that does not
    /// necessarily exist yet. Reporting that as `ToolAbsent` told the user
    /// Homebrew was not installed, on a machine where it was.
    #[test]
    fn a_cache_directory_brew_has_not_written_yet_is_not_tool_absent() {
        let cache = crate::detectors::test_support::make_temp_dir("brew-cache-parent");
        let never_written = cache.join("Homebrew");

        assert_eq!(answered(&never_written), DetectorStatus::Found(Vec::new()));

        std::fs::remove_dir_all(&cache).ok();
    }

    /// `ToolAbsent` remains reachable, and means only what it says: nothing
    /// named `brew` was found to run. Without this, the fix above could have
    /// been "never report ToolAbsent", which is a different false statement.
    #[test]
    fn a_brew_that_is_not_installed_is_tool_absent() {
        assert_eq!(
            status_for(ToolQuery::ToolAbsent),
            DetectorStatus::ToolAbsent
        );
    }

    /// A populated cache is measured, and the bytes are the cache's own —
    /// recursing through the nested layout Homebrew actually uses.
    #[test]
    fn a_populated_cache_is_measured() {
        let cache = crate::detectors::test_support::make_temp_dir("brew-cache-populated");
        std::fs::create_dir_all(cache.join("downloads")).unwrap();
        std::fs::create_dir_all(cache.join("Cask")).unwrap();
        std::fs::write(cache.join("downloads/bottle.tar.gz"), vec![0u8; 4_096]).unwrap();
        std::fs::write(cache.join("Cask/some.dmg"), vec![0u8; 2_048]).unwrap();

        match answered(&cache) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(ev.resource.kind, ResourceKind::HomebrewCache);
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(6_144),
                    "the whole cache tree, including nested Cask/"
                );
                // HORO-992: the two figures are deliberately the same here,
                // and that equivalence is the claim being made.
                assert_eq!(ev.reclaimable_bytes, ev.logical_bytes);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&cache).ok();
    }

    /// A probe that ran and could not be understood is `Failed`, carrying what
    /// went wrong — never `ToolAbsent`, and never an empty result that would
    /// read as "Homebrew has no cache".
    #[test]
    fn an_unusable_answer_is_a_failed_probe() {
        match status_for(ToolQuery::Failed("brew --cache exited with 1".to_string())) {
            DetectorStatus::Failed(msg) => assert_eq!(msg, "brew --cache exited with 1"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The query stays a read-only one. `brew --cache <formula>` downloads the
    /// formula if it is not already cached, so an argument creeping in here
    /// would make a detector write to the resource it measures — the shape of
    /// HORO-1556 all over again.
    #[test]
    fn the_query_takes_no_formula_argument() {
        assert_eq!(CACHE_QUERY, &["--cache"]);
    }
}
