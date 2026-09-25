//! The Autopilot admission gate (HORO-1310): given an
//! [`AutopilotEnvelope`], a [`PolicyDecision`] and what a run has spent so
//! far, decide whether *this* candidate may be acted on *now*.
//!
//! Pure by construction — no I/O, no ambient clock, no filesystem, no
//! model. `elapsed` is passed in rather than read, for the same reason
//! [`crate::policy::classify`] takes `now`: the thing that decides
//! authority must be re-runnable on demand and testable without waiting.
//!
//! This gate is a *narrowing* stage, never a granting one. It sits between
//! policy and [`crate::policy::approval::authorize`] and can only ever turn
//! an already-permitted action into a refusal:
//!
//! ```text
//! classify  ->  admit  ->  authorize  ->  execute (revalidates)
//! (policy)      (here)     (Approval)     (TOCTOU)
//! ```
//!
//! An `Admitted` result is therefore not permission to delete anything. It
//! means "the envelope does not forbid attempting this", and the caller
//! still has to obtain an [`crate::policy::Approval`] the only way anything
//! in this crate can.

use std::time::Duration;

use crate::evidence::ResourceKind;
use crate::monitor::PressureState;
use crate::policy::{PolicyClass, PolicyDecision};
use crate::reporting::{human_bytes, label_for, PolicyLabel};

use super::envelope::AutopilotEnvelope;

/// Why the gate refused a candidate. Carries the numbers involved so a
/// report can say *how far over* a budget the candidate was, rather than
/// only that it was over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// Autopilot is not enabled. Checked first and every time, so a
    /// revocation takes effect for the very next candidate.
    AutopilotRevoked,
    /// The resource's kind is not on the envelope's allowlist.
    KindNotAllowed(ResourceKind),
    /// `PROTECTED`. No envelope can authorize this; see
    /// [`crate::policy::approval::authorize`], which would refuse it too.
    ProtectedRefused,
    /// `UNKNOWN_INCOMPLETE` — `Ask` reached because the evidence is
    /// incomplete, stale, or a probe failed. Never executable under
    /// Autopilot: a pre-authorization is a statement about an understood
    /// risk, and this is the label for not knowing.
    UnknownIncompleteRefused,
    /// `ASK`, and at least one of its reasons is not pre-authorized.
    AskNotPreauthorized,
    /// The run has already performed `max_actions` actions.
    ActionBudgetExhausted { max_actions: u32 },
    /// The run has been going for at least `max_duration`.
    TimeBudgetExhausted {
        max_duration: Duration,
        elapsed: Duration,
    },
    /// Acting would take the run past `max_bytes`.
    ByteBudgetExhausted { would_reclaim: u64, remaining: u64 },
    /// The candidate's reclaimable size is unknown, so the byte budget
    /// cannot be enforced against it. Refused rather than charged as zero —
    /// an unenforceable limit is not a limit. Defensive: `classify` labels a
    /// resource with unmeasured size `UNKNOWN_INCOMPLETE`, which
    /// [`RefusalReason::UnknownIncompleteRefused`] already rejects, so this
    /// arm exists to keep that true if the two ever drift apart.
    ReclaimSizeUnknown,
    /// The envelope requires disk pressure to be at least `required`.
    /// `observed: None` means the reading was unavailable, which is refused
    /// for the same fail-closed reason as
    /// [`RefusalReason::ReclaimSizeUnknown`].
    DiskPressureTooLow {
        required: PressureState,
        observed: Option<PressureState>,
    },
}

impl RefusalReason {
    /// Stable, snake_case tag — safe to write to the audit trail and to
    /// assert on in tests. Deliberately carries no numbers and no paths.
    pub fn as_str(&self) -> &'static str {
        match self {
            RefusalReason::AutopilotRevoked => "autopilot_revoked",
            RefusalReason::KindNotAllowed(_) => "kind_not_allowed",
            RefusalReason::ProtectedRefused => "protected_refused",
            RefusalReason::UnknownIncompleteRefused => "unknown_incomplete_refused",
            RefusalReason::AskNotPreauthorized => "ask_not_preauthorized",
            RefusalReason::ActionBudgetExhausted { .. } => "action_budget_exhausted",
            RefusalReason::TimeBudgetExhausted { .. } => "time_budget_exhausted",
            RefusalReason::ByteBudgetExhausted { .. } => "byte_budget_exhausted",
            RefusalReason::ReclaimSizeUnknown => "reclaim_size_unknown",
            RefusalReason::DiskPressureTooLow { .. } => "disk_pressure_too_low",
        }
    }

    /// Whether this refusal is about the envelope's *safety* rules rather
    /// than its *budgets*. A safety refusal will refuse this candidate
    /// again on the next run; a budget refusal may not.
    pub fn is_safety_refusal(&self) -> bool {
        match self {
            RefusalReason::AutopilotRevoked
            | RefusalReason::KindNotAllowed(_)
            | RefusalReason::ProtectedRefused
            | RefusalReason::UnknownIncompleteRefused
            | RefusalReason::AskNotPreauthorized
            | RefusalReason::ReclaimSizeUnknown
            | RefusalReason::DiskPressureTooLow { .. } => true,
            RefusalReason::ActionBudgetExhausted { .. }
            | RefusalReason::TimeBudgetExhausted { .. }
            | RefusalReason::ByteBudgetExhausted { .. } => false,
        }
    }
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefusalReason::AutopilotRevoked => write!(f, "Autopilot is not enabled"),
            RefusalReason::KindNotAllowed(kind) => {
                write!(f, "{} is not on the Autopilot allowlist", kind.tag())
            }
            RefusalReason::ProtectedRefused => {
                write!(f, "PROTECTED is never executable by Autopilot")
            }
            RefusalReason::UnknownIncompleteRefused => write!(
                f,
                "UNKNOWN_INCOMPLETE is never executable by Autopilot \
                 (the evidence is incomplete, stale, or a probe failed)"
            ),
            RefusalReason::AskNotPreauthorized => {
                write!(f, "ASK, and this risk is not pre-authorized")
            }
            RefusalReason::ActionBudgetExhausted { max_actions } => {
                write!(f, "action budget spent ({max_actions} of {max_actions})")
            }
            RefusalReason::TimeBudgetExhausted {
                max_duration,
                elapsed,
            } => write!(
                f,
                "time budget spent ({}s of {}s)",
                elapsed.as_secs(),
                max_duration.as_secs()
            ),
            RefusalReason::ByteBudgetExhausted {
                would_reclaim,
                remaining,
            } => write!(
                f,
                "would reclaim {} with {} left in the byte budget",
                human_bytes(*would_reclaim),
                human_bytes(*remaining)
            ),
            RefusalReason::ReclaimSizeUnknown => write!(
                f,
                "reclaimable size is unknown, so the byte budget cannot be enforced"
            ),
            RefusalReason::DiskPressureTooLow { required, observed } => match observed {
                Some(observed) => write!(
                    f,
                    "disk pressure is {observed}, and Autopilot runs only at {required} or worse"
                ),
                None => write!(
                    f,
                    "disk pressure is unknown, and Autopilot runs only at {required} or worse"
                ),
            },
        }
    }
}

/// The gate's verdict for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The envelope does not forbid attempting this. `requires_consent` is
    /// `true` for a pre-authorized `ASK`, meaning the caller must present a
    /// [`crate::policy::UserConsent`] pinned to the resource's *observed*
    /// fingerprint to get an `Approval`; `false` for `AUTO_SAFE`, which
    /// needs none.
    Admitted {
        requires_consent: bool,
    },
    Refused(RefusalReason),
}

impl Admission {
    pub fn is_admitted(&self) -> bool {
        matches!(self, Admission::Admitted { .. })
    }

    pub fn refusal(&self) -> Option<RefusalReason> {
        match self {
            Admission::Admitted { .. } => None,
            Admission::Refused(reason) => Some(*reason),
        }
    }
}

/// What one Autopilot run has spent, checked against the envelope's
/// budgets. One ledger per run; never shared across runs, so budgets do not
/// silently accumulate into a larger de-facto allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLedger {
    max_actions: u32,
    max_bytes: u64,
    actions_charged: u32,
    bytes_charged: u64,
}

impl BudgetLedger {
    /// A fresh ledger for one run of `envelope`. Copies the budgets at run
    /// start; the envelope's *enable bit* is re-read per candidate by
    /// [`admit`], so a mid-run revocation still stops the run even though
    /// its budgets were snapshotted here.
    pub fn for_envelope(envelope: &AutopilotEnvelope) -> Self {
        Self {
            max_actions: envelope.max_actions(),
            max_bytes: envelope.max_bytes(),
            actions_charged: 0,
            bytes_charged: 0,
        }
    }

    pub fn actions_charged(&self) -> u32 {
        self.actions_charged
    }

    pub fn actions_remaining(&self) -> u32 {
        self.max_actions.saturating_sub(self.actions_charged)
    }

    pub fn bytes_charged(&self) -> u64 {
        self.bytes_charged
    }

    pub fn bytes_remaining(&self) -> u64 {
        self.max_bytes.saturating_sub(self.bytes_charged)
    }

    /// Charge one attempted action to the run.
    ///
    /// Charges the **larger** of what the candidate's evidence predicted and
    /// what the executor actually reclaimed. Admission can only ever be
    /// checked against the prediction, so a single action can overshoot the
    /// byte budget; charging the larger figure means it cannot overshoot
    /// *repeatedly* by under-predicting. Saturating, so a charge can exhaust
    /// a budget but never wrap it back around to spare capacity.
    ///
    /// Call this for every action *attempted*, including one that failed:
    /// the action budget bounds how much Autopilot does, not how much of it
    /// worked.
    pub fn charge(&mut self, expected_bytes: u64, actual_bytes: u64) {
        self.actions_charged = self.actions_charged.saturating_add(1);
        self.bytes_charged = self
            .bytes_charged
            .saturating_add(expected_bytes.max(actual_bytes));
    }
}

/// Run-level precondition: does the machine's current disk pressure meet
/// the envelope's floor?
///
/// Separate from [`admit`] because it is a property of the run, not of a
/// candidate — checking it once keeps a "not pressured enough" run from
/// reporting the same refusal N times, once per candidate.
pub fn admits_pressure(
    envelope: &AutopilotEnvelope,
    observed: Option<PressureState>,
) -> Result<(), RefusalReason> {
    let Some(required) = envelope.min_pressure() else {
        return Ok(());
    };
    match observed {
        Some(observed) if observed >= required => Ok(()),
        observed => Err(RefusalReason::DiskPressureTooLow { required, observed }),
    }
}

/// Decide whether `decision` may be acted on now, under `envelope`, with
/// `ledger` already spent and `elapsed` time gone in this run.
///
/// `expected_reclaim_bytes` is what the candidate's *evidence* says would be
/// reclaimed — never a model's estimate, and never the action's own claim.
///
/// Safety refusals are evaluated before budget refusals so that a refusal
/// message names the durable reason: a `PROTECTED` candidate seen with an
/// empty action budget reports `protected_refused`, because "come back next
/// run" would be the wrong thing to tell the user about it.
pub fn admit(
    envelope: &AutopilotEnvelope,
    ledger: &BudgetLedger,
    decision: &PolicyDecision,
    expected_reclaim_bytes: Option<u64>,
    elapsed: Duration,
) -> Admission {
    if !envelope.is_enabled() {
        return Admission::Refused(RefusalReason::AutopilotRevoked);
    }

    let kind = decision.resource.kind;
    if !envelope.permits_kind(kind) {
        return Admission::Refused(RefusalReason::KindNotAllowed(kind));
    }

    // Read the *reporting* label rather than only `decision.class`:
    // `UNKNOWN_INCOMPLETE` is not a `PolicyClass` variant, it is `Ask`
    // carrying an evidence-quality reason, and it must not be reachable
    // through the pre-authorization path below.
    let requires_consent = match label_for(decision) {
        PolicyLabel::Protected => return Admission::Refused(RefusalReason::ProtectedRefused),
        PolicyLabel::UnknownIncomplete => {
            return Admission::Refused(RefusalReason::UnknownIncompleteRefused)
        }
        PolicyLabel::Ask => {
            if !envelope.ask_preauthorized(kind, &decision.reasons) {
                return Admission::Refused(RefusalReason::AskNotPreauthorized);
            }
            true
        }
        PolicyLabel::AutoSafe => false,
        // Unreachable by construction, and refused rather than omitted for
        // the same reason `ReclaimSizeUnknown` exists above: `label_for`
        // projects a decision and so never yields this (asserted by
        // `reporting::policy_label`'s own
        // `label_for_never_returns_not_policy_governed`), but if that ever
        // drifts, a label whose entire meaning is "policy did not judge
        // this" must not reach the pre-authorization path. Autopilot acts on
        // developer resources; the one action that carries this label
        // (HORO-1468, emergency's own state file) is not one of them and
        // never passes through this gate.
        PolicyLabel::NotPolicyGoverned => {
            return Admission::Refused(RefusalReason::UnknownIncompleteRefused)
        }
    };

    // Defensive: `label_for` derives `AutoSafe`/`Ask` from `decision.class`,
    // so this cannot currently disagree. If a future label ever stops being
    // a pure function of the class, fail closed rather than acting on a
    // class this gate never inspected.
    if requires_consent && decision.class != PolicyClass::Ask {
        return Admission::Refused(RefusalReason::AskNotPreauthorized);
    }
    if !requires_consent && decision.class != PolicyClass::AutoSafe {
        return Admission::Refused(RefusalReason::ProtectedRefused);
    }

    let Some(would_reclaim) = expected_reclaim_bytes else {
        return Admission::Refused(RefusalReason::ReclaimSizeUnknown);
    };

    if ledger.actions_remaining() == 0 {
        return Admission::Refused(RefusalReason::ActionBudgetExhausted {
            max_actions: ledger.max_actions,
        });
    }

    // Read the wall-clock budget from the envelope rather than from the
    // ledger's start-of-run snapshot, so shortening it mid-run bites
    // immediately, the same way revoking does.
    let max_duration = envelope.max_duration();
    if elapsed >= max_duration {
        return Admission::Refused(RefusalReason::TimeBudgetExhausted {
            max_duration,
            elapsed,
        });
    }

    let remaining = ledger.bytes_remaining();
    if would_reclaim > remaining {
        return Admission::Refused(RefusalReason::ByteBudgetExhausted {
            would_reclaim,
            remaining,
        });
    }

    Admission::Admitted { requires_consent }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use crate::evidence::{ResourceId, ResourceLocator};
    use crate::policy::ReasonCode;

    use super::*;

    fn decision(kind: ResourceKind, class: PolicyClass, reasons: &[ReasonCode]) -> PolicyDecision {
        let now = SystemTime::UNIX_EPOCH;
        PolicyDecision {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from("/tmp/fixture"))),
            class,
            reasons: reasons.to_vec(),
            evidence_collected_at: now,
            evaluated_at: now,
            policy_version: 1,
        }
    }

    fn auto_safe(kind: ResourceKind) -> PolicyDecision {
        decision(
            kind,
            PolicyClass::AutoSafe,
            &[
                ReasonCode::EvidenceFreshAndComplete,
                ReasonCode::RegenerableByTool,
            ],
        )
    }

    /// An envelope that permits exactly one kind, with room for one action
    /// and one byte more than the fixtures below ask for.
    fn narrow_envelope(kind: ResourceKind) -> AutopilotEnvelope {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        envelope.allow_kind(kind).expect("kind is allowlistable");
        envelope
    }

    #[test]
    fn a_revoked_envelope_admits_nothing() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.revoke();
        let ledger = BudgetLedger::for_envelope(&envelope);

        let admission = admit(
            &envelope,
            &ledger,
            &auto_safe(ResourceKind::CargoTargetDir),
            Some(1024),
            Duration::ZERO,
        );

        assert_eq!(
            admission,
            Admission::Refused(RefusalReason::AutopilotRevoked)
        );
    }

    #[test]
    fn an_auto_safe_candidate_on_the_allowlist_is_admitted_without_consent() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        let ledger = BudgetLedger::for_envelope(&envelope);

        assert_eq!(
            admit(
                &envelope,
                &ledger,
                &auto_safe(ResourceKind::CargoTargetDir),
                Some(1024),
                Duration::ZERO
            ),
            Admission::Admitted {
                requires_consent: false
            }
        );
    }

    #[test]
    fn a_kind_outside_the_allowlist_is_refused_even_when_auto_safe() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        let ledger = BudgetLedger::for_envelope(&envelope);

        assert_eq!(
            admit(
                &envelope,
                &ledger,
                &auto_safe(ResourceKind::HomebrewCache),
                Some(1024),
                Duration::ZERO
            ),
            Admission::Refused(RefusalReason::KindNotAllowed(ResourceKind::HomebrewCache))
        );
    }

    #[test]
    fn protected_is_refused_even_though_its_kind_is_allowlisted() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        let ledger = BudgetLedger::for_envelope(&envelope);
        let protected = decision(
            ResourceKind::CargoTargetDir,
            PolicyClass::Protected,
            &[ReasonCode::ProtectedGitInternals],
        );

        assert_eq!(
            admit(&envelope, &ledger, &protected, Some(1024), Duration::ZERO),
            Admission::Refused(RefusalReason::ProtectedRefused)
        );
    }

    /// The AC that cannot be expressed as a `PolicyClass` check: these are
    /// all `Ask`, and all three evidence-quality reasons must be refused
    /// even though the *risk* reason alongside them is pre-authorized.
    #[test]
    fn unknown_incomplete_is_refused_for_every_evidence_quality_reason() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .expect("rebuild cost is pre-authorizable");
        let ledger = BudgetLedger::for_envelope(&envelope);

        for reason in [
            ReasonCode::EvidenceIncomplete,
            ReasonCode::EvidenceStale,
            ReasonCode::EvidenceProbeFailed,
        ] {
            let unknown = decision(
                ResourceKind::CargoTargetDir,
                PolicyClass::Ask,
                &[reason, ReasonCode::RebuildCostHigh],
            );
            assert_eq!(
                admit(&envelope, &ledger, &unknown, Some(1024), Duration::ZERO),
                Admission::Refused(RefusalReason::UnknownIncompleteRefused),
                "{} must never be executable under Autopilot",
                reason.as_str()
            );
        }
    }

    #[test]
    fn ask_without_a_preauthorization_is_refused() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        let ledger = BudgetLedger::for_envelope(&envelope);
        let ask = decision(
            ResourceKind::CargoTargetDir,
            PolicyClass::Ask,
            &[ReasonCode::RebuildCostHigh],
        );

        assert_eq!(
            admit(&envelope, &ledger, &ask, Some(1024), Duration::ZERO),
            Admission::Refused(RefusalReason::AskNotPreauthorized)
        );
    }

    #[test]
    fn a_preauthorized_ask_is_admitted_but_requires_consent() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .expect("rebuild cost is pre-authorizable");
        let ledger = BudgetLedger::for_envelope(&envelope);
        let ask = decision(
            ResourceKind::CargoTargetDir,
            PolicyClass::Ask,
            &[ReasonCode::RebuildCostHigh],
        );

        assert_eq!(
            admit(&envelope, &ledger, &ask, Some(1024), Duration::ZERO),
            Admission::Admitted {
                requires_consent: true
            }
        );
    }

    /// Mirrors the executor's `PolicyReasonsWidened` abort: a
    /// pre-authorization covers one named risk, so a decision that also
    /// carries an *un*-pre-authorized risk is not covered by it.
    #[test]
    fn a_preauthorization_does_not_cover_a_decision_carrying_an_extra_risk() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .expect("rebuild cost is pre-authorizable");
        let ledger = BudgetLedger::for_envelope(&envelope);
        let widened = decision(
            ResourceKind::CargoTargetDir,
            PolicyClass::Ask,
            &[ReasonCode::RebuildCostHigh, ReasonCode::GitWorktreeDirty],
        );

        assert_eq!(
            admit(&envelope, &ledger, &widened, Some(1024), Duration::ZERO),
            Admission::Refused(RefusalReason::AskNotPreauthorized)
        );
    }

    #[test]
    fn an_unknown_reclaim_size_is_refused_rather_than_charged_as_zero() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        let ledger = BudgetLedger::for_envelope(&envelope);

        assert_eq!(
            admit(
                &envelope,
                &ledger,
                &auto_safe(ResourceKind::CargoTargetDir),
                None,
                Duration::ZERO
            ),
            Admission::Refused(RefusalReason::ReclaimSizeUnknown)
        );
    }

    #[test]
    fn the_action_budget_refuses_further_candidates_once_spent() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.set_max_actions(2).expect("below the ceiling");
        let mut ledger = BudgetLedger::for_envelope(&envelope);
        let candidate = auto_safe(ResourceKind::CargoTargetDir);

        for charged in 0..2 {
            assert_eq!(ledger.actions_charged(), charged);
            assert!(
                admit(&envelope, &ledger, &candidate, Some(1), Duration::ZERO).is_admitted(),
                "action {charged} of 2 must be admitted"
            );
            ledger.charge(1, 1);
        }

        assert_eq!(
            admit(&envelope, &ledger, &candidate, Some(1), Duration::ZERO),
            Admission::Refused(RefusalReason::ActionBudgetExhausted { max_actions: 2 })
        );
        assert_eq!(ledger.actions_remaining(), 0);
    }

    #[test]
    fn a_zero_action_budget_admits_nothing_at_all() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.set_max_actions(0).expect("below the ceiling");
        let ledger = BudgetLedger::for_envelope(&envelope);

        assert_eq!(
            admit(
                &envelope,
                &ledger,
                &auto_safe(ResourceKind::CargoTargetDir),
                Some(1),
                Duration::ZERO
            ),
            Admission::Refused(RefusalReason::ActionBudgetExhausted { max_actions: 0 })
        );
    }

    #[test]
    fn the_byte_budget_refuses_a_candidate_larger_than_what_is_left() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.set_max_bytes(1_000).expect("below the ceiling");
        let mut ledger = BudgetLedger::for_envelope(&envelope);
        ledger.charge(400, 400);
        let candidate = auto_safe(ResourceKind::CargoTargetDir);

        assert_eq!(
            admit(&envelope, &ledger, &candidate, Some(601), Duration::ZERO),
            Admission::Refused(RefusalReason::ByteBudgetExhausted {
                would_reclaim: 601,
                remaining: 600,
            })
        );
        assert!(
            admit(&envelope, &ledger, &candidate, Some(600), Duration::ZERO).is_admitted(),
            "a candidate that exactly fills the remaining budget is within it"
        );
    }

    #[test]
    fn the_time_budget_refuses_further_candidates_once_spent() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope
            .set_max_duration(Duration::from_secs(30))
            .expect("below the ceiling");
        let ledger = BudgetLedger::for_envelope(&envelope);
        let candidate = auto_safe(ResourceKind::CargoTargetDir);

        assert!(
            admit(
                &envelope,
                &ledger,
                &candidate,
                Some(1),
                Duration::from_secs(29)
            )
            .is_admitted(),
            "29s of a 30s budget leaves the run inside it"
        );
        assert_eq!(
            admit(
                &envelope,
                &ledger,
                &candidate,
                Some(1),
                Duration::from_secs(30)
            ),
            Admission::Refused(RefusalReason::TimeBudgetExhausted {
                max_duration: Duration::from_secs(30),
                elapsed: Duration::from_secs(30),
            })
        );
    }

    /// Shortening the wall-clock budget mid-run must bite immediately, so
    /// `admit` reads it from the envelope rather than from the ledger's
    /// start-of-run snapshot.
    #[test]
    fn shortening_the_time_budget_mid_run_takes_effect_immediately() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope
            .set_max_duration(Duration::from_secs(600))
            .expect("below the ceiling");
        let ledger = BudgetLedger::for_envelope(&envelope);
        let candidate = auto_safe(ResourceKind::CargoTargetDir);
        let elapsed = Duration::from_secs(30);

        assert!(admit(&envelope, &ledger, &candidate, Some(1), elapsed).is_admitted());

        envelope
            .set_max_duration(Duration::from_secs(10))
            .expect("below the ceiling");

        assert_eq!(
            admit(&envelope, &ledger, &candidate, Some(1), elapsed).refusal(),
            Some(RefusalReason::TimeBudgetExhausted {
                max_duration: Duration::from_secs(10),
                elapsed,
            })
        );
    }

    #[test]
    fn a_safety_refusal_is_reported_ahead_of_an_exhausted_budget() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.set_max_actions(0).expect("below the ceiling");
        let ledger = BudgetLedger::for_envelope(&envelope);
        let protected = decision(
            ResourceKind::CargoTargetDir,
            PolicyClass::Protected,
            &[ReasonCode::ProtectedInfraState],
        );

        assert_eq!(
            admit(&envelope, &ledger, &protected, Some(1), Duration::ZERO),
            Admission::Refused(RefusalReason::ProtectedRefused),
            "the durable reason is that it is PROTECTED, not that the run is out of actions"
        );
    }

    #[test]
    fn the_ledger_charges_the_larger_of_the_predicted_and_actual_bytes() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.set_max_bytes(10_000).expect("below the ceiling");
        let mut ledger = BudgetLedger::for_envelope(&envelope);

        ledger.charge(100, 4_000);
        assert_eq!(ledger.bytes_charged(), 4_000, "an under-prediction");
        ledger.charge(5_000, 0);
        assert_eq!(
            ledger.bytes_charged(),
            9_000,
            "a failed action still spends what it was predicted to spend"
        );
        assert_eq!(ledger.actions_charged(), 2);
        assert_eq!(ledger.bytes_remaining(), 1_000);
    }

    #[test]
    fn the_ledger_saturates_rather_than_wrapping_back_into_spare_capacity() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        let mut ledger = BudgetLedger::for_envelope(&envelope);

        ledger.charge(u64::MAX, u64::MAX);
        ledger.charge(u64::MAX, u64::MAX);

        assert_eq!(ledger.bytes_charged(), u64::MAX);
        assert_eq!(ledger.bytes_remaining(), 0);
        assert_eq!(ledger.actions_charged(), 2);
    }

    #[test]
    fn no_pressure_floor_means_pressure_is_never_a_reason_to_refuse() {
        let envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        assert_eq!(envelope.min_pressure(), None);

        for observed in PressureState::ALL {
            assert_eq!(admits_pressure(&envelope, Some(observed)), Ok(()));
        }
        assert_eq!(admits_pressure(&envelope, None), Ok(()));
    }

    #[test]
    fn a_pressure_floor_refuses_a_healthier_machine_and_an_unknown_one() {
        let mut envelope = narrow_envelope(ResourceKind::CargoTargetDir);
        envelope.set_min_pressure(Some(PressureState::Pressured));

        for healthier in [PressureState::Healthy, PressureState::Warn] {
            assert_eq!(
                admits_pressure(&envelope, Some(healthier)),
                Err(RefusalReason::DiskPressureTooLow {
                    required: PressureState::Pressured,
                    observed: Some(healthier),
                })
            );
        }
        for at_or_worse in [
            PressureState::Pressured,
            PressureState::Critical,
            PressureState::Emergency,
        ] {
            assert_eq!(admits_pressure(&envelope, Some(at_or_worse)), Ok(()));
        }
        assert_eq!(
            admits_pressure(&envelope, None),
            Err(RefusalReason::DiskPressureTooLow {
                required: PressureState::Pressured,
                observed: None,
            }),
            "an unavailable pressure reading must fail closed"
        );
    }

    #[test]
    fn refusal_tags_are_distinct_non_empty_and_free_of_paths() {
        let reasons = [
            RefusalReason::AutopilotRevoked,
            RefusalReason::KindNotAllowed(ResourceKind::HomebrewCache),
            RefusalReason::ProtectedRefused,
            RefusalReason::UnknownIncompleteRefused,
            RefusalReason::AskNotPreauthorized,
            RefusalReason::ActionBudgetExhausted { max_actions: 3 },
            RefusalReason::TimeBudgetExhausted {
                max_duration: Duration::from_secs(60),
                elapsed: Duration::from_secs(61),
            },
            RefusalReason::ByteBudgetExhausted {
                would_reclaim: 2,
                remaining: 1,
            },
            RefusalReason::ReclaimSizeUnknown,
            RefusalReason::DiskPressureTooLow {
                required: PressureState::Warn,
                observed: None,
            },
        ];

        let mut tags: Vec<&'static str> = reasons.iter().map(|r| r.as_str()).collect();
        assert_eq!(tags.len(), 10, "a new refusal reason needs a tag here");
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), 10, "refusal tags must be distinct");
        for reason in reasons {
            assert!(!reason.as_str().is_empty());
            assert!(
                !reason.as_str().contains('/'),
                "an audit tag must not carry a path"
            );
            assert!(
                !reason.to_string().is_empty(),
                "every refusal explains itself"
            );
        }
    }

    #[test]
    fn every_budget_refusal_is_transient_and_every_other_one_is_not() {
        assert!(!RefusalReason::ActionBudgetExhausted { max_actions: 1 }.is_safety_refusal());
        assert!(!RefusalReason::ByteBudgetExhausted {
            would_reclaim: 2,
            remaining: 1
        }
        .is_safety_refusal());
        assert!(!RefusalReason::TimeBudgetExhausted {
            max_duration: Duration::ZERO,
            elapsed: Duration::ZERO
        }
        .is_safety_refusal());

        for safety in [
            RefusalReason::AutopilotRevoked,
            RefusalReason::KindNotAllowed(ResourceKind::Unknown),
            RefusalReason::ProtectedRefused,
            RefusalReason::UnknownIncompleteRefused,
            RefusalReason::AskNotPreauthorized,
            RefusalReason::ReclaimSizeUnknown,
            RefusalReason::DiskPressureTooLow {
                required: PressureState::Warn,
                observed: None,
            },
        ] {
            assert!(
                safety.is_safety_refusal(),
                "{} is a safety refusal",
                safety.as_str()
            );
        }
    }
}
