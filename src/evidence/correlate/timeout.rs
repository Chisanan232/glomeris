//! Minimal subprocess timeout helper shared by every real correlation
//! probe (both `lsof` invocations, `git`, `pgrep`).
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
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Outcome of attempting to run one subprocess under a timeout.
pub(super) enum CommandOutcome {
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
pub(super) fn run_with_timeout(mut command: Command, timeout: Duration) -> CommandOutcome {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child: Child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CommandOutcome::NotFound,
        Err(_) => return CommandOutcome::SpawnFailed,
    };

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

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
            Err(_) => return CommandOutcome::SpawnFailed,
        }
    };

    let (Ok(stdout), Ok(stderr)) = (stdout_thread.join(), stderr_thread.join()) else {
        return CommandOutcome::SpawnFailed;
    };

    match exit_status {
        Some(status) => CommandOutcome::Completed(Output {
            status,
            stdout,
            stderr,
        }),
        None => {
            // Timed out: kill and reap so the child never becomes a
            // zombie.
            let _ = child.kill();
            let _ = child.wait();
            CommandOutcome::TimedOut
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
