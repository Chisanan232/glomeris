//! One bounded Autopilot run (HORO-1310).
//!
//! This is the only place in the crate where a model's output influences
//! which real deletion happens first. What it does with that output is
//! deliberately the least it could do and still be useful: **reorder**.
//!
//! ```text
//! candidates (local discovery)
//!     |
//!     +-- order_candidates(model_order)   <- the ONLY model influence
//!     |
//!     v
//! for each: correlate -> classify -> admit -> authorize -> execute
//!           (probe)     (policy)    (gate)   (Approval)   (TOCTOU)
//! ```
//!
//! Why ordering and nothing else is a safe amount of authority to hand a
//! model: reordering a list cannot add a member to it. Every candidate the
//! loop can reach was discovered locally by a detector, and every one of
//! them still has to pass [`crate::policy::classify`] and
//! [`super::gate::admit`] on its own merits. A maximally adversarial
//! response can therefore change *which* admissible action is tried first,
//! and can waste a run's budget on a small resource instead of a large
//! one — an annoyance, bounded by the envelope — but it cannot make an
//! inadmissible candidate admissible.
//!
//! Two consequences worth stating explicitly, because both are
//! load-bearing and neither is obvious from the call graph:
//!
//! - [`ValidatedPlanItem::action_id`] is deliberately **ignored** here.
//!   The action comes from [`ActionRegistry::ids_for_kind`] on the
//!   candidate's own kind, so a model cannot steer *which* action runs
//!   against a resource even among the registered, already-safe ones.
//! - [`ValidatedPlanItem::model_reason`] is never read here at all. It is
//!   display-only text; this module makes no decision from it.
//!
//! ## What is recorded, and where
//!
//! Real executions are appended to `actions.jsonl` via
//! [`append_audit_record`], with `source` naming the authority
//! (`autopilot_auto_safe` / `autopilot_preauthorized_ask`) and
//! `model_rank` recording where the model put the resource. Refusals are
//! **not** written there: that file is the execution log
//! `glomeris actions history` reads, and filling it with non-executions
//! would corrupt that reading. Refusals are in [`AutopilotReport`], which
//! is what the run prints — so a `PROTECTED` refusal is observable
//! (HORO-1310 AC 10) without pretending an action occurred.
//!
//! ## Known limitation: pre-authorized `ASK` cannot complete today
//!
//! A pre-authorized `Ask` candidate is admitted, authorized with real
//! consent, and then *always* aborts inside [`execute`]'s deletion-time
//! revalidation with [`AbortReason::PolicyClassDowngraded`]. Nothing is
//! deleted. The cause is upstream of this module:
//! `Ask`/[`crate::policy::ReasonCode::RebuildCostHigh`] arises only from a
//! **per-instance** [`crate::evidence::model::Regenerability::NotRegenerable`],
//! while `executor::build_fresh_evidence` rebuilds regenerability from the
//! resource *kind*'s static default — so the fresh classification lands on
//! `AutoSafe` and the class comparison trips.
//!
//! This is left as-is deliberately. The failure direction is the safe one
//! (refuse, mutate nothing), no HORO-1310 acceptance criterion needs the
//! `ASK` path to complete, and "make `build_fresh_evidence` carry a
//! per-instance judgment" is a change to the TOCTOU anchor that every
//! command shares — not something to smuggle into an Autopilot ticket. It
//! is asserted by
//! `tests::a_preauthorized_ask_still_aborts_at_deletion_time_revalidation`
//! rather than left undiscovered, so whoever does fix it upstream gets a
//! failing test pointing here.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::actions::llm::ValidatedPlanItem;
use crate::actions::ActionRegistry;
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{Evidence, ResourceKind};
use crate::executor::{execute, AbortReason, ExecutionOutcome};
use crate::monitor::persistence::{append_audit_record, ActionSource, AuditRecord};
use crate::monitor::PressureState;
use crate::policy::approval::{authorize, UserConsent};
use crate::policy::{classify, PolicyConfig};
use crate::reporting::{human_bytes, label_for};

use super::envelope::AutopilotEnvelope;
use super::gate::{admit, admits_pressure, Admission, BudgetLedger, RefusalReason};

/// Per-`collect()` probe timeout while revalidating one candidate, matching
/// [`crate::emergency`]'s own choice and for the same reason: Autopilot is
/// a bounded background-ish operation, not an interactive investigation. A
/// single `collect()` may still spend roughly `timeout * 4` in the worst
/// case (see [`crate::evidence::correlate`]) — bounded, not eliminated,
/// which is why the run's wall-clock budget is re-checked per candidate.
const CANDIDATE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What happened to one candidate in a run. Every variant is terminal for
/// that candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutopilotItemOutcome {
    /// The gate refused it. Carries the reason so the report can explain
    /// itself rather than saying "skipped".
    Refused(RefusalReason),
    /// Admitted, but no cleanup action is registered for the kind — a
    /// wiring gap, not a policy denial. Kept distinct so the two never
    /// blur together in a report.
    NoRegisteredAction,
    /// Admitted, but more than one registered action applies to the kind.
    /// Refused rather than guessed: picking one would be this module
    /// inventing authority the registry did not express.
    AmbiguousAction {
        candidates: usize,
    },
    /// Admitted, and exactly one action is registered — but
    /// [`crate::executor::structural_refusal`] would reject that action for
    /// this resource on sight, so there was nothing to attempt
    /// (HORO-1360/1359).
    ///
    /// Deliberately not [`Self::Failed`]: nothing ran. No attempt was
    /// charged and no budget was spent, because spending one of a run's
    /// scarce attempts on an action already known to be unrunnable would
    /// starve the candidates that could actually have freed something. A
    /// report that said "failed" here would also send a reader looking for
    /// a transient cause that does not exist.
    Ineligible {
        reason: String,
    },
    /// Admitted and authorized; nothing was executed because this was a
    /// dry run. Still charged against the budget, so the plan shown is the
    /// real bounded plan.
    Planned,
    Succeeded {
        reclaimed_bytes: Option<u64>,
    },
    Failed(String),
    /// [`crate::executor::execute`]'s deletion-time revalidation tripped.
    /// Nothing was mutated. Reaching this is the system working: the
    /// filesystem changed under an approval and the executor noticed.
    AbortedByRevalidation(AbortReason),
}

impl AutopilotItemOutcome {
    /// `true` only when bytes were actually reclaimed from disk.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    /// The audit-log `outcome` tag for a real execution, or `None` for the
    /// variants that never executed anything (and so never produce an
    /// audit record).
    fn audit_tags(&self) -> Option<(&'static str, Option<String>)> {
        match self {
            Self::Succeeded { .. } => Some(("succeeded", None)),
            Self::Failed(_) => Some(("failed", None)),
            Self::AbortedByRevalidation(reason) => {
                Some(("aborted_by_revalidation", Some(format!("{reason:?}"))))
            }
            Self::Refused(_)
            | Self::NoRegisteredAction
            | Self::AmbiguousAction { .. }
            | Self::Ineligible { .. }
            | Self::Planned => None,
        }
    }
}

/// One candidate's line in the run report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotRunItem {
    /// Canonical local identity string, the same form the audit log uses.
    pub resource_id: String,
    pub kind: ResourceKind,
    /// What [`crate::policy::classify`] decided, as the user-facing label
    /// — including `UNKNOWN_INCOMPLETE`, which is a label rather than a
    /// [`crate::policy::PolicyClass`] variant.
    pub policy_label: &'static str,
    /// Where the model ranked this resource, or `None` if no model named
    /// it. 1-based: `Some(1)` is the model's first choice, because this
    /// number is printed in a report and written to the audit log, and a
    /// rank that starts at zero invites exactly one bug — a display that
    /// adds one and a log that does not, disagreeing about the same action.
    /// Separate axis from `policy_label`: a model's ranking never reaches
    /// `classify`.
    pub model_rank: Option<u32>,
    /// What the *evidence* says would be reclaimed — never a model's
    /// estimate, never the action's claim.
    pub expected_reclaim_bytes: Option<u64>,
    pub outcome: AutopilotItemOutcome,
}

/// Result of one `run_autopilot` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotReport {
    pub dry_run: bool,
    /// Every candidate the run considered, in the order it considered them
    /// — i.e. already reordered by the model, if a plan was supplied.
    pub items: Vec<AutopilotRunItem>,
    pub actions_attempted: u32,
    pub actions_succeeded: u32,
    pub total_bytes_freed: u64,
    /// Set when the run stopped before considering every candidate, with
    /// the budget (or pressure floor) that stopped it. `None` means every
    /// candidate got a decision.
    pub stopped_early: Option<RefusalReason>,
    /// Rendered envelope, so the report can state the authority it ran
    /// under rather than leaving the reader to go look it up.
    pub envelope: Vec<String>,
}

impl AutopilotReport {
    fn empty(envelope: &AutopilotEnvelope, dry_run: bool) -> Self {
        Self {
            dry_run,
            items: Vec::new(),
            actions_attempted: 0,
            actions_succeeded: 0,
            total_bytes_freed: 0,
            stopped_early: None,
            envelope: envelope.describe(),
        }
    }

    /// Candidates the gate refused for a safety reason (not a budget one).
    pub fn safety_refusals(&self) -> usize {
        self.items
            .iter()
            .filter(|item| match &item.outcome {
                AutopilotItemOutcome::Refused(reason) => reason.is_safety_refusal(),
                _ => false,
            })
            .count()
    }
}

impl std::fmt::Display for AutopilotReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.dry_run {
            writeln!(f, "glomeris autopilot --dry-run (nothing was deleted):")?;
        } else {
            writeln!(f, "glomeris autopilot report:")?;
        }
        writeln!(f, "  authorized to:")?;
        for line in &self.envelope {
            writeln!(f, "    {line}")?;
        }
        if self.items.is_empty() {
            writeln!(f, "  candidates: none")?;
        } else {
            writeln!(f, "  candidates ({}):", self.items.len())?;
            for item in &self.items {
                let rank = match item.model_rank {
                    // Printed verbatim. The audit log records this same
                    // number, and a reader comparing the two must not have
                    // to know which one is offset.
                    Some(rank) => format!(" [AI rank {rank}]"),
                    None => String::new(),
                };
                writeln!(f, "    {} ({}){rank}", item.resource_id, item.policy_label)?;
                writeln!(f, "      {}", describe_outcome(&item.outcome))?;
            }
        }
        writeln!(f, "  actions attempted: {}", self.actions_attempted)?;
        writeln!(f, "  actions succeeded: {}", self.actions_succeeded)?;
        writeln!(
            f,
            "  bytes freed:       {}",
            human_bytes(self.total_bytes_freed)
        )?;
        match &self.stopped_early {
            Some(reason) => writeln!(f, "  stopped early:     {reason}"),
            None => writeln!(f, "  stopped early:     no"),
        }
    }
}

fn describe_outcome(outcome: &AutopilotItemOutcome) -> String {
    match outcome {
        AutopilotItemOutcome::Refused(reason) => format!("refused: {reason}"),
        AutopilotItemOutcome::NoRegisteredAction => {
            "skipped: no cleanup action is registered for this resource kind".to_string()
        }
        AutopilotItemOutcome::AmbiguousAction { candidates } => format!(
            "skipped: {candidates} registered actions apply to this kind; \
             Autopilot will not choose between them"
        ),
        AutopilotItemOutcome::Ineligible { reason } => {
            format!("skipped, no attempt spent: {reason}")
        }
        AutopilotItemOutcome::Planned => "would run (dry run)".to_string(),
        AutopilotItemOutcome::Succeeded { reclaimed_bytes } => match reclaimed_bytes {
            Some(bytes) => format!("reclaimed {}", human_bytes(*bytes)),
            None => "succeeded; reclaimed bytes not measurable".to_string(),
        },
        AutopilotItemOutcome::Failed(message) => format!("failed: {message}"),
        AutopilotItemOutcome::AbortedByRevalidation(reason) => {
            format!("aborted at deletion-time revalidation: {reason:?}; nothing was deleted")
        }
    }
}

/// Everything one run needs. A struct rather than a parameter list because
/// there are eleven independently fakeable seams here and
/// `#[allow(clippy::too_many_arguments)]` would only hide that, not fix it
/// — and unlike [`crate::emergency::run_emergency`]'s list, several of
/// these need naming at the call site to be readable (`dry_run` next to
/// `observed_pressure` next to `now`).
pub struct AutopilotRunRequest<'a> {
    pub envelope: &'a AutopilotEnvelope,
    /// Locally discovered candidates. Autopilot never discovers on its own
    /// behalf — the caller passes in what detectors found, exactly as
    /// `detect` would show the user.
    pub candidates: Vec<Evidence>,
    /// A validated model plan, or `&[]` for a rule-ordered run. Autopilot
    /// works with no model configured at all; the model is an
    /// optimisation, never a dependency.
    pub model_order: &'a [ValidatedPlanItem],
    pub collector: &'a dyn EvidenceCollector,
    pub actions: &'a ActionRegistry,
    pub policy: &'a PolicyConfig,
    /// The machine's current pressure, or `None` if it could not be read.
    /// `None` fails closed against a `min_pressure` floor — see
    /// [`admits_pressure`].
    pub observed_pressure: Option<PressureState>,
    pub now: SystemTime,
    /// Start of the run's wall-clock budget. Passed in rather than read so
    /// the caller decides what "the run" includes (e.g. whether discovery
    /// counts against it).
    pub started_at: Instant,
    pub audit_log_path: &'a Path,
    /// `true` renders the bounded plan and executes nothing.
    pub dry_run: bool,
}

/// Reorders `candidates` so the ones a model named come first, in the
/// model's order, and returns each with its 1-based model rank.
///
/// Pure, and a permutation by construction: it sorts the vector it was
/// given and never pushes to or removes from it, so no model plan — however
/// adversarial — can add a candidate that was not discovered locally or
/// drop one that was. A resource named twice keeps its first rank; a named
/// resource that is not among the candidates contributes nothing at all.
/// Unranked candidates keep their original relative order (the sort is
/// stable), so a run with no model behaves exactly like a run whose model
/// returned an empty plan.
///
/// "The model's order" means the order of `items` in the plan, not the
/// `priority` field on them. `ValidatedPlanItem::priority` is documented as
/// advisory with nothing ranking on it, and nothing in this codebase — no
/// prompt, no schema doc — ever pinned down whether 1 means most urgent or
/// least. Sorting on a number whose direction was never specified would be
/// inventing a semantics and then depending on it; the array a provider
/// returned is unambiguous.
pub fn order_candidates(
    mut candidates: Vec<Evidence>,
    model_order: &[ValidatedPlanItem],
) -> Vec<(Option<u32>, Evidence)> {
    // `ResourceId` is deliberately not `PartialEq`/`Hash` (see its own
    // docs), so correlate on its canonical Display form — the same string
    // the audit log records.
    let ranked: Vec<String> = model_order
        .iter()
        .map(|item| item.resource.to_string())
        .collect();
    let rank_of = |evidence: &Evidence| -> Option<u32> {
        let id = evidence.resource.to_string();
        ranked
            .iter()
            .position(|candidate| *candidate == id)
            // 1-based: the rank is printed and logged, so it is the
            // human's ordinal, not an index into anything.
            .map(|position| position as u32 + 1)
    };

    candidates.sort_by_key(|evidence| match rank_of(evidence) {
        // `(0, rank)` before `(1, 0)`: every ranked candidate sorts ahead
        // of every unranked one.
        Some(rank) => (0u8, rank),
        None => (1u8, 0),
    });

    candidates
        .into_iter()
        .map(|evidence| (rank_of(&evidence), evidence))
        .collect()
}

/// Runs Autopilot once, bounded by the envelope.
pub fn run_autopilot(request: AutopilotRunRequest<'_>) -> AutopilotReport {
    let AutopilotRunRequest {
        envelope,
        candidates,
        model_order,
        collector,
        actions,
        policy,
        observed_pressure,
        now,
        started_at,
        audit_log_path,
        dry_run,
    } = request;

    let mut report = AutopilotReport::empty(envelope, dry_run);

    // Revocation is checked per candidate by the gate too; checking it here
    // as well is what makes a revoked envelope produce an empty report
    // rather than a list of identical refusals.
    if !envelope.is_enabled() {
        report.stopped_early = Some(RefusalReason::AutopilotRevoked);
        return report;
    }
    // A run-level property, so it is reported once rather than once per
    // candidate.
    if let Err(reason) = admits_pressure(envelope, observed_pressure) {
        report.stopped_early = Some(reason);
        return report;
    }

    let mut ledger = BudgetLedger::for_envelope(envelope);

    for (model_rank, mut evidence) in order_candidates(candidates, model_order) {
        let correlation = collector.collect(
            &evidence.resource,
            ProbeBudget {
                timeout: CANDIDATE_PROBE_TIMEOUT,
            },
        );
        merge_into(&mut evidence, correlation);
        // Stamp with the freshly-correlated timestamp before classifying,
        // exactly as `emergency::process_candidate` and
        // `executor::build_fresh_evidence` do: this is evidence just
        // observed, so it is not stale by definition. Nothing else in
        // `classify`'s gauntlet is bypassed.
        evidence.collected_at = now;

        let kind = evidence.resource.kind;
        let resource_id = evidence.resource.to_string();
        let decision = classify(&evidence, policy, now);
        let policy_label = label_for(&decision).as_str();
        let expected_reclaim_bytes = evidence.reclaimable_bytes.observed().copied();

        let item = |outcome: AutopilotItemOutcome| AutopilotRunItem {
            resource_id: resource_id.clone(),
            kind,
            policy_label,
            model_rank,
            expected_reclaim_bytes,
            outcome,
        };

        let admission = admit(
            envelope,
            &ledger,
            &decision,
            expected_reclaim_bytes,
            started_at.elapsed(),
        );
        let requires_consent = match admission {
            Admission::Admitted { requires_consent } => requires_consent,
            Admission::Refused(reason) => {
                report
                    .items
                    .push(item(AutopilotItemOutcome::Refused(reason)));
                // An exhausted action or time budget will refuse every
                // remaining candidate identically, so stop and say so once.
                // A byte-budget refusal is per-candidate — a smaller
                // candidate later in the list may still fit — so the run
                // continues.
                if matches!(
                    reason,
                    RefusalReason::ActionBudgetExhausted { .. }
                        | RefusalReason::TimeBudgetExhausted { .. }
                ) {
                    report.stopped_early = Some(reason);
                    break;
                }
                continue;
            }
        };

        // The action comes from the registry, keyed only on the resource's
        // own kind — never from the model's `action_id`, and never from a
        // map maintained here. Exactly one match is required: zero is a
        // wiring gap and more than one is ambiguity this module refuses to
        // resolve on the user's behalf.
        let action_id = match actions.ids_for_kind(kind).as_slice() {
            [only] => *only,
            [] => {
                report
                    .items
                    .push(item(AutopilotItemOutcome::NoRegisteredAction));
                continue;
            }
            many => {
                report
                    .items
                    .push(item(AutopilotItemOutcome::AmbiguousAction {
                        candidates: many.len(),
                    }));
                continue;
            }
        };
        let Some(action) = actions.get(action_id) else {
            // `ids_for_kind` returned it, so `get` cannot miss it —
            // defensive, and still reported rather than silently dropped.
            report
                .items
                .push(item(AutopilotItemOutcome::NoRegisteredAction));
            continue;
        };

        // An action being registered for the kind is not the same claim as
        // it being runnable against THIS resource (HORO-1360). Asked here,
        // before an attempt is charged, so a `homebrew.cleanup.cache` in an
        // allowlisted envelope is reported as ineligible instead of
        // consuming one of the run's attempts to produce a failure that was
        // certain in advance.
        //
        // Safe to plan at this point, and only at this point: `admit`
        // refuses `PolicyClass::Protected` above, so nothing reaching here
        // is protected. That is why the policy-free `plan_refusal` is the
        // right half of the predicate to call — passing a decision would
        // imply this site does the Protected ordering itself, which it does
        // not; the gate does.
        if let Some(reason) = crate::actionability::plan_refusal(action, &evidence) {
            report
                .items
                .push(item(AutopilotItemOutcome::Ineligible { reason }));
            continue;
        }

        let fingerprint = evidence.fingerprint.clone();
        // For a pre-authorized `Ask`, the consent handed to `authorize` is
        // built HERE, from the fingerprint just observed — not parsed from
        // a file, not supplied by a caller, and not derivable from anything
        // a model said. The envelope's pre-authorization is what the user
        // narrowly authorized (this kind, this reason code); this is its
        // per-resource materialization, and it is pinned tightly enough
        // that `authorize`'s own equality checks still bind and
        // `execute`'s TOCTOU revalidation still runs unchanged.
        let consent = if requires_consent {
            Some(UserConsent::new(
                decision.resource.clone(),
                fingerprint.clone(),
                now,
            ))
        } else {
            None
        };
        let Some(approval) = authorize(decision, fingerprint, consent.as_ref()) else {
            // The gate already refused everything `authorize` refuses, so
            // this is unreachable today. Report it as a refusal rather than
            // asserting, so a future divergence fails closed and visibly.
            report.items.push(item(AutopilotItemOutcome::Refused(
                RefusalReason::AskNotPreauthorized,
            )));
            continue;
        };

        // Counted for every action ATTEMPTED, not every one that worked: a
        // failed or aborted attempt still spent a slot and still took time,
        // and a budget that only counted successes would let a run retry
        // its way past its own ceiling.
        report.actions_attempted += 1;

        if dry_run {
            // Deliberately authorized above and then dropped: a dry run
            // that skipped authorization could show the user a plan the
            // real run would refuse. Charged like a real attempt so the
            // rendered plan is the genuinely bounded one.
            drop(approval);
            ledger.charge(expected_reclaim_bytes.unwrap_or(0), 0);
            report.items.push(item(AutopilotItemOutcome::Planned));
            continue;
        }

        let exec = execute(action, &approval, collector, policy, now);
        let actual = exec.actual_reclaimed_bytes.observed().copied();
        let outcome = match exec.outcome {
            ExecutionOutcome::Succeeded => {
                report.actions_succeeded += 1;
                if let Some(bytes) = actual {
                    report.total_bytes_freed += bytes;
                }
                AutopilotItemOutcome::Succeeded {
                    reclaimed_bytes: actual,
                }
            }
            ExecutionOutcome::Failed(message) => AutopilotItemOutcome::Failed(message),
            ExecutionOutcome::AbortedByRevalidation(reason) => {
                AutopilotItemOutcome::AbortedByRevalidation(reason)
            }
            // `execute()` never returns this variant — only `dry_run()`
            // does, and this module does not call it (see the `dry_run`
            // branch above, which returns before reaching here).
            ExecutionOutcome::DryRun => AutopilotItemOutcome::Planned,
        };
        // Charged once, after the fact, with both figures: `charge` takes
        // the larger of them, so an action that reclaimed more than its
        // evidence predicted cannot under-spend the byte budget.
        ledger.charge(expected_reclaim_bytes.unwrap_or(0), actual.unwrap_or(0));

        append_autopilot_audit_record(
            &resource_id,
            action_id,
            policy_label,
            requires_consent,
            model_rank,
            &outcome,
            audit_log_path,
            now,
        );
        report.items.push(item(outcome));
    }

    report
}

/// Appends one best-effort audit line for a real execution, discarding the
/// `Result` exactly as [`crate::emergency`] does and for the same reason: a
/// failure to write the log must never change what the run reports having
/// done. Returns without writing for the outcomes that executed nothing.
#[allow(clippy::too_many_arguments)]
fn append_autopilot_audit_record(
    resource_id: &str,
    action_id: &str,
    policy_label: &str,
    requires_consent: bool,
    model_rank: Option<u32>,
    outcome: &AutopilotItemOutcome,
    audit_log_path: &Path,
    now: SystemTime,
) {
    let Some((outcome_tag, abort_reason)) = outcome.audit_tags() else {
        return;
    };
    let record = AuditRecord {
        timestamp: now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        action_id: action_id.to_string(),
        resource_id: resource_id.to_string(),
        policy_label: policy_label.to_string(),
        outcome: outcome_tag.to_string(),
        abort_reason,
        actual_reclaimed_bytes: match outcome {
            AutopilotItemOutcome::Succeeded { reclaimed_bytes } => *reclaimed_bytes,
            _ => None,
        },
        // Two values, not one: after the fact, "Autopilot deleted this
        // because policy called it AUTO_SAFE" and "Autopilot deleted this
        // because the user pre-authorized this ASK reason" are different
        // facts about authority, and a single `autopilot` tag would make
        // them indistinguishable.
        source: if requires_consent {
            ActionSource::AutopilotPreauthorizedAsk.to_string()
        } else {
            ActionSource::AutopilotAutoSafe.to_string()
        },
        model_rank,
    };
    let _ = append_audit_record(audit_log_path, &record);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::actions::ActionId;
    use crate::detectors::{probe_mtime, DetectorId};
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        GitState, NativeCleanup, ProcessRef, Recoverability, Regenerability, ResourceId,
        ResourceLocator,
    };
    use crate::evidence::probe::ProbeOutcome;
    use crate::monitor::persistence::read_audit_tail;
    use crate::policy::ReasonCode;

    use super::*;

    /// A clean machine: nothing holding the resource open, not in a git
    /// worktree, owning tool not running. Same shape as
    /// [`crate::executor`]'s own `FakeCollector`.
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

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-autopilot-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Creates a real, disposable `node_modules` fixture under a fresh
    /// temp directory and returns discovery-shaped evidence for it.
    ///
    /// Everything these tests delete is created here, under
    /// `std::env::temp_dir()`, by this process. No test in this module
    /// touches a real project, a real cache, or anything outside a
    /// directory it made itself.
    fn node_fixture(prefix: &str, bytes: usize) -> (PathBuf, Evidence) {
        let root = make_temp_dir(prefix);
        let node_modules = root.join("node_modules");
        fs::create_dir_all(node_modules.join("pkg")).unwrap();
        fs::write(node_modules.join("pkg/index.js"), vec![0u8; bytes]).unwrap();
        let evidence = evidence_for(
            &node_modules,
            ResourceKind::NodeModules,
            Regenerability::RegenerableByRebuild,
            bytes as u64,
        );
        (root, evidence)
    }

    fn evidence_for(
        path: &Path,
        kind: ResourceKind,
        regenerability: Regenerability,
        reclaimable_bytes: u64,
    ) -> Evidence {
        let resource = ResourceId::new(kind, ResourceLocator::Path(path.to_path_buf()));
        crate::detectors::discovery_evidence(
            resource,
            DetectorId("autopilot_test_fixture"),
            path,
            ProbeOutcome::Observed(reclaimable_bytes),
            ProbeOutcome::Observed(reclaimable_bytes),
            false,
            probe_mtime(path),
            regenerability,
            Recoverability::RegenerableByRebuild,
            NativeCleanup::Unsupported,
        )
    }

    fn enabled_envelope(kinds: &[ResourceKind]) -> AutopilotEnvelope {
        let mut envelope = AutopilotEnvelope::revoked();
        for kind in kinds {
            envelope.allow_kind(*kind).unwrap();
        }
        envelope.enable();
        envelope
    }

    struct Fixture {
        actions: ActionRegistry,
        policy: PolicyConfig,
        audit_log: PathBuf,
        _audit_root: PathBuf,
    }

    fn fixture() -> Fixture {
        let audit_root = make_temp_dir("audit");
        Fixture {
            actions: ActionRegistry::builtin(),
            policy: PolicyConfig::default(),
            audit_log: audit_root.join("actions.jsonl"),
            _audit_root: audit_root,
        }
    }

    fn run(
        fixture: &Fixture,
        envelope: &AutopilotEnvelope,
        candidates: Vec<Evidence>,
        model_order: &[ValidatedPlanItem],
        dry_run: bool,
    ) -> AutopilotReport {
        run_autopilot(AutopilotRunRequest {
            envelope,
            candidates,
            model_order,
            collector: &CleanCollector,
            actions: &fixture.actions,
            policy: &fixture.policy,
            observed_pressure: Some(PressureState::Pressured),
            now: SystemTime::now(),
            started_at: Instant::now(),
            audit_log_path: &fixture.audit_log,
            dry_run,
        })
    }

    fn plan_item(evidence: &Evidence, action_id: &'static str) -> ValidatedPlanItem {
        ValidatedPlanItem {
            resource: evidence.resource.clone(),
            action_id: ActionId(action_id),
            priority: None,
            model_reason: None,
        }
    }

    fn ids(ordered: &[(Option<u32>, Evidence)]) -> Vec<String> {
        ordered
            .iter()
            .map(|(_, evidence)| evidence.resource.to_string())
            .collect()
    }

    // --- order_candidates: pure, and provably unable to widen the set ---

    #[test]
    fn an_empty_model_plan_leaves_the_candidate_order_untouched() {
        let (root_a, a) = node_fixture("order-a", 16);
        let (root_b, b) = node_fixture("order-b", 16);
        let expected = vec![a.resource.to_string(), b.resource.to_string()];

        let ordered = order_candidates(vec![a, b], &[]);

        assert_eq!(ids(&ordered), expected);
        assert!(ordered.iter().all(|(rank, _)| rank.is_none()));

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    #[test]
    fn a_model_plan_moves_the_named_candidate_to_the_front_and_records_its_rank() {
        let (root_a, a) = node_fixture("order-front-a", 16);
        let (root_b, b) = node_fixture("order-front-b", 16);
        let plan = vec![plan_item(&b, "node.clean.node_modules")];
        let expected = vec![b.resource.to_string(), a.resource.to_string()];

        let ordered = order_candidates(vec![a, b], &plan);

        assert_eq!(ids(&ordered), expected);
        assert_eq!(ordered[0].0, Some(1), "the model's first choice is rank 1");
        assert_eq!(ordered[1].0, None);

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    #[test]
    fn a_model_plan_naming_a_resource_that_was_never_discovered_adds_nothing() {
        let (root_a, a) = node_fixture("order-phantom-a", 16);
        let (root_ghost, ghost) = node_fixture("order-phantom-ghost", 16);
        let plan = vec![plan_item(&ghost, "node.clean.node_modules")];
        let expected = vec![a.resource.to_string()];

        // `ghost` is named by the plan but is NOT among the candidates.
        let ordered = order_candidates(vec![a], &plan);

        assert_eq!(ids(&ordered), expected);
        assert_eq!(ordered[0].0, None);

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_ghost).ok();
    }

    #[test]
    fn a_model_plan_naming_the_same_resource_twice_yields_one_candidate_at_its_first_rank() {
        let (root_a, a) = node_fixture("order-dup-a", 16);
        let (root_b, b) = node_fixture("order-dup-b", 16);
        let plan = vec![
            plan_item(&b, "node.clean.node_modules"),
            plan_item(&b, "node.clean.node_modules"),
        ];
        let expected = vec![b.resource.to_string(), a.resource.to_string()];

        let ordered = order_candidates(vec![a, b], &plan);

        assert_eq!(
            ordered.len(),
            2,
            "a duplicate must not duplicate a candidate"
        );
        assert_eq!(ids(&ordered), expected);
        assert_eq!(ordered[0].0, Some(1), "the FIRST mention sets the rank");

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    /// AC 9. The adversarial case for ordering specifically: a plan that is
    /// mostly resources this process never discovered, plus one it did.
    #[test]
    fn ordering_is_a_permutation_no_matter_what_the_plan_says() {
        let (root_a, a) = node_fixture("perm-a", 16);
        let (root_b, b) = node_fixture("perm-b", 16);
        let (root_ghost1, ghost1) = node_fixture("perm-ghost1", 16);
        let (root_ghost2, ghost2) = node_fixture("perm-ghost2", 16);
        let plan = vec![
            plan_item(&ghost1, "node.clean.node_modules"),
            plan_item(&ghost2, "node.clean.node_modules"),
            plan_item(&b, "node.clean.node_modules"),
            plan_item(&ghost1, "node.clean.node_modules"),
        ];
        let mut before = vec![a.resource.to_string(), b.resource.to_string()];
        before.sort();

        let ordered = order_candidates(vec![a, b], &plan);

        let mut after = ids(&ordered);
        after.sort();
        assert_eq!(after, before, "the candidate set must be unchanged");
        assert_eq!(ordered[0].0, Some(3), "b keeps its own place in the plan");

        for root in [&root_a, &root_b, &root_ghost1, &root_ghost2] {
            fs::remove_dir_all(root).ok();
        }
    }

    // --- run-level refusals ---

    #[test]
    fn a_revoked_envelope_runs_nothing_and_says_why() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("revoked", 4096);
        let node_modules = root.join("node_modules");
        let mut envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        envelope.revoke();

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert_eq!(report.stopped_early, Some(RefusalReason::AutopilotRevoked));
        assert!(report.items.is_empty(), "a revoked run considers nothing");
        assert_eq!(report.actions_attempted, 0);
        assert!(node_modules.exists(), "nothing may be deleted");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_run_below_the_pressure_floor_stops_before_touching_a_candidate() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("pressure", 4096);
        let node_modules = root.join("node_modules");
        let mut envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        envelope.set_min_pressure(Some(PressureState::Critical));

        let report = run_autopilot(AutopilotRunRequest {
            envelope: &envelope,
            candidates: vec![evidence],
            model_order: &[],
            collector: &CleanCollector,
            actions: &fixture.actions,
            policy: &fixture.policy,
            observed_pressure: Some(PressureState::Warn),
            now: SystemTime::now(),
            started_at: Instant::now(),
            audit_log_path: &fixture.audit_log,
            dry_run: false,
        });

        assert_eq!(
            report.stopped_early,
            Some(RefusalReason::DiskPressureTooLow {
                required: PressureState::Critical,
                observed: Some(PressureState::Warn),
            })
        );
        assert!(report.items.is_empty());
        assert!(node_modules.exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_kind_outside_the_allowlist_is_refused_and_left_on_disk() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("kind", 4096);
        let node_modules = root.join("node_modules");
        // Allowlisted for cargo only — the candidate is a node_modules.
        let envelope = enabled_envelope(&[ResourceKind::CargoTargetDir]);

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::Refused(RefusalReason::KindNotAllowed(ResourceKind::NodeModules))
        );
        assert_eq!(report.actions_attempted, 0);
        assert!(node_modules.exists());
        assert_eq!(report.stopped_early, None, "a refusal is not a stop");

        fs::remove_dir_all(&root).ok();
    }

    /// AC 10's PROTECTED refusal, on a real disposable fixture: a
    /// `node_modules` that happens to live inside a `.git` directory, which
    /// `policy::classify` reads as git internals.
    #[test]
    fn a_protected_candidate_is_refused_and_left_on_disk() {
        let fixture = fixture();
        let root = make_temp_dir("protected");
        let node_modules = root.join(".git").join("node_modules");
        fs::create_dir_all(node_modules.join("pkg")).unwrap();
        fs::write(node_modules.join("pkg/index.js"), vec![0u8; 4096]).unwrap();
        let evidence = evidence_for(
            &node_modules,
            ResourceKind::NodeModules,
            Regenerability::RegenerableByRebuild,
            4096,
        );
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert_eq!(report.items[0].policy_label, "PROTECTED");
        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::Refused(RefusalReason::ProtectedRefused)
        );
        assert_eq!(report.safety_refusals(), 1);
        assert_eq!(report.actions_attempted, 0);
        assert!(node_modules.exists(), "PROTECTED means untouched");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_ask_candidate_without_preauthorization_is_refused_and_left_on_disk() {
        let fixture = fixture();
        let root = make_temp_dir("ask-refused");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("index.js"), vec![0u8; 4096]).unwrap();
        // `NotRegenerable` is the per-instance judgment that makes
        // `classify` return Ask/RebuildCostHigh.
        let evidence = evidence_for(
            &node_modules,
            ResourceKind::NodeModules,
            Regenerability::NotRegenerable,
            4096,
        );
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert_eq!(report.items[0].policy_label, "ASK");
        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::Refused(RefusalReason::AskNotPreauthorized)
        );
        assert!(node_modules.exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_kind_with_no_registered_action_is_reported_as_a_wiring_gap_not_a_denial() {
        let fixture = fixture();
        let root = make_temp_dir("no-action");
        let derived = root.join("DerivedData");
        fs::create_dir_all(&derived).unwrap();
        fs::write(derived.join("Build.noindex"), vec![0u8; 4096]).unwrap();
        let evidence = evidence_for(
            &derived,
            ResourceKind::XcodeDerivedData,
            Regenerability::RegenerableByRebuild,
            4096,
        );
        let envelope = enabled_envelope(&[ResourceKind::XcodeDerivedData]);

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert_eq!(report.items[0].policy_label, "AUTO_SAFE");
        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::NoRegisteredAction
        );
        assert_eq!(report.safety_refusals(), 0, "not a policy denial");
        assert!(derived.exists());

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1360 AC6 / HORO-1359 AC5: a registered-but-unrunnable action is
    /// reported as ineligible, spends no attempt, and is a different outcome
    /// from an execution that failed.
    ///
    /// `homebrew.cleanup.cache` is registered for `HomebrewCache` and its
    /// step carries no scoped path, so `executor::structural_refusal`
    /// rejects it unconditionally. Before this fix an allowlisted Homebrew
    /// cache charged an attempt and came back `Failed`.
    ///
    /// The fixture is a directory this test creates under `temp_dir()` and
    /// labels `HomebrewCache`. Nothing here reads or touches a real Homebrew
    /// cache, and nothing here could: the whole point is that the action is
    /// refused before it runs.
    #[test]
    fn an_unrunnable_action_is_ineligible_rather_than_a_failed_attempt() {
        let fixture = fixture();
        let brew_root = make_temp_dir("brew-cache");
        let cache = brew_root.join("Cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("some.bottle.tar.gz"), vec![0u8; 8192]).unwrap();
        let brew = evidence_for(
            &cache,
            ResourceKind::HomebrewCache,
            Regenerability::RegenerableByTool,
            8192,
        );
        // Positive control, same run: a candidate whose action IS runnable.
        let (node_root, node) = node_fixture("ineligible-control", 4096);
        let node_modules = node_root.join("node_modules");
        let envelope = enabled_envelope(&[ResourceKind::HomebrewCache, ResourceKind::NodeModules]);

        // A real run, not a dry one: `Planned` would hide the very thing
        // under test, which is what happens on the way to execution.
        let report = run(&fixture, &envelope, vec![brew, node], &[], false);

        // The allowlist admitted it — this is not a policy refusal dressed
        // up as something else.
        assert_eq!(report.items[0].kind, ResourceKind::HomebrewCache);
        assert_eq!(report.items[0].policy_label, "AUTO_SAFE");
        assert_eq!(report.safety_refusals(), 0, "not a policy denial");

        let AutopilotItemOutcome::Ineligible { reason } = &report.items[0].outcome else {
            panic!(
                "expected Ineligible, got {:?} — a `Failed` here is the bug \
                 this test exists for",
                report.items[0].outcome
            );
        };
        assert!(
            reason.contains("scoped_path"),
            "the reason must be the executor's own, got {reason:?}"
        );
        assert!(cache.exists(), "nothing ran, so nothing was deleted");

        // No attempt spent on it. The control succeeded in the same run, so
        // `1` here is the control's attempt and not the brew candidate's:
        // without the fix this would be 2.
        assert_eq!(report.actions_attempted, 1);
        assert_eq!(report.actions_succeeded, 1);
        assert_eq!(
            report.items[1].outcome,
            AutopilotItemOutcome::Succeeded {
                reclaimed_bytes: Some(4096)
            },
            "control invalid: the runnable candidate did not run either, so \
             the outcome above proves nothing about unrunnability"
        );
        assert!(!node_modules.exists());

        // And the two read differently to a human, not just to a matcher.
        let rendered = report.to_string();
        assert!(
            rendered.contains("skipped, no attempt spent:"),
            "the report must say it skipped, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("failed:"),
            "nothing failed in this run, got:\n{rendered}"
        );

        fs::remove_dir_all(&brew_root).ok();
        fs::remove_dir_all(&node_root).ok();
    }

    // --- budgets ---

    #[test]
    fn the_action_budget_stops_the_run_and_leaves_later_candidates_on_disk() {
        let fixture = fixture();
        let (root_a, a) = node_fixture("budget-a", 4096);
        let (root_b, b) = node_fixture("budget-b", 4096);
        let first = root_a.join("node_modules");
        let second = root_b.join("node_modules");
        let mut envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        envelope.set_max_actions(1).unwrap();

        let report = run(&fixture, &envelope, vec![a, b], &[], false);

        assert_eq!(report.actions_attempted, 1);
        assert_eq!(report.actions_succeeded, 1);
        assert!(!first.exists(), "the one admitted action ran");
        assert!(second.exists(), "the budget stopped the second");
        assert_eq!(
            report.stopped_early,
            Some(RefusalReason::ActionBudgetExhausted { max_actions: 1 })
        );

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    /// A byte-budget refusal is per-candidate, not terminal: a smaller
    /// candidate later in the list may still fit inside what is left.
    #[test]
    fn a_byte_budget_refusal_does_not_stop_the_run() {
        let fixture = fixture();
        let (root_big, mut big) = node_fixture("bytes-big", 4096);
        let (root_small, small) = node_fixture("bytes-small", 4096);
        // The evidence claims the first candidate is enormous.
        big.reclaimable_bytes = ProbeOutcome::Observed(8 * 1024 * 1024 * 1024);
        let big_path = root_big.join("node_modules");
        let small_path = root_small.join("node_modules");
        let mut envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        envelope.set_max_bytes(1024 * 1024).unwrap();

        let report = run(&fixture, &envelope, vec![big, small], &[], false);

        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::Refused(RefusalReason::ByteBudgetExhausted {
                would_reclaim: 8 * 1024 * 1024 * 1024,
                remaining: 1024 * 1024,
            })
        );
        assert!(report.items[1].outcome.is_success());
        assert_eq!(report.stopped_early, None, "the run kept going");
        assert!(big_path.exists());
        assert!(!small_path.exists());

        fs::remove_dir_all(&root_big).ok();
        fs::remove_dir_all(&root_small).ok();
    }

    #[test]
    fn a_dry_run_deletes_nothing_and_still_respects_the_action_budget() {
        let fixture = fixture();
        let (root_a, a) = node_fixture("dry-a", 4096);
        let (root_b, b) = node_fixture("dry-b", 4096);
        let first = root_a.join("node_modules");
        let second = root_b.join("node_modules");
        let mut envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        envelope.set_max_actions(1).unwrap();

        let report = run(&fixture, &envelope, vec![a, b], &[], true);

        assert!(report.dry_run);
        assert_eq!(report.items[0].outcome, AutopilotItemOutcome::Planned);
        assert_eq!(
            report.stopped_early,
            Some(RefusalReason::ActionBudgetExhausted { max_actions: 1 }),
            "a dry run shows the REAL bounded plan, not an unbounded wish list"
        );
        assert!(first.exists(), "a dry run deletes nothing");
        assert!(second.exists());
        assert!(
            read_audit_tail(&fixture.audit_log, 8).is_empty(),
            "a dry run executed nothing, so it audits nothing"
        );

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    // --- real execution + audit trail (AC 8) ---

    #[test]
    fn a_real_auto_safe_run_deletes_the_fixture_and_audits_the_autopilot_source() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("real", 4096);
        let node_modules = root.join("node_modules");
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert!(
            report.items[0].outcome.is_success(),
            "expected success, got {:?}",
            report.items[0].outcome
        );
        assert!(!node_modules.exists());
        assert_eq!(report.actions_succeeded, 1);
        assert!(report.total_bytes_freed > 0);

        let audit = read_audit_tail(&fixture.audit_log, 8);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].source, "autopilot_auto_safe");
        assert_eq!(audit[0].policy_label, "AUTO_SAFE");
        assert_eq!(audit[0].outcome, "succeeded");
        assert_eq!(audit[0].action_id, "node.clean.node_modules");
        assert_eq!(
            audit[0].model_rank, None,
            "no model named it, so no rank is recorded"
        );
        assert!(audit[0].actual_reclaimed_bytes.is_some_and(|b| b > 0));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_audit_line_records_the_model_rank_that_suggested_it() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("rank", 4096);
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        let plan = vec![plan_item(&evidence, "node.clean.node_modules")];

        let report = run(&fixture, &envelope, vec![evidence], &plan, false);

        // 1-based on purpose: the report prints this number verbatim and
        // the audit line stores the same one, so the two cannot drift.
        assert_eq!(report.items[0].model_rank, Some(1));
        let audit = read_audit_tail(&fixture.audit_log, 8);
        assert_eq!(audit[0].model_rank, Some(1));

        // Pinned at the rendering boundary too. A dogfood run of an earlier
        // build printed `[AI rank 2]` for an action the log recorded as
        // `model_rank: 1`, because the report added one to a 0-based index
        // and the log did not. One of those two numbers had to go.
        let rendered = report.to_string();
        assert!(
            rendered.contains("[AI rank 1]"),
            "the report must print the number the log stores, got:\n{rendered}"
        );

        fs::remove_dir_all(&root).ok();
    }

    // --- adversarial: the model cannot expand its own authority (AC 9) ---

    /// The single most important test in this module. A model names a
    /// *different registered action* for the resource. Autopilot resolves
    /// the action from the registry by kind and ignores the model's
    /// `action_id` entirely, so the node action runs and nothing Homebrew
    /// owns is touched.
    #[test]
    fn a_model_naming_a_different_registered_action_cannot_change_which_action_runs() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("adv-action", 4096);
        let node_modules = root.join("node_modules");
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        // A real, registered action id — for an entirely different kind.
        let plan = vec![plan_item(&evidence, "homebrew.cleanup.cache")];

        let report = run(&fixture, &envelope, vec![evidence], &plan, false);

        assert!(report.items[0].outcome.is_success());
        assert!(!node_modules.exists());
        let audit = read_audit_tail(&fixture.audit_log, 8);
        assert_eq!(
            audit[0].action_id, "node.clean.node_modules",
            "the registry chose the action, not the model"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_model_ranking_a_protected_candidate_first_still_cannot_delete_it() {
        let fixture = fixture();
        let root = make_temp_dir("adv-protected");
        let node_modules = root.join(".git").join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("index.js"), vec![0u8; 4096]).unwrap();
        let protected = evidence_for(
            &node_modules,
            ResourceKind::NodeModules,
            Regenerability::RegenerableByRebuild,
            4096,
        );
        let (safe_root, safe) = node_fixture("adv-protected-safe", 4096);
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        let plan = vec![plan_item(&protected, "node.clean.node_modules")];

        let report = run(&fixture, &envelope, vec![safe, protected], &plan, false);

        // The model did get its way about ORDER — and that changed nothing
        // about authority.
        assert_eq!(report.items[0].model_rank, Some(1));
        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::Refused(RefusalReason::ProtectedRefused)
        );
        assert!(node_modules.exists());
        assert!(report.items[1].outcome.is_success());

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&safe_root).ok();
    }

    /// The model's rationale is display-only text. Nothing here parses it,
    /// so a rationale shaped like a command changes neither the action nor
    /// the outcome — and never reaches the audit log.
    #[test]
    fn a_model_reason_carrying_shell_syntax_changes_nothing() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("adv-reason", 4096);
        let node_modules = root.join("node_modules");
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        let mut item = plan_item(&evidence, "node.clean.node_modules");
        item.model_reason = Some("; rm -rf /Users/dev/company-repo && echo pwned".to_string());
        let plan = vec![item];

        let report = run(&fixture, &envelope, vec![evidence], &plan, false);

        assert!(report.items[0].outcome.is_success());
        assert!(!node_modules.exists());
        let audit = read_audit_tail(&fixture.audit_log, 8);
        assert_eq!(audit[0].action_id, "node.clean.node_modules");
        let line = fs::read_to_string(&fixture.audit_log).unwrap();
        assert!(
            !line.contains("rm -rf"),
            "the model's words must not reach the audit log"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// A pre-authorized `Ask` reaches `authorize` with real consent and
    /// then honestly aborts inside `execute`'s deletion-time revalidation.
    ///
    /// This is a known upstream limitation, not a bug in this module:
    /// `Ask`/`RebuildCostHigh` comes only from a *per-instance*
    /// `Regenerability::NotRegenerable`, and
    /// `executor::build_fresh_evidence` rebuilds regenerability from the
    /// kind's static default, so the fresh decision lands on `AutoSafe` and
    /// step 5's class comparison trips. The safe direction: nothing is
    /// deleted. Asserted rather than left undiscovered so the day
    /// `build_fresh_evidence` learns to carry a per-instance judgment, this
    /// test fails loudly and tells whoever changed it to revisit the path.
    #[test]
    fn a_preauthorized_ask_still_aborts_at_deletion_time_revalidation() {
        let fixture = fixture();
        let root = make_temp_dir("ask-preauth");
        let node_modules = root.join("node_modules");
        fs::create_dir_all(&node_modules).unwrap();
        fs::write(node_modules.join("index.js"), vec![0u8; 4096]).unwrap();
        let evidence = evidence_for(
            &node_modules,
            ResourceKind::NodeModules,
            Regenerability::NotRegenerable,
            4096,
        );
        let mut envelope = enabled_envelope(&[ResourceKind::NodeModules]);
        envelope
            .preauthorize_ask(ResourceKind::NodeModules, ReasonCode::RebuildCostHigh)
            .unwrap();

        let report = run(&fixture, &envelope, vec![evidence], &[], false);

        assert_eq!(report.items[0].policy_label, "ASK");
        assert_eq!(
            report.items[0].outcome,
            AutopilotItemOutcome::AbortedByRevalidation(AbortReason::PolicyClassDowngraded)
        );
        assert_eq!(report.actions_attempted, 1);
        assert_eq!(report.actions_succeeded, 0);
        assert!(
            node_modules.exists(),
            "an aborted revalidation mutates nothing"
        );

        // The attempt IS audited, under the pre-authorized-ASK authority —
        // distinguishable after the fact from an AUTO_SAFE one.
        let audit = read_audit_tail(&fixture.audit_log, 8);
        assert_eq!(audit[0].source, "autopilot_preauthorized_ask");
        assert_eq!(audit[0].outcome, "aborted_by_revalidation");
        assert_eq!(
            audit[0].abort_reason.as_deref(),
            Some("PolicyClassDowngraded")
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_report_states_the_authority_it_ran_under() {
        let fixture = fixture();
        let (root, evidence) = node_fixture("describe", 4096);
        let envelope = enabled_envelope(&[ResourceKind::NodeModules]);

        let report = run(&fixture, &envelope, vec![evidence], &[], true);

        assert_eq!(report.envelope, envelope.describe());
        let rendered = report.to_string();
        assert!(rendered.contains("authorized to:"));
        assert!(rendered.contains("nothing was deleted"));

        fs::remove_dir_all(&root).ok();
    }
}
