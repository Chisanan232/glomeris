//! HORO-1629: bounded, one-level evaluation of a configured (or
//! about-to-be-configured) project root, plus the "which roots were
//! examined" scan-coverage sentence.
//!
//! ADR-0001 §11 picked option 1 (validate at configuration time) + option 2
//! (report examined roots), never option 3 (no recursive discovery as a
//! default). This module is option 1's bounded read; option 2's sentence
//! lives here too so both founder-approved strings (§16.1) have exactly one
//! producer each, byte-identical everywhere they are shown.
//!
//! The bug this closes: `cargo_target_dir`/`node_modules`/`swiftpm_build_dir`
//! only ever look exactly one level below a configured root (by contract —
//! see each detector's own module doc). A root that is itself a *parent* of
//! several projects (e.g. `~/code` holding 30 repos) therefore makes all
//! three detectors answer `Found(vec![])` — truthfully, about the paths they
//! were handed — with nothing anywhere saying that the thing examined was
//! the parent folder itself. [`check_project_root`] gives Settings a bounded
//! answer to warn on *before* that root is ever added; [`scan_coverage_line`]
//! gives `detect`/`explain`/`free` a sentence to print *after*, naming how
//! many roots were actually looked at.

use std::io;
use std::path::{Path, PathBuf};

use crate::reporting::dto::CheckProjectRootReport;

/// Founder-approved (ADR-0001 §16.1, 2026-10-10), byte-identical. Never
/// reworded — a string-equality test pins this exactly.
pub const PARENT_OF_PROJECTS_WARNING: &str =
    "This folder appears to contain multiple projects rather than being a \
     project root. Glomeris checks only the selected folder and does not \
     automatically scan nested projects. Add each project separately to \
     include its build artifacts.";

/// Founder-approved (ADR-0001 §16.1, 2026-10-10). One producer for the
/// `{count}` sentence so `detect`, `explain`, `free`'s preview and its run
/// report cannot drift into different wording for the same fact.
pub fn scan_coverage_line(examined_root_count: usize) -> String {
    format!(
        "Examined {examined_root_count} configured project root(s). Nested \
         projects were not scanned automatically."
    )
}

/// Project markers this check recognizes — the same manifest/build-output
/// names the three project-scoped detectors already key off of
/// (`cargo.rs`'s `target`, `node.rs`'s `node_modules`, `swiftpm.rs`'s
/// `Package.swift`/`.build`). Hand-written once, here, rather than
/// re-derived per call site, so this check can never recognize a project
/// shape the detectors themselves do not.
const PROJECT_MARKERS: &[&str] = &["Cargo.toml", "package.json", "Package.swift"];
const BUILD_OUTPUT_DIRS: &[&str] = &["target", "node_modules", ".build"];

/// What one bounded, one-level evaluation of a candidate project root
/// concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectRootCheck {
    /// `root` itself carries a project marker or build-output directory —
    /// it IS a project root (which does not preclude it from ALSO holding
    /// sibling members, e.g. a Cargo workspace or a JS monorepo with a
    /// top-level manifest: the root's own marker wins, so those are never
    /// misflagged). No warning.
    ProjectRoot,
    /// `root` carries no marker of its own, but at least one of its
    /// IMMEDIATE children does — the exact "added `~/code`, meant one repo
    /// under it" shape this ticket exists for. Show
    /// [`PARENT_OF_PROJECTS_WARNING`].
    ParentOfProjects {
        /// Immediate child directory names that matched, in read-dir order.
        /// Not shown in the (fixed) warning string, but useful to a caller
        /// that wants to log or test which children were seen.
        matched_children: Vec<String>,
    },
    /// `root` carries no marker of its own, and none of its immediate
    /// children carries one either — an ordinary empty or unrelated folder.
    /// There is no bounded evidence here that this is a parent-of-projects
    /// folder, so no warning.
    NoProjectsFound,
    /// The bounded one-level read could not be completed — `root` itself
    /// could not be probed, could not be listed, or at least one child
    /// could not be probed and no other child supplied a positive match.
    /// AC4: this must never be silently folded into `NoProjectsFound` (a
    /// false "clean, not a parent") or into `ProjectRoot`.
    Undetermined { reason: String },
}

/// Whether `dir` itself carries a project marker or build-output directory,
/// via `try_exists` (never `.is_file()`/`.is_dir()`, which swallow an error
/// into `false` and would silently turn an unreadable entry into "no
/// marker here" — the exact fail-open shape AC4 forbids).
fn has_marker_or_build_output(dir: &Path) -> io::Result<bool> {
    for name in PROJECT_MARKERS.iter().chain(BUILD_OUTPUT_DIRS.iter()) {
        match dir.join(name).try_exists() {
            Ok(true) => return Ok(true),
            Ok(false) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// Expands a leading `~` (home) or `~/...` in `path` against `home`,
/// mirroring the existing `expand_home` helper in
/// `src/evidence/correlate/host_dependency.rs` — the same narrow
/// transform, not full shell globbing, and deliberately not `~user` (which
/// this process cannot resolve without a passwd lookup, and which the
/// Swift-side `expandingTilde` added for HORO-1629 round 2 also declines
/// to expand, so the two surfaces agree on the same literal input).
/// Anything else, including a path that is merely prefixed by `~` without
/// a following `/` (e.g. `~backup`), passes through unchanged.
///
/// HORO-1629 round-2 finding: `glomeris check-project-root ~/code` — this
/// command's own documented example (`src/cli/help.rs`) — reaches this
/// binary with `~/code` literal when nothing upstream is a shell (exactly
/// how `GlomerisMenuBar`'s `GlomerisClient` spawns it, via
/// `Process.executableURL` + `arguments`, never a shell interpreter).
/// `std::fs` does not expand `~` either, so without this the bounded
/// one-level read in [`check_project_root`] was probing a path that could
/// not exist, and silently, correctly, failing closed to `Undetermined`
/// — technically correct but reachable for the exact scenario this
/// feature exists to catch.
pub fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// Bounded, one-level evaluation of `root` (ADR-0001 §11/§16.1, option 1).
///
/// Exactly two filesystem operations deep: `root`'s own marker/build-output
/// probe, then one `read_dir(root)` plus one marker/build-output probe per
/// immediate child. Never `read_dir`s a child — a marker two levels down
/// (a grandchild) is structurally unreachable from here, which is what
/// keeps this bounded to "one level below a path the user chose" (AC3).
///
/// `root`'s own check runs first and, if positive, short-circuits straight
/// to [`ProjectRootCheck::ProjectRoot`] without even listing children: a
/// root that is itself a project must never be flagged as a parent of
/// projects, regardless of what else happens to sit beside its manifest
/// (a Cargo workspace's member crates, a JS monorepo's packages).
///
/// Every failure mode fails closed to [`ProjectRootCheck::Undetermined`]
/// (AC4): `root`'s own probe erroring, `read_dir(root)` erroring (other
/// than a plain "does not exist"), a `DirEntry` erroring during iteration,
/// or a child's own marker probe erroring with no other child supplying a
/// positive match. A positive match found despite some other child's probe
/// having errored still reports `ParentOfProjects` — the positive evidence
/// is real regardless of what a different, unrelated child could not
/// answer about itself.
pub fn check_project_root(root: &Path) -> ProjectRootCheck {
    match has_marker_or_build_output(root) {
        Ok(true) => return ProjectRootCheck::ProjectRoot,
        Ok(false) => {}
        Err(e) => {
            return ProjectRootCheck::Undetermined {
                reason: format!("could not check {}: {e}", root.display()),
            };
        }
    }

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            return ProjectRootCheck::Undetermined {
                reason: format!("could not read {}: {e}", root.display()),
            };
        }
    };

    let mut matched_children: Vec<String> = Vec::new();
    let mut saw_error = false;
    let mut last_error: Option<String> = None;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                saw_error = true;
                last_error = Some(e.to_string());
                continue;
            }
        };
        let child: PathBuf = entry.path();
        match has_marker_or_build_output(&child) {
            Ok(true) => matched_children.push(entry.file_name().to_string_lossy().into_owned()),
            Ok(false) => {}
            Err(e) => {
                saw_error = true;
                last_error = Some(format!("could not check {}: {e}", child.display()));
            }
        }
    }

    if !matched_children.is_empty() {
        return ProjectRootCheck::ParentOfProjects { matched_children };
    }
    if saw_error {
        return ProjectRootCheck::Undetermined {
            reason: last_error.unwrap_or_else(|| {
                format!(
                    "one or more entries under {} could not be read",
                    root.display()
                )
            }),
        };
    }
    ProjectRootCheck::NoProjectsFound
}

impl ProjectRootCheck {
    /// The tag every serialized surface uses for this check's outcome
    /// (mirrors [`super::DetectorOutcome::tag`]'s one-producer convention).
    pub fn tag(&self) -> &'static str {
        match self {
            ProjectRootCheck::ProjectRoot => "project_root",
            ProjectRootCheck::ParentOfProjects { .. } => "parent_of_projects",
            ProjectRootCheck::NoProjectsFound => "no_projects_found",
            ProjectRootCheck::Undetermined { .. } => "undetermined",
        }
    }
}

/// Builds a [`CheckProjectRootReport`] for `path` — the single place that
/// decides which fields are populated for which [`ProjectRootCheck`]
/// outcome, so `--json` and a future Swift caller cannot disagree with
/// each other about when the warning appears.
pub fn build_check_project_root_report(path: &Path) -> CheckProjectRootReport {
    let check = check_project_root(path);
    let status = check.tag();
    let (warning, matched_children, reason) = match check {
        ProjectRootCheck::ProjectRoot | ProjectRootCheck::NoProjectsFound => {
            (None, Vec::new(), None)
        }
        ProjectRootCheck::ParentOfProjects { matched_children } => {
            (Some(PARENT_OF_PROJECTS_WARNING), matched_children, None)
        }
        ProjectRootCheck::Undetermined { reason } => (None, Vec::new(), Some(reason)),
    };
    CheckProjectRootReport {
        path: path.to_string_lossy().into_owned(),
        status,
        warning,
        matched_children,
        reason,
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

    /// HORO-1629 round-2 regression: the ticket's own worked example,
    /// `check-project-root ~/code`, must resolve `~/code` against the
    /// supplied home directory before it is ever probed.
    #[test]
    fn expand_tilde_resolves_bare_tilde_and_tilde_slash_against_home() {
        let home = PathBuf::from("/Users/dev");

        assert_eq!(expand_tilde("~", &home), home);
        assert_eq!(expand_tilde("~/code", &home), home.join("code"));
        assert_eq!(expand_tilde("~/a/b", &home), home.join("a/b"));
    }

    /// Everything else passes through unchanged — in particular `~user`,
    /// which this process cannot resolve without a passwd lookup, and
    /// which the Swift-side `expandingTilde` (HORO-1629 round 2) also
    /// declines to expand, so the two surfaces never disagree about the
    /// same literal input.
    #[test]
    fn expand_tilde_leaves_non_home_relative_paths_unchanged() {
        let home = PathBuf::from("/Users/dev");

        for path in [
            "/Users/dev/code",
            "relative/not/absolute",
            "~user/code",
            "~backup",
            "",
        ] {
            assert_eq!(
                expand_tilde(path, &home),
                PathBuf::from(path),
                "{path} should not have been rewritten"
            );
        }
    }

    /// AC5: the approved strings, byte-for-byte. A fuzzy/contains check
    /// here would have let a rewording slip past review unnoticed.
    #[test]
    fn warning_and_scan_coverage_wording_are_byte_identical_to_adr_0001_section_16_1() {
        assert_eq!(
            PARENT_OF_PROJECTS_WARNING,
            "This folder appears to contain multiple projects rather than being a \
             project root. Glomeris checks only the selected folder and does not \
             automatically scan nested projects. Add each project separately to \
             include its build artifacts."
        );
        assert_eq!(
            scan_coverage_line(3),
            "Examined 3 configured project root(s). Nested projects were not scanned \
             automatically."
        );
        assert_eq!(
            scan_coverage_line(1),
            "Examined 1 configured project root(s). Nested projects were not scanned \
             automatically."
        );
    }

    /// The exact bug scenario: a parent-of-projects root holding one real
    /// project (with build output) one level down.
    #[test]
    fn a_parent_of_two_project_children_is_flagged() {
        let root = make_temp_dir("parent-of-projects");
        let repo_a = root.join("repo-a");
        fs::create_dir_all(repo_a.join("target")).unwrap();
        fs::write(repo_a.join("Cargo.toml"), "[package]\n").unwrap();
        let repo_b = root.join("repo-b");
        fs::create_dir_all(&repo_b).unwrap();
        fs::write(repo_b.join("package.json"), "{}").unwrap();
        // An ordinary non-project sibling must not suppress the warning.
        fs::create_dir_all(root.join("Documents")).unwrap();

        match check_project_root(&root) {
            ProjectRootCheck::ParentOfProjects {
                mut matched_children,
            } => {
                matched_children.sort();
                assert_eq!(matched_children, vec!["repo-a", "repo-b"]);
            }
            other => panic!("expected ParentOfProjects, got {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }

    /// AC2 precedent: a root that IS itself a project (its own `Cargo.toml`)
    /// must never be flagged, even though it also has sibling directories
    /// that look like projects (e.g. workspace members) — the false
    /// positive a reviewer will probe first.
    #[test]
    fn a_root_that_is_itself_a_project_is_never_flagged_even_with_project_shaped_siblings() {
        let root = make_temp_dir("own-project-with-members");
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let member = root.join("crates/member-one");
        fs::create_dir_all(&member).unwrap();
        fs::write(member.join("Cargo.toml"), "[package]\n").unwrap();

        assert_eq!(check_project_root(&root), ProjectRootCheck::ProjectRoot);

        fs::remove_dir_all(&root).ok();
    }

    /// An ordinary folder with no markers anywhere in one level: no bounded
    /// evidence of a parent-of-projects shape, so no warning.
    #[test]
    fn an_ordinary_empty_folder_reports_no_projects_found() {
        let root = make_temp_dir("ordinary-empty");
        fs::create_dir_all(root.join("Documents")).unwrap();
        fs::create_dir_all(root.join("Photos")).unwrap();

        assert_eq!(check_project_root(&root), ProjectRootCheck::NoProjectsFound);

        fs::remove_dir_all(&root).ok();
    }

    /// AC3: bounded to exactly one level. A marker on a GRANDCHILD (not an
    /// immediate child) must never be reached, and must never itself
    /// trigger the warning — only an immediate child's own marker may.
    #[test]
    fn a_marker_two_levels_down_is_never_reached_and_never_warns() {
        let root = make_temp_dir("grandchild-marker-only");
        let grandchild = root.join("a/b");
        fs::create_dir_all(&grandchild).unwrap();
        fs::write(grandchild.join("Cargo.toml"), "[package]\n").unwrap();

        assert_eq!(check_project_root(&root), ProjectRootCheck::NoProjectsFound);

        fs::remove_dir_all(&root).ok();
    }

    /// AC4 (the critical one): a root that cannot be read at all must
    /// report as incomplete, never as "clean, not a parent".
    #[test]
    fn an_unreadable_root_is_undetermined_never_clean() {
        let root = make_temp_dir("unreadable-root");
        let locked = root.join("locked");
        fs::create_dir_all(&locked).unwrap();
        let mut perms = fs::metadata(&locked).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
        fs::set_permissions(&locked, perms.clone()).unwrap();

        // Sanity: the permission change actually broke reading, so this
        // test does not pass vacuously when run as root.
        let actually_blocked = fs::read_dir(&locked).is_err();

        let result = check_project_root(&locked);

        // Restore permissions before cleanup, whatever the outcome.
        perms.set_mode(0o755);
        fs::set_permissions(&locked, perms).ok();
        fs::remove_dir_all(&root).ok();

        if actually_blocked {
            match result {
                ProjectRootCheck::Undetermined { .. } => {}
                other => panic!(
                    "expected Undetermined for an unreadable root, got {other:?} \
                     (this is the AC4 fail-open class this ticket closes)"
                ),
            }
        }
    }

    /// AC4, the other half: a child whose own marker probe errors (and no
    /// OTHER child supplies a positive match) must leave the whole
    /// evaluation `Undetermined`, never silently `NoProjectsFound`.
    #[test]
    fn a_child_whose_marker_probe_errors_is_undetermined_when_no_other_child_matches() {
        let root = make_temp_dir("child-probe-errors");
        let locked_child = root.join("locked-child");
        fs::create_dir_all(&locked_child).unwrap();
        let mut perms = fs::metadata(&locked_child).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
        fs::set_permissions(&locked_child, perms.clone()).unwrap();

        // Sanity: stat'ing a marker inside the locked child must actually
        // fail with something other than "not found" for this test to mean
        // anything (root-run processes bypass the mode bits entirely).
        let probe_blocked = locked_child.join("Cargo.toml").try_exists().is_err();

        let result = check_project_root(&root);

        perms.set_mode(0o755);
        fs::set_permissions(&locked_child, perms).ok();
        fs::remove_dir_all(&root).ok();

        if probe_blocked {
            match result {
                ProjectRootCheck::Undetermined { .. } => {}
                other => panic!(
                    "expected Undetermined when a child's own marker probe errors \
                     and nothing else matched, got {other:?}"
                ),
            }
        }
    }

    /// A child probe error is NOT fatal when a different child independently
    /// supplies a real positive match — the positive evidence stands.
    #[test]
    fn a_positive_match_survives_an_unrelated_sibling_probe_error() {
        let root = make_temp_dir("sibling-error-survives");
        let repo = root.join("repo-a");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("Cargo.toml"), "[package]\n").unwrap();

        let locked_child = root.join("locked-sibling");
        fs::create_dir_all(&locked_child).unwrap();
        let mut perms = fs::metadata(&locked_child).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
        fs::set_permissions(&locked_child, perms.clone()).unwrap();

        let result = check_project_root(&root);

        perms.set_mode(0o755);
        fs::set_permissions(&locked_child, perms).ok();
        fs::remove_dir_all(&root).ok();

        match result {
            ProjectRootCheck::ParentOfProjects { matched_children } => {
                assert!(matched_children.contains(&"repo-a".to_string()));
            }
            ProjectRootCheck::Undetermined { .. } => {
                // Acceptable only if the permission change had no effect in
                // this sandbox (e.g. running as root) — but then the
                // positive match should still have won. Treat as a hard
                // failure to keep this test meaningful.
                panic!(
                    "a real positive match must survive an unrelated sibling's probe \
                     error, got Undetermined"
                );
            }
            other => panic!("expected ParentOfProjects, got {other:?}"),
        }
    }

    /// AC4: a nonexistent root (the folder vanished, or was never there)
    /// reports as undetermined rather than as a silent "no projects found".
    #[test]
    fn a_nonexistent_root_is_undetermined() {
        let root = make_temp_dir("will-not-exist");
        fs::remove_dir_all(&root).unwrap();

        match check_project_root(&root) {
            ProjectRootCheck::Undetermined { .. } => {}
            other => panic!("expected Undetermined for a nonexistent root, got {other:?}"),
        }
    }
}
