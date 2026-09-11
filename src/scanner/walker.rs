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
//!
//! This initial version never follows symlinks (mount-boundary and
//! symlink-cycle handling land in a follow-up commit on this file).

use std::fs;
use std::path::PathBuf;

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
}

impl ScanOptions {
    pub fn new(root: impl Into<PathBuf>, top_k: usize) -> Self {
        Self {
            root: root.into(),
            top_k,
            budget: ScanBudget::default(),
        }
    }

    pub fn with_budget(mut self, budget: ScanBudget) -> Self {
        self.budget = budget;
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

/// Run a bounded, streaming scan. Symlinks are never followed: each
/// symlink entry is recorded as its own small candidate (its own on-disk
/// size), and traversal never descends through it.
pub fn scan(options: &ScanOptions) -> ScanReport {
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

    let mut stack: Vec<(PathBuf, usize)> = vec![(options.root.clone(), 0)];
    let mut stop_reason = StopReason::Exhausted;

    'walk: while let Some((dir, depth)) = stack.pop() {
        if let Some(reason) = tracker.check() {
            stop_reason = reason;
            break;
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

            if symlink_meta.file_type().is_symlink() {
                // Record the link itself (its own small on-disk size),
                // never dereference into the target.
                topk.offer(ScanCandidate::new(
                    path,
                    symlink_meta.len(),
                    child_depth,
                ));
                continue;
            }

            if symlink_meta.is_dir() {
                stack.push((path, child_depth));
                continue;
            }

            topk.offer(ScanCandidate::new(path, symlink_meta.len(), child_depth));
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
