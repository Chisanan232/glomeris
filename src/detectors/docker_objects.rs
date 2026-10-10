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
//! `reclaimable_bytes` is the object's own bytes throughout — what removing
//! *that* object would free, and nothing else. It is not reduced to zero for an
//! object Docker would refuse to remove. Whether removal is permitted, and
//! whether it would require removing something else first, is a policy question
//! that policy answers from [`DockerActivity`] and the protected-kind rule;
//! encoding it here would make a size field disagree with `logical_bytes` for a
//! reason that has nothing to do with size, in the one place a reader would not
//! look for a verdict.
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

const RESOURCE_KINDS: &[ResourceKind] = &[
    ResourceKind::DockerContainer,
    ResourceKind::DockerImage,
    ResourceKind::DockerVolume,
];

const SOURCE: &str = "docker system df -v --format '{{json .}}'";

/// Runs one `docker system df -v` and returns its parsed JSON object.
///
/// The error type is [`DetectorStatus`], not a string. "Did Docker answer" and
/// "what is this detector's health" have the same answer, and carrying it as a
/// status is what keeps a stopped daemon from arriving here as `Failed` after
/// [`super::docker::failure_status`] went to the trouble of telling a stopped
/// daemon, an absent binary and a real error apart.
fn snapshot() -> Result<Value, DetectorStatus> {
    // Why Docker has to be asked (HORO-1560 AC 3): this detector reports
    // individual containers, images and volumes — their ids, sizes, and when
    // each was last used. None of that exists as a readable file on this host;
    // it is the daemon's own bookkeeping. There is no documented path route to
    // prefer, so there is nothing here for HORO-1560 AC 2 to apply to.
    //
    // What Docker does when asked: `docker system df -v` is the same reporting
    // command as in [`super::docker`] with per-object detail added. It reads
    // the daemon's state and prints it; it removes nothing.
    let output = match Command::new(super::docker::DOCKER_PROGRAM)
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

/// One of the snapshot's object lists, by name.
///
/// A section this code cannot read is a [`DetectorStatus::Failed`], never an
/// empty list. "Docker's report changed shape" must not be reported as "this
/// machine has none of those" — that is the detector-failure-as-empty trap, and
/// it is the one place where being wrong is invisible.
///
/// An absent-but-`null` section is the exception: `null` is how Docker renders
/// a section with nothing in it, and nothing in it is a real answer.
fn section<'a>(snapshot: &'a Value, name: &str) -> Result<&'a [Value], DetectorStatus> {
    let Some(entries) = snapshot.get(name) else {
        return Err(DetectorStatus::Failed(format!(
            "docker system df -v reported no {name} section"
        )));
    };
    if entries.is_null() {
        return Ok(&[]);
    }
    match entries.as_array() {
        Some(entries) => Ok(entries),
        None => Err(DetectorStatus::Failed(format!(
            "docker system df -v reported a non-list {name} section"
        ))),
    }
}

/// The `ID` of one object, trimmed, or a [`DetectorStatus::Failed`] naming
/// `what`.
///
/// An entry with no usable `ID` fails the whole detector rather than being
/// skipped. A skipped object is indistinguishable, downstream, from one that
/// does not exist, and a container missing from the list is also a container
/// missing from some image's referrer set.
fn object_id<'a>(entry: &'a Value, what: &str) -> Result<&'a str, DetectorStatus> {
    field(entry, "ID")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            DetectorStatus::Failed(format!("docker system df -v reported a {what} with no ID"))
        })
}

/// Builds one [`Evidence`] per container in the snapshot.
fn containers(snapshot: &Value, detector: DetectorId) -> Result<Vec<Evidence>, DetectorStatus> {
    let entries = section(snapshot, "Containers")?;
    let mut evidence = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = object_id(entry, "container")?;
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
        executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        collected_at: SystemTime::now(),
        sources: vec![SOURCE.to_string()],
    }
}

/// One container as the image half needs to see it: its Docker-native
/// identity, the image reference it was started from, and whether it is active.
struct ContainerRef {
    resource: ResourceId,
    image: String,
    active: bool,
}

fn container_refs(snapshot: &Value) -> Result<Vec<ContainerRef>, DetectorStatus> {
    let entries = section(snapshot, "Containers")?;
    let mut refs = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = object_id(entry, "container")?;
        refs.push(ContainerRef {
            resource: ResourceId::new(
                ResourceKind::DockerContainer,
                ResourceLocator::Tool {
                    tool: crate::evidence::OwningTool::Docker,
                    id: id.to_string(),
                },
            ),
            image: field(entry, "Image").unwrap_or("").to_string(),
            active: activity_from_state(field(entry, "State").unwrap_or(""))
                == DockerActivity::Active,
        });
    }
    Ok(refs)
}

/// Does `container_image` — the reference a container was started from — name
/// the image described by `repository`/`tag`/`id`?
///
/// Docker records whatever was typed at `docker run`: a `repo:tag`, a bare
/// repository relying on `:latest`, a full `sha256:…`, or a truncated digest.
/// All of those name the same image and none of them is string-equal to the
/// others, so this cannot be an equality test.
///
/// `<none>` is Docker's rendering of an absent repository or tag (a dangling
/// image), and is deliberately not matched: two unrelated dangling images would
/// otherwise both "match" every container started from a third.
fn names_image(container_image: &str, repository: &str, tag: &str, id: &str) -> bool {
    if container_image.is_empty() {
        return false;
    }
    if container_image == id {
        return true;
    }
    let digest = id.strip_prefix("sha256:").unwrap_or(id);
    if container_image == digest {
        return true;
    }
    // A truncated digest, as `docker ps` prints one. Length-bounded and
    // hex-only so that a repository name cannot be read as a short id.
    let looks_like_short_id = container_image.len() >= 12
        && container_image
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if looks_like_short_id && digest.starts_with(container_image) {
        return true;
    }
    if repository == "<none>" {
        return false;
    }
    if tag != "<none>" && container_image == format!("{repository}:{tag}") {
        return true;
    }
    // `docker run repo` with no tag records the reference as typed, while the
    // image it resolved to carries `latest`.
    tag == "latest" && container_image == repository
}

/// Builds one [`Evidence`] per image in the snapshot, with the containers that
/// reference it (AC 3).
fn images(snapshot: &Value, detector: DetectorId) -> Result<Vec<Evidence>, DetectorStatus> {
    let entries = section(snapshot, "Images")?;
    let refs = container_refs(snapshot)?;
    let mut evidence = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = object_id(entry, "image")?;
        evidence.push(image_evidence(entry, id, &refs, detector));
    }
    Ok(evidence)
}

fn image_evidence(
    entry: &Value,
    id: &str,
    refs: &[ContainerRef],
    detector: DetectorId,
) -> Evidence {
    let repository = field(entry, "Repository").unwrap_or("<none>");
    let tag = field(entry, "Tag").unwrap_or("<none>");

    // Docker's own count of containers referencing this image, and the
    // authority for whether it is referenced at all. `docker system df -v`
    // computes it; other renderings print `N/A` or `-1`, which do not parse and
    // therefore arrive as "unknown" rather than as zero.
    let declared_referrers = field(entry, "Containers").and_then(|n| n.parse::<usize>().ok());

    let matched: Vec<&ContainerRef> = refs
        .iter()
        .filter(|c| names_image(&c.image, repository, tag, id))
        .collect();

    // Two independent readings of the same fact, and they are only reported as
    // an observed relationship when they agree. If Docker says three containers
    // reference this image and only two could be identified, the honest answer
    // is that the reference query was not answered — an incomplete referrer
    // list would read downstream as "and no others".
    let references = match declared_referrers {
        Some(declared) if declared == matched.len() => ProbeOutcome::Observed(DockerReferences {
            referenced_by: matched.iter().map(|c| c.resource.clone()).collect(),
            active_referrers: matched.iter().filter(|c| c.active).count() as u32,
        }),
        _ => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    // `Active` is documented as "a running container, or an object a running
    // container depends on" — so a non-zero referrer count is not enough on its
    // own. An image only a stopped container references is referenced and not in
    // use, and those are different facts.
    let activity = if matched.iter().any(|c| c.active) {
        // Provable whether or not the counts agree: a container observed
        // running names this image, and `matched` is a subset of the real
        // referrer set, so an incomplete match cannot make this false.
        DockerActivity::Active
    } else {
        match (declared_referrers, references.is_observed()) {
            // Docker was asked and said nothing references this image.
            (Some(0), _) => DockerActivity::Inactive,
            // Every referrer Docker counted was identified, and none is
            // running.
            (Some(_), true) => DockerActivity::Inactive,
            // Referenced by containers this code could not identify — one of
            // them may well be running, so this is not an answer.
            (Some(_), false) => DockerActivity::Unknown,
            (None, _) => DockerActivity::Unknown,
        }
    };

    // `Size` includes layers shared with other images; `UniqueSize` is what
    // belongs to this image alone. Both are reported, in the field each one
    // actually answers: `Size` is how big this image is, and `UniqueSize` is
    // what deleting it would free. Using `Size` for the second would double
    // count every shared layer across a report.
    let logical_bytes = match field(entry, "Size").and_then(super::docker::parse_human_size) {
        Some(bytes) => ProbeOutcome::Observed(bytes),
        None => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };
    // Reported for a referenced image too, by the same rule the container half
    // states: this is the object's own bytes, and whether removal is permitted
    // — Docker refuses `rmi` on a referenced image — is a separate question
    // that belongs to policy, which refuses an `Active` object by way of
    // `DockerObjectInUse`. Answering `0` here would encode that permission
    // judgement in an evidence field, and the size would then disagree with
    // `logical_bytes` for a reason that has nothing to do with size.
    let reclaimable_bytes =
        match field(entry, "UniqueSize").and_then(super::docker::parse_human_size) {
            Some(unique) => ProbeOutcome::Observed(unique),
            None => ProbeOutcome::Unavailable(ProbeReason::Failed),
        };

    let last_modified = match field(entry, "CreatedAt").and_then(parse_docker_timestamp) {
        Some(created) => ProbeOutcome::Observed(created),
        None => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    Evidence {
        resource: ResourceId::new(
            ResourceKind::DockerImage,
            ResourceLocator::Tool {
                tool: crate::evidence::OwningTool::Docker,
                id: id.to_string(),
            },
        ),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            // Nothing to add: an image's `ID` is a content hash, so the
            // locator already is the identity a revalidation would check.
            // `Digest` is deliberately not recorded — it names the exact
            // upstream artifact, which is repository identity.
            tool_revision: None,
        },
        detector,
        logical_bytes,
        physical_bytes: None,
        reclaimable_bytes,
        reclaimable_bytes_is_lower_bound: false,
        // Docker reports when the image was built, which for a registry image
        // is when its publisher built it and not when it arrived here. It can
        // therefore overstate staleness; see this module's header for why that
        // cannot promote anything.
        last_modified,
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        // An image pulled from a registry can be pulled again; an image built
        // here may exist nowhere else, and nothing observable says which this
        // is. `Regenerability` is the axis that carries not knowing — see its
        // own documentation — and `Unknown` here is what sends an image to
        // `Ask` with the accurate reason.
        regenerability: Regenerability::Unknown,
        // `Recoverability` has no `Unknown` variant by design, and
        // `Irreversible` would be a stronger claim than the evidence supports
        // for what is usually a registry copy. The doubt is carried above.
        recoverability: Recoverability::RegenerableByTool,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        docker_lifecycle: Some(DockerLifecycle {
            activity,
            // Not `ToolManaged`, which would say Docker can produce this
            // again, and not `UserManaged`, which would say it holds the
            // user's data. Neither was established.
            persistence: DockerPersistence::Unknown,
            references,
        }),
        executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        collected_at: SystemTime::now(),
        sources: vec![SOURCE.to_string()],
    }
}

/// Is `name` one Docker minted for itself rather than one a human chose?
///
/// Docker names an anonymous volume — the kind a `VOLUME` instruction produces
/// when nothing was mounted over it — with a 64-character hex string, and that
/// is the only thing distinguishing it from a named one in this report.
///
/// Length-and-alphabet exact on purpose. Two of the four volumes on the machine
/// this was written against are named `act-test-port-test-<64 hex>`: they
/// *contain* 64 hex characters, a human chose them, and a `contains`-style test
/// would have called both anonymous.
fn is_anonymous_volume_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Does a container's `Mounts` entry name this volume?
///
/// `Mounts` is a comma-separated list of what the container has mounted, mixing
/// volume names with bind-mount host paths. A bind path is not a volume name, so
/// exact equality against the volume's own name is both the match and the
/// filter — and it is why a container bind-mounting `/Users/x/data` cannot be
/// read as a referrer of some volume.
fn mounts_name_volume(mounts: &str, volume_name: &str) -> bool {
    mounts
        .split(',')
        .map(str::trim)
        .any(|mount| mount == volume_name)
}

/// One container as the volume half needs to see it: identity, what it has
/// mounted, and whether it is active.
struct VolumeContainerRef {
    resource: ResourceId,
    mounts: String,
    active: bool,
}

fn volume_container_refs(snapshot: &Value) -> Result<Vec<VolumeContainerRef>, DetectorStatus> {
    let entries = section(snapshot, "Containers")?;
    let mut refs = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = object_id(entry, "container")?;
        refs.push(VolumeContainerRef {
            resource: ResourceId::new(
                ResourceKind::DockerContainer,
                ResourceLocator::Tool {
                    tool: crate::evidence::OwningTool::Docker,
                    id: id.to_string(),
                },
            ),
            mounts: field(entry, "Mounts").unwrap_or("").to_string(),
            active: activity_from_state(field(entry, "State").unwrap_or(""))
                == DockerActivity::Active,
        });
    }
    Ok(refs)
}

/// Builds one [`Evidence`] per volume in the snapshot.
fn volumes(snapshot: &Value, detector: DetectorId) -> Result<Vec<Evidence>, DetectorStatus> {
    let entries = section(snapshot, "Volumes")?;
    let refs = volume_container_refs(snapshot)?;
    let mut evidence = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = field(entry, "Name")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                DetectorStatus::Failed(
                    "docker system df -v reported a volume with no Name".to_string(),
                )
            })?;
        evidence.push(volume_evidence(entry, name, &refs, detector));
    }
    Ok(evidence)
}

fn volume_evidence(
    entry: &Value,
    name: &str,
    refs: &[VolumeContainerRef],
    detector: DetectorId,
) -> Evidence {
    // Docker's own count of containers using this volume, and the authority for
    // whether it is attached at all. Only `docker system df -v` computes it;
    // `docker volume ls` prints `N/A`, which does not parse and therefore
    // arrives as unknown rather than as zero.
    let declared_links = field(entry, "Links").and_then(|n| n.parse::<usize>().ok());

    let matched: Vec<&VolumeContainerRef> = refs
        .iter()
        .filter(|c| mounts_name_volume(&c.mounts, name))
        .collect();

    // The same two-readings rule the image half uses, for the same reason: an
    // attachment list that is missing a container reads downstream as "and
    // nothing else has this mounted", which is the one thing it must not say
    // about a volume.
    let references = match declared_links {
        Some(links) if links == matched.len() => ProbeOutcome::Observed(DockerReferences {
            referenced_by: matched.iter().map(|c| c.resource.clone()).collect(),
            active_referrers: matched.iter().filter(|c| c.active).count() as u32,
        }),
        _ => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    let activity = if matched.iter().any(|c| c.active) {
        DockerActivity::Active
    } else {
        match (declared_links, references.is_observed()) {
            (Some(0), _) => DockerActivity::Inactive,
            (Some(_), true) => DockerActivity::Inactive,
            (Some(_), false) => DockerActivity::Unknown,
            (None, _) => DockerActivity::Unknown,
        }
    };

    let bytes = match field(entry, "Size").and_then(super::docker::parse_human_size) {
        Some(bytes) => ProbeOutcome::Observed(bytes),
        None => ProbeOutcome::Unavailable(ProbeReason::Failed),
    };

    Evidence {
        resource: ResourceId::new(
            ResourceKind::DockerVolume,
            ResourceLocator::Tool {
                tool: crate::evidence::OwningTool::Docker,
                // The name, not the mount point. Docker addresses a volume by
                // name, and on a VM-backed runtime the mount point is a path
                // inside the VM that means nothing on this host.
                id: name.to_string(),
            },
        ),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            // Nothing to add. A volume's name is its identity and is already
            // the locator; `Links` is a fact about *other* objects and belongs
            // in `references`, not smuggled into a fingerprint.
            tool_revision: None,
        },
        detector,
        logical_bytes: bytes.clone(),
        physical_bytes: None,
        reclaimable_bytes: bytes,
        reclaimable_bytes_is_lower_bound: false,
        // `docker system df -v` reports no timestamp for a volume, and the one
        // path it does report is the mount point — which on this workstation's
        // Colima runtime lives inside the VM and cannot be stat'd from here. Not
        // attempted, therefore, rather than attempted and failed.
        last_modified: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        // Whatever wrote into this volume is not observable from here, and
        // nothing regenerates it. This is the axis that makes a volume `Ask`
        // even before the protected-kind rule makes it `Protected`.
        regenerability: Regenerability::NotRegenerable,
        recoverability: Recoverability::Irreversible,
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        docker_lifecycle: Some(DockerLifecycle {
            activity,
            persistence: if is_anonymous_volume_name(name) {
                // Not `ToolManaged`. Docker minted the name, which is not the
                // same as Docker being able to produce the contents again: an
                // anonymous volume behind a database image's `VOLUME` line holds
                // that database, and no name says so either way. The campaign's
                // Maven argument, applied to the case where there is no name to
                // misread.
                DockerPersistence::Unknown
            } else {
                // A human or a compose file chose this name, so it is treated as
                // possibly holding the only copy of their data.
                DockerPersistence::UserManaged
            },
            references,
        }),
        executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
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
        let mut evidence = match containers(&snapshot, self.id()) {
            Ok(evidence) => evidence,
            Err(status) => return status,
        };
        match images(&snapshot, self.id()) {
            Ok(images) => evidence.extend(images),
            Err(status) => return status,
        }
        match volumes(&snapshot, self.id()) {
            Ok(volumes) => evidence.extend(volumes),
            Err(status) => return status,
        }
        DetectorStatus::Found(evidence)
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
        snapshot_with("", containers)
    }

    fn snapshot_with(images: &str, containers: &str) -> Value {
        snapshot_of(images, containers, "")
    }

    fn snapshot_of(images: &str, containers: &str, volumes: &str) -> Value {
        serde_json::from_str(&format!(
            "{{\"Images\":[{images}],\"Containers\":[{containers}],\
             \"Volumes\":[{volumes}],\"BuildCache\":[]}}"
        ))
        .expect("test fixture must be valid JSON")
    }

    /// The volume field set `docker system df -v` prints, verbatim in shape.
    /// `links` is Docker's own count of containers with the volume mounted.
    /// `Availability`/`Group`/`Status` really are `"N/A"` on the local driver.
    fn volume_json(name: &str, links: &str, size: &str) -> String {
        format!(
            "{{\"Availability\":\"N/A\",\"Driver\":\"local\",\"Group\":\"N/A\",\
             \"Labels\":\"\",\"Links\":\"{links}\",\
             \"Mountpoint\":\"/var/lib/docker/volumes/{name}/_data\",\
             \"Name\":\"{name}\",\"Scope\":\"local\",\"Size\":\"{size}\",\
             \"Status\":\"N/A\"}}"
        )
    }

    /// A container fixture with an explicit `Mounts` rendering — the
    /// comma-separated list `docker system df -v` prints, mixing volume names
    /// with bind-mount host paths.
    fn container_with_mounts(id: &str, state: &str, mounts: &str) -> String {
        container_json(id, state, "0B", "2026-09-20 03:00:00 +0000 UTC")
            .replace("\"Mounts\":\"\"", &format!("\"Mounts\":\"{mounts}\""))
    }

    fn only_volume(volumes: &str, containers: &str) -> Evidence {
        let mut found = volumes_of(&snapshot_of("", containers, volumes)).into_iter();
        let evidence = found.next().expect("one volume expected");
        assert!(found.next().is_none(), "exactly one volume expected");
        evidence
    }

    fn volumes_of(snapshot: &Value) -> Vec<Evidence> {
        volumes(snapshot, DETECTOR).expect("fixture should parse")
    }

    /// A name of the shape Docker mints for an anonymous volume.
    const ANONYMOUS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The image field set `docker system df -v` prints, verbatim in shape.
    /// `containers` is Docker's own count of containers referencing the image.
    fn image_json(
        id: &str,
        repository: &str,
        tag: &str,
        containers: &str,
        size: &str,
        unique: &str,
    ) -> String {
        format!(
            "{{\"Containers\":\"{containers}\",\"CreatedAt\":\"2026-09-19 11:43:19 +0800 CST\",\
             \"CreatedSince\":\"10 days ago\",\"Digest\":\"<none>\",\"ID\":\"{id}\",\
             \"Repository\":\"{repository}\",\"SharedSize\":\"0B\",\"Size\":\"{size}\",\
             \"Tag\":\"{tag}\",\"UniqueSize\":\"{unique}\"}}"
        )
    }

    fn only_image(images: &str, containers: &str) -> Evidence {
        let mut found = images_of(&snapshot_with(images, containers)).into_iter();
        let evidence = found.next().expect("one image expected");
        assert!(found.next().is_none(), "exactly one image expected");
        evidence
    }

    fn images_of(snapshot: &Value) -> Vec<Evidence> {
        images(snapshot, DETECTOR).expect("fixture should parse")
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

    /// Pinned rather than merely non-empty. `resource_kinds()` is what
    /// `AutopilotEnvelope::allow_kind` accepts an allowlist entry against, so a
    /// kind silently added here would silently widen what an existing
    /// allowlist authorizes, and a kind silently dropped would make an entry
    /// authorize nothing while still reporting as configured.
    #[test]
    fn resource_kinds_reports_each_kind_this_detector_declares() {
        assert_eq!(
            DockerObjectDetector.resource_kinds(),
            &[
                ResourceKind::DockerContainer,
                ResourceKind::DockerImage,
                ResourceKind::DockerVolume,
            ]
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

    /// AC 3 and AC 4 together: the image a running container needs is reported
    /// as referenced *by that container*, named, and as active use.
    #[test]
    fn an_image_a_running_container_needs_is_referenced_and_active() {
        let image = image_json(
            "sha256:abc0123456789def",
            "example",
            "1",
            "1",
            "2.07GB",
            "2.07GB",
        );
        // `container_json`'s `Image` is `example:1` — the `repo:tag` this
        // container was started from, which is how Docker records the
        // reference.
        let container = container_json(
            "dfe320e00b9f",
            "running",
            "4.1kB",
            "2026-09-29 10:24:51 +0800 CST",
        );

        let evidence = only_image(&image, &container);
        let lifecycle = lifecycle(&evidence);

        assert_eq!(lifecycle.activity, DockerActivity::Active);
        match &lifecycle.references {
            ProbeOutcome::Observed(references) => {
                assert_eq!(references.active_referrers, 1);
                assert_eq!(
                    references
                        .referenced_by
                        .iter()
                        .map(|r| r.to_string())
                        .collect::<Vec<_>>(),
                    vec!["docker_container:docker:dfe320e00b9f"],
                    "the referrer must be named, not merely counted"
                );
            }
            other => panic!("expected an observed referrer set, got {other:?}"),
        }
    }

    /// §8: "an unused image is NOT automatically authority to delete it". An
    /// unreferenced image is `Inactive` — and still `Unknown` on the axis that
    /// decides whether it can be replaced.
    #[test]
    fn an_unreferenced_image_is_inactive_but_still_unknown_to_regenerate() {
        let evidence = only_image(
            &image_json(
                "sha256:abc0123456789def",
                "example",
                "1",
                "0",
                "75.9MB",
                "75.89MB",
            ),
            "",
        );

        assert_eq!(lifecycle(&evidence).activity, DockerActivity::Inactive);
        assert_eq!(evidence.regenerability, Regenerability::Unknown);
        assert_eq!(
            lifecycle(&evidence).persistence,
            DockerPersistence::Unknown,
            "neither 'Docker can make this again' nor 'this is the user's data' \
             was established"
        );
        assert_eq!(
            lifecycle(&evidence).references,
            ProbeOutcome::Observed(DockerReferences::none())
        );
    }

    /// A referenced image still reports its own bytes. The refusal is carried
    /// by `activity`, which the policy engine reads — not by a size field
    /// answering zero, which would be a verdict wearing a measurement's
    /// clothes.
    #[test]
    fn a_referenced_image_reports_its_own_bytes_and_leaves_permission_to_policy() {
        let evidence = only_image(
            &image_json(
                "sha256:abc0123456789def",
                "example",
                "1",
                "1",
                "2.07GB",
                "2.07GB",
            ),
            &container_json(
                "dfe320e00b9f",
                "running",
                "4.1kB",
                "2026-09-29 10:24:51 +0800 CST",
            ),
        );

        assert_eq!(
            evidence.logical_bytes,
            ProbeOutcome::Observed(2_070_000_000)
        );
        assert_eq!(
            evidence.reclaimable_bytes,
            ProbeOutcome::Observed(2_070_000_000)
        );
        assert_eq!(
            lifecycle(&evidence).activity,
            DockerActivity::Active,
            "this is where the refusal lives"
        );
    }

    /// `UniqueSize`, not `Size`: what deleting an image frees is what belongs to
    /// it alone. `Size` counts layers shared with other images, and summing that
    /// over a report double counts every shared layer.
    #[test]
    fn an_image_reclaims_only_its_unique_bytes() {
        let evidence = only_image(
            &image_json(
                "sha256:abc0123456789def",
                "example",
                "1",
                "0",
                "2.07GB",
                "500MB",
            ),
            "",
        );

        assert_eq!(
            evidence.logical_bytes,
            ProbeOutcome::Observed(2_070_000_000)
        );
        assert_eq!(
            evidence.reclaimable_bytes,
            ProbeOutcome::Observed(500_000_000)
        );
    }

    /// The count is the authority for whether an image is referenced, and it is
    /// not always computed: `docker image ls` prints `N/A`, and some renderings
    /// print `-1`. Neither is zero.
    #[test]
    fn an_uncomputed_referrer_count_is_unknown_not_zero() {
        for count in ["N/A", "-1", ""] {
            let evidence = only_image(
                &image_json(
                    "sha256:abc0123456789def",
                    "example",
                    "1",
                    count,
                    "1GB",
                    "1GB",
                ),
                "",
            );

            assert_eq!(
                lifecycle(&evidence).activity,
                DockerActivity::Unknown,
                "count {count:?} must not read as unreferenced"
            );
            assert_eq!(
                lifecycle(&evidence).references,
                ProbeOutcome::Unavailable(ProbeReason::Failed)
            );
            assert_eq!(
                evidence.reclaimable_bytes,
                ProbeOutcome::Observed(1_000_000_000),
                "the size is known whether or not the referrer count is; what an \
                 unknown referrer count costs is the *activity* answer above, \
                 which is what policy refuses on"
            );
        }
    }

    /// An image only a stopped container references is referenced and not in
    /// use. `Active` is documented as "a running container, or an object a
    /// running container depends on", so a non-zero referrer count alone must
    /// not reach it. `Inactive` is the honest answer about the processes, and
    /// the two fields asserted below are why it is not an answer about the
    /// image: it is referenced, so removing it is not this resource's decision
    /// alone.
    #[test]
    fn an_image_only_a_stopped_container_references_is_not_active() {
        let evidence = only_image(
            &image_json("sha256:abc0123456789def", "example", "1", "1", "1GB", "1GB"),
            &container_json(
                "05b3ad2e9a97",
                "exited",
                "128MB",
                "2026-09-20 03:00:00 +0000 UTC",
            ),
        );

        assert_eq!(lifecycle(&evidence).activity, DockerActivity::Inactive);
        match &lifecycle(&evidence).references {
            ProbeOutcome::Observed(references) => {
                assert_eq!(references.referenced_by.len(), 1, "it is referenced");
                assert_eq!(references.active_referrers, 0, "by nothing running");
            }
            other => panic!("expected an observed referrer set, got {other:?}"),
        }
        assert_eq!(
            evidence.regenerability,
            Regenerability::Unknown,
            "and nothing observable says this image can be pulled again"
        );
    }

    /// The consistency check, and the reason the referrer set is two readings
    /// rather than one. Docker says two containers reference this image; only
    /// one is identifiable here. An incomplete list would read downstream as
    /// "and no others", so the query is reported unanswered — and since the
    /// container that *was* identified is stopped, whether a running one is
    /// among the rest is genuinely unknown.
    #[test]
    fn a_referrer_set_that_cannot_be_fully_accounted_for_is_unanswered() {
        let evidence = only_image(
            &image_json("sha256:abc0123456789def", "example", "1", "2", "1GB", "1GB"),
            &container_json(
                "05b3ad2e9a97",
                "exited",
                "128MB",
                "2026-09-20 03:00:00 +0000 UTC",
            ),
        );

        assert_eq!(
            lifecycle(&evidence).references,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
        assert_eq!(
            lifecycle(&evidence).activity,
            DockerActivity::Unknown,
            "an unidentified referrer may be running; that is not 'not in use'"
        );
    }

    /// The other half of the arm above: an incomplete referrer list cannot make
    /// an observed running container go away. `matched` is a subset of the real
    /// referrer set, so one running member proves active use however many others
    /// went unidentified — while the *list* stays unanswered.
    #[test]
    fn an_identified_running_referrer_establishes_activity_despite_an_incomplete_list() {
        let evidence = only_image(
            &image_json("sha256:abc0123456789def", "example", "1", "3", "1GB", "1GB"),
            &container_json(
                "dfe320e00b9f",
                "running",
                "4.1kB",
                "2026-09-29 10:24:51 +0800 CST",
            ),
        );

        assert_eq!(lifecycle(&evidence).activity, DockerActivity::Active);
        assert_eq!(
            lifecycle(&evidence).references,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }

    /// Docker records the image reference as it was typed. Every spelling below
    /// names the same image, and the matcher has to recognise all of them or
    /// the consistency check above would reject honest snapshots.
    #[test]
    fn a_container_can_name_its_image_by_tag_digest_or_short_id() {
        let id = "sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e";
        for reference in [
            "example:1",
            id,
            "93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e",
            "93ce27a88655",
        ] {
            assert!(
                names_image(reference, "example", "1", id),
                "{reference:?} names this image"
            );
        }
        assert!(
            names_image("example", "example", "latest", id),
            "an untagged `docker run example` resolved to :latest"
        );
    }

    /// The anti-vacuity control for the matcher: if it grew a permissive
    /// fallback, the test above would still pass and this one would fail.
    #[test]
    fn a_container_naming_a_different_image_does_not_match() {
        let id = "sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e";
        let other = "sha256:508a0857ec762b1ab1cece29193345b501fab1dd9d1228a7b617062954cecac6";
        for reference in [
            "example:2",
            "other:1",
            "example",
            other,
            "508a0857ec76",
            "",
            "sha256:",
        ] {
            assert!(
                !names_image(reference, "example", "1", id),
                "{reference:?} does not name this image"
            );
        }
    }

    /// `<none>` is Docker's word for a dangling image's absent repository and
    /// tag, not a name. Matching on it would make every dangling image a
    /// referrer of every container whose image reference is also unnamed.
    #[test]
    fn a_dangling_images_none_repository_is_not_a_name() {
        let id = "sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e";
        assert!(!names_image("<none>:<none>", "<none>", "<none>", id));
        assert!(!names_image("<none>", "<none>", "<none>", id));
        // But a dangling image is still matched by its digest, which is the
        // only way a container can refer to one.
        assert!(names_image(id, "<none>", "<none>", id));
    }

    /// AC 1, from the image side: images and containers come out of one
    /// snapshot as separate resources under separate kinds, never merged.
    #[test]
    fn one_snapshot_yields_images_and_containers_as_distinct_resources() {
        let snapshot = snapshot_with(
            &image_json("sha256:abc0123456789def", "example", "1", "1", "1GB", "1GB"),
            &container_json(
                "dfe320e00b9f",
                "running",
                "4.1kB",
                "2026-09-29 10:24:51 +0800 CST",
            ),
        );

        let mut kinds: Vec<ResourceKind> = containers(&snapshot, DETECTOR)
            .expect("containers parse")
            .into_iter()
            .chain(images_of(&snapshot))
            .map(|e| e.resource.kind)
            .collect();
        kinds.sort_by_key(|k| k.tag());

        assert_eq!(
            kinds,
            vec![ResourceKind::DockerContainer, ResourceKind::DockerImage]
        );
    }

    /// AC 6 / §6 for images too: nothing here constructs a cleanup path.
    #[test]
    fn images_are_detect_only() {
        let evidence = only_image(
            &image_json("sha256:abc0123456789def", "example", "1", "0", "1GB", "1GB"),
            "",
        );

        assert_eq!(evidence.native_cleanup, NativeCleanup::Unsupported);
    }

    /// A shape this code cannot read must not be reported as a machine with no
    /// images — the same trap as the container section, and the same answer.
    #[test]
    fn a_report_with_no_images_section_is_failed_not_empty() {
        let no_section: Value =
            serde_json::from_str("{\"Containers\":[],\"Volumes\":[]}").expect("valid JSON");
        match images(&no_section, DETECTOR) {
            Err(DetectorStatus::Failed(reason)) => assert!(reason.contains("Images")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// An image cannot be reported without its Docker-native identity, and a
    /// container missing from the list is also a container missing from some
    /// image's referrer set — which would then silently pass the consistency
    /// check at a smaller count.
    #[test]
    fn an_image_with_no_id_fails_the_detector() {
        let nameless = "{\"Containers\":\"0\",\"Size\":\"1GB\",\"UniqueSize\":\"1GB\",\"ID\":\"\"}";
        match images(&snapshot_with(nameless, ""), DETECTOR) {
            Err(DetectorStatus::Failed(reason)) => {
                assert!(reason.contains("image"), "got: {reason}");
                assert!(reason.contains("no ID"), "got: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// AC 5 and §8's "a named/persistent/user-data-bearing volume must fail
    /// closed". A name a human chose is treated as possibly holding the only
    /// copy of their data.
    #[test]
    fn a_named_volume_is_user_managed() {
        let evidence = only_volume(&volume_json("horo1500-cargo", "0", "123.5MB"), "");

        assert_eq!(
            lifecycle(&evidence).persistence,
            DockerPersistence::UserManaged
        );
    }

    /// The AC 5 control. Docker minted this name, and that is not the same
    /// fact as Docker being able to produce the contents again — an anonymous
    /// volume behind a database image's `VOLUME` line holds that database.
    /// `ToolManaged` here would be §7's Maven error in the case where there is
    /// no name to misread, so the assertion is written as the inequality it
    /// exists to protect as well as the value.
    #[test]
    fn an_anonymous_volume_is_unknown_not_tool_managed() {
        let evidence = only_volume(&volume_json(ANONYMOUS, "0", "0B"), "");

        assert_ne!(
            lifecycle(&evidence).persistence,
            DockerPersistence::ToolManaged,
            "Docker naming a volume is not Docker regenerating its contents"
        );
        assert_eq!(lifecycle(&evidence).persistence, DockerPersistence::Unknown);
    }

    /// The anti-vacuity control for [`is_anonymous_volume_name`]. Two of the
    /// four volumes on the machine this was written against are named
    /// `act-test-port-test-<64 hex>`: they *contain* 64 hex characters, a human
    /// chose them, and a `contains`-style test would have called both anonymous
    /// and downgraded a user-managed volume to `Unknown`.
    #[test]
    fn a_chosen_name_containing_a_hex_run_is_still_user_managed() {
        let name = format!("act-test-port-test-{ANONYMOUS}");
        let evidence = only_volume(&volume_json(&name, "0", "177B"), "");

        assert_eq!(
            lifecycle(&evidence).persistence,
            DockerPersistence::UserManaged,
            "a human-chosen prefix is what makes {name} a named volume"
        );
    }

    /// AC 3 and AC 4 for volumes: a running container with the volume mounted
    /// is active-use evidence, and the referrer is named so the workspace graph
    /// can link the two rather than re-deriving the relationship from strings.
    #[test]
    fn a_volume_a_running_container_has_mounted_is_active() {
        let evidence = only_volume(
            &volume_json("horo1500-cargo", "1", "123.5MB"),
            &container_with_mounts("694b49533978", "running", "horo1500-cargo"),
        );
        let lifecycle = lifecycle(&evidence);

        assert_eq!(lifecycle.activity, DockerActivity::Active);
        match &lifecycle.references {
            ProbeOutcome::Observed(refs) => {
                assert_eq!(refs.active_referrers, 1);
                assert_eq!(
                    refs.referenced_by
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                    vec!["docker_container:docker:694b49533978"]
                );
            }
            other => panic!("expected observed references, got {other:?}"),
        }
    }

    /// §8's "a stopped container is NOT automatically safe to delete", read
    /// from the volume side: the volume is mounted and nothing is running, so
    /// it is referenced and not in use — two different facts, and `Inactive`
    /// is the one that was established.
    #[test]
    fn a_volume_only_a_stopped_container_has_mounted_is_inactive_but_referenced() {
        let evidence = only_volume(
            &volume_json("horo1500-cargo", "1", "123.5MB"),
            &container_with_mounts("694b49533978", "exited", "horo1500-cargo"),
        );
        let lifecycle = lifecycle(&evidence);

        assert_eq!(lifecycle.activity, DockerActivity::Inactive);
        match &lifecycle.references {
            ProbeOutcome::Observed(refs) => {
                assert_eq!(refs.active_referrers, 0);
                assert_eq!(refs.referenced_by.len(), 1);
            }
            other => panic!("expected observed references, got {other:?}"),
        }
    }

    /// Docker was asked and said nothing has this mounted. A real answer, and
    /// still not deletion authority — the persistence axis and the protected
    /// kind rule both outlive it.
    #[test]
    fn an_unattached_volume_is_inactive_with_observed_empty_references() {
        let evidence = only_volume(&volume_json("act-toolcache", "0", "0B"), "");
        let lifecycle = lifecycle(&evidence);

        assert_eq!(lifecycle.activity, DockerActivity::Inactive);
        assert_eq!(
            lifecycle.references,
            ProbeOutcome::Observed(DockerReferences::none())
        );
    }

    /// `docker volume ls` renders `Links` as `"N/A"`, and only `system df -v`
    /// computes it. An uncounted attachment set must not read as an empty one:
    /// unknown is not zero.
    #[test]
    fn an_uncomputed_attachment_count_is_unknown_not_zero() {
        let evidence = only_volume(&volume_json("horo1500-cargo", "N/A", "123.5MB"), "");
        let lifecycle = lifecycle(&evidence);

        assert_eq!(lifecycle.activity, DockerActivity::Unknown);
        assert_eq!(
            lifecycle.references,
            ProbeOutcome::Unavailable(ProbeReason::Failed),
            "an attachment list that cannot be checked against Docker's own count \
             must not be published as complete"
        );
    }

    /// Docker counts one container with this mounted and the snapshot lists
    /// none. The unaccounted-for container may well be running, so neither the
    /// list nor the activity is an answer — and a published list missing a
    /// referrer would read downstream as "and nothing else has this mounted",
    /// which is the one thing it must not say about a volume.
    #[test]
    fn an_attachment_set_that_cannot_be_fully_accounted_for_is_unanswered() {
        let evidence = only_volume(&volume_json("horo1500-cargo", "1", "123.5MB"), "");
        let lifecycle = lifecycle(&evidence);

        assert_eq!(lifecycle.activity, DockerActivity::Unknown);
        assert_eq!(
            lifecycle.references,
            ProbeOutcome::Unavailable(ProbeReason::Failed)
        );
    }

    /// `Mounts` mixes volume names with bind-mount host paths. A container
    /// bind-mounting a directory is not a referrer of a volume, and matching on
    /// anything looser than exact equality would invent one — here, one that
    /// makes an attached volume look unattached by disagreeing with `Links`.
    #[test]
    fn a_bind_mount_path_never_matches_a_volume_name() {
        let evidence = only_volume(
            &volume_json("data", "0", "0B"),
            &container_with_mounts("694b49533978", "running", "/Users/x/data"),
        );
        let lifecycle = lifecycle(&evidence);

        assert_eq!(
            lifecycle.activity,
            DockerActivity::Inactive,
            "a container bind-mounting /Users/x/data does not have the `data` volume"
        );
        assert_eq!(
            lifecycle.references,
            ProbeOutcome::Observed(DockerReferences::none())
        );
    }

    /// One container, several volumes: `Mounts` is a comma-separated list, and
    /// each volume must find itself in it without a neighbouring name matching
    /// by accident.
    #[test]
    fn a_container_mounting_several_volumes_refers_to_each_of_them() {
        let snapshot = snapshot_of(
            "",
            &container_with_mounts("694b49533978", "running", "cargo-cache,build-out"),
            &format!(
                "{},{}",
                volume_json("cargo-cache", "1", "123.5MB"),
                volume_json("build-out", "1", "4.1kB")
            ),
        );

        for evidence in volumes_of(&snapshot) {
            assert_eq!(
                lifecycle(&evidence).activity,
                DockerActivity::Active,
                "{} should find itself in the mount list",
                evidence.resource
            );
        }
    }

    /// The locator is the volume's name, which is how Docker addresses it. Not
    /// the mount point: on this workstation's VM-backed runtime that path lives
    /// inside the VM and means nothing on the host.
    #[test]
    fn a_volume_is_identified_by_name_not_mount_point() {
        let evidence = only_volume(&volume_json("horo1500-cargo", "0", "123.5MB"), "");

        assert_eq!(
            evidence.resource.to_string(),
            "docker_volume:docker:horo1500-cargo"
        );
    }

    /// AC 6 / §6: a volume is enumerable and has no cleanup contract here.
    /// `NotRegenerable` + `Irreversible` are the two axes that make it `Ask`
    /// before the protected-kind rule makes it `Protected`, and `last_modified`
    /// is honestly not attempted — `system df -v` reports no volume timestamp.
    #[test]
    fn volumes_are_detect_only_with_no_age_claimed() {
        let evidence = only_volume(&volume_json("horo1500-cargo", "0", "123.5MB"), "");

        assert_eq!(evidence.native_cleanup, NativeCleanup::Unsupported);
        assert_eq!(evidence.regenerability, Regenerability::NotRegenerable);
        assert_eq!(evidence.recoverability, Recoverability::Irreversible);
        assert_eq!(
            evidence.last_modified,
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        );
    }

    /// The volume's own bytes, as Docker reports them. No permission judgement
    /// is folded in here: an attached volume still reports what removing it
    /// would free, and whether removal is allowed is policy's question.
    #[test]
    fn a_volume_reports_the_bytes_docker_measured_for_it() {
        let evidence = only_volume(
            &volume_json("horo1500-cargo", "1", "123.5MB"),
            &container_with_mounts("694b49533978", "running", "horo1500-cargo"),
        );

        assert_eq!(evidence.logical_bytes, ProbeOutcome::Observed(123_500_000));
        assert_eq!(
            evidence.reclaimable_bytes,
            ProbeOutcome::Observed(123_500_000)
        );
        assert!(!evidence.reclaimable_bytes_is_lower_bound);
    }

    /// A shape this code cannot read must not be reported as a machine with no
    /// volumes — the third instance of the same trap, and the same answer.
    #[test]
    fn a_report_with_no_volumes_section_is_failed_not_empty() {
        let no_section: Value =
            serde_json::from_str("{\"Images\":[],\"Containers\":[]}").expect("valid JSON");
        match volumes(&no_section, DETECTOR) {
            Err(DetectorStatus::Failed(reason)) => assert!(reason.contains("Volumes")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// A volume has no identity but its name, so a nameless one cannot be
    /// reported at all — and it is also a volume missing from some container's
    /// mount accounting.
    #[test]
    fn a_volume_with_no_name_fails_the_detector() {
        let nameless = "{\"Links\":\"0\",\"Size\":\"0B\",\"Name\":\"\"}";
        match volumes(&snapshot_of("", "", nameless), DETECTOR) {
            Err(DetectorStatus::Failed(reason)) => {
                assert!(reason.contains("volume"), "got: {reason}");
                assert!(reason.contains("no Name"), "got: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// AC 1, completed: one snapshot, three kinds, none of them collapsed into
    /// another.
    #[test]
    fn one_snapshot_yields_all_three_kinds_as_distinct_resources() {
        let snapshot = snapshot_of(
            &image_json("sha256:abc0123456789def", "example", "1", "1", "1GB", "1GB"),
            &container_with_mounts("dfe320e00b9f", "running", "horo1500-cargo"),
            &volume_json("horo1500-cargo", "1", "123.5MB"),
        );

        let mut kinds: Vec<ResourceKind> = containers(&snapshot, DETECTOR)
            .expect("containers parse")
            .into_iter()
            .chain(images_of(&snapshot))
            .chain(volumes_of(&snapshot))
            .map(|e| e.resource.kind)
            .collect();
        kinds.sort_by_key(|k| k.tag());

        assert_eq!(
            kinds,
            vec![
                ResourceKind::DockerContainer,
                ResourceKind::DockerImage,
                ResourceKind::DockerVolume,
            ]
        );
    }

    // No test spawns a real `docker`: daemon presence and reachability are
    // environment-dependent and CI must not depend on either. The
    // ToolAbsent/ToolNotRunning/Failed attribution is covered by
    // `super::docker`'s unit tests, which own `failure_status`.
}
