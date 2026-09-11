//! `pgrep`-backed probe for whether a resource's owning tool has a
//! currently-running process.

use std::process::{Command, Output};
use std::time::Duration;

use super::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::evidence::OwningTool;

pub trait ToolLivenessProbe {
    fn is_running(&self, tool: OwningTool, timeout: Duration) -> ProbeOutcome<bool>;
}

/// Real [`ToolLivenessProbe`] backed by `pgrep -x <process_name>`.
pub struct PgrepToolLivenessProbe;

impl ToolLivenessProbe for PgrepToolLivenessProbe {
    fn is_running(&self, tool: OwningTool, timeout: Duration) -> ProbeOutcome<bool> {
        let Some(process_name) = daemon_process_name(tool) else {
            // Tools with no persistent daemon have no meaningful
            // "is it running" answer — see daemon_process_name's doc
            // comment. Report this without shelling out at all.
            return ProbeOutcome::Unavailable(ProbeReason::ToolNotRunning);
        };

        let mut command = Command::new("pgrep");
        command.arg("-x").arg(process_name);

        match run_with_timeout(command, timeout) {
            CommandOutcome::NotFound => ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
            CommandOutcome::TimedOut => ProbeOutcome::Unavailable(ProbeReason::TimedOut),
            CommandOutcome::SpawnFailed => ProbeOutcome::Unavailable(ProbeReason::Failed),
            CommandOutcome::Completed(output) => interpret_pgrep_output(&output),
        }
    }
}

/// Maps an [`OwningTool`] to the process name `pgrep -x` should match
/// against a persistent, always-on daemon for that tool.
///
/// `Cargo`, `Npm`, `Pnpm`, and `Yarn` are plain CLI invocations with no
/// background daemon — there is no process whose mere existence means
/// "the tool is active" the way `com.docker.backend` does for Docker.
/// `Homebrew` is the same: `brew` runs and exits, it doesn't stay
/// resident. These are structurally, permanently "not running as a
/// daemon", not an unknown — so `None` here, not a guess at a process
/// name that would never realistically match.
fn daemon_process_name(tool: OwningTool) -> Option<&'static str> {
    match tool {
        OwningTool::Xcode => Some("Xcode"),
        // Docker Desktop's actual backend daemon process on macOS, not
        // the `Docker` GUI app wrapper, which can be closed while the
        // backend (and therefore the resources it owns) is still live.
        OwningTool::Docker => Some("com.docker.backend"),
        OwningTool::Homebrew
        | OwningTool::Cargo
        | OwningTool::Npm
        | OwningTool::Pnpm
        | OwningTool::Yarn
        | OwningTool::None => None,
    }
}

fn interpret_pgrep_output(output: &Output) -> ProbeOutcome<bool> {
    if output.status.success() {
        ProbeOutcome::Observed(true)
    } else if output.stdout.is_empty() && output.stderr.is_empty() {
        // pgrep exits 1 with no output when nothing matched — a
        // successful probe with a negative result.
        ProbeOutcome::Observed(false)
    } else {
        ProbeOutcome::Unavailable(ProbeReason::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn exit_status(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn non_daemon_tools_are_unavailable_tool_not_running_without_shelling_out() {
        for tool in [
            OwningTool::Cargo,
            OwningTool::Npm,
            OwningTool::Pnpm,
            OwningTool::Yarn,
            OwningTool::Homebrew,
            OwningTool::None,
        ] {
            assert_eq!(
                PgrepToolLivenessProbe.is_running(tool, Duration::from_secs(5)),
                ProbeOutcome::Unavailable(ProbeReason::ToolNotRunning)
            );
        }
    }

    #[test]
    fn success_exit_is_observed_true() {
        let output = Output {
            status: exit_status(0),
            stdout: b"1234\n".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(
            interpret_pgrep_output(&output),
            ProbeOutcome::Observed(true)
        );
    }

    #[test]
    fn no_match_exit_with_no_output_is_observed_false() {
        let output = Output {
            status: exit_status(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(
            interpret_pgrep_output(&output),
            ProbeOutcome::Observed(false)
        );
    }

    #[test]
    fn usage_error_with_stderr_is_unavailable_failed() {
        let output = Output {
            status: exit_status(2),
            stdout: Vec::new(),
            stderr: b"pgrep: illegal option\n".to_vec(),
        };
        assert_eq!(
            interpret_pgrep_output(&output),
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }
}
