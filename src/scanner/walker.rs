//! Bounded, streaming filesystem traversal.
//!
//! Design choice: hand-rolled on `std::fs::read_dir` rather than the
//! `walkdir` crate. Justification (see PR body for the full writeup):
//! this ticket's safety semantics (default no-symlink-following, explicit
//! mount-boundary refusal via an injectable device lookup, permission-
//! denied recorded as "incomplete" rather than surfaced as an `Err` that
//! aborts the whole walk) need first-class hooks that are simpler to get
//! right by owning the traversal loop directly than by working around a
//! general-purpose crate's own defaults and error model. It also keeps
//! the scanner's dependency footprint at zero beyond `std`.
//!
//! The walk is iterative (an explicit stack), never recursive, so it
//! cannot blow the call stack on a deep tree, and it never buffers the
//! full directory listing: each directory is streamed one `read_dir`
//! entry at a time and only the bounded [`TopKCandidates`] structure is
//! retained across the whole walk.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::budget::{ScanBudget, StopReason};
use super::candidate::ScanCandidate;
use super::topk::TopKCandidates;

/// Cap on how many "incomplete" entry paths are retained as samples in a
/// [`ScanReport`]. The full count is still tracked exactly via
/// [`ScanReport::incomplete_entries`] — only the sample list is bounded,
/// for the same "never buffer unboundedly" reason the candidate list is
/// bounded to `top_k`.
const MAX_INCOMPLETE_SAMPLES: usize = 256;

/// Configuration for one scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Root directory to walk.
    pub root: PathBuf,
    /// Maximum number of candidates to retain (bounded top-K).
    pub top_k: usize,
    /// Time / file-count budget for the scan.
    pub budget: ScanBudget,
    /// Whether to follow symlinked directories during traversal.
    /// Defaults to `false`. When `true`, a directory-cycle guard (keyed
    /// on device + inode) prevents infinite traversal through a symlink
    /// loop.
    pub follow_symlinks: bool,
}

impl ScanOptions {
    pub fn new(root: impl Into<PathBuf>, top_k: usize) -> Self {
        Self {
            root: root.into(),
            top_k,
            budget: ScanBudget::default(),
            follow_symlinks: false,
        }
    }

    pub fn with_budget(mut self, budget: ScanBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_follow_symlinks(mut self, follow_symlinks: bool) -> Self {
        self.follow_symlinks = follow_symlinks;
        self
    }
}

/// Result of one scan.
#[derive(Debug)]
pub struct ScanReport {
    /// Bounded top-K candidates by logical size, descending.
    pub candidates: Vec<ScanCandidate>,
    /// Why the walk stopped.
    pub stop_reason: StopReason,
    /// Total filesystem entries visited (directory listings iterated).
    pub files_visited: u64,
    /// Total number of entries whose metadata could not be read
    /// (permission denied, vanished mid-walk, I/O error, ...).
    pub incomplete_entries: u64,
    /// Bounded sample of the paths behind `incomplete_entries`, capped at
    /// [`MAX_INCOMPLETE_SAMPLES`].
    pub incomplete_samples: Vec<ScanCandidate>,
}

/// Abstraction over "what device does this path live on", so mount-
/// boundary behavior can be tested without mounting a second real
/// filesystem: tests inject a fake that reports different device ids for
/// different subtrees of one real temp directory.
pub(crate) trait DeviceLookup {
    fn device_of(&self, path: &Path) -> io::Result<u64>;
}

pub(crate) struct StdDeviceLookup;

impl DeviceLookup for StdDeviceLookup {
    fn device_of(&self, path: &Path) -> io::Result<u64> {
        Ok(fs::symlink_metadata(path)?.dev())
    }
}

/// Pure predicate: is `dev` the same device as `root_dev`? Extracted so
/// the mount-boundary rule itself is trivially unit-testable.
pub(crate) fn is_same_device(root_dev: u64, dev: u64) -> bool {
    root_dev == dev
}

/// Run a bounded, streaming scan using real filesystem device lookups.
pub fn scan(options: &ScanOptions) -> ScanReport {
    scan_with_device_lookup(options, &StdDeviceLookup)
}

pub(crate) fn scan_with_device_lookup<D: DeviceLookup>(
    options: &ScanOptions,
    device_lookup: &D,
) -> ScanReport {
    let mut topk = TopKCandidates::new(options.top_k);
    let mut tracker = options.budget.tracker();
    let mut incomplete_entries: u64 = 0;
    let mut incomplete_samples: Vec<ScanCandidate> = Vec::new();

    let mut record_incomplete = |path: PathBuf, depth: usize| {
        incomplete_entries += 1;
        if incomplete_samples.len() < MAX_INCOMPLETE_SAMPLES {
            incomplete_samples.push(ScanCandidate::incomplete(path, depth));
        }
    };

    // Root device is used as the mount-boundary reference. If it cannot
    // be determined, boundary enforcement is skipped rather than the
    // whole scan failing.
    let root_dev = device_lookup.device_of(&options.root).ok();

    let mut visited_dirs: HashSet<(u64, u64)> = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(options.root.clone(), 0)];
    let mut stop_reason = StopReason::Exhausted;

    'walk: while let Some((dir, depth)) = stack.pop() {
        if let Some(reason) = tracker.check() {
            stop_reason = reason;
            break;
        }

        // Cycle guard: refuse to re-enter a directory already visited by
        // real (dev, inode) identity. This is what makes following
        // symlinks safe against loops; it is a no-op cost otherwise since
        // a real tree never revisits the same directory inode twice.
        if let Ok(meta) = fs::symlink_metadata(&dir) {
            let key = (meta.dev(), meta.ino());
            if !visited_dirs.insert(key) {
                continue;
            }
        }

        let read_dir = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                record_incomplete(dir, depth);
                continue;
            }
        };

        for entry in read_dir {
            if let Some(reason) = tracker.record_visit() {
                stop_reason = reason;
                break 'walk;
            }

            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    record_incomplete(dir.clone(), depth);
                    continue;
                }
            };
            let path = entry.path();
            let child_depth = depth + 1;

            let symlink_meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    record_incomplete(path, child_depth);
                    continue;
                }
            };

            let is_symlink = symlink_meta.file_type().is_symlink();

            if is_symlink && !options.follow_symlinks {
                // Record the link itself (its own small on-disk size),
                // never dereference into the target.
                topk.offer(ScanCandidate::new(path, symlink_meta.len(), child_depth));
                continue;
            }

            let meta = if is_symlink {
                match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => {
                        record_incomplete(path, child_depth);
                        continue;
                    }
                }
            } else {
                symlink_meta
            };

            if meta.is_dir() {
                if let Some(root_dev) = root_dev {
                    match device_lookup.device_of(&path) {
                        Ok(dev) if !is_same_device(root_dev, dev) => {
                            // Different filesystem/device: do not cross
                            // the mount boundary by default.
                            continue;
                        }
                        _ => {}
                    }
                }
                // When `path` is a symlink to a directory (only reachable
                // here with follow_symlinks=true), push the *resolved*
                // target path, not the symlink path itself. The cycle
                // guard re-lstats whatever is on the stack when it is
                // popped — lstat-ing the symlink path again would report
                // the symlink's own inode, never matching the target
                // directory's inode, so the guard would never trip and a
                // symlink loop would traverse forever.
                let push_path = if is_symlink {
                    match fs::canonicalize(&path) {
                        Ok(resolved) => resolved,
                        Err(_) => {
                            record_incomplete(path, child_depth);
                            continue;
                        }
                    }
                } else {
                    path
                };
                stack.push((push_path, child_depth));
                continue;
            }

            topk.offer(ScanCandidate::new(path, meta.len(), child_depth));
        }
    }

    ScanReport {
        files_visited: tracker.files_visited(),
        candidates: topk.into_sorted_vec(),
        stop_reason,
        incomplete_entries,
        incomplete_samples,
    }
}
