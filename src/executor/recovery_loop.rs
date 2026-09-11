//! Bounded closed-loop disk recovery (HORO-952): the orchestration behind
//! `glomeris free --target <threshold>`.
//!
//! This module wires together, without reimplementing any of them:
//! [`crate::monitor::FsStat`] (measurement), [`crate::detectors::DetectorRegistry`]
//! (discovery), [`crate::evidence::correlate::EvidenceCollector`]
//! (correlation refresh), [`crate::policy::classify`]/`crate::policy::approval::authorize`
//! (the AUTO_SAFE/ASK/PROTECTED decision), and [`crate::executor::execute`]
//! (HORO-951's fully-hardened, TOCTOU-safe deletion). The loop itself adds
//! only orchestration and bookkeeping — target/budget checks, one-candidate-
//! per-iteration selection, and a no-progress guard.
//!
//! Placement note: this lives under `crate::executor` (rather than a new
//! top-level `crate::recovery` module) because it is a thin caller of
//! `executor::execute` plus the other already-hardened modules — it does
//! not introduce a new safety-critical primitive of its own, so it does
//! not need its own top-level module boundary.
//!
//! Canonical safety invariant, unchanged: AI can recommend. Policy
//! decides. Executor verifies. Filesystem reality wins. This loop never
//! bypasses `policy::classify`/`policy::authorize`, never constructs an
//! `Approval` itself, and never touches the filesystem directly — every
//! mutation happens inside a real `executor::execute` call.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::actions::ActionRegistry;
use crate::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{ActionId, Evidence, NativeCleanup, ResourceId};
use crate::evidence::probe::ProbeOutcome;
use crate::executor::{execute, ExecutionOutcome};
use crate::monitor::{Clock, FsStat, FsUsage};
use crate::policy::approval::authorize;
use crate::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision, UserConsent};

/// Time budget applied to each candidate's correlation refresh pass
/// (step 5 of the loop). Mirrors `executor::REVALIDATION_TIMEOUT`'s
/// reasoning: this runs synchronously inside a bounded recovery loop, so
/// it must not stall indefinitely. Not reused directly because that
/// constant is private to `executor`.
const CANDIDATE_CORRELATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Consecutive "executed successfully but freed nothing measurable"
/// iterations before the loop gives up rather than spinning forever (see
/// [`StopReason::NoProgress`]).
const NO_PROGRESS_STREAK_THRESHOLD: u32 = 2;

/// Abstraction over "what time is it right now" for the wall-clock
/// timestamps [`crate::policy::classify`]/`crate::policy::approval::authorize`
/// need (`Evidence::collected_at`, `UserConsent::granted_at`). Distinct
/// from [`crate::monitor::Clock`] (which is `Instant`-based and used here
/// only for the `max_duration` budget check) because `classify` takes a
/// `SystemTime`, and no existing trait in this codebase provides an
/// injectable `SystemTime` source.
pub trait WallClock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// Real wall-clock time. Used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The disk-free goal a recovery run is trying to reach.
///
/// Both variants describe a target *state* of the filesystem (how much
/// free space should exist when the loop stops), not an amount to
/// reclaim — this is what makes "target already met, stop immediately"
/// (loop step 2) a coherent check before any action has run for either
/// variant. `AbsoluteBytes` byte multipliers used by the CLI parser (see
/// [`parse_free_target`]) are binary (1024-based: `5GB` == `5 * 1024^3`
/// bytes), matching the ticket's `5GB` / `5368709120` example pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FreeTarget {
    /// Target: at least this percentage (0.0..=100.0) of total capacity
    /// free.
    Percentage(f64),
    /// Target: at least this many bytes free.
    AbsoluteBytes(u64),
}

/// Bounds and behavior tunables for one recovery run.
#[derive(Debug, Clone)]
pub struct RecoveryConfig {
    pub target: FreeTarget,
    pub max_iterations: u32,
    pub max_actions: u32,
    pub max_duration: Duration,
    /// If `false`, `Ask`-classified candidates are reported as declined/
    /// skipped rather than executed — there is no interactive prompt in
    /// this MVP (see module docs and the PR's "Known limitations": real
    /// interactive approval UX is HORO-955's job). If `true`, an `Ask`
    /// candidate is auto-approved by constructing a `UserConsent` from the
    /// exact evidence/fingerprint just observed, purely as an MVP
    /// simplification for non-interactive runs.
    pub auto_approve_ask: bool,
}

/// Why a recovery run stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// `RecoveryConfig::target` was met (checked at the top of an
    /// iteration, before any candidate is considered).
    TargetReached,
    /// No `AutoSafe` candidate remains, and either no `Ask` candidate
    /// remains or `auto_approve_ask` is `false`.
    SafeExhausted,
    /// `max_iterations`, `max_actions`, or `max_duration` was reached.
    BudgetExceeded,
    /// The last `NO_PROGRESS_STREAK_THRESHOLD` consecutive successfully
    /// executed actions each measured zero (or unmeasurable) actual
    /// reclaimed bytes.
    NoProgress,
    /// The initial or a subsequent disk-usage measurement itself failed.
    Error(String),
}

/// Summary of one recovery run.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    pub stop_reason: StopReason,
    pub iterations_run: u32,
    pub actions_executed: u32,
    /// `Ask` candidates that were found but not executed — either because
    /// `auto_approve_ask` was `false`, or because `policy::authorize`
    /// unexpectedly refused, or because the resource's action id could
    /// not be resolved. Also incremented for a candidate whose executed
    /// action came back `Failed`/`AbortedByRevalidation` (it is skipped,
    /// never retried, for the remainder of this run).
    pub actions_declined_or_skipped: u32,
    /// Sum of ACTUAL (not expected) reclaimed bytes across all executed
    /// actions, per [`crate::executor::ExecutionReport::actual_reclaimed_bytes`].
    pub total_bytes_freed: u64,
    pub started_free_bytes: u64,
    pub final_free_bytes: u64,
}

/// Is `usage` already at or past `target`? Shared by the loop's own
/// target check and available to callers (e.g. the CLI) that want to
/// report progress without duplicating the arithmetic.
pub fn target_met(usage: &FsUsage, target: &FreeTarget) -> bool {
    match target {
        FreeTarget::Percentage(pct) => {
            if usage.total_bytes == 0 {
                return true;
            }
            usage.free_bytes as f64 >= (pct / 100.0) * usage.total_bytes as f64
        }
        FreeTarget::AbsoluteBytes(bytes) => usage.free_bytes >= *bytes,
    }
}

/// Best-effort "how big is this candidate" metric used to rank candidates
/// within a bucket: prefer `reclaimable_bytes` (the more precise probe),
/// falling back to `logical_bytes`, falling back to `0` when neither was
/// observed (such a candidate can still be selected — e.g. an
/// `AutoSafe` bucket with only unmeasured candidates — just not
/// preferentially).
fn candidate_size(ev: &Evidence) -> u64 {
    ev.reclaimable_bytes
        .observed()
        .or_else(|| ev.logical_bytes.observed())
        .copied()
        .unwrap_or(0)
}

/// Resolves the [`ActionId`] to execute for `ev`, if any: prefers
/// `Evidence::native_cleanup` when it names a real registered action,
/// otherwise falls back to [`ActionRegistry::find_for_kind`]. The
/// fallback matters today — see [`ActionRegistry::find_for_kind`]'s doc
/// comment — because every built-in detector currently always leaves
/// `native_cleanup` as `Unsupported`.
fn resolve_action_id(ev: &Evidence, registry: &ActionRegistry) -> Option<ActionId> {
    if let NativeCleanup::Available(id) = ev.native_cleanup {
        if registry.get(id.0).is_some() {
            return Some(id);
        }
    }
    registry.find_for_kind(ev.resource.kind).map(|a| a.id())
}

/// Flattens every detector's [`DetectorStatus::Found`] evidence into one
/// list, pairing each candidate with its resolved [`ActionId`] and
/// dropping any candidate with no resolvable action at all. `ToolAbsent`
/// (normal — the tool isn't installed) and `Failed` (a detector-level
/// probe failure, not a per-resource evidence gap) are silently dropped
/// here: this loop only ever acts on resources evidence was actually
/// found for.
fn candidates_with_actions(
    statuses: Vec<(crate::detectors::DetectorId, DetectorStatus)>,
    registry: &ActionRegistry,
) -> Vec<(Evidence, ActionId)> {
    statuses
        .into_iter()
        .filter_map(|(_, status)| match status {
            DetectorStatus::Found(evidence) => Some(evidence),
            DetectorStatus::ToolAbsent | DetectorStatus::Failed(_) => None,
        })
        .flatten()
        .filter_map(|ev| {
            let action_id = resolve_action_id(&ev, registry)?;
            Some((ev, action_id))
        })
        .collect()
}

/// One classified, sized candidate ready for approval.
struct ScoredCandidate {
    evidence: Evidence,
    action_id: ActionId,
    decision: PolicyDecision,
    size: u64,
}

/// Refreshes `ev`'s correlation fields via `collector` (reusing
/// [`crate::evidence::correlate::merge_into`] exactly as HORO-951's
/// deletion-time revalidation does) and re-stamps `collected_at` with the
/// injected wall-clock `now`, so [`crate::policy::classify`]'s staleness
/// check is evaluated against genuinely fresh evidence rather than
/// whatever moment the detector originally ran at.
fn refresh_evidence(
    mut ev: Evidence,
    collector: &dyn EvidenceCollector,
    now: SystemTime,
) -> Evidence {
    let correlation = collector.collect(
        &ev.resource,
        ProbeBudget {
            timeout: CANDIDATE_CORRELATION_TIMEOUT,
        },
    );
    merge_into(&mut ev, correlation);
    ev.collected_at = now;
    ev
}

/// Selects at most one candidate for this iteration: refreshes evidence
/// and classifies every discovered candidate (skipping any resource in
/// `excluded`, i.e. one this run has already given up on — see loop step
/// 9), then picks the largest-by-`candidate_size` `AutoSafe` candidate if
/// any exist; only when none exist, and only when `auto_approve_ask` is
/// set, picks the largest `Ask` candidate instead. Returns the number of
/// `Ask` candidates seen but not eligible for auto-approval, for the
/// caller's `actions_declined_or_skipped` bookkeeping.
fn select_candidate(
    candidates: Vec<(Evidence, ActionId)>,
    policy_cfg: &PolicyConfig,
    collector: &dyn EvidenceCollector,
    now: SystemTime,
    excluded: &HashSet<ResourceId>,
    auto_approve_ask: bool,
) -> (Option<ScoredCandidate>, u32) {
    let mut auto_safe: Vec<ScoredCandidate> = Vec::new();
    let mut ask: Vec<ScoredCandidate> = Vec::new();

    for (ev, action_id) in candidates {
        if excluded.contains(&ev.resource) {
            continue;
        }
        let refreshed = refresh_evidence(ev, collector, now);
        let decision = classify(&refreshed, policy_cfg, now);
        let size = candidate_size(&refreshed);
        match decision.class {
            PolicyClass::AutoSafe => auto_safe.push(ScoredCandidate {
                evidence: refreshed,
                action_id,
                decision,
                size,
            }),
            PolicyClass::Ask => ask.push(ScoredCandidate {
                evidence: refreshed,
                action_id,
                decision,
                size,
            }),
            PolicyClass::Protected => {}
        }
    }

    if let Some(best) = auto_safe.into_iter().max_by_key(|c| c.size) {
        return (Some(best), 0);
    }

    if auto_approve_ask {
        let best = ask.into_iter().max_by_key(|c| c.size);
        (best, 0)
    } else {
        (None, ask.len() as u32)
    }
}

#[cfg(test)]
mod select_candidate_tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        GitState, Recoverability, ResourceFingerprint, ResourceKind, ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use std::path::PathBuf;

    /// A collector that always reports clean, complete correlation — the
    /// baseline that makes complete/fresh evidence reach `AutoSafe`.
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

    /// A collector that reports the owning tool as live — pins the
    /// resulting decision to `Ask{OwningToolLive}`.
    struct ToolLiveCollector;

    impl EvidenceCollector for ToolLiveCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(true),
            }
        }
    }

    fn evidence(path: &str, kind: ResourceKind, logical_bytes: u64) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(logical_bytes),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(logical_bytes),
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Available(ActionId("test.action")),
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn picks_largest_auto_safe_candidate_by_size() {
        let small = evidence("/tmp/small/target", ResourceKind::CargoTargetDir, 100);
        let big = evidence("/tmp/big/target", ResourceKind::CargoTargetDir, 10_000);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, ask_skipped) = select_candidate(
            vec![
                (small, ActionId("test.action")),
                (big, ActionId("test.action")),
            ],
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            false,
        );

        assert_eq!(ask_skipped, 0);
        let selected = selected.expect("expected an AutoSafe candidate");
        assert_eq!(selected.decision.class, PolicyClass::AutoSafe);
        assert_eq!(selected.size, 10_000);
    }

    #[test]
    fn excludes_resources_already_given_up_on() {
        let only = evidence("/tmp/only/target", ResourceKind::CargoTargetDir, 100);
        let resource = only.resource.clone();
        let mut excluded = HashSet::new();
        excluded.insert(resource);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, _) = select_candidate(
            vec![(only, ActionId("test.action"))],
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &excluded,
            false,
        );

        assert!(selected.is_none());
    }

    #[test]
    fn ask_candidates_are_not_selected_when_auto_approve_ask_is_false() {
        let ev = evidence("/tmp/live/target", ResourceKind::XcodeDerivedData, 100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, ask_skipped) = select_candidate(
            vec![(ev, ActionId("test.action"))],
            &PolicyConfig::default(),
            &ToolLiveCollector,
            now,
            &HashSet::new(),
            false,
        );

        assert!(selected.is_none());
        assert_eq!(ask_skipped, 1);
    }

    #[test]
    fn ask_candidates_are_selected_when_auto_approve_ask_is_true() {
        let ev = evidence("/tmp/live/target", ResourceKind::XcodeDerivedData, 100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, ask_skipped) = select_candidate(
            vec![(ev, ActionId("test.action"))],
            &PolicyConfig::default(),
            &ToolLiveCollector,
            now,
            &HashSet::new(),
            true,
        );

        assert_eq!(ask_skipped, 0);
        let selected = selected.expect("expected an Ask candidate to be auto-approved");
        assert_eq!(selected.decision.class, PolicyClass::Ask);
    }

    #[test]
    fn protected_candidates_are_never_selected() {
        // Unknown resource kind is unconditionally Protected regardless
        // of evidence content.
        let ev = evidence("/tmp/unknown/thing", ResourceKind::Unknown, 100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, _) = select_candidate(
            vec![(ev, ActionId("test.action"))],
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            true,
        );

        assert!(selected.is_none());
    }
}

fn build_report(
    stop_reason: StopReason,
    iterations_run: u32,
    actions_executed: u32,
    actions_declined_or_skipped: u32,
    total_bytes_freed: u64,
    started_free_bytes: u64,
    final_free_bytes: u64,
) -> RecoveryReport {
    RecoveryReport {
        stop_reason,
        iterations_run,
        actions_executed,
        actions_declined_or_skipped,
        total_bytes_freed,
        started_free_bytes,
        final_free_bytes,
    }
}

/// Runs one bounded closed-loop recovery pass against `target_mount`,
/// stopping for exactly one of the [`StopReason`]s. See the module docs
/// for the safety invariant this never bypasses, and the ticket's loop
/// steps 1-12 for the per-iteration shape this implements.
///
/// Every I/O-touching dependency is injected so this function itself is
/// fully unit-testable with fakes: `fs_stat` (disk measurement),
/// `collector` (correlation refresh, reused by `execute`'s own
/// revalidation), `detector_registry` (discovery — production callers
/// pass `DetectorRegistry::builtin()`, but this must be a parameter
/// rather than constructed internally here: several built-in detectors,
/// e.g. `HomebrewDetector`, shell out to a real already-installed system
/// tool regardless of `discovery_ctx`, which would make this function
/// touch genuine machine state during a test no matter what `fs_stat`/
/// `collector` fakes it was given), `clock` (the `max_duration` budget
/// check), and `wall_clock` (the `SystemTime` `classify`/`authorize`
/// need). `action_registry` and `discovery_ctx` are plain data, not I/O
/// seams, but are still parameters rather than constructed internally so
/// a caller controls exactly what's registered/discoverable.
#[allow(clippy::too_many_arguments)]
pub fn run(
    config: &RecoveryConfig,
    fs_stat: &dyn FsStat,
    collector: &dyn EvidenceCollector,
    detector_registry: &DetectorRegistry,
    action_registry: &ActionRegistry,
    clock: &dyn Clock,
    wall_clock: &dyn WallClock,
    policy_cfg: &PolicyConfig,
    target_mount: &Path,
    discovery_ctx: &DiscoveryContext,
) -> RecoveryReport {
    let start_instant = clock.now();

    let started_usage = match fs_stat.stat(target_mount) {
        Ok(usage) => usage,
        Err(e) => {
            return build_report(
                StopReason::Error(format!("initial disk measurement failed: {e}")),
                0,
                0,
                0,
                0,
                0,
                0,
            );
        }
    };
    let started_free_bytes = started_usage.free_bytes;

    let mut iterations_run: u32 = 0;
    let mut actions_executed: u32 = 0;
    let mut actions_declined_or_skipped: u32 = 0;
    let mut total_bytes_freed: u64 = 0;
    let mut no_retry: HashSet<ResourceId> = HashSet::new();
    let mut no_progress_streak: u32 = 0;
    let mut last_free_bytes = started_free_bytes;

    loop {
        // 1. Measure.
        let usage = match fs_stat.stat(target_mount) {
            Ok(usage) => usage,
            Err(e) => {
                return build_report(
                    StopReason::Error(format!("disk measurement failed: {e}")),
                    iterations_run,
                    actions_executed,
                    actions_declined_or_skipped,
                    total_bytes_freed,
                    started_free_bytes,
                    last_free_bytes,
                );
            }
        };
        last_free_bytes = usage.free_bytes;

        // 2. Target already met?
        if target_met(&usage, &config.target) {
            return build_report(
                StopReason::TargetReached,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
            );
        }

        // 3. Budget check.
        let elapsed = clock.now().duration_since(start_instant);
        if iterations_run >= config.max_iterations
            || actions_executed >= config.max_actions
            || elapsed >= config.max_duration
        {
            return build_report(
                StopReason::BudgetExceeded,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
            );
        }

        // 4. Discover.
        let statuses = detector_registry.discover_all(discovery_ctx);
        let candidates = candidates_with_actions(statuses, action_registry);

        // 5-6. Refresh evidence, classify, select one candidate.
        let now = wall_clock.now();
        let (selected, ask_skipped) = select_candidate(
            candidates,
            policy_cfg,
            collector,
            now,
            &no_retry,
            config.auto_approve_ask,
        );
        actions_declined_or_skipped += ask_skipped;

        let Some(candidate) = selected else {
            return build_report(
                StopReason::SafeExhausted,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
            );
        };

        iterations_run += 1;

        let resource = candidate.evidence.resource.clone();
        let fingerprint = candidate.evidence.fingerprint.clone();
        let action_id = candidate.action_id;
        let decision_class = candidate.decision.class;

        // 7. Authorize. AutoSafe needs no consent; Ask needs a
        // UserConsent built from the exact evidence/fingerprint just
        // classified — a non-interactive auto-consent, only when
        // `auto_approve_ask` is true (see `RecoveryConfig::auto_approve_ask`
        // doc comment for why this is an MVP simplification, not real
        // interactive approval).
        let consent = if decision_class == PolicyClass::Ask {
            Some(UserConsent::new(resource.clone(), fingerprint.clone(), now))
        } else {
            None
        };
        let approval = match authorize(candidate.decision, fingerprint, consent.as_ref()) {
            Some(approval) => approval,
            None => {
                // Should not happen given how `consent` was just built to
                // match exactly, but never silently retry a candidate
                // authorize() refused.
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
                continue;
            }
        };

        // 8. Resolve the action (already resolved by `candidates_with_actions`
        // via `resolve_action_id`; re-look-up defensively rather than
        // trusting the id is still valid).
        let action = match action_registry.get(action_id.0) {
            Some(action) => action,
            None => {
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
                continue;
            }
        };

        // 8-9. Execute for real, reusing HORO-951's fully-hardened
        // revalidate-then-mutate path exactly as-is.
        let report = execute(action, &approval, collector, policy_cfg, now);

        match report.outcome {
            ExecutionOutcome::Succeeded => {
                actions_executed += 1;
                let progressed = match report.actual_reclaimed_bytes {
                    ProbeOutcome::Observed(bytes) => {
                        total_bytes_freed += bytes;
                        bytes > 0
                    }
                    ProbeOutcome::Unavailable(_) => false,
                };

                // 10. No-progress guard: consecutive successful executions
                // that freed nothing measurable.
                if progressed {
                    no_progress_streak = 0;
                } else {
                    no_progress_streak += 1;
                    if no_progress_streak >= NO_PROGRESS_STREAK_THRESHOLD {
                        let final_free_bytes = fs_stat
                            .stat(target_mount)
                            .map(|u| u.free_bytes)
                            .unwrap_or(last_free_bytes);
                        return build_report(
                            StopReason::NoProgress,
                            iterations_run,
                            actions_executed,
                            actions_declined_or_skipped,
                            total_bytes_freed,
                            started_free_bytes,
                            final_free_bytes,
                        );
                    }
                }
            }
            ExecutionOutcome::Failed(_) | ExecutionOutcome::AbortedByRevalidation(_) => {
                // 9. Never retry this exact candidate again this run.
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
                no_progress_streak = 0;
            }
            ExecutionOutcome::DryRun => {
                // execute() never returns this variant; defensively treat
                // it like a no-op rather than panicking.
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
            }
        }

        // 11. Loop back to step 1 — the top of the next iteration
        // re-measures disk usage rather than trusting expected bytes.
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::detectors::{Detector, DetectorId, DiscoveryContext};
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        ActionId as EvActionId, GitState, Recoverability, ResourceFingerprint, ResourceKind,
        ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A detector that always reports a fixed, caller-supplied
    /// [`DetectorStatus`] regardless of `ctx`. Used everywhere in this
    /// test module INSTEAD OF `DetectorRegistry::builtin()`'s real
    /// detectors: several of those (e.g. `HomebrewDetector`) shell out to
    /// a real, already-installed system tool and would discover a
    /// genuine resource on whatever machine runs the test, which combined
    /// with a real `ActionRegistry`/`executor::execute` could mutate real
    /// user state — exactly the risk `DetectorRegistry::from_detectors`
    /// exists to let tests avoid. See that constructor's doc comment.
    struct FakeDetector {
        found: Vec<Evidence>,
    }

    impl FakeDetector {
        fn found(evidence: Vec<Evidence>) -> Self {
            Self { found: evidence }
        }

        fn none() -> Self {
            Self { found: Vec::new() }
        }
    }

    impl Detector for FakeDetector {
        fn id(&self) -> DetectorId {
            DetectorId("fake_test_detector")
        }

        fn resource_kinds(&self) -> &'static [ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            // Re-checks each resource's path for existence on every call,
            // the same way a real detector's `canonicalize()` would stop
            // reporting a deleted resource — needed so a fixture actually
            // deleted by an earlier loop iteration doesn't keep getting
            // "rediscovered" with stale evidence on the next one. Only
            // ever touches paths this test itself constructed, never any
            // real machine state.
            let live: Vec<Evidence> = self
                .found
                .iter()
                .filter(|ev| match &ev.resource.locator {
                    ResourceLocator::Path(p) => p.exists(),
                    ResourceLocator::Tool { .. } => true,
                })
                .cloned()
                .collect();

            if live.is_empty() {
                DetectorStatus::ToolAbsent
            } else {
                DetectorStatus::Found(live)
            }
        }
    }

    fn fake_registry(evidence: Vec<Evidence>) -> DetectorRegistry {
        let detector: Box<dyn Detector> = if evidence.is_empty() {
            Box::new(FakeDetector::none())
        } else {
            Box::new(FakeDetector::found(evidence))
        };
        DetectorRegistry::from_detectors(vec![detector])
    }

    /// A real, disposable temp `node_modules` directory (with no files
    /// inside, so a real deletion measures zero bytes freed — see
    /// `stops_with_no_progress_after_two_zero_byte_successful_deletions`),
    /// paired with the `Evidence` a `FakeDetector` reports for it. The
    /// directory itself is real (the loop's own `executor::execute` call
    /// really deletes it, on purpose — that's what this loop is for); only
    /// *discovery* is faked.
    fn empty_node_modules_evidence(dir: &Path) -> Evidence {
        let node_modules = dir.join("node_modules");
        fs::create_dir_all(&node_modules).expect("create node_modules fixture");
        Evidence {
            resource: ResourceId::new(
                ResourceKind::NodeModules,
                ResourceLocator::Path(node_modules.clone()),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: crate::detectors::dev_ino_fingerprint(&node_modules),
                mtime: crate::detectors::probe_mtime(&node_modules)
                    .observed()
                    .copied(),
                tool_revision: None,
            },
            detector: DetectorId("fake_test_detector"),
            logical_bytes: ProbeOutcome::Observed(0),
            physical_bytes: None,
            // Post-HORO-994: `reclaimable_bytes == logical_bytes` is what a
            // real detector reports (see `detectors::discovery_evidence`'s
            // HORO-992 equivalence) AND what `executor::build_fresh_evidence`
            // now reproduces on revalidation — so a `NotAttempted` here
            // would be an artificial fixture mismatch that trips
            // `PolicyClassDowngraded` at execute-time instead of letting
            // the real (genuinely zero-byte) deletion run, which is this
            // test's actual purpose (see its own doc comment above).
            reclaimable_bytes: ProbeOutcome::Observed(0),
            last_modified: ProbeOutcome::Observed(SystemTime::now()),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::NodeModules.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Available(EvActionId("node.clean.node_modules")),
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::now(),
            sources: Vec::new(),
        }
    }

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-recovery-loop-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A fixed disk-usage reading, injected via [`FsStat`] — never touches
    /// the real filesystem.
    struct FixedFsStat(FsUsage);

    impl FsStat for FixedFsStat {
        fn stat(&self, _path: &Path) -> std::io::Result<FsUsage> {
            Ok(self.0)
        }
    }

    /// Always errors — used to prove the `StopReason::Error` path.
    struct FailingFsStat;

    impl FsStat for FailingFsStat {
        fn stat(&self, _path: &Path) -> std::io::Result<FsUsage> {
            Err(std::io::Error::other("simulated disk measurement failure"))
        }
    }

    /// A fixed `SystemTime` for every call — sufficient here because
    /// every candidate's `Evidence::collected_at` is re-stamped to this
    /// same instant immediately before `classify` runs, so the staleness
    /// check never sees an age greater than zero regardless of how many
    /// iterations occur.
    struct FixedWallClock(SystemTime);

    impl WallClock for FixedWallClock {
        fn now(&self) -> SystemTime {
            self.0
        }
    }

    /// A collector that always reports clean, complete correlation.
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

    fn base_config() -> RecoveryConfig {
        RecoveryConfig {
            target: FreeTarget::AbsoluteBytes(999_999_999),
            max_iterations: 50,
            max_actions: 50,
            max_duration: Duration::from_secs(60),
            auto_approve_ask: false,
        }
    }

    fn empty_discovery_ctx() -> DiscoveryContext {
        DiscoveryContext::new(PathBuf::from("/nonexistent-glomeris-test-home"))
    }

    #[test]
    fn stops_with_target_reached_before_scanning_when_already_met() {
        let usage = FsUsage::new(1_000, 990); // 99% free
        let config = RecoveryConfig {
            target: FreeTarget::Percentage(90.0),
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(
            &config,
            &FixedFsStat(usage),
            &CleanCollector,
            &detector_registry,
            &action_registry,
            &clock,
            &wall_clock,
            &PolicyConfig::default(),
            Path::new("/"),
            &ctx,
        );

        assert_eq!(report.stop_reason, StopReason::TargetReached);
        assert_eq!(report.iterations_run, 0);
        assert_eq!(report.actions_executed, 0);
        assert_eq!(report.started_free_bytes, 990);
        assert_eq!(report.final_free_bytes, 990);
    }

    #[test]
    fn stops_with_safe_exhausted_when_no_candidates_exist() {
        let usage = FsUsage::new(1_000_000_000, 100); // far from any target
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(
            &config,
            &FixedFsStat(usage),
            &CleanCollector,
            &detector_registry,
            &action_registry,
            &clock,
            &wall_clock,
            &PolicyConfig::default(),
            Path::new("/"),
            &ctx,
        );

        assert_eq!(report.stop_reason, StopReason::SafeExhausted);
        assert_eq!(report.iterations_run, 0);
        assert_eq!(report.actions_executed, 0);
    }

    #[test]
    fn stops_with_budget_exceeded_when_max_iterations_is_zero() {
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = RecoveryConfig {
            max_iterations: 0,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(
            &config,
            &FixedFsStat(usage),
            &CleanCollector,
            &detector_registry,
            &action_registry,
            &clock,
            &wall_clock,
            &PolicyConfig::default(),
            Path::new("/"),
            &ctx,
        );

        assert_eq!(report.stop_reason, StopReason::BudgetExceeded);
        assert_eq!(report.iterations_run, 0);
    }

    #[test]
    fn stops_with_error_when_initial_measurement_fails() {
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(
            &config,
            &FailingFsStat,
            &CleanCollector,
            &detector_registry,
            &action_registry,
            &clock,
            &wall_clock,
            &PolicyConfig::default(),
            Path::new("/"),
            &ctx,
        );

        match report.stop_reason {
            StopReason::Error(_) => {}
            other => panic!("expected StopReason::Error, got {other:?}"),
        }
    }

    /// Two real, disposable temp `node_modules` directories with no files
    /// inside, reported by a [`FakeDetector`] (never `DetectorRegistry::
    /// builtin()`'s real detectors — see that struct's doc comment for
    /// why). `reclaimable_bytes` matches `logical_bytes` here (post-HORO-994
    /// this is also what `executor::build_fresh_evidence` reproduces on
    /// revalidation, so plan-time and execute-time classification agree —
    /// see `empty_node_modules_evidence`), so the resulting evidence
    /// classifies as `AutoSafe` and is executed directly; `auto_approve_ask:
    /// true` in this test's config is inert for this fixture, kept only to
    /// match this suite's other `base_config()` overrides. Each directory
    /// being empty means `NodeCleanNodeModules`'s real deletion measures
    /// `actual_reclaimed_bytes == Observed(0)` — "succeeded but freed
    /// nothing measurable" — twice in a row, exactly what the no-progress
    /// guard exists to catch.
    #[test]
    fn stops_with_no_progress_after_two_zero_byte_successful_deletions() {
        let root_a = make_temp_dir("no-progress-a");
        let root_b = make_temp_dir("no-progress-b");
        let ev_a = empty_node_modules_evidence(&root_a);
        let ev_b = empty_node_modules_evidence(&root_b);
        let node_modules_a = root_a.join("node_modules");
        let node_modules_b = root_b.join("node_modules");

        let usage = FsUsage::new(1_000_000_000, 100); // never meets target
        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev_a, ev_b]);
        let ctx = empty_discovery_ctx();

        let report = run(
            &config,
            &FixedFsStat(usage),
            &CleanCollector,
            &detector_registry,
            &action_registry,
            &clock,
            &wall_clock,
            &PolicyConfig::default(),
            Path::new("/"),
            &ctx,
        );

        assert_eq!(report.stop_reason, StopReason::NoProgress);
        assert_eq!(report.iterations_run, 2);
        assert_eq!(report.actions_executed, 2);
        assert_eq!(report.total_bytes_freed, 0);
        assert!(!node_modules_a.exists());
        assert!(!node_modules_b.exists());

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }
}

/// Parses a `--target` CLI value into a [`FreeTarget`].
///
/// Format (deliberately minimal — see the PR's "Known limitations"):
/// - A trailing `%` is a [`FreeTarget::Percentage`], e.g. `"20%"`. Must
///   parse as a number in `0.0..=100.0`.
/// - Otherwise the value is a [`FreeTarget::AbsoluteBytes`]. An optional
///   case-insensitive `KB`/`MB`/`GB`/`TB` suffix uses binary (1024-based)
///   multipliers (`"5GB"` == `5 * 1024^3` bytes, matching the ticket's
///   `5GB`/`5368709120` example pair); a bare trailing `B` means "no
///   multiplier"; no suffix at all is also read as a raw byte count
///   (e.g. `"5368709120"`).
/// - No support for fractional shorthand combinations, negative values,
///   or any unit beyond TB.
pub fn parse_free_target(input: &str) -> Result<FreeTarget, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty --target value".to_string());
    }

    if let Some(pct) = trimmed.strip_suffix('%') {
        let value: f64 = pct
            .trim()
            .parse()
            .map_err(|_| format!("invalid percentage in --target value: {input}"))?;
        if !(0.0..=100.0).contains(&value) {
            return Err(format!("--target percentage out of range 0-100: {input}"));
        }
        return Ok(FreeTarget::Percentage(value));
    }

    let upper = trimmed.to_ascii_uppercase();
    let (numeric_part, multiplier): (&str, u64) = if let Some(p) = upper.strip_suffix("TB") {
        (p, 1024u64.pow(4))
    } else if let Some(p) = upper.strip_suffix("GB") {
        (p, 1024u64.pow(3))
    } else if let Some(p) = upper.strip_suffix("MB") {
        (p, 1024u64.pow(2))
    } else if let Some(p) = upper.strip_suffix("KB") {
        (p, 1024)
    } else if let Some(p) = upper.strip_suffix('B') {
        (p, 1)
    } else {
        (upper.as_str(), 1)
    };

    let value: f64 = numeric_part
        .trim()
        .parse()
        .map_err(|_| format!("invalid byte amount in --target value: {input}"))?;
    if value < 0.0 {
        return Err(format!(
            "--target byte amount must not be negative: {input}"
        ));
    }

    Ok(FreeTarget::AbsoluteBytes(
        (value * multiplier as f64) as u64,
    ))
}

#[cfg(test)]
mod parse_free_target_tests {
    use super::*;

    #[test]
    fn parses_percentage_form() {
        assert_eq!(parse_free_target("20%"), Ok(FreeTarget::Percentage(20.0)));
    }

    #[test]
    fn parses_percentage_with_fraction() {
        assert_eq!(parse_free_target("12.5%"), Ok(FreeTarget::Percentage(12.5)));
    }

    #[test]
    fn rejects_percentage_out_of_range() {
        assert!(parse_free_target("150%").is_err());
    }

    #[test]
    fn parses_raw_byte_count_with_no_suffix() {
        assert_eq!(
            parse_free_target("5368709120"),
            Ok(FreeTarget::AbsoluteBytes(5_368_709_120))
        );
    }

    #[test]
    fn parses_gb_suffix_as_binary_gigabytes() {
        assert_eq!(
            parse_free_target("5GB"),
            Ok(FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn parses_lowercase_suffix() {
        assert_eq!(
            parse_free_target("5gb"),
            Ok(FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn parses_kb_and_mb_and_tb_suffixes() {
        assert_eq!(
            parse_free_target("1KB"),
            Ok(FreeTarget::AbsoluteBytes(1024))
        );
        assert_eq!(
            parse_free_target("1MB"),
            Ok(FreeTarget::AbsoluteBytes(1024 * 1024))
        );
        assert_eq!(
            parse_free_target("1TB"),
            Ok(FreeTarget::AbsoluteBytes(1024u64.pow(4)))
        );
    }

    #[test]
    fn rejects_negative_byte_amount() {
        assert!(parse_free_target("-5GB").is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_free_target("").is_err());
        assert!(parse_free_target("   ").is_err());
    }

    #[test]
    fn rejects_garbage_input() {
        assert!(parse_free_target("not-a-target").is_err());
    }
}

#[cfg(test)]
mod types_tests {
    use super::*;

    #[test]
    fn percentage_target_met_when_free_fraction_meets_threshold() {
        let usage = FsUsage::new(100, 20);
        assert!(target_met(&usage, &FreeTarget::Percentage(20.0)));
        assert!(!target_met(&usage, &FreeTarget::Percentage(20.01)));
    }

    #[test]
    fn absolute_target_met_when_free_bytes_meets_threshold() {
        let usage = FsUsage::new(1_000, 500);
        assert!(target_met(&usage, &FreeTarget::AbsoluteBytes(500)));
        assert!(!target_met(&usage, &FreeTarget::AbsoluteBytes(501)));
    }

    #[test]
    fn percentage_target_on_zero_capacity_filesystem_is_trivially_met() {
        // Degenerate zero-total filesystem: nothing to free, so the
        // target can't meaningfully be unmet.
        let usage = FsUsage::new(0, 0);
        assert!(target_met(&usage, &FreeTarget::Percentage(50.0)));
    }
}
