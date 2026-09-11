//! Bounded, streaming filesystem scanner (HORO-947).
//!
//! Walks a directory tree without buffering the full tree in memory,
//! aggregating a bounded top-K set of [`ScanCandidate`]s by logical size.
//! No persistence dependency: everything here is pure in-memory
//! computation plus the filesystem walk itself.

pub mod budget;
pub mod candidate;
pub mod topk;
pub mod walker;

pub use budget::{ScanBudget, StopReason};
pub use candidate::{CandidateKind, EntryStatus, ScanCandidate};
pub use topk::TopKCandidates;
pub use walker::{scan, ScanOptions, ScanReport};
