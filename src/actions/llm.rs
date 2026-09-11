//! Optional BYOK LLM planner (HORO-954).
//!
//! **What this module is**: an advisory ranking suggestion over evidence
//! the crate already collected. [`LlmResourceView`] is an explicit,
//! bounded projection of one [`Evidence`] record, safe to serialize and
//! hand to a model. [`LlmPlan`]/[`LlmPlanItem`] are the model-facing
//! response shape.
//!
//! **Why only two `Deserialize` types exist here (and in the whole
//! crate)**: [`LlmPlan`]/[`LlmPlanItem`] are the sole external-input
//! parse targets. Everything they can produce is either a `String`
//! resolved against real, already-in-memory data, or a plain
//! `u32`/`Option<String>` used only for display/ranking — never a path,
//! never a shell fragment, never anything that reaches
//! [`crate::actions::ActionStep`] construction directly.
//! `#[serde(deny_unknown_fields)]` on [`LlmPlanItem`] additionally
//! ensures a model cannot smuggle an extra field (e.g. a `command`) past
//! the parser: any unknown field fails deserialization of the whole
//! struct, which fails the whole `Vec`, which fails the whole
//! [`LlmPlan`] — see `unexpected_field_rejects_whole_plan` below.

use serde::Deserialize;

use crate::evidence::model::{Completeness, Evidence, Regenerability};

use super::ActionRegistry;

/// Explicit, bounded projection of one [`Evidence`] record — deliberately
/// NOT `#[derive(Serialize)]` on `Evidence` itself. This is what keeps
/// "never ship raw filesystem inventory to an LLM" true even if
/// `Evidence` gains fields later: adding a field to `Evidence` has no
/// effect on what an LLM sees unless someone also, explicitly, adds it
/// here.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct LlmResourceView {
    pub resource_id: String,
    pub kind: &'static str,
    pub reclaimable_bytes: Option<u64>,
    pub age_days: Option<u64>,
    pub regenerability: &'static str,
    pub completeness: &'static str,
    pub offered_action_ids: Vec<&'static str>,
}

impl LlmResourceView {
    /// Explicit, field-by-field conversion — no `From`/blanket impl, so
    /// adding a field to `Evidence` never silently starts flowing to an
    /// LLM; a human has to come here and decide to add it.
    pub fn from_evidence(ev: &Evidence, actions: &ActionRegistry) -> Self {
        // `age_days` is derived from `last_modified` relative to when this
        // evidence was collected — never from an ambient `SystemTime::now()`
        // call, matching `crate::policy::engine::classify`'s "no ambient
        // clock" discipline and keeping this conversion deterministic and
        // testable.
        let age_days = ev.last_modified.observed().and_then(|last_modified| {
            ev.collected_at
                .duration_since(*last_modified)
                .ok()
                .map(|age| age.as_secs() / (24 * 60 * 60))
        });

        Self {
            resource_id: ev.resource.to_string(),
            kind: ev.resource.kind_tag(),
            reclaimable_bytes: ev.reclaimable_bytes.observed().copied(),
            age_days,
            regenerability: regenerability_tag(ev.regenerability),
            completeness: completeness_tag(&ev.completeness()),
            offered_action_ids: actions.ids_for_kind(ev.resource.kind),
        }
    }
}

fn regenerability_tag(r: Regenerability) -> &'static str {
    match r {
        Regenerability::RegenerableByTool => "regenerable_by_tool",
        Regenerability::RegenerableByRebuild => "regenerable_by_rebuild",
        Regenerability::NotRegenerable => "not_regenerable",
        Regenerability::Unknown => "unknown",
    }
}

fn completeness_tag(c: &Completeness) -> &'static str {
    match c {
        Completeness::Complete => "complete",
        Completeness::Partial { .. } => "partial",
        Completeness::Failed => "failed",
    }
}

/// The ONLY model-facing `Deserialize` type in the crate, together with
/// [`LlmPlanItem`]. See the module doc comment for why this is safe.
#[derive(Debug, Deserialize, PartialEq)]
pub struct LlmPlan {
    pub items: Vec<LlmPlanItem>,
}

/// One item of a model-proposed plan. `#[serde(deny_unknown_fields)]`
/// means any extra field (e.g. a smuggled `"command"`) fails
/// deserialization of the whole item — and therefore the whole
/// surrounding `Vec`/`LlmPlan` — rather than being silently ignored. See
/// `unexpected_field_rejects_whole_plan` in this module's tests.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LlmPlanItem {
    /// Resolved against the evidence set passed to `plan_with_llm` (a
    /// follow-up commit) — unknown -> the item is dropped, never a hard
    /// error for the whole plan.
    pub resource_id: String,
    /// Resolved via `ActionRegistry::get` — unknown -> the item is
    /// dropped, never a hard error for the whole plan.
    pub action_id: String,
    pub priority: Option<u32>,
    /// Human-readable explanation. Informational only — NEVER
    /// interpreted as an instruction, a path, or anything that reaches
    /// execution. It exists purely for a human to read in a UI/log.
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use crate::detectors::DetectorId;
    use crate::evidence::{
        NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint, ResourceId,
        ResourceKind, ResourceLocator,
    };

    use super::*;

    fn evidence_for(kind: ResourceKind, path: &str) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(1024),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(1024),
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn from_evidence_projects_expected_fields() {
        let ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        let actions = ActionRegistry::builtin();
        let view = LlmResourceView::from_evidence(&ev, &actions);

        assert_eq!(view.resource_id, "cargo_target_dir:/tmp/proj/target");
        assert_eq!(view.kind, "cargo_target_dir");
        assert_eq!(view.reclaimable_bytes, Some(1024));
        assert_eq!(view.age_days, Some(0));
        assert_eq!(view.regenerability, "regenerable_by_rebuild");
        assert_eq!(view.completeness, "complete");
        assert_eq!(view.offered_action_ids, vec!["cargo.clean.target_dir"]);
    }

    #[test]
    fn from_evidence_handles_missing_last_modified() {
        let mut ev = evidence_for(ResourceKind::CargoTargetDir, "/tmp/proj/target");
        ev.last_modified = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        let actions = ActionRegistry::builtin();
        let view = LlmResourceView::from_evidence(&ev, &actions);
        assert_eq!(view.age_days, None);
    }

    #[test]
    fn unexpected_field_rejects_whole_plan() {
        // deny_unknown_fields rejects the WHOLE containing struct on any
        // unknown field — not just the offending item. A model trying to
        // smuggle a "command" field fails the entire Vec<LlmPlanItem>
        // deserialization, hence the entire LlmPlan. Confirmed here
        // rather than assumed.
        let text = r#"{"items": [{"resource_id": "a", "action_id": "b", "priority": 1, "reason": null, "command": "rm -rf /"}]}"#;
        assert!(serde_json::from_str::<LlmPlan>(text).is_err());
    }
}
