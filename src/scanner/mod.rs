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
///
/// `Err` is a usage error, worded for the caller to print and exit 2 with.
/// Deciding the exit code is deliberately left to the binary, so this
/// library function does not end the process (HORO-1322).
///
/// Both positionals used to be read by index with a silent fallback, which
/// made three different mistakes invisible: `scan --json` read the flag as a
/// directory and reported a successful scan of nothing, `scan . abc` scanned
/// with the default 20 rather than saying `abc` is not a count, and a third
/// argument was dropped without comment.
pub fn run_scan_cli(args: &[String]) -> Result<(), String> {
    // `scan` has no flags at all, so a dash token cannot be a mistyped one —
    // it is either a flag this command does not have, or a path that needs
    // writing as `./-name` to be unambiguous.
    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-')) {
        return Err(format!("unrecognized argument '{flag}'"));
    }
    if let Some(extra) = args.get(2) {
        return Err(format!("unrecognized argument '{extra}'"));
    }

    let root = args.first().cloned().unwrap_or_else(|| ".".to_string());
    let top_k: usize = match args.get(1) {
        Some(value) => value
            .parse()
            .map_err(|_| format!("top_k must be a non-negative integer, got '{value}'"))?,
        None => 20,
    };

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

    Ok(())
}
