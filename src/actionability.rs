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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::SystemTime;

    use crate::actions::{ActionError, ActionId, ActionPlan};
    use crate::detectors::{discovery_evidence, probe_mtime, DetectorId};
    use crate::evidence::model::{
        EvidenceField, NativeCleanup, Recoverability, Regenerability, ResourceId, ResourceKind,
        ResourceLocator,
    };
    use crate::evidence::probe::ProbeOutcome;
    use crate::policy::{classify, PolicyConfig};

    use super::*;

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-actionability-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn evidence_at(path: &Path, kind: ResourceKind) -> Evidence {
        discovery_evidence(
            ResourceId::new(kind, ResourceLocator::Path(path.to_path_buf())),
            DetectorId("actionability_test_fixture"),
            path,
            ProbeOutcome::Observed(4096),
            ProbeOutcome::Observed(4096),
            false,
            probe_mtime(path),
            Regenerability::RegenerableByRebuild,
            Recoverability::RegenerableByRebuild,
            NativeCleanup::Unsupported,
        )
    }

    /// A real cargo project, so `cargo.clean.target_dir` can genuinely plan
    /// for it. Returns the target dir.
    fn cargo_project(prefix: &str) -> PathBuf {
        let root = make_temp_dir(prefix);
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        target_dir
    }

    /// Evidence whose *path* is credential material, so `classify` returns
    /// `Protected` — the same fixture shape as
    /// `tests/golden_llm_plan_protected_refusal.rs`, deliberately keeping the
    /// resource kind ordinary to show the path is what decides.
    fn protected_evidence() -> Evidence {
        evidence_at(
            Path::new("/Users/x/.ssh/id_ed25519"),
            ResourceKind::CargoTargetDir,
        )
    }

    fn decision_for(ev: &Evidence) -> PolicyDecision {
        classify(ev, &PolicyConfig::default(), ev.collected_at)
    }

    /// Counts its own `plan` calls, which is the only way to assert that a
    /// protected resource was *not* planned rather than merely that the
    /// answer came out empty.
    struct CountingAction {
        plan_calls: AtomicUsize,
    }

    impl CountingAction {
        fn new() -> Self {
            Self {
                plan_calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.plan_calls.load(Ordering::SeqCst)
        }
    }

    impl crate::actions::Action for CountingAction {
        fn id(&self) -> ActionId {
            ActionId("test.counting")
        }
        fn applies_to(&self) -> &'static [ResourceKind] {
            &[ResourceKind::CargoTargetDir]
        }
        fn required_evidence(&self) -> &'static [EvidenceField] {
            &[]
        }
        fn recoverability(&self) -> Recoverability {
            Recoverability::RegenerableByRebuild
        }
        fn plan(&self, _ev: &Evidence) -> Result<ActionPlan, ActionError> {
            self.plan_calls.fetch_add(1, Ordering::SeqCst);
            Err(ActionError::Unsupported("counted".to_string()))
        }
    }

    #[test]
    fn a_protected_resource_is_refused_without_being_planned() {
        // The ordering claim this module exists to hold in one place. Not
        // "the answer was a refusal" — anything returns a refusal for a
        // protected resource — but that `plan` was never reached, which is
        // what makes `tests/golden_llm_plan_protected_refusal.rs`'s
        // guarantee structural.
        let action = CountingAction::new();
        let ev = protected_evidence();
        let decision = decision_for(&ev);
        assert_eq!(decision.class, PolicyClass::Protected, "fixture invalid");

        let refusal = static_refusal(&ev, &decision, Some(&action))
            .expect("a protected resource must be refused");
        assert!(refusal.starts_with("PROTECTED: "), "got {refusal:?}");
        assert!(
            refusal.contains("protected_credential_material"),
            "the refusal must carry the real reason code, got {refusal:?}"
        );
        assert_eq!(
            action.calls(),
            0,
            "a protected resource's action must never be planned"
        );

        // Positive control: this same action IS planned for a resource that
        // policy does not protect, so the zero above is the ordering and not
        // an action that simply never gets called.
        let ordinary = evidence_at(
            &cargo_project("counting-control"),
            ResourceKind::CargoTargetDir,
        );
        let ordinary_decision = decision_for(&ordinary);
        assert_ne!(ordinary_decision.class, PolicyClass::Protected);
        let refusal = static_refusal(&ordinary, &ordinary_decision, Some(&action))
            .expect("the fake action always refuses");
        assert!(refusal.contains("counted"), "got {refusal:?}");
        assert_eq!(action.calls(), 1);
    }

    #[test]
    fn a_protected_resource_is_offered_no_action_id() {
        let actions = ActionRegistry::builtin();
        let ev = protected_evidence();
        assert!(eligible_action_ids(&ev, &decision_for(&ev), &actions).is_empty());

        // Positive control: the same kind, unprotected, IS offered its
        // action — so the emptiness above is the policy class and not a
        // registry that has nothing to give.
        let ordinary = evidence_at(
            &cargo_project("protected-control"),
            ResourceKind::CargoTargetDir,
        );
        assert_eq!(
            eligible_action_ids(&ordinary, &decision_for(&ordinary), &actions),
            vec!["cargo.clean.target_dir"]
        );
    }

    #[test]
    fn an_unscoped_step_is_refused_in_the_executors_own_words() {
        // `homebrew.cleanup.cache` plans fine — it is the *step* that has no
        // scoped path — so this exercises the `structural_refusal` arm rather
        // than the plan-error arm, and proves the wording is delegated
        // rather than paraphrased here.
        let actions = ActionRegistry::builtin();
        let action = actions
            .get("homebrew.cleanup.cache")
            .expect("registered built-in");
        let ev = evidence_at(&make_temp_dir("brew-cache"), ResourceKind::HomebrewCache);

        assert!(
            action.plan(&ev).is_ok(),
            "control invalid: this must be a refused STEP, not a failed plan"
        );
        let refusal = plan_refusal(action, &ev).expect("an unscoped step is always refused");
        assert_eq!(
            Some(refusal.as_str()),
            crate::executor::structural_refusal(&action.plan(&ev).unwrap()).as_deref(),
            "the reason must be the executor's verbatim, not a paraphrase"
        );
        assert!(refusal.contains("scoped_path"), "got {refusal:?}");
    }

    #[test]
    fn a_plan_that_cannot_be_built_is_reported_with_the_planners_own_reason() {
        let actions = ActionRegistry::builtin();
        let action = actions
            .get("cargo.clean.target_dir")
            .expect("registered built-in");
        // A target dir with no manifest beside it: nothing to hand `cargo
        // clean --manifest-path`.
        let orphan = make_temp_dir("orphan").join("target");
        fs::create_dir_all(&orphan).unwrap();
        let ev = evidence_at(&orphan, ResourceKind::CargoTargetDir);

        let refusal = plan_refusal(action, &ev).expect("no manifest, so no plan");
        assert!(
            refusal.starts_with("cargo.clean.target_dir cannot be planned for this resource: "),
            "the refusal must name the action that refused, got {refusal:?}"
        );
    }

    #[test]
    fn a_resource_with_no_registered_action_says_exactly_that() {
        let ev = evidence_at(
            &make_temp_dir("derived").as_path().to_path_buf(),
            ResourceKind::XcodeDerivedData,
        );
        let decision = decision_for(&ev);
        assert_ne!(decision.class, PolicyClass::Protected);

        assert_eq!(
            static_refusal(&ev, &decision, None).as_deref(),
            Some("no registered cleanup action for this resource kind")
        );
        // And the registry really does have nothing for this kind, so the
        // `None` above is the honest input rather than a contrived one.
        assert!(ActionRegistry::builtin()
            .ids_for_kind(ResourceKind::XcodeDerivedData)
            .is_empty());
    }
}
