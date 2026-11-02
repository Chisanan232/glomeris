//! Config protective markers as a veto-only probe (HORO-2359).
//!
//! The sibling Config tooling protects directories it considers evidence with
//! two on-disk markers: a `.ca-keep` file (never delete) and an owner receipt
//! `.ca-owner.json` (a live or unverifiable owner session). This probe lets
//! Glomeris notice them. It exists to add protection and nothing else
//! (ADR-0001 §3, §18):
//!
//! * It only calls `symlink_metadata` and `read_dir`. It never opens a file and
//!   never reads a byte of content, so a receipt's fields (`reproducible`,
//!   `private`, `evidence_free`, `session_id`) cannot be parsed and therefore
//!   cannot be used to raise autonomy. A forged or valid receipt ends in the
//!   same place: ASK.
//! * A marker it cannot verify (permission error, over-large directory) is
//!   `Unavailable`, which policy turns into ASK, never into "no marker".
//! * Depth matches Config's own scan for a Cargo target: the directory itself
//!   and one level below.

use std::io::ErrorKind;
use std::path::Path;

use super::probe::{ProbeOutcome, ProbeReason};

/// Marker file name Config uses to say "never delete this".
const KEEP_MARKER: &str = ".ca-keep";
/// Owner receipt file name. Presence is all that is read.
const RECEIPT_MARKER: &str = ".ca-owner.json";
/// Directory entries examined before the probe gives up as unverifiable.
const MAX_ENTRIES: usize = 4096;

/// Which Config markers were seen. Absence is the normal case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigMarkers {
    /// A `.ca-keep` marker at the top level or one directory below.
    pub keep: bool,
    /// A `.ca-owner.json` receipt at the top level.
    pub owner_receipt: bool,
}

impl ConfigMarkers {
    /// No marker seen.
    pub const fn none() -> Self {
        Self {
            keep: false,
            owner_receipt: false,
        }
    }
}

fn reason_for(error: &std::io::Error) -> ProbeReason {
    match error.kind() {
        ErrorKind::PermissionDenied => ProbeReason::PermissionDenied,
        _ => ProbeReason::Failed,
    }
}

/// Looks for Config markers in `dir` and one level below it.
///
/// `NotFound` for `dir` itself is `Observed(none)`: a vanished target is
/// caught by the fingerprint check, and "nothing there" has no marker to
/// honour. Every other failure is `Unavailable`.
pub fn probe(dir: &Path) -> ProbeOutcome<ConfigMarkers> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return ProbeOutcome::Observed(ConfigMarkers::none())
        }
        Err(e) => return ProbeOutcome::Unavailable(reason_for(&e)),
    };

    let mut markers = ConfigMarkers::none();
    for (seen, entry) in entries.enumerate() {
        if seen >= MAX_ENTRIES {
            return ProbeOutcome::Unavailable(ProbeReason::Failed);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => return ProbeOutcome::Unavailable(reason_for(&e)),
        };
        let name = entry.file_name();
        if name == KEEP_MARKER {
            markers.keep = true;
            continue;
        }
        if name == RECEIPT_MARKER {
            markers.owner_receipt = true;
            continue;
        }
        // One level down. `file_type` does not follow a symlink, so a link to
        // a directory elsewhere is not descended into.
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                match std::fs::symlink_metadata(entry.path().join(KEEP_MARKER)) {
                    Ok(_) => markers.keep = true,
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return ProbeOutcome::Unavailable(reason_for(&e)),
                }
            }
            Ok(_) => {}
            Err(e) => return ProbeOutcome::Unavailable(reason_for(&e)),
        }
    }
    ProbeOutcome::Observed(markers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-marker-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn observed(outcome: ProbeOutcome<ConfigMarkers>) -> ConfigMarkers {
        match outcome {
            ProbeOutcome::Observed(markers) => markers,
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_with_no_marker_is_observed_none() {
        let dir = temp_dir("none");
        fs::create_dir_all(dir.join("debug")).unwrap();
        assert_eq!(observed(probe(&dir)), ConfigMarkers::none());
    }

    #[test]
    fn a_missing_directory_is_observed_none() {
        let dir = temp_dir("missing").join("gone");
        assert_eq!(observed(probe(&dir)), ConfigMarkers::none());
    }

    #[test]
    fn a_top_level_keep_marker_is_seen() {
        let dir = temp_dir("keep-top");
        fs::write(dir.join(".ca-keep"), b"").unwrap();
        assert!(observed(probe(&dir)).keep);
    }

    #[test]
    fn a_keep_marker_one_level_down_is_seen() {
        let dir = temp_dir("keep-child");
        fs::create_dir_all(dir.join("horo-1399")).unwrap();
        fs::write(dir.join("horo-1399/.ca-keep"), b"").unwrap();
        assert!(observed(probe(&dir)).keep);
    }

    #[test]
    fn a_keep_marker_two_levels_down_is_not_looked_for() {
        let dir = temp_dir("keep-deep");
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("a/b/.ca-keep"), b"").unwrap();
        assert!(!observed(probe(&dir)).keep);
    }

    #[test]
    fn a_keep_marker_that_is_a_symlink_still_counts() {
        let dir = temp_dir("keep-link");
        std::os::unix::fs::symlink("/nonexistent", dir.join(".ca-keep")).unwrap();
        assert!(observed(probe(&dir)).keep);
    }

    #[test]
    fn a_receipt_is_noted_without_being_read() {
        let dir = temp_dir("receipt");
        // Unreadable content proves it is never opened.
        fs::write(dir.join(".ca-owner.json"), b"\xff\xfe not json").unwrap();
        fs::set_permissions(
            dir.join(".ca-owner.json"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let markers = observed(probe(&dir));
        assert!(markers.owner_receipt);
        assert!(!markers.keep);
    }

    #[test]
    fn a_symlinked_child_directory_is_not_descended_into() {
        let dir = temp_dir("link-child");
        let elsewhere = temp_dir("elsewhere");
        fs::write(elsewhere.join(".ca-keep"), b"").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("link")).unwrap();
        assert!(!observed(probe(&dir)).keep);
    }

    #[test]
    fn an_unreadable_directory_is_unavailable_not_none() {
        let dir = temp_dir("unreadable");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = probe(&dir);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            outcome,
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied)
        );
    }

    #[test]
    fn an_unreadable_child_directory_is_unavailable_not_none() {
        let dir = temp_dir("unreadable-child");
        fs::create_dir_all(dir.join("locked")).unwrap();
        fs::set_permissions(dir.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = probe(&dir);
        fs::set_permissions(dir.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            outcome,
            ProbeOutcome::Unavailable(ProbeReason::PermissionDenied)
        );
    }

    #[test]
    fn an_oversized_directory_is_unavailable() {
        let dir = temp_dir("big");
        for i in 0..MAX_ENTRIES + 1 {
            fs::write(dir.join(format!("f{i}")), b"").unwrap();
        }
        assert_eq!(probe(&dir), ProbeOutcome::Unavailable(ProbeReason::Failed));
    }
}
