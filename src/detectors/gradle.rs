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
//! So the location comes from `GRADLE_USER_HOME` when the caller observed it
//! set, and `~/.gradle` otherwise — the same two rules Gradle itself applies
//! ([`DiscoveryContext::tool_env`] supplies the override).
//!
//! Because the path is inferred, a missing `caches` directory is judged by
//! the Gradle user home above it
//! ([`RootAbsence::InferredUnderToolOwnedParent`]). A Gradle user home that
//! exists means Gradle has run here and simply has no cache yet, which is
//! `Found(vec![])`; only a missing user home is grounds for claiming Gradle
//! is not installed. Reporting `tool_absent` for a present user home told
//! real Gradle users the tool they had just used was not installed
//! (HORO-1575).
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
    ToolEnvVar,
};

pub struct GradleCacheDetector;

const KINDS: &[ResourceKind] = &[ResourceKind::GradleCache];

/// The one subdirectory of the Gradle user home this detector will ever
/// name. See the module doc for why the parent is out of bounds.
const CACHES_SUBDIR: &str = "caches";

/// Resolves the Gradle cache root from `GRADLE_USER_HOME` and `$HOME`,
/// applying Gradle's own two rules.
///
/// Pure so it is testable: the `GRADLE_USER_HOME` override arrives through
/// [`DiscoveryContext::tool_env`] rather than from the process environment,
/// so a fixture context can exercise both rules (see [`super::ToolEnvVar`]
/// for why a detector must not read the environment itself).
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
        let caches = gradle_caches_dir(ctx.tool_env(ToolEnvVar::GradleUserHome), &ctx.home_dir);
        // `caches` is always `<gradle user home>/caches`, so the parent is
        // the Gradle user home itself — a directory only Gradle creates.
        let gradle_user_home = caches
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| caches.clone());

        cache_root_status(
            self.id(),
            ResourceKind::GradleCache,
            &caches,
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            "Gradle cache directory under the Gradle user home \
             (GRADLE_USER_HOME if set, otherwise ~/.gradle)",
            RootAbsence::InferredUnderToolOwnedParent(gradle_user_home),
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

        // The real `discover`, through a hermetic `DiscoveryContext`: with no
        // `GradleUserHome` tool home set it resolves from `home_dir`, so this
        // cannot reach the host's own `~/.gradle` (HORO-1543).
        match GradleCacheDetector.discover(&DiscoveryContext::new(&home)) {
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

    /// No Gradle user home at all: nothing here has observed Gradle, so
    /// `tool_absent` is the honest answer — and still never a zero-byte
    /// resource.
    #[test]
    fn no_gradle_user_home_is_tool_absent_not_zero_bytes() {
        let home = crate::detectors::test_support::make_temp_dir("gradle-home-empty");
        assert_eq!(
            GradleCacheDetector.discover(&DiscoveryContext::new(&home)),
            DetectorStatus::ToolAbsent
        );

        std::fs::remove_dir_all(&home).ok();
    }

    /// The HORO-1575 defect, as the discriminating pair to the test above:
    /// the same missing `caches` directory, with `~/.gradle` present. Gradle
    /// has demonstrably run here, so reporting it as not installed is false.
    ///
    /// The two fixtures differ in exactly one thing — whether `.gradle`
    /// exists — so this cannot pass for any reason other than the
    /// tool-owned-parent rule.
    #[test]
    fn a_gradle_user_home_with_no_caches_yet_is_not_a_missing_gradle() {
        let home = crate::detectors::test_support::make_temp_dir("gradle-home-no-caches");
        std::fs::create_dir_all(home.join(".gradle")).unwrap();
        // Something Gradle itself would have written, and nothing else does.
        std::fs::write(home.join(".gradle/gradle.properties"), b"# empty\n").unwrap();

        let status = GradleCacheDetector.discover(&DiscoveryContext::new(&home));
        std::fs::remove_dir_all(&home).ok();

        match status {
            DetectorStatus::Found(evidence) => assert!(
                evidence.is_empty(),
                "an unwritten cache must not become a zero-byte resource"
            ),
            DetectorStatus::ToolAbsent => panic!(
                "`~/.gradle` exists, so Gradle is installed; reporting tool_absent \
                 tells the user the tool they just used is missing (HORO-1575)"
            ),
            other => panic!("expected Found(empty), got {other:?}"),
        }
    }

    /// The `GRADLE_USER_HOME` override has to carry the rule with it: the
    /// parent whose presence is judged must be the relocated user home, not
    /// `~/.gradle`. The fixture home deliberately has no `.gradle`, so a
    /// detector that judged the wrong parent would report `ToolAbsent`.
    #[test]
    fn the_rule_follows_a_relocated_gradle_user_home() {
        let relocated = crate::detectors::test_support::make_temp_dir("gradle-relocated-no-caches");
        let decoy_home = crate::detectors::test_support::make_temp_dir("gradle-no-dot-gradle");

        let ctx = DiscoveryContext::new(&decoy_home)
            .with_tool_env(ToolEnvVar::GradleUserHome, relocated.to_str().unwrap());
        let status = GradleCacheDetector.discover(&ctx);

        std::fs::remove_dir_all(&relocated).ok();
        std::fs::remove_dir_all(&decoy_home).ok();

        match status {
            DetectorStatus::Found(evidence) => assert!(evidence.is_empty()),
            other => panic!(
                "the relocated user home exists, so the answer must be Found(empty); \
                 got {other:?}, which means the rule judged ~/.gradle instead"
            ),
        }
    }

    /// The `GradleUserHome` override has to reach `discover`, not merely
    /// `gradle_caches_dir`: the pure tests above would still pass if
    /// `discover` ignored `ctx.tool_env`. The decoy under `home_dir` holds a
    /// different byte count, so reading the wrong root reports 1_000 and fails.
    #[test]
    fn the_gradle_user_home_override_reaches_the_detector() {
        let relocated = crate::detectors::test_support::make_temp_dir("gradle-relocated");
        std::fs::create_dir_all(relocated.join("caches/modules-2")).unwrap();
        std::fs::write(
            relocated.join("caches/modules-2/some.jar"),
            vec![0u8; 8_192],
        )
        .unwrap();

        let decoy_home = crate::detectors::test_support::make_temp_dir("gradle-decoy-home");
        std::fs::create_dir_all(decoy_home.join(".gradle/caches")).unwrap();
        std::fs::write(
            decoy_home.join(".gradle/caches/decoy.jar"),
            vec![0u8; 1_000],
        )
        .unwrap();

        let ctx = DiscoveryContext::new(&decoy_home)
            .with_tool_env(ToolEnvVar::GradleUserHome, relocated.to_str().unwrap());

        match GradleCacheDetector.discover(&ctx) {
            DetectorStatus::Found(evidence) => {
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].logical_bytes,
                    crate::evidence::ProbeOutcome::Observed(8_192),
                    "the relocated cache is the one GRADLE_USER_HOME names; \
                     1000 would mean the override never reached `discover`"
                );
            }
            other => panic!("expected Found, got {other:?}"),
        }

        std::fs::remove_dir_all(&relocated).ok();
        std::fs::remove_dir_all(&decoy_home).ok();
    }
}
