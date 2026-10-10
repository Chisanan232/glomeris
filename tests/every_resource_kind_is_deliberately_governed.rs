//! Every [`ResourceKind`], through [`classify`], with the cleanest evidence
//! that kind can carry — and then with each fail-closed signal applied in
//! turn (HORO-1551).
//!
//! # What this adds over the existing policy tests
//!
//! `policy::engine::tests` pins individual cells: a stale resource is Ask, an
//! unknown kind is Protected, a `NotRegenerable` resource is Ask. Each is
//! true, and each is written against the kinds that existed when it was
//! written. HORO-1543 added seven kinds and HORO-1544 added three, and
//! nothing in the suite iterates [`ResourceKind::ALL`] through the
//! classifier, so the thing nobody was checking is the thing that matters
//! most: that a *newly added* kind cannot reach `AUTO_SAFE` because nobody
//! remembered to think about it.
//!
//! Two halves, and both are needed:
//!
//! 1. [`expected_cleanest_class`] is an exhaustive `match`, so adding a
//!    variant to [`ResourceKind`] stops the compiler here until somebody
//!    writes down what the classifier should do with it. That is the part
//!    that cannot rot.
//! 2. The fail-closed sweeps assert a *negative* — "not `AUTO_SAFE`" — for
//!    every kind, which is exactly the shape of assertion that passes when
//!    the fixture is malformed. So the table in (1) doubles as the positive
//!    control: it is checked to contain real `AUTO_SAFE` entries, and the
//!    same builder produces the evidence both halves use. A fixture broken
//!    badly enough to make the sweeps vacuous breaks (1) first.
//!
//! # Hand-built evidence
//!
//! The same seam `docker_lifecycle_fails_closed.rs` uses, for the same
//! reason. Reaching this matrix through real detectors would mean arranging
//! a running container, a populated Maven local repository and a dirty
//! worktree on the founder's workstation. The detectors' own tests own "the
//! tool's output becomes this evidence"; this file owns "this evidence
//! cannot become permission".

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use glomeris::detectors::DetectorId;
use glomeris::evidence::{
    DockerActivity, DockerLifecycle, DockerPersistence, DockerReferences, Evidence, GitState,
    NativeCleanup, OwningTool, ProbeOutcome, ProbeReason, ProcessRef, Recoverability,
    Regenerability, ResourceFingerprint, ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::policy::{classify, PolicyClass, PolicyConfig, ReasonCode};

/// Far enough from the epoch that subtracting a staleness margin below still
/// lands on a representable instant.
const NOW: SystemTime = SystemTime::UNIX_EPOCH;

/// A path that matches none of `policy::protected`'s patterns: no `.git`, no
/// `.ssh`, no `.terraform`, not under `/usr`, `/System`, `/Volumes` or
/// `/dev`, and a final component that is neither `*.pem`/`*.key` nor
/// contains "credentials". Using the kind's own tag keeps each fixture
/// distinguishable in a failure message.
fn unprotected_path(kind: ResourceKind) -> PathBuf {
    PathBuf::from(format!("/tmp/glomeris-governance-gate/{}", kind.tag()))
}

/// Docker addresses its objects by id, not by path, so a Docker kind's
/// locator has to be the tool form — which is also what makes the three
/// path probes `NotAttempted` rather than observed-empty for those kinds.
fn is_docker(kind: ResourceKind) -> bool {
    matches!(
        kind,
        ResourceKind::DockerBuildCache
            | ResourceKind::DockerImage
            | ResourceKind::DockerContainer
            | ResourceKind::DockerVolume
    )
}

/// Docker answered every axis and answered "idle": not in use, tool-managed,
/// and nothing references it. The most permissive lifecycle a Docker object
/// can honestly carry.
fn idle_lifecycle() -> DockerLifecycle {
    DockerLifecycle {
        activity: DockerActivity::Inactive,
        persistence: DockerPersistence::ToolManaged,
        references: ProbeOutcome::Observed(DockerReferences::none()),
    }
}

/// The cleanest evidence this kind can carry: every field
/// [`ResourceKind::required_evidence`] asks for is `Observed`, no process
/// holds it, it is not in a git working tree, its owning tool is not
/// running, and it was collected just now.
///
/// `regenerability` and `recoverability` are the honest per-kind values, not
/// the most permissive ones. Overriding them would make this a test of a
/// resource that cannot exist: a Docker container whose writable layer is
/// refetchable is not a cleaner container, it is a different product.
fn cleanest_evidence(kind: ResourceKind) -> Evidence {
    let locator = if is_docker(kind) {
        ResourceLocator::Tool {
            tool: OwningTool::Docker,
            id: "0123456789ab".to_string(),
        }
    } else {
        ResourceLocator::Path(unprotected_path(kind))
    };

    Evidence {
        resource: ResourceId::new(kind, locator),
        fingerprint: ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        },
        detector: DetectorId("governance_gate"),
        logical_bytes: ProbeOutcome::Observed(8 * 1024 * 1024 * 1024),
        physical_bytes: None,
        reclaimable_bytes: ProbeOutcome::Observed(8 * 1024 * 1024 * 1024),
        reclaimable_bytes_is_lower_bound: false,
        last_modified: ProbeOutcome::Observed(NOW),
        last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
        regenerability: kind.regenerability(),
        recoverability: match kind {
            // The two kinds whose contents no tool can reproduce. See the
            // variant doc comments in `evidence::model`.
            ResourceKind::DockerContainer | ResourceKind::DockerVolume => {
                Recoverability::Irreversible
            }
            _ => Recoverability::RegenerableByTool,
        },
        native_cleanup: NativeCleanup::Unsupported,
        // A Docker object has no path, so the three path-shaped probes were
        // never attempted for it — which is a different fact from their
        // having run and found nothing, and the reason
        // `required_evidence()` does not ask a Docker kind for them.
        open_by_process: if is_docker(kind) {
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        } else {
            ProbeOutcome::Observed(Vec::new())
        },
        process_cwd_match: if is_docker(kind) {
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        } else {
            ProbeOutcome::Observed(Vec::new())
        },
        git_state: if is_docker(kind) {
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        } else {
            ProbeOutcome::Observed(None)
        },
        tool_liveness: ProbeOutcome::Observed(false),
        docker_lifecycle: is_docker(kind).then(idle_lifecycle),
        // HORO-1825: a Docker object has no path to probe, same as the
        // three path-shaped fields above; every other kind gets a clean
        // negative, so the four required-for-four-kinds AC cases below
        // aren't accidentally denied AUTO_SAFE by this field alone.
        executable_dependency: if is_docker(kind) {
            ProbeOutcome::Unavailable(ProbeReason::NotAttempted)
        } else {
            ProbeOutcome::Observed(glomeris::evidence::ExecutableDependencyReport::empty())
        },
        collected_at: NOW,
        sources: Vec::new(),
    }
}

/// What `classify` must do with [`cleanest_evidence`] for this kind, and the
/// reason it must give for doing it.
///
/// Exhaustive on purpose: a new [`ResourceKind`] fails to compile here, and
/// whoever adds it has to state the verdict rather than inherit one. The
/// reason is asserted alongside the class because "Ask" on its own is
/// satisfied by any refusal at all, including a wrong one — a Maven local
/// repository refused for `EvidenceStale` would pass a class-only check
/// while proving the classifier never read its regenerability.
fn expected_cleanest_class(kind: ResourceKind) -> (PolicyClass, ReasonCode) {
    match kind {
        // Fail-closed sink. Refused before any evidence is consulted.
        ResourceKind::Unknown => (
            PolicyClass::Protected,
            ReasonCode::ProtectedUnknownResourceKind,
        ),
        // The one Docker object whose contents are the user's, refused
        // unconditionally and without regard to how idle Docker says it is.
        ResourceKind::DockerVolume => (
            PolicyClass::Protected,
            ReasonCode::ProtectedPersistentVolume,
        ),
        // Deleting a container destroys a writable layer that recreating it
        // does not restore. Consent is possible; it cannot be assumed.
        ResourceKind::DockerContainer => (PolicyClass::Ask, ReasonCode::RecoverabilityIrreversible),
        // The two kinds that may hold the only copy of something no registry
        // has, with nothing observable to say which. Unknown is not false.
        ResourceKind::DockerImage | ResourceKind::MavenLocalRepository => {
            (PolicyClass::Ask, ReasonCode::RegenerabilityUnknown)
        }
        // Everything else: a real cache, refetchable or rebuildable, with
        // complete fresh evidence and no active use.
        ResourceKind::XcodeDerivedData
        | ResourceKind::HomebrewCache
        | ResourceKind::CargoTargetDir
        | ResourceKind::CargoRegistryCache
        | ResourceKind::NodeModules
        | ResourceKind::NodePackageManagerCache
        | ResourceKind::DockerBuildCache
        | ResourceKind::PipCache
        | ResourceKind::UvCache
        | ResourceKind::GoBuildCache
        | ResourceKind::GoModuleCache
        | ResourceKind::GradleCache
        | ResourceKind::SwiftPackageManagerCache
        | ResourceKind::SwiftPackageManagerBuildDir => {
            (PolicyClass::AutoSafe, ReasonCode::NoActiveUseObserved)
        }
    }
}

fn cfg() -> PolicyConfig {
    PolicyConfig::default()
}

/// The table itself, checked against the classifier. This is the test that
/// makes the negative sweeps below non-vacuous.
#[test]
fn the_cleanest_evidence_for_every_kind_lands_where_this_file_says_it_should() {
    for kind in ResourceKind::ALL {
        let (expected_class, expected_reason) = expected_cleanest_class(*kind);
        let decision = classify(&cleanest_evidence(*kind), &cfg(), NOW);

        assert_eq!(
            decision.class,
            expected_class,
            "{}: cleanest possible evidence classified {:?}, expected {:?} (reasons: {:?})",
            kind.tag(),
            decision.class,
            expected_class,
            decision.reasons,
        );
        assert!(
            decision.reasons.contains(&expected_reason),
            "{}: classified {:?} as expected but for the wrong reason — got {:?}, expected {:?} among them",
            kind.tag(),
            decision.class,
            decision.reasons,
            expected_reason,
        );
    }
}

/// Positive control for the sweeps. "No kind is AutoSafe under a failed
/// probe" is also true of a classifier that refuses everything, and of a
/// fixture builder that produces garbage. Both are excluded by requiring
/// the clean table to reach all three classes, with `AUTO_SAFE` reached by
/// a substantial majority rather than by one lucky kind.
#[test]
fn the_expectation_table_is_not_a_blanket_refusal() {
    let mut auto_safe = Vec::new();
    let mut ask = Vec::new();
    let mut protected = Vec::new();

    for kind in ResourceKind::ALL {
        match expected_cleanest_class(*kind).0 {
            PolicyClass::AutoSafe => auto_safe.push(kind.tag()),
            PolicyClass::Ask => ask.push(kind.tag()),
            PolicyClass::Protected => protected.push(kind.tag()),
        }
    }

    assert!(
        auto_safe.len() >= 10,
        "only {} kinds reach AUTO_SAFE on clean evidence ({auto_safe:?}) — the negative sweeps \
         in this file would be close to vacuous",
        auto_safe.len(),
    );
    assert!(
        !ask.is_empty(),
        "no kind is expected to reach ASK, so this file proves nothing about uncertainty"
    );
    assert!(
        !protected.is_empty(),
        "no kind is expected to reach PROTECTED, so this file proves nothing about refusal"
    );
    assert_eq!(
        auto_safe.len() + ask.len() + protected.len(),
        ResourceKind::ALL.len(),
        "the table and ResourceKind::ALL disagree on how many kinds exist",
    );
}

/// Every required probe failed at once, which is what
/// [`glomeris::evidence::Completeness::Failed`] means. No kind may be
/// `AUTO_SAFE`, and the ones that were must say the probe failed rather than
/// something vaguer.
#[test]
fn a_wholly_failed_probe_set_denies_auto_safe_to_every_kind() {
    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.logical_bytes = ProbeOutcome::Unavailable(ProbeReason::Failed);
        evidence.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::Failed);
        evidence.last_modified = ProbeOutcome::Unavailable(ProbeReason::Failed);
        evidence.tool_liveness = ProbeOutcome::Unavailable(ProbeReason::Failed);
        if !is_docker(*kind) {
            evidence.open_by_process = ProbeOutcome::Unavailable(ProbeReason::Failed);
            evidence.process_cwd_match = ProbeOutcome::Unavailable(ProbeReason::Failed);
            evidence.git_state = ProbeOutcome::Unavailable(ProbeReason::Failed);
            // HORO-1825: a *wholly* failed probe set must fail this field
            // too, or an executable-bearing kind (which now requires it)
            // only has 6 of 7 required fields missing — `Partial`, not
            // `Failed` — and the reason below becomes `EvidenceIncomplete`
            // instead of `EvidenceProbeFailed`, which is what this test
            // pins.
            evidence.executable_dependency = ProbeOutcome::Unavailable(ProbeReason::Failed);
        }

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: every probe failed and the classifier still said AUTO_SAFE (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );

        if expected_cleanest_class(*kind).0 == PolicyClass::AutoSafe {
            assert!(
                decision.reasons.contains(&ReasonCode::EvidenceProbeFailed),
                "{}: refused a wholly failed probe set for {:?} rather than for the failure itself",
                kind.tag(),
                decision.reasons,
            );
        }
    }
}

/// One required probe failed and the rest answered — the partial case, which
/// is the one a classifier is most likely to wave through, because most of
/// what it wanted is present. `reclaimable_bytes` is the field chosen
/// because every kind requires it.
#[test]
fn a_single_missing_required_probe_denies_auto_safe_to_every_kind() {
    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::TimedOut);

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: reclaimable bytes were never measured and the classifier still said AUTO_SAFE \
             (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );

        if expected_cleanest_class(*kind).0 == PolicyClass::AutoSafe {
            assert!(
                decision.reasons.contains(&ReasonCode::EvidenceIncomplete),
                "{}: refused incomplete evidence for {:?} rather than for its incompleteness",
                kind.tag(),
                decision.reasons,
            );
        }
    }
}

/// A probe that came back `ToolAbsent` is a different fact from one that
/// came back `Failed`, and neither is an observation of absence. Both must
/// deny `AUTO_SAFE`, and this sweep exists because
/// `ProbeOutcome::Unavailable(ToolAbsent)` is the reason a reader is most
/// tempted to treat as benign.
#[test]
fn an_absent_tool_is_not_an_observation_of_an_idle_resource() {
    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::ToolAbsent);

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: a required measurement was never taken because the tool is not installed, and \
             the classifier read that as clean (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );
    }
}

/// A process holds the resource open. Checked for every kind rather than for
/// path-backed kinds only: `classify` reads `open_by_process` regardless of
/// whether the kind's `required_evidence` asked for it, and that is the
/// behaviour worth pinning — a Docker object that some process turns out to
/// have open must not be exempt because Docker objects are addressed by id.
#[test]
fn a_process_holding_the_resource_denies_auto_safe_to_every_kind() {
    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.open_by_process =
            ProbeOutcome::Observed(vec![ProcessRef::new(4242, "claude".to_string())]);

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: a process has it open and the classifier said AUTO_SAFE (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );

        if expected_cleanest_class(*kind).0 == PolicyClass::AutoSafe {
            assert!(
                decision.reasons.contains(&ReasonCode::ResourceInActiveUse),
                "{}: refused an in-use resource for {:?} rather than for the active use",
                kind.tag(),
                decision.reasons,
            );
        }
    }
}

/// A coding agent's working directory sits inside the resource. The same
/// property as the open-file sweep on a different probe, because the two
/// are separate fields and only one of them was checked before HORO-1542.
#[test]
fn an_agent_working_inside_the_resource_denies_auto_safe_to_every_kind() {
    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.process_cwd_match =
            ProbeOutcome::Observed(vec![ProcessRef::new(9191, "codex".to_string())]);

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: an agent is working inside it and the classifier said AUTO_SAFE (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );
    }
}

/// Uncommitted work in the containing worktree. Path-backed kinds only —
/// a Docker image has no worktree, and asserting one would be asserting
/// against a fixture that cannot occur.
#[test]
fn a_dirty_worktree_denies_auto_safe_to_every_path_backed_kind() {
    for kind in ResourceKind::ALL.iter().filter(|k| !is_docker(**k)) {
        let mut evidence = cleanest_evidence(*kind);
        evidence.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from("/tmp/glomeris-governance-gate/repo"),
            common_dir: PathBuf::from("/tmp/glomeris-governance-gate/repo/.git"),
            dirty: true,
            untracked: false,
            worktree: false,
        }));

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: the containing worktree has uncommitted changes and the classifier said \
             AUTO_SAFE (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );

        if expected_cleanest_class(*kind).0 == PolicyClass::AutoSafe {
            assert!(
                decision.reasons.contains(&ReasonCode::GitWorktreeDirty),
                "{}: refused a dirty worktree for {:?} rather than for the dirtiness",
                kind.tag(),
                decision.reasons,
            );
        }
    }
}

/// Evidence older than the configured maximum. A resource measured long
/// enough ago may have been written to since, so the measurement is not a
/// current fact about it.
#[test]
fn stale_evidence_denies_auto_safe_to_every_kind() {
    let cfg = cfg();
    let collected_at = SystemTime::UNIX_EPOCH;
    let now = collected_at + cfg.max_evidence_age + Duration::from_secs(1);

    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.collected_at = collected_at;
        evidence.last_modified = ProbeOutcome::Observed(collected_at);

        let decision = classify(&evidence, &cfg, now);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: evidence older than max_evidence_age still reached AUTO_SAFE (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );

        if expected_cleanest_class(*kind).0 == PolicyClass::AutoSafe {
            assert!(
                decision.reasons.contains(&ReasonCode::EvidenceStale),
                "{}: refused stale evidence for {:?} rather than for its age",
                kind.tag(),
                decision.reasons,
            );
        }
    }
}

/// Per-instance regenerability overrides the kind's static default in the
/// conservative direction. A detector that established something worse
/// about this particular instance than its kind implies must be believed.
#[test]
fn per_instance_unknown_regenerability_denies_auto_safe_to_every_kind() {
    for kind in ResourceKind::ALL {
        let mut evidence = cleanest_evidence(*kind);
        evidence.regenerability = Regenerability::Unknown;

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: the detector could not establish that this instance is reproducible, and the \
             classifier said AUTO_SAFE anyway (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );
    }
}

/// The realistic Docker case, and the one the clean table deliberately does
/// not describe. A Docker object's evidence only exists because
/// `docker system df` answered, which needs a live daemon — so the honest
/// state for every Docker kind in practice carries `tool_liveness:
/// Observed(true)`, and no Docker kind is `AUTO_SAFE` there, build cache
/// included.
#[test]
fn a_live_daemon_denies_auto_safe_to_every_docker_kind() {
    for kind in ResourceKind::ALL.iter().filter(|k| is_docker(**k)) {
        let mut evidence = cleanest_evidence(*kind);
        evidence.tool_liveness = ProbeOutcome::Observed(true);

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: the Docker daemon is running and the classifier said AUTO_SAFE (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );
    }
}

/// The reference query alone, with Docker's activity answer left positive.
///
/// `classify` has two Docker gates and this is the second one: an
/// `active_referrers` count that never arrived must not travel the same path
/// as a count of zero. Nothing pinned it before, and the reason is worth
/// stating because it is a general trap — every fixture in the suite that
/// exercised an unobserved reference set *also* carried
/// `DockerActivity::Unknown`, so the activity gate refused it first and the
/// reference gate was never the thing under test. Deleting the reference
/// gate outright left the whole suite green.
///
/// So the discriminating input is this one: activity positively `Inactive`,
/// persistence tool-managed, and only the reference query unanswered. There
/// is exactly one gate left that can refuse it.
#[test]
fn an_unanswered_reference_query_alone_denies_auto_safe_to_every_docker_kind() {
    for kind in ResourceKind::ALL.iter().filter(|k| is_docker(**k)) {
        let mut evidence = cleanest_evidence(*kind);
        evidence.docker_lifecycle = Some(DockerLifecycle {
            activity: DockerActivity::Inactive,
            persistence: DockerPersistence::ToolManaged,
            references: ProbeOutcome::Unavailable(ProbeReason::Failed),
        });

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: Docker called it idle but never said what references it, and the classifier \
             read the silence as nothing (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );

        if expected_cleanest_class(*kind).0 == PolicyClass::AutoSafe {
            assert!(
                decision
                    .reasons
                    .contains(&ReasonCode::DockerActivityUnknown),
                "{}: refused an unanswered reference query for {:?} rather than for the gap \
                 itself",
                kind.tag(),
                decision.reasons,
            );
        }
    }
}

/// Docker declining to answer whether an object is in use must not travel
/// the same path as Docker answering that it is idle, for any Docker kind.
#[test]
fn unanswered_docker_activity_denies_auto_safe_to_every_docker_kind() {
    for kind in ResourceKind::ALL.iter().filter(|k| is_docker(**k)) {
        let mut evidence = cleanest_evidence(*kind);
        evidence.docker_lifecycle = Some(DockerLifecycle {
            activity: DockerActivity::Unknown,
            persistence: DockerPersistence::Unknown,
            references: ProbeOutcome::Unavailable(ProbeReason::Failed),
        });

        let decision = classify(&evidence, &cfg(), NOW);
        assert_ne!(
            decision.class,
            PolicyClass::AutoSafe,
            "{}: Docker never said whether this is in use and the classifier said AUTO_SAFE \
             (reasons: {:?})",
            kind.tag(),
            decision.reasons,
        );
    }
}
