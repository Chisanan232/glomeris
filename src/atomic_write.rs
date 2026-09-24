//! The one producer of temp paths for Glomeris's atomic file writes.
//!
//! Three places write a file that another process may be writing at the
//! same moment: the launchd plist (and its `.bak`), the daemon heartbeat,
//! and the autopilot grant envelope. All three did the same thing — write
//! a sibling temp file, then `fs::rename` it over the target — and all
//! three derived that temp path deterministically from the target
//! (`path.with_extension("plist.tmp")` and friends).
//!
//! The rename is atomic. The temp file was not private. Two writers of the
//! same target chose the *same* temp path, so each truncated the other's
//! scratch file from offset 0 and one of them renamed the resulting
//! byte-level mixture over the target. The atomic rename then published a
//! torn file rather than preventing one — the tearing had simply moved to
//! the far side of the atomic step, where the reader-facing guarantee each
//! of those call sites documents no longer covered it (HORO-1464).
//!
//! Two independent properties fix that, and both matter:
//!
//! 1. The temp name carries this process's id and a per-process sequence
//!    number, so two writers do not name the same file in the first place.
//! 2. The temp file is created with `create_new`, so a writer never
//!    truncates a file it did not itself create. Process ids are reused
//!    across time and the sequence restarts at zero in every process, so
//!    residue from a long-dead process *can* occupy a name we would pick;
//!    property 1 alone would truncate it. Refusing and trying the next
//!    name means the only file this module ever writes into is one it
//!    exclusively created.
//!
//! No cross-process locking is involved or wanted. Concurrent writers do
//! not need to take turns; they need to stop sharing scratch space. Which
//! writer's content ends up published is still whichever renamed last —
//! that was always true of an atomic rename, and it is a complete
//! description of the outcome, because the published file is now always
//! exactly one writer's content.
//!
//! Durability is deliberately unchanged: as before, neither the temp file
//! nor its parent directory is `fsync`ed, so this says nothing about what
//! survives a power loss. That is a separate property from the one the
//! call sites claim, and widening it here would have hidden this fix
//! inside an unrelated behaviour change.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Separates a target file's own name from the per-writer suffix. Kept as
/// a named constant because the residue check in
/// [`temp_paths_beside`] recognises temp files by exactly this
/// marker: if the two ever disagreed, that check would silently stop
/// finding anything and start passing vacuously.
const TEMP_INFIX: &str = ".tmp.";

/// How many temp names to try before giving up. Only reached if that many
/// consecutive candidate names are already occupied by residue, which
/// needs a directory holding leftovers from a previous incarnation of this
/// exact process id at this exact sequence position — not something to
/// retry indefinitely over, and not something to paper over either.
const MAX_TEMP_NAME_ATTEMPTS: u64 = 64;

/// Per-process sequence, bumped for every temp path handed out. Makes two
/// threads in one process pick different names without either of them
/// consulting the filesystem.
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Builds the candidate temp path for `target` at `sequence`. The name is
/// `<target file name>.tmp.<pid>.<sequence>`, appended to the whole file
/// name rather than substituted into the extension the way the call sites
/// this replaces did. `Path::with_extension` *replaces* the final
/// extension, so `x.plist.bak.with_extension("plist.tmp")` produced
/// `x.plist.plist.tmp` — not a collision with `x.plist`'s own temp file as
/// it happens, but a name that has to be worked out rather than read, and
/// the temp file for one target should be obviously derived from that
/// target when it turns up in a directory listing.
///
/// Errors if `target` has no final path component (`/`, `..`), which is
/// not a file any caller can atomically replace.
fn temp_candidate(target: &Path, sequence: u64) -> io::Result<PathBuf> {
    let file_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot atomically write {}: it has no final path component",
                target.display()
            ),
        )
    })?;
    let mut candidate = file_name.to_os_string();
    candidate.push(format!("{TEMP_INFIX}{}.{sequence}", std::process::id()));
    Ok(target.with_file_name(candidate))
}

/// Creates a temp file beside `target` that no other writer can be holding,
/// returning it together with its path.
fn create_exclusive_temp(target: &Path) -> io::Result<(File, PathBuf)> {
    create_exclusive_temp_from(target, || NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

/// The body of [`create_exclusive_temp`], with the sequence source passed
/// in. Production always passes the shared [`NEXT_SEQUENCE`]; a test passes
/// a counter of its own so it can predict the exact names this will try.
/// That prediction is the only way to pre-place a file at a name we are
/// about to reach and prove it survives — against the shared counter, which
/// every test in the process advances, a predicted name is a guess.
fn create_exclusive_temp_from(
    target: &Path,
    mut next_sequence: impl FnMut() -> u64,
) -> io::Result<(File, PathBuf)> {
    let mut last_occupied = None;
    for _ in 0..MAX_TEMP_NAME_ATTEMPTS {
        let sequence = next_sequence();
        let candidate = temp_candidate(target, sequence)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, candidate)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                last_occupied = Some(candidate);
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "cannot atomically write {}: {MAX_TEMP_NAME_ATTEMPTS} candidate temp names in a row \
             were already taken (last tried {})",
            target.display(),
            last_occupied
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
    ))
}

/// Writes `contents` to `target`, replacing it atomically.
///
/// Creates a temp file in `target`'s own directory — so the rename is
/// within one filesystem and therefore atomic — writes `contents` into it,
/// and renames it over `target`. Removes the temp file on any failure
/// before the rename, so neither a failed write nor a failed rename leaves
/// residue or a partially-written `target`.
///
/// Does **not** create `target`'s parent directory: callers that need that
/// do it themselves, and inventing directories inside a "replace this file"
/// primitive would turn a misspelled path into a silently-created tree.
pub fn write_atomically(target: &Path, contents: &[u8]) -> io::Result<()> {
    let (mut file, tmp_path) = create_exclusive_temp(target)?;
    if let Err(e) = file.write_all(contents) {
        drop(file);
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    drop(file);
    if let Err(e) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}
