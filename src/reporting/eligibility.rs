//! Eligibility taxonomy (HORO-1828 / ADR-0001 §12 AC map: "Distinguish
//! NOTHING_ELIGIBLE vs DISCOVERY_INCOMPLETE vs UNKNOWN").
//!
//! `glomeris detect`/`free --dry-run` already tell a user *what* was found
//! and *why* each candidate is or isn't actionable, one candidate at a
//! time. What they did not say, before this ticket, is the one sentence an
//! operator actually wants within 10-30 seconds: "is there anything for me
//! to do here at all, and if not, why not?" An empty or all-protected
//! candidate list reads identically today whether discovery genuinely found
//! nothing, discovery itself was incomplete, or evidence was too thin to
//! classify anything — and only the first of those three is a real clean
//! bill of health.
//!
//! This module adds nothing to evidence or policy. It is a pure,
//! read-only projection over already-computed [`DetectCandidateReport`]s
//! (themselves already a projection of `Evidence`/`PolicyDecision` —
//! see [`super::dto`]'s own module doc comment) plus the already-computed
//! `discovery_complete` flag. No new policy class, no new reason code, no
//! new probe.
//!
//! # Precedence (checked in this exact order; see [`eligibility_verdict`])
//!
//! 1. Discovery itself was incomplete (a detector failed or was not run) —
//!    [`EligibilityVerdict::DiscoveryIncomplete`]. This is checked FIRST,
//!    before anything about the candidates themselves, because an
//!    incomplete scan cannot honestly support any claim about what it did
//!    or didn't find — including "nothing eligible".
//! 2. At least one candidate is executable without confirmation —
//!    [`EligibilityVerdict::Eligible`]. There is real, automatic safe work
//!    available right now.
//! 3. No automatic candidate exists, but at least one candidate's evidence
//!    was too thin to classify at all (`UNKNOWN_INCOMPLETE`) —
//!    [`EligibilityVerdict::Unknown`]. This is deliberately checked before
//!    "nothing eligible": a disk full of unresolved evidence is not the
//!    same honest state as a disk that was fully examined and found clean.
//! 4. Otherwise — [`EligibilityVerdict::NothingEligible`], including when
//!    there are no candidates at all.
//!
//! # The HORO-1629 relationship (do not duplicate it here)
//!
//! A configured root that is itself a parent-of-projects folder (no project
//! marker, no build output directly inside it) produces zero candidates
//! from that root and, with nothing else to report, lands this function on
//! [`EligibilityVerdict::NothingEligible`] — a clean bill of health for a
//! folder that was never really examined below the surface. That is
//! exactly the false-all-clear HORO-1629 is scoped to fix, with its own
//! acceptance criteria and founder-approved wording (ADR-0001 §16.1). This
//! module does not re-implement, pre-empt, or restate that fix: it only
//! names the relationship so a future reader does not mistake
//! `NothingEligible` for a safe default in every case, and does not add any
//! parent-folder detection logic of its own.

use super::dto::DetectCandidateReport;
use super::policy_label::PolicyLabel;

/// What, if anything, an operator can actually do right now — see this
/// module's doc comment for the exact precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EligibilityVerdict {
    /// Discovery itself did not finish (a detector failed, or was not run) —
    /// nothing here can be read as a complete picture, including an empty
    /// candidate list.
    DiscoveryIncomplete,
    /// Discovery completed, but at least one candidate's evidence was too
    /// thin to classify (`UNKNOWN_INCOMPLETE`) and no candidate is
    /// executable without confirmation.
    Unknown,
    /// Discovery completed, every candidate's evidence was resolvable, and
    /// none of them is executable without confirmation — including the
    /// case of zero candidates at all.
    NothingEligible,
    /// At least one candidate is executable without confirmation: real,
    /// automatic safe work is available right now.
    Eligible,
}

impl EligibilityVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            EligibilityVerdict::DiscoveryIncomplete => "discovery_incomplete",
            EligibilityVerdict::Unknown => "unknown",
            EligibilityVerdict::NothingEligible => "nothing_eligible",
            EligibilityVerdict::Eligible => "eligible",
        }
    }
}

/// Derives the eligibility verdict for a completed (or incomplete)
/// discovery pass. See this module's doc comment for the precedence and
/// its rationale. Pure: takes already-computed reports, touches nothing.
pub fn eligibility_verdict(
    discovery_complete: bool,
    candidates: &[DetectCandidateReport],
) -> EligibilityVerdict {
    if !discovery_complete {
        return EligibilityVerdict::DiscoveryIncomplete;
    }

    let has_automatic_candidate = candidates.iter().any(|c| {
        c.offered_actions
            .iter()
            .any(|action| !action.requires_confirmation)
    });
    if has_automatic_candidate {
        return EligibilityVerdict::Eligible;
    }

    let has_unknown_candidate = candidates
        .iter()
        .any(|c| c.policy_label == PolicyLabel::UnknownIncomplete.as_str());
    if has_unknown_candidate {
        return EligibilityVerdict::Unknown;
    }

    EligibilityVerdict::NothingEligible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::dto::OfferedAction;

    fn candidate(policy_label: &'static str, executable_no_confirm: bool) -> DetectCandidateReport {
        let offered_actions = if executable_no_confirm {
            vec![OfferedAction {
                action_id: "cargo.clean.target_dir".to_string(),
                requires_confirmation: false,
            }]
        } else {
            Vec::new()
        };
        DetectCandidateReport {
            resource_id: "cargo_target_dir:/tmp/x".to_string(),
            kind: "cargo_target_dir",
            reclaimable_bytes: Some(1),
            reclaimable_human: Some("1 B".to_string()),
            reclaimable_bytes_is_lower_bound: false,
            impact_tier: "normal",
            policy_label,
            reasons: vec![],
            executable: executable_no_confirm,
            offered_actions,
            refusal_reason: None,
        }
    }

    fn ask_requires_confirmation() -> DetectCandidateReport {
        DetectCandidateReport {
            offered_actions: vec![OfferedAction {
                action_id: "cargo.clean.target_dir".to_string(),
                requires_confirmation: true,
            }],
            executable: true,
            ..candidate("ASK", false)
        }
    }

    /// Precedence 1: discovery incomplete wins regardless of candidate shape,
    /// including a candidate that would otherwise be `Eligible`.
    #[test]
    fn discovery_incomplete_wins_even_with_an_automatic_candidate() {
        let candidates = vec![candidate("AUTO_SAFE", true)];
        assert_eq!(
            eligibility_verdict(false, &candidates),
            EligibilityVerdict::DiscoveryIncomplete
        );
    }

    /// Precedence 2: an automatic candidate wins over an `UNKNOWN_INCOMPLETE`
    /// sibling.
    #[test]
    fn eligible_wins_over_an_unknown_sibling() {
        let candidates = vec![
            candidate("AUTO_SAFE", true),
            candidate("UNKNOWN_INCOMPLETE", false),
        ];
        assert_eq!(
            eligibility_verdict(true, &candidates),
            EligibilityVerdict::Eligible
        );
    }

    /// Precedence 3: no automatic candidate, but an `UNKNOWN_INCOMPLETE` one
    /// exists among otherwise-resolved candidates.
    #[test]
    fn unknown_when_no_automatic_candidate_but_one_is_unknown_incomplete() {
        let candidates = vec![
            ask_requires_confirmation(),
            candidate("UNKNOWN_INCOMPLETE", false),
        ];
        assert_eq!(
            eligibility_verdict(true, &candidates),
            EligibilityVerdict::Unknown
        );
    }

    /// Precedence 4: discovery complete, nothing automatic, nothing unknown
    /// — real candidates exist (e.g. protected or confirmation-gated) but
    /// none is eligible.
    #[test]
    fn nothing_eligible_when_only_protected_and_confirmation_gated_candidates_exist() {
        let candidates = vec![candidate("PROTECTED", false), ask_requires_confirmation()];
        assert_eq!(
            eligibility_verdict(true, &candidates),
            EligibilityVerdict::NothingEligible
        );
    }

    /// Precedence 4, the degenerate case: discovery complete, zero
    /// candidates at all. This is exactly the HORO-1629 false-all-clear
    /// shape this module's doc comment describes — see that comment for why
    /// it is not fixed here.
    #[test]
    fn nothing_eligible_when_there_are_no_candidates_at_all() {
        assert_eq!(
            eligibility_verdict(true, &[]),
            EligibilityVerdict::NothingEligible
        );
    }

    /// Anti-vacuity: an `AUTO_SAFE`-labelled candidate that is nonetheless
    /// not executable (e.g. a live tool currently using it) must not be
    /// read as `Eligible` — only the `offered_actions`/`requires_confirmation`
    /// fields decide this, never the policy label text.
    #[test]
    fn auto_safe_label_with_no_offered_action_is_not_eligible() {
        let candidates = vec![candidate("AUTO_SAFE", false)];
        assert_eq!(
            eligibility_verdict(true, &candidates),
            EligibilityVerdict::NothingEligible
        );
    }
}
