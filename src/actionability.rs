//! The one answer to "would execution refuse this action outright?"
//! (HORO-1360).
//!
//! HORO-1358 established the rule: a registered action *existing* for a
//! resource kind is not the same claim as that action being *runnable*.
//! `homebrew.cleanup.cache` is registered for `HomebrewCache`, but `brew
//! cleanup -s` takes no path to scope, so its step carries `scoped_path:
//! None` and [`crate::executor::structural_refusal`] rejects it on sight,
//! every time, regardless of policy class. It put that rule into
//! `reporting::dto`, so `detect --json` and `explain --json` stopped
//! promising a Clean button that could not work.
//!
//! Five other surfaces choose an action *without* consulting that answer:
//! the LLM prompt view, Autopilot, `clean --dry-run`, `free` and
//! `emergency`. Each resolves from the registry keyed on resource kind and
//! stops there. So the product could still nominate an action it already
//! knew execution would refuse.
//!
//! This module exists so the fix is one predicate that all of them read,
//! rather than five restatements of the same rule that can drift apart. It
//! is deliberately the *only* place the sequencing below is expressed.
//!
//! # This is not a safety boundary
//!
//! Nothing here is what stands between an unscoped mutation and execution —
//! [`crate::executor::structural_refusal`] is, it runs independently inside
//! [`crate::executor::execute`], and it is unchanged. This module only
//! stops the product from *choosing* an action that would then be refused.
//! Deleting this module would restore a guaranteed-to-fail recommendation;
//! it would not make anything unsafe.
//!
//! Clearing these checks means "not already refused", **not** "guaranteed
//! to succeed": `structural_refusal` is blind to runtime state, and
//! deletion-time revalidation may still abort.
//!
//! # Why the ordering is here and not at each call site
//!
//! Deciding whether an action would be refused requires *planning* it, and
//! a [`PolicyClass::Protected`] resource must never be planned — that is
//! what makes "no LLM response can cause a protected resource's action to
//! be planned" structurally true rather than a comment (see
//! `tests/golden_llm_plan_protected_refusal.rs`). So the Protected check
//! has to come first, and every caller needing it gets it from
//! [`static_refusal`] / [`eligible_action_ids`] instead of rediscovering
//! the ordering. [`plan_refusal`] is the policy-free half, for callers that
//! have already passed a Protected gate and can say so.

use crate::actions::{Action, ActionRegistry};
use crate::evidence::Evidence;
use crate::policy::{PolicyClass, PolicyDecision};

/// Whether execution would refuse `action` for `ev` on sight: `None` when
/// nothing statically known refuses it, `Some(reason)` with a sentence a
/// user can read otherwise.
///
/// Takes no policy input at all, deliberately — exactly as
/// [`crate::executor::structural_refusal`] takes none. **Calling this on a
/// [`PolicyClass::Protected`] resource plans that resource's action**,
/// which some callers must never do; they want [`static_refusal`].
///
/// Costs one [`Action::plan`] call. Of the three registered actions only
/// `cargo.clean.target_dir` touches the filesystem while planning, and
/// only for a single `is_file` stat on `Cargo.toml`.
pub fn plan_refusal(action: &dyn Action, ev: &Evidence) -> Option<String> {
    // An action that cannot even produce a plan for this resource certainly
    // cannot run against it. Reported with the planner's own words rather
    // than a generic phrase, since `ActionError` already explains itself.
    match action.plan(ev) {
        Err(e) => Some(format!(
            "{} cannot be planned for this resource: {e}",
            action.id().0
        )),
        // The executor's rule, not a restatement of it.
        Ok(plan) => crate::executor::structural_refusal(&plan),
    }
}

/// The full verdict for a resource whose [`PolicyDecision`] is known:
/// `None` when `resolved_action` is one nothing statically known refuses,
/// `Some(reason)` otherwise.
///
/// Sequenced so a [`PolicyClass::Protected`] resource returns before
/// [`Action::plan`] is reached. That ordering is the guarantee, not a test:
/// keep it first on its own merits.
pub fn static_refusal(
    ev: &Evidence,
    decision: &PolicyDecision,
    resolved_action: Option<&dyn Action>,
) -> Option<String> {
    if decision.class == PolicyClass::Protected {
        let reason = decision
            .reasons
            .first()
            .map(|r| r.as_str().to_string())
            .unwrap_or_else(|| "protected".to_string());
        return Some(format!("PROTECTED: {reason}"));
    }

    let Some(action) = resolved_action else {
        return Some("no registered cleanup action for this resource kind".to_string());
    };

    plan_refusal(action, ev)
}

/// The subset of `actions.ids_for_kind(ev.resource.kind)` that nothing
/// statically known refuses for `ev` — what may honestly be *offered* for
/// this resource, in registry order.
///
/// A [`PolicyClass::Protected`] resource yields an empty list without
/// planning anything. That is forced rather than chosen: the filter cannot
/// be evaluated without planning, and planning a protected resource is
/// exactly what must not happen. It is also the consistent answer, since
/// `reporting::dto`'s `executable_fields` already offers a protected
/// resource nothing.
pub fn eligible_action_ids(
    ev: &Evidence,
    decision: &PolicyDecision,
    actions: &ActionRegistry,
) -> Vec<&'static str> {
    if decision.class == PolicyClass::Protected {
        return Vec::new();
    }

    actions
        .ids_for_kind(ev.resource.kind)
        .into_iter()
        .filter(|id| match actions.get(id) {
            Some(action) => plan_refusal(action, ev).is_none(),
            // `ids_for_kind` returned it, so `get` cannot miss it. Treated
            // as not-offerable rather than asserted, so a future registry
            // divergence fails closed and quietly correct.
            None => false,
        })
        .collect()
}
