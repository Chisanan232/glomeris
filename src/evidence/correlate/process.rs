//! `lsof`-backed probe for which processes have a resource as their
//! current working directory.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::open_files::run_lsof;
use super::ProcessRef;
use crate::evidence::probe::ProbeOutcome;

pub trait ProcessCwdProbe {
    fn processes_with_cwd_under(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<Vec<ProcessRef>>;
}

/// Real [`ProcessCwdProbe`], backed by the same `lsof` tool as
/// [`super::open_files::LsofOpenFileProbe`], with `-a -d cwd` added:
/// `-d cwd` restricts matches to a process's cwd file descriptor, and
/// `-a` ANDs that restriction together with `+D <path>` (without `-a`,
/// lsof would OR the two selectors and also return every open-file match
/// from the sibling open-files probe). Reuses `run_lsof`'s output
/// interpretation and field parsing — same tool, same exit-code/stderr
/// semantics, only the selector flags differ.
pub struct LsofProcessCwdProbe;

impl ProcessCwdProbe for LsofProcessCwdProbe {
    fn processes_with_cwd_under(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> ProbeOutcome<Vec<ProcessRef>> {
        run_lsof(build_command(path), timeout)
    }
}

fn build_command(path: &Path) -> Command {
    let mut command = Command::new("lsof");
    command
        .args(["-a", "-d", "cwd", "-F", "pcn", "+D"])
        .arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_lsof_command_with_cwd_selector_anded_to_path() {
        let command = build_command(Path::new("/tmp/example"));
        assert_eq!(command.get_program(), "lsof");
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec!["-a", "-d", "cwd", "-F", "pcn", "+D", "/tmp/example"]
        );
    }
}
