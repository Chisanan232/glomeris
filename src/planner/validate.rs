//! Turning what a provider claimed into what this process will act on
//! (HORO-1548).
//!
//! [`super::response`] answers one question — is this JSON the shape the
//! version 2 contract describes. Every field it produces is still a `String`
//! chosen by a provider. This module answers the second question: which of
//! those strings name something that exists on this machine, and what happens
//! to the ones that do not.
//!
//! # Drop or degrade, decided per field
//!
//! A single rule for both would be wrong in one direction or the other.
//! Refusing a whole plan over one mistyped confidence would throw away the
//! ranking a user paid for; keeping an item whose `resource_id` resolves to
//! nothing would put a recommendation in front of a human about a resource
//! this request never saw. So each field is classified once, here, and the
//! classification is the interesting content of this module:
//!
//! **Authority-bearing — the item is dropped and counted.** A field that
//! decides *what* a recommendation is about, or *what* is being recommended:
//! `resource_id`, `action_id`, `disposition`. There is no safe fallback for
//! any of them. "Some resource" is not a resource, and an unknown
//! disposition cannot degrade to [`Disposition::Keep`] either — a silent
//! rewrite of `recommend_now` to `keep` looks like the model made a
//! conservative call it did not make.
//!
//! **Descriptive — the value degrades and is counted.** A field that
//! qualifies a recommendation rather than constituting one: `confidence`
//! degrades to [`ClaimConfidence::Unknown`], an `evidence_refs` entry that
//! was never issued is removed from the list. Neither can make an item more
//! permissive than it already was.
//!
//! **Fail closed regardless — the whole entry is dropped.** An
//! `evidence_requests` entry with an unrecognised `probe_id`, a subject this
//! request never issued, or a subject of a kind that probe cannot answer for.
//! §16 of the campaign brief: unknown probe ids fail closed. These get run
//! (HORO-1549); a request that reached the runner holding a word this build
//! does not implement would be a decision deferred to the thing executing it,
//! and one naming a repository where a working tree belongs would leave the
//! runner choosing a subject on the model's behalf.
//!
//! # What cannot be constructed here at all
//!
//! [`ValidatedWorkspaceItem::action_id`] is an [`ActionId`], which holds a
//! `&'static str`. Model bytes are borrowed for the lifetime of one response
//! and are not `'static`, so there is no expression in this module — or in
//! any caller — that builds an [`ActionId`] out of a provider's string. The
//! one here comes from [`crate::actions::Action::id`] on an action the
//! registry already holds. Same for [`ResourceId`]: it is cloned out of
//! [`super::AliasTable`], which was built from local discovery. §15's "may
//! not invent an ActionId / ResourceId / filesystem path" is therefore a
//! property of the types rather than a check that could be forgotten.
//!
//! # This module still grants nothing
//!
//! A [`ValidatedWorkspacePlan`] is a ranking and an explanation. Every
//! resource in it must be re-judged by [`crate::policy::classify`] against
//! freshly collected evidence before anything executes, exactly as
//! [`crate::actions::llm::plan_with_llm`] already requires. Validation
//! narrows what a provider can say; it does not widen what the executor may
//! do.

use std::collections::BTreeSet;

use super::contract::{ClaimConfidence, Disposition, ObservationKind, ProbeId};
use super::project::GraphProjection;
use super::response::{
    EvidenceRequestClaim, ObservationClaim, PlanItemClaim, PlannerResponseClaim,
    WorkspaceProfileClaim,
};
use crate::actions::llm::sanitize_model_reason;
use crate::actions::ActionRegistry;
use crate::evidence::{ActionId, ResourceId};
use crate::workspace::WorkflowMode;

/// How many items one response may contribute.
///
/// Generous on purpose: this is a runaway bound, not a ranking policy. A
/// machine with several hundred discovered resources is ordinary, and an
/// honest plan covering all of them must not be cut.
pub const MAX_ITEMS: usize = 500;

/// How many free-text observations one response may contribute. Small,
/// because every one of these becomes a line a human reads.
pub const MAX_OBSERVATIONS: usize = 20;

/// How many probes one response may ask for. §16 requires a bound on the
/// number of probes per round; this is that bound at the parse boundary, so
/// HORO-1549 cannot be handed an unbounded list to bound for itself.
pub const MAX_EVIDENCE_REQUESTS: usize = 8;

/// How many uncertainties one item may carry.
pub const MAX_UNCERTAINTIES_PER_ITEM: usize = 5;

/// How many evidence references any one claim may cite. Applies after the
/// uncited ones are removed, so a response cannot spend the budget on
/// references it invented.
pub const MAX_EVIDENCE_REFS: usize = 8;

/// One provider response, reduced to what exists locally.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidatedWorkspacePlan {
    /// `None` when the response sent no profile, or sent one that could not
    /// be kept. Distinct from a profile whose mode is
    /// [`WorkflowMode::Unknown`]: that is the model reporting it cannot tell,
    /// which is an answer.
    pub profile: Option<ValidatedProfile>,
    pub items: Vec<ValidatedWorkspaceItem>,
    pub observations: Vec<ValidatedObservation>,
    pub evidence_requests: Vec<ValidatedEvidenceRequest>,
    pub counters: PlanValidationCounters,
}

/// The model's reading of how this developer is working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProfile {
    pub mode: WorkflowMode,
    pub confidence: ClaimConfidence,
    pub evidence_refs: Vec<String>,
    pub summary: Option<String>,
}

/// One recommendation, about a resource that exists, naming an action that
/// was on offer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedWorkspaceItem {
    /// Cloned out of the alias table this request was built from — never
    /// parsed from the response.
    pub resource: ResourceId,
    /// A registered action's own id, and one that
    /// [`super::dto::ResourceView::offered_action_ids`] listed for *this*
    /// resource.
    pub action_id: ActionId,
    pub disposition: Disposition,
    /// Whether the model is reporting an observation, an inference, or that
    /// it does not know. Degraded to [`ClaimConfidence::Unknown`] rather
    /// than guessed at.
    pub confidence: ClaimConfidence,
    /// The model's ordering hint. Advisory: nothing gates on it.
    pub priority: Option<u32>,
    /// References this request actually issued, and only those.
    pub evidence_refs: Vec<String>,
    /// What the model says it could not establish. Display-only model text,
    /// sanitized like every other free-text field here.
    pub uncertainties: Vec<String>,
    pub model_reason: Option<String>,
}

/// Something the model noticed that is not about one resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedObservation {
    pub kind: ObservationKind,
    pub evidence_refs: Vec<String>,
    pub detail: Option<String>,
}

/// A read-only probe the model would like run before it commits to an answer.
///
/// Carries a [`ProbeId`] and an alias, and there is nowhere in it to put a
/// command, an argument vector, a path, a URL or a credential — §16's list of
/// what a provider must never supply, enforced by the absence of a field
/// rather than by rejecting one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEvidenceRequest {
    pub probe_id: ProbeId,
    /// An alias this request issued. Which local subject it refers to is
    /// resolved by whoever runs the probe, from the same projection.
    pub subject_ref: String,
    pub reason: Option<String>,
}

/// What validation had to throw away.
///
/// Every field is a count rather than a boolean because the numbers are
/// reported: a response that lost 1 item of 40 is a typo, and one that lost
/// 39 is a provider answering about a different machine, and those need
/// different responses from whoever is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanValidationCounters {
    /// `resource_id` named an alias this request never issued.
    pub dropped_unknown_resource: u32,
    /// `action_id` was not among the actions offered for that resource.
    pub dropped_unoffered_action: u32,
    /// `disposition` was not one of the four words.
    pub dropped_unknown_disposition: u32,
    /// A second item about the same resource and action.
    pub dropped_duplicate_item: u32,
    /// An observation whose `kind` was not one of the five.
    pub dropped_unknown_observation_kind: u32,
    /// An evidence request naming a probe this build does not implement.
    pub dropped_unknown_probe: u32,
    /// An evidence request about a subject this request never issued.
    pub dropped_unknown_probe_subject: u32,
    /// An evidence request whose subject was issued and is the wrong *kind* of
    /// thing for that probe — `git_branch_state` about a repository, say. A
    /// separate count from [`Self::dropped_unknown_probe_subject`] because the
    /// two mean different things about the provider: one cited something it was
    /// never shown, the other misunderstood what a probe is for.
    pub dropped_incompatible_probe_subject: u32,
    /// A second request naming the same probe and the same subject.
    pub dropped_duplicate_evidence_request: u32,
    /// A `confidence` that was not one of the three words, degraded to
    /// [`ClaimConfidence::Unknown`].
    pub degraded_unknown_confidence: u32,
    /// A profile `mode` outside [`WorkflowMode::ALL`], degraded to
    /// [`WorkflowMode::Unknown`].
    pub degraded_unknown_workflow_mode: u32,
    /// Evidence references removed because this request never issued them.
    pub dropped_uncited_evidence_ref: u32,
    /// Entries cut by [`MAX_ITEMS`].
    pub truncated_items: u32,
    /// Entries cut by [`MAX_OBSERVATIONS`].
    pub truncated_observations: u32,
    /// Entries cut by [`MAX_EVIDENCE_REQUESTS`].
    pub truncated_evidence_requests: u32,
    /// Entries cut by [`MAX_UNCERTAINTIES_PER_ITEM`].
    pub truncated_uncertainties: u32,
    /// Entries cut by [`MAX_EVIDENCE_REFS`].
    pub truncated_evidence_refs: u32,
}

impl PlanValidationCounters {
    /// Whether anything at all was dropped, degraded or cut. For deciding
    /// whether a report needs to say so — not for deciding whether to use
    /// the plan, which stays usable either way.
    pub fn any(&self) -> bool {
        *self != Self::default()
    }
}

/// Validates one parsed response against the projection it answers.
///
/// Never fails: a response in which nothing resolves produces an empty plan
/// with counters explaining why, and the caller falls back to rule-only
/// ranking the same way it does when no provider is configured. An error
/// return here would make a provider's confusion into a user-visible failure
/// of Glomeris.
pub fn validate_response(
    claim: &PlannerResponseClaim,
    projection: &GraphProjection,
    actions: &ActionRegistry,
) -> ValidatedWorkspacePlan {
    let mut counters = PlanValidationCounters::default();
    let issued = projection.issued_evidence_refs();

    let profile = claim
        .workspace_profile
        .as_ref()
        .map(|profile| validate_profile(profile, &issued, &mut counters));

    let items = validate_items(&claim.items, projection, actions, &issued, &mut counters);
    let observations = validate_observations(&claim.observations, &issued, &mut counters);
    let evidence_requests =
        validate_evidence_requests(&claim.evidence_requests, projection, &mut counters);

    ValidatedWorkspacePlan {
        profile,
        items,
        observations,
        evidence_requests,
        counters,
    }
}

fn validate_profile(
    claim: &WorkspaceProfileClaim,
    issued: &BTreeSet<&str>,
    counters: &mut PlanValidationCounters,
) -> ValidatedProfile {
    let mode = match WorkflowMode::from_tag(&claim.mode) {
        Some(mode) => mode,
        None => {
            counters.degraded_unknown_workflow_mode += 1;
            WorkflowMode::Unknown
        }
    };

    // An unreadable mode forces the confidence down with it. Keeping the
    // model's "observed" beside a mode that degraded would publish "mode:
    // unknown, confidence: observed" — which reads as an established finding
    // that this developer has no pattern, from a response that in fact said
    // something this build could not read.
    let confidence = if counters.degraded_unknown_workflow_mode > 0 {
        ClaimConfidence::Unknown
    } else {
        confidence_or_unknown(&claim.confidence, counters)
    };

    ValidatedProfile {
        mode,
        confidence,
        evidence_refs: keep_issued_refs(&claim.evidence_refs, issued, counters),
        summary: claim.summary.as_deref().and_then(sanitize_model_reason),
    }
}

fn validate_items(
    claims: &[PlanItemClaim],
    projection: &GraphProjection,
    actions: &ActionRegistry,
    issued: &BTreeSet<&str>,
    counters: &mut PlanValidationCounters,
) -> Vec<ValidatedWorkspaceItem> {
    let mut items: Vec<ValidatedWorkspaceItem> = Vec::new();

    for claim in claims {
        if items.len() >= MAX_ITEMS {
            counters.truncated_items += 1;
            continue;
        }

        // Three authority-bearing fields, each of which drops the item.
        let Some(resource) = projection.aliases.resolve(&claim.resource_id) else {
            counters.dropped_unknown_resource += 1;
            continue;
        };
        let Some(action_id) =
            offered_action_id(projection, actions, &claim.resource_id, &claim.action_id)
        else {
            counters.dropped_unoffered_action += 1;
            continue;
        };
        let Some(disposition) = Disposition::from_tag(&claim.disposition) else {
            counters.dropped_unknown_disposition += 1;
            continue;
        };

        if items
            .iter()
            .any(|kept| kept.resource == *resource && kept.action_id == action_id)
        {
            counters.dropped_duplicate_item += 1;
            continue;
        }

        let mut uncertainties: Vec<String> = Vec::new();
        for uncertainty in &claim.uncertainties {
            let Some(text) = sanitize_model_reason(uncertainty) else {
                continue;
            };
            if uncertainties.len() >= MAX_UNCERTAINTIES_PER_ITEM {
                counters.truncated_uncertainties += 1;
                continue;
            }
            uncertainties.push(text);
        }

        items.push(ValidatedWorkspaceItem {
            resource: resource.clone(),
            action_id,
            disposition,
            confidence: confidence_or_unknown(&claim.confidence, counters),
            priority: claim.priority,
            evidence_refs: keep_issued_refs(&claim.evidence_refs, issued, counters),
            uncertainties,
            model_reason: claim.reason.as_deref().and_then(sanitize_model_reason),
        });
    }

    items
}

fn validate_observations(
    claims: &[ObservationClaim],
    issued: &BTreeSet<&str>,
    counters: &mut PlanValidationCounters,
) -> Vec<ValidatedObservation> {
    let mut observations: Vec<ValidatedObservation> = Vec::new();

    for claim in claims {
        // Dropped rather than degraded to some catch-all kind, because there
        // is no catch-all kind: the five are what the GUI knows how to
        // present, and a sixth would render as an unlabelled paragraph.
        let Some(kind) = ObservationKind::from_tag(&claim.kind) else {
            counters.dropped_unknown_observation_kind += 1;
            continue;
        };
        if observations.len() >= MAX_OBSERVATIONS {
            counters.truncated_observations += 1;
            continue;
        }

        observations.push(ValidatedObservation {
            kind,
            evidence_refs: keep_issued_refs(&claim.evidence_refs, issued, counters),
            detail: claim.detail.as_deref().and_then(sanitize_model_reason),
        });
    }

    observations
}

fn validate_evidence_requests(
    claims: &[EvidenceRequestClaim],
    projection: &GraphProjection,
    counters: &mut PlanValidationCounters,
) -> Vec<ValidatedEvidenceRequest> {
    let issued = projection.issued_evidence_refs();
    let mut requests: Vec<ValidatedEvidenceRequest> = Vec::new();

    for claim in claims {
        let Some(probe_id) = ProbeId::from_tag(&claim.probe_id) else {
            counters.dropped_unknown_probe += 1;
            continue;
        };
        // The subject is checked against every reference this request issued
        // rather than against resource aliases alone: `git_branch_state` is
        // asked about a worktree, and `workspace_history_summary` about the
        // baseline, neither of which is a resource.
        if !issued.contains(claim.subject_ref.as_str()) {
            counters.dropped_unknown_probe_subject += 1;
            continue;
        }
        // Issued is not enough (HORO-1549). The machine, the workflow baseline
        // and every repository are issued references, so `git_branch_state`
        // about `repo_1` cleared the check above while naming something a
        // branch probe cannot be run against — leaving whoever runs it to pick
        // one of that repository's working trees on the model's behalf.
        let subject_kind = projection.probe_subject_kind(&claim.subject_ref);
        if !subject_kind.is_some_and(|kind| probe_id.accepts(kind)) {
            counters.dropped_incompatible_probe_subject += 1;
            continue;
        }
        // Deduplicated on the whole question, not on the probe: two probes of
        // the same kind about two different working trees are two different
        // questions and both are wanted. `reason` is excluded from the key
        // deliberately — re-asking the same question with new prose is still
        // the same probe run, and letting the text distinguish them would make
        // the round budget spendable by rewording.
        if requests
            .iter()
            .any(|kept| kept.probe_id == probe_id && kept.subject_ref == claim.subject_ref)
        {
            counters.dropped_duplicate_evidence_request += 1;
            continue;
        }
        if requests.len() >= MAX_EVIDENCE_REQUESTS {
            counters.truncated_evidence_requests += 1;
            continue;
        }

        requests.push(ValidatedEvidenceRequest {
            probe_id,
            subject_ref: claim.subject_ref.clone(),
            reason: claim.reason.as_deref().and_then(sanitize_model_reason),
        });
    }

    requests
}

/// The registered action id a claim names, if that action was offered for
/// that resource.
///
/// Both halves matter and neither implies the other. The offered list is what
/// [`crate::actionability`] judged eligible for this resource's evidence; the
/// registry lookup is what produces an [`ActionId`] whose `&'static str` the
/// response could not have supplied. Version 1 did the second check only, so
/// it accepted any registered action on any resource.
fn offered_action_id(
    projection: &GraphProjection,
    actions: &ActionRegistry,
    resource_alias: &str,
    claimed: &str,
) -> Option<ActionId> {
    let offered = projection.resource_view(resource_alias)?;
    if !offered.offered_action_ids.contains(&claimed) {
        return None;
    }
    // Unreachable in practice — the offered list is built from this same
    // registry — but the alternative is `ActionId(claimed_as_static)`, and
    // there is no such thing, which is the point.
    Some(actions.get(claimed)?.id())
}

fn confidence_or_unknown(claimed: &str, counters: &mut PlanValidationCounters) -> ClaimConfidence {
    match ClaimConfidence::from_tag(claimed) {
        Some(confidence) => confidence,
        None => {
            counters.degraded_unknown_confidence += 1;
            ClaimConfidence::Unknown
        }
    }
}

/// The subset of `claimed` this request actually issued, bounded.
///
/// Removing an uncited reference rather than dropping the whole claim is the
/// descriptive-field rule: a citation is provenance for a human to follow,
/// and one bad citation does not make the recommendation itself unreadable.
/// What it must never do is stay in the list — a report rendering
/// `workspace_9` would be showing a reader a trail that leads nowhere.
fn keep_issued_refs(
    claimed: &[String],
    issued: &BTreeSet<&str>,
    counters: &mut PlanValidationCounters,
) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();

    for reference in claimed {
        if !issued.contains(reference.as_str()) {
            counters.dropped_uncited_evidence_ref += 1;
            continue;
        }
        if kept.contains(reference) {
            continue;
        }
        if kept.len() >= MAX_EVIDENCE_REFS {
            counters.truncated_evidence_refs += 1;
            continue;
        }
        kept.push(reference.clone());
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        Evidence, GitState, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability,
        ResourceFingerprint, ResourceKind, ResourceLocator,
    };
    use crate::planner::response::read_planner_response;
    use crate::planner::PlannerResponse;
    use crate::policy::{PolicyClass, PolicyDecision, ReasonCode};
    use crate::workspace::{MachineContext, WorkspaceEvidenceGraph, WorkspaceSurvey};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    const T0: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    /// A real cargo project, because `offered_action_ids` is computed by
    /// planning the action — and planning stats `Cargo.toml`. A fabricated
    /// path yields an empty offer list, which would make every
    /// offered-action assertion below pass for the wrong reason.
    fn temp_cargo_project() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glomeris-validate-{}-{nanos}-{n}",
            std::process::id()
        ));
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).expect("create temp target dir");
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write manifest");
        target_dir
    }

    fn candidate(path: &Path) -> (Evidence, PolicyDecision) {
        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(path.to_path_buf()),
        );
        let evidence = Evidence {
            resource: resource.clone(),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(4096),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(T0),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::CargoTargetDir.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            executable_dependency: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: at(86_400 * 7),
            sources: Vec::new(),
        };
        let decision = PolicyDecision {
            resource,
            class: PolicyClass::AutoSafe,
            reasons: vec![ReasonCode::NoActiveUseObserved],
            evidence_collected_at: T0,
            evaluated_at: T0,
            policy_version: 1,
        };
        (evidence, decision)
    }

    /// One projection carrying exactly one resource, with
    /// `cargo.clean.target_dir` genuinely on offer for it.
    fn projection() -> GraphProjection {
        wide_projection(1)
    }

    /// One projection that also carries a repository and a working tree, so
    /// `repo_1` and `workspace_1` are genuinely issued references.
    ///
    /// [`projection`] cannot serve for the kind checks: its resource sits in no
    /// repository, so those two references do not exist there and a wrong-kind
    /// request naming one would be refused as *unknown* — passing the assertion
    /// for the wrong reason.
    fn projection_in_a_repository() -> GraphProjection {
        let target = temp_cargo_project();
        let root = target
            .parent()
            .expect("a target has a project root")
            .to_path_buf();
        let mut candidate = candidate(&target);
        candidate.0.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: root.clone(),
            common_dir: root.join(".git"),
            dirty: false,
            untracked: false,
            worktree: true,
        }));

        let candidates = vec![candidate];
        let graph = WorkspaceEvidenceGraph::build(
            &candidates,
            &WorkspaceSurvey::unsurveyed(),
            MachineContext::unmeasured(),
            at(86_400 * 7),
        );
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );

        // Only ever the directory `temp_cargo_project` just created.
        assert!(
            root.starts_with(std::env::temp_dir()),
            "refusing to remove {root:?}, which is not under the temporary directory"
        );
        let _ = fs::remove_dir_all(&root);

        projection
    }

    /// A projection carrying `count` resources, for the bounds that cannot be
    /// reached with one. Each is its own cargo project, so
    /// `cargo.clean.target_dir` is on offer for every one of them.
    ///
    /// The temporary projects are removed once the projection is built, which
    /// is the only moment they are read: `offered_action_ids` comes from
    /// planning the action, and planning stats `Cargo.toml`. [`MAX_ITEMS`]
    /// needs 502 of them, and a test run that left 502 directories behind in
    /// the temporary directory every time would be its own small mess.
    fn wide_projection(count: usize) -> GraphProjection {
        let targets: Vec<PathBuf> = (0..count).map(|_| temp_cargo_project()).collect();
        let candidates: Vec<(Evidence, PolicyDecision)> =
            targets.iter().map(|target| candidate(target)).collect();
        let graph = WorkspaceEvidenceGraph::build(
            &candidates,
            &WorkspaceSurvey::unsurveyed(),
            MachineContext::unmeasured(),
            at(86_400 * 7),
        );
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );

        for target in &targets {
            let root = target.parent().expect("a target has a project root");
            // Only ever a directory this function created, under the
            // temporary directory, named for this process.
            assert!(
                root.starts_with(std::env::temp_dir()),
                "refusing to remove {root:?}, which is not under the temporary directory"
            );
            let _ = fs::remove_dir_all(root);
        }

        projection
    }

    /// Reads `raw` through the real parser, so no test here can assert about
    /// a claim shape the parser would have refused.
    fn validated(raw: &str, projection: &GraphProjection) -> ValidatedWorkspacePlan {
        match read_planner_response(raw).expect("the fixture parses as a v2 response") {
            PlannerResponse::V2 { claim, .. } => {
                validate_response(&claim, projection, &ActionRegistry::builtin())
            }
            PlannerResponse::V1(_) => panic!("the fixture was read as a version 1 response"),
        }
    }

    fn one_item(resource_id: &str, action_id: &str, disposition: &str, confidence: &str) -> String {
        format!(
            r#"{{"contract_version":2,"items":[{{"resource_id":"{resource_id}",
               "action_id":"{action_id}","disposition":"{disposition}",
               "confidence":"{confidence}"}}]}}"#
        )
    }

    // ---------------------------------------------------------------
    // The happy path, and that it is not vacuous
    // ---------------------------------------------------------------

    #[test]
    fn a_complete_response_validates_into_local_values() {
        let projection = projection();
        let plan = validated(
            r#"{
              "contract_version": 2,
              "workspace_profile": {
                "mode": "parallel_multi_worktree",
                "confidence": "inferred",
                "evidence_refs": ["workflow_history", "machine"],
                "summary": "several checkouts of one repository"
              },
              "items": [{
                "resource_id": "resource_1",
                "action_id": "cargo.clean.target_dir",
                "disposition": "recommend_now",
                "confidence": "observed",
                "priority": 1,
                "evidence_refs": ["resource_1"],
                "uncertainties": ["the build may be needed again this week"],
                "reason": "nothing has opened it in a week"
              }],
              "observations": [{
                "kind": "conflicting_evidence",
                "evidence_refs": ["resource_1"],
                "detail": "reported idle, but the tool was never asked"
              }],
              "evidence_requests": [{
                "probe_id": "process_activity",
                "subject_ref": "resource_1",
                "reason": "to tell idle from unobserved"
              }]
            }"#,
            &projection,
        );

        let profile = plan.profile.as_ref().expect("the profile survived");
        assert_eq!(profile.mode, WorkflowMode::ParallelMultiWorktree);
        assert_eq!(profile.confidence, ClaimConfidence::Inferred);
        assert_eq!(
            profile.evidence_refs,
            vec!["workflow_history".to_string(), "machine".to_string()]
        );
        assert_eq!(
            profile.summary.as_deref(),
            Some("several checkouts of one repository")
        );

        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        assert_eq!(item.action_id, ActionId("cargo.clean.target_dir"));
        assert_eq!(item.disposition, Disposition::RecommendNow);
        assert_eq!(item.confidence, ClaimConfidence::Observed);
        assert_eq!(item.priority, Some(1));
        assert_eq!(item.evidence_refs, vec!["resource_1".to_string()]);
        assert_eq!(item.uncertainties.len(), 1);
        assert_eq!(
            item.model_reason.as_deref(),
            Some("nothing has opened it in a week")
        );
        // The resource is the locally discovered one, not a value from the
        // response — which the response could not have named, since the
        // payload carried no path.
        assert_eq!(
            item.resource,
            *projection
                .aliases
                .resolve("resource_1")
                .expect("resource_1 was issued")
        );

        assert_eq!(plan.observations.len(), 1);
        assert_eq!(
            plan.observations[0].kind,
            ObservationKind::ConflictingEvidence
        );
        assert_eq!(plan.evidence_requests.len(), 1);
        assert_eq!(plan.evidence_requests[0].probe_id, ProbeId::ProcessActivity);
        assert_eq!(plan.evidence_requests[0].subject_ref, "resource_1");

        assert!(
            !plan.counters.any(),
            "a fully valid response lost something: {:?}",
            plan.counters
        );
    }

    #[test]
    fn an_empty_response_validates_to_an_empty_plan() {
        let plan = validated(r#"{"contract_version":2}"#, &projection());
        assert_eq!(plan, ValidatedWorkspacePlan::default());
        assert!(!plan.counters.any());
    }

    // ---------------------------------------------------------------
    // Authority-bearing fields drop the item
    // ---------------------------------------------------------------

    #[test]
    fn a_resource_alias_this_request_never_issued_drops_the_item() {
        let plan = validated(
            &one_item(
                "resource_9",
                "cargo.clean.target_dir",
                "recommend_now",
                "observed",
            ),
            &projection(),
        );
        assert!(plan.items.is_empty());
        assert_eq!(plan.counters.dropped_unknown_resource, 1);
    }

    /// The tightening over version 1. `node.clean.node_modules` is a
    /// registered action, so a registry-only check accepts it — on a Cargo
    /// target directory it was never offered for.
    #[test]
    fn a_registered_action_that_was_not_offered_for_this_resource_is_refused() {
        let projection = projection();
        let offered = projection
            .resource_view("resource_1")
            .expect("resource_1 was issued");
        assert!(
            offered
                .offered_action_ids
                .contains(&"cargo.clean.target_dir"),
            "the fixture offers nothing, so this test would pass vacuously: {:?}",
            offered.offered_action_ids
        );
        assert!(
            ActionRegistry::builtin()
                .get("node.clean.node_modules")
                .is_some(),
            "the refused id is not registered, so this test would pass for the wrong reason"
        );

        let plan = validated(
            &one_item(
                "resource_1",
                "node.clean.node_modules",
                "recommend_now",
                "observed",
            ),
            &projection,
        );
        assert!(plan.items.is_empty());
        assert_eq!(plan.counters.dropped_unoffered_action, 1);
    }

    #[test]
    fn an_invented_action_id_is_refused() {
        let plan = validated(
            &one_item("resource_1", "rm -rf /", "recommend_now", "observed"),
            &projection(),
        );
        assert!(plan.items.is_empty());
        assert_eq!(plan.counters.dropped_unoffered_action, 1);
    }

    /// An unknown disposition must not become the conservative one. Silently
    /// rewriting it to `keep` would credit the model with a caution it did
    /// not express, and rewriting it to `recommend_now` needs no argument.
    #[test]
    fn an_unknown_disposition_drops_the_item_rather_than_becoming_keep() {
        for claimed in ["delete", "recommend", "RECOMMEND_NOW", "auto_safe", "yes"] {
            let plan = validated(
                &one_item("resource_1", "cargo.clean.target_dir", claimed, "observed"),
                &projection(),
            );
            assert!(plan.items.is_empty(), "{claimed} produced an item");
            assert_eq!(
                plan.counters.dropped_unknown_disposition, 1,
                "{claimed} was not counted as an unknown disposition"
            );
        }
    }

    #[test]
    fn a_second_item_about_the_same_resource_and_action_is_dropped() {
        let plan = validated(
            r#"{"contract_version":2,"items":[
                 {"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"recommend_now","confidence":"observed"},
                 {"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"keep","confidence":"unknown"}]}"#,
            &projection(),
        );
        assert_eq!(plan.items.len(), 1);
        // The first one is kept, so a response cannot overwrite its own
        // earlier recommendation with a contradictory one.
        assert_eq!(plan.items[0].disposition, Disposition::RecommendNow);
        assert_eq!(plan.counters.dropped_duplicate_item, 1);
    }

    // ---------------------------------------------------------------
    // Descriptive fields degrade
    // ---------------------------------------------------------------

    #[test]
    fn an_unreadable_confidence_degrades_and_keeps_the_item() {
        let plan = validated(
            &one_item(
                "resource_1",
                "cargo.clean.target_dir",
                "recommend_now",
                "very_high",
            ),
            &projection(),
        );
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].confidence, ClaimConfidence::Unknown);
        assert_eq!(plan.counters.degraded_unknown_confidence, 1);
    }

    #[test]
    fn an_uncited_evidence_reference_is_removed_and_the_item_survives() {
        let plan = validated(
            r#"{"contract_version":2,"items":[
                 {"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"ask_user","confidence":"inferred",
                  "evidence_refs":["resource_1","workspace_9","/Users/someone/code",
                                   "machine"]}]}"#,
            &projection(),
        );
        assert_eq!(plan.items.len(), 1);
        assert_eq!(
            plan.items[0].evidence_refs,
            vec!["resource_1".to_string(), "machine".to_string()]
        );
        assert_eq!(plan.counters.dropped_uncited_evidence_ref, 2);
    }

    /// A mode this build cannot read takes the confidence down with it.
    /// Otherwise the report would carry "mode: unknown, confidence:
    /// observed", which reads as an established finding that this developer
    /// has no pattern.
    #[test]
    fn an_unreadable_workflow_mode_takes_its_confidence_with_it() {
        let plan = validated(
            r#"{"contract_version":2,"workspace_profile":
                 {"mode":"advanced","confidence":"observed"}}"#,
            &projection(),
        );
        let profile = plan.profile.as_ref().expect("the profile survived");
        assert_eq!(profile.mode, WorkflowMode::Unknown);
        assert_eq!(profile.confidence, ClaimConfidence::Unknown);
        assert_eq!(plan.counters.degraded_unknown_workflow_mode, 1);
    }

    #[test]
    fn an_unknown_observation_kind_drops_the_observation() {
        let plan = validated(
            r#"{"contract_version":2,"observations":[
                 {"kind":"general","detail":"something"},
                 {"kind":"missing_evidence","detail":"the tool was never asked"}]}"#,
            &projection(),
        );
        assert_eq!(plan.observations.len(), 1);
        assert_eq!(plan.observations[0].kind, ObservationKind::MissingEvidence);
        assert_eq!(plan.counters.dropped_unknown_observation_kind, 1);
    }

    // ---------------------------------------------------------------
    // Evidence requests fail closed
    // ---------------------------------------------------------------

    #[test]
    fn an_unknown_probe_id_fails_closed() {
        for claimed in ["shell", "run_command", "git", "gitBranchState", "rm -rf /"] {
            let plan = validated(
                &format!(
                    r#"{{"contract_version":2,"evidence_requests":
                        [{{"probe_id":"{claimed}","subject_ref":"resource_1"}}]}}"#
                ),
                &projection(),
            );
            assert!(
                plan.evidence_requests.is_empty(),
                "{claimed} was accepted as a probe"
            );
            assert_eq!(plan.counters.dropped_unknown_probe, 1, "{claimed}");
        }
    }

    #[test]
    fn a_probe_about_a_subject_this_request_never_issued_fails_closed() {
        for subject in ["resource_9", "/Users/someone/code", "workspace_4", ""] {
            let plan = validated(
                &format!(
                    r#"{{"contract_version":2,"evidence_requests":
                        [{{"probe_id":"git_branch_state","subject_ref":"{subject}"}}]}}"#
                ),
                &projection(),
            );
            assert!(
                plan.evidence_requests.is_empty(),
                "{subject} was accepted as a subject"
            );
            assert_eq!(plan.counters.dropped_unknown_probe_subject, 1, "{subject}");
        }
    }

    /// A subject need not be a resource. The workflow baseline is citable and
    /// probeable and has no entry in the alias table, so validating subjects
    /// against resource aliases alone would refuse a legitimate request.
    #[test]
    fn a_probe_about_a_non_resource_subject_is_accepted() {
        let plan = validated(
            r#"{"contract_version":2,"evidence_requests":
                 [{"probe_id":"workspace_history_summary",
                   "subject_ref":"workflow_history"}]}"#,
            &projection(),
        );
        assert_eq!(plan.evidence_requests.len(), 1);
        assert_eq!(
            plan.evidence_requests[0].probe_id,
            ProbeId::WorkspaceHistorySummary
        );
        assert!(!plan.counters.any());
    }

    /// The gap HORO-1549 closes over HORO-1548.
    ///
    /// Every subject below *was* issued by this request, so none of them is
    /// caught by the unknown-subject check — which the `dropped_unknown_probe_
    /// subject == 0` assertion holds to. They are refused for being the wrong
    /// shape of thing: a branch probe cannot be run against a repository
    /// without picking one of its working trees on the model's behalf, and a
    /// liveness probe has no owning tool to read off a working tree.
    #[test]
    fn a_probe_about_an_issued_subject_of_the_wrong_kind_fails_closed() {
        let projection = projection_in_a_repository();
        // Non-vacuity: all five references really are issued here, so each
        // refusal below is about the kind and not about the reference.
        for reference in [
            "machine",
            "workflow_history",
            "repo_1",
            "workspace_1",
            "resource_1",
        ] {
            assert!(
                projection.probe_subject_kind(reference).is_some(),
                "{reference} was not issued, so this fixture proves nothing"
            );
        }

        for (probe, subject) in [
            ("git_branch_state", "repo_1"),
            ("git_branch_state", "machine"),
            ("git_branch_state", "workflow_history"),
            ("git_branch_state", "resource_1"),
            ("git_patch_equivalence", "repo_1"),
            ("process_activity", "repo_1"),
            ("process_activity", "machine"),
            ("tool_liveness", "workspace_1"),
            ("tool_liveness", "workflow_history"),
            ("github_pr_state", "resource_1"),
            ("jira_task_state", "machine"),
            ("workspace_history_summary", "workspace_1"),
        ] {
            let plan = validated(
                &format!(
                    r#"{{"contract_version":2,"evidence_requests":
                        [{{"probe_id":"{probe}","subject_ref":"{subject}"}}]}}"#
                ),
                &projection,
            );
            assert!(
                plan.evidence_requests.is_empty(),
                "{probe} was accepted about {subject}"
            );
            assert_eq!(
                plan.counters.dropped_incompatible_probe_subject, 1,
                "{probe} about {subject}"
            );
            assert_eq!(
                plan.counters.dropped_unknown_probe_subject, 0,
                "{subject} was refused as unknown, so the kind check was not what refused it"
            );
        }
    }

    /// The positive control for the test above: every one of the seven probes
    /// is accepted about a subject of a kind it can answer for. Without this,
    /// a mapping that accepted nothing at all would pass.
    #[test]
    fn every_probe_is_accepted_about_a_subject_of_a_kind_it_accepts() {
        let projection = projection_in_a_repository();
        let mut covered: BTreeSet<&str> = BTreeSet::new();

        for (probe, subject) in [
            ("git_branch_state", "workspace_1"),
            ("git_patch_equivalence", "workspace_1"),
            ("process_activity", "workspace_1"),
            ("process_activity", "resource_1"),
            ("tool_liveness", "resource_1"),
            ("github_pr_state", "workspace_1"),
            ("jira_task_state", "workspace_1"),
            ("workspace_history_summary", "workflow_history"),
        ] {
            let plan = validated(
                &format!(
                    r#"{{"contract_version":2,"evidence_requests":
                        [{{"probe_id":"{probe}","subject_ref":"{subject}"}}]}}"#
                ),
                &projection,
            );
            assert_eq!(
                plan.evidence_requests.len(),
                1,
                "{probe} was refused about {subject}: {:?}",
                plan.counters
            );
            assert!(!plan.counters.any(), "{probe} about {subject}");
            covered.insert(probe);
        }

        // And the list above really does exercise all seven.
        assert_eq!(
            covered,
            ProbeId::ALL
                .iter()
                .map(|p| p.tag())
                .collect::<BTreeSet<_>>()
        );
    }

    /// Asking the same question twice runs it once. The budget in §16 is a
    /// budget on probes actually run, so a response that repeats itself must
    /// not be able to spend it twice over.
    #[test]
    fn a_repeated_evidence_request_is_asked_once() {
        let projection = projection_in_a_repository();
        let plan = validated(
            r#"{"contract_version":2,"evidence_requests":[
                 {"probe_id":"git_branch_state","subject_ref":"workspace_1"},
                 {"probe_id":"git_branch_state","subject_ref":"workspace_1"},
                 {"probe_id":"git_branch_state","subject_ref":"workspace_1",
                  "reason":"asking again, differently worded"}]}"#,
            &projection,
        );

        assert_eq!(plan.evidence_requests.len(), 1);
        assert_eq!(plan.counters.dropped_duplicate_evidence_request, 2);
        // The first one wins, reason and all, rather than the last.
        assert_eq!(plan.evidence_requests[0].reason, None);
    }

    /// Deduplication is on the whole question. Two probes of the same kind
    /// about two different subjects are two different questions, and a key of
    /// `probe_id` alone would silently answer only the first.
    #[test]
    fn the_same_probe_about_two_subjects_is_two_requests() {
        let projection = projection_in_a_repository();
        let plan = validated(
            r#"{"contract_version":2,"evidence_requests":[
                 {"probe_id":"process_activity","subject_ref":"workspace_1"},
                 {"probe_id":"process_activity","subject_ref":"resource_1"}]}"#,
            &projection,
        );

        assert_eq!(plan.evidence_requests.len(), 2);
        assert_eq!(
            plan.evidence_requests
                .iter()
                .map(|request| request.subject_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["workspace_1", "resource_1"]
        );
        assert!(!plan.counters.any());
    }

    // ---------------------------------------------------------------
    // Model text, and the bounds
    // ---------------------------------------------------------------

    /// Every free-text field the v2 contract added goes through the same
    /// sanitizer as version 1's single one, so none of them can carry a
    /// newline or an escape into a terminal line or a popover row.
    #[test]
    fn model_text_cannot_forge_output_in_any_of_the_new_fields() {
        let plan = validated(
            "{\"contract_version\":2,\
               \"workspace_profile\":{\"mode\":\"mixed\",\"confidence\":\"inferred\",\
                 \"summary\":\"one\\nRECOVERED 40 GB\"},\
               \"items\":[{\"resource_id\":\"resource_1\",\
                 \"action_id\":\"cargo.clean.target_dir\",\
                 \"disposition\":\"keep\",\"confidence\":\"observed\",\
                 \"reason\":\"two\\r\\u001b[2Kforged\",\
                 \"uncertainties\":[\"three\\nforged\"]}],\
               \"observations\":[{\"kind\":\"recovery_outlook\",\
                 \"detail\":\"four\\tforged\"}],\
               \"evidence_requests\":[{\"probe_id\":\"tool_liveness\",\
                 \"subject_ref\":\"resource_1\",\"reason\":\"five\\nforged\"}]}",
            &projection(),
        );

        let texts = [
            plan.profile.as_ref().and_then(|p| p.summary.clone()),
            plan.items[0].model_reason.clone(),
            Some(plan.items[0].uncertainties[0].clone()),
            plan.observations[0].detail.clone(),
            plan.evidence_requests[0].reason.clone(),
        ];
        for text in texts {
            let text = text.expect("the field survived sanitizing");
            assert!(
                !text.contains('\n') && !text.contains('\r') && !text.contains('\u{1b}'),
                "a control character survived: {text:?}"
            );
        }
    }

    #[test]
    fn a_blank_free_text_field_becomes_absent_rather_than_an_empty_row() {
        let plan = validated(
            r#"{"contract_version":2,"items":[
                 {"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"defer","confidence":"unknown","reason":"   ",
                  "uncertainties":["  ","real"]}]}"#,
            &projection(),
        );
        assert_eq!(plan.items[0].model_reason, None);
        assert_eq!(plan.items[0].uncertainties, vec!["real".to_string()]);
    }

    #[test]
    fn every_bound_holds_and_says_it_cut() {
        let observations: Vec<String> = (0..MAX_OBSERVATIONS + 3)
            .map(|n| format!(r#"{{"kind":"workflow_shape","detail":"n{n}"}}"#))
            .collect();
        // Each about a different resource. Ten copies of one question are
        // deduplicated (HORO-1549) and would never reach the count bound, so a
        // repeated fixture would test the wrong refusal.
        let requests: Vec<String> = (1..=MAX_EVIDENCE_REQUESTS + 2)
            .map(|n| format!(r#"{{"probe_id":"tool_liveness","subject_ref":"resource_{n}"}}"#))
            .collect();
        let uncertainties: Vec<String> = (0..MAX_UNCERTAINTIES_PER_ITEM + 4)
            .map(|n| format!(r#""u{n}""#))
            .collect();

        let plan = validated(
            &format!(
                r#"{{"contract_version":2,
                     "items":[{{"resource_id":"resource_1",
                       "action_id":"cargo.clean.target_dir","disposition":"keep",
                       "confidence":"unknown","uncertainties":[{}],
                       "evidence_refs":["machine","resource_1","machine"]}}],
                     "observations":[{}],"evidence_requests":[{}]}}"#,
                uncertainties.join(","),
                observations.join(","),
                requests.join(",")
            ),
            &wide_projection(MAX_EVIDENCE_REQUESTS + 2),
        );

        assert_eq!(plan.observations.len(), MAX_OBSERVATIONS);
        assert_eq!(plan.counters.truncated_observations, 3);
        assert_eq!(plan.evidence_requests.len(), MAX_EVIDENCE_REQUESTS);
        assert_eq!(plan.counters.truncated_evidence_requests, 2);
        assert_eq!(
            plan.items[0].uncertainties.len(),
            MAX_UNCERTAINTIES_PER_ITEM
        );
        assert_eq!(plan.counters.truncated_uncertainties, 4);
        // A repeated reference collapses rather than spending the budget
        // twice, and repetition is not a citation this request failed to
        // issue, so nothing is counted as uncited.
        assert_eq!(
            plan.items[0].evidence_refs,
            vec!["machine".to_string(), "resource_1".to_string()]
        );
        assert_eq!(plan.counters.dropped_uncited_evidence_ref, 0);
        assert_eq!(plan.counters.truncated_evidence_refs, 0);
    }

    /// [`MAX_ITEMS`] is the only bound here that needs a projection larger
    /// than a real machine's, and it is tested anyway: a bound nothing ever
    /// reaches is a bound nobody knows works.
    #[test]
    fn the_item_bound_cuts_items_that_would_otherwise_have_been_kept() {
        let count = MAX_ITEMS + 2;
        let projection = wide_projection(count);
        let items: Vec<String> = (1..=count)
            .map(|n| {
                format!(
                    r#"{{"resource_id":"resource_{n}",
                        "action_id":"cargo.clean.target_dir",
                        "disposition":"keep","confidence":"unknown"}}"#
                )
            })
            .collect();

        let plan = validated(
            &format!(r#"{{"contract_version":2,"items":[{}]}}"#, items.join(",")),
            &projection,
        );

        assert_eq!(plan.items.len(), MAX_ITEMS);
        assert_eq!(plan.counters.truncated_items, 2);
        // Every one of them was a valid item, so nothing else explains the
        // shortfall.
        assert_eq!(plan.counters.dropped_unknown_resource, 0);
        assert_eq!(plan.counters.dropped_unoffered_action, 0);
        assert_eq!(plan.counters.dropped_duplicate_item, 0);
    }

    /// The reference bound needs more distinct issued references than
    /// [`MAX_EVIDENCE_REFS`] to be exercised at all — a one-resource
    /// projection has three, and a test over it would report the bound
    /// holding when nothing had reached it.
    #[test]
    fn the_reference_bound_cuts_references_that_were_genuinely_issued() {
        let count = MAX_EVIDENCE_REFS + 3;
        let projection = wide_projection(count);
        let cited: Vec<String> = (1..=count).map(|n| format!(r#""resource_{n}""#)).collect();
        let issued = projection.issued_evidence_refs();
        for n in 1..=count {
            assert!(
                issued.contains(format!("resource_{n}").as_str()),
                "resource_{n} was never issued, so the bound is not what cut it"
            );
        }

        let plan = validated(
            &format!(
                r#"{{"contract_version":2,
                     "items":[{{"resource_id":"resource_1",
                       "action_id":"cargo.clean.target_dir","disposition":"keep",
                       "confidence":"unknown","evidence_refs":[{}]}}]}}"#,
                cited.join(",")
            ),
            &projection,
        );

        assert_eq!(plan.items[0].evidence_refs.len(), MAX_EVIDENCE_REFS);
        assert_eq!(plan.counters.truncated_evidence_refs, 3);
        assert_eq!(plan.counters.dropped_uncited_evidence_ref, 0);
    }

    /// The one place a bound is reported as a bound rather than inferred from
    /// a shortfall: `any()` is what a report keys the "this response lost
    /// something" line on.
    #[test]
    fn counters_are_silent_only_when_nothing_was_lost() {
        assert!(!PlanValidationCounters::default().any());
        let mut counters = PlanValidationCounters::default();
        counters.dropped_uncited_evidence_ref += 1;
        assert!(counters.any());
    }
}
