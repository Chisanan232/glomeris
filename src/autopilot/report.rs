//! The machine-readable projection of an [`AutopilotEnvelope`] (HORO-1310).
//!
//! # Why this exists separately from `describe`
//!
//! [`AutopilotEnvelope::describe`] answers "what is Autopilot allowed to do"
//! for somebody reading a terminal: fixed-width labels, wrapped lists, one
//! fact per line. A GUI needs the same facts as values it can put in a
//! checkbox and a stepper, and it needs the *choices* too — every kind that
//! could be allowlisted, every reason that could be pre-authorized — which a
//! human reader of `describe` does not need because the help text lists them.
//!
//! So this module is the second rendering of one truth, not a second truth.
//! Every field below is read from the envelope or from the same predicate the
//! gate consults; nothing here decides anything, and nothing here can widen
//! anything. `envelope_report` takes `&AutopilotEnvelope` and returns a
//! `Serialize`-only DTO, so the type system says that too.
//!
//! # Why the refusals travel with the grant
//!
//! The thin client must be able to tell a user what Glomeris will always
//! refuse — and it must not be the thing that decides what that is. Those two
//! requirements together mean the refusals have to arrive from here, as data.
//! A Swift screen listing "PROTECTED is never executable" from its own
//! knowledge would be a policy claim made in the client; the same sentence
//! rendered from `never_executable_labels` is a quotation.

use crate::autopilot::envelope::{
    is_preauthorizable, ACTIONS_CEILING, BYTES_CEILING, DURATION_CEILING,
};
use crate::autopilot::AutopilotEnvelope;
use crate::evidence::ResourceKind;
use crate::monitor::pressure::PressureState;
use crate::policy::ReasonCode;
use crate::reporting::dto::{
    AutopilotAskPreauthorizationReport, AutopilotCeilingsReport, AutopilotEnvelopeReport,
};
use crate::reporting::{human_bytes, PolicyLabel};

/// What the model may and may not do, one sentence per fact.
///
/// In Rust because each line is a claim about Rust's own behaviour, checkable
/// against the code in [`crate::autopilot::run`] and
/// [`crate::actions::llm`] rather than against a designer's intent. The
/// canonical invariant is the first line for the same reason it opens the
/// module docs: everything else here is a consequence of it.
const AI_AUTHORITY: &[&str] = &[
    "AI can recommend. Policy decides. Executor verifies. Filesystem reality wins.",
    "The model's only influence is the order candidates are considered in. \
     Reordering a list cannot add anything to it.",
    "Every candidate was found by this Mac's own detectors and classified \
     before the model saw anything about it.",
    "The model cannot name a path, a command, a resource this machine did not \
     report, an action that is not registered, or a policy class.",
    "Every action the model ranks first still has to pass the envelope, the \
     normal authorization check, and a re-check of the filesystem at the \
     moment of deletion.",
];

/// The policy labels no envelope can make executable.
///
/// Written as an explicit list of two rather than "every label except
/// AUTO_SAFE and ASK", because the interesting property is which labels are
/// unconditionally refused, and an expression that derives it by subtraction
/// would silently start including a newly added label.
const NEVER_EXECUTABLE_LABELS: &[PolicyLabel] =
    &[PolicyLabel::Protected, PolicyLabel::UnknownIncomplete];

/// Projects `envelope` into the DTO `autopilot show|enable|revoke --json`
/// print.
///
/// `stored_at` is passed in rather than resolved here so that this function
/// is pure and testable without a HOME: the caller already had to resolve the
/// path to read the envelope.
pub fn envelope_report(
    envelope: &AutopilotEnvelope,
    stored_at: Option<String>,
) -> AutopilotEnvelopeReport {
    AutopilotEnvelopeReport {
        enabled: envelope.is_enabled(),
        allowed_kinds: envelope.allowed_kinds().iter().map(|k| k.tag()).collect(),
        ask_preauthorizations: envelope
            .ask_preauthorizations()
            .iter()
            .map(|entry| AutopilotAskPreauthorizationReport {
                kind: entry.kind().tag(),
                reason: entry.reason().as_str(),
            })
            .collect(),
        max_actions: envelope.max_actions(),
        max_bytes: envelope.max_bytes(),
        max_bytes_human: human_bytes(envelope.max_bytes()),
        max_duration_secs: envelope.max_duration().as_secs(),
        min_pressure: envelope.min_pressure().map(|state| state.as_str()),
        ceilings: AutopilotCeilingsReport {
            max_actions: ACTIONS_CEILING,
            max_bytes: BYTES_CEILING,
            max_bytes_human: human_bytes(BYTES_CEILING),
            max_duration_secs: DURATION_CEILING.as_secs(),
        },
        // Partitioned by asking the envelope itself, one kind at a time,
        // rather than by naming the excluded kind here. A second place that
        // knew which kinds are unallowlistable is a second place that could
        // be wrong about it.
        allowlistable_kinds: ResourceKind::ALL
            .iter()
            .filter(|kind| is_allowlistable(**kind))
            .map(|kind| kind.tag())
            .collect(),
        never_allowlistable_kinds: ResourceKind::ALL
            .iter()
            .filter(|kind| !is_allowlistable(**kind))
            .map(|kind| kind.tag())
            .collect(),
        preauthorizable_reasons: ReasonCode::ALL
            .iter()
            .filter(|reason| is_preauthorizable(**reason))
            .map(|reason| reason.as_str())
            .collect(),
        never_preauthorizable_reasons: ReasonCode::ALL
            .iter()
            .filter(|reason| is_refusal_reason(**reason) && !is_preauthorizable(**reason))
            .map(|reason| reason.as_str())
            .collect(),
        pressure_states: PressureState::ALL.iter().map(|s| s.as_str()).collect(),
        never_executable_labels: NEVER_EXECUTABLE_LABELS
            .iter()
            .map(|label| label.as_str())
            .collect(),
        ai_authority: AI_AUTHORITY.to_vec(),
        stored_at,
    }
}

/// Whether `kind` may enter an allowlist, asked of the envelope's own
/// checked setter on a throwaway envelope.
///
/// Deliberately not `kind != ResourceKind::Unknown`. That is today's answer,
/// and it is [`AutopilotEnvelope::allow_kind`]'s to give: if a second kind
/// ever becomes unallowlistable, this function keeps telling the truth
/// without being edited, and a report that disagreed with the setter would be
/// worse than no report.
fn is_allowlistable(kind: ResourceKind) -> bool {
    AutopilotEnvelope::revoked().allow_kind(kind).is_ok()
}

/// Whether `reason` is one a decision can be held back *by*, as opposed to
/// one that justifies letting it through.
///
/// `never_preauthorizable_reasons` exists to answer "what could I not consent
/// to in advance, even if I wanted to". The three `AUTO_SAFE` justifications
/// are true answers to that question and useless ones: they are why something
/// needed no consent in the first place, so listing them alongside
/// `protected_credential_material` would present a reader with warnings about
/// nothing and bury the ones that matter.
///
/// Written as an exhaustive `match` rather than a list, so a nineteenth reason
/// code cannot be added without someone deciding which side it falls on — the
/// build stops here until they do.
fn is_refusal_reason(reason: ReasonCode) -> bool {
    match reason {
        ReasonCode::EvidenceFreshAndComplete
        | ReasonCode::RegenerableByTool
        | ReasonCode::NoActiveUseObserved => false,

        ReasonCode::ProtectedCredentialMaterial
        | ReasonCode::ProtectedGitInternals
        | ReasonCode::ProtectedInfraState
        | ReasonCode::ProtectedPersistentVolume
        | ReasonCode::ProtectedUserDocuments
        | ReasonCode::ProtectedSystemPath
        | ReasonCode::ProtectedUnsafeMountOrSymlink
        | ReasonCode::ProtectedUnknownResourceKind
        | ReasonCode::EvidenceIncomplete
        | ReasonCode::EvidenceStale
        | ReasonCode::EvidenceProbeFailed
        | ReasonCode::ResourceInActiveUse
        | ReasonCode::GitWorktreeDirty
        | ReasonCode::RebuildCostHigh
        | ReasonCode::OwningToolLive => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn revoked_envelope_reports_a_grant_that_does_nothing() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        assert!(!report.enabled);
        assert!(report.allowed_kinds.is_empty());
        assert!(report.ask_preauthorizations.is_empty());
        assert_eq!(report.min_pressure, None);
        assert_eq!(report.stored_at, None);
    }

    #[test]
    fn a_grant_is_reported_field_for_field() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::NodeModules).unwrap();
        envelope
            .preauthorize_ask(ResourceKind::NodeModules, ReasonCode::RebuildCostHigh)
            .unwrap();
        envelope.set_max_actions(2).unwrap();
        envelope.set_max_bytes(1024 * 1024 * 1024).unwrap();
        envelope.set_max_duration(Duration::from_secs(45)).unwrap();
        envelope.set_min_pressure(Some(PressureState::Pressured));
        envelope.enable();

        let report = envelope_report(&envelope, Some("/tmp/envelope.conf".to_string()));

        assert!(report.enabled);
        assert_eq!(report.allowed_kinds, vec!["node_modules"]);
        assert_eq!(report.ask_preauthorizations.len(), 1);
        assert_eq!(report.ask_preauthorizations[0].kind, "node_modules");
        assert_eq!(report.ask_preauthorizations[0].reason, "rebuild_cost_high");
        assert_eq!(report.max_actions, 2);
        assert_eq!(report.max_bytes, 1024 * 1024 * 1024);
        assert_eq!(report.max_bytes_human, human_bytes(1024 * 1024 * 1024));
        assert_eq!(report.max_duration_secs, 45);
        assert_eq!(report.min_pressure, Some("PRESSURED"));
        assert_eq!(report.stored_at.as_deref(), Some("/tmp/envelope.conf"));
    }

    /// The ceilings are the part a client must not learn independently: a
    /// stepper whose maximum disagreed with the CLI's would offer a value
    /// `enable` then refuses.
    #[test]
    fn ceilings_match_the_constants_the_setters_enforce() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        assert_eq!(report.ceilings.max_actions, ACTIONS_CEILING);
        assert_eq!(report.ceilings.max_bytes, BYTES_CEILING);
        assert_eq!(
            report.ceilings.max_duration_secs,
            DURATION_CEILING.as_secs()
        );

        let mut envelope = AutopilotEnvelope::revoked();
        assert!(envelope
            .set_max_actions(report.ceilings.max_actions)
            .is_ok());
        assert!(envelope
            .set_max_actions(report.ceilings.max_actions + 1)
            .is_err());
    }

    /// Every kind is in exactly one of the two lists, and the reported
    /// partition is the one the setter actually enforces — checked by
    /// running the setter, not by re-deriving the rule.
    #[test]
    fn kind_lists_partition_every_kind_the_way_the_setter_does() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        let mut union = report.allowlistable_kinds.clone();
        union.extend(report.never_allowlistable_kinds.iter().copied());
        union.sort_unstable();
        let mut all: Vec<&str> = ResourceKind::ALL.iter().map(|k| k.tag()).collect();
        all.sort_unstable();
        assert_eq!(union, all, "every kind must appear in exactly one list");

        for kind in ResourceKind::ALL {
            let mut envelope = AutopilotEnvelope::revoked();
            let accepted = envelope.allow_kind(*kind).is_ok();
            assert_eq!(
                accepted,
                report.allowlistable_kinds.contains(&kind.tag()),
                "{} is reported on the wrong side of the allowlistable split",
                kind.tag()
            );
        }
    }

    /// Same property for reasons, and the one that matters most: a client
    /// offering a pre-authorization the envelope refuses would be offering
    /// consent to something that cannot be consented to.
    ///
    /// The two lists together cover every reason a decision can be held back
    /// by, not every reason code — the `AUTO_SAFE` justifications are
    /// deliberately in neither, see [`is_refusal_reason`].
    #[test]
    fn reason_lists_partition_every_refusal_reason_the_way_the_predicate_does() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        let mut union = report.preauthorizable_reasons.clone();
        union.extend(report.never_preauthorizable_reasons.iter().copied());
        union.sort_unstable();
        let mut refusals: Vec<&str> = ReasonCode::ALL
            .iter()
            .filter(|r| is_refusal_reason(**r))
            .map(|r| r.as_str())
            .collect();
        refusals.sort_unstable();
        assert_eq!(
            union, refusals,
            "every refusal reason must appear in exactly one list"
        );

        for reason in ReasonCode::ALL.iter().filter(|r| is_refusal_reason(**r)) {
            assert_eq!(
                is_preauthorizable(*reason),
                report.preauthorizable_reasons.contains(&reason.as_str()),
                "{} is reported on the wrong side of the pre-authorizable split",
                reason.as_str()
            );
        }
    }

    /// Every pre-authorizable reason must be a refusal reason, or the report
    /// would be offering consent to something that was never blocking
    /// anything. This is the direction the narrowing above could get wrong.
    #[test]
    fn nothing_preauthorizable_is_an_auto_safe_justification() {
        for reason in ReasonCode::ALL {
            if is_preauthorizable(*reason) {
                assert!(
                    is_refusal_reason(*reason),
                    "{} is pre-authorizable but not a refusal reason",
                    reason.as_str()
                );
            }
        }
    }

    /// The omitted reasons are exactly the three `AUTO_SAFE` justifications,
    /// named here so that silently dropping a fourth — a real refusal reason
    /// a user would never learn they cannot consent to — fails.
    #[test]
    fn only_the_auto_safe_justifications_are_omitted() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        let omitted: Vec<&str> = ReasonCode::ALL
            .iter()
            .map(|r| r.as_str())
            .filter(|tag| {
                !report.preauthorizable_reasons.contains(tag)
                    && !report.never_preauthorizable_reasons.contains(tag)
            })
            .collect();

        assert_eq!(
            omitted,
            vec![
                "evidence_fresh_and_complete",
                "regenerable_by_tool",
                "no_active_use_observed"
            ]
        );
    }

    /// Both protected reasons and both evidence-quality reasons must be on
    /// the refused side, named individually. The partition test above would
    /// still pass if `is_preauthorizable` started returning true for
    /// everything, so this is the test that would fail.
    #[test]
    fn live_use_and_ignorance_are_never_preauthorizable() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        for refused in [
            "resource_in_active_use",
            "evidence_incomplete",
            "evidence_stale",
            "evidence_probe_failed",
            "protected_credential_material",
        ] {
            assert!(
                report.never_preauthorizable_reasons.contains(&refused),
                "{refused} must be reported as never pre-authorizable"
            );
            assert!(!report.preauthorizable_reasons.contains(&refused));
        }
    }

    #[test]
    fn protected_and_unknown_incomplete_are_reported_as_never_executable() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        assert_eq!(
            report.never_executable_labels,
            vec!["PROTECTED", "UNKNOWN_INCOMPLETE"]
        );
    }

    /// The pressure choices are the CLI's own, so a client's threshold
    /// picker cannot offer a state `--min-pressure` would reject.
    #[test]
    fn pressure_states_are_every_state_in_urgency_order() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        assert_eq!(
            report.pressure_states,
            vec!["HEALTHY", "WARN", "PRESSURED", "CRITICAL", "EMERGENCY"]
        );
    }

    /// Not a wording test. The canonical invariant has to be in the payload
    /// a GUI renders, because a GUI that omitted it would be presenting
    /// standing deletion authority with the constraint left out.
    #[test]
    fn ai_authority_states_the_invariant_and_the_ordering_limit() {
        let report = envelope_report(&AutopilotEnvelope::revoked(), None);

        assert!(!report.ai_authority.is_empty());
        assert!(report
            .ai_authority
            .iter()
            .any(|line| line.contains("Policy decides")));
        assert!(report
            .ai_authority
            .iter()
            .any(|line| line.contains("order")));
    }

    /// The report is a projection, not a mutation path: producing one from an
    /// envelope leaves the envelope exactly as it was.
    #[test]
    fn reporting_does_not_change_the_envelope() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope.enable();
        let before = envelope.clone();

        let _ = envelope_report(&envelope, None);

        assert_eq!(envelope, before);
    }
}
