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
use super::policy_label::{label_for, PolicyLabel};

fn regenerability_tag(r: Regenerability) -> &'static str {
    match r {
        Regenerability::RegenerableByTool => "regenerable_by_tool",
        Regenerability::RegenerableByRebuild => "regenerable_by_rebuild",
        Regenerability::NotRegenerable => "not_regenerable",
        Regenerability::Unknown => "unknown",
    }
}

fn completeness_tag(c: &Completeness) -> &'static str {
    match c {
        Completeness::Complete => "complete",
        Completeness::Partial { .. } => "partial",
        Completeness::Failed => "failed",
    }
}

fn confidence_tag(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
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
    pub fn from_evidence_and_decision(
        ev: &Evidence,
        decision: &PolicyDecision,
        resolved_action: Option<&dyn Action>,
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
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmPlanItemReport {
    pub resource_id: String,
    pub policy_label: &'static str,
    pub requested_action_id: Option<&'static str>,
    pub priority: Option<u32>,
    /// Rendered from the typed `ActionPlan.explain` — `None` for a
    /// `PROTECTED` item (never resolved) or a resolution/dry-run failure
    /// (see `skip_reason` in that case).
    pub explain: Option<String>,
    pub skip_reason: Option<String>,
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
        let report = DetectCandidateReport::from_evidence_and_decision(&ev, &d, None);

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
        let report = DetectCandidateReport::from_evidence_and_decision(&ev, &d, None);

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
        let report = DetectCandidateReport::from_evidence_and_decision(&ev, &d, None);

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
        let report = DetectCandidateReport::from_evidence_and_decision(&ev, &d, None);

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
}
