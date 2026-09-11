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

/// Minimal `glomeris scan [path] [top_k]` CLI entry point. Kept here (not
/// in `main.rs`) so the binary's diff for wiring this in is a single
/// function call.
pub fn run_scan_cli(args: &[String]) {
    let root = args.first().cloned().unwrap_or_else(|| ".".to_string());
    let top_k: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);

    let options = ScanOptions::new(root, top_k);
    let report = scan(&options);

    println!(
        "scanned {} entries ({:?}), {} incomplete",
        report.files_visited, report.stop_reason, report.incomplete_entries
    );
    for candidate in &report.candidates {
        println!(
            "{:>12}  depth={:<3}  {}",
            candidate.logical_size_bytes,
            candidate.depth,
            candidate.path.display()
        );
    }
}
