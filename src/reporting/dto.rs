//! Report DTOs (HORO-955): small, explicit projections of domain types
//! (`Evidence`, `PolicyDecision`) safe to `Serialize`.
//!
//! Mirrors [`crate::actions::llm::LlmResourceView`]'s own reasoning (see
//! that type's doc comment): never `#[derive(Serialize)]` a domain type
//! directly. Every field here is deliberately hand-picked so adding a
//! field to `Evidence`/`PolicyDecision` upstream has no effect on what a
//! `--json` report shows until a human explicitly adds it here.

use serde::Serialize;

use crate::actions::Action;
use crate::evidence::{Completeness, Confidence, Evidence, NativeCleanup, Regenerability};
use crate::policy::PolicyDecision;

use super::bytes::human_bytes;
use super::impact::{classify_impact, ImpactContext, ImpactThresholds};
use super::policy_label::{label_for, PolicyLabel};
use crate::evidence::probe::ProbeOutcome;
use crate::workspace::{UpstreamState, WorkspaceFamily, WorkspaceWorktree};

fn regenerability_tag(r: Regenerability) -> &'static str {
    match r {
        Regenerability::RegenerableByTool => "regenerable_by_tool",
        Regenerability::RegenerableByRebuild => "regenerable_by_rebuild",
        Regenerability::NotRegenerable => "not_regenerable",
        Regenerability::Unknown => "unknown",
    }
}

/// `pub(crate)` rather than private since HORO-1308: `cli::build_llm_plan_report`
/// needs the same two tags for `LlmPlanItemReport`, and one mapping shared
/// with `ExplainReport` beats a second copy that could disagree with it
/// about what `Partial` is called.
pub(crate) fn completeness_tag(c: &Completeness) -> &'static str {
    match c {
        Completeness::Complete => "complete",
        Completeness::Partial { .. } => "partial",
        Completeness::Failed => "failed",
    }
}

/// See [`completeness_tag`] for why this is `pub(crate)`.
pub(crate) fn confidence_tag(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

/// One line of the `--progress-json` NDJSON stream emitted on stderr
/// (HORO-1052) while `detect`/`explain`/`llm-plan`'s shared discovery
/// phase (`cli::discover_and_classify_with_progress`) runs — the phase
/// v0.2.0 founder-dogfood measured at up to 3m40s on a contended host with
/// no signal a spawning UI (HORO-1060) could use to distinguish "still
/// working" from "hung".
///
/// Never emitted unless `--progress-json` is passed; stdout's report DTOs
/// above are completely unaffected either way. `#[serde(tag = "phase")]`
/// is deliberately used here (unlike every other enum in this module,
/// which is projected to a plain `&'static str` tag by a free function)
/// because this is the one DTO a consumer parses as structured NDJSON
/// rather than reads as a report field, so serde's own internally-tagged
/// shape is the simplest way to guarantee the exact
/// `{"phase":"detector_started","detector":"..."}` shape HORO-1055 and the
/// Swift UI tickets depend on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// One detector (`detectors::DetectorRegistry::discover_all`'s
    /// per-detector granularity — the natural boundary already in that
    /// code) is about to run its bounded probe.
    DetectorStarted { detector: &'static str },
    /// The same detector's probe has returned. `candidates_found` is `0`
    /// for `DetectorStatus::ToolAbsent`/`Failed` as well as a genuine
    /// empty `Found(vec![])`, so `outcome` says which of the three
    /// happened and `reason` carries a failure's own message.
    ///
    /// `outcome`/`reason` were added by HORO-1484. Until then this event
    /// was the count alone, and its doc comment sent a consumer that
    /// needed detector health to "the final report, not this stream" —
    /// but `DetectReport` carried only `candidates`, so there was nowhere
    /// to go: a probe that errored streamed, and reported, exactly what a
    /// probe that looked and found nothing did.
    ///
    /// Both values come from [`crate::cli::DetectorOutcome`], which is the
    /// one producer of the three tags this stream and `detect --json`
    /// share.
    DetectorFinished {
        detector: &'static str,
        candidates_found: usize,
        /// `"found"`, `"tool_absent"` or `"failed"`.
        outcome: &'static str,
        /// The probe's failure message. Present only for
        /// `outcome: "failed"`, and omitted from the JSON otherwise
        /// rather than serialized as `null`.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// One action a caller (HORO-1053: the future SwiftUI menu-bar app) may
/// offer to the user for a resource — never a raw policy label. This is
/// the mechanism that keeps a second policy implementation out of that
/// UI: it must never branch on `policy_label` itself to decide whether a
/// "Clean" button is enabled or needs a confirmation step, it reads these
/// already-computed fields instead.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OfferedAction {
    pub action_id: String,
    /// `true` for `ASK` and `UNKNOWN_INCOMPLETE` candidates (ambiguous or
    /// incomplete evidence — still worth surfacing as available, but must
    /// be confirmed before running), `false` for `AUTO_SAFE`.
    pub requires_confirmation: bool,
}

/// Computes the `executable`/`offered_actions`/`refusal_reason` triple
/// (HORO-1053) shared by [`DetectCandidateReport`] and [`ExplainReport`],
/// from an already-computed [`PolicyDecision`] and the [`Action`] (if
/// any) [`crate::cli::resolve_action_for`] resolved for this resource.
/// Never reimplements policy logic — `decision.class` and
/// [`label_for`]'s `UNKNOWN_INCOMPLETE` derivation are the only policy
/// inputs consulted.
///
/// # Why this plans the action (HORO-1358)
///
/// A registered action *existing* for a resource kind is not the same claim
/// as that action being *runnable*. `homebrew.cleanup.cache` is registered
/// for `HomebrewCache`, but `brew cleanup -s` takes no path to scope, so its
/// step carries `scoped_path: None` and
/// [`crate::executor::structural_refusal`] rejects it on sight — every time,
/// regardless of policy class. Reporting only the first two facts produced a
/// candidate labelled `AUTO_SAFE` with `executable: true` whose Clean button
/// was guaranteed to fail with a refusal the user could not have predicted.
///
/// So the action is asked for its plan and that plan is put to the
/// executor's own structural rule. That keeps the two answers in agreement
/// *by construction* rather than by comment: there is one definition of
/// "execution would refuse this outright", and both the offer and the
/// execution read it.
///
/// HORO-1360 moved that sequencing — Protected first, then a missing action,
/// then plan-and-check — into [`crate::actionability::static_refusal`], so
/// that the LLM prompt view, Autopilot, `clean --dry-run`, `free` and
/// `emergency` could read the same answer instead of restating it. This
/// function keeps the part that is genuinely reporting's own: turning that
/// verdict into the `executable`/`offered_actions`/`refusal_reason` triple,
/// and deciding from the policy label whether an offer needs confirming.
///
/// Clearing that check means "not already refused", **not** "guaranteed to
/// succeed" — `structural_refusal` is deliberately blind to runtime state,
/// and revalidation at execution time may still abort. That is the honest
/// direction for this field to err in: it can no longer promise something
/// impossible, and it never promises something merely uncertain.
fn executable_fields(
    ev: &Evidence,
    decision: &PolicyDecision,
    resolved_action: Option<&dyn Action>,
) -> (bool, Vec<OfferedAction>, Option<String>) {
    // Protected-before-planning lives inside `static_refusal`, which is what
    // `build_llm_plan_report` relies on so that no LLM response can cause a
    // protected resource's action to be planned. See that function's docs.
    if let Some(reason) = crate::actionability::static_refusal(ev, decision, resolved_action) {
        return (false, Vec::new(), Some(reason));
    }

    // `static_refusal` returned `None`, which it only does with an action in
    // hand — a missing one is one of the refusals above.
    let Some(action) = resolved_action else {
        debug_assert!(false, "static_refusal cleared a resource with no action");
        return (
            false,
            Vec::new(),
            Some("no registered cleanup action for this resource kind".to_string()),
        );
    };

    // Exhaustive rather than `matches!` (HORO-1468): the default arm of a
    // `matches!` is `false`, i.e. "offer this without asking", so a label this
    // expression had not been taught about would fail open. `label_for` cannot
    // return `NotPolicyGoverned` — `reporting::policy_label`'s
    // `label_for_never_returns_not_policy_governed` asserts that over every
    // class and reason shape — and a label meaning "policy never judged this"
    // must still require confirmation if it ever arrives here, for the same
    // reason `autopilot::gate::admit` refuses it outright.
    let requires_confirmation = match label_for(decision) {
        PolicyLabel::Ask | PolicyLabel::UnknownIncomplete | PolicyLabel::NotPolicyGoverned => true,
        PolicyLabel::AutoSafe => false,
        // Unreachable: `static_refusal` above returns early for a protected
        // resource, which is why this is not a plain `_ => true`.
        PolicyLabel::Protected => true,
    };

    (
        true,
        vec![OfferedAction {
            action_id: action.id().0.to_string(),
            requires_confirmation,
        }],
        None,
    )
}

/// Human-readable active-use signal descriptions for `explain` output.
/// Empty when nothing observed anything active — never inferred from
/// `Unavailable` (an unattempted/failed probe is silence, not "no active
/// use").
pub fn active_use_signals(ev: &Evidence) -> Vec<String> {
    let mut signals = Vec::new();

    if let Some(procs) = ev.open_by_process.observed() {
        if !procs.is_empty() {
            let names: Vec<String> = procs
                .iter()
                .map(|p| format!("{}(pid {})", p.command, p.pid))
                .collect();
            signals.push(format!("open by process: {}", names.join(", ")));
        }
    }
    if let Some(procs) = ev.process_cwd_match.observed() {
        if !procs.is_empty() {
            let names: Vec<String> = procs
                .iter()
                .map(|p| format!("{}(pid {})", p.command, p.pid))
                .collect();
            signals.push(format!("process cwd match: {}", names.join(", ")));
        }
    }
    if let Some(Some(git)) = ev.git_state.observed() {
        if git.dirty {
            signals.push("git: working tree dirty".to_string());
        }
        if git.worktree {
            signals.push("git: is a linked worktree".to_string());
        }
    }
    if let Some(true) = ev.tool_liveness.observed() {
        signals.push("owning tool is currently running".to_string());
    }

    signals
}

/// `glomeris status` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: f64,
    pub free_human: String,
    pub total_human: String,
    pub pressure_state: &'static str,
}

/// `glomeris daemon status` report (HORO-1045).
///
/// `loaded` (launchd-reported: the job is registered/loaded) and
/// `heartbeat_age_secs` (derived from the poll loop's own last-write) are
/// deliberately kept as two separate fields and never collapsed into a
/// single computed `healthy`/`ok` boolean — a loaded-but-wedged daemon and
/// an actually-polling one must stay distinguishable to any caller (a
/// future Swift UI decides what "healthy" means, not this report).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DaemonStatusReport {
    pub plist_installed: bool,
    pub plist_path: String,
    /// `true` if `launchctl` currently reports the job as loaded. This is
    /// NOT evidence the poll loop is alive — see `heartbeat_age_secs`.
    pub loaded: bool,
    /// Seconds since the poll loop's last recorded heartbeat, or `None`
    /// when no heartbeat file exists yet (e.g. the daemon has never run).
    pub heartbeat_age_secs: Option<u64>,
}

/// One entry of a `glomeris history --json` report (HORO-1046) — a
/// projection of [`crate::monitor::HistoryEntry`], hand-picked rather than
/// derived on the domain type directly, same reasoning as this module's doc
/// comment. `from`/`to` are the raw stable string tags already written to
/// `history.tsv` (`PressureState::as_str()`'s output), not re-parsed back
/// into `PressureState`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryEventReport {
    pub unix_time_secs: u64,
    pub from: String,
    pub to: String,
    pub used_percent: f64,
    pub free_bytes: u64,
    pub free_human: String,
}

/// `glomeris history --json` report (HORO-1046): a bounded, oldest-first
/// tail of `history.tsv`. `events.len()` is never more than the `--limit`
/// the caller requested — see
/// [`crate::monitor::read_history_tail`]'s doc comment for the bounding and
/// malformed-line-skipping contract this projects.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryReport {
    pub events: Vec<HistoryEventReport>,
}

/// One entry of a `glomeris actions history --json` report (HORO-1057) —
/// a direct projection of [`crate::monitor::AuditRecord`], hand-picked
/// rather than derived on that type directly, same reasoning as this
/// module's doc comment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionHistoryEventReport {
    pub timestamp: u64,
    pub action_id: String,
    pub resource_id: String,
    pub policy_label: String,
    pub outcome: String,
    pub abort_reason: Option<String>,
    pub actual_reclaimed_bytes: Option<u64>,
    pub actual_reclaimed_human: Option<String>,
    pub source: String,
    /// `Some(rank)` when a model's plan named this resource, 1-based — see
    /// [`crate::monitor::AuditRecord::model_rank`]. Projected so `glomeris
    /// actions history --json` can answer "what did Autopilot do because a
    /// model suggested it?" without a second file or a second command.
    pub model_rank: Option<u32>,
}

/// `glomeris actions history --json` report (HORO-1057): a bounded,
/// oldest-first tail of `actions.jsonl`. `events.len()` is never more
/// than the `--limit` the caller requested — see
/// [`crate::monitor::read_audit_tail`]'s doc comment for the bounding and
/// malformed-line-skipping contract this projects.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionHistoryReport {
    pub events: Vec<ActionHistoryEventReport>,
}

/// One candidate line of a `glomeris detect` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectCandidateReport {
    pub resource_id: String,
    pub kind: &'static str,
    pub reclaimable_bytes: Option<u64>,
    pub reclaimable_human: Option<String>,
    /// `true` when `reclaimable_bytes` is a truthful lower bound rather
    /// than a settled measurement — see
    /// [`Evidence::reclaimable_bytes_is_lower_bound`]'s doc comment.
    /// Always `false` when `reclaimable_bytes` is `None`.
    pub reclaimable_bytes_is_lower_bound: bool,
    /// How much this candidate's size is worth the user's attention:
    /// `"unknown"`, `"normal"`, `"notable"` or `"large"` (HORO-1307). A
    /// product judgment about magnitude, computed here so the CLI and the
    /// menu-bar app cannot disagree about which findings matter — see
    /// [`crate::reporting::impact`] for the threshold model.
    ///
    /// Strictly independent of `policy_label`. A `"large"` candidate may be
    /// `AUTO_SAFE` (the best thing a user can be shown) and a `"normal"` one
    /// may be `PROTECTED`. Nothing may branch on this field to decide
    /// whether an action is allowed.
    pub impact_tier: &'static str,
    pub policy_label: &'static str,
    pub reasons: Vec<&'static str>,
    /// `true` only when all four hold: a real registered action resolves
    /// for this resource, its policy class doesn't unconditionally forbid
    /// it (i.e. not `PROTECTED`), that action can actually *plan* for this
    /// resource, and the resulting plan is not one
    /// [`crate::executor::structural_refusal`] rejects on sight. See
    /// [`executable_fields`]. HORO-1053, HORO-1358.
    pub executable: bool,
    /// One entry when `executable`, empty otherwise — the same four
    /// conditions. Do NOT read this as "one entry unless PROTECTED or no
    /// action resolves": an action can be registered, resolved and
    /// `AUTO_SAFE` and still be offered nothing, which is exactly what
    /// HORO-1358 fixed. HORO-1053, HORO-1358.
    pub offered_actions: Vec<OfferedAction>,
    /// Human-readable reason set exactly when `executable` is `false`.
    /// HORO-1053.
    pub refusal_reason: Option<String>,
}

impl DetectCandidateReport {
    /// `impact` carries what is known about the filesystem's free space, for
    /// the relative half of the impact-tier model. Pass
    /// [`ImpactContext::default`] where that is genuinely unavailable — the
    /// absolute thresholds alone still produce an honest tier, and guessing
    /// a capacity would produce a confidently wrong one.
    pub fn from_evidence_and_decision(
        ev: &Evidence,
        decision: &PolicyDecision,
        resolved_action: Option<&dyn Action>,
        impact: ImpactContext,
    ) -> Self {
        let reclaimable = ev.reclaimable_bytes.observed().copied();
        let (executable, offered_actions, refusal_reason) =
            executable_fields(ev, decision, resolved_action);
        Self {
            resource_id: ev.resource.to_string(),
            kind: ev.resource.kind_tag(),
            reclaimable_bytes: reclaimable,
            reclaimable_human: reclaimable.map(human_bytes),
            reclaimable_bytes_is_lower_bound: reclaimable.is_some()
                && ev.reclaimable_bytes_is_lower_bound,
            impact_tier: classify_impact(reclaimable, impact, &ImpactThresholds::default())
                .as_str(),
            policy_label: label_for(decision).as_str(),
            reasons: decision.reasons.iter().map(|r| r.as_str()).collect(),
            executable,
            offered_actions,
            refusal_reason,
        }
    }
}

/// One detector's health in a [`DetectReport`] (HORO-1484): what its probe
/// *did*, which is a different question from what it found.
///
/// `src/detectors/mod.rs` states the rule this exists to make keepable —
/// "`Failed` must never be silently converted to an empty/safe result by an
/// upstream caller: a failed probe is not evidence of 'nothing to clean
/// up', it is evidence of 'we don't know'". A JSON consumer had no way to
/// keep it: `detect --json` emitted `candidates` and nothing else, so the
/// menu-bar app rendered a candidate list assembled from a partly-failed
/// discovery as a complete picture.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectorHealthReport {
    /// The detector's registered id, e.g. `cargo_target_dir`.
    pub detector: String,
    /// `"found"`, `"tool_absent"` or `"failed"` — produced by
    /// [`crate::cli::DetectorOutcome::tag`], the same one producer the
    /// `--progress-json` stream uses.
    pub status: &'static str,
    /// Evidences this detector contributed to `candidates`. `0` for
    /// `tool_absent` and for `failed` alike, which is precisely why
    /// `status` is a separate field rather than something a consumer could
    /// infer from this number.
    pub candidates_found: usize,
    /// The probe's own failure message, for `"failed"` only. Omitted from
    /// the JSON rather than serialized as `null` when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One worktree inside a [`WorkspaceFamilyReport`] (HORO-1511).
///
/// Every field is a statement about this worktree alone. None of them is a
/// permission, and there is deliberately no `executable` field, no action id
/// and no offered action: a client that wants to know what may run joins
/// `member_resource_ids` against [`DetectReport::candidates`], where the
/// action and the refusal reason live.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceWorktreeReport {
    pub root: String,
    /// `true` when this is a `git worktree add` sibling rather than the
    /// repository's main checkout.
    pub linked_worktree: bool,
    pub dirty: bool,
    pub untracked: bool,
    /// `"in_use"`, `"idle"` or `"unknown"`. `"unknown"` is not `"idle"`: a
    /// correlation probe that could not answer leaves it here, and
    /// `holds_work_in_progress` counts it as possible use.
    pub activity: &'static str,
    /// `"untracked"`, `"tracking"` or `"unknown"`. `"untracked"` means there
    /// is no published counterpart to compare against — not that nothing is
    /// unpushed.
    pub upstream: &'static str,
    /// Commits this branch has that its upstream does not, and vice versa.
    /// Both `None` unless `upstream` is `"tracking"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    /// The checked-out branch, or omitted for a detached HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `"merged"`, `"not_merged"` or `"unknown"`, always alongside
    /// `merged_into` for the first two. `"unknown"` is what a repository
    /// with no recorded default branch gets — never a guess at `main`.
    pub merged: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<String>,
    /// Whether this worktree holds something that should stop it being
    /// treated as spent: uncommitted work, untracked files, something using
    /// it, or commits no remote has. A *sentence*, not a gate — the executor
    /// never reads it, and a worktree where this is `false` still goes
    /// through policy classification and deletion-time revalidation
    /// unchanged.
    pub holds_work_in_progress: bool,
    /// The resource ids of this worktree's discovered candidates, in
    /// [`DetectReport::candidates`]' own order. Ids only: the aggregate
    /// explains, the candidate list authorizes.
    pub member_resource_ids: Vec<String>,
}

/// Every worktree sharing one git directory — one repository's checkouts —
/// and what they add up to (HORO-1511).
///
/// This exists so a person can be told "this project accounts for 30 GiB
/// across nine worktrees" instead of reading thirty unrelated-looking lines.
/// That figure is an attention figure and nothing else: it is summed from
/// detector **estimates**, and campaign section 9 reserves progress and
/// completion for re-measured filesystem state.
///
/// The byte split follows [`RecoveryOpportunityReport`]'s: confirmation-gated
/// space is separated from automatic space, and protected space is *counted,
/// not summed*, because presenting bytes a run can never take as part of a
/// project's reclaimable bulk is the exact misread the split prevents.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceFamilyReport {
    /// The shared git directory that identifies this family.
    pub common_dir: String,
    pub worktree_count: usize,
    /// Members labelled `AUTO_SAFE`: estimated space a recovery run could
    /// take from this family without asking.
    pub actionable_now_bytes: u64,
    pub actionable_now_human: String,
    pub actionable_now_count: usize,
    /// Members labelled `ASK`. Real bulk, but not automatic.
    pub requires_confirmation_bytes: u64,
    pub requires_confirmation_human: String,
    pub requires_confirmation_count: usize,
    /// Counted, not summed. See this type's doc comment.
    pub protected_count: usize,
    /// Members whose label is `UNKNOWN_INCOMPLETE` — evidence too thin to
    /// classify, which policy treats as protected.
    pub unknown_count: usize,
    /// Members no probe measured. Distinct from a zero-byte member, and the
    /// reason the totals above are not the whole story.
    pub unmeasured_count: usize,
    /// `true` when any member contributing to a byte total reported its
    /// estimate as a lower bound, so the real figure may be larger.
    pub is_lower_bound: bool,
    /// How many worktrees in this family hold work in progress. `> 0` is
    /// why a family's total must never read as "delete this project": the
    /// bulk and the outstanding work are in the same group.
    pub worktrees_holding_work_in_progress: usize,
    /// Sorted by root path, so two runs over one disk state agree.
    pub worktrees: Vec<WorkspaceWorktreeReport>,
}

impl WorkspaceFamilyReport {
    /// Projects one [`WorkspaceFamily`].
    ///
    /// Byte figures are summed from the members' own
    /// [`WorkspaceMember::reclaimable_bytes`], which is the same
    /// `Option<u64>` [`DetectCandidateReport::reclaimable_bytes`] carries —
    /// so the family total and the candidate lines a client shows beside it
    /// cannot disagree.
    pub fn from_family(family: &WorkspaceFamily) -> Self {
        let mut actionable_now_bytes = 0u64;
        let mut actionable_now_count = 0usize;
        let mut requires_confirmation_bytes = 0u64;
        let mut requires_confirmation_count = 0usize;
        let mut protected_count = 0usize;
        let mut unknown_count = 0usize;
        let mut unmeasured_count = 0usize;
        let mut is_lower_bound = false;

        for member in family.members() {
            if member.reclaimable_bytes.is_none() {
                unmeasured_count += 1;
            }
            let bytes = member.reclaimable_bytes.unwrap_or(0);
            match member.label {
                PolicyLabel::AutoSafe => {
                    actionable_now_count += 1;
                    actionable_now_bytes = actionable_now_bytes.saturating_add(bytes);
                    is_lower_bound |= member.reclaimable_bytes_is_lower_bound;
                }
                PolicyLabel::Ask => {
                    requires_confirmation_count += 1;
                    requires_confirmation_bytes = requires_confirmation_bytes.saturating_add(bytes);
                    is_lower_bound |= member.reclaimable_bytes_is_lower_bound;
                }
                PolicyLabel::Protected => protected_count += 1,
                PolicyLabel::UnknownIncomplete => unknown_count += 1,
                // Unreachable for a worktree member: `label_for` projects a
                // decision and every decision has a class, so this variant
                // only ever labels an audit entry for the tool's own
                // disposable state. Counted with the protected members
                // rather than silently dropped, because a member that
                // reached here contributed bulk that no arm above claimed
                // and a family whose counts do not add up to its member
                // list is the failure this whole split guards against.
                PolicyLabel::NotPolicyGoverned => protected_count += 1,
            }
        }

        Self {
            common_dir: family.common_dir.display().to_string(),
            worktree_count: family.worktree_count(),
            actionable_now_bytes,
            actionable_now_human: human_bytes(actionable_now_bytes),
            actionable_now_count,
            requires_confirmation_bytes,
            requires_confirmation_human: human_bytes(requires_confirmation_bytes),
            requires_confirmation_count,
            protected_count,
            unknown_count,
            unmeasured_count,
            is_lower_bound,
            worktrees_holding_work_in_progress: family
                .worktrees
                .iter()
                .filter(|w| w.holds_work_in_progress())
                .count(),
            worktrees: family
                .worktrees
                .iter()
                .map(WorkspaceWorktreeReport::from_worktree)
                .collect(),
        }
    }
}

impl WorkspaceWorktreeReport {
    fn from_worktree(worktree: &WorkspaceWorktree) -> Self {
        // A branch state that could not be read reports the same
        // `"unknown"` tags a probe-level failure would, and `branch`/
        // `merged_into` stay absent. There is no tidier default: claiming a
        // detached HEAD with no upstream and no merge answer would be three
        // assertions from one failure.
        let branch = match &worktree.branch {
            ProbeOutcome::Observed(state) => Some(state),
            ProbeOutcome::Unavailable(_) => None,
        };
        let (ahead, behind) = match branch.map(|s| &s.upstream) {
            Some(UpstreamState::Tracking { ahead, behind }) => (Some(*ahead), Some(*behind)),
            _ => (None, None),
        };
        Self {
            root: worktree.root.display().to_string(),
            linked_worktree: worktree.linked,
            dirty: worktree.dirty,
            untracked: worktree.untracked,
            activity: worktree.activity.tag(),
            upstream: branch.map_or("unknown", |s| s.upstream.tag()),
            ahead,
            behind,
            branch: branch.and_then(|s| s.branch.clone()),
            merged: branch.map_or("unknown", |s| s.merged.tag()),
            merged_into: branch
                .and_then(|s| s.merged.compared_against())
                .map(str::to_string),
            holds_work_in_progress: worktree.holds_work_in_progress(),
            member_resource_ids: worktree
                .members
                .iter()
                .map(|m| m.resource_id.clone())
                .collect(),
        }
    }
}

/// `glomeris detect` report: one candidate line per discovered resource,
/// plus the health of every detector that ran.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct DetectReport {
    pub candidates: Vec<DetectCandidateReport>,
    /// Every detector the registry ran, in registration order — including
    /// the ones that contributed no candidates (HORO-1484).
    pub detectors: Vec<DetectorHealthReport>,
    /// Whether `candidates` is the whole picture: `false` as soon as any
    /// detector's `status` is `"failed"`.
    ///
    /// Derived, not independently tracked — see
    /// [`crate::cli::build_detect_report`]. A consumer that only wants to
    /// know "can I present this list as complete?" reads this; one that
    /// wants to say which probe failed reads `detectors`.
    ///
    /// `Default` gives `false`, which is the safe direction for a report
    /// nobody has filled in: an empty candidate list that has not been
    /// asserted complete should not read as "nothing to clean up".
    pub discovery_complete: bool,
    /// Discovered resources grouped by the git worktree family they belong
    /// to (HORO-1511) — explanatory metadata for "where did my disk go",
    /// never an authorization. Empty for a caller that did not group, which
    /// is why it is a separate field rather than something a client could
    /// read as "this machine has no worktree families".
    ///
    /// A resource outside any git working tree, or one whose git probe
    /// failed, appears in `candidates` and in no family. The two lists are
    /// joined on resource id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<WorkspaceFamilyReport>,
}

/// `glomeris explain <resource>` report: the full evidence-and-policy
/// picture for exactly one resource.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExplainReport {
    pub resource_id: String,
    pub kind: &'static str,
    /// Which detector discovered this resource — part of the "evidence/
    /// provenance" this ticket's AC requires `explain` to show.
    pub detector: &'static str,
    /// Bounded provenance notes (capped at 8 by
    /// `Evidence::push_source`) — the rest of the "provenance" half of
    /// that same AC.
    pub sources: Vec<String>,
    /// Logical size as reported by the filesystem — explicitly distinct
    /// from `reclaimable_bytes` (see field doc there and the ticket's AC:
    /// "size (logical vs reclaimable, explicitly labeled as different)").
    pub logical_bytes: Option<u64>,
    pub logical_human: Option<String>,
    /// Estimated bytes an action would reclaim — NOT a measured result;
    /// see `crate::executor::ExecutionReport::actual_reclaimed_bytes` for
    /// the measured counterpart, which only exists after real execution.
    pub reclaimable_bytes: Option<u64>,
    pub reclaimable_human: Option<String>,
    /// `true` when `logical_bytes`/`reclaimable_bytes` is a truthful lower
    /// bound rather than a settled measurement — see
    /// [`Evidence::reclaimable_bytes_is_lower_bound`]'s doc comment.
    /// Always `false` when `reclaimable_bytes` is `None`.
    pub reclaimable_bytes_is_lower_bound: bool,
    pub completeness: &'static str,
    pub confidence: &'static str,
    pub active_use_signals: Vec<String>,
    pub regenerability: &'static str,
    pub policy_label: &'static str,
    pub reasons: Vec<&'static str>,
    pub native_cleanup_available: bool,
    pub native_cleanup_action_id: Option<&'static str>,
    /// Opaque, wire-safe encoding of `ev.fingerprint` (HORO-1051) — lets a
    /// future interactive `execute` subcommand (HORO-1055) carry the exact
    /// fingerprint the UI observed here back to a later process invocation,
    /// so `policy::approval::authorize` can pin consent to this exact
    /// resource instance. `None` when the resource carries no fingerprint
    /// to report at all (e.g. a `ResourceLocator::Tool` resource such as
    /// Docker's build cache, which has no dev/inode/mtime identity).
    pub fingerprint_token: Option<String>,
    /// `true` only when all four hold: a real registered action resolves
    /// for this resource, its policy class doesn't unconditionally forbid
    /// it (i.e. not `PROTECTED`), that action can actually *plan* for this
    /// resource, and the resulting plan is not one
    /// [`crate::executor::structural_refusal`] rejects on sight. See
    /// [`executable_fields`]. HORO-1053, HORO-1358.
    pub executable: bool,
    /// One entry when `executable`, empty otherwise — the same four
    /// conditions. Do NOT read this as "one entry unless PROTECTED or no
    /// action resolves": an action can be registered, resolved and
    /// `AUTO_SAFE` and still be offered nothing, which is exactly what
    /// HORO-1358 fixed. HORO-1053, HORO-1358.
    pub offered_actions: Vec<OfferedAction>,
    /// Human-readable reason set exactly when `executable` is `false`.
    /// HORO-1053.
    pub refusal_reason: Option<String>,
}

impl ExplainReport {
    pub fn from_evidence_and_decision(
        ev: &Evidence,
        decision: &PolicyDecision,
        resolved_action: Option<&dyn Action>,
    ) -> Self {
        let logical = ev.logical_bytes.observed().copied();
        let reclaimable = ev.reclaimable_bytes.observed().copied();
        let (native_cleanup_available, native_cleanup_action_id) = match ev.native_cleanup {
            NativeCleanup::Available(id) => (true, Some(id.0)),
            NativeCleanup::Unsupported => (false, None),
        };
        let fingerprint_token = if ev.fingerprint.dev_ino.is_some()
            || ev.fingerprint.mtime.is_some()
            || ev.fingerprint.tool_revision.is_some()
        {
            Some(crate::evidence::encode_fingerprint_token(&ev.fingerprint))
        } else {
            None
        };
        let (executable, offered_actions, refusal_reason) =
            executable_fields(ev, decision, resolved_action);

        Self {
            resource_id: ev.resource.to_string(),
            kind: ev.resource.kind_tag(),
            detector: ev.detector.0,
            sources: ev.sources.clone(),
            logical_bytes: logical,
            logical_human: logical.map(human_bytes),
            reclaimable_bytes: reclaimable,
            reclaimable_human: reclaimable.map(human_bytes),
            reclaimable_bytes_is_lower_bound: reclaimable.is_some()
                && ev.reclaimable_bytes_is_lower_bound,
            completeness: completeness_tag(&ev.completeness()),
            confidence: confidence_tag(ev.confidence()),
            active_use_signals: active_use_signals(ev),
            regenerability: regenerability_tag(ev.regenerability),
            policy_label: label_for(decision).as_str(),
            reasons: decision.reasons.iter().map(|r| r.as_str()).collect(),
            native_cleanup_available,
            native_cleanup_action_id,
            fingerprint_token,
            executable,
            offered_actions,
            refusal_reason,
        }
    }
}

/// One item of a `glomeris clean --dry-run` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CleanDryRunItem {
    pub resource_id: String,
    pub policy_label: &'static str,
    pub action_id: Option<&'static str>,
    /// Rendered from the typed `ActionPlan.explain` — never composed as
    /// freeform text independently of it.
    pub explain: Option<String>,
    pub skip_reason: Option<String>,
}

/// `glomeris clean --dry-run` report.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct CleanDryRunReport {
    pub items: Vec<CleanDryRunItem>,
}

/// One item of a `glomeris llm-plan` report (HORO-1008).
///
/// `Serialize` only, never `Deserialize` — alongside
/// `crate::actions::llm::LlmPlan`/`LlmPlanItem`, no type in this module
/// ever derives `Deserialize`. Those two remain the crate's only two
/// `#[serde(deny_unknown_fields)]` `Deserialize` types; this DTO must never
/// change that.
///
/// ## Which fields are the model's, and which are the machine's
///
/// HORO-1308 gave this DTO a GUI consumer, which makes the distinction
/// load-bearing rather than editorial. Exactly two fields carry anything
/// the provider chose:
///
/// - `priority` — the model's claimed ordering hint.
/// - `model_reason` — the model's own words, already bounded and stripped
///   by `crate::actions::llm::sanitize_model_reason`.
///
/// Everything else is this machine's own finding, computed from local
/// evidence and the real policy engine, and would read identically if no
/// provider had ever been contacted: `resource_id`, `policy_label`,
/// `completeness`, `confidence`, `explain`, `skip_reason`, and every field
/// of the nested `candidate`. In particular `candidate.executable`,
/// `candidate.offered_actions` and `candidate.refusal_reason` — the three
/// fields any UI is required to read to decide what may be done — are
/// produced by [`executable_fields`] from the [`PolicyDecision`], and there
/// is no input path from the model's bytes to any of them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmPlanItemReport {
    pub resource_id: String,
    pub policy_label: &'static str,
    pub requested_action_id: Option<&'static str>,
    pub priority: Option<u32>,
    /// The model's own rationale for suggesting this item (HORO-1308),
    /// bounded and control-character-stripped upstream by
    /// `crate::actions::llm::sanitize_model_reason`.
    ///
    /// `None` when the model gave none, or gave only whitespace. Display
    /// only, and a surface showing it MUST attribute it to the model rather
    /// than presenting it as Glomeris's own finding — it is a claim, not
    /// evidence, and it sits beside `reasons`/`explain`, which are evidence.
    pub model_reason: Option<String>,
    /// Rendered from the typed `ActionPlan.explain` — `None` for a
    /// `PROTECTED` item (never resolved) or a resolution/dry-run failure
    /// (see `skip_reason` in that case).
    pub explain: Option<String>,
    pub skip_reason: Option<String>,
    /// The same evidence-and-policy projection `glomeris detect` prints for
    /// this resource (HORO-1308), nested verbatim rather than re-derived.
    ///
    /// Nested — not flattened, and not a second hand-rolled set of fields —
    /// for one specific reason: it lets a UI render an AI-suggested row with
    /// the exact same code that renders a plain `detect` row, so there is no
    /// second enablement path for a model recommendation to travel down. A
    /// flattened copy would be a place for the two to drift.
    ///
    /// Deliberately NOT [`ExplainReport`], which carries
    /// `fingerprint_token`. A plan report must not hand out the token that
    /// pins consent: cleaning something suggested here still goes through
    /// its own `explain` call first, exactly as cleaning something from the
    /// candidates list does.
    pub candidate: DetectCandidateReport,
    /// Evidence completeness for this resource (HORO-1308) — `"complete"`,
    /// `"partial"` or `"failed"`, straight from [`completeness_tag`], the
    /// same mapping `explain` reports go through.
    ///
    /// Present here and not in [`DetectCandidateReport`] because this is the
    /// surface where it matters most: a ranked *recommendation* invites
    /// action, so how good the underlying evidence is belongs next to it.
    pub completeness: &'static str,
    /// Evidence confidence for this resource (HORO-1308) — see
    /// `completeness` above for why both are here.
    pub confidence: &'static str,
}

/// `glomeris llm-plan` report (HORO-1008): advisory ranking suggestion
/// only — see `crate::cli::build_llm_plan_report`'s doc comment for why
/// this command never executes anything.
///
/// `Serialize` only, never `Deserialize` — see [`LlmPlanItemReport`]'s doc
/// comment.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct LlmPlanReport {
    pub items: Vec<LlmPlanItemReport>,
    pub dropped_unknown_resource: u32,
    pub dropped_unknown_action: u32,
    pub provider_error: Option<String>,
}

/// One wire-id -> real-resource mapping of a `glomeris llm-plan
/// --print-payload` report (HORO-1298).
///
/// `local_resource_id` is the real `ResourceId` string and therefore, for
/// path-backed resource kinds, an absolute path. That is correct here and
/// only here: this DTO exists to show the operator what the opaque ids in
/// `user_prompt` stand for, on their own machine. It is deliberately NOT
/// part of `LlmPayloadReport`'s prompt fields, which are the bytes that
/// actually leave.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmPayloadResourceAlias {
    pub wire_resource_id: String,
    pub local_resource_id: String,
}

/// `glomeris llm-plan --print-payload` report (HORO-1298): the exact
/// request a live `llm-plan` run would send, shown without sending it, so
/// "no absolute paths leave this machine" is checkable by the person whose
/// machine it is rather than taken on trust.
///
/// `Serialize` only, never `Deserialize` — see [`LlmPlanItemReport`]'s doc
/// comment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmPayloadReport {
    pub system_prompt: String,
    pub user_prompt: String,
    pub resource_aliases: Vec<LlmPayloadResourceAlias>,
}

/// `glomeris llm-check` report (HORO-1309): the outcome of one bounded,
/// trivial round trip to the configured provider, so "is my BYOK setup
/// actually working" is answerable without spending a real planning request
/// and without a user having to interpret a raw HTTP failure.
///
/// ## What is deliberately not in here
///
/// The API key, obviously — but also the base URL. Only its path survives,
/// as `endpoint_path`, for the same reasons
/// [`crate::actions::llm::LlmError::ProviderStatus`] keeps only the path: a
/// host may be private infrastructure, and some gateways accept a credential
/// in the query string, so a user who pasted one into their base URL must
/// not have it copied into a report that a UI may render and a log may
/// retain. The path alone is what diagnoses the one misconfiguration this
/// report exists to catch — host root configured instead of API root. The
/// full URL is never needed here because the surface asking the question
/// already has it: the user typed it in.
///
/// `Serialize` only, never `Deserialize` — see [`LlmPlanItemReport`]'s doc
/// comment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmCheckReport {
    /// One of `"ok"`, `"misconfigured"`, `"unreachable"`, `"rejected"`, or
    /// `"unusable_response"` — a stable token for a UI to branch on, so it
    /// never has to pattern-match on the prose in `error`.
    ///
    /// `"misconfigured"` means nothing was sent, because the settings
    /// themselves cannot work — the user has something to fix locally.
    /// `"rejected"` means the provider answered and refused: a wrong key, a
    /// wrong path, a model the account cannot use. `"unreachable"` means
    /// there was no answer at all. Keeping those apart is the whole of
    /// HORO-1299's lesson — collapsing them is what made a BYOK 401
    /// undiagnosable.
    pub outcome: &'static str,
    /// The configured model name, echoed back so a successful check says
    /// which model answered rather than only that something did. Not a
    /// secret, and chosen by the user.
    pub model: String,
    /// The request path a live call posts to — see this type's doc comment
    /// for why the host and query string are dropped.
    pub endpoint_path: String,
    /// One readable, secret-free sentence, present for every outcome except
    /// `"ok"`. Straight from
    /// [`crate::actions::llm::LlmError`]'s `Display`, whose key-free output
    /// is asserted per variant in that module's own tests.
    pub error: Option<String>,
    /// A bounded excerpt of what the provider actually replied, present only
    /// for `"ok"`. Bounded via [`crate::actions::llm::excerpt`] because this
    /// is provider-controlled text heading for a fixed-width popover: a
    /// gateway that answers a two-word connection test with an essay does
    /// not get to decide how much of the UI it occupies.
    pub response_excerpt: Option<String>,
}

/// `glomeris execute` report (HORO-1055): a thin projection of
/// [`crate::executor::ExecutionReport`] — never duplicates its outcome
/// logic, only renders the already-decided outcome. Built only for the
/// `Executed(_)` branch of `crate::cli::ExecuteResolution`; every refusal
/// or not-found branch is reported directly by `main.rs` and never
/// reaches this DTO at all.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecuteReport {
    pub action_id: &'static str,
    pub resource_id: String,
    /// One of `"succeeded"`, `"failed"`, `"aborted_by_revalidation"`, or
    /// `"dry_run"` — mirrors `crate::executor::ExecutionOutcome`'s
    /// variants without deriving `Serialize` on that type directly.
    pub outcome: &'static str,
    /// Populated only when `outcome == "failed"`.
    pub failure_message: Option<String>,
    /// Populated only when `outcome == "aborted_by_revalidation"` — the
    /// `Debug` rendering of `crate::executor::AbortReason`.
    pub abort_reason: Option<String>,
    pub expected_reclaimed_bytes: Option<u64>,
    pub actual_reclaimed_bytes: Option<u64>,
    /// HORO-1312. The same two numbers as
    /// [`crate::reporting::human_bytes`] renders them, for the same reason
    /// [`ActionHistoryEventReport::actual_reclaimed_human`] exists: a client
    /// that formats the byte count itself will disagree with this product's
    /// own text output. The menu-bar app did exactly that, with
    /// `ByteCountFormatter(countStyle: .file)` — 1000-based — so one panel
    /// showed a 1024-based estimate from Rust beside a 1000-based measured
    /// result from Swift, and the difference read as bytes that went
    /// missing. Whoever owns the convention has to emit the string.
    pub expected_reclaimed_human: Option<String>,
    pub actual_reclaimed_human: Option<String>,
}

/// Why `glomeris execute` refused, as the machine-readable token
/// [`ExecuteRefusalReport::reason`] carries.
///
/// HORO-1327. These eight strings used to be written as literals at their
/// call sites in `main.rs`, which left the `refusal` vocabulary with no
/// single producer for `scripts/check-vocabulary-covers-cli-tokens.sh` to
/// diff the menu-bar app's wording against — so the guard reported 10 of 12
/// and said so, rather than pretending to cover them.
///
/// That is not a theoretical gap. `AuditRecord::source` had the identical
/// shape until HORO-1312, and in between HORO-1310 added two new source
/// values at new call sites with nothing able to say whether the GUI had
/// learned words for them. `refusal` is the vocabulary where that silence
/// costs the most: an unrecognised token renders as "The CLI refused for a
/// reason this app has no wording for" at exactly the moment a user is
/// being told they may not delete something.
///
/// [`RefusalReason::as_str`] is now the sole producer, and it is load-bearing
/// rather than parallel to serialization: the `Serialize` impl below goes
/// through it, so serde cannot emit a token the guard has not seen. A
/// `#[serde(rename_all = "snake_case")]` derive would have produced the same
/// JSON while leaving the guard blind — the tokens would exist only as
/// variant names transformed at compile time.
///
/// The Swift side stays tolerant on read, for the reason `AuditRecord::source`
/// stayed a `String` (HORO-1312): a newer CLI's new reason code must degrade
/// to the app's default wording, never erase the refusal it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// No discoverable candidate matches the `--resource-id` given.
    ResourceNotFound,
    /// No registered action resolves for the resource's kind.
    ActionNotFound,
    /// `--action-id` named a registered action other than the one this
    /// resource resolves to.
    ActionMismatch,
    /// The resource is `PROTECTED`; no flag combination authorizes execution.
    Protected,
    /// The resource requires confirmation and none was supplied.
    AskNoConsent,
    /// A confirmation was supplied but does not match the resource's freshly
    /// observed identity.
    AskConsentMismatch,
    /// An `AUTO_SAFE` decision failed to authorize, contradicting
    /// `policy::approval::authorize`'s documented contract.
    AutoSafeContractViolation,
    /// The HORO-1054 execution lock is already held by another invocation.
    /// The one refusal that happens before a `crate::cli::ExecuteResolution`
    /// exists at all, which is why it has no counterpart variant there.
    Busy,
}

impl RefusalReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefusalReason::ResourceNotFound => "resource_not_found",
            RefusalReason::ActionNotFound => "action_not_found",
            RefusalReason::ActionMismatch => "action_mismatch",
            RefusalReason::Protected => "protected",
            RefusalReason::AskNoConsent => "ask_no_consent",
            RefusalReason::AskConsentMismatch => "ask_consent_mismatch",
            RefusalReason::AutoSafeContractViolation => "auto_safe_contract_violation",
            RefusalReason::Busy => "busy",
        }
    }

    /// Every reason, in the order `execute`'s own exit-code table documents
    /// them (the five-code not-found pair, then the refusals, then the
    /// pre-resolution lock). See the `all_lists_every_variant_exactly_once`
    /// test for the compile-time guard that keeps this exhaustive.
    pub const ALL: &'static [RefusalReason] = &[
        RefusalReason::ResourceNotFound,
        RefusalReason::ActionNotFound,
        RefusalReason::ActionMismatch,
        RefusalReason::Protected,
        RefusalReason::AskNoConsent,
        RefusalReason::AskConsentMismatch,
        RefusalReason::AutoSafeContractViolation,
        RefusalReason::Busy,
    ];
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Serialized as the bare token, so `--json` output is byte-identical to the
/// string literals this type replaced. Hand-written rather than derived
/// precisely so [`RefusalReason::as_str`] is the only place the tokens exist
/// — see this type's doc comment.
impl Serialize for RefusalReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Structured `--json` rendering for every non-`Executed` branch of
/// `crate::cli::ExecuteResolution` (HORO-1055 nit: `--json` callers — the
/// interactive UI this subcommand exists for — previously got empty
/// stdout plus a bare exit code on every refusal/not-found path, which is
/// exactly the case a UI most needs structured detail on). Never carries
/// anything an `Approval` would — this is a report of a refusal that
/// already happened, not a mechanism for causing one.
///
/// Also reused (HORO-1056) for the one `execute` refusal that happens
/// BEFORE an `ExecuteResolution` exists at all:
/// [`RefusalReason::Busy`], emitted by `main.rs`'s
/// `acquire_execution_lock_or_exit` when the HORO-1054 execution lock is
/// already held by another invocation. Same shape, same `--json` contract,
/// deliberately not a new DTO.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecuteRefusalReport {
    /// Typed rather than a `&'static str` (HORO-1327): the token can then
    /// only come from [`RefusalReason::as_str`], so a new refusal path
    /// cannot introduce a ninth token by writing a literal the menu-bar app
    /// has never heard of. The serialized JSON is unchanged.
    pub reason: RefusalReason,
    pub message: String,
}

/// One registered action from `ActionRegistry::builtin()`, as reported by
/// `glomeris actions list --json` (HORO-1047). `applies_to` is a direct
/// projection of [`crate::actions::Action::applies_to`] — never a
/// hand-maintained list — which is what makes registering a new action
/// require no change to this DTO or the command that builds it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionListItem {
    pub action_id: &'static str,
    pub applies_to: Vec<&'static str>,
}

/// `glomeris actions list --json`'s top-level report (HORO-1047): every
/// action currently registered in [`crate::actions::ActionRegistry::builtin`],
/// enumerated via [`crate::actions::ActionRegistry::actions`] — origin:
/// v0.2.0 founder-dogfood had to read `src/actions/homebrew.rs` source
/// directly to find a real registered action id, because no command
/// exposed the registry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionListReport {
    pub actions: Vec<ActionListItem>,
}

/// One standing `ASK` pre-authorization, as reported by
/// `glomeris autopilot show --json` (HORO-1310, GUI surface added under
/// HORO-1310's reopened scope).
///
/// Both fields are the canonical tokens — [`crate::evidence::ResourceKind::tag`]
/// and [`crate::policy::ReasonCode::as_str`] — rather than prose, because the
/// menu-bar app already has a plain-language table for both and
/// `scripts/check-vocabulary-covers-cli-tokens.sh` diffs that table against
/// those two producers. Sending prose instead would put the wording somewhere
/// nothing checks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AutopilotAskPreauthorizationReport {
    pub kind: &'static str,
    pub reason: &'static str,
}

/// The hard ceilings no Autopilot envelope can exceed, from
/// [`crate::autopilot::envelope`]'s constants.
///
/// Reported rather than hardcoded in the client for the reason the whole
/// envelope is enforced in Rust: a GUI that knew the ceilings independently
/// could offer a slider position the CLI then refuses, and the user would
/// read that as the app lying about its own limits.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AutopilotCeilingsReport {
    pub max_actions: u32,
    pub max_bytes: u64,
    pub max_bytes_human: String,
    pub max_duration_secs: u64,
}

/// `glomeris autopilot show|enable|revoke --json` report (HORO-1310): the
/// complete statement of what Autopilot is authorized to do, plus the
/// complete statement of what no envelope can ever authorize.
///
/// ## Why the refusals are in the same report
///
/// Because they are the half a reader cannot infer from a list of settings,
/// and a client that rendered only the settings would be describing the
/// grant as larger than it is. `allowed_kinds` says what was granted;
/// `never_allowlistable_kinds`, `never_preauthorizable_reasons` and
/// `never_executable_labels` say what the grant could not have included even
/// if someone had tried. Both halves come from the same Rust functions the
/// gate itself consults ([`crate::autopilot::envelope::is_preauthorizable`],
/// [`crate::autopilot::AutopilotEnvelope::allow_kind`]), so there is no
/// second copy to drift.
///
/// ## Why the choices are enumerated here too
///
/// `allowlistable_kinds`, `preauthorizable_reasons` and `pressure_states`
/// exist so a client can build its controls from this report instead of from
/// a transcribed list. A hardcoded Swift list of resource kinds would go
/// stale the first time a detector is added, and the failure would be
/// invisible: a kind the CLI accepts that the GUI cannot offer.
///
/// `Serialize` only, never `Deserialize` — see [`LlmPlanItemReport`]'s doc
/// comment. Nothing a client sends can construct an envelope; the only way
/// to change one is `autopilot enable`, whose own argument parsing runs the
/// envelope's checked setters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AutopilotEnvelopeReport {
    /// Whether Autopilot may run at all. False is not the only thing that
    /// stops it: an enabled envelope with an empty `allowed_kinds` or a
    /// `max_actions` of zero also executes nothing.
    pub enabled: bool,
    pub allowed_kinds: Vec<&'static str>,
    pub ask_preauthorizations: Vec<AutopilotAskPreauthorizationReport>,
    pub max_actions: u32,
    pub max_bytes: u64,
    /// The same number as [`crate::reporting::human_bytes`] renders it, for
    /// the reason [`ExecuteReport::actual_reclaimed_human`] carries one:
    /// this product's byte convention is 1024-based and a client that
    /// formats it independently will disagree with the CLI's own output.
    pub max_bytes_human: String,
    pub max_duration_secs: u64,
    /// The pressure state a run must have reached, or `null` when the run is
    /// not gated on disk pressure at all. A [`crate::monitor::PressureState`]
    /// token, upper case, as `as_str` produces it.
    pub min_pressure: Option<&'static str>,
    /// Whether the grant says Autopilot may start a run in answer to a
    /// disk-pressure alert — the setting as the user left it, in force or not
    /// (HORO-1510). For a control's on/off state.
    pub respond_to_alerts: bool,
    /// Whether an unprompted run is authorized **right now**: this grant is in
    /// force and it says so. The field to branch on.
    ///
    /// Both are reported because they answer different questions and a client
    /// needs each for a different job. A settings toggle has to show what the
    /// user chose even while Autopilot is revoked — otherwise revoking would
    /// look like it had silently cleared the preference. Anything that *acts*
    /// has to consult the conjunction, and must not be the thing that computes
    /// it: `enabled && respond_to_alerts` evaluated in Swift would be the
    /// client deciding its own authority. Here it is a quotation of
    /// [`AutopilotEnvelope::starts_unprompted`](crate::autopilot::AutopilotEnvelope::starts_unprompted).
    pub starts_unprompted: bool,
    pub ceilings: AutopilotCeilingsReport,
    /// Every resource kind that may be allowlisted, in
    /// [`crate::evidence::ResourceKind::ALL`] order.
    pub allowlistable_kinds: Vec<&'static str>,
    /// Every resource kind that never may be — today exactly the unknown
    /// kind, which [`crate::policy::classify`] treats as unconditionally
    /// protected.
    pub never_allowlistable_kinds: Vec<&'static str>,
    /// Every policy reason an `ASK` decision may be pre-authorized for.
    pub preauthorizable_reasons: Vec<&'static str>,
    /// Every reason a decision can be held back by that never may be
    /// pre-authorized: live use, a dirty worktree, a live owning tool, all
    /// three evidence-quality reasons, and every protected reason.
    ///
    /// Narrower than "every reason not in `preauthorizable_reasons`" on
    /// purpose. The three `AUTO_SAFE` justifications are also not
    /// pre-authorizable, and saying so would be a warning about nothing —
    /// they are why a decision needed no consent in the first place. See
    /// `crate::autopilot::report`'s `is_refusal_reason`.
    pub never_preauthorizable_reasons: Vec<&'static str>,
    /// Every pressure state, so a client can offer the threshold choices
    /// without knowing what they are.
    pub pressure_states: Vec<&'static str>,
    /// The policy labels no envelope can make executable, as
    /// [`crate::reporting::PolicyLabel::as_str`] tokens.
    pub never_executable_labels: Vec<&'static str>,
    /// What the model's authority actually is, one sentence per fact, in
    /// Rust because it is a statement about Rust's behaviour. A client that
    /// wrote these sentences itself would be describing an implementation it
    /// cannot see.
    pub ai_authority: Vec<&'static str>,
    /// Where the envelope file lives, or `null` if the path could not be
    /// resolved. Local, and never part of any provider request.
    pub stored_at: Option<String>,
}

/// A recovery goal, rendered on **both** axes plus one unambiguous sentence
/// (HORO-1506).
///
/// Both percentages are present deliberately. A client that shows only one
/// of them still cannot be wrong about which it has, because the field names
/// say so, and `description` gives a client with nowhere to put two numbers
/// a single string that is still unambiguous. Produced by
/// [`crate::executor::goal::RecoveryGoal`], which is the only type allowed
/// to convert between the two.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecoveryGoalReport {
    /// Target disk usage. The product-facing axis.
    pub used_percent: f64,
    /// The same goal as the recovery loop's own `FreeTarget::Percentage`
    /// value, i.e. percent of capacity free.
    pub free_percent: f64,
    /// e.g. `"60% used (40% free)"`. Built in Rust so the CLI, the JSON, the
    /// menu-bar label and the spoken accessibility string cannot word this
    /// differently — the same reasoning as [`ExecuteReport`]'s human byte
    /// fields (HORO-1312).
    pub description: String,
}

/// What is *estimated* to be reclaimable right now, split by what policy
/// would actually permit (HORO-1506).
///
/// Every byte figure here is a detector **estimate**, and the split is the
/// point: a client that shows one total invites the user to read protected
/// and confirmation-gated space as space they are about to get back. Nothing
/// in this struct may be used to decide that a goal was met — see
/// [`RecoveryPreviewReport::goal_appears_reachable`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RecoveryOpportunityReport {
    /// Candidates that are `executable` and whose offered action does not
    /// require confirmation: the space a recovery run could take without
    /// asking.
    pub actionable_now_count: usize,
    pub actionable_now_bytes: u64,
    pub actionable_now_human: String,
    /// Candidates that are `executable` but whose offered action requires
    /// confirmation. Real opportunity, but not automatic.
    pub requires_confirmation_count: usize,
    pub requires_confirmation_bytes: u64,
    pub requires_confirmation_human: String,
    /// Candidates that are not executable at all, and the `PROTECTED` subset
    /// of them. Counted, not summed: presenting bytes a run can never take
    /// as an "opportunity" would be the exact misread this split exists to
    /// prevent.
    pub not_executable_count: usize,
    pub protected_count: usize,
    /// `true` when any candidate contributing to a byte total above reported
    /// its estimate as a lower bound, so the real figure may be larger.
    pub is_lower_bound: bool,
}

/// The pre-flight for a recovery goal: what the volume looks like now, what
/// the goal requires, and what is estimated to be available toward it —
/// before anything is mutated (HORO-1506).
///
/// Produced by `glomeris free --dry-run`. Nothing in this report deletes
/// anything, and nothing in it is authoritative about completion: the loop
/// decides that from re-measured filesystem state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecoveryPreviewReport {
    pub goal: RecoveryGoalReport,
    /// Current capacity and pressure state, the same shape `status --json`
    /// prints, so a client parses one thing to learn one thing.
    pub current: StatusReport,
    /// Free bytes the volume must reach for the goal to be met.
    pub required_free_bytes: u64,
    pub required_free_human: String,
    /// Additional free bytes still needed. Measured, from `current`.
    pub bytes_needed: u64,
    pub bytes_needed_human: String,
    pub opportunity: RecoveryOpportunityReport,
    /// A **planning** judgment only: whether the estimated
    /// `actionable_now_bytes` covers `bytes_needed`.
    ///
    /// Deliberately named "appears". Section 9 of the recovery campaign is
    /// explicit that a target may never be satisfied from summed candidate
    /// estimates, and this field is that forbidden sum — safe here precisely
    /// because it decides nothing. `false` does not mean a run is pointless
    /// (estimates are often lower bounds), and `true` does not mean the goal
    /// will be reached. Only re-measured free space determines that.
    pub goal_appears_reachable: bool,
    /// `false` as soon as any detector's probe failed, so an incomplete
    /// search cannot read as a complete one.
    pub discovery_complete: bool,
    /// Plain sentences a client can show verbatim, covering exactly the ways
    /// this report can mislead: estimates are not measurements, discovery may
    /// be partial, and confirmation-gated space is not automatic.
    pub caveats: Vec<String>,
    pub detectors: Vec<DetectorHealthReport>,
    pub candidates: Vec<DetectCandidateReport>,
}

/// One line of the `--progress-json` NDJSON stream a recovery run emits on
/// stderr (HORO-1509) — the projection of
/// [`crate::executor::recovery_loop::RecoveryProgress`].
///
/// This is what makes the closed loop watchable: which iteration is running,
/// what it is doing right now, and how many bytes have *actually* been
/// reclaimed. A client that had only the final [`RecoveryRunReport`] could show
/// nothing but a spinner for a phase the v0.2.0 dogfood measured in minutes.
///
/// Same internally-tagged shape and same reasoning as [`ProgressEvent`], whose
/// doc comment explains why this module's one NDJSON family uses
/// `#[serde(tag = "phase")]` while every report field gets a `&'static str` tag
/// from a free function. The two streams share the `phase` key and nothing else:
/// `ProgressEvent` describes a discovery scan, this describes a run that
/// mutates the filesystem, and no phase name appears in both.
///
/// Two rules hold across every variant, and both exist so a progress line can
/// never overstate what happened:
///
/// - **Measured bytes only.** `bytes_freed_so_far` is re-measured free space,
///   never a sum of candidate estimates (campaign section 9). The one estimate
///   in the stream is [`RecoveryProgressEvent::ActionStarted::estimated_bytes`],
///   named so it cannot be read as progress.
/// - **Absent, not zero.** Every optional byte count is omitted rather than
///   serialized as `0`/`null` when it could not be measured, because a client
///   showing "0 bytes reclaimed" for an action whose size is unknown is
///   reporting a measurement nobody took.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum RecoveryProgressEvent {
    /// Loop step 1: the filesystem was measured. The only statement of fact
    /// about free space in this stream, and therefore the only event a client
    /// may update its "current usage" display from.
    Measured {
        iteration: u32,
        total_bytes: u64,
        free_bytes: u64,
        used_percent: f64,
        free_human: String,
        bytes_freed_so_far: u64,
        bytes_freed_so_far_human: String,
    },
    /// Loop step 4 is starting: detectors are being asked what exists *now*.
    /// The "rescanning" state HORO-1509 asks to be visible. Re-entered every
    /// iteration by design — the loop never reuses an earlier pass's list.
    Discovering { iteration: u32 },
    /// Loop step 4 finished. `candidates` counts what has a resolvable action
    /// rather than what a detector saw, and a non-zero `detectors_failed` is
    /// what withdraws a later `safe_exhausted`'s usual meaning: part of the
    /// disk was never looked at.
    Discovered {
        iteration: u32,
        candidates: u32,
        detectors_failed: u32,
    },
    /// Loop steps 5-6: evidence is being re-collected and reclassified before
    /// anything is chosen. A client must not offer a "confirm" affordance from
    /// a stale earlier pass while this is in flight.
    Revalidating { iteration: u32 },
    /// Loop step 8: a real mutation is about to run.
    ActionStarted {
        iteration: u32,
        resource: String,
        action: String,
        policy_label: &'static str,
        /// An estimate, named as one. Never accumulated into progress.
        #[serde(skip_serializing_if = "Option::is_none")]
        estimated_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        estimated_human: Option<String>,
    },
    /// Loop step 9: the mutation finished. `reclaimed_bytes` is what the
    /// executor measured for this one action; absent means it could not be
    /// measured.
    ActionFinished {
        iteration: u32,
        resource: String,
        action: String,
        outcome: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        reclaimed_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reclaimed_human: Option<String>,
        bytes_freed_so_far: u64,
        bytes_freed_so_far_human: String,
    },
    /// The user's cooperative stop was observed — between actions, never
    /// during one. The run finishes with `stop_reason: "stopped_by_user"`; this
    /// event is what lets a UI stop offering the button before that arrives.
    StopRequested { iteration: u32 },
}

/// What a run that ran out of safe work left behind (HORO-1509).
///
/// Present on [`RecoveryRunReport`] for the two stops for which "what is still
/// there" is part of the answer: `safe_exhausted`, and `envelope_refused` where
/// the envelope refused after a discovery pass (HORO-1510). A run that reached
/// its goal, hit a run budget, was stopped by the user, or was refused by its
/// envelope *before* looking concluded nothing about the candidates it never got
/// to, and reporting zeros for those would be a claim it did not make.
///
/// Counts of candidates, never bytes. Summing space a run is not permitted to
/// take would present unreachable space as an opportunity — the same misread
/// [`RecoveryOpportunityReport`] splits its own totals to prevent. The field
/// names match that report's deliberately: a client learns one vocabulary for
/// "needs confirmation / protected / not executable" and uses it before and
/// after a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecoveryRemainingReport {
    /// Reachable, real, and waiting for the user to say yes.
    pub requires_confirmation_count: u32,
    /// Refused by policy on evidence. Not a queue: a later run refuses these
    /// again on the same evidence.
    pub protected_count: u32,
    /// Past the policy gate, but the offered action refuses to run against the
    /// resource as it currently stands — a live tool, work in progress. This
    /// one may well be available tomorrow.
    pub not_executable_count: u32,
    /// Executable and safe, and refused by the *Autopilot envelope* instead:
    /// a resource kind the user did not allowlist, an `Ask` risk they did not
    /// pre-authorize, or a size the remaining byte budget cannot cover
    /// (HORO-1510).
    ///
    /// Always `0` for a run the user started themselves. Kept apart from the
    /// three above because it is the only one of the four that says nothing
    /// about the resource: a recovery the user starts can take all of these,
    /// today, and a client that folded this into `protected_count` would tell
    /// them the opposite.
    pub not_permitted_by_autopilot_count: u32,
}

/// The outcome of a real recovery run, machine-readable (HORO-1506).
///
/// `glomeris free` printed prose only, which left a GUI with nothing to
/// parse but terminal output. Every number here comes from
/// [`crate::executor::recovery_loop::RecoveryReport`], and the byte figures
/// are measured, never estimated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecoveryRunReport {
    /// The goal this run worked toward, present whenever the run was started
    /// from a used-percent goal. `null` for the raw `--target` form, whose
    /// value is a free-space floor and is reported in `target` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<RecoveryGoalReport>,
    /// How the target was expressed on the command line, e.g.
    /// `"20% free"` or `"5 GiB free"` — always naming the axis.
    pub target: String,
    /// Stable snake_case tag, from [`stop_reason_tag`].
    pub stop_reason: &'static str,
    /// One sentence explaining the stop reason in the terms a user cares
    /// about. Never the bare word "Done": a run that stopped because nothing
    /// safe was left says so.
    pub stop_reason_detail: String,
    /// Present only for `stop_reason == "error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Which envelope limit refused, as a stable snake_case token from
    /// [`crate::autopilot::RefusalReason::as_str`]. Present only for
    /// `stop_reason == "envelope_refused"` (HORO-1510).
    ///
    /// A separate field rather than a second stop-reason tag per refusal, so
    /// `stop_reason` stays a small closed set a client can exhaustively handle,
    /// and so the token here is the *same* vocabulary `glomeris execute` and
    /// `glomeris autopilot` already publish for the same refusals — a client
    /// that has wording for `action_budget_exhausted` does not need new wording
    /// because the refusal arrived at the end of a recovery run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope_refusal: Option<&'static str>,
    /// What the run's last discovery pass looked at and left alone. Present
    /// for `stop_reason == "safe_exhausted"`, and for
    /// `stop_reason == "envelope_refused"` when the envelope refused *after* a
    /// discovery pass rather than before one — see
    /// [`RecoveryRemainingReport`], which explains why it is absent otherwise
    /// rather than zeroed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<RecoveryRemainingReport>,
    pub iterations_run: u32,
    pub actions_executed: u32,
    pub actions_declined_or_skipped: u32,
    /// Sum of **actual** reclaimed bytes, never expected.
    pub bytes_freed_measured: u64,
    pub bytes_freed_measured_human: String,
    pub started_free_bytes: u64,
    pub started_free_human: String,
    pub final_free_bytes: u64,
    pub final_free_human: String,
    /// `true` when the goal/target was satisfied by the final re-measured
    /// free space. Derived from the filesystem reading, not from the sum of
    /// what was deleted.
    pub target_met: bool,
    /// Detectors whose probe failed during the run, `<id>: <reason>`.
    pub detector_failures: Vec<String>,
    /// `false` when `detector_failures` is non-empty.
    pub discovery_complete: bool,
    /// The same caveat sentences the prose output prints, so a client cannot
    /// present a partial search as a complete one.
    pub caveats: Vec<String>,
}

/// A refused recovery goal, machine-readable (HORO-1506 AC4).
///
/// Emitted instead of [`RecoveryRunReport`] when a goal is rejected before
/// anything runs, so a `--json` client learns *why* without reading terminal
/// prose. The two axes are named in the field names for the same reason
/// [`RecoveryGoalReport`] carries both.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecoveryGoalRejectionReport {
    /// Stable snake_case tag, from
    /// [`crate::executor::goal::GoalRejection::as_str`].
    pub reason: &'static str,
    /// The rejection's own `Display` text, shown verbatim to a user.
    pub message: String,
    /// The goal that was asked for, on the used axis. `null` when the value
    /// was not a usable number at all (`reason == "not_finite"`), because
    /// there is no finite figure to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_used_percent: Option<f64>,
    /// The volume's usage at the moment of refusal, present only when the
    /// refusal was decided against an observation
    /// (`reason == "not_an_improvement"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_used_percent: Option<f64>,
}

/// The two user-configurable numbers, plus where they came from (HORO-1507).
///
/// The alert threshold and the recovery goal are reported as separate fields
/// with separate names, because they are separate concepts: one decides when
/// the user is *told*, the other where a recovery run *stops*. A client that
/// showed them as one "disk percentage" would be describing a product that
/// does not exist.
///
/// The goal reuses [`RecoveryGoalReport`] rather than carrying a bare number,
/// so the stored default and a goal typed on the command line are rendered by
/// the same code and cannot disagree about which axis they are on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecoverySettingsReport {
    /// Percent of capacity **used** at which the user wants to be notified.
    pub notify_at_used_percent: f64,
    /// e.g. `"75% used"`. Built in Rust for the same reason
    /// [`RecoveryGoalReport::description`] is.
    pub notify_at_description: String,
    /// The recovery goal a new run starts from unless the user overrides it.
    pub default_goal: RecoveryGoalReport,
    /// What each of the two numbers is allowed to be.
    pub bounds: RecoverySettingsBoundsReport,
    /// Absolute path of the settings file, or `null` when `$HOME` could not
    /// be resolved. Local, and never part of any provider request — same rule
    /// as [`AutopilotEnvelopeReport::stored_at`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_at: Option<String>,
    /// `false` when no settings file exists yet and these are therefore the
    /// built-in defaults.
    ///
    /// Deliberately about the file rather than the values: a bool named
    /// `is_default` would be ambiguous for a user who has explicitly saved
    /// the default numbers, and a GUI that showed "not configured" to someone
    /// who had just configured it would be wrong in the more confusing
    /// direction.
    pub loaded_from_file: bool,
}

/// What the two settings are each allowed to be (HORO-1507).
///
/// Reported for the same reason [`AutopilotCeilingsReport`] is: a settings
/// screen that knew these numbers independently would eventually offer a value
/// the CLI then refuses, and the refusal would arrive after the user pressed
/// Save. Published as data, a control can be built that cannot compose a
/// request outside them.
///
/// What is deliberately **not** here is the cross-field rule — that the goal
/// must be below the threshold. It is not a bound on either number: it depends
/// on the other one, it moves as the other one moves, and a client that tried
/// to encode it as a range would be reimplementing
/// [`RecoverySettings::with_changes`](crate::settings::RecoverySettings::with_changes)
/// rather than reading it. That rule stays where it is enforced, and a client
/// learns it from the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RecoverySettingsBoundsReport {
    /// Inclusive lower bound on the alert threshold, percent used.
    pub notify_at_minimum_used_percent: f64,
    /// Inclusive upper bound on the alert threshold, percent used.
    pub notify_at_maximum_used_percent: f64,
    /// Inclusive lower bound on the recovery goal, percent used.
    pub goal_minimum_used_percent: f64,
    /// Inclusive upper bound on the recovery goal, percent used.
    pub goal_maximum_used_percent: f64,
}

/// A refused settings change, machine-readable (HORO-1507).
///
/// Emitted instead of [`RecoverySettingsReport`] when a change is rejected,
/// so a `--json` client learns *why* without reading terminal prose — the
/// same contract as [`RecoveryGoalRejectionReport`].
///
/// There is no `field` tag: `reason` already says which field is at fault
/// (`notify_threshold_*` versus `goal_*`), and a second vocabulary saying the
/// same thing would be one more thing that can disagree with the first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingsRejectionReport {
    /// Stable snake_case tag, from
    /// [`crate::settings::SettingsRejection::as_str`].
    pub reason: &'static str,
    /// The rejection's own `Display` text, shown verbatim to a user.
    pub message: String,
    /// The alert threshold that would have been in force, on the used axis.
    /// Present whenever a finite figure was involved in the refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_at_used_percent: Option<f64>,
    /// The goal that would have been in force, on the used axis. Present on
    /// the same terms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_used_percent: Option<f64>,
}

/// The current pressure episode, machine-readable (HORO-1508).
///
/// A projection of [`crate::monitor::PressureEpisode`], which already derives
/// `Serialize` for its own state file. Projected anyway, for the reason this
/// module's doc comment gives: the state file is this build's private format
/// and may change shape, whereas this is a contract the menu-bar app parses.
/// Serializing the domain type directly would make every future field rename
/// a breaking change to the app.
///
/// Nothing here is authority to delete anything. An episode decides when the
/// user is *spoken to*; what may run is decided by policy against each actual
/// candidate, every time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PressureEpisodeReport {
    /// Identifies this episode, and with it the notification: the app uses it
    /// as the notification's identifier so a second episode cannot coalesce
    /// into the first one's banner.
    pub episode_id: u64,
    pub opened_unix_secs: u64,
    /// Usage when the episode opened, the worst seen since, and the latest
    /// reading — three separate numbers because a user who is told "92%" by a
    /// notification and shown "78%" by the app has been told two things and
    /// believes neither.
    pub opened_used_percent: f64,
    pub peak_used_percent: f64,
    pub latest_used_percent: f64,
    pub latest_free_bytes: u64,
    pub latest_free_human: String,
    pub latest_unix_secs: u64,
    /// `true` when a notification is owed and has not been raised. The
    /// daemon sets this; the app raises the notification and clears it by
    /// calling `glomeris pressure notified`.
    pub notification_due: bool,
    /// How many notifications this episode has produced. One per episode
    /// normally, plus one per elapsed snooze — never one per poll.
    pub notifications_raised: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_notified_unix_secs: Option<u64>,
    /// The user's answer, from
    /// [`crate::monitor::EpisodeResponse::as_str`], or `null` if they have
    /// not answered yet. A closed set of three tags — see
    /// [`PressureStatusReport::responses`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responded_unix_secs: Option<u64>,
    /// When a "remind me later" answer expires. Reported as an absolute
    /// instant rather than a countdown so a client that renders it late
    /// cannot show a duration that has quietly gone stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snoozed_until_unix_secs: Option<u64>,
    /// Whether the snooze above is still in force *as of the reading this
    /// report was built from*. Derived, and reported separately from the
    /// deadline because a client must not have to do clock arithmetic to
    /// decide whether it is allowed to show a banner.
    pub is_snoozed: bool,
}

/// Everything the menu-bar app needs to decide whether to raise a pressure
/// notification, and what to say in it (HORO-1508).
///
/// Printed by `glomeris pressure show --json`. The division of labour this
/// shape encodes is the whole design: Rust decides *whether* a notification is
/// owed (`episode.notification_due`), because an actionable notification
/// requires `UNUserNotificationCenter` and therefore an app bundle, which the
/// launchd daemon is not. Swift decides only how it looks. No part of the
/// hysteresis, snooze or dedupe rules is re-expressible by a client.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PressureStatusReport {
    /// The user's alert threshold, percent **used** — the same number
    /// [`RecoverySettingsReport::notify_at_used_percent`] reports, read from
    /// the same settings file.
    pub notify_at_used_percent: f64,
    /// e.g. `"75% used"`.
    pub notify_at_description: String,
    /// Usage at or below which an open episode ends: the threshold less the
    /// hysteresis margin. Published because AC1 requires the hysteresis to be
    /// documented, and a client that showed the threshold alone could not
    /// explain why a 74%-used disk is still in an episode.
    pub clear_at_used_percent: f64,
    /// How long "remind me later" defers a notification, in seconds. Fixed,
    /// not a preference, and reported so the app's button can say how long
    /// without hard-coding a number Rust could later change.
    pub snooze_secs: u64,
    /// The volume right now, the same shape `status --json` prints.
    pub current: StatusReport,
    /// Whether the latest reading is at or above the threshold. Note this is
    /// *not* the same as "an episode is open": hysteresis keeps an episode
    /// open between the clear boundary and the threshold, and that gap is
    /// exactly what stops a disk hovering at the boundary from notifying
    /// repeatedly.
    pub threshold_crossed: bool,
    /// `null` when no episode is open, which is the ordinary state of a
    /// healthy disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<PressureEpisodeReport>,
    /// The one field a client acts on: raise a notification if and only if
    /// this is `true`. `false` whenever there is no episode, the episode has
    /// been answered, a snooze is running, or the notification was already
    /// raised.
    ///
    /// Hoisted out of `episode` deliberately. Buried one level down it would
    /// invite `episode != null && used >= threshold` — a client
    /// re-deriving the rule, and getting the snooze and the already-raised
    /// cases wrong.
    pub notification_due: bool,
    /// The goal a "Review & recover" press should arrive at, so the deep link
    /// carries the user's configured destination rather than one the app
    /// invented (HORO-1508 AC2).
    pub default_goal: RecoveryGoalReport,
    /// The complete set of answers `glomeris pressure respond` accepts, from
    /// [`crate::monitor::EpisodeResponse::ALL`].
    ///
    /// Published as data for the reason [`RecoverySettingsBoundsReport`] is:
    /// a client that knew these strings independently could offer a fourth
    /// button, and the refusal would arrive after the user pressed it. There
    /// is deliberately no "skip" — see HORO-1508's acceptance criteria, which
    /// require each action to say what it does.
    pub responses: Vec<&'static str>,
    /// Absolute path of the episode state file, or `null` when `$HOME` could
    /// not be resolved. Local, and never part of any provider request — same
    /// rule as [`RecoverySettingsReport::stored_at`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_path: Option<String>,
}

/// A refused answer or acknowledgement, machine-readable (HORO-1508).
///
/// Emitted instead of [`PressureStatusReport`] when `pressure respond` or
/// `pressure notified` cannot be applied — the same contract as
/// [`SettingsRejectionReport`]. The common case is benign and must still be
/// reported honestly: the disk recovered between the banner appearing and the
/// button being pressed, so there is no longer an episode the answer belongs
/// to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PressureRejectionReport {
    /// Stable snake_case tag, from
    /// [`crate::monitor::EpisodeRejection::as_str`].
    pub reason: &'static str,
    /// The rejection's own `Display` text, shown verbatim to a user.
    pub message: String,
}

/// Stable snake_case tag per [`crate::executor::recovery_loop::StopReason`],
/// following this module's convention of projecting a domain enum to a
/// `&'static str` rather than deriving `Serialize` on it.
pub fn stop_reason_tag(reason: &crate::executor::recovery_loop::StopReason) -> &'static str {
    use crate::executor::recovery_loop::StopReason;
    match reason {
        StopReason::TargetReached => "target_reached",
        StopReason::SafeExhausted(_) => "safe_exhausted",
        StopReason::BudgetExceeded => "budget_exceeded",
        StopReason::NoProgress => "no_progress",
        StopReason::StoppedByUser => "stopped_by_user",
        StopReason::EnvelopeRefused { .. } => "envelope_refused",
        StopReason::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        GitState, ProbeOutcome, ProbeReason, ProcessRef, Recoverability, ResourceFingerprint,
        ResourceId, ResourceKind, ResourceLocator,
    };
    use crate::policy::{PolicyClass, ReasonCode};

    // --- HORO-1327: RefusalReason is the sole producer of execute's tokens ---

    /// Every reason must map to a distinct, non-empty token: this is the
    /// machine-readable string a `--json` caller switches on, and the key the
    /// menu-bar app looks its wording up by.
    #[test]
    fn as_str_is_distinct_and_non_empty_for_every_variant() {
        let mut tokens: Vec<&'static str> = RefusalReason::ALL.iter().map(|r| r.as_str()).collect();
        let original_len = tokens.len();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), original_len, "as_str tokens must be distinct");
        assert!(tokens.iter().all(|t| !t.is_empty()));
    }

    /// Compile-time guard, mirroring `ReasonCode`'s: the `match` is
    /// exhaustive, so a variant missing from [`RefusalReason::ALL`] fails to
    /// build here. The length assertion catches the reverse — a stale or
    /// duplicated entry.
    ///
    /// `ALL` matters more here than for a type that only needs iteration:
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` diffs `as_str` against
    /// the Swift table, and a variant absent from `ALL` would still be
    /// *emitted* by the CLI. The guard would catch that too, but only after
    /// it shipped; this fails at build time.
    #[test]
    fn all_lists_every_variant_exactly_once() {
        for (index, reason) in RefusalReason::ALL.iter().enumerate() {
            let expected_index = match reason {
                RefusalReason::ResourceNotFound => 0,
                RefusalReason::ActionNotFound => 1,
                RefusalReason::ActionMismatch => 2,
                RefusalReason::Protected => 3,
                RefusalReason::AskNoConsent => 4,
                RefusalReason::AskConsentMismatch => 5,
                RefusalReason::AutoSafeContractViolation => 6,
                RefusalReason::Busy => 7,
            };
            assert_eq!(
                index, expected_index,
                "{reason} is at index {index} of RefusalReason::ALL, expected {expected_index}"
            );
        }
        assert_eq!(
            RefusalReason::ALL.len(),
            8,
            "RefusalReason::ALL has gained, lost, or duplicated an entry"
        );
    }

    /// The typed field must serialize to the bare token, unchanged from the
    /// string literals it replaced — a `--json` caller parsing `reason` sees
    /// no difference, which is the whole point of typing it.
    #[test]
    fn a_refusal_report_serializes_its_reason_as_the_bare_token() {
        let json = serde_json::to_value(ExecuteRefusalReport {
            reason: RefusalReason::AskConsentMismatch,
            message: "irrelevant here".to_string(),
        })
        .expect("serialize");
        assert_eq!(json["reason"], "ask_consent_mismatch");
        assert!(
            json["reason"].is_string(),
            "must be the token itself, not an object or a variant name: {}",
            json["reason"]
        );
    }

    /// Every variant, not just a representative one: it is `as_str` that
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` reads, so any variant
    /// whose serialized token differs from it would be a token the guard
    /// verified and the CLI never emits.
    ///
    /// Near-tautological against the hand-written `Serialize` impl, and
    /// deliberately kept anyway: it is what makes a future switch to
    /// `#[serde(rename_all = "snake_case")]` detectable the moment any token
    /// stops equalling the snake_case of its variant name. It would not catch
    /// that switch on its own today, because all eight currently agree — the
    /// type's doc comment is where the reason for hand-writing the impl is
    /// recorded, and this test is the tripwire, not the argument.
    #[test]
    fn every_variant_serializes_to_exactly_its_as_str() {
        for reason in RefusalReason::ALL {
            let json = serde_json::to_value(ExecuteRefusalReport {
                reason: *reason,
                message: String::new(),
            })
            .expect("serialize");
            assert_eq!(
                json["reason"],
                reason.as_str(),
                "{reason} serializes to something other than its own as_str"
            );
        }
    }

    fn base_evidence() -> Evidence {
        Evidence {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from("/tmp/proj/target")),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(2048),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(1024),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: Regenerability::RegenerableByRebuild,
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    fn decision(class: PolicyClass, reasons: Vec<ReasonCode>) -> PolicyDecision {
        PolicyDecision {
            resource: ResourceId::new(
                ResourceKind::CargoTargetDir,
                ResourceLocator::Path(PathBuf::from("/tmp/proj/target")),
            ),
            class,
            reasons,
            evidence_collected_at: SystemTime::UNIX_EPOCH,
            evaluated_at: SystemTime::UNIX_EPOCH,
            policy_version: 1,
        }
    }

    #[test]
    fn detect_candidate_report_projects_expected_fields() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            None,
            ImpactContext::default(),
        );

        assert_eq!(report.resource_id, "cargo_target_dir:/tmp/proj/target");
        assert_eq!(report.kind, "cargo_target_dir");
        assert_eq!(report.reclaimable_bytes, Some(1024));
        assert_eq!(report.reclaimable_human.as_deref(), Some("1.0 KB"));
        assert!(!report.reclaimable_bytes_is_lower_bound);
        assert_eq!(report.policy_label, "AUTO_SAFE");
        assert_eq!(report.reasons, vec!["no_active_use_observed"]);
    }

    #[test]
    fn detect_candidate_report_handles_missing_reclaimable_bytes() {
        let mut ev = base_evidence();
        ev.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            None,
            ImpactContext::default(),
        );

        assert_eq!(report.reclaimable_bytes, None);
        assert_eq!(report.reclaimable_human, None);
        assert_eq!(report.policy_label, "UNKNOWN_INCOMPLETE");
    }

    /// HORO-1049: the typed lower-bound flag surfaces onto the DTO when
    /// set on `Evidence`.
    #[test]
    fn detect_candidate_report_surfaces_lower_bound_flag() {
        let mut ev = base_evidence();
        ev.reclaimable_bytes_is_lower_bound = true;
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            None,
            ImpactContext::default(),
        );

        assert!(report.reclaimable_bytes_is_lower_bound);
    }

    /// HORO-1049: the flag must never surface as `true` when there is no
    /// observed `reclaimable_bytes` to attach it to — a lower bound on
    /// nothing is a contradiction, not a signal.
    #[test]
    fn detect_candidate_report_lower_bound_flag_is_false_without_observed_bytes() {
        let mut ev = base_evidence();
        ev.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.reclaimable_bytes_is_lower_bound = true;
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            None,
            ImpactContext::default(),
        );

        assert!(!report.reclaimable_bytes_is_lower_bound);
    }

    #[test]
    fn explain_report_distinguishes_logical_from_reclaimable_bytes() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, None);

        assert_eq!(report.logical_bytes, Some(2048));
        assert_eq!(report.reclaimable_bytes, Some(1024));
        assert_ne!(report.logical_bytes, report.reclaimable_bytes);
        assert_eq!(report.logical_human.as_deref(), Some("2.0 KB"));
        assert_eq!(report.reclaimable_human.as_deref(), Some("1.0 KB"));
        assert!(!report.reclaimable_bytes_is_lower_bound);
    }

    /// HORO-1049: `explain`'s DTO surfaces the typed lower-bound flag too.
    #[test]
    fn explain_report_surfaces_lower_bound_flag() {
        let mut ev = base_evidence();
        ev.reclaimable_bytes_is_lower_bound = true;
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, None);

        assert!(report.reclaimable_bytes_is_lower_bound);
    }

    #[test]
    fn explain_report_native_cleanup_available() {
        let mut ev = base_evidence();
        ev.native_cleanup =
            NativeCleanup::Available(crate::evidence::ActionId("cargo.clean.target_dir"));
        let d = decision(PolicyClass::AutoSafe, vec![]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, None);

        assert!(report.native_cleanup_available);
        assert_eq!(
            report.native_cleanup_action_id,
            Some("cargo.clean.target_dir")
        );
    }

    #[test]
    fn explain_report_native_cleanup_unsupported() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, None);

        assert!(!report.native_cleanup_available);
        assert_eq!(report.native_cleanup_action_id, None);
    }

    #[test]
    fn active_use_signals_empty_for_clean_evidence() {
        let ev = base_evidence();
        assert!(active_use_signals(&ev).is_empty());
    }

    #[test]
    fn active_use_signals_reports_open_by_process() {
        let mut ev = base_evidence();
        ev.open_by_process = ProbeOutcome::Observed(vec![ProcessRef {
            pid: 42,
            command: "cargo".to_string(),
        }]);
        let signals = active_use_signals(&ev);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].contains("cargo(pid 42)"));
    }

    #[test]
    fn active_use_signals_reports_dirty_git_worktree() {
        let mut ev = base_evidence();
        ev.git_state = ProbeOutcome::Observed(Some(GitState {
            repo_root: PathBuf::from("/tmp/proj"),
            common_dir: PathBuf::from("/tmp/proj/.git"),
            dirty: true,
            untracked: false,
            worktree: false,
        }));
        let signals = active_use_signals(&ev);
        assert!(signals.iter().any(|s| s.contains("dirty")));
    }

    #[test]
    fn active_use_signals_reports_tool_liveness() {
        let mut ev = base_evidence();
        ev.tool_liveness = ProbeOutcome::Observed(true);
        let signals = active_use_signals(&ev);
        assert!(signals.iter().any(|s| s.contains("running")));
    }

    #[test]
    fn active_use_signals_never_infers_activity_from_unavailable_probes() {
        let mut ev = base_evidence();
        ev.open_by_process = ProbeOutcome::Unavailable(ProbeReason::Failed);
        ev.tool_liveness = ProbeOutcome::Unavailable(ProbeReason::Failed);
        assert!(active_use_signals(&ev).is_empty());
    }

    #[test]
    fn status_report_serializes_to_json() {
        let report = StatusReport {
            total_bytes: 100,
            free_bytes: 25,
            used_percent: 75.0,
            free_human: "25 B".to_string(),
            total_human: "100 B".to_string(),
            pressure_state: "WARN",
        };
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(json.contains("\"pressure_state\":\"WARN\""));
    }

    // --- HORO-1053: executable/offered_actions/refusal_reason mappings ---
    //
    // One test per policy-class/action-availability combination named in
    // the ticket's AC, for BOTH `DetectCandidateReport` and `ExplainReport`
    // — the ticket explicitly calls this the most important AC and warns
    // against under-testing it.
    //
    // `base_evidence()`'s resource kind is `CargoTargetDir`, which has a
    // real registered action (`cargo.clean.target_dir`); `DockerBuildCache`
    // has none (see `actions::ActionRegistry`'s own
    // `find_for_kind_returns_none_for_a_kind_with_no_registered_action`
    // test) — used here for the "no resolvable action at all" case.

    fn action_for(kind: ResourceKind) -> &'static dyn Action {
        use std::sync::OnceLock;
        static REGISTRY: OnceLock<crate::actions::ActionRegistry> = OnceLock::new();
        REGISTRY
            .get_or_init(crate::actions::ActionRegistry::builtin)
            .find_for_kind(kind)
            .expect("kind must have a registered action")
    }

    fn cargo_action() -> &'static dyn Action {
        action_for(ResourceKind::CargoTargetDir)
    }

    /// HORO-1358: the policy-class → `requires_confirmation` mapping tests
    /// below need a resource whose action really does produce an executable
    /// plan, because `executable_fields` now puts that plan to the
    /// executor's own structural rule. `node.clean.node_modules` is the one
    /// registered action whose planner is a pure function of `Evidence`
    /// (a single `DeletePath` step — see `actions::node`), so these tests
    /// isolate the mapping they are actually about instead of depending on
    /// whether a `Cargo.toml` happens to exist on the test host.
    ///
    /// `cargo_action()`/[`base_evidence`] are still used by the tests that
    /// are specifically about plan-time failure and about PROTECTED and
    /// no-action-resolved, none of which reach the plan-shape check.
    fn node_action() -> &'static dyn Action {
        action_for(ResourceKind::NodeModules)
    }

    /// [`base_evidence`] with a kind whose planner touches no filesystem.
    /// See [`node_action`].
    fn plannable_evidence() -> Evidence {
        let mut ev = base_evidence();
        ev.resource = ResourceId::new(
            ResourceKind::NodeModules,
            ResourceLocator::Path(PathBuf::from("/tmp/proj/node_modules")),
        );
        ev
    }

    #[test]
    fn protected_is_never_executable_and_offers_no_action_detect() {
        let ev = base_evidence();
        let d = decision(
            PolicyClass::Protected,
            vec![ReasonCode::ProtectedCredentialMaterial],
        );
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(cargo_action()),
            ImpactContext::default(),
        );

        assert!(!report.executable);
        assert!(report.offered_actions.is_empty());
        assert!(report.refusal_reason.is_some());
        assert!(report
            .refusal_reason
            .as_deref()
            .unwrap()
            .contains("protected_credential_material"));
    }

    #[test]
    fn protected_is_never_executable_and_offers_no_action_explain() {
        let ev = base_evidence();
        let d = decision(
            PolicyClass::Protected,
            vec![ReasonCode::ProtectedCredentialMaterial],
        );
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(cargo_action()));

        assert!(!report.executable);
        assert!(report.offered_actions.is_empty());
        assert!(report.refusal_reason.is_some());
    }

    #[test]
    fn ask_with_resolvable_action_requires_confirmation_detect() {
        let ev = plannable_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::ResourceInActiveUse]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(node_action()),
            ImpactContext::default(),
        );

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert_eq!(
            report.offered_actions[0].action_id,
            "node.clean.node_modules"
        );
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn ask_with_resolvable_action_requires_confirmation_explain() {
        let ev = plannable_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::ResourceInActiveUse]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(node_action()));

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    /// `UNKNOWN_INCOMPLETE` is `PolicyClass::Ask` presentation-side
    /// (`label_for`), reached via an evidence-quality-only reason code —
    /// see `reporting::policy_label`'s doc comment.
    #[test]
    fn unknown_incomplete_with_resolvable_action_requires_confirmation_detect() {
        let ev = plannable_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(node_action()),
            ImpactContext::default(),
        );

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn unknown_incomplete_with_resolvable_action_requires_confirmation_explain() {
        let ev = plannable_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(node_action()));

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn auto_safe_with_resolvable_action_does_not_require_confirmation_detect() {
        let ev = plannable_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(node_action()),
            ImpactContext::default(),
        );

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert_eq!(
            report.offered_actions[0].action_id,
            "node.clean.node_modules"
        );
        assert!(!report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn auto_safe_with_resolvable_action_does_not_require_confirmation_explain() {
        let ev = plannable_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(node_action()));

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert!(!report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    /// A resource with no resolvable action at all is `executable:false`
    /// with `offered_actions:[]` regardless of policy class (tested here
    /// with `AutoSafe`, the class that would otherwise be executable) —
    /// and its `refusal_reason` must be distinguishable from PROTECTED's.
    #[test]
    fn no_resolvable_action_is_never_executable_regardless_of_policy_class_detect() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            None,
            ImpactContext::default(),
        );

        assert!(!report.executable);
        assert!(report.offered_actions.is_empty());
        let reason = report.refusal_reason.expect("must set a refusal reason");
        assert!(!reason.contains("PROTECTED"));
        assert!(reason.contains("no registered cleanup action"));
    }

    #[test]
    fn no_resolvable_action_is_never_executable_regardless_of_policy_class_explain() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, None);

        assert!(!report.executable);
        assert!(report.offered_actions.is_empty());
        let reason = report.refusal_reason.expect("must set a refusal reason");
        assert!(!reason.contains("PROTECTED"));
        assert!(reason.contains("no registered cleanup action"));
    }

    /// The two `refusal_reason` shapes ("PROTECTED: ..." vs "no registered
    /// cleanup action...") must never collide — a caller (the future
    /// SwiftUI app) still must not need to parse this string to know
    /// *which* refusal it got, but this locks that they are at least
    /// textually distinguishable today.
    #[test]
    fn protected_and_no_action_refusal_reasons_are_distinguishable() {
        let ev = base_evidence();
        let protected = decision(
            PolicyClass::Protected,
            vec![ReasonCode::ProtectedCredentialMaterial],
        );
        let protected_report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &protected,
            None,
            ImpactContext::default(),
        );
        let no_action = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let no_action_report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &no_action,
            None,
            ImpactContext::default(),
        );

        assert_ne!(
            protected_report.refusal_reason,
            no_action_report.refusal_reason
        );
    }
}
