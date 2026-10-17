//! Minimal subprocess timeout helper shared by every real correlation
//! probe (both `lsof` invocations, `git`, `pgrep`) and, since HORO-1511,
//! by the worktree branch probe in [`crate::workspace`] — which is
//! outside this module on purpose (see that module's header) but has the
//! same need to run `git` without hanging.
//!
//! Deliberately no async runtime or extra dependency — HORO-949's
//! contract prefers tool-native, minimal-footprint execution. Approach:
//! spawn the child with piped stdout/stderr, drain those pipes on two
//! background threads (so a chatty child can never deadlock on a full
//! pipe buffer while we poll), and poll `try_wait()` on the main thread
//! until either the child exits or the deadline passes. On timeout the
//! child is killed AND reaped (`wait()` after `kill()`) so a timed-out
//! probe never leaves a zombie process behind.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Outcome of attempting to run one subprocess under a timeout.
pub(crate) enum CommandOutcome {
    /// The child ran to completion within the deadline.
    Completed(Output),
    /// The executable could not be found (`ErrorKind::NotFound` on spawn).
    NotFound,
    /// The child did not exit before the deadline; it has been killed
    /// and reaped.
    TimedOut,
    /// Spawning or waiting on the child failed for some other reason
    /// (e.g. permission denied, or the pipe-reader thread panicked).
    SpawnFailed,
}

/// Run `command` with piped stdout/stderr, waiting up to `timeout` for it
/// to complete.
pub(crate) fn run_with_timeout(mut command: Command, timeout: Duration) -> CommandOutcome {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child: Child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CommandOutcome::NotFound,
        Err(_) => return CommandOutcome::SpawnFailed,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout_cancel = cancel.clone();
    let stderr_cancel = cancel.clone();
    let stdout_thread = std::thread::spawn(move || drain(stdout, stdout_cancel));
    let stderr_thread = std::thread::spawn(move || drain(stderr, stderr_cancel));

    let deadline = Instant::now() + timeout;
    let exit_status: Option<ExitStatus> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                cancel.store(true, Ordering::Release);
                return CommandOutcome::SpawnFailed;
            }
        }
    };

    // Reap before pipe joins: a living child (or descendant) otherwise keeps EOF away.
    if exit_status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        cancel.store(true, Ordering::Release);
    }
    cancel.store(true, Ordering::Release);
    let (Ok(stdout), Ok(stderr)) = (stdout_thread.join(), stderr_thread.join()) else {
        return CommandOutcome::SpawnFailed;
    };
    if exit_status.is_none() {
        return CommandOutcome::TimedOut;
    }
    let (Ok(stdout), Ok(stderr)) = (stdout, stderr) else {
        return CommandOutcome::SpawnFailed;
    };
    match exit_status {
        Some(status) => CommandOutcome::Completed(Output {
            status,
            stdout,
            stderr,
        }),
        None => CommandOutcome::TimedOut,
    }
}

/// HORO-1916: `run_with_timeout` alone is not robust to real host
/// contention — e.g. 25+ `glomeris` child processes each forking their own
/// `ps`/`sysctl` probe. Measured evidence (host at 155-167 GiB free,
/// otherwise idle, running `tests/managed_resource_lifecycle.rs` under
/// default parallelism): failures cluster right at the 3s boundary
/// (3015-3046ms elapsed), not far beyond it — a brief scheduling delay
/// losing the race against a fixed deadline, not a hung or dead command.
///
/// This retries exactly once, and only on [`CommandOutcome::TimedOut`] —
/// `NotFound`/`SpawnFailed` are deterministic (the binary is missing, or
/// spawning itself failed) and retrying them wastes the remaining budget
/// for no chance of a different outcome. The retry is bounded by
/// `overall_budget`, not by attempt count alone, so a caller's total
/// added latency is capped regardless of how slow the first attempt was —
/// this is what keeps a liveness probe from eating into the correlation
/// budget (HORO-1912) or starving other candidates, per the founder
/// directive accompanying this ticket (Jira comment 27746).
///
/// `build` is called once per attempt because [`Command`] is not
/// `Clone`.
pub(crate) fn run_with_timeout_retrying(
    mut build: impl FnMut() -> Command,
    timeout: Duration,
    overall_budget: Duration,
) -> CommandOutcome {
    let start = Instant::now();
    let outcome = run_with_timeout(build(), timeout);
    if !matches!(outcome, CommandOutcome::TimedOut) {
        return outcome;
    }
    let remaining = overall_budget.saturating_sub(start.elapsed());
    if remaining.is_zero() {
        return outcome;
    }
    run_with_timeout(build(), remaining.min(timeout))
}

fn drain(mut pipe: impl Read + AsRawFd, cancel: Arc<AtomicBool>) -> std::io::Result<Vec<u8>> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags < 0
        || unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(buffer),
            Ok(n) => {
                if buffer.len() + n > 8 * 1024 * 1024 {
                    return Err(std::io::Error::other("probe output budget exceeded"));
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if cancel.load(Ordering::Acquire) {
                    return Err(std::io::Error::other("probe pipes did not reach EOF"));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_command_returns_its_output() {
        let command = Command::new("true");
        match run_with_timeout(command, Duration::from_secs(5)) {
            CommandOutcome::Completed(output) => assert!(output.status.success()),
            _ => panic!("expected Completed"),
        }
    }

    #[test]
    fn missing_executable_is_not_found() {
        let command = Command::new("glomeris-definitely-not-a-real-binary-xyz");
        match run_with_timeout(command, Duration::from_secs(5)) {
            CommandOutcome::NotFound => {}
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn slow_command_times_out_and_reaps_child() {
        let mut command = Command::new("sleep");
        command.arg("5");
        match run_with_timeout(command, Duration::from_millis(100)) {
            CommandOutcome::TimedOut => {}
            _ => panic!("expected TimedOut"),
        }
    }
}
#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn timeout_then_success_on_retry_succeeds() {
        let attempts = AtomicUsize::new(0);
        let outcome = run_with_timeout_retrying(
            || {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                let mut c = Command::new("/bin/sh");
                // First attempt outlives its 100ms timeout; the retry
                // exits immediately.
                c.args(["-c", if n == 0 { "sleep 0.3" } else { "exit 0" }]);
                c
            },
            Duration::from_millis(100),
            Duration::from_secs(2),
        );
        assert!(matches!(outcome, CommandOutcome::Completed(o) if o.status.success()));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "expected exactly one retry"
        );
    }

    #[test]
    fn timeout_twice_is_still_an_error() {
        let attempts = AtomicUsize::new(0);
        let outcome = run_with_timeout_retrying(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let mut c = Command::new("/bin/sh");
                c.args(["-c", "sleep 5"]);
                c
            },
            Duration::from_millis(100),
            Duration::from_millis(400),
        );
        assert!(matches!(outcome, CommandOutcome::TimedOut));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn not_found_is_never_retried() {
        let attempts = AtomicUsize::new(0);
        let outcome = run_with_timeout_retrying(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Command::new("glomeris-definitely-not-a-real-binary-xyz")
            },
            Duration::from_millis(100),
            Duration::from_secs(2),
        );
        assert!(matches!(outcome, CommandOutcome::NotFound));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a deterministic NotFound must not consume retry budget"
        );
    }

    #[test]
    fn exhausted_budget_skips_the_second_attempt() {
        let attempts = AtomicUsize::new(0);
        let outcome = run_with_timeout_retrying(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                let mut c = Command::new("/bin/sh");
                c.args(["-c", "sleep 5"]);
                c
            },
            // First attempt alone consumes the entire overall budget.
            Duration::from_millis(100),
            Duration::from_millis(100),
        );
        assert!(matches!(outcome, CommandOutcome::TimedOut));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "no budget remains for a second attempt"
        );
    }

    #[test]
    fn total_elapsed_stays_within_budget_plus_epsilon() {
        let start = Instant::now();
        let outcome = run_with_timeout_retrying(
            || {
                let mut c = Command::new("/bin/sh");
                c.args(["-c", "sleep 5"]);
                c
            },
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        assert!(matches!(outcome, CommandOutcome::TimedOut));
        // Bounded by overall_budget, not by 2x the per-attempt timeout
        // (which would be 600ms+ here).
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "elapsed {:?} exceeded the overall budget plus a reasonable epsilon",
            start.elapsed()
        );
    }
}
#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn enforced_deadline_does_not_wait_for_child_pipe_eof() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "exec sleep 3"]);
        let start = Instant::now();
        assert!(matches!(
            run_with_timeout(c, Duration::from_millis(100)),
            CommandOutcome::TimedOut
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn surviving_descendant_pipe_is_incomplete_not_a_success() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "sleep 1 & exit 0"]);
        let start = Instant::now();
        assert!(matches!(
            run_with_timeout(c, Duration::from_millis(100)),
            CommandOutcome::SpawnFailed
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
