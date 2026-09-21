//! The Autopilot policy envelope (HORO-1310): the complete, explicit
//! statement of what Autopilot is allowed to do.
//!
//! # Why this type exists at all
//!
//! Autopilot is the feature where a model gets to influence what actually
//! gets deleted, so the interesting question is not "what can the model
//! say" but "what is the largest amount of damage the most adversarial
//! possible model response could cause". An [`AutopilotEnvelope`] is this
//! crate's answer: a bound the model never sees, never sends, and cannot
//! name. Every field here is a ceiling on outcomes, not a hint about
//! intent.
//!
//! The model's entire influence is *ordering* — see
//! [`crate::autopilot::run`]. Ordering a list cannot add an item to it,
//! and every item was already discovered locally and already classified
//! by [`crate::policy::classify`]. So the worst a perfectly malicious
//! response can achieve is "the safe things you authorized, in an order
//! you did not choose".
//!
//! # Every field is a refusal, not a permission
//!
//! An envelope's default ([`AutopilotEnvelope::revoked`]) authorizes
//! nothing: disabled, with an empty resource-kind allowlist. Enabling it
//! is not sufficient — a kind must also be named. Authority here is only
//! ever enumerated, never inherited, and never implied by a neighbouring
//! setting.
//!
//! # Why the fields are private
//!
//! Because an envelope built by struct literal would let any caller — a
//! future GUI, a hand-edited JSON file, a well-meaning refactor — grant
//! something the constructors refuse:
//!
//! - [`ResourceKind::Unknown`] can never enter the allowlist. It is
//!   unconditionally `PROTECTED` in [`crate::policy::classify`], so
//!   allowlisting it could only ever be a misunderstanding, and a
//!   misunderstanding in an allowlist should fail loudly.
//! - Most [`ReasonCode`]s can never be pre-authorized (see
//!   [`is_preauthorizable`]). "I pre-authorize ASK" must not become a way
//!   to delete something a process currently has open.
//! - Budgets have hard ceilings ([`ACTIONS_CEILING`], [`BYTES_CEILING`],
//!   [`DURATION_CEILING`]) that no configuration can exceed. This is what
//!   makes "the thin client cannot widen the envelope" true in code
//!   rather than by convention: a Swift caller writing
//!   `max_bytes: u64::MAX` into the envelope file gets a rejected file,
//!   not an unbounded run.

use std::time::Duration;

use crate::evidence::ResourceKind;
use crate::monitor::pressure::PressureState;
use crate::policy::ReasonCode;

/// Hard ceiling on [`AutopilotEnvelope::max_actions`]. No configuration,
/// file, flag or client can raise it.
pub const ACTIONS_CEILING: u32 = 25;

/// Hard ceiling on [`AutopilotEnvelope::max_bytes`] — 64 GiB. Chosen as
/// "larger than any plausible single Autopilot run on a developer laptop,
/// smaller than a disk", so that a corrupted or hostile configuration
/// value cannot express "everything".
pub const BYTES_CEILING: u64 = 64 * 1024 * 1024 * 1024;

/// Hard ceiling on [`AutopilotEnvelope::max_duration`].
pub const DURATION_CEILING: Duration = Duration::from_secs(15 * 60);

/// Default action budget for a newly enabled envelope. Deliberately small
/// enough that a first run is something a human can read the audit trail
/// of in full.
pub const DEFAULT_MAX_ACTIONS: u32 = 3;

/// Default byte budget for a newly enabled envelope — 5 GiB.
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Default wall-clock budget for one Autopilot run.
pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(60);

/// Whether `reason` is a risk an Autopilot envelope may be pre-authorized
/// for.
///
/// The rule, stated once: **Autopilot may be pre-authorized for risk that
/// is about cost, never for risk that is about live use, and never for
/// absence of knowledge.** Losing a `not_regenerable` build output costs
/// rebuild time, which a human can decide to accept in advance because
/// the cost does not change between deciding and acting. Whether a process
/// has the resource open right now is not like that — it is a fact about
/// this instant, so consenting to it in advance consents to nothing
/// meaningful.
///
/// Written as an exhaustive match rather than a deny-list so that adding a
/// [`ReasonCode`] to the policy engine does not silently grant it
/// pre-authorizability: the compiler stops here and asks.
pub fn is_preauthorizable(reason: ReasonCode) -> bool {
    match reason {
        // The one Ask reason that is a cost judgment rather than a
        // statement about right now, or about not knowing.
        ReasonCode::RebuildCostHigh => true,

        // Evidence quality. These are exactly what
        // `PolicyLabel::UnknownIncomplete` is derived from: we do not know
        // enough to judge. Pre-authorizing ignorance is the single worst
        // thing this envelope could be allowed to express.
        ReasonCode::EvidenceIncomplete
        | ReasonCode::EvidenceStale
        | ReasonCode::EvidenceProbeFailed => false,

        // Live use. True at classification time, possibly false a second
        // later, and vice versa — not a thing anybody can consent to in
        // advance.
        ReasonCode::ResourceInActiveUse
        | ReasonCode::GitWorktreeDirty
        | ReasonCode::OwningToolLive => false,

        // Protected reasons. `authorize` refuses these unconditionally, so
        // an envelope entry naming one could never do anything except
        // mislead whoever read the envelope.
        ReasonCode::ProtectedCredentialMaterial
        | ReasonCode::ProtectedGitInternals
        | ReasonCode::ProtectedInfraState
        | ReasonCode::ProtectedPersistentVolume
        | ReasonCode::ProtectedUserDocuments
        | ReasonCode::ProtectedSystemPath
        | ReasonCode::ProtectedUnsafeMountOrSymlink
        | ReasonCode::ProtectedUnknownResourceKind => false,

        // AutoSafe reasons are not Ask reasons; an `ASK` pre-authorization
        // naming one describes a decision that cannot occur.
        ReasonCode::EvidenceFreshAndComplete
        | ReasonCode::RegenerableByTool
        | ReasonCode::NoActiveUseObserved => false,
    }
}

/// Why an attempt to widen an [`AutopilotEnvelope`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    /// [`ResourceKind::Unknown`] — unconditionally `PROTECTED`, so it can
    /// never be allowlisted.
    KindNotAllowlistable(ResourceKind),
    /// See [`is_preauthorizable`].
    ReasonNotPreauthorizable(ReasonCode),
    /// An `ASK` pre-authorization for a kind that is not in the allowlist:
    /// it would authorize nothing while reading as though it did.
    PreauthorizedKindNotAllowed(ResourceKind),
    ActionsAboveCeiling {
        requested: u32,
        ceiling: u32,
    },
    BytesAboveCeiling {
        requested: u64,
        ceiling: u64,
    },
    DurationAboveCeiling {
        requested: Duration,
        ceiling: Duration,
    },
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvelopeError::KindNotAllowlistable(kind) => write!(
                f,
                "resource kind '{}' can never be allowlisted: it is \
                 unconditionally PROTECTED",
                kind.tag()
            ),
            EnvelopeError::ReasonNotPreauthorizable(reason) => write!(
                f,
                "reason '{}' can never be pre-authorized: Autopilot may be \
                 pre-authorized for rebuild cost only, never for live use \
                 or for incomplete evidence",
                reason.as_str()
            ),
            EnvelopeError::PreauthorizedKindNotAllowed(kind) => write!(
                f,
                "cannot pre-authorize ASK for resource kind '{}': it is not \
                 in the envelope's allowed kinds",
                kind.tag()
            ),
            EnvelopeError::ActionsAboveCeiling { requested, ceiling } => write!(
                f,
                "max actions {requested} exceeds the hard ceiling of {ceiling}"
            ),
            EnvelopeError::BytesAboveCeiling { requested, ceiling } => write!(
                f,
                "max bytes {requested} exceeds the hard ceiling of {ceiling}"
            ),
            EnvelopeError::DurationAboveCeiling { requested, ceiling } => write!(
                f,
                "max duration {}s exceeds the hard ceiling of {}s",
                requested.as_secs(),
                ceiling.as_secs()
            ),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// Standing authorization for one `(resource kind, reason)` pair — the
/// narrow form of "ASK is pre-authorized" the ticket calls for.
///
/// Narrow in two senses at once. It names a kind, so pre-authorizing
/// rebuild cost for `cargo_target_dir` says nothing about
/// `xcode_derived_data`. And it names a single [`ReasonCode`], so it
/// authorizes one specific risk rather than the whole heterogeneous `Ask`
/// bucket — which matters because a candidate whose fresh decision carries
/// *any* reason outside the pre-authorized set is refused (see
/// [`AutopilotEnvelope::ask_preauthorized`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskPreauthorization {
    kind: ResourceKind,
    reason: ReasonCode,
}

impl AskPreauthorization {
    pub fn kind(&self) -> ResourceKind {
        self.kind
    }

    pub fn reason(&self) -> ReasonCode {
        self.reason
    }
}

/// The complete statement of what Autopilot may do. See the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct AutopilotEnvelope {
    enabled: bool,
    allowed_kinds: Vec<ResourceKind>,
    ask_preauthorizations: Vec<AskPreauthorization>,
    max_actions: u32,
    max_bytes: u64,
    max_duration: Duration,
    min_pressure: Option<PressureState>,
}

impl Default for AutopilotEnvelope {
    fn default() -> Self {
        Self::revoked()
    }
}

impl AutopilotEnvelope {
    /// An envelope that authorizes nothing: disabled, empty allowlist, no
    /// pre-authorizations, conservative budgets.
    ///
    /// This is also [`Default`], which is deliberate — every way of
    /// arriving at an envelope without saying anything must arrive at one
    /// that does nothing. A missing configuration file, a deserialization
    /// that fell back to a default, a `mem::take`: all of them land here.
    pub fn revoked() -> Self {
        Self {
            enabled: false,
            allowed_kinds: Vec::new(),
            ask_preauthorizations: Vec::new(),
            max_actions: DEFAULT_MAX_ACTIONS,
            max_bytes: DEFAULT_MAX_BYTES,
            max_duration: DEFAULT_MAX_DURATION,
            min_pressure: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Flips the enable bit. Grants nothing on its own: an enabled
    /// envelope with an empty allowlist still executes nothing.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Revokes authorization, keeping the rest of the envelope intact so a
    /// later re-enable does not silently come back with different limits
    /// than the ones the user last read.
    ///
    /// Revocation is a single bit for a reason: it must be impossible for
    /// it to partially fail. Every run re-reads the envelope from disk
    /// immediately before acting (see [`crate::autopilot::store`]), so
    /// clearing this bit takes effect without restarting the daemon, the
    /// app, or anything else.
    pub fn revoke(&mut self) {
        self.enabled = false;
    }

    /// Adds `kind` to the allowlist. Idempotent.
    pub fn allow_kind(&mut self, kind: ResourceKind) -> Result<(), EnvelopeError> {
        if kind == ResourceKind::Unknown {
            return Err(EnvelopeError::KindNotAllowlistable(kind));
        }
        if !self.allowed_kinds.contains(&kind) {
            self.allowed_kinds.push(kind);
        }
        Ok(())
    }

    /// Pre-authorizes one `(kind, reason)` pair. Idempotent.
    ///
    /// Requires `kind` to already be allowlisted: a pre-authorization for
    /// a kind Autopilot may not touch at all authorizes nothing, and an
    /// envelope that reads as though it granted something it did not is
    /// worse than one that refused.
    pub fn preauthorize_ask(
        &mut self,
        kind: ResourceKind,
        reason: ReasonCode,
    ) -> Result<(), EnvelopeError> {
        if !is_preauthorizable(reason) {
            return Err(EnvelopeError::ReasonNotPreauthorizable(reason));
        }
        if !self.allowed_kinds.contains(&kind) {
            return Err(EnvelopeError::PreauthorizedKindNotAllowed(kind));
        }
        let entry = AskPreauthorization { kind, reason };
        if !self.ask_preauthorizations.contains(&entry) {
            self.ask_preauthorizations.push(entry);
        }
        Ok(())
    }

    pub fn set_max_actions(&mut self, max_actions: u32) -> Result<(), EnvelopeError> {
        if max_actions > ACTIONS_CEILING {
            return Err(EnvelopeError::ActionsAboveCeiling {
                requested: max_actions,
                ceiling: ACTIONS_CEILING,
            });
        }
        self.max_actions = max_actions;
        Ok(())
    }

    pub fn set_max_bytes(&mut self, max_bytes: u64) -> Result<(), EnvelopeError> {
        if max_bytes > BYTES_CEILING {
            return Err(EnvelopeError::BytesAboveCeiling {
                requested: max_bytes,
                ceiling: BYTES_CEILING,
            });
        }
        self.max_bytes = max_bytes;
        Ok(())
    }

    pub fn set_max_duration(&mut self, max_duration: Duration) -> Result<(), EnvelopeError> {
        if max_duration > DURATION_CEILING {
            return Err(EnvelopeError::DurationAboveCeiling {
                requested: max_duration,
                ceiling: DURATION_CEILING,
            });
        }
        self.max_duration = max_duration;
        Ok(())
    }

    /// Requires disk pressure to have reached at least `min_pressure`
    /// before Autopilot will run at all. `None` (the default) means the
    /// run is not gated on pressure.
    ///
    /// Unbounded, unlike the budgets, because every value of this field
    /// narrows the envelope rather than widening it — there is no
    /// direction to abuse.
    pub fn set_min_pressure(&mut self, min_pressure: Option<PressureState>) {
        self.min_pressure = min_pressure;
    }

    pub fn allowed_kinds(&self) -> &[ResourceKind] {
        &self.allowed_kinds
    }

    pub fn ask_preauthorizations(&self) -> &[AskPreauthorization] {
        &self.ask_preauthorizations
    }

    pub fn max_actions(&self) -> u32 {
        self.max_actions
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn max_duration(&self) -> Duration {
        self.max_duration
    }

    pub fn min_pressure(&self) -> Option<PressureState> {
        self.min_pressure
    }

    pub fn permits_kind(&self, kind: ResourceKind) -> bool {
        self.allowed_kinds.contains(&kind)
    }

    /// Whether an `Ask` decision carrying exactly `reasons` for a resource
    /// of `kind` is pre-authorized.
    ///
    /// Requires **every** reason to be individually pre-authorized for this
    /// kind. `Ask` is a heterogeneous bucket — the same invariant
    /// [`crate::executor::execute`] enforces with
    /// [`crate::executor::AbortReason::PolicyReasonsWidened`] — so a
    /// decision reading `Ask{RebuildCostHigh, ResourceInActiveUse}` is not
    /// covered by a pre-authorization of `RebuildCostHigh`. It is a
    /// different, larger risk that happens to include the authorized one.
    ///
    /// An empty `reasons` slice is never pre-authorized: "all zero of the
    /// reasons are covered" is vacuously true and exactly the wrong answer
    /// for a function whose output is permission.
    pub fn ask_preauthorized(&self, kind: ResourceKind, reasons: &[ReasonCode]) -> bool {
        if reasons.is_empty() {
            return false;
        }
        reasons.iter().all(|reason| {
            self.ask_preauthorizations
                .iter()
                .any(|entry| entry.kind == kind && entry.reason == *reason)
        })
    }

    /// Renders the envelope as the answer to "what is Autopilot allowed to
    /// do", one fact per line (HORO-1310 AC 6: the user must be able to see
    /// exactly that, before enabling it).
    ///
    /// Written to be read by somebody deciding whether to enable this, so
    /// it states the refusals too — the things no envelope can authorize
    /// are part of what the envelope means, and they are the part a reader
    /// cannot infer from a list of settings.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();

        lines.push(format!(
            "status:            {}",
            if self.enabled { "ENABLED" } else { "revoked" }
        ));

        if self.allowed_kinds.is_empty() {
            lines.push("allowed kinds:     none — nothing can run".to_string());
        } else {
            let kinds: Vec<&str> = self.allowed_kinds.iter().map(|k| k.tag()).collect();
            lines.extend(wrap_labelled_list("allowed kinds:", &kinds));
        }

        lines.push(format!(
            "max actions:       {}{}",
            self.max_actions,
            if self.max_actions == 0 {
                " — nothing can run"
            } else {
                ""
            }
        ));
        lines.push(format!(
            "max bytes:         {}",
            crate::reporting::human_bytes(self.max_bytes)
        ));
        lines.push(format!(
            "max duration:      {}s",
            self.max_duration.as_secs()
        ));
        lines.push(format!(
            "disk pressure:     {}",
            match self.min_pressure {
                Some(state) => format!("runs only at {} or worse", state.as_str()),
                None => "not required".to_string(),
            }
        ));

        if self.ask_preauthorizations.is_empty() {
            lines.push("ASK pre-authorized: none — ASK always refused".to_string());
        } else {
            for entry in &self.ask_preauthorizations {
                lines.push(format!(
                    "ASK pre-authorized: {} for {}",
                    entry.reason.as_str(),
                    entry.kind.tag()
                ));
            }
        }

        lines.push("never executable:   PROTECTED, UNKNOWN_INCOMPLETE".to_string());
        lines.push("AI authority:       ordering only, never selection of".to_string());
        lines.push("                    paths, commands, or policy classes".to_string());

        lines
    }
}

/// Column the values in [`AutopilotEnvelope::describe`] start at, so a
/// wrapped list lines up under its own first entry rather than under the
/// label.
const DESCRIBE_VALUE_COLUMN: usize = 19;

/// Renders `label` followed by a comma-separated `items`, wrapped so no
/// line exceeds 80 columns and continuation lines are indented to
/// [`DESCRIBE_VALUE_COLUMN`].
///
/// Exists because the allowlist can name eight resource kinds whose tags
/// total well over a terminal width, and a describe() that wraps at the
/// terminal's mercy is a describe() a user reads wrong.
fn wrap_labelled_list(label: &str, items: &[&str]) -> Vec<String> {
    const LIMIT: usize = 80;

    let indent = " ".repeat(DESCRIBE_VALUE_COLUMN);
    let mut lines = Vec::new();
    let mut current = format!("{label:<DESCRIBE_VALUE_COLUMN$}");
    let mut current_has_item = false;

    for (index, item) in items.iter().enumerate() {
        let piece = if index + 1 == items.len() {
            (*item).to_string()
        } else {
            format!("{item},")
        };

        // The `+ 1` is the space this piece would be separated by. Only
        // wrap when the line already carries an item, so a single item
        // longer than the limit overflows rather than landing on a line of
        // its own that still overflows.
        if current_has_item && current.chars().count() + 1 + piece.chars().count() > LIMIT {
            lines.push(current);
            current = indent.clone();
            current_has_item = false;
        }

        if current_has_item {
            current.push(' ');
        }
        current.push_str(&piece);
        current_has_item = true;
    }

    lines.push(current.trim_end().to_string());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_revoked_are_the_same_envelope_and_authorize_nothing() {
        let envelope = AutopilotEnvelope::default();
        assert_eq!(envelope, AutopilotEnvelope::revoked());
        assert!(!envelope.is_enabled());
        assert!(envelope.allowed_kinds().is_empty());
        assert!(envelope.ask_preauthorizations().is_empty());
        assert!(envelope.min_pressure().is_none());
    }

    /// Enabling is necessary but never sufficient. The allowlist is a
    /// separate, also-mandatory act.
    #[test]
    fn enabling_alone_permits_no_kind() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        assert!(envelope.is_enabled());
        for kind in ResourceKind::ALL {
            assert!(
                !envelope.permits_kind(*kind),
                "an enabled but empty envelope permits {}",
                kind.tag()
            );
        }
    }

    #[test]
    fn unknown_kind_can_never_be_allowlisted() {
        let mut envelope = AutopilotEnvelope::revoked();
        assert_eq!(
            envelope.allow_kind(ResourceKind::Unknown),
            Err(EnvelopeError::KindNotAllowlistable(ResourceKind::Unknown))
        );
        assert!(!envelope.permits_kind(ResourceKind::Unknown));
        assert!(envelope.allowed_kinds().is_empty());
    }

    #[test]
    fn allow_kind_is_idempotent() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        assert_eq!(envelope.allowed_kinds(), &[ResourceKind::CargoTargetDir]);
    }

    #[test]
    fn revoke_clears_only_the_enable_bit() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope.set_max_actions(7).unwrap();
        envelope.revoke();

        assert!(!envelope.is_enabled());
        // The limits the user last read are still the limits, so a later
        // re-enable is not a surprise.
        assert_eq!(envelope.allowed_kinds(), &[ResourceKind::CargoTargetDir]);
        assert_eq!(envelope.max_actions(), 7);
    }

    /// The pre-authorizability rule, asserted over every reason code rather
    /// than over a couple of examples: exactly one reason is
    /// pre-authorizable, and it is the cost one.
    #[test]
    fn exactly_rebuild_cost_high_is_preauthorizable() {
        let preauthorizable: Vec<&str> = ReasonCode::ALL
            .iter()
            .filter(|r| is_preauthorizable(**r))
            .map(|r| r.as_str())
            .collect();
        assert_eq!(preauthorizable, vec!["rebuild_cost_high"]);
    }

    #[test]
    fn evidence_quality_and_live_use_reasons_are_refused_by_preauthorize_ask() {
        for reason in [
            ReasonCode::EvidenceIncomplete,
            ReasonCode::EvidenceStale,
            ReasonCode::EvidenceProbeFailed,
            ReasonCode::ResourceInActiveUse,
            ReasonCode::GitWorktreeDirty,
            ReasonCode::OwningToolLive,
        ] {
            let mut envelope = AutopilotEnvelope::revoked();
            envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
            assert_eq!(
                envelope.preauthorize_ask(ResourceKind::CargoTargetDir, reason),
                Err(EnvelopeError::ReasonNotPreauthorizable(reason)),
                "{} must not be pre-authorizable",
                reason.as_str()
            );
            assert!(envelope.ask_preauthorizations().is_empty());
        }
    }

    #[test]
    fn protected_reasons_are_refused_by_preauthorize_ask() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        assert!(envelope
            .preauthorize_ask(
                ResourceKind::CargoTargetDir,
                ReasonCode::ProtectedCredentialMaterial
            )
            .is_err());
    }

    #[test]
    fn preauthorizing_a_kind_outside_the_allowlist_is_refused() {
        let mut envelope = AutopilotEnvelope::revoked();
        assert_eq!(
            envelope.preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh),
            Err(EnvelopeError::PreauthorizedKindNotAllowed(
                ResourceKind::CargoTargetDir
            ))
        );
    }

    #[test]
    fn ask_preauthorization_is_scoped_to_the_kind_it_names() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope.allow_kind(ResourceKind::XcodeDerivedData).unwrap();
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .unwrap();

        assert!(envelope
            .ask_preauthorized(ResourceKind::CargoTargetDir, &[ReasonCode::RebuildCostHigh]));
        assert!(!envelope.ask_preauthorized(
            ResourceKind::XcodeDerivedData,
            &[ReasonCode::RebuildCostHigh]
        ));
    }

    /// The `Ask`-is-heterogeneous invariant: a pre-authorization covers a
    /// reason, not a bucket.
    #[test]
    fn ask_preauthorization_does_not_cover_a_decision_with_an_extra_reason() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .unwrap();

        assert!(!envelope.ask_preauthorized(
            ResourceKind::CargoTargetDir,
            &[ReasonCode::RebuildCostHigh, ReasonCode::ResourceInActiveUse],
        ));
    }

    #[test]
    fn an_empty_reason_set_is_never_preauthorized() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .unwrap();

        assert!(!envelope.ask_preauthorized(ResourceKind::CargoTargetDir, &[]));
    }

    #[test]
    fn budgets_cannot_exceed_their_hard_ceilings() {
        let mut envelope = AutopilotEnvelope::revoked();

        assert_eq!(
            envelope.set_max_actions(ACTIONS_CEILING + 1),
            Err(EnvelopeError::ActionsAboveCeiling {
                requested: ACTIONS_CEILING + 1,
                ceiling: ACTIONS_CEILING,
            })
        );
        assert_eq!(
            envelope.set_max_bytes(u64::MAX),
            Err(EnvelopeError::BytesAboveCeiling {
                requested: u64::MAX,
                ceiling: BYTES_CEILING,
            })
        );
        assert!(envelope
            .set_max_duration(DURATION_CEILING + Duration::from_secs(1))
            .is_err());

        // A refused write leaves the previous value in place — never a
        // partially applied budget, and never a silent clamp to the
        // ceiling, which would let an over-large request read as accepted.
        assert_eq!(envelope.max_actions(), DEFAULT_MAX_ACTIONS);
        assert_eq!(envelope.max_bytes(), DEFAULT_MAX_BYTES);
        assert_eq!(envelope.max_duration(), DEFAULT_MAX_DURATION);
    }

    #[test]
    fn budgets_exactly_at_the_ceiling_are_accepted() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.set_max_actions(ACTIONS_CEILING).unwrap();
        envelope.set_max_bytes(BYTES_CEILING).unwrap();
        envelope.set_max_duration(DURATION_CEILING).unwrap();
        assert_eq!(envelope.max_actions(), ACTIONS_CEILING);
    }

    #[test]
    fn describe_states_status_limits_and_the_things_no_envelope_can_authorize() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        envelope.allow_kind(ResourceKind::CargoTargetDir).unwrap();
        envelope
            .preauthorize_ask(ResourceKind::CargoTargetDir, ReasonCode::RebuildCostHigh)
            .unwrap();
        envelope.set_min_pressure(Some(PressureState::Pressured));

        let rendered = envelope.describe().join("\n");

        assert!(rendered.contains("ENABLED"), "{rendered}");
        assert!(rendered.contains("cargo_target_dir"), "{rendered}");
        assert!(rendered.contains("rebuild_cost_high"), "{rendered}");
        assert!(rendered.contains("PROTECTED"), "{rendered}");
        assert!(rendered.contains("UNKNOWN_INCOMPLETE"), "{rendered}");
        assert!(rendered.contains("ordering only"), "{rendered}");
    }

    /// A revoked envelope must say so in the first thing anybody reads, and
    /// must not describe its dormant limits in a way that reads as active.
    #[test]
    fn describe_leads_with_revoked_status_for_a_revoked_envelope() {
        let lines = AutopilotEnvelope::revoked().describe();
        assert!(lines[0].contains("revoked"), "{:?}", lines[0]);
        assert!(
            lines.iter().any(|l| l.contains("nothing can run")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("ASK always refused")),
            "{lines:?}"
        );
    }

    /// `describe` is printed in a terminal and inside a fixed-width GUI
    /// panel, so it holds to the same 80-column bound the CLI help does.
    #[test]
    fn describe_lines_fit_eighty_columns() {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        for kind in ResourceKind::ALL
            .iter()
            .filter(|k| **k != ResourceKind::Unknown)
        {
            envelope.allow_kind(*kind).unwrap();
        }
        envelope.set_max_bytes(BYTES_CEILING).unwrap();
        envelope.set_min_pressure(Some(PressureState::Emergency));

        for line in envelope.describe() {
            assert!(
                line.chars().count() <= 80,
                "describe() line is {} columns: {line}",
                line.chars().count()
            );
        }
    }
}
