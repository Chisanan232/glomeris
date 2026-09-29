//! Why an external provider produced no observation (HORO-1546).
//!
//! # Two levels of detail, on purpose
//!
//! This enum has eight variants and [`ExternalProviderError::reason`] maps
//! them onto five [`ProbeReason`]s. That is not a lossy accident, it is the
//! layering:
//!
//! - **Locally** the full variant survives, because the person diagnosing a
//!   provider that is not working is the person whose machine it is, and
//!   "the base URL has no host" and "the token was refused" send them to
//!   different places.
//! - **On the wire** only the coarser reason goes, because a ranking model
//!   needs to know that an answer is missing and roughly why, and every extra
//!   token is one more thing describing this machine's setup to a third party.
//!
//! # What is deliberately not an error here
//!
//! "There is no pull request for this branch" and "there is no task with that
//! key" are *answers*. They are
//! [`crate::workspace::PullRequestState::NoneObserved`] and
//! [`crate::workspace::TaskState::NoneObserved`] — `Ok` values, reached only
//! when a provider answered. Nothing in this enum can produce them, which is
//! the structural half of the campaign's §10 rule: a provider that could not
//! answer and a provider that answered "nothing" cannot arrive as the same
//! value, because they do not even travel in the same variant of `Result`.

use crate::evidence::ProbeReason;
use std::fmt;

/// Why one read-only lookup against an external service produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalProviderError {
    /// Nothing was asked: no provider is enabled, or an enabled one has no
    /// credential in the environment. The default state of the product, and
    /// the one it must stay completely usable in.
    NotConfigured,
    /// Configuration that cannot work — a malformed base URL, an empty host.
    /// Refused before a request is built, so a URL that cannot work never
    /// reaches a request builder. Same rule as
    /// [`crate::actions::llm::validate_base_url`].
    InvalidConfiguration(String),
    /// The service answered and refused the credential.
    AuthRejected { status: u16 },
    /// The service answered and refused on quota.
    ///
    /// Kept apart from [`Self::AuthRejected`] even though GitHub reports both
    /// as `403`: the disambiguation is the remaining-quota header, and getting
    /// it wrong would tell a user to replace a token that is perfectly good.
    RateLimited,
    /// No answer arrived at all.
    Unreachable(String),
    /// No answer arrived before the deadline.
    TimedOut,
    /// The service answered with a status this adapter will not read as either
    /// an answer or an absence.
    ///
    /// A GitHub `404` on the repository lands here rather than becoming "no
    /// pull request", and that is the point: the `404` is an answer about the
    /// *container* — this credential cannot see that repository — and says
    /// nothing whatever about the branch. Turning it into `NoneObserved` is
    /// exactly the conflation §10 of the campaign forbids, and it would read
    /// as "nothing is open against this work" for every private repository a
    /// scopeless token cannot see.
    UnexpectedStatus { status: u16 },
    /// The service answered, and the body was not a shape this adapter can
    /// read. A working endpoint and credential must not be reported as a
    /// credential problem — same boundary as
    /// [`crate::actions::llm::LlmError::InvalidResponse`].
    UnusableResponse(String),
}

impl ExternalProviderError {
    /// The bounded reason this failure is allowed to travel as.
    ///
    /// Four of the eight map to [`ProbeReason::Failed`]. That is deliberate:
    /// they are all "the provider is configured and something went wrong in a
    /// way that is this machine's business", and splitting them on the wire
    /// would describe a local setup to a provider without changing any
    /// ranking. The distinctions AC 3 is about are the ones that survive —
    /// not configured, auth, quota and transport are four different reasons
    /// here, and none of them is the `NoneObserved` answer.
    pub fn reason(&self) -> ProbeReason {
        match self {
            Self::NotConfigured => ProbeReason::NotAttempted,
            Self::AuthRejected { .. } => ProbeReason::PermissionDenied,
            Self::RateLimited => ProbeReason::RateLimited,
            Self::TimedOut => ProbeReason::TimedOut,
            Self::InvalidConfiguration(_)
            | Self::Unreachable(_)
            | Self::UnexpectedStatus { .. }
            | Self::UnusableResponse(_) => ProbeReason::Failed,
        }
    }

    /// A stable snake_case token for the *local* surfaces — the CLI report and
    /// the preferences pane — where the full distinction is worth having.
    ///
    /// Sole producer of these strings, so
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` can diff them against
    /// the GUI's wording later. Same convention as
    /// [`crate::actions::llm::llm_check_outcome`].
    pub fn tag(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::InvalidConfiguration(_) => "invalid_configuration",
            Self::AuthRejected { .. } => "auth_rejected",
            Self::RateLimited => "rate_limited",
            Self::Unreachable(_) => "unreachable",
            Self::TimedOut => "timed_out",
            Self::UnexpectedStatus { .. } => "unexpected_status",
            Self::UnusableResponse(_) => "unusable_response",
        }
    }

    /// Every variant, for tests that must cover the enum without being edited
    /// when one is added. The payloads are placeholders; only the shape is
    /// used.
    #[cfg(test)]
    pub(crate) fn all() -> Vec<Self> {
        vec![
            Self::NotConfigured,
            Self::InvalidConfiguration("base URL has no host".into()),
            Self::AuthRejected { status: 401 },
            Self::RateLimited,
            Self::Unreachable("connection refused".into()),
            Self::TimedOut,
            Self::UnexpectedStatus { status: 404 },
            Self::UnusableResponse("response was not JSON".into()),
        ]
    }
}

impl fmt::Display for ExternalProviderError {
    /// One readable, credential-free sentence per variant.
    ///
    /// Note what none of these interpolate: a token, a URL, an account name or
    /// a subject identity. The status number and the adapter's own words are
    /// all a user needs, and a message is the one place a secret pasted into a
    /// base URL would otherwise resurface — see
    /// [`crate::reporting::dto::LlmCheckReport`] for the same reasoning about
    /// hosts.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => write!(
                f,
                "no external context provider is configured, so nothing was asked"
            ),
            Self::InvalidConfiguration(detail) => {
                write!(f, "external context is misconfigured: {detail}")
            }
            Self::AuthRejected { status } => write!(
                f,
                "the service answered {status} and refused the credential; \
                 check that the token exists and is allowed to read this subject"
            ),
            Self::RateLimited => write!(
                f,
                "the service refused on quota; it is reachable and the \
                 credential is good, so this answer is available later"
            ),
            Self::Unreachable(detail) => {
                write!(f, "the service could not be reached: {detail}")
            }
            Self::TimedOut => write!(f, "the service did not answer before the deadline"),
            Self::UnexpectedStatus { status } => write!(
                f,
                "the service answered {status}, which is neither an answer \
                 about this subject nor an absence of one"
            ),
            Self::UnusableResponse(detail) => {
                write!(f, "the service answered, unreadably: {detail}")
            }
        }
    }
}

impl std::error::Error for ExternalProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens are what a local surface branches on, so two failures
    /// sharing one would make a refused credential and an exhausted quota
    /// indistinguishable at exactly the point a user is deciding what to fix.
    #[test]
    fn every_variant_has_a_distinct_non_empty_tag() {
        let all = ExternalProviderError::all();
        let mut tags: Vec<&str> = all.iter().map(ExternalProviderError::tag).collect();
        assert_eq!(tags.len(), 8, "all() must list every variant");
        for tag in &tags {
            assert!(!tag.is_empty());
        }
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), all.len(), "two variants share a tag");
    }

    /// AC 3. The four the ticket names must be four different reasons on the
    /// wire, not one bucket.
    #[test]
    fn not_configured_auth_quota_and_transport_are_four_distinct_reasons() {
        let reasons = [
            ExternalProviderError::NotConfigured.reason(),
            ExternalProviderError::AuthRejected { status: 401 }.reason(),
            ExternalProviderError::RateLimited.reason(),
            ExternalProviderError::Unreachable("refused".into()).reason(),
        ];
        let mut tags: Vec<&str> = reasons.iter().map(|r| r.tag()).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), 4, "{reasons:?} collapsed onto fewer reasons");
    }

    /// Nothing in this enum may become "not attempted" except the one variant
    /// that really did attempt nothing. A transport failure reported as
    /// `not_attempted` would say Glomeris never asked, which is the opposite
    /// of what happened and hides a broken setup.
    #[test]
    fn only_not_configured_reports_that_nothing_was_attempted() {
        for error in ExternalProviderError::all() {
            let attempted_nothing = error.reason() == ProbeReason::NotAttempted;
            assert_eq!(
                attempted_nothing,
                error == ExternalProviderError::NotConfigured,
                "{error:?} maps to {:?}",
                error.reason()
            );
        }
    }

    /// A failure message ends up in a CLI report and may end up in a log. The
    /// literals carry no interpolated URL, so the only way one could appear is
    /// through a variant's own payload — and the two variants that carry free
    /// text are fed by this crate, never by a provider's response body.
    #[test]
    fn no_message_is_empty_and_none_names_a_credential() {
        for error in ExternalProviderError::all() {
            let message = error.to_string();
            assert!(!message.is_empty(), "{error:?} has no message");
            for forbidden in ["Bearer", "Basic ", "token=", "api_key", "://"] {
                assert!(
                    !message.contains(forbidden),
                    "{message:?} contains {forbidden:?}"
                );
            }
        }
    }

    /// The reason a quota refusal was worth its own `ProbeReason`: it is the
    /// one failure that is not a defect, and the message has to say so or a
    /// user will go and rotate a working token.
    #[test]
    fn the_quota_message_says_the_setup_is_fine() {
        let message = ExternalProviderError::RateLimited.to_string();
        assert!(message.contains("later"), "{message}");
        assert!(message.contains("credential is good"), "{message}");
    }
}
