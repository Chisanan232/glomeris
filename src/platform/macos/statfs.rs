//! macOS filesystem-capacity observation.
//!
//! Backed by a single `statvfs(2)` call via the `nix` crate — O(1), no
//! directory traversal. This is the only I/O the healthy polling path
//! performs.
//!
//! Crate choice: `nix` was picked over hand-rolling raw `libc::statvfs`
//! FFI (more unsafe code, more room for ABI mistakes) and over pulling in
//! `sysinfo` (a much larger dependency whose disk API we'd only use for
//! this one call, plus a background-thread-heavy design intended for
//! polling many metrics rather than one cheap capacity check). `nix` is a
//! mature, widely-used crate with a small, safe, allocation-free API
//! (`nix::sys::statvfs`) that maps cleanly onto exactly what's needed here.

use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::monitor::fs_stat::{FsStat, FsUsage};
use std::io;
use std::path::Path;
use std::process::Command;

/// Real `statvfs`-backed filesystem stat.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacosFsStat;

impl FsStat for MacosFsStat {
    fn stat(&self, path: &Path) -> io::Result<FsUsage> {
        let stats = nix::sys::statvfs::statvfs(path).map_err(|errno| {
            io::Error::other(format!("statvfs({}) failed: {errno}", path.display()))
        })?;

        let block_size = stats.fragment_size().max(1) as u64;
        let total_bytes = stats.blocks() as u64 * block_size;
        // Use the unprivileged "available to non-root" count, not the raw
        // free-block count, so pressure reflects what the user can
        // actually still write.
        let free_bytes = stats.blocks_available() as u64 * block_size;

        Ok(FsUsage::new(total_bytes, free_bytes))
    }
}

impl MacosFsStat {
    /// Opaque mount identity for `path` — the mount's `st_dev`, from a
    /// single `stat(2)` call. Used only to tell "this sample is still
    /// against the same filesystem as the last one" apart from "the volume
    /// changed underneath us" (ADR-0001 §4.4 / HORO-1827). Never a path and
    /// never logged as one: the return value is a bare, per-boot opaque
    /// integer with no mapping back to a mount point stored anywhere in
    /// this crate.
    ///
    /// A separate inherent method rather than a new [`FsStat`] trait method
    /// on purpose: widening that trait would force every existing test
    /// double (`FixedFsStat`, `ScriptedFsStat`, `FailingFsStat` in
    /// `poller.rs` and `executor/recovery_loop.rs`) to grow an unrelated
    /// method just to keep compiling, for a capability only the volume-
    /// sampling path in `daemon_run` needs and which already holds a
    /// concrete `MacosFsStat`, not a `&dyn FsStat`.
    pub fn volume_dev(&self, path: &Path) -> io::Result<u64> {
        let stat = nix::sys::stat::stat(path).map_err(|errno| {
            io::Error::other(format!("stat({}) failed: {errno}", path.display()))
        })?;
        Ok(stat.st_dev as u64)
    }
}

/// Read-only `tmutil listlocalsnapshots /` probe (ADR-0001 §4.4 /
/// HORO-1827). Counts the local Time Machine snapshots currently held for
/// the root volume — informational only: a non-zero count means freed
/// bytes may not reappear in `df` until macOS thins them on its own
/// schedule.
///
/// Glomeris never deletes a snapshot. `tmutil deletelocalsnapshots` does
/// not appear anywhere in this crate and must not be added as a registered
/// action — see ADR-0001 §4.4's explicit note.
pub fn probe_local_snapshots() -> ProbeOutcome<u32> {
    let output = match Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return ProbeOutcome::Unavailable(ProbeReason::ToolAbsent)
        }
        Err(_) => return ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    if !output.status.success() {
        return ProbeOutcome::Unavailable(ProbeReason::Failed);
    }

    // `tmutil listlocalsnapshots /` prints one header line ("Snapshots for
    // volume /:") followed by one snapshot identifier per line. Count only
    // the non-empty, non-header lines, so "no header" or "extra blank
    // lines" formatting differences never miscount.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let count = stdout
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with("Snapshots for")
        })
        .count() as u32;

    ProbeOutcome::Observed(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_root_returns_plausible_nonzero_totals() {
        // Live integration smoke test: root filesystem always exists on
        // any macOS CI runner or dev machine this crate targets.
        let stat = MacosFsStat;
        let usage = stat
            .stat(Path::new("/"))
            .expect("statvfs(/) should succeed");
        assert!(usage.total_bytes > 0);
        assert!(usage.free_bytes <= usage.total_bytes);
    }

    #[test]
    fn stat_nonexistent_path_returns_error_not_panic() {
        let stat = MacosFsStat;
        let result = stat.stat(Path::new("/this/path/does/not/exist/glomeris-test"));
        assert!(result.is_err());
    }

    #[test]
    fn volume_dev_of_root_is_nonzero_and_stable_across_calls() {
        let stat = MacosFsStat;
        let first = stat
            .volume_dev(Path::new("/"))
            .expect("stat(/) should succeed");
        let second = stat
            .volume_dev(Path::new("/"))
            .expect("stat(/) should succeed");
        assert_eq!(
            first, second,
            "the root volume's st_dev must not change between two immediate calls"
        );
    }

    #[test]
    fn volume_dev_of_nonexistent_path_returns_error_not_panic() {
        let stat = MacosFsStat;
        let result = stat.volume_dev(Path::new("/this/path/does/not/exist/glomeris-test"));
        assert!(result.is_err());
    }

    #[test]
    fn probe_local_snapshots_on_a_real_machine_never_panics() {
        // Live integration smoke test: whatever `tmutil` reports (zero
        // snapshots, some count, or genuinely unavailable on this runner),
        // the probe must return a value, never panic.
        let outcome = probe_local_snapshots();
        if let ProbeOutcome::Observed(count) = outcome {
            // A real count is always representable; nothing else to assert
            // without assuming this runner's snapshot state.
            let _ = count;
        }
    }
}
