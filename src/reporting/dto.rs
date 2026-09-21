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
use crate::policy::{PolicyClass, PolicyDecision};

use super::bytes::human_bytes;
use super::impact::{classify_impact, ImpactContext, ImpactThresholds};
use super::policy_label::{label_for, PolicyLabel};

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
    /// empty `Found(vec![])` — this event reports "how many candidates
    /// came out", not detector health; a caller that needs to tell those
    /// apart uses the final report, not this stream.
    DetectorFinished {
        detector: &'static str,
        candidates_found: usize,
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
/// [`label_for`]'s `UNKNOWN_INCOMPLETE` derivation are the only inputs
/// consulted.
fn executable_fields(
    decision: &PolicyDecision,
    resolved_action: Option<&dyn Action>,
) -> (bool, Vec<OfferedAction>, Option<String>) {
    if decision.class == PolicyClass::Protected {
        let reason = decision
            .reasons
            .first()
            .map(|r| r.as_str().to_string())
            .unwrap_or_else(|| "protected".to_string());
        return (false, Vec::new(), Some(format!("PROTECTED: {reason}")));
    }

    let Some(action) = resolved_action else {
        return (
            false,
            Vec::new(),
            Some("no registered cleanup action for this resource kind".to_string()),
        );
    };

    let requires_confirmation = matches!(
        label_for(decision),
        PolicyLabel::Ask | PolicyLabel::UnknownIncomplete
    );

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
    /// `true` only when there's a real registered action for this
    /// resource AND its policy class doesn't unconditionally forbid it
    /// (i.e. not `PROTECTED`) — see [`executable_fields`]. HORO-1053.
    pub executable: bool,
    /// Empty for `PROTECTED` or when no action resolves; one entry
    /// otherwise. HORO-1053.
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
            executable_fields(decision, resolved_action);
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

/// `glomeris detect` report: one candidate line per discovered resource.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct DetectReport {
    pub candidates: Vec<DetectCandidateReport>,
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
    /// `true` only when there's a real registered action for this
    /// resource AND its policy class doesn't unconditionally forbid it
    /// (i.e. not `PROTECTED`) — see [`executable_fields`]. HORO-1053.
    pub executable: bool,
    /// Empty for `PROTECTED` or when no action resolves; one entry
    /// otherwise. HORO-1053.
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
            executable_fields(decision, resolved_action);

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
/// BEFORE an `ExecuteResolution` exists at all: `reason: "busy"`, emitted
/// by `main.rs`'s `acquire_execution_lock_or_exit` when the HORO-1054
/// execution lock is already held by another invocation. Same shape,
/// same `--json` contract, deliberately not a new DTO.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecuteRefusalReport {
    /// One of `"resource_not_found"`, `"action_not_found"`,
    /// `"action_mismatch"`, `"protected"`, `"ask_no_consent"`,
    /// `"ask_consent_mismatch"`, `"auto_safe_contract_violation"` —
    /// mirrors `crate::cli::ExecuteResolution`'s non-`Executed` variants —
    /// or `"busy"`, emitted before that enum exists at all (see this
    /// struct's doc comment).
    pub reason: &'static str,
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

    fn cargo_action() -> &'static dyn Action {
        use std::sync::OnceLock;
        static REGISTRY: OnceLock<crate::actions::ActionRegistry> = OnceLock::new();
        REGISTRY
            .get_or_init(crate::actions::ActionRegistry::builtin)
            .find_for_kind(ResourceKind::CargoTargetDir)
            .expect("cargo_target_dir must have a registered action")
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
        let ev = base_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::ResourceInActiveUse]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(cargo_action()),
            ImpactContext::default(),
        );

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert_eq!(
            report.offered_actions[0].action_id,
            "cargo.clean.target_dir"
        );
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn ask_with_resolvable_action_requires_confirmation_explain() {
        let ev = base_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::ResourceInActiveUse]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(cargo_action()));

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
        let ev = base_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(cargo_action()),
            ImpactContext::default(),
        );

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn unknown_incomplete_with_resolvable_action_requires_confirmation_explain() {
        let ev = base_evidence();
        let d = decision(PolicyClass::Ask, vec![ReasonCode::EvidenceIncomplete]);
        assert_eq!(label_for(&d), PolicyLabel::UnknownIncomplete);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(cargo_action()));

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert!(report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn auto_safe_with_resolvable_action_does_not_require_confirmation_detect() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = DetectCandidateReport::from_evidence_and_decision(
            &ev,
            &d,
            Some(cargo_action()),
            ImpactContext::default(),
        );

        assert!(report.executable);
        assert_eq!(report.offered_actions.len(), 1);
        assert_eq!(
            report.offered_actions[0].action_id,
            "cargo.clean.target_dir"
        );
        assert!(!report.offered_actions[0].requires_confirmation);
        assert!(report.refusal_reason.is_none());
    }

    #[test]
    fn auto_safe_with_resolvable_action_does_not_require_confirmation_explain() {
        let ev = base_evidence();
        let d = decision(PolicyClass::AutoSafe, vec![ReasonCode::NoActiveUseObserved]);
        let report = ExplainReport::from_evidence_and_decision(&ev, &d, Some(cargo_action()));

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
