//! Advisory, per-user execution lock (HORO-1054).
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

/// Resolves the daemon's single-instance lock file path (HORO-1907).
///
/// A separate file from [`default_lock_path`], deliberately: that lock
/// guards real destructive EXECUTE operations, and `daemon run` must never
/// contend with — or be mistaken for a holder of — that lock. The monitor
/// loop never executes anything (ADR-0001 §14: statvfs sampling only), so
/// its own lock exists purely to stop two overlapping `daemon run`
/// processes from both writing `history.tsv`/`monitor.log` at once.
///
/// What is actually known, versus assumed: `daemon_run()` has never had any
/// instance guard, and `~/Library/Logs/Glomeris/monitor.log` was observed
/// (2026-10-16 incident) with word-level interleaving consistent with two
/// writers racing on the same file offset. The EXACT mechanism that put two
/// `daemon run` processes in flight at once — a launchd restart racing a
/// slow-exiting prior instance, an unload/reload during a plist edit, a
/// manual invocation overlapping the loaded agent, or something else — was
/// NOT reproduced or captured live, and this fix does not depend on which
/// one it was: a single-instance guard is the correct defense against any
/// of them, not only the specific one first guessed.
pub fn default_daemon_lock_path() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| io::Error::other("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Glomeris")
        .join("daemon.lock"))
}

/// Outcome of [`acquire_daemon_guard`] (HORO-1907).
///
/// Four, not two, because "the lock could not be taken" is not one outcome:
/// a disk that is out of space (the exact condition this daemon exists to
/// report) must not be confused with a permissions error, a read-only
/// filesystem, or a locking-table exhaustion — only the first of those may
/// run unguarded. See [`is_fail_open_lock_error`].
#[derive(Debug)]
pub enum DaemonGuardOutcome {
    /// Acquired. Holds this for the daemon's lifetime; dropping it releases
    /// the lock (see [`ExecutionLockGuard`]).
    Held(ExecutionLockGuard),
    /// Could not be taken for a reason where running unguarded is the
    /// correct call anyway (see [`is_fail_open_lock_error`]), or `HOME` was
    /// unset — the same degraded-but-keep-monitoring posture `daemon_run`
    /// already takes elsewhere (episode state, `history_path`) rather than
    /// a new, stricter failure mode introduced only for this lock.
    ProceedUnguarded(io::Error),
    /// Another `daemon run` already holds the lock. The caller should exit
    /// rather than start a second monitor loop.
    ExitAlreadyRunning,
    /// Could not be taken for a reason that is NOT disk-space-related —
    /// permissions, a read-only filesystem, a locking-table exhaustion.
    /// Failing open here would silently let an unbounded number of
    /// unguarded monitors accumulate for as long as the underlying
    /// condition persists, which is exactly what this guard exists to
    /// prevent. The caller should exit non-zero instead, so launchd's own
    /// restart/backoff — not a second concurrent monitor — is what happens
    /// next.
    ExitLockError(io::Error),
}

/// True only for the disk-space-exhaustion errnos (`ENOSPC`, `EDQUOT`).
///
/// The daemon's entire purpose is reporting a disk that is running out of
/// space, and creating [`default_daemon_lock_path`]'s parent directory or
/// the lock file itself can fail for that exact reason on a first run (an
/// already-existing lock file's directory needs no further allocation to
/// open and `flock`, so this case is narrower than it may look — it is a
/// first-run/missing-directory condition, not a steady-state one). Every
/// other I/O error is a configuration or environment problem that
/// ENOSPC-style fail-open reasoning does not apply to.
pub fn is_fail_open_lock_error(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(Errno::from_raw),
        Some(Errno::ENOSPC) | Some(Errno::EDQUOT)
    )
}

/// Acquires the daemon's single-instance guard at
/// [`default_daemon_lock_path`]. Pure dispatch over [`acquire_execution_lock_at`]
/// and [`is_fail_open_lock_error`], so the decision logic is testable
/// without `cfg(macos)` and without spawning the real daemon loop — see
/// this module's tests for the positive (second acquire busy) and negative
/// (acquire after release) controls, and `tests::classifier` for the
/// errno split.
pub fn acquire_daemon_guard() -> DaemonGuardOutcome {
    let path = match default_daemon_lock_path() {
        Ok(p) => p,
        // HOME unset: not disk-space-related, but `daemon_run` already
        // tolerates a missing HOME everywhere else by degrading rather
        // than refusing to monitor (see its `episode_state_path`/
        // `history_path` handling) — this lock follows the same posture
        // rather than introducing a new, stricter one.
        Err(e) => return DaemonGuardOutcome::ProceedUnguarded(e),
    };
    acquire_daemon_guard_at(&path)
}

/// [`acquire_daemon_guard`] against a caller-supplied path — the seam this
/// module's tests use to stay hermetic.
pub fn acquire_daemon_guard_at(path: &Path) -> DaemonGuardOutcome {
    match acquire_execution_lock_at(path) {
        Ok(guard) => DaemonGuardOutcome::Held(guard),
        Err(LockError::AlreadyHeld) => DaemonGuardOutcome::ExitAlreadyRunning,
        Err(LockError::Io(e)) if is_fail_open_lock_error(&e) => {
            DaemonGuardOutcome::ProceedUnguarded(e)
        }
        Err(LockError::Io(e)) => DaemonGuardOutcome::ExitLockError(e),
    }
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

    #[test]
    fn default_daemon_lock_path_is_distinct_from_the_execution_lock_path() {
        let daemon = default_daemon_lock_path().expect("HOME should be set in test environment");
        let execution = default_lock_path().expect("HOME should be set in test environment");

        assert_ne!(
            daemon, execution,
            "the daemon's single-instance lock must never be the same file as the \
             real-execution lock, or the monitor loop would contend with (or be \
             mistaken for a holder of) a destructive-operation guard"
        );
        let rendered = daemon.to_string_lossy();
        assert!(rendered.contains("Library/Application Support/Glomeris"));
        assert!(rendered.ends_with("daemon.lock"));
    }

    // -- acquire_daemon_guard_at / DaemonGuardOutcome (HORO-1907) --------

    /// Mutation target: deleting the `Err(LockError::AlreadyHeld) =>
    /// ExitAlreadyRunning` arm in [`acquire_daemon_guard_at`] turns this red
    /// (it would instead return `Held`, which this test's `matches!` would
    /// reject). This is the regression test the single-instance guard
    /// exists for — the real `daemon_run` wiring is a thin, untested call
    /// site on top of this.
    #[test]
    fn daemon_guard_reports_another_instance_while_first_is_held() {
        let path = make_temp_lock_path("daemon-guard-busy");

        let first = acquire_daemon_guard_at(&path);
        assert!(matches!(first, DaemonGuardOutcome::Held(_)));

        let second = acquire_daemon_guard_at(&path);
        assert!(
            matches!(second, DaemonGuardOutcome::ExitAlreadyRunning),
            "expected ExitAlreadyRunning while the first guard is held, got {second:?}"
        );

        drop(first);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn daemon_guard_succeeds_again_after_the_holder_is_dropped() {
        let path = make_temp_lock_path("daemon-guard-release-reacquire");

        let first = acquire_daemon_guard_at(&path);
        assert!(matches!(first, DaemonGuardOutcome::Held(_)));
        drop(first);

        let second = acquire_daemon_guard_at(&path);
        assert!(
            matches!(second, DaemonGuardOutcome::Held(_)),
            "expected Held after the prior guard dropped, got {second:?}"
        );

        drop(second);
        std::fs::remove_file(&path).ok();
    }

    /// The crash/PID-reuse/stale-lock control: a lock held by a process
    /// that is then `SIGKILL`ed — not dropped cleanly — must still release,
    /// because `flock(2)` is tied to the open file description and the
    /// kernel closes every fd of a killed process. There is no PID or
    /// liveness check anywhere in this module because none is needed; this
    /// test proves that rather than asserting it.
    ///
    /// The external holder is a `python3 -c` one-liner, not this same test
    /// binary re-invoked via [`std::env::current_exe`]. An earlier version
    /// of this test did exactly that and failed twice in CI
    /// (CI_FAILED, PR #180): once because `--exact`'s required test name
    /// differs from `module_path!()`'s crate-prefixed output, and the fix
    /// for *that* still left this parent polling `acquire_execution_lock_at`
    /// before the child had synchronized anything — racing its own
    /// transient successful acquires against the child's startup, with no
    /// way to tell "child hasn't locked yet" apart from "child lost the
    /// race to us" from the outside. `python3` is present on every runner
    /// and dev machine this crate already requires for CI, needs no
    /// `--exact` name to stay in sync with this function if it is ever
    /// renamed, and its one `print("locked")` before sleeping gives a
    /// deterministic handshake instead of a poll racing a cold start.
    #[test]
    fn daemon_guard_releases_automatically_when_the_holder_is_killed() {
        use std::io::{BufRead, BufReader};

        let path = make_temp_lock_path("daemon-guard-crash-recovery");

        let mut child = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import fcntl, sys, time\n\
                 f = open(sys.argv[1], 'a')\n\
                 fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n\
                 print('locked', flush=True)\n\
                 time.sleep(60)\n",
            )
            .arg(&path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("failed to spawn the python3 lock-holding child");

        // Deterministic handshake: block until the child has actually
        // taken the flock and said so, rather than polling for it.
        let stdout = child.stdout.take().expect("child stdout must be piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("failed to read the child's handshake line");
        assert_eq!(
            line.trim(),
            "locked",
            "expected the child to report it acquired the lock before this \
             test asserts contention against it"
        );

        // Negative control: our own guard must now see AlreadyHeld — a
        // real flock(2) conflict against an independent process, not a
        // simulated one.
        let contended = acquire_daemon_guard_at(&path);
        assert!(
            matches!(contended, DaemonGuardOutcome::ExitAlreadyRunning),
            "expected ExitAlreadyRunning while the external holder's flock \
             is held, got {contended:?}"
        );

        // Positive control: SIGKILL the child (not a graceful exit) and
        // confirm the lock becomes acquirable again without any staleness
        // check on our part — the kernel's own fd cleanup is what releases
        // it.
        child.kill().expect("failed to SIGKILL the holder process");
        child.wait().ok();

        let mut reacquired = false;
        for _ in 0..100 {
            if matches!(acquire_daemon_guard_at(&path), DaemonGuardOutcome::Held(_)) {
                reacquired = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            reacquired,
            "expected the lock to be acquirable again after the holder was killed"
        );

        std::fs::remove_file(&path).ok();
    }

    // -- is_fail_open_lock_error (HORO-1907) ------------------------------

    #[test]
    fn enospc_is_classified_as_fail_open() {
        let e = io::Error::from_raw_os_error(Errno::ENOSPC as i32);
        assert!(is_fail_open_lock_error(&e));
    }

    #[test]
    fn edquot_is_classified_as_fail_open() {
        let e = io::Error::from_raw_os_error(Errno::EDQUOT as i32);
        assert!(is_fail_open_lock_error(&e));
    }

    /// The founder's explicit concern: a permissions problem (or any other
    /// non-disk-space I/O error) must NOT fail open, or an unrelated
    /// misconfiguration would silently let unguarded monitors accumulate
    /// for as long as it persists.
    #[test]
    fn permission_denied_is_not_classified_as_fail_open() {
        let e = io::Error::from_raw_os_error(Errno::EACCES as i32);
        assert!(!is_fail_open_lock_error(&e));
    }

    #[test]
    fn read_only_filesystem_is_not_classified_as_fail_open() {
        let e = io::Error::from_raw_os_error(Errno::EROFS as i32);
        assert!(!is_fail_open_lock_error(&e));
    }
}
