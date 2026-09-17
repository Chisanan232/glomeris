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

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::actions::llm::{plan_with_llm, LlmProvider};
use crate::actions::{Action, ActionRegistry};
use crate::detectors::{DetectorProgress, DetectorRegistry, DetectorStatus, DiscoveryContext};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Evidence, NativeCleanup, ResourceFingerprint, ResourceLocator};
use crate::executor::{dry_run, execute, ExecutionOutcome, ExecutionReport};
use crate::monitor::{FsUsage, Heartbeat, HistoryEntry, ThresholdConfig};
use crate::policy::approval::authorize;
use crate::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision, UserConsent};
use crate::reporting::dto::{
    ActionListItem, ActionListReport, CleanDryRunItem, CleanDryRunReport, DaemonStatusReport,
    DetectCandidateReport, DetectReport, ExecuteReport, ExplainReport, HistoryEventReport,
    HistoryReport, LlmPlanItemReport, LlmPlanReport, ProgressEvent, StatusReport,
};

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
pub fn discover_and_classify_with_progress(
    registry: &DetectorRegistry,
    ctx: &DiscoveryContext,
    collector: &dyn EvidenceCollector,
    policy_cfg: &PolicyConfig,
    now: SystemTime,
    mut on_event: impl FnMut(ProgressEvent),
) -> Vec<(Evidence, PolicyDecision)> {
    let discovered = registry.discover_all_with_progress(ctx, |id, progress| match progress {
        DetectorProgress::Started => {
            on_event(ProgressEvent::DetectorStarted { detector: id.0 });
        }
        DetectorProgress::Finished(status) => {
            let candidates_found = match status {
                DetectorStatus::Found(evidences) => evidences.len(),
                DetectorStatus::ToolAbsent | DetectorStatus::Failed(_) => 0,
            };
            on_event(ProgressEvent::DetectorFinished {
                detector: id.0,
                candidates_found,
            });
        }
    });

    discovered
        .into_iter()
        .filter_map(|(_, status)| match status {
            DetectorStatus::Found(evidences) => Some(evidences),
            DetectorStatus::ToolAbsent | DetectorStatus::Failed(_) => None,
        })
        .flatten()
        .map(|mut ev| {
            let correlation = collector.collect(
                &ev.resource,
                ProbeBudget {
                    timeout: CLI_CORRELATION_TIMEOUT,
                },
            );
            merge_into(&mut ev, correlation);
            ev.collected_at = now;
            let decision = classify(&ev, policy_cfg, now);
            (ev, decision)
        })
        .collect()
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
pub fn build_detect_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
) -> DetectReport {
    DetectReport {
        candidates: candidates
            .iter()
            .map(|(ev, d)| {
                DetectCandidateReport::from_evidence_and_decision(
                    ev,
                    d,
                    resolve_action_for(ev, actions),
                )
            })
            .collect(),
    }
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
/// [`crate::actions::dry_run`] — never executing anything. A resource
/// with no resolvable action, or whose `Action::plan` refuses (e.g.
/// `ActionError::Unsupported`), is reported with `skip_reason` set rather
/// than silently omitted.
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
        Some(action) => match dry_run(action, ev) {
            Ok(plan) => CleanDryRunItem {
                resource_id,
                policy_label,
                action_id: Some(action.id().0),
                explain: Some(plan.explain),
                skip_reason: None,
            },
            Err(e) => CleanDryRunItem {
                resource_id,
                policy_label,
                action_id: Some(action.id().0),
                explain: None,
                skip_reason: Some(format!("{e:?}")),
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
/// here, BEFORE `actions.get`/`dry_run` are ever called for that item —
/// this is what makes it structurally impossible for a hallucinating (or
/// adversarial) LLM response to cause a protected resource's action to be
/// resolved, let alone rendered. See `tests/golden_llm_plan_protected_refusal.rs`
/// for an end-to-end proof.
pub fn build_llm_plan_report(
    candidates: &[(Evidence, PolicyDecision)],
    actions: &ActionRegistry,
    provider: &dyn LlmProvider,
) -> LlmPlanReport {
    let evidences: Vec<Evidence> = candidates.iter().map(|(ev, _)| ev.clone()).collect();
    let result = plan_with_llm(provider, &evidences, actions);

    let mut items = Vec::with_capacity(result.validated_items.len());
    for (resource, action_id, priority) in result.validated_items {
        let Some((ev, decision)) = candidates.iter().find(|(ev, _)| ev.resource == resource) else {
            // Unreachable in practice: `resource` came from `evidences`,
            // which is itself derived from `candidates` — but never panic
            // on a defensive fallback, matching this module's style.
            continue;
        };

        let resource_id = ev.resource.to_string();
        let policy_label = crate::reporting::label_for(decision).as_str();

        if decision.class == PolicyClass::Protected {
            items.push(LlmPlanItemReport {
                resource_id,
                policy_label,
                requested_action_id: None,
                priority,
                explain: None,
                skip_reason: Some(
                    "PROTECTED — no cleanup action is ever rendered for this resource".to_string(),
                ),
            });
            continue;
        }

        items.push(match actions.get(action_id.0) {
            Some(action) => match dry_run(action, ev) {
                Ok(plan) => LlmPlanItemReport {
                    resource_id,
                    policy_label,
                    requested_action_id: Some(action.id().0),
                    priority,
                    explain: Some(plan.explain),
                    skip_reason: None,
                },
                Err(e) => LlmPlanItemReport {
                    resource_id,
                    policy_label,
                    requested_action_id: Some(action.id().0),
                    priority,
                    explain: None,
                    skip_reason: Some(format!("{e:?}")),
                },
            },
            None => LlmPlanItemReport {
                resource_id,
                policy_label,
                requested_action_id: None,
                priority,
                explain: None,
                skip_reason: Some("no registered action for this action id".to_string()),
            },
        });
    }

    LlmPlanReport {
        items,
        dropped_unknown_resource: result.dropped_unknown_resource,
        dropped_unknown_action: result.dropped_unknown_action,
        provider_error: result.provider_error.map(|e| format!("{e:?}")),
    }
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
/// `#[allow(clippy::too_many_arguments)]`: eight parameters, every one an
/// independently fakeable seam (candidates/actions/collector/cfg/now are
/// exactly what makes this function unit-testable without touching the
/// real filesystem or clock) — precedented in this crate at
/// `emergency::run_emergency` and `detectors::discovery_evidence`.
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

    match authorize(decision.clone(), ev.fingerprint.clone(), consent.as_ref()) {
        Some(approval) => {
            let report = execute(action, &approval, collector, cfg, now);
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

    ExecuteReport {
        action_id: report.action.0,
        resource_id: report.resource.to_string(),
        outcome,
        failure_message,
        abort_reason,
        expected_reclaimed_bytes: report.expected_reclaimed_bytes.observed().copied(),
        actual_reclaimed_bytes: report.actual_reclaimed_bytes.observed().copied(),
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
    println!(
        "expected reclaimed:  {}",
        report
            .expected_reclaimed_bytes
            .map(crate::reporting::human_bytes)
            .unwrap_or_else(|| "unavailable".to_string())
    );
    println!(
        "actual reclaimed:    {}",
        report
            .actual_reclaimed_bytes
            .map(crate::reporting::human_bytes)
            .unwrap_or_else(|| "unavailable".to_string())
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
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
        let report = build_detect_report(&candidates, &actions);
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
        let detect_report = build_detect_report(&[(ev.clone(), decision.clone())], &actions);
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
        let detect_report = build_detect_report(&[(ev.clone(), decision.clone())], &actions);
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

        let report = build_llm_plan_report(&candidates, &actions, &provider);

        assert!(report.provider_error.is_none());
        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.policy_label, "PROTECTED");
        assert!(item.requested_action_id.is_none());
        assert!(item.explain.is_none());
        assert!(item.skip_reason.is_some());
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

        let report = build_llm_plan_report(&candidates, &actions, &provider);

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

        let report = build_llm_plan_report(&candidates, &actions, &provider);

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

        let report = build_llm_plan_report(&candidates, &actions, &provider);

        assert!(report.items.is_empty());
        assert!(report.provider_error.is_some());
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
        let resolution = resolve_and_execute(
            &candidates,
            &actions,
            &ev.resource.to_string(),
            action.id().0,
            None,
            &collector,
            &PolicyConfig::default(),
            now,
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

        fs::remove_dir_all(&root).ok();
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
