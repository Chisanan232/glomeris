//! Maven local-repository detector (HORO-1543).
//!
//! `~/.m2/repository` is NOT assumed to be a disposable cache, and this is
//! the whole reason this detector is written differently from every other
//! one in this module. Maven's local repository holds two kinds of thing
//! that are indistinguishable from the directory's name, or from its
//! layout, or from anything a bounded probe can see:
//!
//! 1. artifacts downloaded from a remote repository, which a later build
//!    refetches; and
//! 2. artifacts put there by `mvn install` — a local module, an internal
//!    library built from a branch, a jar from a repository that no longer
//!    exists — which nothing refetches, ever.
//!
//! Establishing which is which would mean resolving every artifact against
//! every configured remote repository, over the network, with the user's
//! credentials. This detector does not do that, so it does not know, and it
//! says it does not know: [`Regenerability::Unknown`], which
//! `policy::classify` diverts to `PolicyClass::Ask` with
//! `ReasonCode::RegenerabilityUnknown`. Unknown reproducibility must never
//! be reported as safe reproducibility.
//!
//! [`Recoverability`] is reported as
//! [`Recoverability::RegenerableByTool`] rather than
//! [`Recoverability::Irreversible`] because that axis is for a detector
//! that has *positively established* permanent loss, which a directory-level
//! probe has not: Maven really does refetch everything it downloaded. The
//! part it cannot refetch is exactly the part this detector cannot
//! identify, and that lack of knowledge belongs on the regenerability axis,
//! where the type system can express it.
//!
//! Location resolution reads `~/.m2/settings.xml`'s `<localRepository>` when
//! it declares one, and defaults to `~/.m2/repository` otherwise. Maven also
//! merges a global `$M2_HOME/conf/settings.xml`, which this detector does not
//! read: a global override would leave the default path simply not existing,
//! which reports no resource — never a wrong path that does exist.

use std::path::{Path, PathBuf};

use crate::evidence::{Recoverability, Regenerability, ResourceKind};

use super::{
    cache_root_status, Detector, DetectorId, DetectorStatus, DiscoveryContext, RootAbsence,
};

pub struct MavenLocalRepositoryDetector;

const KINDS: &[ResourceKind] = &[ResourceKind::MavenLocalRepository];

/// Largest `settings.xml` this detector will read. A file past this size is
/// not a Maven settings file in any normal sense, and a detector should not
/// pull an unbounded amount of a user's XML into memory to find one element.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// The `${user.home}` property Maven expands inside `<localRepository>`.
const USER_HOME_PROPERTY: &str = "${user.home}";

/// What `settings.xml` says about `<localRepository>`.
#[derive(Debug, PartialEq)]
enum LocalRepositorySetting {
    Declared(String),
    /// No `<localRepository>` element — the documented, overwhelmingly
    /// common case. Maven's own shipped `settings.xml` ships the element
    /// *commented out*, which this must report as not declared.
    NotDeclared,
    /// The file declares something this detector refuses to interpret. Never
    /// silently treated as `NotDeclared`: falling back to the default path
    /// would name a directory the user has explicitly moved away from.
    Ambiguous(String),
}

/// Removes XML comments from `xml`, or reports that it cannot.
///
/// This runs before `<localRepository>` is looked for, and it is the single
/// most important line of this module. Maven's own shipped `settings.xml`
/// contains a commented-out example:
///
/// ```text
/// <!-- localRepository
///  | The path to the local repository maven will use to store artifacts.
///  | Default: ${user.home}/.m2/repository
/// <localRepository>/path/to/local/repo</localRepository>
/// -->
/// ```
///
/// A naive search for the element would extract `/path/to/local/repo` from
/// that and report it as fact — a path that does not exist, on a machine
/// whose real repository is somewhere else entirely.
fn strip_xml_comments(xml: &str) -> Result<String, String> {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        match after.find("-->") {
            Some(end) => rest = &after[end + 3..],
            None => {
                return Err(
                    "settings.xml contains an unterminated XML comment, so which \
                     <localRepository> elements are commented out cannot be established"
                        .to_string(),
                )
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Pure interpretation of a `settings.xml`'s text.
///
/// Pure because the interesting cases are all textual: the commented-out
/// example Maven ships, two declarations, an empty declaration. A test that
/// needed a real Maven installation to reach any of them would not be run.
fn parse_local_repository(settings_xml: &str) -> LocalRepositorySetting {
    let stripped = match strip_xml_comments(settings_xml) {
        Ok(s) => s,
        Err(why) => return LocalRepositorySetting::Ambiguous(why),
    };

    let mut declared: Vec<String> = Vec::new();
    let mut rest = stripped.as_str();
    while let Some(start) = rest.find("<localRepository") {
        let after = &rest[start + "<localRepository".len()..];
        let Some(gt) = after.find('>') else {
            return LocalRepositorySetting::Ambiguous(
                "settings.xml contains an unterminated <localRepository element".to_string(),
            );
        };
        let inside_tag = &after[..gt];

        // `<localRepositoryPath>` and friends are different elements.
        if !inside_tag.is_empty() && !inside_tag.starts_with(char::is_whitespace) {
            if inside_tag == "/" {
                return LocalRepositorySetting::Ambiguous(
                    "settings.xml declares a self-closing <localRepository/>, which names \
                     no path"
                        .to_string(),
                );
            }
            rest = &after[gt + 1..];
            continue;
        }

        let body = &after[gt + 1..];
        let Some(end) = body.find("</localRepository>") else {
            return LocalRepositorySetting::Ambiguous(
                "settings.xml has a <localRepository> element with no closing tag".to_string(),
            );
        };
        declared.push(body[..end].trim().to_string());
        rest = &body[end..];
    }

    match declared.len() {
        0 => LocalRepositorySetting::NotDeclared,
        1 if declared[0].is_empty() => LocalRepositorySetting::Ambiguous(
            "settings.xml declares an empty <localRepository>".to_string(),
        ),
        1 => LocalRepositorySetting::Declared(declared.remove(0)),
        n => LocalRepositorySetting::Ambiguous(format!(
            "settings.xml declares <localRepository> {n} times, so the effective local \
             repository cannot be established from it"
        )),
    }
}

/// Resolves the local repository path, or explains why it cannot be.
fn maven_local_repository(setting: LocalRepositorySetting, home: &Path) -> Result<PathBuf, String> {
    match setting {
        LocalRepositorySetting::Declared(value) => {
            let expanded = if let Some(tail) = value.strip_prefix(USER_HOME_PROPERTY) {
                home.join(tail.trim_start_matches('/'))
            } else {
                PathBuf::from(value)
            };
            Ok(expanded)
        }
        LocalRepositorySetting::NotDeclared => Ok(home.join(".m2").join("repository")),
        LocalRepositorySetting::Ambiguous(why) => Err(why),
    }
}

/// Reads `~/.m2/settings.xml`, if it is there and small enough to be one.
fn read_user_settings(home: &Path) -> Result<LocalRepositorySetting, String> {
    let settings = home.join(".m2").join("settings.xml");
    match std::fs::metadata(&settings) {
        Ok(meta) if meta.len() > MAX_SETTINGS_BYTES => {
            return Err(format!(
                "{} is {} bytes, past the {MAX_SETTINGS_BYTES}-byte limit this detector \
                 will read, so <localRepository> was not established",
                settings.display(),
                meta.len()
            ))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LocalRepositorySetting::NotDeclared)
        }
        Err(e) => {
            return Err(format!(
                "failed to stat {}: {e}, so <localRepository> was not established",
                settings.display()
            ))
        }
    }

    match std::fs::read_to_string(&settings) {
        Ok(text) => Ok(parse_local_repository(&text)),
        Err(e) => Err(format!(
            "failed to read {}: {e}, so <localRepository> was not established",
            settings.display()
        )),
    }
}

impl Detector for MavenLocalRepositoryDetector {
    fn id(&self) -> DetectorId {
        DetectorId("maven_local_repository")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        KINDS
    }

    fn discover(&self, ctx: &DiscoveryContext) -> DetectorStatus {
        let setting = match read_user_settings(&ctx.home_dir) {
            Ok(setting) => setting,
            Err(why) => return DetectorStatus::Failed(why),
        };
        let declared = setting != LocalRepositorySetting::NotDeclared;
        let repository = match maven_local_repository(setting, &ctx.home_dir) {
            Ok(path) => path,
            Err(why) => return DetectorStatus::Failed(why),
        };

        let provenance = if declared {
            "path declared by <localRepository> in ~/.m2/settings.xml"
        } else {
            "Maven's default local repository (~/.m2/repository); \
             ~/.m2/settings.xml declares no <localRepository>"
        };

        cache_root_status(
            self.id(),
            ResourceKind::MavenLocalRepository,
            &repository,
            // See the module doc: this detector cannot tell a downloaded
            // artifact from an `mvn install`ed one, so it reports that it
            // does not know rather than guessing in the permissive
            // direction.
            Regenerability::Unknown,
            Recoverability::RegenerableByTool,
            provenance,
            RootAbsence::NothingObservedAboutTheTool,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::evidence::correlate::{merge_into, CorrelationResult};
    use crate::evidence::{Completeness, Evidence, ProbeOutcome};
    use crate::policy::{classify, PolicyClass, PolicyConfig, ReasonCode};

    #[test]
    fn id_and_kinds_are_stable() {
        assert_eq!(
            MavenLocalRepositoryDetector.id(),
            DetectorId("maven_local_repository")
        );
        assert_eq!(
            MavenLocalRepositoryDetector.resource_kinds(),
            &[ResourceKind::MavenLocalRepository]
        );
    }

    /// Maven's own shipped `settings.xml`. The commented-out example must
    /// not become a fact.
    ///
    /// Mutation control: removing the `strip_xml_comments` call from
    /// `parse_local_repository` makes this fail with
    /// `Declared("/path/to/local/repo")`.
    #[test]
    fn a_commented_out_local_repository_is_not_declared() {
        let shipped = r#"<settings>
  <!-- localRepository
   | The path to the local repository maven will use to store artifacts.
   | Default: ${user.home}/.m2/repository
  <localRepository>/path/to/local/repo</localRepository>
  -->
  <interactiveMode>true</interactiveMode>
</settings>"#;

        assert_eq!(
            parse_local_repository(shipped),
            LocalRepositorySetting::NotDeclared
        );
        assert_eq!(
            maven_local_repository(parse_local_repository(shipped), Path::new("/Users/dev")),
            Ok(PathBuf::from("/Users/dev/.m2/repository")),
            "the default must be used, not the commented-out example"
        );
    }

    #[test]
    fn a_real_declaration_is_honoured() {
        let xml = "<settings><localRepository>/data/m2repo</localRepository></settings>";
        assert_eq!(
            parse_local_repository(xml),
            LocalRepositorySetting::Declared("/data/m2repo".to_string())
        );
        assert_eq!(
            maven_local_repository(parse_local_repository(xml), Path::new("/Users/dev")),
            Ok(PathBuf::from("/data/m2repo"))
        );
    }

    #[test]
    fn the_user_home_property_is_expanded() {
        let xml = "<settings><localRepository>${user.home}/alt/m2</localRepository></settings>";
        assert_eq!(
            maven_local_repository(parse_local_repository(xml), Path::new("/Users/dev")),
            Ok(PathBuf::from("/Users/dev/alt/m2"))
        );
    }

    /// Whitespace and newlines around the value are Maven-legal.
    #[test]
    fn surrounding_whitespace_is_trimmed() {
        let xml =
            "<settings>\n  <localRepository>\n    /data/m2repo\n  </localRepository>\n</settings>";
        assert_eq!(
            parse_local_repository(xml),
            LocalRepositorySetting::Declared("/data/m2repo".to_string())
        );
    }

    /// Two declarations: Maven's merge order decides which wins, and this
    /// detector does not implement Maven's merge order. Saying so beats
    /// picking one.
    #[test]
    fn two_declarations_are_ambiguous_not_first_wins() {
        let xml = "<settings><localRepository>/a</localRepository>\
                   <localRepository>/b</localRepository></settings>";
        match parse_local_repository(xml) {
            LocalRepositorySetting::Ambiguous(why) => assert!(why.contains("2 times")),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_declaration_is_ambiguous_not_the_default() {
        match parse_local_repository("<settings><localRepository></localRepository></settings>") {
            LocalRepositorySetting::Ambiguous(why) => assert!(why.contains("empty")),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn an_unterminated_comment_is_ambiguous() {
        let xml = "<settings><!-- <localRepository>/a</localRepository></settings>";
        match parse_local_repository(xml) {
            LocalRepositorySetting::Ambiguous(why) => assert!(why.contains("unterminated")),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// An ambiguous settings file must reach `Failed`, never a guess and
    /// never "nothing found".
    #[test]
    fn an_ambiguous_setting_is_a_failed_probe() {
        let result = maven_local_repository(
            LocalRepositorySetting::Ambiguous("because reasons".to_string()),
            Path::new("/Users/dev"),
        );
        assert_eq!(result, Err("because reasons".to_string()));
    }

    /// A differently-named element that merely starts with the same prefix
    /// is not this element.
    #[test]
    fn a_similarly_named_element_is_not_local_repository() {
        let xml = "<settings><localRepositoryPath>/a</localRepositoryPath></settings>";
        assert_eq!(
            parse_local_repository(xml),
            LocalRepositorySetting::NotDeclared
        );
    }

    /// HORO-1543 AC 5, end to end on this detector's real output: a Maven
    /// local repository is never AUTO_SAFE, however clean everything else
    /// about it looks.
    ///
    /// Mutation control: changing the detector's `Regenerability::Unknown`
    /// to `RegenerableByTool` makes this fail with `AutoSafe`, naming the
    /// class rather than counting reasons.
    #[test]
    fn a_maven_local_repository_is_ask_not_auto_safe() {
        let home = crate::detectors::test_support::make_temp_dir("maven-home-fixture");
        let repository = home.join(".m2/repository/com/example/lib/1.0");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::write(repository.join("lib-1.0.jar"), vec![0u8; 2_048]).unwrap();

        let ctx = DiscoveryContext::new(&home);
        let mut evidence: Evidence = match MavenLocalRepositoryDetector.discover(&ctx) {
            DetectorStatus::Found(mut found) => {
                assert_eq!(found.len(), 1);
                found.remove(0)
            }
            other => panic!("expected Found, got {other:?}"),
        };

        assert_eq!(evidence.logical_bytes, ProbeOutcome::Observed(2_048));
        assert_eq!(evidence.regenerability, Regenerability::Unknown);

        // The cleanest correlation possible: nothing open, nothing's cwd,
        // no git working tree, tool idle. Only correlation is supplied here;
        // every size and reproducibility judgment is the real detector's.
        merge_into(
            &mut evidence,
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None),
                tool_liveness: ProbeOutcome::Observed(false),
            },
        );
        assert_eq!(evidence.completeness(), Completeness::Complete);

        let decision = classify(
            &evidence,
            &PolicyConfig::default(),
            evidence.collected_at + Duration::from_secs(1),
        );
        assert_eq!(
            decision.class,
            PolicyClass::Ask,
            "a local repository that may hold mvn install'ed artifacts must not be \
             auto-deletable; reasons: {:?}",
            decision.reasons
        );
        assert_eq!(decision.reasons, vec![ReasonCode::RegenerabilityUnknown]);

        std::fs::remove_dir_all(&home).ok();
    }

    /// No `~/.m2` at all reports nothing — never a zero-byte repository.
    #[test]
    fn no_maven_home_is_tool_absent_not_zero_bytes() {
        let home = crate::detectors::test_support::make_temp_dir("maven-home-empty");
        let ctx = DiscoveryContext::new(&home);
        assert_eq!(
            MavenLocalRepositoryDetector.discover(&ctx),
            DetectorStatus::ToolAbsent
        );
        std::fs::remove_dir_all(&home).ok();
    }
}
