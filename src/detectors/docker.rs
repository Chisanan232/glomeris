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
//! Known limitation (unchanged by HORO-992): populating `reclaimable_bytes`
//! honestly is still not enough to make [`ResourceKind::DockerBuildCache`]
//! reach [`crate::evidence::Completeness::Complete`]. Its resource uses
//! [`ResourceLocator::Tool`] (no single canonical filesystem path), so
//! [`crate::evidence::correlate::DefaultEvidenceCollector`] always reports
//! `open_by_process`/`process_cwd_match`/`git_state` as
//! `Unavailable(NotAttempted)` for it — and `required_evidence()` still
//! requires all three. `AutoSafe` for Docker build cache remains out of
//! reach until a future ticket gives this kind a real path-based or
//! tool-native correlation strategy.

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
fn parse_human_size(s: &str) -> Option<u64> {
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
            // Covers "daemon isn't running" (`docker system df` exits
            // non-zero with a connection-refused message) as well as any
            // other daemon-unreachable case.
            return DetectorStatus::ToolAbsent;
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
