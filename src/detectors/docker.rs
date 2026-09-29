//! Docker build-cache detector.
//!
//! Detect-only per the accepted design cut: this ticket emits informational
//! `Evidence` for the reported build-cache size, with no cleanup action
//! path implied ([`NativeCleanup::Unsupported`]). No image-level detail is
//! attempted — only `docker system df` is queried (argument-array
//! `Command`, never shell string interpolation).
//!
//! `reclaimable_bytes` (HORO-992) is parsed from the same `docker system
//! df` row's `Reclaimable` field, not from a raw directory walk: Docker's
//! build cache is not a simple directory tree that can be safely walked
//! (it is backed by its own internal storage driver state), and Docker
//! itself already computes an honest reclaimable-vs-total distinction
//! (e.g. some build cache entries are "active"/in-use and therefore not
//! reclaimable) — using Docker's own number is strictly more honest than
//! re-deriving one. `Reclaimable` is rendered either as a bare size
//! (`"2.36GB"`, when nothing in that row is active) or a size followed by
//! a parenthesized percentage (`"512MB (100%)"`); only the leading size
//! token is parsed, and any unparseable/missing value becomes
//! `Unavailable(Failed)`, never a fabricated number.
//!
//! Known limitation: [`ResourceKind::DockerBuildCache`] still does not reach
//! [`crate::evidence::Completeness::Complete`], though HORO-1544 changed the
//! reason. It used to be structural — `required_evidence()` asked a
//! [`ResourceLocator::Tool`] resource for the three path probes
//! (`open_by_process`/`process_cwd_match`/`git_state`), which
//! [`crate::evidence::correlate::DefaultEvidenceCollector`] cannot run
//! without a path and reports `Unavailable(NotAttempted)` by construction, so
//! no Docker resource could ever be complete no matter what any detector
//! observed. That is fixed: a Docker object is now asked only for facts about
//! a Docker object.
//!
//! What remains is an honest gap in this detector. `docker system df` reports
//! one aggregate row for the whole build cache and no modification time for
//! it, so `last_modified` stays `Unavailable(NotAttempted)` and the evidence
//! is [`crate::evidence::Completeness::Partial`] — `Ask`, for the true reason
//! that nobody knows how old this is.

use std::process::Command;
use std::time::SystemTime;

use crate::evidence::{
    NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, Regenerability, ResourceFingerprint,
    ResourceId, ResourceKind, ResourceLocator,
};

use super::{Detector, DetectorId, DetectorStatus, DiscoveryContext};

pub struct DockerDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::DockerBuildCache];

/// Find the `Type: "Build Cache"` row and extract the string value of
/// `field_name` (e.g. `"Size"`, `"Reclaimable"`) out of `docker system df
/// --format '{{json .}}'`'s newline-delimited JSON objects, without
/// pulling in a JSON crate for two fields. This is a best-effort scrape:
/// any parse failure just means the caller's `ProbeOutcome` stays
/// `Unavailable(Failed)`, never a crash.
fn find_build_cache_field<'a>(stdout: &'a str, field_name: &str) -> Option<&'a str> {
    for line in stdout.lines() {
        if !line.contains("\"Type\":\"Build Cache\"") && !line.contains("\"Type\": \"Build Cache\"")
        {
            continue;
        }
        let field_needle = format!("\"{field_name}\"");
        let idx = line.find(&field_needle)?;
        let rest = &line[idx..];
        let colon = rest.find(':')?;
        let after_colon = rest[colon + 1..].trim_start();
        let value = after_colon.strip_prefix('"')?;
        let end = value.find('"')?;
        return Some(&value[..end]);
    }
    None
}

/// Parse the `Type: "Build Cache"` row's `Size` figure. See
/// [`find_build_cache_field`].
fn parse_build_cache_bytes(stdout: &str) -> Option<u64> {
    parse_human_size(find_build_cache_field(stdout, "Size")?)
}

/// Parse the `Type: "Build Cache"` row's `Reclaimable` figure. Docker
/// renders this either as a bare size (`"2.36GB"`) or a size followed by a
/// parenthesized percentage (`"512MB (100%)"`) — only the leading size
/// token is parsed. See [`find_build_cache_field`].
fn parse_build_cache_reclaimable_bytes(stdout: &str) -> Option<u64> {
    let raw = find_build_cache_field(stdout, "Reclaimable")?;
    let size_token = raw.split_whitespace().next()?;
    parse_human_size(size_token)
}

/// Parse a docker-style human size string like `"1.2GB"`/`"512MB"`/`"0B"`
/// into bytes. Best-effort: unrecognized suffixes return `None`.
///
/// Shared with [`super::docker_objects`], which reads the same renderings out
/// of the same `docker system df` report — one parser, so a suffix Docker
/// starts printing cannot be understood by one detector and not the other.
pub(super) fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split_at = s.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let (number, suffix) = s.split_at(split_at);
    let value: f64 = number.parse().ok()?;
    let multiplier: f64 = match suffix {
        "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        _ => return None,
    };
    Some((value * multiplier) as u64)
}

/// Signatures a Docker client prints when it is installed and cannot reach a
/// daemon, lowercased.
///
/// Deliberately not one substring. The client, Docker Desktop, Colima and
/// Podman's docker shim each word this differently, and the socket path in
/// the message differs per runtime — matching on the wording that is common
/// to each family is what keeps this working on a machine whose Docker is
/// not the one this was written on (HORO-1562 covers the same question for
/// the liveness probe).
const NOT_RUNNING_SIGNATURES: &[&str] = &[
    "cannot connect to the docker daemon",
    "is the docker daemon running",
    "the docker daemon is not running",
    "error during connect",
];

/// What a non-zero `docker` exit means, as far as its own stderr says.
///
/// Before HORO-1544 every non-zero exit returned `ToolAbsent`, which said
/// "Docker is not installed on this machine" about a machine holding 11 GB of
/// images. Two facts were folded into one word, and the reported one was the
/// wrong one either way: a stopped daemon is not an absent tool, and a real
/// error is not normal state.
///
/// An exit this function cannot attribute becomes [`DetectorStatus::Failed`],
/// never `ToolNotRunning` and never `ToolAbsent`. "We don't know" is the
/// answer that keeps the resources visible as unknown rather than reporting
/// them away.
///
/// Shared with [`super::docker_objects`] for the same reason
/// [`parse_human_size`] is: both detectors invoke the same client, so a daemon
/// that is down must not be `ToolNotRunning` for one of them and `Failed` for
/// the other.
pub(super) fn failure_status(stderr: &str) -> DetectorStatus {
    let haystack = stderr.to_ascii_lowercase();
    if NOT_RUNNING_SIGNATURES
        .iter()
        .any(|sig| haystack.contains(sig))
    {
        return DetectorStatus::ToolNotRunning;
    }
    let detail = stderr.trim();
    if detail.is_empty() {
        return DetectorStatus::Failed("docker system df exited non-zero".to_string());
    }
    // First line only: `docker` can print a multi-line hint, and a detector
    // health field is not a log sink.
    let first_line = detail.lines().next().unwrap_or(detail);
    DetectorStatus::Failed(format!("docker system df failed: {first_line}"))
}

impl Detector for DockerDetector {
    fn id(&self) -> DetectorId {
        DetectorId("docker_build_cache")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
        let output = match Command::new("docker")
            .args(["system", "df", "--format", "{{json .}}"])
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return DetectorStatus::ToolAbsent;
            }
            Err(e) => return DetectorStatus::Failed(format!("failed to spawn docker: {e}")),
        };

        if !output.status.success() {
            return failure_status(&String::from_utf8_lossy(&output.stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let logical_bytes = match parse_build_cache_bytes(&stdout) {
            Some(bytes) => ProbeOutcome::Observed(bytes),
            None => ProbeOutcome::Unavailable(ProbeReason::Failed),
        };
        let reclaimable_bytes = match parse_build_cache_reclaimable_bytes(&stdout) {
            Some(bytes) => ProbeOutcome::Observed(bytes),
            None => ProbeOutcome::Unavailable(ProbeReason::Failed),
        };

        let resource = ResourceId::new(
            ResourceKind::DockerBuildCache,
            ResourceLocator::Tool {
                tool: crate::evidence::OwningTool::Docker,
                id: "build_cache".to_string(),
            },
        );

        let evidence = crate::evidence::Evidence {
            resource,
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: self.id(),
            logical_bytes,
            physical_bytes: None,
            reclaimable_bytes,
            // Docker's byte counts come from `docker system df` output
            // parsing, never `estimate_logical_bytes`'s budget-truncated
            // walk — never a lower bound.
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: Regenerability::RegenerableByTool,
            recoverability: Recoverability::RegenerableByTool,
            // Detect-only per the accepted design cut for this ticket — no
            // cleanup action path is implied here.
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            docker_lifecycle: None,
            collected_at: SystemTime::now(),
            sources: vec!["docker system df --format '{{json .}}'".to_string()],
        };

        DetectorStatus::Found(vec![evidence])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_kinds_reports_docker_build_cache() {
        assert_eq!(
            DockerDetector.resource_kinds(),
            &[ResourceKind::DockerBuildCache]
        );
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(DockerDetector.id(), DetectorId("docker_build_cache"));
    }

    /// AC2's central distinction. Each message is what a real client prints
    /// when its daemon is down — Docker Desktop, Colima (whose socket lives
    /// under the user's home, hence the elided path) and Podman's shim — and
    /// none of them means the tool is absent.
    #[test]
    fn a_daemon_that_is_not_answering_is_tool_not_running() {
        for stderr in [
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. \
             Is the docker daemon running?\n",
            "Cannot connect to the Docker daemon at unix:///Users/x/.colima/default/docker.sock. \
             Is the docker daemon running?\n",
            "error during connect: Get \"http://%2F%2F.%2Fpipe%2Fdocker_engine/v1.24/info\": \
             open //./pipe/docker_engine: The system cannot find the file specified.\n",
            "Error: the Docker daemon is not running\n",
        ] {
            assert_eq!(
                failure_status(stderr),
                DetectorStatus::ToolNotRunning,
                "should be tool_not_running: {stderr}"
            );
        }
    }

    /// The case that matters more than the one above: an exit this code cannot
    /// attribute must not be guessed at. `ToolNotRunning` would say the
    /// resources are presumably still there and `ToolAbsent` would say they
    /// are not — both are claims, and neither was established.
    #[test]
    fn an_unattributable_failure_is_failed_not_absent_or_not_running() {
        let status = failure_status("permission denied while trying to connect\n");

        assert_ne!(status, DetectorStatus::ToolAbsent);
        assert_ne!(status, DetectorStatus::ToolNotRunning);
        match status {
            DetectorStatus::Failed(reason) => {
                assert!(
                    reason.contains("permission denied"),
                    "the reason must carry docker's own words; got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// A failure with nothing on stderr is still a failure. The old code's
    /// `ToolAbsent` was reachable here too, which is how a silent non-zero
    /// exit came to mean "no Docker on this machine".
    #[test]
    fn a_silent_failure_is_still_failed() {
        match failure_status("   \n") {
            DetectorStatus::Failed(reason) => assert!(!reason.is_empty()),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// A detector health field is not a log sink: a client that prints a hint
    /// under its error contributes the error, not the hint.
    #[test]
    fn a_multi_line_failure_reports_only_its_first_line() {
        match failure_status("something broke\nRun 'docker system df --help' for more.\n") {
            DetectorStatus::Failed(reason) => {
                assert!(reason.ends_with("something broke"), "got: {reason}");
                assert!(!reason.contains("--help"), "got: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// An empty build cache is a real answer, and a different one from either
    /// failure above: the row is present, Docker reported `0B`, and the
    /// detector must report the observed zero rather than an unavailable
    /// probe. This is AC2's "empty storage" half.
    #[test]
    fn a_daemon_reporting_an_empty_build_cache_parses_as_observed_zero() {
        let stdout = concat!(
            "{\"Active\":\"0\",\"Reclaimable\":\"0B\",\"Size\":\"0B\",",
            "\"TotalCount\":\"0\",\"Type\":\"Build Cache\"}\n"
        );

        assert_eq!(parse_build_cache_bytes(stdout), Some(0));
        assert_eq!(parse_build_cache_reclaimable_bytes(stdout), Some(0));
    }

    #[test]
    fn parse_human_size_handles_common_suffixes() {
        assert_eq!(parse_human_size("0B"), Some(0));
        assert_eq!(parse_human_size("512MB"), Some(512_000_000));
        assert_eq!(parse_human_size("1.2GB"), Some(1_200_000_000));
    }

    #[test]
    fn parse_human_size_rejects_unknown_suffix() {
        assert_eq!(parse_human_size("12XB"), None);
    }

    #[test]
    fn parse_build_cache_bytes_extracts_size_field() {
        let stdout = concat!(
            "{\"Type\":\"Images\",\"Size\":\"3.4GB\"}\n",
            "{\"Type\":\"Build Cache\",\"Size\":\"512MB\"}\n"
        );
        assert_eq!(parse_build_cache_bytes(stdout), Some(512_000_000));
    }

    #[test]
    fn parse_build_cache_bytes_returns_none_without_a_build_cache_row() {
        let stdout = "{\"Type\":\"Images\",\"Size\":\"3.4GB\"}\n";
        assert_eq!(parse_build_cache_bytes(stdout), None);
    }

    #[test]
    fn parse_build_cache_reclaimable_bytes_extracts_bare_size() {
        let stdout = concat!(
            "{\"Active\":\"0\",\"Reclaimable\":\"2.361GB\",\"Size\":\"2.361GB\",",
            "\"TotalCount\":\"254\",\"Type\":\"Build Cache\"}\n"
        );
        assert_eq!(
            parse_build_cache_reclaimable_bytes(stdout),
            Some(2_361_000_000)
        );
    }

    #[test]
    fn parse_build_cache_reclaimable_bytes_strips_trailing_percentage() {
        let stdout = concat!(
            "{\"Active\":\"3\",\"Reclaimable\":\"512MB (100%)\",\"Size\":\"512MB\",",
            "\"TotalCount\":\"10\",\"Type\":\"Build Cache\"}\n"
        );
        assert_eq!(
            parse_build_cache_reclaimable_bytes(stdout),
            Some(512_000_000)
        );
    }

    #[test]
    fn parse_build_cache_reclaimable_bytes_handles_zero_with_percentage() {
        let stdout = concat!(
            "{\"Active\":\"6\",\"Reclaimable\":\"0B (0%)\",\"Size\":\"3.111MB\",",
            "\"TotalCount\":\"7\",\"Type\":\"Build Cache\"}\n"
        );
        assert_eq!(parse_build_cache_reclaimable_bytes(stdout), Some(0));
    }

    #[test]
    fn parse_build_cache_reclaimable_bytes_returns_none_without_a_build_cache_row() {
        let stdout = "{\"Type\":\"Images\",\"Reclaimable\":\"3.4GB\"}\n";
        assert_eq!(parse_build_cache_reclaimable_bytes(stdout), None);
    }

    // No integration test spawning a real `docker` process: daemon
    // presence/reachability is environment-dependent and CI must not
    // depend on it. ToolAbsent/Failed/Found branch behavior is covered by
    // the parsing unit tests above plus the discover_all smoke test in
    // `detectors::tests`.
}
