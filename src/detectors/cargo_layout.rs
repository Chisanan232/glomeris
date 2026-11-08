//! Report-only breakdown of a Cargo target directory (HORO-2360 slice C).
//!
//! A shared `target/` of several hundred GiB is one number to `detect`; this
//! module shows *where* it is — per profile `incremental`, `deps`, `build`,
//! `.fingerprint`, per target triple, `llvm-cov-target`, `doc`, … — with each
//! unit's logical bytes, allocated bytes and newest mtime.
//!
//! Safety contract (ADR-0001 §19/§20): the survey is **never** an input to
//! `classify`, the planner or the executor, and grants nothing. It is not
//! stored on [`crate::evidence::Evidence`]; it is computed on demand by
//! `explain` for one already-discovered candidate and rendered beside it. A
//! unit's name (for example `incremental`) is a label for the reader, not a
//! deletion handle — there is no per-unit action.
//!
//! Honesty rules:
//! - Every unit is measured with the same bounded walk as the whole-target
//!   figure, so a unit that hit its budget is flagged `is_lower_bound`.
//! - A total deadline bounds the survey. Units not reached are listed with
//!   `measured: false` and no bytes — never dropped, never extrapolated.
//! - Hardlinks are deduplicated *within* a unit only. Cargo hardlinks final
//!   artifacts between `<profile>/` and `<profile>/deps/`, so a COMPLETE
//!   survey's unit sum bounds the units' content from above
//!   (`sum_is_upper_bound`). It never includes the blocks of the container
//!   directories themselves, and an incomplete survey claims no bound at all.
//!
//! Symlink swaps: the survey never follows a symlink it sees. Every directory
//! the walk lists, in a unit or in the container listings, must still resolve
//! to where the survey found it (see [`Anchor`]) both before and after its
//! listing, and so must the unit after its measurement. Anything that moved
//! is `"changed"` and unmeasured, never a figure for another tree. Known
//! limit: the checks are path resolutions, not open directory handles, so a
//! swap that is made and undone between a check and the listing it guards is
//! not detected. The survey reads and deletes nothing, so that residual
//! effect is a wrong figure for a directory someone is replacing, with write
//! access to the build directory, while it is measured.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::{estimate_logical_bytes_within, size_estimate_budget};

/// Total wall-clock budget for one survey.
pub(crate) const LAYOUT_DEADLINE: Duration = Duration::from_secs(15);
/// Cap on listed units; the remainder is counted in `omitted_units`.
const MAX_UNITS: usize = 96;
/// Directory depth below the target dir the layout recursion may reach
/// (`<triple>/<profile>/<sub-unit>` is depth 3).
const MAX_DEPTH: usize = 4;
/// Directory entries read from any one directory while listing units, and
/// loose files summed into one unit. A directory with more is reported as
/// truncated rather than read to the end.
const MAX_ENTRIES_PER_DIR: usize = 20_000;

/// What a unit is, by its conventional Cargo name. A reader's label only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutRole {
    Incremental,
    Deps,
    Build,
    Fingerprint,
    Examples,
    Doc,
    Package,
    Tmp,
    /// Loose files directly inside a profile or target directory.
    LooseFiles,
    Other,
}

impl LayoutRole {
    fn of_dir(name: &str) -> Self {
        match name {
            "incremental" => Self::Incremental,
            "deps" => Self::Deps,
            "build" => Self::Build,
            ".fingerprint" => Self::Fingerprint,
            "examples" => Self::Examples,
            "doc" => Self::Doc,
            "package" => Self::Package,
            "tmp" => Self::Tmp,
            _ => Self::Other,
        }
    }
}

/// One measured (or deliberately unmeasured) slice of the target directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayoutUnit {
    /// Path relative to the target directory, `/`-separated.
    pub path: String,
    pub role: LayoutRole,
    pub profile: Option<String>,
    pub triple: Option<String>,
    /// Inside `llvm-cov-target/`.
    pub llvm_cov: bool,
    /// `false` when the unit was not measured; `unmeasured_reason` says why.
    pub measured: bool,
    /// `"deadline"` (survey time ran out before this unit), `"changed"` (the
    /// directory no longer resolved to where it was found) or `"unreadable"`
    /// (the unit's own probe failed). `None` when measured.
    pub unmeasured_reason: Option<&'static str>,
    pub logical_bytes: Option<u64>,
    /// On-disk blocks: an upper bound on what deleting this unit frees.
    pub allocated_bytes: Option<u64>,
    pub newest_mtime_unix: Option<u64>,
    /// The unit's walk stopped on its budget, so the figures are lower bounds.
    pub is_lower_bound: bool,
}

/// The whole survey for one target directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CargoLayoutSurvey {
    /// Largest allocated first; unmeasured units last, by path.
    pub units: Vec<LayoutUnit>,
    /// Units found beyond the listing cap (not measured, not listed).
    pub omitted_units: usize,
    pub unmeasured_units: usize,
    pub measured_logical_bytes: u64,
    pub measured_allocated_bytes: u64,
    /// `true` only when `complete`: cross-unit hardlinks are counted in each
    /// unit, so a complete survey's sum bounds the units' content from above.
    /// The blocks of the container directories themselves are not included.
    /// An incomplete survey's sum is a partial figure, so this is `false`.
    pub sum_is_upper_bound: bool,
    /// Listing stopped on the deadline or an entry cap, so units may be missing.
    pub listing_truncated: bool,
    /// `true` only when every unit was measured and none hit its budget.
    pub complete: bool,
}

struct UnitSpec {
    abs: PathBuf,
    rel: String,
    role: LayoutRole,
    profile: Option<String>,
    triple: Option<String>,
    llvm_cov: bool,
    /// `Some(names)` for a loose-files group; the files are summed directly.
    loose_files: Option<Vec<PathBuf>>,
    /// The directory listing that produced `loose_files` was cut short, so the
    /// group's figures are lower bounds.
    files_partial: bool,
}

/// Shared bound for the listing phase: the same deadline as measurement, so
/// a huge fan-out cannot run unmetered before the first unit is measured.
struct Listing {
    started: Instant,
    deadline: Duration,
    truncated: bool,
    anchor: Anchor,
}

/// Where the survey started, resolved once. A path is only trusted while it
/// still resolves to the root's real location plus its own relative path, so a
/// component replaced by a symlink after discovery is noticed.
struct Anchor {
    root: PathBuf,
    canonical: Option<PathBuf>,
}

impl Anchor {
    fn new(root: &Path) -> Self {
        Anchor {
            root: root.to_path_buf(),
            canonical: fs::canonicalize(root).ok(),
        }
    }

    fn holds(&self, path: &Path) -> bool {
        let (Some(canonical), Ok(rel)) = (&self.canonical, path.strip_prefix(&self.root)) else {
            return false;
        };
        fs::canonicalize(path).is_ok_and(|resolved| resolved == canonical.join(rel))
    }
}

impl Listing {
    fn expired(&self) -> bool {
        self.started.elapsed() >= self.deadline
    }
}

#[derive(Clone, Default)]
struct Ctx {
    profile: Option<String>,
    triple: Option<String>,
    llvm_cov: bool,
}

/// Surveys `target_dir`. Never fails: an unreadable directory yields an
/// empty, `complete: false` survey.
pub fn survey_cargo_layout(target_dir: &Path) -> CargoLayoutSurvey {
    survey_with_deadline(target_dir, LAYOUT_DEADLINE)
}

pub(crate) fn survey_with_deadline(target_dir: &Path, deadline: Duration) -> CargoLayoutSurvey {
    let started = Instant::now();
    let mut listing = Listing {
        started,
        deadline,
        truncated: false,
        anchor: Anchor::new(target_dir),
    };
    // A symlinked root is never followed: the survey describes the directory
    // that was discovered, not wherever a link currently points.
    if fs::symlink_metadata(target_dir).is_ok_and(|m| m.file_type().is_symlink()) {
        listing.truncated = true;
    }
    let mut specs = Vec::new();
    let mut omitted = 0usize;
    if !listing.truncated {
        enumerate(
            target_dir,
            "",
            &Ctx::default(),
            false,
            0,
            &mut specs,
            &mut omitted,
            &mut listing,
        );
    }

    let mut units = Vec::with_capacity(specs.len());
    for spec in specs {
        let expired = started.elapsed() >= deadline;
        units.push(measure(spec, expired, started, deadline, &listing.anchor));
    }

    let measured_logical_bytes = units.iter().filter_map(|u| u.logical_bytes).sum();
    let measured_allocated_bytes = units.iter().filter_map(|u| u.allocated_bytes).sum();
    let unmeasured_units = units.iter().filter(|u| !u.measured).count();
    let complete = omitted == 0
        && !listing.truncated
        && unmeasured_units == 0
        && units.iter().all(|u| !u.is_lower_bound);
    units.sort_by(|a, b| {
        b.measured
            .cmp(&a.measured)
            .then(b.allocated_bytes.cmp(&a.allocated_bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
    CargoLayoutSurvey {
        units,
        omitted_units: omitted,
        unmeasured_units,
        measured_logical_bytes,
        measured_allocated_bytes,
        sum_is_upper_bound: complete,
        listing_truncated: listing.truncated,
        complete,
    }
}

/// Directories, loose files, and whether this listing was cut short.
type ChildListing = (Vec<(String, PathBuf)>, Vec<PathBuf>, bool);

fn child_dirs_and_files(dir: &Path, listing: &mut Listing) -> Option<ChildListing> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut partial = false;
    if !listing.anchor.holds(dir) {
        return None;
    }
    for (seen, entry) in fs::read_dir(dir).ok()?.enumerate() {
        if seen >= MAX_ENTRIES_PER_DIR || listing.expired() {
            listing.truncated = true;
            partial = true;
            break;
        }
        let Ok(entry) = entry else {
            listing.truncated = true;
            partial = true;
            continue;
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // A symlink is never followed: it is a loose file whose own lstat
        // size is counted, whatever it points at.
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => dirs.push((name, path)),
            Ok(_) => files.push(path),
            Err(_) => {
                listing.truncated = true;
                partial = true;
            }
        }
    }
    // Swapped for a symlink while it was being listed: what was read may
    // belong to another tree.
    if !listing.anchor.holds(dir) {
        return None;
    }
    dirs.sort();
    files.sort();
    Some((dirs, files, partial))
}

/// A Cargo profile directory holds the build products for one profile.
fn is_profile_dir(dir: &Path) -> bool {
    [".fingerprint", "deps", "incremental", "build"]
        .iter()
        .any(|n| fs::symlink_metadata(dir.join(n)).is_ok_and(|m| m.is_dir()))
}

/// A target-triple directory holds profile directories (cross builds).
fn holds_profiles(dir: &Path, listing: &mut Listing) -> bool {
    let Some((dirs, _, _)) = child_dirs_and_files(dir, listing) else {
        return false;
    };
    for (_, path) in &dirs {
        // Each probe is up to four metadata calls; they answer to the survey's
        // deadline like the listing does.
        if listing.expired() {
            listing.truncated = true;
            return false;
        }
        if is_profile_dir(path) {
            return true;
        }
    }
    false
}

fn join_rel(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}/{name}")
    }
}

fn push(specs: &mut Vec<UnitSpec>, omitted: &mut usize, spec: UnitSpec) {
    if specs.len() >= MAX_UNITS {
        *omitted += 1;
    } else {
        specs.push(spec);
    }
}

// Eight parameters: the walk threads its shared accumulators (specs, omitted,
// listing) through the recursion rather than allocating per level.
#[allow(clippy::too_many_arguments)]
fn enumerate(
    dir: &Path,
    rel: &str,
    ctx: &Ctx,
    in_profile: bool,
    depth: usize,
    specs: &mut Vec<UnitSpec>,
    omitted: &mut usize,
    listing: &mut Listing,
) {
    let Some((dirs, files, files_partial)) = child_dirs_and_files(dir, listing) else {
        // A directory that cannot be listed is a hole in the survey, not an
        // empty directory.
        listing.truncated = true;
        return;
    };
    for (name, path) in dirs {
        let child_rel = join_rel(rel, &name);
        // Once the cap or deadline is hit, stop probing and recursing; every
        // remaining directory is still listed as a plain unit (or counted as
        // omitted), never silently dropped.
        let probing = specs.len() < MAX_UNITS && !listing.expired();
        if !probing && !in_profile {
            listing.truncated = true;
        }
        if in_profile || !probing {
            push(
                specs,
                omitted,
                UnitSpec {
                    abs: path,
                    rel: child_rel,
                    role: LayoutRole::of_dir(&name),
                    profile: ctx.profile.clone(),
                    triple: ctx.triple.clone(),
                    llvm_cov: ctx.llvm_cov,
                    loose_files: None,
                    files_partial: false,
                },
            );
        } else if depth < MAX_DEPTH && name == "llvm-cov-target" {
            let mut next = ctx.clone();
            next.llvm_cov = true;
            enumerate(
                &path,
                &child_rel,
                &next,
                false,
                depth + 1,
                specs,
                omitted,
                listing,
            );
        } else if depth < MAX_DEPTH && is_profile_dir(&path) {
            let mut next = ctx.clone();
            next.profile = Some(name.clone());
            enumerate(
                &path,
                &child_rel,
                &next,
                true,
                depth + 1,
                specs,
                omitted,
                listing,
            );
        } else if depth < MAX_DEPTH && ctx.triple.is_none() && holds_profiles(&path, listing) {
            let mut next = ctx.clone();
            next.triple = Some(name.clone());
            enumerate(
                &path,
                &child_rel,
                &next,
                false,
                depth + 1,
                specs,
                omitted,
                listing,
            );
        } else {
            push(
                specs,
                omitted,
                UnitSpec {
                    abs: path,
                    rel: child_rel,
                    role: LayoutRole::of_dir(&name),
                    profile: ctx.profile.clone(),
                    triple: ctx.triple.clone(),
                    llvm_cov: ctx.llvm_cov,
                    loose_files: None,
                    files_partial: false,
                },
            );
        }
    }
    if !files.is_empty() {
        let label = if rel.is_empty() {
            "(files)".to_string()
        } else {
            format!("{rel}/(files)")
        };
        push(
            specs,
            omitted,
            UnitSpec {
                abs: dir.to_path_buf(),
                rel: label,
                role: LayoutRole::LooseFiles,
                profile: ctx.profile.clone(),
                triple: ctx.triple.clone(),
                llvm_cov: ctx.llvm_cov,
                loose_files: Some(files),
                files_partial,
            },
        );
    }
}

fn unix_secs(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn unmeasured(spec: UnitSpec, reason: &'static str) -> LayoutUnit {
    LayoutUnit {
        unmeasured_reason: Some(reason),
        path: spec.rel,
        role: spec.role,
        profile: spec.profile,
        triple: spec.triple,
        llvm_cov: spec.llvm_cov,
        measured: false,
        logical_bytes: None,
        allocated_bytes: None,
        newest_mtime_unix: None,
        is_lower_bound: false,
    }
}

fn measure(
    spec: UnitSpec,
    deadline_passed: bool,
    started: Instant,
    deadline: Duration,
    anchor: &Anchor,
) -> LayoutUnit {
    if deadline_passed {
        return unmeasured(spec, "deadline");
    }
    if !anchor.holds(&spec.abs) {
        return unmeasured(spec, "changed");
    }
    let abs = spec.abs.clone();
    let unit = measure_unit(spec, started, deadline, anchor);
    if unit.measured && !anchor.holds(&abs) {
        return LayoutUnit {
            unmeasured_reason: Some("changed"),
            measured: false,
            logical_bytes: None,
            allocated_bytes: None,
            newest_mtime_unix: None,
            is_lower_bound: false,
            ..unit
        };
    }
    unit
}

fn measure_unit(
    spec: UnitSpec,
    started: Instant,
    deadline: Duration,
    anchor: &Anchor,
) -> LayoutUnit {
    if let Some(files) = &spec.loose_files {
        return measure_loose_files(&spec, files, started, deadline);
    }
    // The walk gets whichever is smaller: its usual bound or what is left of
    // the survey's time.
    let remaining = deadline.saturating_sub(started.elapsed());
    // Every directory the walk lists must still resolve inside the root, so a
    // nested directory swapped for a symlink mid-walk cannot pull outside
    // bytes into the sum (or restore a larger tree after a smaller one).
    let escaped = std::cell::Cell::new(false);
    let est = estimate_logical_bytes_within(
        &spec.abs,
        size_estimate_budget().with_max_duration(remaining.min(super::SIZE_ESTIMATE_DEADLINE)),
        &|dir| {
            let held = anchor.holds(dir);
            if !held {
                escaped.set(true);
            }
            held
        },
    );
    if escaped.get() {
        return unmeasured(spec, "changed");
    }
    let Some(logical) = est.bytes.observed().copied() else {
        return unmeasured(spec, "unreadable");
    };
    LayoutUnit {
        unmeasured_reason: None,
        is_lower_bound: est.is_lower_bound(),
        allocated_bytes: est.allocated(),
        newest_mtime_unix: est.newest_mtime.and_then(unix_secs),
        logical_bytes: Some(logical),
        measured: true,
        path: spec.rel,
        role: spec.role,
        profile: spec.profile,
        triple: spec.triple,
        llvm_cov: spec.llvm_cov,
    }
}

fn measure_loose_files(
    spec: &UnitSpec,
    files: &[PathBuf],
    started: Instant,
    deadline: Duration,
) -> LayoutUnit {
    let mut logical = 0u64;
    let mut allocated = 0u64;
    let mut newest: Option<SystemTime> = None;
    #[cfg(unix)]
    let mut seen = std::collections::HashSet::new();
    let mut unreadable = false;
    for f in files {
        if started.elapsed() >= deadline {
            unreadable = true;
            break;
        }
        let Ok(meta) = fs::symlink_metadata(f) else {
            unreadable = true;
            continue;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() > 1 && !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
            allocated = allocated.saturating_add(meta.blocks().saturating_mul(512));
        }
        logical = logical.saturating_add(meta.len());
        if let Ok(m) = meta.modified() {
            newest = Some(newest.map_or(m, |n| n.max(m)));
        }
    }
    LayoutUnit {
        path: spec.rel.clone(),
        role: spec.role,
        profile: spec.profile.clone(),
        triple: spec.triple.clone(),
        llvm_cov: spec.llvm_cov,
        measured: true,
        unmeasured_reason: None,
        logical_bytes: Some(logical),
        allocated_bytes: cfg!(unix).then_some(allocated),
        newest_mtime_unix: newest.and_then(unix_secs),
        is_lower_bound: unreadable || spec.files_partial,
    }
}

#[cfg(test)]
mod tests {
    use super::super::estimate_logical_bytes;
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-cargo-layout-{prefix}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn file(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; len]).unwrap();
    }

    /// A synthetic shared target: two profiles, a cross triple, llvm-cov.
    fn shared_target() -> PathBuf {
        let t = temp_dir("shared");
        file(&t.join("debug/incremental/a/x.bin"), 5000);
        file(&t.join("debug/deps/dep.rlib"), 3000);
        file(&t.join("debug/build/b/out"), 1000);
        file(&t.join("debug/.fingerprint/f/fp"), 100);
        file(&t.join("debug/app"), 200);
        file(&t.join("release/deps/r.rlib"), 2000);
        file(
            &t.join("aarch64-apple-darwin/debug/incremental/i/y.bin"),
            4000,
        );
        file(&t.join("aarch64-apple-darwin/debug/deps/d.rlib"), 600);
        file(&t.join("llvm-cov-target/debug/deps/c.rlib"), 800);
        file(&t.join("doc/index.html"), 50);
        file(&t.join(".rustc_info.json"), 10);
        t
    }

    fn find<'a>(s: &'a CargoLayoutSurvey, path: &str) -> &'a LayoutUnit {
        s.units
            .iter()
            .find(|u| u.path == path)
            .unwrap_or_else(|| panic!("no unit {path}: {:?}", s.units))
    }

    #[test]
    fn breaks_a_shared_target_into_profile_triple_and_llvm_cov_units() {
        let t = shared_target();
        let s = survey_cargo_layout(&t);

        assert_eq!(find(&s, "debug/incremental").role, LayoutRole::Incremental);
        assert_eq!(find(&s, "debug/incremental").logical_bytes, Some(5000));
        assert_eq!(
            find(&s, "debug/incremental").profile.as_deref(),
            Some("debug")
        );
        assert_eq!(find(&s, "debug/deps").logical_bytes, Some(3000));
        assert_eq!(find(&s, "debug/build").role, LayoutRole::Build);
        assert_eq!(find(&s, "debug/.fingerprint").role, LayoutRole::Fingerprint);
        assert_eq!(find(&s, "debug/(files)").role, LayoutRole::LooseFiles);
        assert_eq!(find(&s, "release/deps").profile.as_deref(), Some("release"));

        let tri = find(&s, "aarch64-apple-darwin/debug/incremental");
        assert_eq!(tri.triple.as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(tri.logical_bytes, Some(4000));

        let cov = find(&s, "llvm-cov-target/debug/deps");
        assert!(cov.llvm_cov);
        assert_eq!(cov.logical_bytes, Some(800));

        assert_eq!(find(&s, "doc").role, LayoutRole::Doc);
        assert_eq!(find(&s, "(files)").role, LayoutRole::LooseFiles);

        // Units partition the tree: their logical sum is the whole.
        let whole = estimate_logical_bytes(&t, crate::scanner::ScanBudget::unlimited()).bytes;
        assert_eq!(whole.observed().copied(), Some(s.measured_logical_bytes));
        assert!(s.complete);
        assert!(s.sum_is_upper_bound);
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn units_are_ordered_largest_allocated_first() {
        let t = shared_target();
        let s = survey_cargo_layout(&t);
        let alloc: Vec<_> = s.units.iter().filter_map(|u| u.allocated_bytes).collect();
        let mut sorted = alloc.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(alloc, sorted);
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn newest_mtime_is_reported_per_unit() {
        let t = shared_target();
        let s = survey_cargo_layout(&t);
        let m = find(&s, "debug/incremental").newest_mtime_unix;
        assert!(m.is_some_and(|secs| secs > 1_500_000_000));
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn expired_deadline_marks_the_listing_truncated_and_measures_nothing() {
        let t = shared_target();
        let s = survey_with_deadline(&t, Duration::ZERO);

        assert!(!s.complete);
        assert!(s.listing_truncated);
        assert_eq!(s.measured_logical_bytes, 0);
        assert!(s
            .units
            .iter()
            .all(|u| !u.measured && u.logical_bytes.is_none() && u.allocated_bytes.is_none()));
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn a_unit_reached_after_the_deadline_is_listed_unmeasured() {
        let t = shared_target();
        let spec = UnitSpec {
            abs: t.join("debug/incremental"),
            rel: "debug/incremental".to_string(),
            role: LayoutRole::Incremental,
            profile: Some("debug".to_string()),
            triple: None,
            llvm_cov: false,
            loose_files: None,
            files_partial: false,
        };
        let anchor = Anchor::new(&t);
        let unit = measure(spec, true, Instant::now(), Duration::ZERO, &anchor);
        assert!(!unit.measured);
        assert_eq!(unit.logical_bytes, None);
        assert_eq!(unit.path, "debug/incremental");
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn unit_listing_is_capped_and_the_rest_counted() {
        let t = temp_dir("many");
        for i in 0..(MAX_UNITS + 5) {
            file(&t.join(format!("d{i:03}/f")), 1);
        }
        let s = survey_cargo_layout(&t);
        assert_eq!(s.units.len(), MAX_UNITS);
        assert_eq!(s.omitted_units, 5);
        assert!(!s.complete);
        fs::remove_dir_all(&t).ok();
    }

    #[test]
    fn unreadable_root_yields_an_empty_incomplete_listing_not_a_panic() {
        let s = survey_cargo_layout(Path::new("/definitely/not/a/real/target"));
        assert!(s.units.is_empty());
        assert!(!s.complete, "a missing root is not a complete empty survey");
        assert!(s.listing_truncated);
        assert!(
            !s.sum_is_upper_bound,
            "an incomplete survey's zero is not a bound on anything"
        );
    }

    /// A loose-file group built from a cut-short listing is a lower bound even
    /// though every file in it was read.
    #[test]
    fn a_loose_file_group_from_a_truncated_listing_is_a_lower_bound() {
        let t = temp_dir("partial-files");
        file(&t.join("a"), 10);
        let spec = |files_partial| UnitSpec {
            abs: t.clone(),
            rel: "(files)".to_string(),
            role: LayoutRole::LooseFiles,
            profile: None,
            triple: None,
            llvm_cov: false,
            loose_files: Some(vec![t.join("a")]),
            files_partial,
        };
        let far = Duration::from_secs(60);
        let anchor = Anchor::new(&t);
        let whole = measure(spec(false), false, Instant::now(), far, &anchor);
        let cut = measure(spec(true), false, Instant::now(), far, &anchor);
        assert!(!whole.is_lower_bound);
        assert!(cut.is_lower_bound);
        assert_eq!(cut.logical_bytes, Some(10));
        fs::remove_dir_all(&t).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_not_followed() {
        let real = temp_dir("real-root");
        file(&real.join("debug/deps/x"), 5000);
        let holder = temp_dir("link-holder");
        let link = holder.join("target");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let s = survey_cargo_layout(&link);
        assert!(s.units.is_empty());
        assert!(!s.complete);
        fs::remove_dir_all(&real).ok();
        fs::remove_dir_all(&holder).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_into_other_trees() {
        let t = temp_dir("symlink");
        let elsewhere = temp_dir("elsewhere");
        file(&elsewhere.join("huge.bin"), 100_000);
        std::os::unix::fs::symlink(&elsewhere, t.join("debug")).unwrap();
        file(&t.join("doc/x"), 10);
        let s = survey_cargo_layout(&t);
        assert!(
            s.measured_logical_bytes < 100_000,
            "a symlinked profile must not be walked: {s:?}"
        );
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&elsewhere).ok();
    }

    #[test]
    fn survey_never_reaches_policy_code() {
        // The survey is report-only (ADR-0001 §19/§20): no policy, planner,
        // executor or evidence module may reference it.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for dir in ["policy", "planner", "executor", "evidence", "actions"] {
            let mut stack = vec![root.join(dir)];
            while let Some(d) = stack.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x == "rs") {
                        let text = fs::read_to_string(&p).unwrap();
                        for needle in ["cargo_layout", "CargoLayoutSurvey", "LayoutUnit"] {
                            assert!(
                                !text.contains(needle),
                                "{} references report-only `{needle}`",
                                p.display()
                            );
                        }
                    }
                }
            }
        }
    }

    /// A nested directory swapped for a symlink *during* a unit's walk, after
    /// its parent was listed, abandons the walk: nothing from outside the root
    /// is summed and the unit is "changed".
    #[cfg(unix)]
    #[test]
    fn a_nested_directory_swapped_during_the_walk_is_changed() {
        let t = temp_dir("walk-swap-root");
        let outside = temp_dir("walk-swap-outside");
        file(&t.join("debug/deps/pkg/a"), 100);
        file(&outside.join("b"), 9_999_999);
        let anchor = Anchor::new(&t);
        let deps = t.join("debug/deps");
        let calls = std::cell::Cell::new(0usize);
        let est =
            estimate_logical_bytes_within(&deps, crate::scanner::ScanBudget::unlimited(), &|dir| {
                // Call 1 is the root's pre-listing check, call 2 its
                // post-listing check: `pkg` is already queued by then.
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    fs::remove_dir_all(deps.join("pkg")).unwrap();
                    std::os::unix::fs::symlink(&outside, deps.join("pkg")).unwrap();
                }
                anchor.holds(dir)
            });
        assert!(
            est.bytes.observed().is_none(),
            "an escaped walk sums nothing"
        );
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&outside).ok();
    }

    /// A unit that is a lone file has no listing to guard, so the walker
    /// asks the predicate about the file itself before and after reading it.
    #[test]
    fn a_lone_file_unit_is_checked_for_containment_too() {
        let t = temp_dir("lone-file");
        file(&t.join("debug/deps"), 42);
        let calls = std::cell::Cell::new(0usize);
        let kept = estimate_logical_bytes_within(
            &t.join("debug/deps"),
            crate::scanner::ScanBudget::unlimited(),
            &|_| {
                calls.set(calls.get() + 1);
                true
            },
        );
        assert_eq!(kept.bytes.observed().copied(), Some(42));
        assert_eq!(calls.get(), 2, "once before and once after the read");

        let swapped = estimate_logical_bytes_within(
            &t.join("debug/deps"),
            crate::scanner::ScanBudget::unlimited(),
            &|_| false,
        );
        assert!(swapped.bytes.observed().is_none());

        // Contained when first asked, swapped by the time the read is done.
        let asked = std::cell::Cell::new(0usize);
        let swapped_after = estimate_logical_bytes_within(
            &t.join("debug/deps"),
            crate::scanner::ScanBudget::unlimited(),
            &|_| {
                asked.set(asked.get() + 1);
                asked.get() < 2
            },
        );
        assert!(swapped_after.bytes.observed().is_none());
        fs::remove_dir_all(&t).ok();
    }

    /// A directory replaced by a symlink after discovery is "changed", not a
    /// figure for the tree the link points at, whether the swap lands before a
    /// listing or before a measurement.
    #[cfg(unix)]
    #[test]
    fn a_directory_swapped_for_a_symlink_is_changed_not_measured() {
        let t = temp_dir("swap-root");
        let outside = temp_dir("swap-outside");
        file(&t.join("debug/deps/x"), 100);
        file(&outside.join("deps/y"), 9_999_999);
        let anchor = Anchor::new(&t);
        let spec = || UnitSpec {
            abs: t.join("debug/deps"),
            rel: "debug/deps".to_string(),
            role: LayoutRole::Deps,
            profile: Some("debug".to_string()),
            triple: None,
            llvm_cov: false,
            loose_files: None,
            files_partial: false,
        };
        let far = Duration::from_secs(60);
        let before = measure(spec(), false, Instant::now(), far, &anchor);
        assert!(before.measured);
        assert_eq!(before.logical_bytes, Some(100));

        fs::remove_dir_all(t.join("debug")).unwrap();
        std::os::unix::fs::symlink(&outside, t.join("debug")).unwrap();
        let after = measure(spec(), false, Instant::now(), far, &anchor);
        assert!(!after.measured);
        assert_eq!(after.unmeasured_reason, Some("changed"));
        assert_eq!(after.logical_bytes, None);

        let mut listing = Listing {
            started: Instant::now(),
            deadline: far,
            truncated: false,
            anchor,
        };
        assert!(child_dirs_and_files(&t.join("debug"), &mut listing).is_none());
        fs::remove_dir_all(&t).ok();
        fs::remove_dir_all(&outside).ok();
    }
}
