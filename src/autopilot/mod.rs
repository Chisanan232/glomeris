//! Policy-constrained Autopilot (HORO-1310).
//!
//! Autopilot is the one feature in Glomeris where a model's output
//! influences what actually gets deleted. It exists because the manual
//! path — read `detect`, read `explain`, run `execute` per resource — is
//! the right default but the wrong thing to do fifteen times a week for
//! the same fifteen build directories.
//!
//! The canonical invariant is unchanged and unweakened here: **AI can
//! recommend. Policy decides. Executor verifies. Filesystem reality
//! wins.** What this module adds is a fourth clause that only matters
//! because a machine rather than a human is now doing the recommending:
//! **and the envelope bounds the outcome.**
//!
//! ## What the model can and cannot do
//!
//! Can: reorder a list of locally discovered candidates, and explain why
//! (display-only text, see
//! [`crate::actions::llm::ValidatedPlanItem::model_reason`]).
//!
//! Cannot, structurally rather than by prompt instruction:
//!
//! - **Name a resource.** A response's `resource_id` is resolved against
//!   an alias table built from this process's own discovery
//!   ([`crate::actions::llm::build_request_payload`]).
//! - **Name a path or a command.** There is no field for one. The two
//!   `Deserialize` types in the whole crate
//!   ([`crate::actions::llm::LlmPlan`]/`LlmPlanItem`) carry
//!   `#[serde(deny_unknown_fields)]`, so a smuggled `"command"` fails the
//!   whole plan rather than being ignored.
//! - **Invent an action.** `action_id` is resolved through
//!   [`crate::actions::ActionRegistry::get`] to a registered action's own
//!   id.
//! - **Influence policy.** Nothing a model returns reaches
//!   [`crate::policy::classify`] as an input, and no code path here can
//!   construct an [`crate::policy::Approval`] — `authorize` remains the
//!   only construction path in the crate (its `Seal` makes that a
//!   type-level fact, not a convention this module promises to respect).
//! - **Exceed the envelope.** Ordering cannot change which candidates are
//!   admissible, only which admissible one is tried first.

pub mod envelope;

pub use envelope::{AskPreauthorization, AutopilotEnvelope, EnvelopeError};
