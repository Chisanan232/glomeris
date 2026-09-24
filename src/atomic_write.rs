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
/// Every temp path this module could have created beside `target` that is
/// still on disk. Empty is the only acceptable result once a write has
/// returned, success or failure.
///
/// Exists so tests can assert "no residue" against the *naming scheme*
/// rather than against one hardcoded name. The assertions this replaces
/// each named a single fixed path (`x.plist.tmp`, `x.json.tmp`,
/// `x.conf.tmp`); a name this module no longer produces is a path that can
/// never exist, so those assertions would have passed without testing
/// anything.
///
/// Test-only: a residue scan is not something production code should ever
/// need, and shipping it as public API would invite a caller to treat
/// leftover temp files as a normal state to clean up rather than as a bug.
#[cfg(test)]
pub(crate) fn temp_paths_beside(target: &Path) -> Vec<PathBuf> {
    let Some(file_name) = target.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let Some(parent) = target.parent() else {
        return Vec::new();
    };
    let prefix = format!("{file_name}{TEMP_INFIX}");
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own. Every test here predicts exact temp
    /// file names, which is only sound because no other test writes into
    /// the same directory.
    fn unique_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-atomic-write-test-{tag}-{}-{}",
            std::process::id(),
            NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    /// Hands out 0, 1, 2, ... regardless of what the rest of the process is
    /// doing, so a test can name the files this write is about to try.
    fn counting_from(start: u64) -> impl FnMut() -> u64 {
        let mut next = start;
        move || {
            let current = next;
            next += 1;
            current
        }
    }

    #[test]
    fn a_temp_name_is_the_target_name_plus_this_process_and_a_sequence() {
        let candidate = temp_candidate(Path::new("/a/b/heartbeat.json"), 7).expect("named");

        assert_eq!(
            candidate,
            Path::new(&format!("/a/b/heartbeat.json.tmp.{}.7", std::process::id())),
            "the temp name must stay derived from the whole target file name: \
             a reader who finds one in a directory listing has to be able to \
             tell which file it belongs to"
        );
    }

    #[test]
    fn a_backup_and_its_plist_do_not_share_a_temp_name() {
        // The shape that made `with_extension` the wrong tool: it replaces
        // the last extension, so `.bak`'s temp name was spelled in terms of
        // the plist's, and reasoning about whether they collided took a
        // second look at `file_stem`. These are both concrete targets in
        // `launchd::install_impl`, written one after the other.
        let plist = temp_candidate(Path::new("/a/x.plist"), 0).expect("named");
        let backup = temp_candidate(Path::new("/a/x.plist.bak"), 1).expect("named");

        assert_ne!(plist, backup);
        assert!(backup.to_string_lossy().contains("x.plist.bak.tmp."));
    }

    #[test]
    fn every_call_gets_a_temp_name_no_other_call_is_using() {
        let dir = unique_test_dir("distinct-names");
        let target = dir.join("shared.conf");

        let (_first_file, first) = create_exclusive_temp(&target).expect("first temp");
        let (_second_file, second) = create_exclusive_temp(&target).expect("second temp");

        assert_ne!(
            first, second,
            "two writers of one target must not name the same scratch file — \
             sharing it is the whole defect (HORO-1464)"
        );
        assert!(first.exists() && second.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_already_sitting_at_a_candidate_name_is_skipped_not_truncated() {
        let dir = unique_test_dir("never-truncate");
        let target = dir.join("envelope.conf");
        const SQUATTER: &str = "another writer's in-flight content";

        // Occupy the first four names this write will try. Standing in for
        // residue from a dead process that had this pid, and for the
        // in-flight temp file of a concurrent writer if the naming scheme
        // ever stopped separating them.
        let occupied: Vec<PathBuf> = (0..4)
            .map(|sequence| {
                let path = temp_candidate(&target, sequence).expect("named");
                fs::write(&path, format!("{SQUATTER} {sequence}")).expect("occupy");
                path
            })
            .collect();

        let (_file, chosen) =
            create_exclusive_temp_from(&target, counting_from(0)).expect("a free name exists");

        assert_eq!(
            chosen,
            temp_candidate(&target, 4).expect("named"),
            "the write must step past every occupied name to the first free one"
        );
        for (sequence, path) in occupied.iter().enumerate() {
            assert_eq!(
                fs::read_to_string(path).expect("still readable"),
                format!("{SQUATTER} {sequence}"),
                "an occupied candidate must be left byte-identical: opening it \
                 with truncation is how one writer destroys another's scratch file"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exhausting_every_candidate_name_errors_rather_than_reusing_one() {
        let dir = unique_test_dir("exhausted");
        let target = dir.join("heartbeat.json");
        for sequence in 0..MAX_TEMP_NAME_ATTEMPTS {
            let path = temp_candidate(&target, sequence).expect("named");
            fs::write(&path, "occupied").expect("occupy");
        }

        let error = create_exclusive_temp_from(&target, counting_from(0))
            .expect_err("no candidate name is free");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            error.to_string().contains("already taken"),
            "the error must say what it ran out of, not just fail: {error}"
        );
        for sequence in 0..MAX_TEMP_NAME_ATTEMPTS {
            let path = temp_candidate(&target, sequence).expect("named");
            assert_eq!(fs::read_to_string(&path).expect("readable"), "occupied");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_write_replaces_the_target_and_leaves_no_residue() {
        let dir = unique_test_dir("success");
        let target = dir.join("heartbeat.json");
        fs::write(&target, "stale").expect("seed");

        write_atomically(&target, b"fresh").expect("write");

        assert_eq!(fs::read_to_string(&target).expect("readable"), "fresh");
        assert_eq!(
            temp_paths_beside(&target),
            Vec::<PathBuf>::new(),
            "a successful write must not leave scratch files behind"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_cannot_be_renamed_leaves_neither_the_target_nor_residue() {
        // A target that is a directory: the temp file is created and
        // written, then `fs::rename` refuses to replace a directory with a
        // file. That exercises the cleanup path *after* a temp file exists,
        // which a failure before creation never reaches.
        let dir = unique_test_dir("rename-fails");
        let target = dir.join("in-the-way");
        fs::create_dir(&target).expect("occupy the target path with a directory");

        let error = write_atomically(&target, b"content").expect_err("rename must fail");

        assert!(
            target.is_dir(),
            "the failed write must not have replaced it"
        );
        assert_eq!(
            temp_paths_beside(&target),
            Vec::<PathBuf>::new(),
            "a failed write must clean up its own scratch file: {error}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_with_no_file_name_is_refused_rather_than_guessed_at() {
        let error = write_atomically(Path::new("/"), b"content").expect_err("not a file");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("no final path component"));
    }

    #[test]
    fn the_residue_scan_finds_a_temp_file_the_producer_really_created() {
        // Anti-vacuity for `temp_paths_beside` itself. Every "no residue"
        // assertion in this crate now goes through it, so a scan that
        // matches nothing would turn all of them into decoration — which is
        // exactly what the fixed-name assertions it replaced became.
        let dir = unique_test_dir("scan-finds-it");
        let target = dir.join("envelope.conf");

        let (_file, created) = create_exclusive_temp(&target).expect("temp");

        assert_eq!(
            temp_paths_beside(&target),
            vec![created.clone()],
            "the scan must recognise the producer's own output"
        );

        // And it must not claim a sibling that merely lives next door.
        fs::write(dir.join("envelope.conf.bak"), "not a temp file").expect("seed");
        fs::write(dir.join("unrelated.conf.tmp.1.1"), "another file's").expect("seed");
        assert_eq!(temp_paths_beside(&target), vec![created]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_never_publish_a_mixture_of_their_content() {
        // The defect, reproduced as the property it violates. Eight threads
        // write deliberately different-length content to one target while a
        // ninth reads it. Every read, and the final state, must be exactly
        // one writer's content: a mixed temp file renamed into place shows
        // up as a value that is in neither set.
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        const WRITERS: usize = 8;
        const WRITES_EACH: usize = 60;

        let dir = unique_test_dir("no-mixture");
        let target = dir.join("envelope.conf");

        // Distinct lengths as well as distinct bytes: equal-length content
        // can interleave into something that still looks well-formed, and
        // this test would then pass over a real tear.
        let contents: Vec<String> = (0..WRITERS)
            .map(|i| format!("writer-{i}-{}", "x".repeat(1 + i * 997)))
            .collect();
        write_atomically(&target, contents[0].as_bytes()).expect("seed");

        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let target = target.clone();
            let contents = contents.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut reads = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(seen) = fs::read_to_string(&target) {
                        assert!(
                            contents.contains(&seen),
                            "a reader saw content belonging to no single writer \
                             ({} bytes) — that is a torn publish",
                            seen.len()
                        );
                        reads += 1;
                    }
                }
                reads
            })
        };

        let writers: Vec<_> = (0..WRITERS)
            .map(|i| {
                let target = target.clone();
                let content = contents[i].clone();
                std::thread::spawn(move || {
                    for _ in 0..WRITES_EACH {
                        write_atomically(&target, content.as_bytes()).expect("write");
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("writer thread");
        }
        stop.store(true, Ordering::Relaxed);
        let reads = reader.join().expect("reader thread");

        let final_contents = fs::read_to_string(&target).expect("readable");
        assert!(
            contents.contains(&final_contents),
            "the published file must be exactly one writer's content"
        );
        assert_eq!(
            temp_paths_beside(&target),
            Vec::<PathBuf>::new(),
            "{} concurrent writes must leave no scratch files behind",
            WRITERS * WRITES_EACH
        );
        eprintln!(
            "atomic_write concurrency: {} writes across {WRITERS} threads, {reads} reads, \
             0 torn",
            WRITERS * WRITES_EACH
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
