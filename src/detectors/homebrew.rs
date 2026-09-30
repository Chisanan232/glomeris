//! Homebrew cache detector.
//!
//! Does not hardcode a cache path: the location can be overridden by
//! `HOMEBREW_CACHE` and differs between Intel/Apple Silicon default prefixes.
//!
//! What it does read, before considering a subprocess, is Homebrew's own
//! documented configuration (HORO-1560 AC 2): `HOMEBREW_CACHE`, then the
//! documented default `~/Library/Caches/Homebrew`. Homebrew does not consult
//! `XDG_CACHE_HOME` on macOS — checked against the installed Homebrew, because
//! it does on Linux. `brew --cache` remains the fallback for when neither of
//! those names a directory that is there, which is also the case a
//! `HOMEBREW_CACHE` set in a shell profile Glomeris never sources falls into.
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

use std::path::{Path, PathBuf};

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, documented_cache_root, query_tool_single_path, user_caches_dir, CacheRoute,
    Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence, ToolEnvVar, ToolQuery,
    DOCUMENTED_ROUTE_ABSENCE,
};

pub struct HomebrewDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::HomebrewCache];

/// The program this detector runs. Named here rather than inline so
/// [`super::SPAWNED_PROGRAMS`] can be built from the detectors' own
/// declarations instead of a second list that could drift from them.
pub(super) const BREW_PROGRAM: &str = "brew";

/// Asking Homebrew where its cache is. An argument array, never a shell
/// string, and a read-only query: `brew --cache` with no formula argument
/// prints the directory and downloads nothing.
const CACHE_QUERY: &[&str] = &["--cache"];

/// Homebrew's documented default cache directory, inside the platform's own
/// cache directory.
const CACHE_SUBDIR: &str = "Homebrew";

/// Where Homebrew's own documented configuration puts its cache, when that
/// answers without running `brew`. `HOMEBREW_CACHE` first, then the documented
/// default.
fn documented_cache_dir(ctx: &DiscoveryContext) -> Option<(PathBuf, CacheRoute)> {
    documented_cache_root(
        ctx,
        ToolEnvVar::HomebrewCache,
        user_caches_dir(&ctx.home_dir).map(|caches| caches.join(CACHE_SUBDIR)),
    )
}

impl Detector for HomebrewDetector {
    fn id(&self) -> DetectorId {
        DetectorId("homebrew_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        if let Some((root, route)) = documented_cache_dir(ctx) {
            return cache_root_status(
                self.id(),
                ResourceKind::HomebrewCache,
                &root,
                Regenerability::RegenerableByTool,
                Recoverability::RegenerableByTool,
                &route.provenance("Homebrew's cache"),
                DOCUMENTED_ROUTE_ABSENCE,
            );
        }

        // Why Homebrew has to be asked (HORO-1560 AC 3): `HOMEBREW_CACHE` is
        // normally set in a shell profile, which Glomeris never sources, so a
        // relocated cache is invisible to the documented route above. And only
        // `brew` can tell a Homebrew that has downloaded nothing from one that
        // is not installed — the distinction this detector exists to keep
        // honest (HORO-1558).
        //
        // What Homebrew does when asked: `brew --cache`, with no formula
        // argument, prints the cache directory and downloads nothing. See
        // [`CACHE_QUERY`] for why that argument must stay absent.
        status_for(query_tool_single_path(BREW_PROGRAM, CACHE_QUERY))
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
        // A timeout is reported as a failed probe, never as `ToolAbsent`: we
        // know `brew` exists, because we spawned it (HORO-1559).
        ToolQuery::Failed(msg) | ToolQuery::TimedOut(msg) => DetectorStatus::Failed(msg),
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

    /// HORO-1560 AC 2 for Homebrew, end to end through `discover`: a cache at
    /// the documented default location is measured and the evidence says no
    /// tool was asked.
    ///
    /// Also the mutation test for that branch. Delete it and `discover` runs
    /// the real `brew --cache`, which names this workstation's own cache
    /// directory (wrong bytes) or reports `ToolAbsent` where Homebrew is not
    /// installed — failing either way, never silently passing.
    ///
    /// macOS only: [`user_caches_dir`] declines to invent a default location on
    /// a platform where it does not know Homebrew's, and this fixture is that
    /// location.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_documented_default_location_is_measured_without_running_brew() {
        let home = crate::detectors::test_support::make_temp_dir("brew-documented-home");
        let cache = home.join("Library/Caches/Homebrew");
        std::fs::create_dir_all(cache.join("downloads")).unwrap();
        std::fs::write(cache.join("downloads/bottle.tar.gz"), vec![0u8; 4_096]).unwrap();

        match HomebrewDetector.discover(&DiscoveryContext::new(&home)) {
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
                    !ev.sources.iter().any(|s| s.contains("brew --cache")),
                    "nothing may claim `brew --cache` reported this: {:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// `HOMEBREW_CACHE` outranks the default location, and the evidence names
    /// the variable that answered.
    #[cfg(target_os = "macos")]
    #[test]
    fn homebrew_cache_outranks_the_default_location() {
        let home = crate::detectors::test_support::make_temp_dir("brew-relocated-home");
        let decoy = home.join("Library/Caches/Homebrew");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("decoy.tar.gz"), vec![0u8; 8_192]).unwrap();
        let relocated = home.join("elsewhere");
        std::fs::create_dir_all(&relocated).unwrap();
        std::fs::write(relocated.join("real.tar.gz"), vec![0u8; 1_024]).unwrap();

        let ctx = DiscoveryContext::new(&home)
            .with_tool_env(ToolEnvVar::HomebrewCache, relocated.to_str().unwrap());

        match HomebrewDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(1_024),
                    "the relocated cache, not the stale default one"
                );
                assert!(
                    ev.sources.iter().any(|s| s.contains("HOMEBREW_CACHE")),
                    "{:?}",
                    ev.sources
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// The anti-vacuity half: with nothing at either documented location the
    /// route declines to answer, so `brew --cache` stays reachable — which is
    /// what keeps "Homebrew is installed but has downloaded nothing" and
    /// "Homebrew is not installed" distinguishable (HORO-1558, AC 6).
    #[test]
    fn an_empty_home_leaves_the_documented_route_with_no_answer() {
        let home = crate::detectors::test_support::make_temp_dir("brew-empty-home");

        assert!(documented_cache_dir(&DiscoveryContext::new(&home)).is_none());

        std::fs::remove_dir_all(&home).ok();
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
