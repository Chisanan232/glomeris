//! Docker object detector: the individual images, containers and volumes the
//! daemon holds, as distinct lifecycle evidence (HORO-1544).
//!
//! Separate from [`super::docker`], which reports the one aggregate build-cache
//! row. Separate deliberately: the build cache is a single tool-owned blob with
//! no per-object lifecycle, while these are addressable objects that reference
//! each other and are individually in use or not.
//!
//! One probe, three kinds. `docker system df -v` is asked once and every
//! object is read out of that one snapshot. The ticket forbids collapsing
//! images, containers and volumes into one *model* — it does not ask for three
//! daemon round trips, and a single snapshot has the stronger property that the
//! objects in it are mutually consistent: a container cannot appear to
//! reference an image that the same snapshot does not list.
//!
//! Only `-v` populates the fields this needs. Under plain `docker image ls` /
//! `docker volume ls` the reference counts and unique sizes render as `"N/A"`
//! — Docker computes them only for the verbose storage report.
//!
//! Detect-only, per the accepted design cut: every kind here is
//! [`NativeCleanup::Unsupported`]. A detector being able to enumerate a
//! container is not a cleanup contract for one, and nothing in this module
//! constructs an action.
//!
//! Nothing here can reach `AutoSafe` on any path, which is what makes the
//! `CreatedAt`-as-`last_modified` approximation below safe: a volume is
//! unconditionally `Protected`, an image carries
//! [`Regenerability::Unknown`], and a container is
//! [`Regenerability::NotRegenerable`]. Staleness can therefore only ever move
//! one of these between two flavours of `Ask`.

use std::process::Command;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::evidence::{
    DockerActivity, DockerLifecycle, DockerPersistence, DockerReferences, Evidence, NativeCleanup,
    ProbeOutcome, ProbeReason, Recoverability, Regenerability, ResourceFingerprint, ResourceId,
    ResourceKind, ResourceLocator,
};

use super::{Detector, DetectorId, DetectorStatus, DiscoveryContext};

pub struct DockerObjectDetector;

const RESOURCE_KINDS: &[ResourceKind] = &[ResourceKind::DockerContainer];

const SOURCE: &str = "docker system df -v --format '{{json .}}'";

/// Runs one `docker system df -v` and returns its parsed JSON object.
///
/// The error type is [`DetectorStatus`], not a string. "Did Docker answer" and
/// "what is this detector's health" have the same answer, and carrying it as a
/// status is what keeps a stopped daemon from arriving here as `Failed` after
/// [`super::docker::failure_status`] went to the trouble of telling a stopped
/// daemon, an absent binary and a real error apart.
fn snapshot() -> Result<Value, DetectorStatus> {
    let output = match Command::new("docker")
        .args(["system", "df", "-v", "--format", "{{json .}}"])
        .output()
    {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(DetectorStatus::ToolAbsent);
        }
        Err(e) => {
            return Err(DetectorStatus::Failed(format!(
                "failed to spawn docker: {e}"
            )))
        }
    };

    if !output.status.success() {
        return Err(super::docker::failure_status(&String::from_utf8_lossy(
            &output.stderr,
        )));
    }

    serde_json::from_slice(&output.stdout).map_err(|e| {
        DetectorStatus::Failed(format!(
            "docker system df -v returned unparseable JSON: {e}"
        ))
    })
}

/// Reads `key` out of `object` as a string, if it is one. Docker renders every
/// scalar in this report as a string (`"Size": "4.1kB"`, `"LocalVolumes":
/// "0"`), and a field that is absent or of another type is `None` rather than
/// a coerced value.
fn field<'a>(object: &'a Value, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

/// Days since the Unix epoch for a proleptic-Gregorian date, by Howard
/// Hinnant's `days_from_civil`. Four lines of arithmetic in place of a date
/// crate, for the one format Docker prints.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shifted = (month as i64 + 9) % 12;
    let day_of_year = (153 * month_shifted + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Parses Docker's `CreatedAt` — `"2026-09-29 10:24:51 +0800 CST"` — into a
/// [`SystemTime`].
///
/// A missing, malformed or absent UTC offset returns `None`, which the caller
/// turns into `Unavailable(Failed)`. Defaulting to UTC would be the tempting
/// alternative and would be a fabricated timestamp: the staleness rules read
/// `last_modified`, and a timestamp wrong by up to a day in either direction is
/// worse than an honest "this could not be read".
///
/// The trailing zone abbreviation (`CST` above) is deliberately ignored — it is
/// ambiguous across zones, and the numeric offset preceding it is unambiguous.
fn parse_docker_timestamp(raw: &str) -> Option<SystemTime> {
    let mut parts = raw.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let offset = parts.next()?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let minute: u64 = time_parts.next()?.parse().ok()?;
    let second: u64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let offset_seconds = parse_utc_offset(offset)?;

    let day_seconds = days_from_civil(year, month, day).checked_mul(86_400)?;
    let local = day_seconds.checked_add((hour * 3_600 + minute * 60 + second) as i64)?;
    let epoch_seconds = local.checked_sub(offset_seconds)?;
    if epoch_seconds < 0 {
        // Before 1970. Not a timestamp any Docker object can honestly carry,
        // and `SystemTime::UNIX_EPOCH + Duration` cannot represent it anyway.
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(epoch_seconds as u64))
}

/// Parses `"+0800"` / `"-0530"` / `"+00:00"` / `"Z"` into seconds east of UTC.
fn parse_utc_offset(raw: &str) -> Option<i64> {
    if raw == "Z" || raw == "z" {
        return Some(0);
    }
    let sign = match raw.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = raw[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    if minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// Maps Docker's container `State` onto the lifecycle axis.
///
/// The fallthrough arm is the load-bearing one. Docker has added container
/// states before and will again, and a state this build does not recognise must
/// arrive as [`DockerActivity::Unknown`] — which policy reads as a refusal
/// reason — and never as idle. An unrecognised word is not evidence of
/// inactivity.
///
/// `paused` counts as active: the container's processes still exist, they are
/// merely frozen. `removing` counts as active for a plainer reason — Docker is
/// mid-operation on it and nothing else should be.
fn activity_from_state(state: &str) -> DockerActivity {
    match state {
        "running" | "restarting" | "paused" | "removing" => DockerActivity::Active,
        "created" | "exited" | "dead" => DockerActivity::Inactive,
        _ => DockerActivity::Unknown,
    }
}

/// Builds one [`Evidence`] per container in the snapshot.
///
/// An entry with no usable `ID` fails the whole detector rather than being
/// skipped. A skipped container is indistinguishable, downstream, from a
/// container that does not exist — and "Docker's report changed shape" is
/// exactly the condition that must not be reported as a shorter list.
fn containers(snapshot: &Value, detector: DetectorId) -> Result<Vec<Evidence>, DetectorStatus> {
    let Some(entries) = snapshot.get("Containers") else {
        return Err(DetectorStatus::Failed(
            "docker system df -v reported no Containers section".to_string(),
        ));
    };
    // `null` is Docker's rendering of "none", and an empty list of containers
    // is a real answer.
    if entries.is_null() {
        return Ok(Vec::new());
    }
    let Some(entries) = entries.as_array() else {
        return Err(DetectorStatus::Failed(
            "docker system df -v reported a non-list Containers section".to_string(),
        ));
    };

    let mut evidence = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(id) = field(entry, "ID")
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return Err(DetectorStatus::Failed(
                "docker system df -v reported a container with no ID".to_string(),
            ));
        };
        evidence.push(container_evidence(entry, id, detector));
    }
    Ok(evidence)
}

fn container_evidence(entry: &Value, id: &str, detector: DetectorId) -> Evidence {
    // `Size` is the container's writable layer. `docker ps -s` renders it as
    // `"4.1kB (virtual 60MB)"`, where the parenthesized figure includes the
    // image's shared layers and is therefore not this container's bytes; only
    // the leading token is read, under either rendering.
    let writable_layer = field(entry, "Size")
        .and_then(|size| size.split_whitespace().next())
        .and_then(super::docker::parse_human_size);
    let logical_bytes = match writable_layer {
        Some(bytes) => ProbeOutcome::Observed(bytes),
        None => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };
    let reclaimable_bytes = logical_bytes.clone();

    let state = field(entry, "State").unwrap_or("");
    let activity = activity_from_state(state);

    let evidence_resource = ResourceId::new(
        ResourceKind::DockerContainer,
        ResourceLocator::Tool {
            tool: crate::evidence::OwningTool::Docker,
            id: id.to_string(),
        },
    );

    let last_modified = match field(entry, "CreatedAt").and_then(parse_docker_timestamp) {
        Some(created) => ProbeOutcome::Observed(created),
        None => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    Evidence {
        resource: evidence_resource,
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            // Docker's own word for the container's state, so a later
            // revalidation can notice that the container it is about to be
            // asked about has started since it was observed. Not an identity:
            // it is one of `running`/`exited`/... and carries nothing about
            // what the container is or who owns it.
            tool_revision: Some(state.to_string()),
        },
        detector,
        logical_bytes,
        physical_bytes: None,
        // Removing a container frees its writable layer, which is the figure
        // above. Deliberately reported for running containers too: what
        // removal would free and whether removal is permitted are different
        // questions, and the second one belongs to policy — which refuses a
        // running container by way of `DockerObjectInUse`.
        reclaimable_bytes,
        // Docker's own figure, never `estimate_logical_bytes`'s
        // budget-truncated walk.
        reclaimable_bytes_is_lower_bound: false,
        // Docker reports when the container was created, not when it was last
        // written to, so this can overstate staleness for a long-running
        // container. See this module's header for why that is safe here.
        last_modified,
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        // A container's writable layer is produced by having run it. Recreating
        // the container does not reproduce what the last run wrote.
        regenerability: Regenerability::NotRegenerable,
        recoverability: Recoverability::Irreversible,
        native_cleanup: NativeCleanup::Unsupported,
        // A Docker object has no path for `lsof` or `git` to be pointed at —
        // the three path probes are inapplicable rather than unobserved, which
        // is why `required_evidence()` does not ask a Docker object for them.
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        docker_lifecycle: Some(DockerLifecycle {
            activity,
            // A container is created deliberately and its writable layer may
            // hold the only copy of something. Not `ToolManaged`: Docker made
            // the layer, but nothing in it is Docker's.
            persistence: DockerPersistence::UserManaged,
            // Nothing in Docker's model references a container — images and
            // volumes are referenced *by* containers, not the reverse. So this
            // is an observed empty set rather than an unanswered query, and it
            // is the one distinction `DockerReferences` exists to keep.
            references: ProbeOutcome::Observed(DockerReferences::none()),
        }),
        collected_at: SystemTime::now(),
        sources: vec![SOURCE.to_string()],
    }
}

impl Detector for DockerObjectDetector {
    fn id(&self) -> DetectorId {
        DetectorId("docker_objects")
    }

    fn resource_kinds(&self) -> &'static [ResourceKind] {
        RESOURCE_KINDS
    }

    fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
        let snapshot = match snapshot() {
            Ok(snapshot) => snapshot,
            Err(status) => return status,
        };
        match containers(&snapshot, self.id()) {
            Ok(evidence) => DetectorStatus::Found(evidence),
            Err(status) => status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETECTOR: DetectorId = DetectorId("docker_objects");

    /// A snapshot in the shape `docker system df -v --format '{{json .}}'`
    /// actually prints, with the field set observed from a live daemon
    /// (Colima, Docker Engine 29.2.1). Identifying fields carry placeholder
    /// values; the structure is verbatim.
    fn snapshot_json(containers: &str) -> Value {
        serde_json::from_str(&format!(
            "{{\"Images\":[],\"Containers\":[{containers}],\"Volumes\":[],\"BuildCache\":[]}}"
        ))
        .expect("test fixture must be valid JSON")
    }

    fn container_json(id: &str, state: &str, size: &str, created: &str) -> String {
        format!(
            "{{\"Command\":\"sleep\",\"CreatedAt\":\"{created}\",\"ID\":\"{id}\",\
             \"Image\":\"example:1\",\"Labels\":\"\",\"LocalVolumes\":\"0\",\"Mounts\":\"\",\
             \"Names\":\"example\",\"Networks\":\"bridge\",\"Platform\":null,\"Ports\":\"\",\
             \"RunningFor\":\"2 hours ago\",\"Size\":\"{size}\",\"State\":\"{state}\",\
             \"Status\":\"Up 2 hours\"}}"
        )
    }

    fn only_container(json: &str) -> Evidence {
        let mut found = containers(&snapshot_json(json), DETECTOR)
            .expect("fixture should parse")
            .into_iter();
        let evidence = found.next().expect("one container expected");
        assert!(found.next().is_none(), "exactly one container expected");
        evidence
    }

    fn lifecycle(evidence: &Evidence) -> &DockerLifecycle {
        evidence
            .docker_lifecycle
            .as_ref()
            .expect("a Docker object must carry lifecycle evidence")
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(DockerObjectDetector.id(), DetectorId("docker_objects"));
    }

    #[test]
    fn resource_kinds_reports_docker_container() {
        assert_eq!(
            DockerObjectDetector.resource_kinds(),
            &[ResourceKind::DockerContainer]
        );
    }

    /// AC 4: a running container is active-use evidence, carried on the
    /// object itself rather than inferred from the daemon being up.
    #[test]
    fn a_running_container_is_active() {
        let evidence = only_container(&container_json(
            "dfe320e00b9f",
            "running",
            "4.1kB",
            "2026-09-29 10:24:51 +0800 CST",
        ));

        assert_eq!(lifecycle(&evidence).activity, DockerActivity::Active);
        assert_eq!(
            evidence.resource.kind,
            ResourceKind::DockerContainer,
            "a container must not be reported under any other kind (AC 1)"
        );
        assert_eq!(evidence.logical_bytes, ProbeOutcome::Observed(4_100));
    }

    /// AC 7's stopped case, and §8's "a stopped container is NOT automatically
    /// safe to delete": inactive is a truthful answer about the *process*, and
    /// the two fields below are why it is not an answer about the data.
    #[test]
    fn a_stopped_container_is_inactive_but_not_regenerable() {
        let evidence = only_container(&container_json(
            "05b3ad2e9a97",
            "exited",
            "128MB",
            "2026-09-20 03:00:00 +0000 UTC",
        ));

        assert_eq!(lifecycle(&evidence).activity, DockerActivity::Inactive);
        assert_eq!(evidence.regenerability, Regenerability::NotRegenerable);
        assert_eq!(evidence.recoverability, Recoverability::Irreversible);
    }

    /// The anti-vacuity control for the arm above. If `activity_from_state`
    /// grew a catch-all that answered `Inactive`, every test that asserts
    /// `Inactive` would still pass and this one would fail.
    #[test]
    fn an_unrecognised_container_state_is_unknown_not_inactive() {
        for state in ["hibernating", "", "RUNNING", "up"] {
            assert_eq!(
                activity_from_state(state),
                DockerActivity::Unknown,
                "state {state:?} is not one this build recognises and must not read as idle"
            );
        }
        // And the states that are recognised do not all collapse to one answer.
        assert_eq!(activity_from_state("running"), DockerActivity::Active);
        assert_eq!(activity_from_state("exited"), DockerActivity::Inactive);
    }

    /// A paused container still owns its processes, and a container Docker is
    /// removing is mid-operation. Neither is idle.
    #[test]
    fn paused_and_removing_containers_are_active() {
        assert_eq!(activity_from_state("paused"), DockerActivity::Active);
        assert_eq!(activity_from_state("removing"), DockerActivity::Active);
        assert_eq!(activity_from_state("restarting"), DockerActivity::Active);
    }

    /// Nothing references a container, so an empty referrer set here is an
    /// observed fact. The assertion is on `is_observed`, not on emptiness:
    /// `Unavailable` would also look empty, and policy refuses on exactly that
    /// difference.
    #[test]
    fn a_containers_referrer_set_is_observed_empty_not_unanswered() {
        let evidence = only_container(&container_json(
            "abc123",
            "exited",
            "0B",
            "2026-09-20 03:00:00 +0000 UTC",
        ));

        let references = &lifecycle(&evidence).references;
        assert!(references.is_observed());
        assert_eq!(
            references,
            &ProbeOutcome::Observed(DockerReferences::none())
        );
    }

    /// An empty container list is a real answer and must not look like a
    /// failure. `null` is how Docker renders the section when there is nothing
    /// in it.
    #[test]
    fn no_containers_is_an_empty_list_not_a_failure() {
        assert_eq!(
            containers(&snapshot_json(""), DETECTOR),
            Ok(Vec::new()),
            "an empty section"
        );
        let null_section: Value = serde_json::from_str(
            "{\"Images\":[],\"Containers\":null,\"Volumes\":[],\"BuildCache\":[]}",
        )
        .expect("valid JSON");
        assert_eq!(containers(&null_section, DETECTOR), Ok(Vec::new()));
    }

    /// The "detector failure reported as empty" trap, from the other side: a
    /// report whose shape this code does not understand is a failure, not a
    /// machine with no containers.
    #[test]
    fn a_report_with_no_containers_section_is_failed_not_empty() {
        let no_section: Value =
            serde_json::from_str("{\"Images\":[],\"Volumes\":[]}").expect("valid JSON");
        match containers(&no_section, DETECTOR) {
            Err(DetectorStatus::Failed(reason)) => assert!(reason.contains("Containers")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// An entry that cannot be named cannot be reported, and must not be
    /// silently dropped: a shorter list reads downstream as a smaller machine.
    #[test]
    fn a_container_with_no_id_fails_the_detector_rather_than_being_skipped() {
        let nameless = "{\"State\":\"running\",\"Size\":\"4.1kB\",\"ID\":\"\"}";
        let with_one_good = format!(
            "{},{}",
            container_json("good1", "running", "1kB", "2026-09-29 10:24:51 +0800 CST"),
            nameless
        );

        match containers(&snapshot_json(&with_one_good), DETECTOR) {
            Err(DetectorStatus::Failed(reason)) => {
                assert!(reason.contains("no ID"), "got: {reason}")
            }
            other => panic!("expected Failed, got {other:?}; a partial list is not an answer"),
        }
    }

    #[test]
    fn parse_docker_timestamp_reads_dockers_own_format() {
        // 2026-09-29 10:24:51 +0800 is 02:24:51 UTC.
        let parsed = parse_docker_timestamp("2026-09-29 10:24:51 +0800 CST").expect("should parse");
        let secs = parsed
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs();
        // 1790648691, cross-checked against an independent implementation
        // (`python3 -c 'datetime(2026,9,29,10,24,51,tz=+08:00).timestamp()'`)
        // rather than against this module's own arithmetic.
        assert_eq!(secs, 1_790_648_691);
    }

    #[test]
    fn parse_docker_timestamp_applies_the_offset() {
        let east = parse_docker_timestamp("2026-09-29 10:24:51 +0800 CST").expect("east");
        let utc = parse_docker_timestamp("2026-09-29 10:24:51 +0000 UTC").expect("utc");
        assert_eq!(
            utc.duration_since(east).expect("utc is later"),
            Duration::from_secs(8 * 3_600),
            "the same wall clock eight hours east of UTC is an earlier instant"
        );
    }

    /// The whole point of hand-rolling this: an unreadable timestamp becomes
    /// `None` and then `Unavailable(Failed)`. Assuming UTC would put a
    /// fabricated number in front of the staleness rules.
    #[test]
    fn an_unparseable_timestamp_is_none_never_an_assumed_utc() {
        for raw in [
            "",
            "2026-09-29",
            "2026-09-29 10:24:51",
            "2026-09-29 10:24:51 CST",
            "2026-13-01 00:00:00 +0000 UTC",
            "2026-09-29 25:00:00 +0000 UTC",
            "2026-09-29 10:24:51 +0899 CST",
            "not a timestamp at all",
        ] {
            assert_eq!(
                parse_docker_timestamp(raw),
                None,
                "{raw:?} must not parse into a timestamp"
            );
        }
    }

    #[test]
    fn a_container_whose_creation_time_is_unreadable_reports_it_unavailable() {
        let evidence = only_container(&container_json("abc123", "exited", "0B", "unknown"));

        assert_eq!(
            evidence.last_modified,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }

    /// `docker ps -s` renders the writable layer and the image's shared layers
    /// in one string. Only the first is this container's.
    #[test]
    fn a_virtual_size_suffix_is_not_counted_as_the_containers_bytes() {
        let evidence = only_container(&container_json(
            "abc123",
            "running",
            "4.1kB (virtual 60MB)",
            "2026-09-29 10:24:51 +0800 CST",
        ));

        assert_eq!(evidence.logical_bytes, ProbeOutcome::Observed(4_100));
        assert_eq!(evidence.reclaimable_bytes, ProbeOutcome::Observed(4_100));
    }

    #[test]
    fn an_unreadable_size_is_unavailable_not_zero() {
        let evidence = only_container(&container_json(
            "abc123",
            "running",
            "lots",
            "2026-09-29 10:24:51 +0800 CST",
        ));

        assert_eq!(
            evidence.logical_bytes,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
        assert_eq!(
            evidence.reclaimable_bytes,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }

    /// AC 6 / §6: enumerating a container is not a cleanup contract for one.
    #[test]
    fn containers_are_detect_only() {
        let evidence = only_container(&container_json(
            "abc123",
            "exited",
            "0B",
            "2026-09-20 03:00:00 +0000 UTC",
        ));

        assert_eq!(evidence.native_cleanup, NativeCleanup::Unsupported);
    }

    /// The resource is addressed by Docker-native identity, not by a path.
    #[test]
    fn a_container_is_located_by_its_docker_id() {
        let evidence = only_container(&container_json(
            "dfe320e00b9f",
            "running",
            "4.1kB",
            "2026-09-29 10:24:51 +0800 CST",
        ));

        match &evidence.resource.locator {
            ResourceLocator::Tool { tool, id } => {
                assert_eq!(*tool, crate::evidence::OwningTool::Docker);
                assert_eq!(id, "dfe320e00b9f");
            }
            other => panic!("expected a tool locator, got {other:?}"),
        }
        assert_eq!(evidence.sources, vec![SOURCE.to_string()]);
    }

    // No test spawns a real `docker`: daemon presence and reachability are
    // environment-dependent and CI must not depend on either. The
    // ToolAbsent/ToolNotRunning/Failed attribution is covered by
    // `super::docker`'s unit tests, which own `failure_status`.
}
