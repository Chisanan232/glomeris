//! Gradle cache detector (HORO-1543).
//!
//! The resource is `<gradle user home>/caches`, and deliberately NEVER the
//! Gradle user home itself. That directory also holds `gradle.properties`
//! — the conventional place for signing keys, repository passwords and
//! publish tokens — plus `init.d` startup scripts and `daemon` state. A
//! detector that named the parent would put credential material inside a
//! reclaimable resource, which no later policy class could make safe again.
//!
//! Unlike the pip/uv/Go detectors, this one infers its path instead of
//! asking the tool: Gradle has no cheap "print your cache directory"
//! command (`gradle --version` starts a JVM and does not print it anyway).
//! So the location comes from `GRADLE_USER_HOME` when set and `~/.gradle`
//! otherwise — the same two rules Gradle itself applies — and a missing
//! directory is [`RootAbsence::NothingObservedAboutTheTool`], because
//! nothing here ever observed whether Gradle is installed.
//!
//! `caches/` mixes downloaded dependency artifacts (`modules-2`) with local
//! build-cache output (`build-cache-1`), so its contents come back partly
//! by refetching and partly by recompiling. It is reported as
//! [`Regenerability::RegenerableByRebuild`] for that reason: describing the
//! whole root as tool-refetchable would overstate what a refetch restores.

use std::path::{Path, PathBuf};

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence,
};

pub struct GradleCacheDetector;

const KINDS: &[ResourceKind] = &[ResourceKind::GradleCache];

/// The one subdirectory of the Gradle user home this detector will ever
/// name. See the module doc for why the parent is out of bounds.
const CACHES_SUBDIR: &str = "caches";

/// Resolves the Gradle cache root from `GRADLE_USER_HOME` and `$HOME`,
/// applying Gradle's own two rules.
///
/// Pure so it is testable: `std::env::set_var` is unsound in Rust's
/// threaded test harness, so the environment is read once at the call site
/// and passed in.
///
/// An empty or whitespace-only `GRADLE_USER_HOME` falls back to `~/.gradle`
/// rather than resolving to `/caches` or to a relative path — an exported
/// but unset variable is a common shell accident, and treating it as an
/// answer would name a directory belonging to something else entirely.
fn gradle_caches_dir(gradle_user_home: Option<&str>, home: &Path) -> PathBuf {
    let user_home = match gradle_user_home.map(str::trim) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".gradle"),
    };
    user_home.join(CACHES_SUBDIR)
}

impl Detector for GradleCacheDetector {
    fn id(&self) -> DetectorId {
        DetectorId("gradle_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let gradle_user_home = std::env::var("GRADLE_USER_HOME").ok();
        let caches = gradle_caches_dir(gradle_user_home.as_deref(), &ctx.home_dir);

        cache_root_status(
            self.id(),
            ResourceKind::GradleCache,
            &caches,
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            "Gradle cache directory under the Gradle user home \
             (GRADLE_USER_HOME if set, otherwise ~/.gradle)",
            RootAbsence::NothingObservedAboutTheTool,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kinds_are_stable() {
        assert_eq!(GradleCacheDetector.id(), DetectorId("gradle_cache"));
        assert_eq!(
            GradleCacheDetector.resource_kinds(),
            &[ResourceKind::GradleCache]
        );
    }

    #[test]
    fn gradle_user_home_wins_over_the_default() {
        assert_eq!(
            gradle_caches_dir(Some("/opt/gradle-home"), Path::new("/Users/dev")),
            PathBuf::from("/opt/gradle-home/caches")
        );
    }

    #[test]
    fn the_default_is_under_the_home_directory() {
        assert_eq!(
            gradle_caches_dir(None, Path::new("/Users/dev")),
            PathBuf::from("/Users/dev/.gradle/caches")
        );
    }

    /// An exported-but-empty `GRADLE_USER_HOME` must not become an answer:
    /// `""` would otherwise resolve to `caches` (relative) or `/caches`.
    #[test]
    fn an_empty_gradle_user_home_falls_back_rather_than_naming_a_stray_path() {
        for value in ["", "   ", "\t"] {
            assert_eq!(
                gradle_caches_dir(Some(value), Path::new("/Users/dev")),
                PathBuf::from("/Users/dev/.gradle/caches"),
                "GRADLE_USER_HOME={value:?} should have fallen back"
            );
        }
    }

    /// The credential-safety invariant, asserted rather than commented: the
    /// resolved root is never the Gradle user home, under either rule.
    #[test]
    fn the_gradle_user_home_itself_is_never_the_resource() {
        let from_env = gradle_caches_dir(Some("/opt/gradle-home"), Path::new("/Users/dev"));
        assert_ne!(from_env, PathBuf::from("/opt/gradle-home"));
        assert!(from_env.ends_with(CACHES_SUBDIR));

        let from_home = gradle_caches_dir(None, Path::new("/Users/dev"));
        assert_ne!(from_home, PathBuf::from("/Users/dev/.gradle"));
        assert!(from_home.ends_with(CACHES_SUBDIR));
    }

    /// Positive control for the same invariant end to end: a Gradle user
    /// home holding both `gradle.properties` and `caches/` yields evidence
    /// that measures only the cache subtree. If the detector ever named the
    /// parent, the observed byte count would include the credential file and
    /// this assertion would fail.
    #[test]
    fn credential_bearing_files_beside_the_cache_are_not_part_of_the_resource() {
        let home = crate::detectors::test_support::make_temp_dir("gradle-home-fixture");
        let gradle_user_home = home.join(".gradle");
        std::fs::create_dir_all(gradle_user_home.join("caches/modules-2")).unwrap();
        std::fs::write(
            gradle_user_home.join("gradle.properties"),
            vec![b'x'; 1_000],
        )
        .unwrap();
        std::fs::write(
            gradle_user_home.join("caches/modules-2/some.jar"),
            vec![0u8; 4_096],
        )
        .unwrap();

        let caches = gradle_caches_dir(None, &home);
        let status = cache_root_status(
            GradleCacheDetector.id(),
            ResourceKind::GradleCache,
            &caches,
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            "test fixture",
            RootAbsence::NothingObservedAboutTheTool,
        );

        match status {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                let ev = &evidence[0];
                assert_eq!(
                    ev.logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(4_096),
                    "only the cache subtree may be measured"
                );
                match &ev.resource.locator {
                    crate::evidence::ResourceLocator::Path(named) => {
                        assert!(named.ends_with("caches"));
                        assert!(!named.ends_with(".gradle"));
                    }
                    other => panic!("expected a path locator, got {other:?}"),
                }
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&home).ok();
    }

    /// A Gradle user home that exists but has no `caches` yet reports
    /// nothing, never a zero-byte resource.
    #[test]
    fn a_missing_cache_directory_is_tool_absent_not_zero_bytes() {
        let home = crate::detectors::test_support::make_temp_dir("gradle-home-empty");
        let status = cache_root_status(
            GradleCacheDetector.id(),
            ResourceKind::GradleCache,
            &gradle_caches_dir(None, &home),
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            "test fixture",
            RootAbsence::NothingObservedAboutTheTool,
        );
        assert_eq!(status, DetectorStatus::ToolAbsent);

        std::fs::remove_dir_all(&home).ok();
    }
}
