//! Thin CLI orchestration (HORO-955) for `glomeris status|detect|explain|
//! clean|execute`. Every function here only calls already-existing
//! evidence/policy/action APIs — no policy/evidence/execution logic is
//! duplicated here, only wired together and projected into
//! [`crate::reporting`]'s report DTOs.
//!
//! `glomeris scan`/`free`/`emergency`/`daemon install`/`uninstall`/`run`
//! are unaffected and stay wired directly in `main.rs`, per this ticket's
//! scope. `daemon status --json` (HORO-1045) is the one `daemon`
//! subcommand whose report-building lives here, matching every other
//! `--json`-capable subcommand's shape.
//!
//! [`resolve_and_execute`] (HORO-1055) is `glomeris execute`'s enforcement
//! core: it calls the real, completely unmodified
//! [`crate::policy::approval::authorize`] and [`crate::executor::execute`]
//! — this module never re-derives or short-circuits either decision — and
//! is kept portable (no `std::process::exit`, no macOS gate) so it can be
//! exercised directly by unit tests. The macOS-only, exit-code-mapping
//! wiring (including the HORO-1054 execution lock) stays in `main.rs`.

pub mod help;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::actions::llm::{build_request_payload, plan_with_llm, LlmProvider};
use crate::actions::{Action, ActionRegistry};
use crate::detectors::{
    DetectorId, DetectorProgress, DetectorRegistry, DetectorStatus, DiscoveryContext,
};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Evidence, NativeCleanup, ResourceFingerprint, ResourceLocator};
use crate::executor::{execute, ExecutionOutcome, ExecutionReport};
use crate::monitor::{
    ActionSource, AuditRecord, FsUsage, Heartbeat, HistoryEntry, ThresholdConfig,
};
use crate::policy::approval::authorize;
use crate::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision, UserConsent};
use crate::reporting::dto::{
    ActionHistoryEventReport, ActionHistoryReport, ActionListItem, ActionListReport,
    CleanDryRunItem, CleanDryRunReport, DaemonStatusReport, DetectCandidateReport, DetectReport,
    ExecuteReport, ExplainReport, HistoryEventReport, HistoryReport, LlmCheckReport,
    LlmPayloadReport, LlmPayloadResourceAlias, LlmPlanItemReport, LlmPlanReport, ProgressEvent,
    StatusReport,
};
use crate::reporting::impact::ImpactContext;
use crate::reporting::policy_label::label_for;
use crate::reporting::ranking;

/// Correlation-refresh timeout for one candidate at the CLI layer. Mirrors
/// `executor::recovery_loop::CANDIDATE_CORRELATION_TIMEOUT`'s reasoning
/// (that constant is private to its module, so this is a separate,
/// intentionally short budget for an interactive CLI command rather than
/// a background recovery loop).
const CLI_CORRELATION_TIMEOUT: Duration = Duration::from_secs(3);

/// Builds a [`StatusReport`] from a raw [`FsUsage`] reading and the
/// pressure thresholds it's classified against. Pure — no I/O.
pub fn build_status_report(usage: &FsUsage, thresholds: &ThresholdConfig) -> StatusReport {
    let used_percent = usage.used_percent();
    let pressure_state = thresholds.classify(used_percent, usage.free_bytes).as_str();

    StatusReport {
        total_bytes: usage.total_bytes,
        free_bytes: usage.free_bytes,
        used_percent,
        free_human: crate::reporting::human_bytes(usage.free_bytes),
        total_human: crate::reporting::human_bytes(usage.total_bytes),
        pressure_state,
    }
}

/// Builds a [`DaemonStatusReport`] (HORO-1045) from the launchd-reported
/// install/load state plus the poll loop's own heartbeat file. Pure — no
/// I/O; callers pass in whatever `read_heartbeat` and `SystemTime`/
/// `unix_now_secs` already produced.
///
/// `loaded` and `heartbeat_age_secs` are surfaced as two independent
/// fields deliberately — see [`DaemonStatusReport`]'s doc comment for why
/// this must never collapse into one `healthy` boolean.
pub fn build_daemon_status_report(
    plist_installed: bool,
    plist_path: &Path,
    loaded: bool,
    heartbeat: Option<&Heartbeat>,
    now_unix_secs: u64,
) -> DaemonStatusReport {
    DaemonStatusReport {
        plist_installed,
        plist_path: plist_path.display().to_string(),
        loaded,
        heartbeat_age_secs: heartbeat
            .map(|hb| now_unix_secs.saturating_sub(hb.last_poll_unix_secs)),
    }
}

/// Builds a [`HistoryReport`] (HORO-1046) from whatever
/// [`crate::monitor::read_history_tail`] already returned. Pure — no I/O;
/// the bounding and malformed-line-skipping already happened in
/// `read_history_tail`, so this only projects each [`HistoryEntry`] into
/// its report DTO.
pub fn build_history_report(entries: &[HistoryEntry]) -> HistoryReport {
    HistoryReport {
        events: entries
            .iter()
            .map(|entry| HistoryEventReport {
                unix_time_secs: entry.unix_time_secs,
                from: entry.from.clone(),
                to: entry.to.clone(),
                used_percent: entry.used_percent,
                free_bytes: entry.free_bytes,
                free_human: crate::reporting::human_bytes(entry.free_bytes),
            })
            .collect(),
    }
}

/// Builds an [`ActionHistoryReport`] (HORO-1057) from whatever
/// [`crate::monitor::read_audit_tail`] already returned. Pure — no I/O;
/// the bounding and malformed-line-skipping already happened in
/// `read_audit_tail`, so this only projects each [`AuditRecord`] into its
/// report DTO.
pub fn build_action_history_report(records: &[AuditRecord]) -> ActionHistoryReport {
    ActionHistoryReport {
        events: records
            .iter()
            .map(|record| ActionHistoryEventReport {
                timestamp: record.timestamp,
                action_id: record.action_id.clone(),
                resource_id: record.resource_id.clone(),
                policy_label: record.policy_label.clone(),
                outcome: record.outcome.clone(),
                abort_reason: record.abort_reason.clone(),
                actual_reclaimed_bytes: record.actual_reclaimed_bytes,
                actual_reclaimed_human: record
                    .actual_reclaimed_bytes
                    .map(crate::reporting::human_bytes),
                source: record.source.clone(),
                model_rank: record.model_rank,
            })
            .collect(),
    }
}

/// Discovers every candidate `registry` finds under `ctx`, refreshes each
/// one's correlation fields via `collector`, and classifies it — the same
/// discover-refresh-classify shape
/// `executor::recovery_loop::select_candidate` uses internally,
/// reimplemented at this thin CLI layer only because that function (and
/// its module-private timeout constant) is not `pub`. Detector-level
/// `ToolAbsent`/`Failed` statuses are silently dropped here exactly as
/// they are in the recovery loop and emergency mode — a tool being absent
/// is normal, expected state, never an error a CLI report needs to
/// surface per-candidate.
pub fn discover_and_classify(
    registry: &DetectorRegistry,
    ctx: &DiscoveryContext,
    collector: &dyn EvidenceCollector,
    policy_cfg: &PolicyConfig,
    now: SystemTime,
) -> Vec<(Evidence, PolicyDecision)> {
    discover_and_classify_with_progress(registry, ctx, collector, policy_cfg, now, |_| {})
}

/// Same discover-refresh-classify pipeline as [`discover_and_classify`] —
/// identical output for identical input — but additionally invokes
/// `on_event` with a [`ProgressEvent`] immediately before and after each
/// detector's probe (HORO-1052), by projecting
/// [`DetectorRegistry::discover_all_with_progress`]'s
/// [`crate::detectors::DetectorProgress`] callback into the
/// presentation-layer `ProgressEvent` DTO. `detect`/`explain`/`llm-plan`
/// are the three subcommands sharing this one discovery phase, so this is
/// the single instrumentation point behind `--progress-json` for all
/// three; [`discover_and_classify`] itself passes a no-op `on_event`, so
/// this refactor changes no observable behavior for any existing caller.
///
/// Returns the candidates only, discarding which detector produced each
/// one and what the detectors that produced none reported. A caller that
/// needs those — `detect`, whose human output prints a line per detector —
/// calls [`discover_and_classify_pass`] instead of running a second,
/// independent discovery pass to recover them (HORO-1487).
pub fn discover_and_classify_with_progress(
    registry: &DetectorRegistry,
    ctx: &DiscoveryContext,
    collector: &dyn EvidenceCollector,
    policy_cfg: &PolicyConfig,
    now: SystemTime,
    on_event: impl FnMut(ProgressEvent),
) -> Vec<(Evidence, PolicyDecision)> {
    discover_and_classify_pass(registry, ctx, collector, policy_cfg, now, on_event).candidates
}

/// What one detector reported during a single discovery pass, once its
/// evidence has moved on into that pass's classified candidate list.
///
/// Deliberately not [`DetectorStatus`]: that type *owns* the
/// `Vec<Evidence>` a `Found` detector produced, and the entire point of
/// [`DiscoveryPass`] is that those very evidences travel on to be
/// correlated and classified rather than being cloned so that a caller can
/// count them afterwards.
///
/// `Failed`'s reason is carried through verbatim rather than flattened
/// into `ToolAbsent`, and since HORO-1484 every serialized surface that
/// reports discovery reports this distinction: `detect --json`'s
/// `detectors` array, the `--progress-json` stream, `free`'s report and
/// `emergency`'s. Before that, this type made the distinction available
/// and nothing consumed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectorOutcome {
    /// The probe succeeded and produced `candidates` evidences, all of
    /// which are in the pass's candidate list.
    Found { candidates: usize },
    /// The detector's tool is not installed, or is installed but
    /// unreachable. Normal, expected state.
    ToolAbsent,
    /// The probe itself failed. Not evidence of "nothing to clean up".
    Failed(String),
}

impl DetectorOutcome {
    /// The tag every serialized surface uses for this outcome.
    ///
    /// One producer for all three strings (HORO-1484), so `detect --json`,
    /// the `--progress-json` stream and the book cannot drift into
    /// describing the same probe with different words.
    pub fn tag(&self) -> &'static str {
        match self {
            DetectorOutcome::Found { .. } => "found",
            DetectorOutcome::ToolAbsent => "tool_absent",
            DetectorOutcome::Failed(_) => "failed",
        }
    }

    /// How many evidences this detector contributed.
    ///
    /// `0` for `ToolAbsent` and for `Failed` — which is the whole reason
    /// [`DetectorOutcome::tag`] exists. A count alone cannot tell a probe
    /// that looked and found nothing from one that never looked.
    pub fn candidates_found(&self) -> usize {
        match self {
            DetectorOutcome::Found { candidates } => *candidates,
            DetectorOutcome::ToolAbsent | DetectorOutcome::Failed(_) => 0,
        }
    }

    /// The probe's own failure message, for `Failed` only.
    pub fn failure_reason(&self) -> Option<&str> {
        match self {
            DetectorOutcome::Failed(reason) => Some(reason),
            DetectorOutcome::Found { .. } | DetectorOutcome::ToolAbsent => None,
        }
    }
}

/// Projects a [`DetectorStatus`] without consuming it — the evidences a
/// `Found` status owns are counted, not cloned.
///
/// The single mapping from status to outcome. Both places a discovery pass
/// needs one go through it: the progress callback, which only borrows the
/// status, and the pass loop below, which then consumes the evidences it
/// just counted. A second hand-written match in either place is how the
/// two halves of one pass would come to disagree about what a detector
/// reported.
impl From<&DetectorStatus> for DetectorOutcome {
    fn from(status: &DetectorStatus) -> Self {
        match status {
            DetectorStatus::Found(evidences) => DetectorOutcome::Found {
                candidates: evidences.len(),
            },
            DetectorStatus::ToolAbsent => DetectorOutcome::ToolAbsent,
            DetectorStatus::Failed(reason) => DetectorOutcome::Failed(reason.clone()),
        }
    }
}

/// One discovery pass's complete result: what every detector reported, and
/// the classified candidates that came out of that same pass.
///
/// The pairing is the whole point (HORO-1487). `detect`'s human output
/// used to print its per-detector status lines from one `discover_all`
/// call and its candidate report from a second, independent one, so a
/// single command's two halves described two different probes of a
/// filesystem that changes underneath them — and every detector ran twice,
/// paying the full probe cost again, to produce that disagreement.
pub struct DiscoveryPass {
    /// Every detector the registry ran, in registration order — including
    /// the ones that contributed no candidates.
    pub detectors: Vec<(DetectorId, DetectorOutcome)>,
    pub candidates: Vec<(Evidence, PolicyDecision)>,
}

/// The discover-refresh-classify pipeline itself: runs every detector in
/// `registry` exactly once and returns both halves of that one pass.
///
/// [`discover_and_classify`] and [`discover_and_classify_with_progress`]
/// are thin wrappers that keep only [`DiscoveryPass::candidates`], so
/// there is one implementation of this pipeline rather than one per
/// caller-visible return shape.
pub fn discover_and_classify_pass(
    registry: &DetectorRegistry,
    ctx: &DiscoveryContext,
    collector: &dyn EvidenceCollector,
    policy_cfg: &PolicyConfig,
    now: SystemTime,
    mut on_event: impl FnMut(ProgressEvent),
) -> DiscoveryPass {
    let discovered = registry.discover_all_with_progress(ctx, |id, progress| match progress {
        DetectorProgress::Started => {
            on_event(ProgressEvent::DetectorStarted { detector: id.0 });
        }
        DetectorProgress::Finished(status) => {
            // A count alone is the collapse this ticket is about
            // (HORO-1484): a probe that errored used to stream the same
            // `candidates_found: 0` as one that looked and found nothing,
            // and the doc comment sent a consumer that needed to tell them
            // apart to "the final report", which did not carry it either.
            // The outcome tag and a failure's reason travel with the count.
            let outcome = DetectorOutcome::from(status);
            on_event(ProgressEvent::DetectorFinished {
                detector: id.0,
                candidates_found: outcome.candidates_found(),
                outcome: outcome.tag(),
                reason: outcome.failure_reason().map(str::to_string),
            });
        }
    });

    let mut detectors = Vec::with_capacity(discovered.len());
    let mut candidates = Vec::new();

    for (id, status) in discovered {
        // Counted before the evidences are consumed, through the same
        // `From` impl the progress callback above used, so the two halves
        // of one pass cannot describe a detector differently.
        let outcome = DetectorOutcome::from(&status);
        detectors.push((id, outcome));
        match status {
            DetectorStatus::Found(evidences) => {
                for mut ev in evidences {
                    let correlation = collector.collect(
                        &ev.resource,
                        ProbeBudget {
                            timeout: CLI_CORRELATION_TIMEOUT,
                        },
                    );
                    merge_into(&mut ev, correlation);
                    ev.collected_at = now;
                    let decision = classify(&ev, policy_cfg, now);
                    candidates.push((ev, decision));
                }
            }
            // Nothing further to do for either: the outcome recorded above
            // already carries which one it was, and a failure's reason.
            DetectorStatus::ToolAbsent | DetectorStatus::Failed(_) => {}
        }
    }

    DiscoveryPass {
        detectors,
        candidates,
    }
}

/// Extracts every repeatable, valued `--project-root <path>` occurrence
/// from `args` (HORO-957): returns the collected roots in the order they
/// appeared, plus every other token from `args` — in its original
/// order, with the `--project-root`/value pairs removed — for the
/// caller's own subcommand-specific parser to keep handling exactly as
/// it does today. A trailing `--project-root` with no following value is
/// reported as an error rather than silently dropped or left for the
/// caller's parser to stumble over, mirroring how `clean`'s own
/// `--target` parsing already reports a missing value.
///
/// Never validates or canonicalizes a collected path — a
/// nonexistent-looking root is passed through as-is; the detectors'
/// own real filesystem probes already degrade gracefully (`ToolAbsent`/
/// empty results) for a root that isn't there.
pub fn extract_project_roots(args: &[String]) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let mut roots = Vec::new();
    let mut remaining = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--project-root" {
            match args.get(i + 1) {
                Some(value) => {
                    roots.push(PathBuf::from(value));
                    i += 2;
                }
                None => return Err("--project-root requires a value".to_string()),
            }
        } else {
            remaining.push(args[i].clone());
            i += 1;
        }
    }
    Ok((roots, remaining))
}

/// Finds the candidate whose [`crate::evidence::ResourceId::to_string`]
/// output, or whose raw filesystem path, matches `query` exactly. Accepts
/// either form per this ticket's `glomeris explain`/`clean --target` AC.
pub fn find_candidate<'a>(
    query: &str,
    candidates: &'a [(Evidence, PolicyDecision)],
) -> Option<&'a (Evidence, PolicyDecision)> {
    candidates.iter().find(|(ev, _)| {
        if ev.resource.to_string() == query {
            return true;
        }
        match &ev.resource.locator {
            ResourceLocator::Path(p) => p.to_string_lossy() == query,
            ResourceLocator::Tool { .. } => false,
        }
    })
}

/// Builds a [`DetectReport`] from already discovered-and-classified
/// candidates. `actions` is consulted only via [`resolve_action_for`]
/// (HORO-1053) to populate each candidate's `executable`/
/// `offered_actions`/`refusal_reason` fields — no policy logic is
/// duplicated here.
///
/// `impact` supplies free-space context for each candidate's
/// `impact_tier` (HORO-1307); pass [`ImpactContext::default`] where the
/// caller has none.
///
/// The returned candidates are in the canonical order documented in
/// [`crate::reporting::ranking`] — biggest reclaimable size first,
/// unmeasured sizes last, fully deterministic. Ordering happens here, at the
/// single point where the report is assembled, so the human-readable output
/// and `--json` cannot present different orders. Before HORO-1307 this
/// returned detector-registration order, which meant a 40 GB build directory
/// could sit below a 2 MB cache.
pub fn build_detect_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
    impact: ImpactContext,
) -> DetectReport {
    let mut candidates: Vec<DetectCandidateReport> = candidates
        .iter()
        .map(|(ev, d)| {
            DetectCandidateReport::from_evidence_and_decision(
                ev,
                d,
                resolve_action_for(ev, actions),
                impact,
            )
        })
        .collect();
    ranking::sort_detect_candidates(&mut candidates);
    DetectReport { candidates }
}

/// Builds an [`ExplainReport`] for one already-classified candidate.
/// `actions` is consulted only via [`resolve_action_for`] (HORO-1053) to
/// populate `executable`/`offered_actions`/`refusal_reason` — no policy
/// logic is duplicated here.
pub fn build_explain_report(
    ev: &Evidence,
    decision: &PolicyDecision,
    actions: &ActionRegistry,
) -> ExplainReport {
    ExplainReport::from_evidence_and_decision(ev, decision, resolve_action_for(ev, actions))
}

/// Resolves the [`Action`] to dry-run for `ev`: prefers
/// [`crate::evidence::model::NativeCleanup::Available`] when it names a
/// real registered action, otherwise falls back to
/// [`ActionRegistry::find_for_kind`] — mirroring
/// `executor::recovery_loop::resolve_action_id`'s exact fallback
/// reasoning (that function is private to its module).
pub fn resolve_action_for<'a>(
    ev: &Evidence,
    actions: &'a ActionRegistry,
) -> Option<&'a dyn Action> {
    if let NativeCleanup::Available(id) = ev.native_cleanup {
        if let Some(action) = actions.get(id.0) {
            return Some(action);
        }
    }
    actions.find_for_kind(ev.resource.kind)
}

/// Builds one [`CleanDryRunItem`] for `ev`/`decision`: resolves an action
/// and renders its [`crate::actions::ActionPlan::explain`] via
/// [`crate::actionability::dry_run_explain`] — never executing anything.
/// Three things are reported with `skip_reason` set rather than silently
/// omitted or, worse, rendered as a proposal: a resource with no resolvable
/// action, one whose `Action::plan` refuses (e.g.
/// `ActionError::Unsupported`), and one whose plan builds but which
/// execution would refuse on sight (HORO-1359).
///
/// SAFETY-CRITICAL: a [`PolicyClass::Protected`] decision short-circuits
/// here, before `resolve_action_for`/`dry_run` ever run — this is what
/// makes "Protected items never render an executable cleanup action" (this
/// ticket's AC) true even for `clean --dry-run --target <protected>`,
/// which bypasses `build_clean_dry_run_report`'s own `AutoSafe | Ask`
/// filter (that filter only applies to the no-`--target` "dry-run
/// everything" path).
pub fn build_clean_dry_run_item(
    ev: &Evidence,
    decision: &PolicyDecision,
    actions: &ActionRegistry,
) -> CleanDryRunItem {
    let policy_label = crate::reporting::label_for(decision).as_str();
    let resource_id = ev.resource.to_string();

    if decision.class == PolicyClass::Protected {
        return CleanDryRunItem {
            resource_id,
            policy_label,
            action_id: None,
            explain: None,
            skip_reason: Some(
                "PROTECTED — no cleanup action is ever rendered for this resource".to_string(),
            ),
        };
    }

    match resolve_action_for(ev, actions) {
        // `dry_run_explain` rather than `dry_run` (HORO-1359). A plan built
        // successfully is not the same claim as a plan execution would run:
        // `homebrew.cleanup.cache` plans fine and is then refused on sight,
        // so this used to print `Run \`brew cleanup -s\`…` as a live
        // proposal for a resource `detect --json` was simultaneously
        // reporting as `executable: false`. The refusal is now the row.
        Some(action) => match crate::actionability::dry_run_explain(action, ev) {
            Ok(explain) => CleanDryRunItem {
                resource_id,
                policy_label,
                action_id: Some(action.id().0),
                explain: Some(explain),
                skip_reason: None,
            },
            // `action_id` stays set: an action really was resolved for this
            // resource, and naming the one that refused is more use than
            // implying none was found.
            Err(reason) => CleanDryRunItem {
                resource_id,
                policy_label,
                action_id: Some(action.id().0),
                explain: None,
                skip_reason: Some(reason),
            },
        },
        None => CleanDryRunItem {
            resource_id,
            policy_label,
            action_id: None,
            explain: None,
            skip_reason: Some("no registered action for this resource kind".to_string()),
        },
    }
}

/// Builds a [`CleanDryRunReport`]: without `target`, dry-runs every
/// [`PolicyClass::AutoSafe`]/[`PolicyClass::Ask`] candidate (skipping
/// `Protected`, which is never actionable); with `target`, dry-runs just
/// the one candidate [`find_candidate`] resolves it to, erroring if none
/// matches. Never executes anything — see [`build_clean_dry_run_item`].
pub fn build_clean_dry_run_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
    target: Option<&str>,
) -> Result<CleanDryRunReport, String> {
    let selected: Vec<&(Evidence, PolicyDecision)> = match target {
        Some(query) => {
            let found = find_candidate(query, candidates)
                .ok_or_else(|| format!("no discoverable candidate matches '{query}'"))?;
            vec![found]
        }
        None => candidates
            .iter()
            .filter(|(_, d)| matches!(d.class, PolicyClass::AutoSafe | PolicyClass::Ask))
            .collect(),
    };

    Ok(CleanDryRunReport {
        items: selected
            .into_iter()
            .map(|(ev, d)| build_clean_dry_run_item(ev, d, actions))
            .collect(),
    })
}

/// Extracts a single valued, optional `--plan-file <path>` occurrence from
/// `args` (HORO-1008): returns the path (if present) plus every other
/// token from `args`, in original order, with the `--plan-file`/value pair
/// removed — same shape as [`extract_project_roots`], but for a flag that
/// may appear at most once. A trailing `--plan-file` with no following
/// value is reported as an error rather than silently dropped, mirroring
/// `extract_project_roots`'s own handling of a missing value.
pub fn extract_plan_file(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut plan_file = None;
    let mut remaining = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--plan-file" {
            match args.get(i + 1) {
                Some(value) => {
                    plan_file = Some(PathBuf::from(value));
                    i += 2;
                }
                None => return Err("--plan-file requires a value".to_string()),
            }
        } else {
            remaining.push(args[i].clone());
            i += 1;
        }
    }
    Ok((plan_file, remaining))
}

/// Extracts the flag *name* from a `glomeris llm-plan` argument token,
/// stripping off an `=`-joined value if present — `"--api-key=sk-..."` and
/// `"--api-key"` both yield `"--api-key"`. Used to reject a credential
/// flag by name only, so the token's value is never echoed into an error
/// message regardless of which of the two forms the caller used.
pub fn credential_flag_name(arg: &str) -> &str {
    arg.split_once('=').map_or(arg, |(name, _)| name)
}

/// Builds an [`LlmPlanReport`] from already discovered-and-classified
/// `candidates`, calling `plan_with_llm` and then resolving each validated
/// item exactly the way [`build_clean_dry_run_item`] resolves a rule-only
/// candidate.
///
/// **ADVISORY ONLY — never executes anything and never authorizes
/// anything.** This function never constructs a
/// [`crate::policy::Approval`] and never calls
/// [`crate::policy::approval::authorize`] or
/// [`crate::executor::execute`] — see `crate::actions::llm`'s module docs
/// for why an LLM-proposed plan must never reach execution directly.
///
/// SAFETY-CRITICAL: a [`PolicyClass::Protected`] decision short-circuits
/// here, BEFORE `dry_run` is ever called for that item — this is what makes
/// it structurally impossible for a hallucinating (or adversarial) LLM
/// response to cause a protected resource's action to be *planned*, let
/// alone rendered. See `tests/golden_llm_plan_protected_refusal.rs` for an
/// end-to-end proof.
///
/// HORO-1308 narrowed that sentence by one word, deliberately and with no
/// loss: it used to say "before `actions.get`/`dry_run`". Every item now
/// also carries the same [`DetectCandidateReport`] projection `glomeris
/// detect` prints, and building it calls [`resolve_action_for`] — a registry
/// lookup — for protected resources too, exactly as `detect` already does
/// for them. For a protected decision that lookup's result is *discarded
/// unused*: `executable_fields` tests `PolicyClass::Protected` first and
/// returns `(false, [], Some("PROTECTED: …"))` whatever action was resolved.
///
/// HORO-1358 gave `executable_fields` a reason to call [`Action::plan`] — it
/// now refuses to report a candidate executable when the executor would
/// refuse its plan on sight. That call sits *after* the `Protected` early
/// return, so the narrowing above is unaffected: for a protected resource
/// nothing is planned and no filesystem is touched, and the property that
/// matters — no action is ever offered for a protected resource, however
/// insistently a model asks — is unchanged and is asserted directly in the
/// golden test. A reorder would in fact be caught there too, since the
/// golden fixture's action cannot plan for its path and the test asserts the
/// refusal `starts_with("PROTECTED: ")` — but only incidentally, as a
/// consequence of that one fixture. Keep the `Protected` check first on its
/// own merits: it, not a test, is what guarantees nothing is planned.
///
/// `impact` is the free-space context for the nested candidates' impact
/// tiers; pass [`ImpactContext::default`] where it is genuinely unknown.
pub fn build_llm_plan_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
    provider: &dyn LlmProvider,
    impact: ImpactContext,
) -> LlmPlanReport {
    let result = plan_with_llm(provider, candidates, actions);

    let mut items = Vec::with_capacity(result.validated_items.len());
    for validated in result.validated_items {
        let Some((ev, decision)) = candidates
            .iter()
            .find(|(ev, _)| ev.resource == validated.resource)
        else {
            // Unreachable in practice: `resource` came from `evidences`,
            // which is itself derived from `candidates` — but never panic
            // on a defensive fallback, matching this module's style.
            continue;
        };

        let resource_id = ev.resource.to_string();
        let policy_label = crate::reporting::label_for(decision).as_str();
        let priority = validated.priority;
        let model_reason = validated.model_reason;
        // Built from `ev` and `decision` only — the local evidence and the
        // real policy verdict. Note what is NOT an input: anything from
        // `validated`. The model cannot influence `executable`,
        // `offered_actions` or `refusal_reason` even by naming a different
        // action than the one policy would resolve.
        let candidate = DetectCandidateReport::from_evidence_and_decision(
            ev,
            decision,
            resolve_action_for(ev, actions),
            impact,
        );
        let completeness = crate::reporting::dto::completeness_tag(&ev.completeness());
        let confidence = crate::reporting::dto::confidence_tag(ev.confidence());

        if decision.class == PolicyClass::Protected {
            items.push(LlmPlanItemReport {
                resource_id,
                policy_label,
                requested_action_id: None,
                priority,
                model_reason,
                explain: None,
                skip_reason: Some(
                    "PROTECTED — no cleanup action is ever rendered for this resource".to_string(),
                ),
                candidate,
                completeness,
                confidence,
            });
            continue;
        }

        items.push(match actions.get(validated.action_id.0) {
            // `dry_run_explain` rather than `dry_run` (HORO-1359). The
            // commonest refusal here is still a model naming a registered
            // action that does not apply to the resource it named, which is
            // the plan-error arm; the arm this change adds is the one where
            // the plan builds and execution would refuse the step anyway. In
            // that case `candidate.executable` was already `false` with a
            // truthful `refusal_reason`, while this row's own `explain`
            // rendered the action as a live proposal — one JSON object
            // disagreeing with itself.
            Some(action) => match crate::actionability::dry_run_explain(action, ev) {
                Ok(explain) => LlmPlanItemReport {
                    resource_id,
                    policy_label,
                    requested_action_id: Some(action.id().0),
                    priority,
                    model_reason,
                    explain: Some(explain),
                    skip_reason: None,
                    candidate,
                    completeness,
                    confidence,
                },
                Err(reason) => LlmPlanItemReport {
                    resource_id,
                    policy_label,
                    requested_action_id: Some(action.id().0),
                    priority,
                    model_reason,
                    explain: None,
                    skip_reason: Some(reason),
                    candidate,
                    completeness,
                    confidence,
                },
            },
            None => LlmPlanItemReport {
                resource_id,
                policy_label,
                requested_action_id: None,
                priority,
                model_reason,
                explain: None,
                skip_reason: Some("no registered action for this action id".to_string()),
                candidate,
                completeness,
                confidence,
            },
        });
    }

    LlmPlanReport {
        items,
        dropped_unknown_resource: result.dropped_unknown_resource,
        dropped_unknown_action: result.dropped_unknown_action,
        // `Display`, not `Debug`: `LlmError`'s `Display` renders a
        // human-readable sentence naming the HTTP status, API style,
        // endpoint path and redacted provider message, whereas `Debug`
        // renders Rust struct syntax. Both are secret-safe (see
        // `api_key_never_appears_in_display_output_of_any_variant` in
        // `actions::llm`'s tests), but only one is readable in a terminal
        // or a `--json` field.
        provider_error: result.provider_error.map(|e| e.to_string()),
    }
}

/// Builds an [`LlmPayloadReport`] — the exact request a live `llm-plan`
/// run would send for `candidates`, without sending it and without needing
/// a provider or a credential (HORO-1298).
///
/// Calls [`build_request_payload`], the same function [`plan_with_llm`]
/// calls, rather than reconstructing the payload: a `--print-payload`
/// output that could drift from the real request would be worse than no
/// output at all, since its whole purpose is letting an operator verify
/// what leaves their machine.
pub fn build_llm_payload_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
) -> Result<LlmPayloadReport, crate::actions::llm::LlmError> {
    let payload = build_request_payload(candidates, actions)?;

    Ok(LlmPayloadReport {
        system_prompt: payload.system_prompt.to_string(),
        user_prompt: payload.user_prompt.clone(),
        resource_aliases: payload
            .aliases()
            .iter()
            .map(|(wire_resource_id, resource)| LlmPayloadResourceAlias {
                wire_resource_id: wire_resource_id.clone(),
                local_resource_id: resource.to_string(),
            })
            .collect(),
    })
}

/// Prints an [`LlmPayloadReport`] as human-readable text, with the two
/// outbound strings and the local alias table under separate headings —
/// the separation is the point, so it must be visible and not require
/// reading `--json` to see.
pub fn print_llm_payload_report(report: &LlmPayloadReport) {
    println!("LLM REQUEST PAYLOAD — nothing is sent by this command");
    println!();
    println!("-- sent: system_prompt --");
    println!("{}", report.system_prompt);
    println!();
    println!("-- sent: user_prompt --");
    println!("{}", report.user_prompt);
    println!();
    println!(
        "-- NOT sent: local resource aliases ({}) --",
        report.resource_aliases.len()
    );
    if report.resource_aliases.is_empty() {
        println!("(no resources discovered)");
        return;
    }
    for alias in &report.resource_aliases {
        println!("{} = {}", alias.wire_resource_id, alias.local_resource_id);
    }
}

/// How many characters of the provider's reply survive into
/// [`LlmCheckReport::response_excerpt`] (HORO-1309). Far smaller than
/// `PROVIDER_ERROR_BODY_LIMIT` because a correct answer to
/// [`CONNECTION_TEST_USER_PROMPT`] is two letters; this only needs to be wide
/// enough that a gateway answering with a refusal sentence instead is
/// readable rather than clipped to nothing.
const CHECK_RESPONSE_EXCERPT_LIMIT: usize = 160;

/// Builds an [`LlmCheckReport`] (HORO-1309) by performing one real, trivial
/// completion through `provider`.
///
/// ## Why this goes through the provider rather than around it
///
/// The point of a connection test is that passing it means a real request
/// will work. So this sends through the same [`LlmProvider::complete`] a
/// plan does, which means the same URL construction, the same
/// `Authorization: Bearer` scheme, the same body keys, the same two message
/// roles and the same response parsing. A leaner bespoke request — a `GET`
/// of the base URL, say, or a `/models` probe — would be cheaper and would
/// also be able to pass on a setup where planning fails, which is the one
/// outcome a connection test must never produce.
///
/// What differs from a plan is only the payload: two fixed literals that
/// describe nothing about this machine (see
/// [`CONNECTION_TEST_SYSTEM_PROMPT`]). No evidence is collected, no resource
/// is named, and discovery never runs.
///
/// `model` and `endpoint_path` are passed in rather than read off the
/// provider because [`LlmProvider`] is a trait with neither — which is also
/// what lets this function be tested against a fake provider for every
/// failure class without a network.
pub fn build_llm_check_report(
    provider: &dyn LlmProvider,
    model: &str,
    endpoint_path: &str,
) -> LlmCheckReport {
    use crate::actions::llm::{
        excerpt, llm_check_outcome, CONNECTION_TEST_SYSTEM_PROMPT, CONNECTION_TEST_USER_PROMPT,
    };

    let report = |outcome, error, response_excerpt| LlmCheckReport {
        outcome,
        model: model.to_string(),
        endpoint_path: endpoint_path.to_string(),
        error,
        response_excerpt,
    };

    match provider.complete(CONNECTION_TEST_SYSTEM_PROMPT, CONNECTION_TEST_USER_PROMPT) {
        Ok(reply) => report(
            llm_check_outcome(None),
            None,
            Some(excerpt(&reply, CHECK_RESPONSE_EXCERPT_LIMIT)),
        ),
        // The token comes from `llm_check_outcome`, the sole producer, so the
        // macOS app's wording for these five states can be diffed against it
        // mechanically. The prose alongside it — already secret-free, and
        // asserted so per variant in `actions::llm`'s tests — carries the
        // detail a human needs to fix it.
        Err(e) => report(llm_check_outcome(Some(&e)), Some(e.to_string()), None),
    }
}

/// Prints an [`LlmCheckReport`] as human-readable text. The endpoint path is
/// always shown, success or failure: it is the field that answers "did I
/// configure the API root or the host root", and that question is worth
/// answering while things work too.
pub fn print_llm_check_report(report: &LlmCheckReport) {
    match report.outcome {
        "ok" => println!("LLM CONNECTION OK"),
        _ => println!("LLM CONNECTION FAILED ({})", report.outcome),
    }
    println!("model: {}", report.model);
    println!("endpoint path: {}", report.endpoint_path);
    if let Some(excerpt) = &report.response_excerpt {
        println!("provider replied: {excerpt}");
    }
    if let Some(error) = &report.error {
        println!("error: {error}");
    }
}

/// Renders an example, syntactically valid `LlmPlan` JSON document
/// (HORO-1048) — `glomeris llm-plan --schema`'s entire stdout, and the
/// literal text embedded in `book/src/byok.md`'s schema example. Built
/// directly as a `serde_json::Value` rather than by serializing a real
/// [`crate::actions::llm::LlmPlan`] — that type deliberately has no
/// `Serialize` derive (see `actions::llm`'s module doc comment on why it
/// and `LlmPlanItem` are the crate's only external-input parse targets),
/// and this function must never need one. `llm_plan_schema_example_round_trips`
/// below proves the string this returns parses back into a real
/// `LlmPlan` unchanged, and `tests/llm_plan_schema_round_trip.rs` proves
/// the same thing through the actual `glomeris llm-plan --plan-file`
/// input surface.
///
/// The example's `resource_id` is a `ResourceId::to_string()`-shaped value
/// because this document's audience is a human hand-writing a
/// `--plan-file` fixture, and that is the form `glomeris detect` prints.
/// A live model never sees such a value — it is handed positional wire
/// aliases instead (HORO-1298, see `crate::actions::llm`'s module docs) —
/// and both forms resolve. The placeholder path is deliberately not
/// home-shaped: a `/Users/<name>/...` example invited exactly the
/// assumption HORO-1298 was about, that real local paths are what crosses
/// the wire. It is also deliberately a path that no real discovery run
/// will match, which `tests/llm_plan_schema_round_trip.rs` depends on.
pub fn llm_plan_schema_example() -> String {
    let value = serde_json::json!({
        "items": [
            {
                "resource_id": "cargo_target_dir:/path/to/project/target",
                "action_id": "cargo.clean.target_dir",
                "priority": 1,
                "reason": "stale build artifacts, not modified in 30 days"
            }
        ]
    });
    serde_json::to_string_pretty(&value).expect("static example JSON value always serializes")
}

/// Builds an [`ActionListReport`] (HORO-1047) by enumerating every action
/// [`ActionRegistry::actions`] actually returns — never a hand-maintained
/// list — so registering a new action in [`ActionRegistry::builtin`]
/// requires no change to this function, `glomeris actions list --json`, or
/// this DTO's contents.
pub fn build_action_list_report(actions: &ActionRegistry) -> ActionListReport {
    ActionListReport {
        actions: actions
            .actions()
            .map(|action| ActionListItem {
                action_id: action.id().0,
                applies_to: action.applies_to().iter().map(|kind| kind.tag()).collect(),
            })
            .collect(),
    }
}

/// Prints an [`ActionListReport`] as concise, human-readable text.
pub fn print_action_list_report(report: &ActionListReport) {
    if report.actions.is_empty() {
        println!("no actions registered");
        return;
    }
    for item in &report.actions {
        println!(
            "{:<28} applies_to={}",
            item.action_id,
            item.applies_to.join(",")
        );
    }
}

/// Prints a [`StatusReport`] as concise, human-readable text.
pub fn print_status_report(report: &StatusReport) {
    println!("disk pressure state: {}", report.pressure_state);
    println!(
        "used: {:.1}%   free: {} ({} bytes)   total: {} ({} bytes)",
        report.used_percent,
        report.free_human,
        report.free_bytes,
        report.total_human,
        report.total_bytes
    );
}

/// Prints a [`DaemonStatusReport`] as concise, human-readable text.
/// Includes the heartbeat line alongside install/load state — the whole
/// point of HORO-1045 is making heartbeat staleness visible in the default
/// text output, not only to `--json` consumers.
pub fn print_daemon_status_report(report: &DaemonStatusReport) {
    println!("plist installed: {}", report.plist_installed);
    println!("plist path: {}", report.plist_path);
    println!("loaded in launchd: {}", report.loaded);
    match report.heartbeat_age_secs {
        Some(age) => println!("last poll: {age}s ago"),
        None => println!("last poll: never"),
    }
}

/// Prints a [`HistoryReport`] as concise, human-readable text.
pub fn print_history_report(report: &HistoryReport) {
    if report.events.is_empty() {
        println!("no pressure history recorded");
        return;
    }
    for event in &report.events {
        println!(
            "{}\t{} -> {}\t{:.2}%\tfree {}",
            event.unix_time_secs, event.from, event.to, event.used_percent, event.free_human
        );
    }
}

/// Prints an [`ActionHistoryReport`] as concise, human-readable text.
pub fn print_action_history_report(report: &ActionHistoryReport) {
    if report.events.is_empty() {
        println!("no action history recorded");
        return;
    }
    for event in &report.events {
        let reclaimed = event
            .actual_reclaimed_human
            .as_deref()
            .unwrap_or("unavailable");
        print!(
            "{}\t{}\t{}\t{}\t{}\t{}\treclaimed {}",
            event.timestamp,
            event.source,
            event.action_id,
            event.resource_id,
            event.policy_label,
            event.outcome,
            reclaimed
        );
        if let Some(reason) = &event.abort_reason {
            print!("\tabort: {reason}");
        }
        println!();
    }
}

/// Renders a human-readable byte-count string for text output, prefixing
/// it with `≥` and an explicit "scan truncated" note when the underlying
/// evidence marked it as a lower bound (HORO-1049) — `"unknown"` when no
/// size was observed at all, matching this module's existing
/// `unwrap_or("unknown")` convention.
fn format_size_field(human: Option<&str>, is_lower_bound: bool) -> String {
    match (human, is_lower_bound) {
        (Some(human), true) => format!("≥ {human} (lower bound — scan truncated)"),
        (Some(human), false) => human.to_string(),
        (None, _) => "unknown".to_string(),
    }
}

/// Prints a [`DetectReport`] as concise, human-readable text.
pub fn print_detect_report(report: &DetectReport) {
    if report.candidates.is_empty() {
        println!("no candidates discovered");
        return;
    }
    for c in &report.candidates {
        let size = format_size_field(
            c.reclaimable_human.as_deref(),
            c.reclaimable_bytes_is_lower_bound,
        );
        println!(
            "[{}] {:<28} {:<10} reclaimable(est.)={:<10} reasons={}",
            c.policy_label,
            c.kind,
            c.resource_id,
            size,
            c.reasons.join(",")
        );
    }
}

/// Prints an [`ExplainReport`] as a multi-line human-readable explanation.
pub fn print_explain_report(report: &ExplainReport) {
    println!("resource:      {}", report.resource_id);
    println!("kind:          {}", report.kind);
    println!("detector:      {}", report.detector);
    if report.sources.is_empty() {
        println!("provenance:    none recorded");
    } else {
        println!("provenance:");
        for source in &report.sources {
            println!("  - {source}");
        }
    }
    println!(
        "logical size:  {}",
        format_size_field(
            report.logical_human.as_deref(),
            report.reclaimable_bytes_is_lower_bound
        )
    );
    println!(
        "reclaimable:   {} (estimate, not a measured result)",
        format_size_field(
            report.reclaimable_human.as_deref(),
            report.reclaimable_bytes_is_lower_bound
        )
    );
    println!("completeness:  {}", report.completeness);
    println!("confidence:    {}", report.confidence);
    println!("regenerable:   {}", report.regenerability);
    if report.active_use_signals.is_empty() {
        println!("active use:    none observed");
    } else {
        println!("active use:");
        for signal in &report.active_use_signals {
            println!("  - {signal}");
        }
    }
    println!(
        "native cleanup: {}",
        match report.native_cleanup_action_id {
            Some(id) => format!("available ({id})"),
            None => "unsupported".to_string(),
        }
    );
    println!(
        "policy:        [{}] {}",
        report.policy_label,
        report.reasons.join(", ")
    );
}

/// Prints a [`CleanDryRunReport`] as concise, human-readable text. Always
/// opens with an explicit "DRY RUN" banner — this ticket's AC requires
/// dry-run and real-execution output to be visually/semantically distinct,
/// and `free --target`'s real-execution report
/// (`main.rs::print_recovery_report`) never prints this banner.
pub fn print_clean_dry_run_report(report: &CleanDryRunReport) {
    println!("DRY RUN — nothing is executed by this command");
    if report.items.is_empty() {
        println!("no candidates to dry-run");
        return;
    }
    for item in &report.items {
        println!("[{}] {}", item.policy_label, item.resource_id);
        match (&item.explain, &item.skip_reason) {
            (Some(explain), _) => println!("  {explain}"),
            (None, Some(reason)) => println!("  skipped: {reason}"),
            (None, None) => println!("  (no plan rendered)"),
        }
    }
}

/// Prints an [`LlmPlanReport`] as concise, human-readable text. Always
/// opens with an explicit advisory banner — this command never executes
/// anything, and its output must never be mistaken for `clean --dry-run`'s
/// or `free --target`'s output.
pub fn print_llm_plan_report(report: &LlmPlanReport) {
    println!("LLM SUGGESTION — advisory only, nothing is executed by this command");
    if let Some(err) = &report.provider_error {
        println!("provider error: {err}");
    }
    if report.dropped_unknown_resource > 0 || report.dropped_unknown_action > 0 {
        println!(
            "dropped: {} unknown resource(s), {} unknown action(s)",
            report.dropped_unknown_resource, report.dropped_unknown_action
        );
    }
    if report.items.is_empty() {
        println!("no suggestions");
        return;
    }
    for item in &report.items {
        let action = item.requested_action_id.unwrap_or("(none)");
        let priority = item
            .priority
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "[{}] {} action={} priority={}",
            item.policy_label, item.resource_id, action, priority
        );
        // Attributed, and printed before the explain line rather than after
        // it: "the model says" has to arrive before the sentence it
        // qualifies, or the reader has already taken the claim as ours. The
        // `model says:` prefix is not decoration — it is the only thing
        // distinguishing a provider's assertion from this machine's finding
        // on the line below (HORO-1308).
        if let Some(reason) = &item.model_reason {
            println!("  model says: {reason}");
        }
        match (&item.explain, &item.skip_reason) {
            (Some(explain), _) => println!("  {explain}"),
            (None, Some(reason)) => println!("  skipped: {reason}"),
            (None, None) => println!("  (no plan rendered)"),
        }
    }
}

/// Outcome of resolving `glomeris execute`'s `--resource-id`/`--action-id`
/// selectors and, when authorized, actually running
/// [`crate::executor::execute`] (HORO-1055). `main.rs` maps each variant
/// to this subcommand's own typed exit code; this type carries no exit
/// code itself, keeping [`resolve_and_execute`] portable/testable.
///
/// `Debug` is implemented by hand rather than derived: `ExecutionReport`
/// (the `Executed` payload) doesn't derive `Debug` itself, and adding
/// that derive belongs to `executor/mod.rs` — deliberately kept at zero
/// diff by this ticket (see the PR description's AC 6).
pub enum ExecuteResolution {
    /// No discovered candidate matches the supplied `--resource-id`.
    ResourceNotFound,
    /// The resolved resource has no registered action at all.
    ActionNotFound,
    /// A resource DID resolve to a registered action, but its id differs
    /// from the caller's `--action-id` — the caller asked for a specific
    /// action, not "whatever the system thinks is right", so this refuses
    /// rather than silently substituting `resolved`.
    ActionMismatch {
        requested: String,
        resolved: &'static str,
    },
    /// `policy::approval::authorize` returned `None` for a `Protected`
    /// decision — refuses unconditionally, regardless of any flag
    /// combination. This is `authorize`'s own logic; this variant only
    /// labels which of its `None` cases fired.
    RefusedProtected,
    /// `authorize` returned `None` for an `Ask` decision because no
    /// [`UserConsent`] was built at all — the caller never passed
    /// `--confirm-ask`/`--observed-fingerprint`.
    RefusedAskNoConsent,
    /// `authorize` returned `None` for an `Ask` decision even though a
    /// [`UserConsent`] was supplied — the observed fingerprint the caller
    /// supplied did not match the freshly observed one (stale or for a
    /// different resource instance).
    RefusedAskConsentMismatch,
    /// `authorize` returned `None` for an `AutoSafe` decision. Per
    /// `authorize`'s own doc comment this never actually happens
    /// (`AutoSafe` always authorizes) — this variant exists only as an
    /// explicit, fail-closed label for that impossible case, never as an
    /// expected outcome.
    RefusedAutoSafeContractViolation,
    /// Authorization succeeded and [`crate::executor::execute`] ran.
    /// `report.outcome` still distinguishes success/failure/abort — see
    /// [`build_execute_report`].
    Executed(ExecutionReport),
}

impl std::fmt::Debug for ExecuteResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResourceNotFound => write!(f, "ResourceNotFound"),
            Self::ActionNotFound => write!(f, "ActionNotFound"),
            Self::ActionMismatch {
                requested,
                resolved,
            } => write!(
                f,
                "ActionMismatch {{ requested: {requested:?}, resolved: {resolved:?} }}"
            ),
            Self::RefusedProtected => write!(f, "RefusedProtected"),
            Self::RefusedAskNoConsent => write!(f, "RefusedAskNoConsent"),
            Self::RefusedAskConsentMismatch => write!(f, "RefusedAskConsentMismatch"),
            Self::RefusedAutoSafeContractViolation => {
                write!(f, "RefusedAutoSafeContractViolation")
            }
            // `ExecutionReport` doesn't derive `Debug` (see this enum's
            // doc comment) — render its outcome tag via the already-hand-
            // projected `ExecuteReport` DTO instead of duplicating that
            // projection here.
            Self::Executed(report) => {
                write!(f, "Executed({:?})", build_execute_report(report))
            }
        }
    }
}

/// Resolves `resource_id`/`action_id` against already discovered-and-
/// classified `candidates`, then — only when authorization succeeds —
/// calls the real, completely unmodified [`execute`]. HORO-1055's actual
/// enforcement surface.
///
/// `observed_fingerprint_from_token` is the ONLY input a [`UserConsent`]
/// is ever built from here, and it must already be the result of decoding
/// the caller's own `--observed-fingerprint` token via
/// [`crate::evidence::decode_fingerprint_token`] — see this function's
/// only caller, `main.rs`'s `run_execute_command`, for that decoding (and
/// the `--confirm-ask`/`--observed-fingerprint` usage validation) that
/// must happen first. This function never substitutes the
/// freshly-observed `ev.fingerprint` for a missing token — doing so would
/// silently defeat the whole fingerprint-pinning purpose the ticket calls
/// out, since it would let a caller "confirm" a resource without ever
/// having been shown its actual identity.
///
/// `#[allow(clippy::too_many_arguments)]`: nine parameters, every one an
/// independently fakeable seam (candidates/actions/collector/cfg/now are
/// exactly what makes this function unit-testable without touching the
/// real filesystem or clock) — precedented in this crate at
/// `emergency::run_emergency` and `detectors::discovery_evidence`.
/// `audit_log_path` (HORO-1057) is one more such seam: a plain `&Path`
/// rather than an injected backend, matching how `main.rs` already passes
/// `self_state_path`/`history_path` around as plain paths for this same
/// reason.
#[allow(clippy::too_many_arguments)]
pub fn resolve_and_execute(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
    resource_id: &str,
    action_id: &str,
    observed_fingerprint_from_token: Option<ResourceFingerprint>,
    collector: &dyn EvidenceCollector,
    cfg: &PolicyConfig,
    now: SystemTime,
    audit_log_path: &Path,
) -> ExecuteResolution {
    let Some((ev, decision)) = find_candidate(resource_id, candidates) else {
        return ExecuteResolution::ResourceNotFound;
    };

    let Some(action) = resolve_action_for(ev, actions) else {
        return ExecuteResolution::ActionNotFound;
    };
    if action.id().0 != action_id {
        return ExecuteResolution::ActionMismatch {
            requested: action_id.to_string(),
            resolved: action.id().0,
        };
    }

    let consent =
        observed_fingerprint_from_token.map(|fp| UserConsent::new(ev.resource.clone(), fp, now));
    let policy_label = label_for(decision).as_str();

    match authorize(decision.clone(), ev.fingerprint.clone(), consent.as_ref()) {
        Some(approval) => {
            let report = execute(action, &approval, collector, cfg, now);
            // HORO-1057: best-effort audit-log append, AFTER the real
            // outcome is already known. `record_audit`'s `Result` is
            // never inspected here — see that function's doc comment for
            // why an audit-write failure must never change `report`'s
            // own outcome, which is exactly what's returned below
            // regardless of whether this line succeeded.
            record_audit(
                &report,
                policy_label,
                ActionSource::Execute,
                audit_log_path,
                now,
            );
            ExecuteResolution::Executed(report)
        }
        None => match decision.class {
            PolicyClass::Protected => ExecuteResolution::RefusedProtected,
            PolicyClass::Ask if consent.is_none() => ExecuteResolution::RefusedAskNoConsent,
            PolicyClass::Ask => ExecuteResolution::RefusedAskConsentMismatch,
            PolicyClass::AutoSafe => ExecuteResolution::RefusedAutoSafeContractViolation,
        },
    }
}

/// Builds an [`AuditRecord`] (HORO-1057) from a real [`ExecutionReport`]
/// and appends it to `audit_log_path`, unconditionally discarding the
/// `Result` — matching [`crate::monitor::append_audit_record`]'s
/// best-effort contract: no caller of this function may ever propagate,
/// log as a warning that changes control flow, or otherwise let an
/// audit-write failure influence the real execution's already-decided
/// outcome. Never called for [`ExecutionOutcome::DryRun`]'s tag, since no
/// real-execution call site (`execute`/`free`/`emergency`) ever produces
/// it — see [`build_execute_report`]'s own comment on that same variant.
fn record_audit(
    report: &ExecutionReport,
    policy_label: &'static str,
    source: ActionSource,
    audit_log_path: &Path,
    now: SystemTime,
) {
    let (outcome, abort_reason) = match &report.outcome {
        ExecutionOutcome::Succeeded => ("succeeded", None),
        ExecutionOutcome::Failed(_) => ("failed", None),
        ExecutionOutcome::AbortedByRevalidation(reason) => {
            ("aborted_by_revalidation", Some(format!("{reason:?}")))
        }
        ExecutionOutcome::DryRun => ("dry_run", None),
    };
    let record = AuditRecord {
        timestamp: now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        action_id: report.action.0.to_string(),
        resource_id: report.resource.to_string(),
        policy_label: policy_label.to_string(),
        outcome: outcome.to_string(),
        abort_reason,
        actual_reclaimed_bytes: report.actual_reclaimed_bytes.observed().copied(),
        source: source.to_string(),
        // `execute`/`free` act on a resource the user named, never on a
        // model's ranking — only `autopilot` ever sets this.
        model_rank: None,
    };
    let _ = crate::monitor::append_audit_record(audit_log_path, &record);
}

/// Projects a real [`ExecutionReport`] into the [`ExecuteReport`] DTO.
/// Only ever called for [`ExecuteResolution::Executed`] — never
/// duplicates `execute`'s own outcome logic, only renders it.
pub fn build_execute_report(report: &ExecutionReport) -> ExecuteReport {
    let (outcome, failure_message, abort_reason) = match &report.outcome {
        ExecutionOutcome::Succeeded => ("succeeded", None, None),
        ExecutionOutcome::Failed(message) => ("failed", Some(message.clone()), None),
        ExecutionOutcome::AbortedByRevalidation(reason) => {
            ("aborted_by_revalidation", None, Some(format!("{reason:?}")))
        }
        ExecutionOutcome::DryRun => ("dry_run", None, None),
    };

    let expected_reclaimed_bytes = report.expected_reclaimed_bytes.observed().copied();
    let actual_reclaimed_bytes = report.actual_reclaimed_bytes.observed().copied();

    ExecuteReport {
        action_id: report.action.0,
        resource_id: report.resource.to_string(),
        outcome,
        failure_message,
        abort_reason,
        expected_reclaimed_bytes,
        actual_reclaimed_bytes,
        // Rendered here rather than by the caller: `human_bytes` is
        // 1024-based, and a client guessing otherwise renders a different
        // number for the same bytes (HORO-1312). `None` stays `None` — an
        // unavailable probe is not zero bytes, and "0 B" would claim it was.
        expected_reclaimed_human: expected_reclaimed_bytes.map(crate::reporting::human_bytes),
        actual_reclaimed_human: actual_reclaimed_bytes.map(crate::reporting::human_bytes),
    }
}

/// Prints an [`ExecuteReport`] as concise, human-readable text.
pub fn print_execute_report(report: &ExecuteReport) {
    println!("action:              {}", report.action_id);
    println!("resource:            {}", report.resource_id);
    println!("outcome:             {}", report.outcome);
    if let Some(message) = &report.failure_message {
        println!("failure:             {message}");
    }
    if let Some(reason) = &report.abort_reason {
        println!("abort reason:        {reason}");
    }
    // Reads the `*_human` fields rather than re-rendering the bytes, so the
    // text output and the `--json` output cannot drift apart into two
    // conventions the way the CLI and the GUI once did (HORO-1312).
    println!(
        "expected reclaimed:  {}",
        report
            .expected_reclaimed_human
            .clone()
            .unwrap_or_else(|| "unavailable".to_string())
    );
    println!(
        "actual reclaimed:    {}",
        report
            .actual_reclaimed_human
            .clone()
            .unwrap_or_else(|| "unavailable".to_string())
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    use super::*;
    use crate::actions::llm::LlmError;
    use crate::detectors::{Detector, DetectorId};
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        ActionId, GitState, Recoverability, ResourceFingerprint, ResourceId, ResourceKind,
    };
    use crate::evidence::{ProbeOutcome, ProbeReason};

    fn evidence(path: &str, kind: ResourceKind, reclaimable: Option<u64>) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(reclaimable.unwrap_or(0)),
            physical_bytes: None,
            reclaimable_bytes: match reclaimable {
                Some(b) => ProbeOutcome::Observed(b),
                None => ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            },
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    struct FakeDetector(Vec<Evidence>);

    impl Detector for FakeDetector {
        fn id(&self) -> DetectorId {
            DetectorId("fake_cli_test_detector")
        }

        fn resource_kinds(&self) -> &'static [ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            if self.0.is_empty() {
                DetectorStatus::ToolAbsent
            } else {
                DetectorStatus::Found(self.0.clone())
            }
        }
    }

    struct CleanCollector;
    impl EvidenceCollector for CleanCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    fn ctx() -> DiscoveryContext {
        DiscoveryContext::new(PathBuf::from("/nonexistent-glomeris-cli-test-home"))
    }

    #[test]
    fn build_status_report_reflects_pressure_classification() {
        // Realistic disk-sized numbers: `ThresholdConfig::default()` gates
        // on absolute free bytes as well as percentage (see
        // `monitor::config`), so a tiny total/free pair like `(1000, 100)`
        // trips the multi-GiB absolute-bytes floor before the percentage
        // even matters. 200 GiB total / 28 GiB free (86% used, 14% free)
        // clears every absolute-bytes threshold and is classified purely
        // by the 85%-used `Pressured` percentage boundary.
        const GIB: u64 = 1024 * 1024 * 1024;
        let usage = FsUsage::new(200 * GIB, 28 * GIB);
        let thresholds = ThresholdConfig::default();
        let report = build_status_report(&usage, &thresholds);

        assert_eq!(report.total_bytes, 200 * GIB);
        assert_eq!(report.free_bytes, 28 * GIB);
        assert!((report.used_percent - 86.0).abs() < 0.001);
        assert_eq!(report.pressure_state, "PRESSURED");
    }

    #[test]
    fn daemon_status_report_computes_age_from_recent_heartbeat() {
        let heartbeat = Heartbeat::new(
            1_700_000_000,
            crate::monitor::PressureState::Healthy,
            10.0,
            1_000_000,
        );
        let now = 1_700_000_042;

        let report = build_daemon_status_report(
            true,
            Path::new("/Users/example/Library/LaunchAgents/dev.glomeris.plist"),
            true,
            Some(&heartbeat),
            now,
        );

        assert_eq!(report.heartbeat_age_secs, Some(42));
        assert!(report.plist_installed);
        assert!(report.loaded);
    }

    #[test]
    fn daemon_status_report_no_heartbeat_file_is_none_not_error() {
        let report = build_daemon_status_report(
            true,
            Path::new("/Users/example/Library/LaunchAgents/dev.glomeris.plist"),
            true,
            None,
            1_700_000_000,
        );

        assert_eq!(report.heartbeat_age_secs, None);
    }

    #[test]
    fn daemon_status_report_never_installed_still_produces_valid_output() {
        let report = build_daemon_status_report(
            false,
            Path::new("/Users/example/Library/LaunchAgents/dev.glomeris.plist"),
            false,
            None,
            1_700_000_000,
        );

        assert!(!report.plist_installed);
        assert!(!report.loaded);
        assert_eq!(report.heartbeat_age_secs, None);
    }

    #[test]
    fn daemon_status_report_json_never_collapses_loaded_and_heartbeat_into_healthy() {
        let heartbeat = Heartbeat::new(
            1_700_000_000,
            crate::monitor::PressureState::Healthy,
            10.0,
            1_000_000,
        );
        let report = build_daemon_status_report(
            true,
            Path::new("/Users/example/Library/LaunchAgents/dev.glomeris.plist"),
            true,
            Some(&heartbeat),
            1_700_000_042,
        );

        let json = serde_json::to_value(&report).expect("serialize");
        let obj = json.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();

        assert_eq!(
            keys,
            vec![
                "heartbeat_age_secs",
                "loaded",
                "plist_installed",
                "plist_path",
            ]
        );
        assert!(!obj.contains_key("healthy"));
        assert!(!obj.contains_key("ok"));
    }

    #[test]
    fn build_history_report_projects_entries_in_order() {
        let entries = vec![
            HistoryEntry {
                unix_time_secs: 1_700_000_000,
                from: "HEALTHY".to_string(),
                to: "WARN".to_string(),
                used_percent: 76.5,
                free_bytes: 1_000_000,
            },
            HistoryEntry {
                unix_time_secs: 1_700_000_060,
                from: "WARN".to_string(),
                to: "PRESSURED".to_string(),
                used_percent: 88.0,
                free_bytes: 500_000,
            },
        ];

        let report = build_history_report(&entries);

        assert_eq!(report.events.len(), 2);
        assert_eq!(report.events[0].unix_time_secs, 1_700_000_000);
        assert_eq!(report.events[0].from, "HEALTHY");
        assert_eq!(report.events[0].to, "WARN");
        assert_eq!(report.events[1].to, "PRESSURED");
    }

    #[test]
    fn build_history_report_empty_entries_produces_empty_events() {
        let report = build_history_report(&[]);

        assert!(report.events.is_empty());
    }

    #[test]
    fn discover_and_classify_correlates_and_classifies_every_candidate() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let registry = DetectorRegistry::from_detectors(vec![Box::new(FakeDetector(vec![ev]))]);
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let results = discover_and_classify(&registry, &ctx(), &CleanCollector, &cfg, now);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.class, PolicyClass::AutoSafe);
    }

    #[test]
    fn discover_and_classify_drops_tool_absent_detectors() {
        let registry = DetectorRegistry::from_detectors(vec![Box::new(FakeDetector(vec![]))]);
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;

        let results = discover_and_classify(&registry, &ctx(), &CleanCollector, &cfg, now);
        assert!(results.is_empty());
    }

    /// A detector that records how many times it was probed, so that the
    /// tests below can assert the number of discovery passes by counting
    /// invocations rather than by timing them (HORO-1487).
    struct CountingDetector {
        id: &'static str,
        result: DetectorStatus,
        probes: Arc<AtomicUsize>,
    }

    impl CountingDetector {
        fn new(id: &'static str, result: DetectorStatus) -> (Self, Arc<AtomicUsize>) {
            let probes = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    id,
                    result,
                    probes: Arc::clone(&probes),
                },
                probes,
            )
        }
    }

    impl Detector for CountingDetector {
        fn id(&self) -> DetectorId {
            DetectorId(self.id)
        }

        fn resource_kinds(&self) -> &'static [ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            self.probes.fetch_add(1, AtomicOrdering::SeqCst);
            match &self.result {
                DetectorStatus::Found(evidences) => DetectorStatus::Found(evidences.clone()),
                DetectorStatus::ToolAbsent => DetectorStatus::ToolAbsent,
                DetectorStatus::Failed(reason) => DetectorStatus::Failed(reason.clone()),
            }
        }
    }

    /// One pass probes each detector exactly once, whatever that detector
    /// reports — including the ones that contribute no candidates, which a
    /// change that only rearranged the evidence could get wrong.
    #[test]
    fn discovery_pass_probes_every_detector_exactly_once() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let (found, found_probes) = CountingDetector::new("found", DetectorStatus::Found(vec![ev]));
        let (absent, absent_probes) = CountingDetector::new("absent", DetectorStatus::ToolAbsent);
        let (failed, failed_probes) =
            CountingDetector::new("failed", DetectorStatus::Failed("boom".to_string()));
        let registry = DetectorRegistry::from_detectors(vec![
            Box::new(found),
            Box::new(absent),
            Box::new(failed),
        ]);

        let pass = discover_and_classify_pass(
            &registry,
            &ctx(),
            &CleanCollector,
            &PolicyConfig::default(),
            SystemTime::UNIX_EPOCH,
            |_| {},
        );

        assert_eq!(found_probes.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(absent_probes.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(failed_probes.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(pass.detectors.len(), 3);
    }

    /// The two halves of the pass agree by construction: every `Found`
    /// outcome's count adds up to exactly the candidates that came out, so
    /// `detect`'s status lines and its report cannot describe different
    /// probes.
    #[test]
    fn discovery_pass_outcomes_account_for_exactly_the_candidates() {
        let ev1 = evidence("/tmp/a/target", ResourceKind::CargoTargetDir, Some(1024));
        let ev2 = evidence("/tmp/b/node_modules", ResourceKind::NodeModules, Some(2048));
        let (found, _) = CountingDetector::new("found", DetectorStatus::Found(vec![ev1, ev2]));
        let (absent, _) = CountingDetector::new("absent", DetectorStatus::ToolAbsent);
        let registry = DetectorRegistry::from_detectors(vec![Box::new(found), Box::new(absent)]);

        let pass = discover_and_classify_pass(
            &registry,
            &ctx(),
            &CleanCollector,
            &PolicyConfig::default(),
            SystemTime::UNIX_EPOCH,
            |_| {},
        );

        let claimed: usize = pass
            .detectors
            .iter()
            .map(|(_, outcome)| match outcome {
                DetectorOutcome::Found { candidates } => *candidates,
                DetectorOutcome::ToolAbsent | DetectorOutcome::Failed(_) => 0,
            })
            .sum();
        assert_eq!(claimed, pass.candidates.len());
        assert_eq!(pass.candidates.len(), 2);
    }

    /// A failed probe keeps its reason, and stays distinguishable from an
    /// absent tool, in the outcome the pass hands back — which is what
    /// `detect`'s `failed: <reason>` line has always printed and could only
    /// print before by probing everything a second time.
    #[test]
    fn discovery_pass_keeps_a_failed_probes_reason_apart_from_tool_absent() {
        let (absent, _) = CountingDetector::new("absent", DetectorStatus::ToolAbsent);
        let (failed, _) = CountingDetector::new(
            "failed",
            DetectorStatus::Failed("brew --cache exited with status 1".to_string()),
        );
        let registry = DetectorRegistry::from_detectors(vec![Box::new(absent), Box::new(failed)]);

        let pass = discover_and_classify_pass(
            &registry,
            &ctx(),
            &CleanCollector,
            &PolicyConfig::default(),
            SystemTime::UNIX_EPOCH,
            |_| {},
        );

        assert_eq!(
            pass.detectors,
            vec![
                (DetectorId("absent"), DetectorOutcome::ToolAbsent),
                (
                    DetectorId("failed"),
                    DetectorOutcome::Failed("brew --cache exited with status 1".to_string())
                ),
            ]
        );
        assert!(pass.candidates.is_empty());
    }

    /// The candidates-only wrappers are the same pipeline, not a second
    /// one: `discover_and_classify` returns exactly the pass's candidates,
    /// and probes each detector once doing it.
    #[test]
    fn discover_and_classify_is_one_pass_of_the_same_pipeline() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let (found, probes) = CountingDetector::new("found", DetectorStatus::Found(vec![ev]));
        let registry = DetectorRegistry::from_detectors(vec![Box::new(found)]);

        let results = discover_and_classify(
            &registry,
            &ctx(),
            &CleanCollector,
            &PolicyConfig::default(),
            SystemTime::UNIX_EPOCH,
        );

        assert_eq!(probes.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn find_candidate_matches_by_resource_id_string() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev.clone(), decision)];

        let found = find_candidate("cargo_target_dir:/tmp/proj/target", &candidates);
        assert!(found.is_some());
    }

    #[test]
    fn find_candidate_matches_by_raw_path() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev.clone(), decision)];

        let found = find_candidate("/tmp/proj/target", &candidates);
        assert!(found.is_some());
    }

    #[test]
    fn find_candidate_returns_none_for_unknown_query() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev, decision)];

        assert!(find_candidate("/tmp/other/target", &candidates).is_none());
    }

    #[test]
    fn build_detect_report_projects_every_candidate() {
        let ev1 = evidence("/tmp/a/target", ResourceKind::CargoTargetDir, Some(100));
        let ev2 = evidence("/tmp/b/node_modules", ResourceKind::NodeModules, Some(9999));
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let candidates = vec![
            (ev1.clone(), classify(&ev1, &cfg, now)),
            (ev2.clone(), classify(&ev2, &cfg, now)),
        ];

        let actions = ActionRegistry::builtin();
        let report = build_detect_report(&candidates, &actions, ImpactContext::default());
        assert_eq!(report.candidates.len(), 2);
    }

    /// HORO-1049: `format_size_field` — the shared text-rendering helper
    /// both `print_detect_report` and `print_explain_report` use — marks a
    /// lower-bound size with the `"≥ ... (lower bound — scan truncated)"`
    /// wording, leaves a settled size bare, and falls back to `"unknown"`
    /// with no size at all (never marked, regardless of the flag).
    #[test]
    fn format_size_field_marks_lower_bound_only_when_flagged() {
        assert_eq!(
            format_size_field(Some("155.1 MB"), true),
            "≥ 155.1 MB (lower bound — scan truncated)"
        );
        assert_eq!(format_size_field(Some("155.1 MB"), false), "155.1 MB");
        assert_eq!(format_size_field(None, true), "unknown");
        assert_eq!(format_size_field(None, false), "unknown");
    }

    /// Builds an [`Evidence`] the way a detector would for a genuinely
    /// budget-truncated size estimate (HORO-1016/HORO-1049): a real
    /// tempdir walked with a deliberately tiny [`crate::scanner::ScanBudget`]
    /// (mirroring `detectors::size_estimate_tests`'s own truncation
    /// fixture pattern), fed through the real `discovery_evidence` +
    /// `SizeEstimate::is_lower_bound` plumbing this ticket adds — never a
    /// hand-set `true` literal, so this proves the typed signal actually
    /// flows from the estimate rather than being asserted by fiat.
    fn evidence_from_truncated_estimate(root: &Path) -> Evidence {
        use crate::detectors::{discovery_evidence, estimate_logical_bytes, probe_mtime};
        use crate::evidence::{Recoverability, Regenerability};
        use crate::scanner::ScanBudget;

        let canonical = root.canonicalize().expect("canonicalize fixture root");
        let budget = ScanBudget::default().with_max_files_visited(1);
        let estimate = estimate_logical_bytes(&canonical, budget);
        assert!(
            estimate.is_lower_bound(),
            "fixture must genuinely force truncation"
        );

        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(canonical.clone()),
        );
        let is_lower_bound = estimate.is_lower_bound();
        discovery_evidence(
            resource,
            DetectorId("test"),
            &canonical,
            estimate.bytes.clone(),
            estimate.bytes,
            is_lower_bound,
            probe_mtime(&canonical),
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            NativeCleanup::Unsupported,
        )
    }

    /// HORO-1049 AC: a genuinely budget-truncated size estimate surfaces
    /// the lower-bound marker in both `DetectCandidateReport`'s and
    /// `ExplainReport`'s rendered text, and the typed flag is `true` in
    /// their `--json` (`serde_json`) representation.
    #[test]
    fn genuinely_truncated_estimate_surfaces_lower_bound_marker_in_text_and_json() {
        let root = std::env::temp_dir().join(format!(
            "glomeris-cli-lower-bound-truncated-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("b.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("c.bin"), vec![0u8; 100]).unwrap();

        let ev = evidence_from_truncated_estimate(&root);
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::now());

        let actions = ActionRegistry::builtin();
        let detect_report = build_detect_report(
            &[(ev.clone(), decision.clone())],
            &actions,
            ImpactContext::default(),
        );
        let candidate = &detect_report.candidates[0];
        assert!(candidate.reclaimable_bytes_is_lower_bound);
        assert_eq!(
            format_size_field(
                candidate.reclaimable_human.as_deref(),
                candidate.reclaimable_bytes_is_lower_bound
            ),
            format!(
                "≥ {} (lower bound — scan truncated)",
                candidate.reclaimable_human.as_deref().unwrap()
            )
        );
        let detect_json = serde_json::to_string(&detect_report).expect("serialize");
        assert!(detect_json.contains("\"reclaimable_bytes_is_lower_bound\":true"));

        let explain_report = build_explain_report(&ev, &decision, &actions);
        assert!(explain_report.reclaimable_bytes_is_lower_bound);
        assert_eq!(
            format_size_field(
                explain_report.reclaimable_human.as_deref(),
                explain_report.reclaimable_bytes_is_lower_bound
            ),
            format!(
                "≥ {} (lower bound — scan truncated)",
                explain_report.reclaimable_human.as_deref().unwrap()
            )
        );
        let explain_json = serde_json::to_string(&explain_report).expect("serialize");
        assert!(explain_json.contains("\"reclaimable_bytes_is_lower_bound\":true"));

        // Prove the print functions don't panic on a lower-bound report —
        // matches this module's existing `print_functions_do_not_panic_*`
        // convention.
        print_detect_report(&detect_report);
        print_explain_report(&explain_report);

        std::fs::remove_dir_all(&root).ok();
    }

    /// HORO-1049 AC: an estimate that completes within budget never shows
    /// the lower-bound marker, in either text or `--json`.
    #[test]
    fn estimate_within_budget_shows_no_lower_bound_marker() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        assert!(!ev.reclaimable_bytes_is_lower_bound);
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);

        let actions = ActionRegistry::builtin();
        let detect_report = build_detect_report(
            &[(ev.clone(), decision.clone())],
            &actions,
            ImpactContext::default(),
        );
        let candidate = &detect_report.candidates[0];
        assert!(!candidate.reclaimable_bytes_is_lower_bound);
        assert_eq!(
            format_size_field(
                candidate.reclaimable_human.as_deref(),
                candidate.reclaimable_bytes_is_lower_bound
            ),
            candidate.reclaimable_human.as_deref().unwrap()
        );
        let detect_json = serde_json::to_string(&detect_report).expect("serialize");
        assert!(detect_json.contains("\"reclaimable_bytes_is_lower_bound\":false"));

        let explain_report = build_explain_report(&ev, &decision, &actions);
        assert!(!explain_report.reclaimable_bytes_is_lower_bound);
        let explain_json = serde_json::to_string(&explain_report).expect("serialize");
        assert!(explain_json.contains("\"reclaimable_bytes_is_lower_bound\":false"));
    }

    /// HORO-1051 AC: `fingerprint_token` is `Some` for a resource that
    /// carries any real fingerprint field, and `None` for one that
    /// doesn't — e.g. a `ResourceLocator::Tool` resource such as Docker's
    /// build cache, which never gets a dev/inode/mtime (see
    /// `detectors/docker.rs`).
    #[test]
    fn fingerprint_token_is_some_for_path_resource_and_none_for_tool_resource() {
        let mut ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1024));
        ev.fingerprint = ResourceFingerprint {
            dev_ino: Some((1, 2)),
            mtime: Some(SystemTime::UNIX_EPOCH),
            tool_revision: None,
        };
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let actions = ActionRegistry::builtin();
        let report = build_explain_report(&ev, &decision, &actions);
        let token = report
            .fingerprint_token
            .expect("path resource with a real fingerprint must report a token");
        let decoded = crate::evidence::decode_fingerprint_token(&token).expect("token must decode");
        assert_eq!(decoded, ev.fingerprint);

        let mut tool_ev = evidence(
            "/tmp/proj/target",
            ResourceKind::DockerBuildCache,
            Some(1024),
        );
        tool_ev.resource = ResourceId::new(
            ResourceKind::DockerBuildCache,
            ResourceLocator::Tool {
                tool: crate::evidence::OwningTool::Docker,
                id: "build_cache".to_string(),
            },
        );
        tool_ev.fingerprint = ResourceFingerprint {
            dev_ino: None,
            mtime: None,
            tool_revision: None,
        };
        let tool_decision = classify(&tool_ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let tool_report = build_explain_report(&tool_ev, &tool_decision, &actions);
        assert!(
            tool_report.fingerprint_token.is_none(),
            "a resource with no real fingerprint fields must report no token"
        );
    }

    #[test]
    fn resolve_action_for_prefers_native_cleanup_when_registered() {
        let mut ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        ev.native_cleanup = NativeCleanup::Available(ActionId("cargo.clean.target_dir"));
        let actions = ActionRegistry::builtin();

        let action = resolve_action_for(&ev, &actions).expect("expected a resolved action");
        assert_eq!(action.id(), ActionId("cargo.clean.target_dir"));
    }

    #[test]
    fn resolve_action_for_falls_back_to_find_for_kind() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        let actions = ActionRegistry::builtin();

        let action = resolve_action_for(&ev, &actions).expect("expected a resolved action");
        assert_eq!(action.id(), ActionId("cargo.clean.target_dir"));
    }

    #[test]
    fn resolve_action_for_returns_none_for_unregistered_kind() {
        let ev = evidence("/tmp/proj/cache", ResourceKind::DockerBuildCache, Some(1));
        let actions = ActionRegistry::builtin();
        assert!(resolve_action_for(&ev, &actions).is_none());
    }

    #[test]
    fn build_clean_dry_run_item_renders_explain_for_a_resolvable_action() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-cli-dry-run-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target_dir = dir.join("target");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let ev = evidence(
            target_dir.to_str().unwrap(),
            ResourceKind::CargoTargetDir,
            Some(4096),
        );
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let actions = ActionRegistry::builtin();

        let item = build_clean_dry_run_item(&ev, &decision, &actions);
        assert_eq!(item.action_id, Some("cargo.clean.target_dir"));
        assert!(item.explain.is_some());
        assert!(item.skip_reason.is_none());
        // Never executes: the fixture directory must still exist.
        assert!(target_dir.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_clean_dry_run_item_reports_skip_reason_for_unresolvable_action() {
        let ev = evidence(
            "/tmp/proj/build-cache",
            ResourceKind::DockerBuildCache,
            Some(1),
        );
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let actions = ActionRegistry::builtin();

        let item = build_clean_dry_run_item(&ev, &decision, &actions);
        assert!(item.action_id.is_none());
        assert!(item.explain.is_none());
        assert!(item.skip_reason.is_some());
    }

    #[test]
    fn build_clean_dry_run_report_without_target_skips_protected_candidates() {
        let mut protected_ev = evidence("/tmp/x", ResourceKind::Unknown, Some(1));
        protected_ev.resource = ResourceId::new(
            ResourceKind::Unknown,
            ResourceLocator::Path(PathBuf::from("/tmp/x")),
        );
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let candidates = vec![(protected_ev.clone(), classify(&protected_ev, &cfg, now))];
        let actions = ActionRegistry::builtin();

        let report = build_clean_dry_run_report(&candidates, &actions, None).unwrap();
        assert!(report.items.is_empty());
    }

    /// SAFETY-CRITICAL regression test: `--target` against a
    /// path-protected resource (credential material) must never render an
    /// executable cleanup action, even though a real `cargo.clean.
    /// target_dir` action WOULD otherwise resolve for `CargoTargetDir` —
    /// this is exactly the gap the no-`--target` filter alone does not
    /// cover, since `--target` bypasses that filter to select one
    /// specific candidate.
    #[test]
    fn build_clean_dry_run_report_with_target_on_protected_resource_renders_no_action() {
        let mut protected_ev = evidence(
            "/Users/x/.ssh/id_ed25519",
            ResourceKind::CargoTargetDir,
            Some(1024),
        );
        protected_ev.resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/.ssh/id_ed25519")),
        );
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let decision = classify(&protected_ev, &cfg, now);
        assert_eq!(decision.class, PolicyClass::Protected);
        let candidates = vec![(protected_ev.clone(), decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_clean_dry_run_report(&candidates, &actions, Some("/Users/x/.ssh/id_ed25519"))
                .unwrap();

        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.policy_label, "PROTECTED");
        assert!(item.action_id.is_none());
        assert!(item.explain.is_none());
        assert!(item.skip_reason.is_some());
    }

    #[test]
    fn build_clean_dry_run_report_with_unknown_target_errors() {
        let candidates: Vec<(Evidence, PolicyDecision)> = Vec::new();
        let actions = ActionRegistry::builtin();

        let result = build_clean_dry_run_report(&candidates, &actions, Some("nope"));
        assert!(result.is_err());
    }

    #[test]
    fn build_clean_dry_run_report_with_target_selects_only_that_candidate() {
        let ev_a = evidence("/tmp/a/target", ResourceKind::CargoTargetDir, Some(1));
        let ev_b = evidence("/tmp/b/target", ResourceKind::CargoTargetDir, Some(1));
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let candidates = vec![
            (ev_a.clone(), classify(&ev_a, &cfg, now)),
            (ev_b.clone(), classify(&ev_b, &cfg, now)),
        ];
        let actions = ActionRegistry::builtin();

        let report =
            build_clean_dry_run_report(&candidates, &actions, Some("/tmp/a/target")).unwrap();
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].resource_id, ev_a.resource.to_string());
    }

    #[test]
    fn extract_project_roots_with_no_flag_returns_all_args_unchanged() {
        let args = vec!["--json".to_string()];
        let (roots, remaining) = extract_project_roots(&args).unwrap();
        assert!(roots.is_empty());
        assert_eq!(remaining, args);
    }

    #[test]
    fn extract_project_roots_collects_a_single_occurrence() {
        let args = vec!["--project-root".to_string(), "/tmp/proj".to_string()];
        let (roots, remaining) = extract_project_roots(&args).unwrap();
        assert_eq!(roots, vec![PathBuf::from("/tmp/proj")]);
        assert!(remaining.is_empty());
    }

    #[test]
    fn extract_project_roots_collects_repeated_occurrences_in_order() {
        let args = vec![
            "--project-root".to_string(),
            "/tmp/a".to_string(),
            "--json".to_string(),
            "--project-root".to_string(),
            "/tmp/b".to_string(),
        ];
        let (roots, remaining) = extract_project_roots(&args).unwrap();
        assert_eq!(
            roots,
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
        assert_eq!(remaining, vec!["--json".to_string()]);
    }

    #[test]
    fn extract_project_roots_does_not_validate_nonexistent_paths() {
        let args = vec![
            "--project-root".to_string(),
            "/definitely/does/not/exist".to_string(),
        ];
        let (roots, _remaining) = extract_project_roots(&args).unwrap();
        assert_eq!(roots, vec![PathBuf::from("/definitely/does/not/exist")]);
    }

    #[test]
    fn extract_project_roots_errors_on_missing_trailing_value() {
        let args = vec!["--project-root".to_string()];
        let result = extract_project_roots(&args);
        assert!(result.is_err());
    }

    #[test]
    fn print_functions_do_not_panic_on_empty_reports() {
        print_status_report(&build_status_report(
            &FsUsage::new(100, 50),
            &ThresholdConfig::default(),
        ));
        print_detect_report(&DetectReport::default());
        print_clean_dry_run_report(&CleanDryRunReport::default());
        let ev = evidence("/tmp/x/target", ResourceKind::CargoTargetDir, None);
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let actions = ActionRegistry::builtin();
        print_explain_report(&build_explain_report(&ev, &decision, &actions));
    }

    #[test]
    fn extract_plan_file_with_no_flag_returns_all_args_unchanged() {
        let args = vec!["--json".to_string()];
        let (plan_file, remaining) = extract_plan_file(&args).unwrap();
        assert!(plan_file.is_none());
        assert_eq!(remaining, args);
    }

    #[test]
    fn extract_plan_file_collects_the_value() {
        let args = vec!["--plan-file".to_string(), "/tmp/plan.json".to_string()];
        let (plan_file, remaining) = extract_plan_file(&args).unwrap();
        assert_eq!(plan_file, Some(PathBuf::from("/tmp/plan.json")));
        assert!(remaining.is_empty());
    }

    #[test]
    fn extract_plan_file_leaves_other_args_in_order() {
        let args = vec![
            "--json".to_string(),
            "--plan-file".to_string(),
            "/tmp/plan.json".to_string(),
            "--project-root".to_string(),
            "/tmp/proj".to_string(),
        ];
        let (plan_file, remaining) = extract_plan_file(&args).unwrap();
        assert_eq!(plan_file, Some(PathBuf::from("/tmp/plan.json")));
        assert_eq!(
            remaining,
            vec![
                "--json".to_string(),
                "--project-root".to_string(),
                "/tmp/proj".to_string(),
            ]
        );
    }

    #[test]
    fn extract_plan_file_errors_on_missing_trailing_value() {
        let args = vec!["--plan-file".to_string()];
        assert!(extract_plan_file(&args).is_err());
    }

    #[test]
    fn credential_flag_name_matches_space_separated_form() {
        assert_eq!(credential_flag_name("--api-key"), "--api-key");
    }

    #[test]
    fn credential_flag_name_strips_an_equals_joined_value() {
        // HORO-1008 adversarial review finding: `--api-key=<secret>` must
        // still be recognized as the `--api-key` flag, not fall through to
        // an error path that echoes the whole token (and the secret with
        // it) verbatim.
        assert_eq!(
            credential_flag_name("--api-key=sk-should-never-appear-anywhere"),
            "--api-key"
        );
        assert_eq!(credential_flag_name("--token=sk-abc"), "--token");
        assert_eq!(credential_flag_name("--key=sk-abc"), "--key");
    }

    #[test]
    fn credential_flag_name_leaves_an_unrelated_equals_joined_token_alone() {
        assert_eq!(
            credential_flag_name("--project-root=/tmp/x"),
            "--project-root"
        );
    }

    struct FakeLlmPlanProvider {
        response: Result<String, crate::actions::llm::LlmError>,
    }

    impl LlmProvider for FakeLlmPlanProvider {
        fn complete(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> Result<String, crate::actions::llm::LlmError> {
            self.response.clone()
        }
    }

    /// HORO-1298: `--print-payload` is only worth anything if the two
    /// prompt fields — the bytes that would actually be transmitted — are
    /// path-free while the alias table, which stays local, still names the
    /// real resource. A regression that reverted the wire ids would show up
    /// as the account name appearing in `user_prompt`.
    #[test]
    fn build_llm_payload_report_separates_outbound_prompts_from_local_aliases() {
        let ev = evidence(
            "/Users/someaccount/Library/Developer/Xcode/DerivedData/MyApp-abc123",
            ResourceKind::XcodeDerivedData,
            Some(4096),
        );
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let candidates = vec![(ev.clone(), decision)];
        let actions = ActionRegistry::builtin();

        let report = build_llm_payload_report(&candidates, &actions).unwrap();

        for (label, text) in [
            ("system_prompt", &report.system_prompt),
            ("user_prompt", &report.user_prompt),
        ] {
            assert!(
                !text.contains("someaccount") && !text.contains('/'),
                "{label} must carry no path or account name: {text}"
            );
        }

        assert_eq!(report.resource_aliases.len(), 1);
        let alias = &report.resource_aliases[0];
        assert_eq!(alias.wire_resource_id, "resource_1");
        assert_eq!(alias.local_resource_id, ev.resource.to_string());
        assert!(alias.local_resource_id.contains("someaccount"));
    }

    #[test]
    fn build_llm_payload_report_on_no_candidates_yields_an_empty_alias_table() {
        let actions = ActionRegistry::builtin();
        let report = build_llm_payload_report(&[], &actions).unwrap();

        assert!(report.resource_aliases.is_empty());
        assert_eq!(report.user_prompt, "[]");
        assert!(!report.system_prompt.is_empty());
    }

    /// SAFETY-CRITICAL regression test: a `PROTECTED` candidate that the
    /// LLM "approves" for a real, registered action must short-circuit
    /// before that action is ever resolved — `requested_action_id` and
    /// `explain` must stay `None`, and `skip_reason` must be set,
    /// regardless of the fact that `cargo.clean.target_dir` is a real
    /// action that WOULD otherwise resolve for `CargoTargetDir`. See
    /// `tests/golden_llm_plan_protected_refusal.rs` for the stronger,
    /// type-level end-to-end version of this proof.
    #[test]
    fn build_llm_plan_report_short_circuits_protected_before_action_resolution() {
        let mut ev = evidence(
            "/Users/x/.ssh/id_ed25519",
            ResourceKind::CargoTargetDir,
            Some(1024),
        );
        ev.resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(PathBuf::from("/Users/x/.ssh/id_ed25519")),
        );
        let cfg = PolicyConfig::default();
        let now = SystemTime::UNIX_EPOCH;
        let decision = classify(&ev, &cfg, now);
        assert_eq!(decision.class, PolicyClass::Protected);

        let resource_id = ev.resource.to_string();
        let text = format!(
            r#"{{"items": [{{"resource_id": "{resource_id}", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": "looks stale"}}]}}"#
        );
        let provider = FakeLlmPlanProvider { response: Ok(text) };
        let candidates = vec![(ev, decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());

        assert!(report.provider_error.is_none());
        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.policy_label, "PROTECTED");
        assert!(item.requested_action_id.is_none());
        assert!(item.explain.is_none());
        assert!(item.skip_reason.is_some());
    }

    /// A model naming a real action that does not apply to the resource it
    /// named is the commonest way a suggestion gets refused, and the
    /// `skip_reason` on that row is the only place a user learns why. It
    /// used to read `ResourceMismatch`.
    ///
    /// Asserted here rather than in `actions`' own `Display` tests because
    /// this is the path that reaches a screen: the row is built, refused,
    /// and its refusal sentence read back through the report a surface
    /// actually renders.
    #[test]
    fn build_llm_plan_report_explains_a_mismatched_action_in_words() {
        let ev = evidence(
            "/Users/x/proj/node_modules",
            ResourceKind::NodeModules,
            Some(2048),
        );
        let cfg = PolicyConfig::default();
        let decision = classify(&ev, &cfg, SystemTime::UNIX_EPOCH);
        assert_ne!(decision.class, PolicyClass::Protected);

        // A registered action, asked for against the wrong kind of resource.
        let resource_id = ev.resource.to_string();
        let text = format!(
            r#"{{"items": [{{"resource_id": "{resource_id}", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": "same thing really"}}]}}"#
        );
        let provider = FakeLlmPlanProvider { response: Ok(text) };
        let candidates = vec![(ev, decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());

        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert!(
            item.explain.is_none(),
            "nothing was planned, so nothing to explain"
        );
        let skip = item.skip_reason.as_deref().unwrap();
        assert_eq!(skip, "that action does not apply to this kind of resource");
        assert!(
            !skip.contains("ResourceMismatch"),
            "a Rust variant name must never reach a user: {skip}"
        );
    }

    #[test]
    fn build_llm_plan_report_renders_explain_for_auto_safe_item() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-llm-plan-report-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target_dir = dir.join("target");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        // `evidence()`'s defaults leave open_by_process/process_cwd_match/
        // git_state/tool_liveness `Unavailable` (matching its other
        // callers' needs), which alone would classify `Ask` via
        // `EvidenceIncomplete` — override them to `Observed` so this
        // fixture actually reaches `AutoSafe`, which is what this test is
        // about.
        let mut ev = evidence(
            target_dir.to_str().unwrap(),
            ResourceKind::CargoTargetDir,
            Some(4096),
        );
        ev.open_by_process = ProbeOutcome::Observed(Vec::new());
        ev.process_cwd_match = ProbeOutcome::Observed(Vec::new());
        ev.git_state = ProbeOutcome::Observed(None);
        ev.tool_liveness = ProbeOutcome::Observed(false);

        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        assert_eq!(decision.class, PolicyClass::AutoSafe);

        let resource_id = ev.resource.to_string();
        let text = format!(
            r#"{{"items": [{{"resource_id": "{resource_id}", "action_id": "cargo.clean.target_dir", "priority": 3, "reason": "stale"}}]}}"#
        );
        let provider = FakeLlmPlanProvider { response: Ok(text) };
        let candidates = vec![(ev, decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());

        assert!(report.provider_error.is_none());
        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.policy_label, "AUTO_SAFE");
        assert_eq!(item.requested_action_id, Some("cargo.clean.target_dir"));
        assert_eq!(item.priority, Some(3));
        assert!(item.explain.is_some());
        assert!(item.skip_reason.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_llm_plan_report_propagates_dropped_counters() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let text = r#"{"items": [
            {"resource_id": "cargo_target_dir:/nonexistent", "action_id": "cargo.clean.target_dir", "priority": 1, "reason": null},
            {"resource_id": "cargo_target_dir:/tmp/proj/target", "action_id": "docker.nuke.everything", "priority": 1, "reason": null}
        ]}"#;
        let provider = FakeLlmPlanProvider {
            response: Ok(text.to_string()),
        };
        let candidates = vec![(ev, decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());

        assert!(report.items.is_empty());
        assert_eq!(report.dropped_unknown_resource, 1);
        assert_eq!(report.dropped_unknown_action, 1);
    }

    #[test]
    fn build_llm_plan_report_propagates_provider_error() {
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let provider = FakeLlmPlanProvider {
            response: Err(crate::actions::llm::LlmError::NetworkError(
                "connection refused".to_string(),
            )),
        };
        let candidates = vec![(ev, decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());

        assert!(report.items.is_empty());
        assert!(report.provider_error.is_some());
    }

    #[test]
    fn build_llm_plan_report_renders_provider_status_readably() {
        // HORO-1299: the reported string must name the HTTP status and the
        // endpoint path, and must not be Rust `Debug` struct syntax — this
        // field is what a user sees in `--json` output and in a bug report.
        let ev = evidence("/tmp/proj/target", ResourceKind::CargoTargetDir, Some(1));
        let decision = classify(&ev, &PolicyConfig::default(), SystemTime::UNIX_EPOCH);
        let provider = FakeLlmPlanProvider {
            response: Err(crate::actions::llm::LlmError::ProviderStatus {
                status: 401,
                api_style: crate::actions::llm::API_STYLE_CHAT_COMPLETIONS.to_string(),
                endpoint_path: "/v1/chat/completions".to_string(),
                request_id: Some("req-7".to_string()),
                body_excerpt: r#"{"error":{"code":"invalid_api_key"}}"#.to_string(),
            }),
        };
        let candidates = vec![(ev, decision)];
        let actions = ActionRegistry::builtin();

        let report =
            build_llm_plan_report(&candidates, &actions, &provider, ImpactContext::default());
        let rendered = report.provider_error.expect("provider error is set");

        assert!(rendered.contains("HTTP 401"), "got: {rendered}");
        assert!(rendered.contains("/v1/chat/completions"), "got: {rendered}");
        assert!(rendered.contains("invalid_api_key"), "got: {rendered}");
        assert!(rendered.contains("request_id=req-7"), "got: {rendered}");
        assert!(
            !rendered.contains("ProviderStatus {"),
            "must not be Debug struct syntax: {rendered}"
        );
    }

    #[test]
    fn llm_plan_schema_example_round_trips() {
        // Real deserialization through `LlmPlan`, not a hand-rolled
        // shape check — proves the example `glomeris llm-plan --schema`
        // emits is a genuine, valid `LlmPlan` document.
        let text = llm_plan_schema_example();
        let plan: crate::actions::llm::LlmPlan = serde_json::from_str(&text)
            .expect("llm_plan_schema_example must produce valid LlmPlan JSON");
        assert_eq!(plan.items.len(), 1);
        let item = &plan.items[0];
        assert_eq!(item.resource_id, "cargo_target_dir:/path/to/project/target");
        assert_eq!(item.action_id, "cargo.clean.target_dir");
        assert_eq!(item.priority, Some(1));

        // The example's action_id is a real, registered action — not a
        // placeholder that would always drop as unknown.
        let actions = ActionRegistry::builtin();
        assert!(actions.get(&item.action_id).is_some());
    }

    /// HORO-1309 AC 4: the four outcomes are distinct tokens, so a UI can
    /// tell "your gateway is unreachable" from "your gateway refused this
    /// key" without parsing English.
    #[test]
    fn build_llm_check_report_maps_each_failure_class_to_its_own_outcome() {
        let cases: [(LlmError, &str); 3] = [
            (
                LlmError::NetworkError("connection refused".to_string()),
                "unreachable",
            ),
            (
                LlmError::ProviderStatus {
                    status: 401,
                    api_style: crate::actions::llm::API_STYLE_CHAT_COMPLETIONS.to_string(),
                    endpoint_path: "/v1/chat/completions".to_string(),
                    request_id: None,
                    body_excerpt: r#"{"error":{"code":"invalid_api_key"}}"#.to_string(),
                },
                "rejected",
            ),
            (
                LlmError::InvalidResponse("no choices[0].message.content".to_string()),
                "unusable_response",
            ),
        ];

        for (error, expected) in cases {
            let provider = FakeLlmPlanProvider {
                response: Err(error),
            };
            let report = build_llm_check_report(&provider, "some-model", "/v1/chat/completions");

            assert_eq!(report.outcome, expected);
            assert!(
                report.error.is_some(),
                "{expected} must carry the human-readable detail too"
            );
            assert!(
                report.response_excerpt.is_none(),
                "{expected} has no successful reply to excerpt"
            );
            assert_eq!(report.model, "some-model");
            assert_eq!(report.endpoint_path, "/v1/chat/completions");
        }
    }

    #[test]
    fn build_llm_check_report_reports_ok_with_the_reply_and_no_error() {
        let provider = FakeLlmPlanProvider {
            response: Ok("ok".to_string()),
        };
        let report = build_llm_check_report(&provider, "some-model", "/v1/chat/completions");

        assert_eq!(report.outcome, "ok");
        assert_eq!(report.error, None);
        assert_eq!(report.response_excerpt.as_deref(), Some("ok"));
    }

    /// AC 5, at the boundary where it is cheapest to enforce: a provider that
    /// echoes the request — which is exactly what a misconfigured proxy or a
    /// debug endpoint does — must not be able to get an arbitrary amount of
    /// text into a field the GUI renders and the shell scrolls. The excerpt
    /// is bounded here rather than by each consumer, so there is one place to
    /// check instead of one per surface.
    #[test]
    fn build_llm_check_report_bounds_a_provider_that_replies_with_a_flood() {
        let provider = FakeLlmPlanProvider {
            response: Ok("A".repeat(20_000)),
        };
        let report = build_llm_check_report(&provider, "some-model", "/v1/chat/completions");
        let excerpt = report.response_excerpt.expect("ok carries an excerpt");

        assert_eq!(report.outcome, "ok");
        assert!(
            excerpt.len() < CHECK_RESPONSE_EXCERPT_LIMIT + 32,
            "excerpt grew to {} bytes",
            excerpt.len()
        );
        assert!(excerpt.contains("(truncated)"), "got: {excerpt}");
    }

    /// The check report is a *reduction* of the configuration, not a copy of
    /// it: whatever a user pasted into the base-URL field must not come back
    /// out, because a host may itself be private infrastructure. Only the
    /// path survives — enough to tell an API root from a host root, which is
    /// the one thing this field is for. See `LlmCheckReport`'s doc comment.
    #[test]
    fn build_llm_check_report_carries_no_base_url_only_its_path() {
        let provider = FakeLlmPlanProvider {
            response: Ok("ok".to_string()),
        };
        let report = build_llm_check_report(
            &provider,
            "some-model",
            &crate::actions::llm::chat_completions_endpoint_path(
                "https://gateway.internal.example/v1",
            ),
        );

        let serialized = serde_json::to_string(&report).expect("report serializes");
        for forbidden in ["gateway.internal.example", "https://"] {
            assert!(
                !serialized.contains(forbidden),
                "{forbidden} must not survive into the report: {serialized}"
            );
        }
        assert_eq!(report.endpoint_path, "/v1/chat/completions");
    }

    /// Defence in depth for the same property against the nastiest input:
    /// some gateways accept a credential in the query string, so a user may
    /// paste one into the base-URL field. Such a URL is rejected upstream by
    /// `validate_base_url`, but if it ever reached here the report still must
    /// not carry the credential — `diagnostic_endpoint_path` keeps the path
    /// and drops the query, so it does not.
    #[test]
    fn build_llm_check_report_drops_a_query_string_credential_from_the_path() {
        let provider = FakeLlmPlanProvider {
            response: Ok("ok".to_string()),
        };
        let report = build_llm_check_report(
            &provider,
            "some-model",
            &crate::actions::llm::chat_completions_endpoint_path(
                "https://gateway.internal.example/v1?access_token=pasted-into-the-url",
            ),
        );

        let serialized = serde_json::to_string(&report).expect("report serializes");
        for forbidden in ["access_token", "pasted-into-the-url"] {
            assert!(
                !serialized.contains(forbidden),
                "{forbidden} must not survive into the report: {serialized}"
            );
        }
    }
}

/// HORO-1055: `resolve_and_execute` tests. Real `classify`/`authorize`/
/// `execute` throughout — never hand-written decisions/approvals — so
/// these exercise the actual enforcement path an adversarial reviewer
/// would poke at, not a mock of it.
#[cfg(test)]
mod execute_tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::detectors::{dev_ino_fingerprint, probe_mtime, DetectorId};
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        GitState, ProcessRef, Recoverability, ResourceFingerprint, ResourceId, ResourceKind,
    };
    use crate::evidence::{decode_fingerprint_token, encode_fingerprint_token};
    use crate::evidence::{ProbeOutcome, ProbeReason};

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-cli-execute-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A collector reporting no active use of any kind — pairs with a
    /// clean fixture to reach `PolicyClass::AutoSafe`.
    struct CleanCollector;
    impl EvidenceCollector for CleanCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::<ProcessRef>::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    /// A collector reporting a live process holding the resource open —
    /// pairs with an otherwise-clean fixture to reach
    /// `PolicyClass::Ask{ResourceInActiveUse}`. Used on BOTH sides
    /// (building the candidate and passed into `resolve_and_execute`) so
    /// `execute`'s revalidation reproduces the identical decision rather
    /// than tripping `PolicyReasonsWidened`.
    struct ActiveUseCollector;
    impl EvidenceCollector for ActiveUseCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(vec![ProcessRef {
                    pid: 1,
                    command: "node".to_string(),
                }]),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    /// Builds one discovered-and-classified candidate for `path`/`kind`
    /// the same discover-refresh-classify way
    /// `discover_and_classify_with_progress` does per resource: a raw
    /// `Evidence` reflecting real on-disk fields, a real correlation pass
    /// via `collector`, then the real `classify`. Never hand-writes a
    /// `PolicyDecision`.
    fn discover_one(
        path: &Path,
        kind: ResourceKind,
        collector: &dyn EvidenceCollector,
        now: SystemTime,
    ) -> Vec<(Evidence, PolicyDecision)> {
        let resource = ResourceId::new(kind, ResourceLocator::Path(path.to_path_buf()));
        let mut ev = Evidence {
            resource: resource.clone(),
            fingerprint: ResourceFingerprint {
                dev_ino: dev_ino_fingerprint(path),
                mtime: probe_mtime(path).observed().copied(),
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(4096),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(now),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: now,
            sources: Vec::new(),
        };
        let correlation = collector.collect(
            &resource,
            ProbeBudget {
                timeout: Duration::from_secs(3),
            },
        );
        merge_into(&mut ev, correlation);
        ev.collected_at = now;
        let decision = classify(&ev, &PolicyConfig::default(), now);
        vec![(ev, decision)]
    }

    /// Round-trips `fingerprint` through the exact encode/decode pair a
    /// real `--observed-fingerprint` token would go through (as if a UI
    /// had captured it from a prior `explain --json` call) — the fixture
    /// never hands `resolve_and_execute` a `ResourceFingerprint` value
    /// directly.
    fn token_for(fingerprint: &ResourceFingerprint) -> ResourceFingerprint {
        let token = encode_fingerprint_token(fingerprint);
        decode_fingerprint_token(&token).expect("a token this function just encoded must decode")
    }

    /// AC 1: a PROTECTED resource refuses `execute` unconditionally —
    /// even with `--confirm-ask` and a genuinely matching
    /// `--observed-fingerprint` token. Fixture mirrors the HORO-1008-style
    /// golden protected pattern (`.ssh` path component), matched by the
    /// real `policy::protected` matcher via the real `classify` call
    /// inside `discover_one`.
    #[test]
    fn protected_resource_refuses_regardless_of_confirm_ask_and_matching_fingerprint() {
        let root = make_temp_dir("protected");
        let target = root.join(".ssh").join("node_modules");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("pkg.js"), vec![0u8; 16]).unwrap();

        let now = SystemTime::now();
        let collector = CleanCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, decision) = &candidates[0];
        assert_eq!(
            decision.class,
            PolicyClass::Protected,
            "fixture must classify PROTECTED for this test to exercise the real refusal path"
        );

        let actions = ActionRegistry::builtin();
        let action =
            resolve_action_for(ev, &actions).expect("NodeModules kind must resolve an action");
        let matching_fingerprint = token_for(&ev.fingerprint);

        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            Some(matching_fingerprint),
            &collector,
            &PolicyConfig::default(),
            now,
            &root.join("actions.jsonl"),
        );

        assert!(
            matches!(resolution, ExecuteResolution::RefusedProtected),
            "expected RefusedProtected, got {resolution:?}"
        );
        assert!(target.exists(), "PROTECTED must never mutate the resource");

        fs::remove_dir_all(&root).ok();
    }

    /// AC 2: `Ask` without `--confirm-ask` (no consent at all) refuses.
    #[test]
    fn ask_without_confirm_ask_refuses() {
        let root = make_temp_dir("ask-no-consent");
        let target = root.join("node_modules");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("pkg.js"), vec![0u8; 16]).unwrap();

        let now = SystemTime::now();
        let collector = ActiveUseCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, decision) = &candidates[0];
        assert_eq!(decision.class, PolicyClass::Ask);

        let actions = ActionRegistry::builtin();
        let action = resolve_action_for(ev, &actions).unwrap();

        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            None,
            &collector,
            &PolicyConfig::default(),
            now,
            &root.join("actions.jsonl"),
        );

        assert!(
            matches!(resolution, ExecuteResolution::RefusedAskNoConsent),
            "expected RefusedAskNoConsent, got {resolution:?}"
        );
        assert!(target.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// AC 3: `Ask` with `--confirm-ask` but a stale/mismatched
    /// `--observed-fingerprint` refuses.
    #[test]
    fn ask_with_confirm_ask_but_mismatched_fingerprint_refuses() {
        let root = make_temp_dir("ask-mismatch");
        let target = root.join("node_modules");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("pkg.js"), vec![0u8; 16]).unwrap();

        let now = SystemTime::now();
        let collector = ActiveUseCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, decision) = &candidates[0];
        assert_eq!(decision.class, PolicyClass::Ask);

        let actions = ActionRegistry::builtin();
        let action = resolve_action_for(ev, &actions).unwrap();

        // Deliberately NOT `ev.fingerprint` — a stale/wrong observation,
        // exactly what a UI would carry if the resource changed after the
        // user was shown its identity.
        let stale_fingerprint = ResourceFingerprint {
            dev_ino: None,
            mtime: Some(SystemTime::UNIX_EPOCH),
            tool_revision: None,
        };
        assert_ne!(stale_fingerprint, ev.fingerprint);

        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            Some(stale_fingerprint),
            &collector,
            &PolicyConfig::default(),
            now,
            &root.join("actions.jsonl"),
        );

        assert!(
            matches!(resolution, ExecuteResolution::RefusedAskConsentMismatch),
            "expected RefusedAskConsentMismatch, got {resolution:?}"
        );
        assert!(target.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// AC 4: `Ask` with `--confirm-ask` and a genuinely matching
    /// `--observed-fingerprint` (obtained via the real encode/decode round
    /// trip, mirroring what `explain --json`'s `fingerprint_token` would
    /// hand a caller) reaches `execute` — asserts the `Executed(_)`
    /// discriminant specifically, not `Succeeded`, since AC 4's contract
    /// is "reaches execute", and this fixture's `ActiveUseCollector` is
    /// reused unchanged at revalidation time so the fresh decision still
    /// agrees with the approved one (no `PolicyReasonsWidened` abort).
    #[test]
    fn ask_with_confirm_ask_and_matching_fingerprint_reaches_execute() {
        let root = make_temp_dir("ask-match");
        let target = root.join("node_modules");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("pkg.js"), vec![0u8; 16]).unwrap();

        let now = SystemTime::now();
        let collector = ActiveUseCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, decision) = &candidates[0];
        assert_eq!(decision.class, PolicyClass::Ask);

        let actions = ActionRegistry::builtin();
        let action = resolve_action_for(ev, &actions).unwrap();
        let matching_fingerprint = token_for(&ev.fingerprint);

        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            Some(matching_fingerprint),
            &collector,
            &PolicyConfig::default(),
            now,
            &root.join("actions.jsonl"),
        );

        assert!(
            matches!(resolution, ExecuteResolution::Executed(_)),
            "expected Executed(_), got {resolution:?}"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// AC 5: a real `AUTO_SAFE` action executes end-to-end against a
    /// disposable fixture and reports actual reclaimed bytes measured
    /// after deletion — not an estimate. Uses `NodeCleanNodeModules`
    /// specifically (a `DeletePath` action) rather than the cargo action,
    /// because the cargo `RunTool` step's `actual_reclaimed_bytes` is
    /// honestly `Unavailable` (see `executor::execute_plan`'s doc
    /// comment) and would make "reports actual reclaimed bytes"
    /// unassertable.
    #[test]
    fn auto_safe_action_executes_and_reports_real_reclaimed_bytes() {
        let root = make_temp_dir("auto-safe-real");
        let target = root.join("node_modules");
        fs::create_dir_all(target.join("pkg")).unwrap();
        fs::write(target.join("pkg/index.js"), vec![0u8; 4096]).unwrap();

        let now = SystemTime::now();
        let collector = CleanCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, decision) = &candidates[0];
        assert_eq!(
            decision.class,
            PolicyClass::AutoSafe,
            "fixture must classify AUTO_SAFE for this test to exercise the real happy path"
        );

        let actions = ActionRegistry::builtin();
        let action = resolve_action_for(ev, &actions).unwrap();

        // No `--confirm-ask`/`--observed-fingerprint` at all — AUTO_SAFE
        // needs no consent, per `authorize`'s own contract.
        let audit_log_path = root.join("actions.jsonl");
        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            None,
            &collector,
            &PolicyConfig::default(),
            now,
            &audit_log_path,
        );

        let ExecuteResolution::Executed(report) = resolution else {
            panic!("expected Executed(_), got {resolution:?}");
        };
        assert_eq!(report.outcome, ExecutionOutcome::Succeeded);
        assert!(
            !target.exists(),
            "the real fixture must actually be deleted"
        );

        let execute_report = build_execute_report(&report);
        assert_eq!(execute_report.outcome, "succeeded");
        match execute_report.actual_reclaimed_bytes {
            Some(bytes) => assert!(bytes > 0, "expected a real measured reclaim, got 0"),
            None => panic!("expected Some(actual_reclaimed_bytes), got None"),
        }

        // HORO-1057 AC: the real `execute` call site appends an audit
        // record after the outcome is known.
        let audit_tail = crate::monitor::read_audit_tail(&audit_log_path, 10);
        assert_eq!(audit_tail.len(), 1, "expected exactly one audit record");
        let audit_record = &audit_tail[0];
        assert_eq!(audit_record.source, "execute");
        assert_eq!(audit_record.outcome, "succeeded");
        assert_eq!(audit_record.policy_label, "AUTO_SAFE");
        assert_eq!(audit_record.resource_id, ev.resource.to_string());
        assert!(audit_record.actual_reclaimed_bytes.unwrap_or(0) > 0);

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1057 AC, proven literally: an audit-write failure never
    /// changes `resolve_and_execute`'s own returned outcome. `audit_log_path`
    /// is pointed at a path whose PARENT already exists as a plain file
    /// (not a directory) — `append_audit_record`'s own
    /// `create_dir_all(parent)` step is guaranteed to fail against that,
    /// deterministically simulating an unwritable audit destination
    /// without relying on OS permission quirks. The real action still
    /// executes and succeeds exactly as in
    /// `auto_safe_action_executes_and_reports_real_reclaimed_bytes` above.
    #[test]
    fn audit_write_failure_does_not_change_execution_outcome() {
        let root = make_temp_dir("audit-write-failure");
        let target = root.join("node_modules");
        fs::create_dir_all(target.join("pkg")).unwrap();
        fs::write(target.join("pkg/index.js"), vec![0u8; 4096]).unwrap();

        // A regular file, not a directory — `actions.jsonl`'s intended
        // parent — so `create_dir_all` on it must fail.
        let unwritable_parent = root.join("not-a-directory");
        fs::write(&unwritable_parent, b"blocking file").unwrap();
        let audit_log_path = unwritable_parent.join("actions.jsonl");

        let now = SystemTime::now();
        let collector = CleanCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, decision) = &candidates[0];
        assert_eq!(decision.class, PolicyClass::AutoSafe);

        let actions = ActionRegistry::builtin();
        let action = resolve_action_for(ev, &actions).unwrap();

        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            None,
            &collector,
            &PolicyConfig::default(),
            now,
            &audit_log_path,
        );

        let ExecuteResolution::Executed(report) = resolution else {
            panic!(
                "expected Executed(_) regardless of the audit-write failure, got {resolution:?}"
            );
        };
        assert_eq!(
            report.outcome,
            ExecutionOutcome::Succeeded,
            "the real execution outcome must be completely unaffected by the audit-write failure"
        );
        assert!(
            !target.exists(),
            "the real fixture must still actually be deleted"
        );
        // Confirm the audit write genuinely did fail, rather than this
        // test accidentally not exercising the failure path at all.
        assert!(!audit_log_path.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1056: proves `build_execute_report` actually renders every
    /// [`ExecutionOutcome`] variant — including the two `AbortReason`
    /// cases already exercised at the `executor::execute` level by
    /// `execute_aborts_when_resource_identity_changed_between_approval_and_execution`
    /// and `execute_aborts_when_fresh_reasons_widen_beyond_planned_ask_bucket`
    /// in `src/executor/mod.rs` — as a distinct, machine-readable
    /// `outcome`/`abort_reason` pair, never collapsed into one generic
    /// string. `"succeeded"` is already covered by
    /// `auto_safe_action_executes_and_reports_real_reclaimed_bytes` above;
    /// this test rounds out `"failed"`, `"aborted_by_revalidation"` (both
    /// `AbortReason` variants that matter — `ResourceIdentityChanged` vs.
    /// `PolicyReasonsWidened`, which the AC's "FingerprintMismatch" name
    /// maps to), and `"dry_run"`.
    #[test]
    fn build_execute_report_distinguishes_every_execution_outcome() {
        use crate::executor::AbortReason;

        fn report_for(outcome: ExecutionOutcome) -> ExecutionReport {
            ExecutionReport {
                action: crate::evidence::model::ActionId("test.action"),
                resource: ResourceId::new(
                    ResourceKind::NodeModules,
                    ResourceLocator::Path(PathBuf::from("/tmp/does-not-matter")),
                ),
                outcome,
                expected_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
                actual_reclaimed_bytes: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            }
        }

        let failed = build_execute_report(&report_for(ExecutionOutcome::Failed(
            "disk full".to_string(),
        )));
        assert_eq!(failed.outcome, "failed");
        assert_eq!(failed.failure_message.as_deref(), Some("disk full"));
        assert_eq!(failed.abort_reason, None);

        let identity_changed = build_execute_report(&report_for(
            ExecutionOutcome::AbortedByRevalidation(AbortReason::ResourceIdentityChanged),
        ));
        assert_eq!(identity_changed.outcome, "aborted_by_revalidation");
        assert_eq!(identity_changed.failure_message, None);
        assert_eq!(
            identity_changed.abort_reason.as_deref(),
            Some("ResourceIdentityChanged")
        );

        let reasons_widened = build_execute_report(&report_for(
            ExecutionOutcome::AbortedByRevalidation(AbortReason::PolicyReasonsWidened),
        ));
        assert_eq!(reasons_widened.outcome, "aborted_by_revalidation");
        assert_eq!(
            reasons_widened.abort_reason.as_deref(),
            Some("PolicyReasonsWidened"),
            "distinct AbortReason variants must render as distinct strings, never collapsed"
        );
        assert_ne!(
            identity_changed.abort_reason, reasons_widened.abort_reason,
            "two different AbortReason variants must not render identically"
        );

        let dry_run_report = build_execute_report(&report_for(ExecutionOutcome::DryRun));
        assert_eq!(dry_run_report.outcome, "dry_run");
        assert_eq!(dry_run_report.failure_message, None);
        assert_eq!(dry_run_report.abort_reason, None);

        // Every outcome string above, plus "succeeded" (covered by
        // `auto_safe_action_executes_and_reports_real_reclaimed_bytes`),
        // must be pairwise distinct.
        let outcomes = [
            "succeeded",
            failed.outcome,
            identity_changed.outcome,
            dry_run_report.outcome,
        ];
        for (i, a) in outcomes.iter().enumerate() {
            for (j, b) in outcomes.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "outcome strings must be pairwise distinct");
                }
            }
        }
    }

    /// A resource whose `--action-id` does not match what actually
    /// resolves for it is refused before `authorize` is ever consulted —
    /// the caller asked for a specific action, not "whatever resolves".
    #[test]
    fn mismatched_action_id_is_refused_before_authorize_runs() {
        let root = make_temp_dir("action-mismatch");
        let target = root.join("node_modules");
        fs::create_dir_all(&target).unwrap();

        let now = SystemTime::now();
        let collector = CleanCollector;
        let candidates = discover_one(&target, ResourceKind::NodeModules, &collector, now);
        let (ev, _decision) = &candidates[0];
        let actions = ActionRegistry::builtin();

        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            "cargo.clean.target_dir",
            None,
            &collector,
            &PolicyConfig::default(),
            now,
            &root.join("actions.jsonl"),
        );

        assert!(
            matches!(resolution, ExecuteResolution::ActionMismatch { .. }),
            "expected ActionMismatch, got {resolution:?}"
        );
        assert!(target.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// A `--resource-id` matching nothing in the discovered candidate set
    /// is refused outright.
    #[test]
    fn unknown_resource_id_is_not_found() {
        let candidates: Vec<(Evidence, PolicyDecision)> = Vec::new();
        let actions = ActionRegistry::builtin();
        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            "does-not-exist",
            "node.clean.node_modules",
            None,
            &CleanCollector,
            &PolicyConfig::default(),
            SystemTime::now(),
            Path::new("/nonexistent-glomeris-audit-test-path/actions.jsonl"),
        );

        assert!(matches!(resolution, ExecuteResolution::ResourceNotFound));
    }

    /// HORO-1047 AC: `actions list --json`'s action-id set is derived
    /// directly from `ActionRegistry::actions()`'s real iteration — never a
    /// hand-typed expected list — so this test cannot silently desync from
    /// the registry when a new action is added to `ActionRegistry::builtin`.
    #[test]
    fn build_action_list_report_enumerates_every_registered_action() {
        let registry = ActionRegistry::builtin();

        let expected: Vec<&'static str> = registry.actions().map(|action| action.id().0).collect();

        let report = build_action_list_report(&registry);
        let actual: Vec<&'static str> = report.actions.iter().map(|item| item.action_id).collect();

        assert_eq!(actual, expected);
        assert_eq!(report.actions.len(), registry.actions().count());
    }

    /// Each item's `applies_to` is a direct projection of the real
    /// `Action::applies_to()` slice for that action — never hardcoded —
    /// proven here against the one action currently registered for
    /// `ResourceKind::CargoTargetDir`.
    #[test]
    fn build_action_list_report_applies_to_matches_the_real_action() {
        let registry = ActionRegistry::builtin();
        let report = build_action_list_report(&registry);

        let cargo_action = registry
            .get("cargo.clean.target_dir")
            .expect("cargo.clean.target_dir must be registered");
        let expected_applies_to: Vec<&'static str> = cargo_action
            .applies_to()
            .iter()
            .map(|kind| kind.tag())
            .collect();

        let item = report
            .actions
            .iter()
            .find(|item| item.action_id == "cargo.clean.target_dir")
            .expect("cargo.clean.target_dir must appear in the report");
        assert_eq!(item.applies_to, expected_applies_to);
    }

    /// `print_action_list_report` must not panic on a real, non-empty
    /// registry — matches this module's existing `print_*_do_not_panic`
    /// convention.
    #[test]
    fn print_action_list_report_does_not_panic() {
        let registry = ActionRegistry::builtin();
        let report = build_action_list_report(&registry);
        print_action_list_report(&report);
    }
}
