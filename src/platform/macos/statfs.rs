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

use crate::monitor::fs_stat::{FsStat, FsUsage};
use std::io;
use std::path::Path;

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
}
