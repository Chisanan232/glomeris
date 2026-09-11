//! Structured scan results consumed by later Evidence logic (HORO-949).
//!
//! Kept intentionally minimal: no detector classification (HORO-948) and
//! no Evidence model (HORO-949) is pre-built here.

use std::path::PathBuf;

/// Placeholder classification for a candidate's storage "kind". Real
/// detector-driven classification lands in HORO-948; every candidate
/// produced by the scanner today is [`CandidateKind::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Unknown,
}

/// Whether a candidate's metadata was fully observed, or only partially
/// observed because of a filesystem error encountered mid-walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    /// Metadata was read successfully.
    Complete,
    /// Metadata could not be read (permission denied, the entry vanished
    /// mid-walk, an I/O error, ...). This is "unknown", never "empty" —
    /// callers must not treat an incomplete entry as zero bytes.
    Incomplete,
}

/// One candidate hotspot discovered by the scanner.
///
/// # Size semantics
///
/// `logical_size_bytes` is the entry's `st_size` as reported by the
/// filesystem for a single directory entry (the scanner does not sum
/// directory subtrees). This is a **logical size**, not a guaranteed
/// "reclaimable bytes" figure: sparse files, hard links, copy-on-write
/// clones, and filesystem-level compression can all make the space
/// actually freed by deleting this entry smaller or larger than
/// `logical_size_bytes`. Deciding what is safe to delete, and computing
/// an honest reclaimable-bytes estimate, is the executor's job
/// (HORO-951/952) — the scanner only ever reports what the filesystem
/// told it about a single entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanCandidate {
    /// Path to the entry, resolved from a canonicalized scan root.
    pub path: PathBuf,
    /// Logical size in bytes as reported by the filesystem (`st_size`).
    /// NOT a guaranteed-reclaimable-bytes figure — see the type doc above.
    pub logical_size_bytes: u64,
    /// Depth of this entry relative to the scan root (root's direct
    /// children are depth 1).
    pub depth: usize,
    /// Placeholder storage-kind classification (HORO-948 lands real
    /// detectors).
    pub kind: CandidateKind,
    /// Whether this entry's metadata was fully read.
    pub status: EntryStatus,
}

impl ScanCandidate {
    /// Build a candidate whose metadata was fully observed.
    pub fn new(path: PathBuf, logical_size_bytes: u64, depth: usize) -> Self {
        Self {
            path,
            logical_size_bytes,
            depth,
            kind: CandidateKind::Unknown,
            status: EntryStatus::Complete,
        }
    }

    /// Build a candidate representing an entry whose metadata could not be
    /// read. `logical_size_bytes` is `0` here only because there is no
    /// size to report — `status` is what callers must check, not the size.
    pub fn incomplete(path: PathBuf, depth: usize) -> Self {
        Self {
            path,
            logical_size_bytes: 0,
            depth,
            kind: CandidateKind::Unknown,
            status: EntryStatus::Incomplete,
        }
    }
}
