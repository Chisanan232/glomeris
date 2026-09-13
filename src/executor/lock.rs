//! Advisory, machine-wide execution lock (HORO-1054).
//!
//! `free --target` and `emergency` have never raced against each other
//! before: they are the only two real-execution paths, and each is a
//! single-shot closed loop invoked from the CLI. That changes once a
//! future interactive `execute` subcommand exists (HORO-1055, not this
//! ticket) — an interactive UI makes double-invocation possible (a
//! clickable button double-firing), and `execute`'s own revalidation (see
//! [`crate::executor::execute`]) protects resource *identity*/*policy*,
//! not against two concurrent destructive runs stepping on each other.
//!
//! This module is a standalone, reusable primitive for that problem: a
//! single per-user lock file at [`default_lock_path`], guarded by a real
//! `flock(2)` advisory lock (via [`nix::fcntl::Flock`], part of the `nix`
//! crate's `fcntl` module, gated behind the `fs` feature this crate
//! already enables). Held for the duration of one real-execution call,
//! released automatically when the returned [`ExecutionLockGuard`] drops.
//!
//! `flock(2)` locks are associated with the *open file description*, not
//! the process — this is exactly what makes the lock usable both across
//! two real OS processes (two separate `glomeris` invocations, the actual
//! production scenario) and, faithfully, within a single test process via
//! two independent `File::open` calls on the same path (see this module's
//! own tests). A POSIX `fcntl` byte-range lock would NOT have this
//! property: those locks are associated with `(process, inode)`, so a
//! second lock request from the *same* process merely replaces its own
//! first lock instead of conflicting with it — unusable for testing this
//! contract without spawning a real second process. `flock(2)` avoids
//! that pitfall entirely.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

/// Resolves the per-user execution lock file path, using `$HOME`. Mirrors
/// [`crate::platform::macos::launchd::default_plist_path`]'s convention
/// (same `$HOME`-based resolution, same `io::Error::other` on a missing
/// `HOME`) and the `~/Library/Application Support/Glomeris/` directory
/// this codebase already uses for its other per-user state (see
/// `history.tsv` in `main.rs`).
pub fn default_lock_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("execution.lock"))
}

/// A held execution lock. Dropping this releases it (via `nix::fcntl::Flock`'s
/// own `Drop` impl, which issues `flock(fd, LOCK_UN)`).
#[derive(Debug)]
pub struct ExecutionLockGuard {
    _flock: Flock<File>,
}

/// Why [`acquire_execution_lock`]/[`acquire_execution_lock_at`] failed.
#[derive(Debug)]
pub enum LockError {
    /// Another invocation already holds the lock. Callers should map this
    /// to a distinct "busy" exit code rather than a generic failure.
    AlreadyHeld,
    /// Anything else: the lock directory/file couldn't be created or
    /// opened, or the underlying `flock(2)` call failed for a reason
    /// other than contention.
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyHeld => {
                write!(
                    f,
                    "the execution lock is already held by another invocation"
                )
            }
            LockError::Io(e) => write!(f, "failed to acquire the execution lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

impl From<io::Error> for LockError {
    fn from(e: io::Error) -> Self {
        LockError::Io(e)
    }
}

/// Acquires the standalone, reusable execution lock at
/// [`default_lock_path`]. Returns [`LockError::AlreadyHeld`] if another
/// invocation already holds it; the caller should map that to a "busy"
/// exit code rather than treating it as a generic error. On success,
/// holds the lock until the returned guard is dropped.
pub fn acquire_execution_lock() -> Result<ExecutionLockGuard, LockError> {
    let path = default_lock_path()?;
    acquire_execution_lock_at(&path)
}

/// Same as [`acquire_execution_lock`], but against a caller-supplied path
/// rather than [`default_lock_path`] — the seam this module's own tests
/// use to stay hermetic (never touching the real `$HOME`).
pub fn acquire_execution_lock_at(path: &Path) -> Result<ExecutionLockGuard, LockError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // `truncate(false)`: this is a lock file, never a data file — its
    // content (if any) must never be discarded just for taking the lock.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;

    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(flock) => Ok(ExecutionLockGuard { _flock: flock }),
        Err((_file, Errno::EAGAIN)) => Err(LockError::AlreadyHeld),
        Err((_file, errno)) => Err(LockError::Io(io::Error::from(errno))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    fn make_temp_lock_path(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "glomeris-lock-test-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ))
    }

    /// Real `flock(2)` semantics, exercised for real (not faked): two
    /// independent `File::open` calls on the SAME path — exactly what two
    /// separate `glomeris free`/`glomeris emergency` process invocations
    /// would each do — must conflict, because `flock(2)` locks are keyed
    /// on the open file description, not the process. See this module's
    /// doc comment for why this is the correct thing to test rather than
    /// a POSIX `fcntl` byte-range lock (which would NOT conflict here).
    #[test]
    fn second_acquire_fails_busy_while_first_is_held() {
        let path = make_temp_lock_path("busy");

        let first = acquire_execution_lock_at(&path).expect("first acquire must succeed");
        let second = acquire_execution_lock_at(&path);

        match second {
            Err(LockError::AlreadyHeld) => {}
            other => {
                panic!("expected LockError::AlreadyHeld while first guard is held, got {other:?}")
            }
        }

        drop(first);
        std::fs::remove_file(&path).ok();
    }

    /// Dropping the guard (releasing the lock) must allow a subsequent
    /// acquire to succeed.
    #[test]
    fn acquire_succeeds_again_after_the_holder_is_dropped() {
        let path = make_temp_lock_path("release-then-reacquire");

        let first = acquire_execution_lock_at(&path).expect("first acquire must succeed");
        drop(first);

        let second = acquire_execution_lock_at(&path);
        assert!(
            second.is_ok(),
            "acquiring after the prior guard dropped must succeed, got {second:?}"
        );

        drop(second);
        std::fs::remove_file(&path).ok();
    }

    /// A third acquire after the second guard (from the previous-style
    /// sequence) is also dropped must succeed again — proves release is
    /// not a one-shot fluke of `Drop` running exactly once ever.
    #[test]
    fn lock_can_be_acquired_and_released_repeatedly() {
        let path = make_temp_lock_path("repeated-cycles");

        for _ in 0..3 {
            let guard = acquire_execution_lock_at(&path).expect("acquire should succeed");
            drop(guard);
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn acquire_creates_missing_parent_directories() {
        let base = make_temp_lock_path("missing-parents");
        let nested_path = base.join("nested").join("dir").join("execution.lock");

        let guard = acquire_execution_lock_at(&nested_path);
        assert!(
            guard.is_ok(),
            "expected missing parent directories to be created, got {guard:?}"
        );
        assert!(nested_path.exists());

        drop(guard);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn default_lock_path_uses_home_and_the_established_app_support_convention() {
        let path = default_lock_path().expect("HOME should be set in test environment");
        let rendered = path.to_string_lossy();
        assert!(rendered.contains("Library/Application Support/Glomeris"));
        assert!(rendered.ends_with("execution.lock"));
    }
}
