//! Every Docker lifecycle state, through the whole classifier and the action
//! registry, with the controls that make the refusals mean something
//! (HORO-1544 AC 7).
//!
//! # What this adds over `policy::engine::tests`
//!
//! Those tests each pin one cell: a running container is Ask, an unanswered
//! reference query is Ask, a volume is Protected. Each is true and each would
//! also be true of a classifier that refused every Docker object
//! unconditionally — which would be a different product, one whose Docker
//! reasoning is decoration. Three properties are checked here that a blanket
//! refusal fails:
//!
//! 1. Every cell of the running/stopped/referenced/unreferenced/persistent/
//!    unknown matrix refuses, and the refusals are **not all the same**. A
//!    classifier that stopped reading `docker_lifecycle` would still refuse
//!    everything, and would collapse the reason set.
//! 2. The refusals are reached by this harness, which is also shown reaching
//!    `AUTO_SAFE` for a non-Docker resource built the same way. Without that
//!    control, "no cell is AutoSafe" could just as well mean the fixtures were
//!    malformed.
//! 3. No Docker kind carries an offered action — exhaustively over
//!    [`ResourceKind::ALL`] rather than for the one kind that existed when the
//!    registry test was written, and with a non-Docker kind proving the
//!    registry does offer actions to somebody.
//!
//! # Hand-built evidence, and why that is the right seam here
//!
//! The lifecycle facts are shaped exactly as `detectors::docker_objects`
//! produces them from `docker system df -v`, and that parse is tested against
//! captured Docker JSON in that module. Driving the real detector from here
//! would require a real daemon in a state this test chose, which is precisely
//! what must never be arranged on the founder's workstation: the matrix below
//! includes "a container is running" and "a volume holds a developer's
//! database", and no test of mine is going to create or remove either. The
//! division is deliberate — the detector's tests own "Docker's output becomes
//! this evidence", this file owns "this evidence cannot become permission".

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glomeris::actionability::eligible_action_ids;
use glomeris::actions::ActionRegistry;
use glomeris::detectors::DetectorId;
use glomeris::evidence::{
    DockerActivity, DockerLifecycle, DockerPersistence, DockerReferences, Evidence, NativeCleanup,
    OwningTool, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint, ResourceId,
    ResourceKind, ResourceLocator,
};
use glomeris::policy::{classify, PolicyClass, PolicyConfig, ReasonCode};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

/// A Docker object as the detector emits one: addressed by Docker's own id,
/// with the three path probes `NotAttempted` because there is no path to point
/// them at, and the per-kind `regenerability()` the model assigns.
fn docker_evidence(kind: ResourceKind, lifecycle: DockerLifecycle) -> Evidence {
    Evidence {
        resource: ResourceId::new(
            kind,
            ResourceLocator::Tool {
                tool: OwningTool::Docker,
                id: "0123456789ab".to_string(),
            },
        ),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: DetectorId("docker_objects"),
        logical_bytes: ProbeOutcome::Observed(4 * 1024 * 1024 * 1024),
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Observed(4 * 1024 * 1024 * 1024),
        reclaimable_bytes_is_lower_bound: false,
        last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability: kind.regenerability(),
        recoverability: match kind {
            // A container's writable layer and a volume's contents are gone
            // for good; an image can be pulled again if its registry still
            // has it, which is why the image line is the one the activity
            // axis has to carry on its own.
            ResourceKind::DockerContainer | ResourceKind::DockerVolume => {
                Recoverability::Irreversible
            }
            _ => Recoverability::RegenerableByTool,
        },
        native_cleanup: NativeCleanup::Unsupported,
        open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        tool_liveness: ProbeOutcome::Observed(true),
        docker_lifecycle: Some(lifecycle),
        executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        collected_at: NOW,
        sources: Vec::new(),
    }
}

/// The identity of some other Docker object that references the subject.
fn referrer() -> ResourceId {
    ResourceId::new(
        ResourceKind::DockerContainer,
        ResourceLocator::Tool {
            tool: OwningTool::Docker,
            id: "fedcba987654".to_string(),
        },
    )
}

/// Docker answered on every axis: nothing references this, nothing runs.
fn observed_idle(persistence: DockerPersistence) -> DockerLifecycle {
    DockerLifecycle {
        activity: DockerActivity::Inactive,
        persistence,
        references: ProbeOutcome::Observed(DockerReferences::none()),
    }
}

/// One cell of the matrix: a description, the evidence, and nothing else. What
/// each cell classifies to is asserted rather than tabulated, because a table
/// of expected classes is the shape of test that gets updated to match a
/// regression.
struct Cell {
    what: &'static str,
    evidence: Evidence,
}

/// The running/stopped/referenced/unreferenced/persistent/unknown matrix of
/// AC 7, over all three object kinds plus the build cache.
fn matrix() -> Vec<Cell> {
    let mut cells = vec![
        Cell {
            what: "a running container",
            evidence: docker_evidence(
                ResourceKind::DockerContainer,
                DockerLifecycle {
                    activity: DockerActivity::Active,
                    persistence: DockerPersistence::ToolManaged,
                    references: ProbeOutcome::Observed(DockerReferences::none()),
                },
            ),
        },
        Cell {
            what: "a stopped container",
            evidence: docker_evidence(
                ResourceKind::DockerContainer,
                observed_idle(DockerPersistence::ToolManaged),
            ),
        },
        Cell {
            what: "an image a running container needs",
            evidence: docker_evidence(
                ResourceKind::DockerImage,
                DockerLifecycle {
                    activity: DockerActivity::Active,
                    persistence: DockerPersistence::ToolManaged,
                    references: ProbeOutcome::Observed(DockerReferences {
                        referenced_by: vec![referrer()],
                        active_referrers: 1,
                    }),
                },
            ),
        },
        Cell {
            what: "an image only a stopped container needs",
            evidence: docker_evidence(
                ResourceKind::DockerImage,
                DockerLifecycle {
                    activity: DockerActivity::Inactive,
                    persistence: DockerPersistence::ToolManaged,
                    references: ProbeOutcome::Observed(DockerReferences {
                        referenced_by: vec![referrer()],
                        active_referrers: 0,
                    }),
                },
            ),
        },
        Cell {
            what: "an unreferenced image Docker calls 100% reclaimable",
            evidence: docker_evidence(
                ResourceKind::DockerImage,
                observed_idle(DockerPersistence::ToolManaged),
            ),
        },
        Cell {
            what: "a named volume nothing has mounted",
            evidence: docker_evidence(
                ResourceKind::DockerVolume,
                observed_idle(DockerPersistence::UserManaged),
            ),
        },
        Cell {
            what: "an anonymous volume Docker minted itself",
            evidence: docker_evidence(
                ResourceKind::DockerVolume,
                observed_idle(DockerPersistence::ToolManaged),
            ),
        },
        Cell {
            what: "a volume whose persistence intent could not be established",
            evidence: docker_evidence(
                ResourceKind::DockerVolume,
                observed_idle(DockerPersistence::Unknown),
            ),
        },
        Cell {
            what: "a build cache Docker reported as idle and unreferenced",
            evidence: docker_evidence(
                ResourceKind::DockerBuildCache,
                observed_idle(DockerPersistence::ToolManaged),
            ),
        },
    ];

    // The same daemon-unreachable state on every kind, because "the answer is
    // unknown" has to survive independently of which object was asked about.
    cells.extend(
        [
            ResourceKind::DockerImage,
            ResourceKind::DockerContainer,
            ResourceKind::DockerVolume,
            ResourceKind::DockerBuildCache,
        ]
        .into_iter()
        .map(|kind| Cell {
            what: "an object whose daemon stopped answering",
            evidence: docker_evidence(kind, DockerLifecycle::unknown(ProbeReason::ToolNotRunning)),
        }),
    );

    cells
}

/// A disposable directory under the system temp dir. Nothing outside it is
/// touched, and the one test that creates one removes it again.
fn make_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "glomeris-h1544-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A non-Docker resource built by the same code path, with the same clean
/// correlation story, that does reach `AUTO_SAFE`. The control for
/// [`no_docker_lifecycle_state_reaches_auto_safe`]: without it, a fixture
/// malformed enough to be refused for some unrelated reason would read as
/// proof of the Docker rules.
///
/// `target` must be inside a real minimal cargo project, because
/// `cargo.clean.target_dir`'s planner stats `Cargo.toml` and refuses without
/// one — which the action-registry control below depends on.
fn auto_safe_control(target: &Path) -> Evidence {
    let mut ev = docker_evidence(
        ResourceKind::CargoTargetDir,
        observed_idle(DockerPersistence::ToolManaged),
    );
    ev.resource = ResourceId::new(
        ResourceKind::CargoTargetDir,
        ResourceLocator::Path(target.to_path_buf()),
    );
    ev.detector = DetectorId("cargo_target_dir");
    ev.regenerability = ResourceKind::CargoTargetDir.regenerability();
    ev.recoverability = Recoverability::RegenerableByRebuild;
    // A path-located resource is asked the path questions, and answering them
    // is what `Completeness::Complete` means for one.
    ev.open_by_process = ProbeOutcome::Observed(Vec::new());
    ev.process_cwd_match = ProbeOutcome::Observed(Vec::new());
    ev.git_state = ProbeOutcome::Observed(None);
    ev.tool_liveness = ProbeOutcome::Observed(false);
    ev.docker_lifecycle = None;
    // HORO-1825: CargoTargetDir now requires this field too.
    ev.executable_dependency =
        ProbeOutcome::Observed(glomeris::evidence::ExecutableDependencyReport::empty());
    ev
}

/// A real, minimal cargo project in a disposable directory. Returns its
/// `target/` — the resource a detector would report.
fn cargo_project(root: &Path) -> PathBuf {
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write Cargo.toml");
    let target = root.join("target");
    fs::create_dir_all(&target).expect("create target dir");
    target
}

/// The control runs first in its own test, so a failure here is read as "the
/// harness is broken" rather than as "the Docker rules are broken".
#[test]
fn the_harness_can_reach_auto_safe_so_the_refusals_below_are_findings() {
    let root = make_temp_dir("control-policy");
    let ev = auto_safe_control(&cargo_project(&root));
    let decision = classify(&ev, &PolicyConfig::default(), NOW);

    assert_eq!(
        decision.class,
        PolicyClass::AutoSafe,
        "the control must be AutoSafe or nothing this file asserts is meaningful; \
         got reasons {:?}",
        decision.reasons
    );

    fs::remove_dir_all(&root).ok();
}

/// AC 5 generalised: no cell of the matrix is executable without a human,
/// whatever Docker said about it and however favourably the rest of the
/// evidence reads.
#[test]
fn no_docker_lifecycle_state_reaches_auto_safe() {
    for cell in matrix() {
        let decision = classify(&cell.evidence, &PolicyConfig::default(), NOW);

        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{} ({}) must not be auto-safe; reasons {:?}",
            cell.what,
            cell.evidence.resource.kind.tag(),
            decision.reasons
        );
        assert!(
            !decision.reasons.is_empty(),
            "{} was refused without saying why",
            cell.what
        );
    }
}

/// The mutation control for the test above. A classifier that refused every
/// Docker object without reading `docker_lifecycle` would satisfy
/// `no_docker_lifecycle_state_reaches_auto_safe` completely, and would fail
/// here: the matrix distinguishes in-use from unknown from persistent-by-kind,
/// and those must arrive as different reasons.
#[test]
fn the_refusals_are_not_one_blanket_refusal() {
    let mut seen: Vec<Vec<ReasonCode>> = Vec::new();
    for cell in matrix() {
        let decision = classify(&cell.evidence, &PolicyConfig::default(), NOW);
        if !seen.contains(&decision.reasons) {
            seen.push(decision.reasons);
        }
    }

    assert!(
        seen.len() >= 4,
        "the matrix should be refused for distinguishable reasons, not one; got {seen:?}"
    );

    let flat: Vec<ReasonCode> = seen.iter().flatten().copied().collect();
    for required in [
        // A running container, and the image it needs.
        ReasonCode::DockerObjectInUse,
        // A daemon that stopped answering, and an unanswered reference query.
        ReasonCode::DockerActivityUnknown,
        // Every volume, by kind, before any evidence is consulted.
        ReasonCode::ProtectedPersistentVolume,
    ] {
        assert!(
            flat.contains(&required),
            "{} never appeared, so nothing in the matrix exercises it: {seen:?}",
            required.as_str()
        );
    }
}

/// Unknown is not a quieter idle, at the level a caller sees. The two cells
/// differ in the activity axis alone and must not classify alike.
#[test]
fn an_unknown_activity_is_refused_differently_from_an_observed_idle_one() {
    let idle = docker_evidence(
        ResourceKind::DockerImage,
        observed_idle(DockerPersistence::ToolManaged),
    );
    let mut unknown_activity = idle.clone();
    unknown_activity.docker_lifecycle = Some(DockerLifecycle {
        activity: DockerActivity::Unknown,
        persistence: DockerPersistence::ToolManaged,
        references: ProbeOutcome::Observed(DockerReferences::none()),
    });

    let idle_reasons = classify(&idle, &PolicyConfig::default(), NOW).reasons;
    let unknown_reasons = classify(&unknown_activity, &PolicyConfig::default(), NOW).reasons;

    assert_ne!(
        idle_reasons, unknown_reasons,
        "the activity axis is the only difference between these two, so it has to show"
    );
    assert!(unknown_reasons.contains(&ReasonCode::DockerActivityUnknown));
    assert!(!idle_reasons.contains(&ReasonCode::DockerActivityUnknown));
}

/// AC 6, and the safety requirement that no deletion action lands merely
/// because a detector can enumerate an object. Exhaustive over
/// [`ResourceKind::ALL`], so a fifth Docker kind added later is covered by this
/// test on the day it is added rather than on the day someone remembers.
#[test]
fn every_docker_kind_is_detect_only_in_the_action_registry() {
    let registry = ActionRegistry::builtin();
    let docker_kinds: Vec<ResourceKind> = ResourceKind::ALL
        .iter()
        .copied()
        .filter(|kind| kind.owning_tool() == OwningTool::Docker)
        .collect();

    assert!(
        docker_kinds.len() >= 4,
        "expected the four Docker kinds HORO-1544 leaves behind, found {docker_kinds:?}"
    );

    for kind in docker_kinds {
        assert!(
            registry.find_for_kind(kind).is_none(),
            "{} has a registered cleanup action; Docker is detect-only",
            kind.tag()
        );
        assert!(
            registry.ids_for_kind(kind).is_empty(),
            "{} offers an action id; Docker is detect-only",
            kind.tag()
        );
    }

    // The control: the registry does hand out actions, so the emptiness above
    // is about Docker and not about an empty registry.
    assert!(
        registry
            .find_for_kind(ResourceKind::CargoTargetDir)
            .is_some(),
        "the registry must offer something to somebody"
    );
}

/// The same question one layer up, where a surface actually asks it. A Docker
/// object reaching `Ask` rather than `Protected` still offers nothing, so
/// nothing downstream can present it as executable-pending-consent.
#[test]
fn no_docker_lifecycle_state_offers_an_action_to_a_surface() {
    let registry = ActionRegistry::builtin();

    for cell in matrix() {
        let decision = classify(&cell.evidence, &PolicyConfig::default(), NOW);
        let offered = eligible_action_ids(&cell.evidence, &decision, &registry);

        assert!(
            offered.is_empty(),
            "{} ({:?}) offered {offered:?}",
            cell.what,
            decision.class
        );
    }

    // The control again, at this layer: `eligible_action_ids` is capable of
    // returning something, so the empties above are Docker's doing.
    let root = make_temp_dir("control-actionability");
    let control = auto_safe_control(&cargo_project(&root));
    let decision = classify(&control, &PolicyConfig::default(), NOW);
    assert!(
        !eligible_action_ids(&control, &decision, &registry).is_empty(),
        "a clean cargo target dir must offer its cargo action"
    );

    fs::remove_dir_all(&root).ok();
}
