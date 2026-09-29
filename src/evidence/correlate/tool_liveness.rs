//! Whether a resource's owning tool is running right now.
//!
//! # Two different questions
//!
//! "Is Xcode open" and "is there a Docker daemon" are not the same shape of
//! question, and asking them the same way is how this probe came to assert
//! that a live daemon was down (HORO-1562).
//!
//! Xcode is a macOS application: it runs as a process on this host, under a
//! name that is a property of the product, so `pgrep -x Xcode` is a direct
//! observation. A Docker daemon is a *service reachable over a socket*. Docker
//! Desktop happens to run it behind `com.docker.backend` on this host; Colima
//! runs it inside a Lima VM, where no host process names it at all; OrbStack
//! names it something else; `DOCKER_HOST` and `docker context` can put it on
//! another machine entirely. Matching a process name answers "is Docker
//! Desktop installed and started", which is a different question that happens
//! to have the same answer on one vendor's setup.
//!
//! So Docker is asked instead of matched: the client already knows whether its
//! server answered, and `docker version` is the read-only way to make it say
//! so. That is runtime-agnostic by construction — it needs no allowlist of
//! process names per Docker distribution, and a new way of hosting a daemon
//! cannot defeat it.
//!
//! # Why the distinction is load-bearing rather than cosmetic
//!
//! `pgrep` exits 1 with no output when nothing matched, which this module
//! reads — correctly, for a host process — as the positive observation
//! `Observed(false)`. Routed through that path, a daemon running under any
//! name but Docker Desktop's produced a confident claim that it was not
//! running. `policy::classify` treats a live owning tool as active use, and
//! for a [`crate::evidence::ResourceLocator::Tool`] resource it is the *only*
//! active-use signal available, because the three path-based probes
//! structurally do not run without a path. A false `Observed(false)` therefore
//! removed the sole barrier between a Docker resource and `AutoSafe`, on every
//! machine not running Docker Desktop.
//!
//! An inapplicable probe became a negative observation. Which is the exact
//! thing this campaign exists to prevent, arrived at from the inside.

use std::process::{Command, Output};
use std::time::Duration;

use super::timeout::{run_with_timeout, CommandOutcome};
use crate::evidence::probe::{ProbeOutcome, ProbeReason};
use crate::evidence::OwningTool;

pub trait ToolLivenessProbe {
    fn is_running(&self, tool: OwningTool, timeout: Duration) -> ProbeOutcome<bool>;
}

/// Real [`ToolLivenessProbe`], answering each tool the way that tool can
/// actually be answered — see [`liveness_method`].
pub struct SystemToolLivenessProbe;

/// What `docker version` is asked. The template is what makes this a question
/// about the *server*: a client with no reachable daemon cannot render it, and
/// says why on stderr.
const DOCKER_SERVER_QUERY: &[&str] = &["version", "--format", "{{.Server.Version}}"];

impl ToolLivenessProbe for SystemToolLivenessProbe {
    fn is_running(&self, tool: OwningTool, timeout: Duration) -> ProbeOutcome<bool> {
        match liveness_method(tool) {
            LivenessMethod::HostProcess(name) => host_process_liveness(name, timeout),
            LivenessMethod::AskDockerServer => docker_server_liveness(timeout),
            // Tools with no persistent daemon have no meaningful "is it
            // running" answer — see `LivenessMethod::NoDaemon`. Reported
            // without shelling out at all.
            LivenessMethod::NoDaemon => ProbeOutcome::Unavailable(ProbeReason::ToolNotRunning),
        }
    }
}

/// How "is this tool running" can be answered for one [`OwningTool`].
///
/// An enum rather than an `Option<&str>` so that [`liveness_method`]'s match
/// is exhaustive over a closed set of *methods* as well as of tools. A new
/// tool cannot compile without choosing one, and — the reason this shape was
/// worth the extra type — Docker cannot fall back to a process name or to
/// `NoDaemon` by anyone's oversight, because neither is spelled the same as
/// the arm it needs.
enum LivenessMethod {
    /// Match a host process name with `pgrep -x`. For a tool that really does
    /// run as a named process on this machine.
    HostProcess(&'static str),
    /// Ask the Docker client whether its server answered.
    AskDockerServer,
    /// The tool has no resident daemon, so nothing about it can be running.
    ///
    /// `Cargo`, `Npm`, `Pnpm`, `Yarn` and `Homebrew` are plain CLI
    /// invocations: they run and exit, and there is no process whose mere
    /// existence means "the tool is active". Same for every tool added by
    /// HORO-1543 — `pip`, `uv`, `go`, `mvn`, `swift build`.
    ///
    /// Gradle is the one that invites a guess, because it really does leave a
    /// long-lived daemon behind. But a Gradle daemon is per-project and named
    /// for a JVM, not for the shared `~/.gradle/caches` directory this would
    /// be asked about, so matching on it would report the liveness of
    /// something other than the resource in question.
    ///
    /// These are structurally, permanently not-a-daemon rather than unknown,
    /// which is why the answer is `Unavailable(ToolNotRunning)` and not a
    /// guess at a process name that could never match.
    NoDaemon,
}

fn liveness_method(tool: OwningTool) -> LivenessMethod {
    match tool {
        OwningTool::Xcode => LivenessMethod::HostProcess("Xcode"),
        OwningTool::Docker => LivenessMethod::AskDockerServer,
        OwningTool::Homebrew
        | OwningTool::Cargo
        | OwningTool::Npm
        | OwningTool::Pnpm
        | OwningTool::Yarn
        | OwningTool::Pip
        | OwningTool::Uv
        | OwningTool::Go
        | OwningTool::Gradle
        | OwningTool::Maven
        | OwningTool::SwiftPm
        | OwningTool::None => LivenessMethod::NoDaemon,
    }
}

fn host_process_liveness(process_name: &str, timeout: Duration) -> ProbeOutcome<bool> {
    let mut command = Command::new("pgrep");
    command.arg("-x").arg(process_name);

    match run_with_timeout(command, timeout) {
        CommandOutcome::NotFound => ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
        CommandOutcome::TimedOut => ProbeOutcome::Unavailable(ProbeReason::TimedOut),
        CommandOutcome::SpawnFailed => ProbeOutcome::Unavailable(ProbeReason::Failed),
        CommandOutcome::Completed(output) => interpret_pgrep_output(&output),
    }
}

fn docker_server_liveness(timeout: Duration) -> ProbeOutcome<bool> {
    let mut command = Command::new("docker");
    command.args(DOCKER_SERVER_QUERY);
    interpret_docker_outcome(run_with_timeout(command, timeout))
}

/// What running `docker version` said about the daemon.
///
/// Split from the spawn so every branch below is reachable from a test without
/// a Docker installation in any particular state, which no CI runner can be
/// relied on to have.
fn interpret_docker_outcome(outcome: CommandOutcome) -> ProbeOutcome<bool> {
    match outcome {
        // No `docker` binary. An absent tool, and specifically *not* a daemon
        // reported as not running: there is no daemon either way, but "Docker
        // is not installed here" and "Docker is installed and stopped" are
        // different facts about the resources it would own.
        CommandOutcome::NotFound => ProbeOutcome::Unavailable(ProbeReason::ToolAbsent),
        // The client did not answer in time. Not an answer in either
        // direction — a busy daemon is still a daemon.
        CommandOutcome::TimedOut => ProbeOutcome::Unavailable(ProbeReason::TimedOut),
        CommandOutcome::SpawnFailed => ProbeOutcome::Unavailable(ProbeReason::Failed),
        CommandOutcome::Completed(output) => interpret_docker_version_output(&output),
    }
}

fn interpret_docker_version_output(output: &Output) -> ProbeOutcome<bool> {
    if output.status.success() {
        if String::from_utf8_lossy(&output.stdout).trim().is_empty() {
            // Exit 0 having printed no server version is not a statement
            // about a server. Whatever produced it, it is not the observation
            // asked for.
            return ProbeOutcome::Unavailable(ProbeReason::Failed);
        }
        return ProbeOutcome::Observed(true);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if crate::evidence::daemon_unreachable(&stderr) {
        // The client is installed, tried, and reported that nothing was
        // listening. A real negative observation, unlike `pgrep` finding no
        // process by one vendor's name.
        return ProbeOutcome::Observed(false);
    }

    // Some other failure. Not "running", not "stopped" — unknown, so that the
    // resources Docker owns stay visible as unexplained rather than reported
    // away in either direction.
    ProbeOutcome::Unavailable(ProbeReason::Failed)
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

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: exit_status(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// What the client on this workstation prints when its socket is not
    /// answering, captured from a real run against a nonexistent socket path.
    /// The wording is transport-dependent: a unix socket gives this, a
    /// `tcp://` host gives the older `Cannot connect to the Docker daemon`
    /// sentence, and macOS Docker is a unix socket.
    const UNIX_SOCKET_DOWN: &str = "failed to connect to the docker API at \
         unix:///Users/x/.colima/default/docker.sock; check if the path is \
         correct and if the daemon is running: dial unix \
         /Users/x/.colima/default/docker.sock: connect: no such file or directory\n";

    /// AC 1. Docker is asked, not matched. This is the assertion the whole
    /// ticket reduces to, and the one a regression would have to get past.
    #[test]
    fn docker_liveness_is_answered_by_its_client_not_by_a_host_process_name() {
        assert!(matches!(
            liveness_method(OwningTool::Docker),
            LivenessMethod::AskDockerServer
        ));
    }

    /// AC 6, the mutation control. This fixture is a machine where the daemon
    /// is live and no host process carries Docker Desktop's name — which is
    /// this workstation, running Colima.
    ///
    /// Both halves are asserted so the control cannot pass vacuously: the
    /// host-process path really would answer `Observed(false)` here, and the
    /// client really does say the server is there. Restoring
    /// `HostProcess("com.docker.backend")` for Docker makes the dispatch
    /// assertion fail, naming Docker as the tool that moved.
    #[test]
    fn a_live_daemon_with_no_matching_host_process_is_not_reported_as_stopped() {
        let nothing_matched = output(1, "", "");
        assert_eq!(
            interpret_pgrep_output(&nothing_matched),
            ProbeOutcome::Observed(false),
            "the host-process path is what this test exists to keep Docker off"
        );

        assert!(
            matches!(
                liveness_method(OwningTool::Docker),
                LivenessMethod::AskDockerServer
            ),
            "Docker routed to a host process name reports this live daemon as stopped"
        );
        assert_eq!(
            interpret_docker_outcome(CommandOutcome::Completed(output(0, "29.2.1\n", ""))),
            ProbeOutcome::Observed(true)
        );
    }

    /// AC 2. No `docker` binary is an absent tool, and never a daemon
    /// reported as not running.
    #[test]
    fn an_absent_docker_binary_is_tool_absent_not_a_stopped_daemon() {
        let outcome = interpret_docker_outcome(CommandOutcome::NotFound);

        assert_eq!(outcome, ProbeOutcome::Unavailable(ProbeReason::ToolAbsent));
        assert_ne!(outcome, ProbeOutcome::Observed(false));
    }

    /// AC 3. A client that tried and found nothing listening made a real
    /// observation, so this is the one branch that may be negative.
    #[test]
    fn a_present_client_with_an_unreachable_daemon_is_observed_not_running() {
        for stderr in [
            UNIX_SOCKET_DOWN,
            "Cannot connect to the Docker daemon at tcp://127.0.0.1:2376. \
             Is the docker daemon running?\n",
        ] {
            assert_eq!(
                interpret_docker_outcome(CommandOutcome::Completed(output(1, "", stderr))),
                ProbeOutcome::Observed(false),
                "should be an observed negative: {stderr}"
            );
        }
    }

    /// AC 4. A timeout is neither state. A daemon slow enough to miss the
    /// deadline is still a daemon, and reporting it stopped would remove the
    /// active-use signal from every resource it owns.
    #[test]
    fn a_timed_out_probe_is_neither_running_nor_stopped() {
        let outcome = interpret_docker_outcome(CommandOutcome::TimedOut);

        assert_eq!(outcome, ProbeOutcome::Unavailable(ProbeReason::TimedOut));
        assert_ne!(outcome, ProbeOutcome::Observed(false));
        assert_ne!(outcome, ProbeOutcome::Observed(true));
    }

    /// The honest-unknown branch, and the reason `daemon_unreachable`
    /// returning `false` must not be read as "then it is up". A permission
    /// error is not a stopped daemon and not a running one.
    #[test]
    fn an_unattributable_docker_failure_is_unavailable_in_both_directions() {
        let outcome = interpret_docker_outcome(CommandOutcome::Completed(output(
            1,
            "",
            "permission denied while trying to connect to the socket\n",
        )));

        assert_eq!(outcome, ProbeOutcome::Unavailable(ProbeReason::Failed));
        assert_ne!(outcome, ProbeOutcome::Observed(false));
        assert_ne!(outcome, ProbeOutcome::Observed(true));
    }

    /// Exit 0 with nothing on stdout is not an answer about a server, however
    /// successful the process was. Reading the exit code alone would make a
    /// client that printed nothing into evidence that a daemon is live.
    #[test]
    fn a_silent_success_is_not_an_observation_that_the_daemon_is_up() {
        let outcome = interpret_docker_outcome(CommandOutcome::Completed(output(0, "  \n", "")));

        assert_eq!(outcome, ProbeOutcome::Unavailable(ProbeReason::Failed));
        assert_ne!(outcome, ProbeOutcome::Observed(true));
    }

    /// The query has to be about the *server*. Asking for the client version
    /// would succeed with a stopped daemon and turn this probe back into a
    /// statement about an installation.
    #[test]
    fn the_docker_query_asks_for_the_server_version() {
        assert!(
            DOCKER_SERVER_QUERY.contains(&"{{.Server.Version}}"),
            "got: {DOCKER_SERVER_QUERY:?}"
        );
        assert!(!DOCKER_SERVER_QUERY.iter().any(|a| a.contains("Client")));
    }

    /// AC 5. Every tool that has no daemon still answers without shelling
    /// out, including the ecosystems HORO-1543 added.
    #[test]
    fn non_daemon_tools_are_unavailable_tool_not_running_without_shelling_out() {
        for tool in [
            OwningTool::Cargo,
            OwningTool::Npm,
            OwningTool::Pnpm,
            OwningTool::Yarn,
            OwningTool::Homebrew,
            OwningTool::Pip,
            OwningTool::Uv,
            OwningTool::Go,
            OwningTool::Gradle,
            OwningTool::Maven,
            OwningTool::SwiftPm,
            OwningTool::None,
        ] {
            assert_eq!(
                SystemToolLivenessProbe.is_running(tool, Duration::from_secs(5)),
                ProbeOutcome::Unavailable(ProbeReason::ToolNotRunning),
                "{tool:?} has no daemon"
            );
        }
    }

    /// AC 5's other half: the tool that genuinely does run as a named host
    /// process keeps doing so. Xcode is a macOS application, not a service
    /// behind a socket, so a process-name match is the direct observation
    /// there — the point of this ticket is the mismatch for Docker, not that
    /// `pgrep` is wrong.
    #[test]
    fn a_tool_that_really_is_a_host_process_still_matches_on_its_name() {
        match liveness_method(OwningTool::Xcode) {
            LivenessMethod::HostProcess(name) => assert_eq!(name, "Xcode"),
            _ => panic!("Xcode is a host process"),
        }
    }

    #[test]
    fn success_exit_is_observed_true() {
        assert_eq!(
            interpret_pgrep_output(&output(0, "1234\n", "")),
            ProbeOutcome::Observed(true)
        );
    }

    #[test]
    fn no_match_exit_with_no_output_is_observed_false() {
        assert_eq!(
            interpret_pgrep_output(&output(1, "", "")),
            ProbeOutcome::Observed(false)
        );
    }

    #[test]
    fn usage_error_with_stderr_is_unavailable_failed() {
        assert_eq!(
            interpret_pgrep_output(&output(2, "", "pgrep: illegal option\n")),
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }
}
