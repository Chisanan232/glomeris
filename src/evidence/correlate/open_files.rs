//! `lsof`-backed probe for which processes have a resource's subtree
//! open.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use super::timeout::{run_with_timeout, CommandOutcome};
use super::ProcessRef;
use crate::evidence::probe::{ProbeOutcome, ProbeReason};

pub trait OpenFileProbe {
    fn processes_with_open_files_under(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<Vec<ProcessRef>>;
}

/// Real [`OpenFileProbe`] backed by `lsof +D <path>`.
pub struct LsofOpenFileProbe;

impl OpenFileProbe for LsofOpenFileProbe {
    fn processes_with_open_files_under(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<Vec<ProcessRef>> {
        let mut command = Command::new("lsof");
        // `-F pcn` requests machine-parseable field output (one field per
        // line, tagged by its first byte) instead of lsof's column-
        // aligned default, which is fragile to parse when COMMAND names
        // contain spaces or get truncated.
        command.args(["-F", "pcn", "+D"]).arg(path);
        run_lsof(command, timeout)
    }
}

/// Runs an `lsof -F pcn ...` invocation and interprets its result.
///
/// `lsof` exits non-zero both when nothing matched (empty stdout, empty
/// stderr — a successful probe with a negative result) AND when it
/// rejects its arguments or the target vanished mid-probe (non-empty
/// stderr). Those two cases are distinguished by stderr content, never
/// by exit code alone — collapsing them would turn "the probe failed"
/// into a false "nothing is active here".
pub(super) fn run_lsof(command: Command, timeout: Duration) -> ProbeOutcome<Vec<ProcessRef>> {
    match run_with_timeout(command, timeout) {
        CommandOutcome::NotFound => ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
        CommandOutcome::TimedOut => ProbeOutcome::Unavailable(ProbeReason::TimedOut),
        CommandOutcome::SpawnFailed => ProbeOutcome::Unavailable(ProbeReason::Failed),
        CommandOutcome::Completed(output) => interpret_lsof_output(&output),
    }
}

fn interpret_lsof_output(output: &Output) -> ProbeOutcome<Vec<ProcessRef>> {
    if output.status.success() || output.stderr.is_empty() {
        ProbeOutcome::Observed(parse_lsof_field_output(&output.stdout))
    } else {
        ProbeOutcome::Unavailable(ProbeReason::Failed)
    }
}

/// Parses `lsof -F pcn` output into one [`ProcessRef`] per distinct
/// process record. Each record starts with a `p<pid>` line; a `c<name>`
/// line anywhere before the next `p` line sets that record's command
/// name. Any other field (`f...`, `n...`, etc.) is ignored — this probe
/// only needs pid + command.
fn parse_lsof_field_output(stdout: &[u8]) -> Vec<ProcessRef> {
    let text = String::from_utf8_lossy(stdout);
    let mut processes = Vec::new();
    let mut current: Option<(u32, Option<String>)> = None;

    for line in text.lines() {
        let Some((tag, value)) = line.split_at_checked(1) else {
            continue;
        };
        match tag {
            "p" => {
                if let Some((pid, command)) = current.take() {
                    processes.push(ProcessRef {
                        pid,
                        command: command.unwrap_or_default(),
                    });
                }
                if let Ok(pid) = value.parse::<u32>() {
                    current = Some((pid, None));
                }
            }
            "c" => {
                if let Some((_, command)) = current.as_mut() {
                    *command = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some((pid, command)) = current.take() {
        processes.push(ProcessRef {
            pid,
            command: command.unwrap_or_default(),
        });
    }

    processes
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
    fn parses_multiple_process_records() {
        let stdout = b"p123\ncsomeproc\nfcwd\nn/some/path\np456\ncother\nf3\nn/some/path/x\n";
        let processes = parse_lsof_field_output(stdout);
        assert_eq!(
            processes,
            vec![
                ProcessRef {
                    pid: 123,
                    command: "someproc".to_string()
                },
                ProcessRef {
                    pid: 456,
                    command: "other".to_string()
                },
            ]
        );
    }

    #[test]
    fn record_with_no_command_line_defaults_to_empty_string() {
        let stdout = b"p789\nfcwd\nn/some/path\n";
        let processes = parse_lsof_field_output(stdout);
        assert_eq!(
            processes,
            vec![ProcessRef {
                pid: 789,
                command: String::new()
            }]
        );
    }

    #[test]
    fn empty_stdout_parses_to_empty_vec() {
        assert_eq!(parse_lsof_field_output(b""), Vec::new());
    }

    #[test]
    fn successful_run_with_output_is_observed_with_parsed_processes() {
        let output = Output {
            status: exit_status(0),
            stdout: b"p1\nccmd\n".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(
            interpret_lsof_output(&output),
            ProbeOutcome::Observed(vec![ProcessRef {
                pid: 1,
                command: "cmd".to_string()
            }])
        );
    }

    #[test]
    fn nonzero_exit_with_empty_stdout_is_observed_empty_not_a_failure() {
        let output = Output {
            status: exit_status(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(
            interpret_lsof_output(&output),
            ProbeOutcome::Observed(Vec::new())
        );
    }

    #[test]
    fn nonzero_exit_with_stderr_content_is_unavailable_failed() {
        let output = Output {
            status: exit_status(1),
            stdout: Vec::new(),
            stderr: b"lsof: WARNING: can't stat(...)\n".to_vec(),
        };
        assert_eq!(
            interpret_lsof_output(&output),
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }
}
