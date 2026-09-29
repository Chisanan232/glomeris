//! The bounded evidence-expansion loop (HORO-1549).
//!
//! Observe, hypothesize, identify what is missing, run a deterministic
//! read-only probe, re-evaluate. Campaign section 16's shape, and its
//! insistence that "verify" means new evidence rather than asking the model to
//! think harder — which is why the only thing that changes between rounds is
//! the contents of [`super::dto::PlannerRequestView::probe_results`]. The
//! prompt does not change, the projection does not change, and nothing tells
//! the model it got the last round wrong.
//!
//! # What stops a run
//!
//! [`StopReason`], and the distinction it draws is the point of reporting it.
//! [`StopReason::NothingMoreAsked`] means the plan is as good as the evidence
//! allows. Every other variant means it is not, and a reader deciding how much
//! to trust a disposition needs to know which. A loop that reported only "done"
//! would make a run that hit its ceiling indistinguishable from one that
//! converged.
//!
//! # A round that fails does not discard the round before it
//!
//! If round three's provider call fails, the run returns round two's plan and
//! says why it stopped. The alternative — returning the empty plan a failed
//! round produces — would mean a provider hiccup on a follow-up question
//! throwing away a complete, already-validated plan. See [`ExpansionRun::plan`].
//!
//! # Nothing here grants authority
//!
//! Probes read; they do not act. The plan this returns is still subject to
//! [`crate::policy::classify`] against freshly collected evidence before
//! anything executes, exactly as a single-round plan is. More evidence makes
//! the model better informed, never more powerful.

use std::collections::BTreeSet;

use super::bounds::{ElapsedClock, EvidenceBounds};
use super::dto::ProbeResultView;
use super::plan::{plan_workspace_with, WorkspacePlanResult};
use super::probe::ProbeRunner;
use super::project::GraphProjection;
use super::validate::ValidatedEvidenceRequest;
use crate::actions::llm::LlmProvider;
use crate::actions::ActionRegistry;

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The last round asked for no evidence it had not already been given.
    ///
    /// The converged case, and the only one where the plan is as good as the
    /// local evidence allows. Covers both "asked for nothing" and "asked only
    /// for things already answered", because from the plan's point of view
    /// those are the same fact: another round would send an identical request
    /// and get an identical answer.
    NothingMoreAsked,
    /// [`EvidenceBounds::max_rounds`] reached with questions outstanding.
    RoundLimit,
    /// [`EvidenceBounds::max_probes_total`] reached.
    ProbeLimit,
    /// [`EvidenceBounds::max_total_duration`] passed before the next round.
    TimeLimit,
    /// A round's provider call failed. The plan returned is the last good one.
    ProviderError,
}

impl StopReason {
    /// Stable snake_case tag, for the report and for tests.
    pub fn tag(self) -> &'static str {
        match self {
            Self::NothingMoreAsked => "nothing_more_asked",
            Self::RoundLimit => "round_limit",
            Self::ProbeLimit => "probe_limit",
            Self::TimeLimit => "time_limit",
            Self::ProviderError => "provider_error",
        }
    }

    /// Whether the run ended because it had finished rather than because it ran
    /// out of something.
    pub fn converged(self) -> bool {
        matches!(self, Self::NothingMoreAsked)
    }
}

/// What a bounded expansion run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpansionRun {
    /// The last round that produced a usable plan.
    ///
    /// Not necessarily the last round *attempted*: see the module doc on why a
    /// failed follow-up returns the previous plan rather than an empty one.
    pub plan: WorkspacePlanResult,
    /// How many provider calls were made, the first included.
    pub rounds_run: u32,
    /// Every finding, in the order acquired. The same vec the final round was
    /// given, so a caller reporting "what did this cost" and a caller auditing
    /// "what did the model see" read the same thing.
    pub probe_results: Vec<ProbeResultView>,
    pub stopped_because: StopReason,
}

impl ExpansionRun {
    /// How many probes ran, which is how many findings there are.
    pub fn probes_run(&self) -> usize {
        self.probe_results.len()
    }
}

/// Runs the loop.
///
/// With [`EvidenceBounds::SINGLE_ROUND`] this is exactly
/// [`super::plan::plan_workspace`] plus a report saying one round ran — the
/// deterministic path AC 8 requires stay available, and it stays available by
/// being the same code rather than a parallel one.
///
/// # Why the runner is a trait object and the bounds are a parameter
///
/// So the bound tests can run hundreds of probes without a `git` process, and
/// shrink `max_rounds` to 2 rather than waiting out the default. A bound whose
/// test is too slow to run is not a bound.
pub fn expand_and_plan(
    provider: &dyn LlmProvider,
    projection: &GraphProjection,
    actions: &ActionRegistry,
    runner: &dyn ProbeRunner,
    bounds: EvidenceBounds,
    clock: &dyn ElapsedClock,
    now: std::time::SystemTime,
) -> ExpansionRun {
    let mut findings: Vec<ProbeResultView> = Vec::new();
    // Every question already answered, so a provider repeating itself across
    // rounds costs nothing. Keyed on the whole question rather than on the
    // probe id: `process_activity` about two different worktrees is two
    // questions, and deduping on the probe alone would drop the second.
    let mut answered: BTreeSet<(&'static str, String)> = BTreeSet::new();

    let mut round: u32 = 0;
    let mut last_good: Option<WorkspacePlanResult> = None;
    // Set when a ceiling cut a question the provider did in fact ask. Sticky,
    // and it outranks `NothingMoreAsked` at the end: a later round happening
    // not to re-ask does not mean the question was answered, and a plan formed
    // without evidence that was asked for has not converged. Reporting it as
    // converged would be the one thing `StopReason` exists to prevent.
    let mut cut_by: Option<StopReason> = None;

    loop {
        round += 1;
        let result = plan_workspace_with(provider, projection, actions, findings.clone());

        if result.provider_error.is_some() {
            // The first round failing is the failure the caller expects to see:
            // an empty plan and the reason. A later round failing keeps the
            // plan that already validated.
            return match last_good {
                Some(plan) => ExpansionRun {
                    plan,
                    rounds_run: round,
                    probe_results: findings,
                    stopped_because: StopReason::ProviderError,
                },
                None => ExpansionRun {
                    plan: result,
                    rounds_run: round,
                    probe_results: findings,
                    stopped_because: StopReason::ProviderError,
                },
            };
        }

        let fresh: Vec<&ValidatedEvidenceRequest> = result
            .plan
            .evidence_requests
            .iter()
            .filter(|request| {
                !answered.contains(&(request.probe_id.tag(), request.subject_ref.clone()))
            })
            .collect();

        if fresh.is_empty() {
            return ExpansionRun {
                plan: result,
                rounds_run: round,
                probe_results: findings,
                stopped_because: cut_by.unwrap_or(StopReason::NothingMoreAsked),
            };
        }

        // Questions outstanding. Whether any of them get asked is now a matter
        // of what is left of the budget, and each ceiling is reported as
        // itself: a reader deciding how much to trust this plan is owed the
        // difference between "we stopped asking" and "it stopped asking".
        let stopped = if round >= bounds.rounds_allowed() {
            Some(StopReason::RoundLimit)
        } else if findings.len() as u32 >= bounds.max_probes_total {
            Some(StopReason::ProbeLimit)
        } else if clock.elapsed() >= bounds.max_total_duration {
            Some(StopReason::TimeLimit)
        } else {
            None
        };
        if let Some(stopped_because) = stopped {
            return ExpansionRun {
                plan: result,
                rounds_run: round,
                probe_results: findings,
                stopped_because,
            };
        }

        for request in fresh {
            // Re-checked per probe, not once per round: eight requests at the
            // per-probe timeout is eight times the timeout, and a ceiling
            // checked only between rounds would be overshot by all of it.
            if findings.len() as u32 >= bounds.max_probes_total {
                cut_by = Some(StopReason::ProbeLimit);
                break;
            }
            if clock.elapsed() >= bounds.max_total_duration {
                cut_by = Some(StopReason::TimeLimit);
                break;
            }
            // A reference the projection does not know is not probed. `validate`
            // already dropped those, and this is the second check: the subject a
            // probe runs against comes out of the table this machine built, and
            // never out of the string a response supplied.
            let Some(subject) = projection.subjects.resolve(&request.subject_ref) else {
                continue;
            };
            answered.insert((request.probe_id.tag(), request.subject_ref.clone()));
            findings.push(runner.run(
                round + 1,
                request.probe_id,
                &request.subject_ref,
                subject,
                now,
            ));
        }

        // Nothing could be run — every fresh request named a reference that no
        // longer resolves. Returning here rather than looping keeps the run
        // from sending an identical request and getting an identical answer
        // until the round ceiling.
        if findings.is_empty() {
            return ExpansionRun {
                plan: result,
                rounds_run: round,
                probe_results: findings,
                stopped_because: cut_by.unwrap_or(StopReason::NothingMoreAsked),
            };
        }

        last_good = Some(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::{
        Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint,
        ResourceId, ResourceKind, ResourceLocator,
    };
    use crate::planner::contract::ProbeId;
    use crate::planner::dto::ProbeFindingView;
    use crate::planner::probe::ProbeRunner;
    use crate::planner::ProbeSubject;
    use crate::policy::{PolicyClass, PolicyDecision, ReasonCode};
    use crate::workspace::{MachineContext, WorkspaceEvidenceGraph, WorkspaceSurvey};
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    const T0: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    /// Replies with a scripted answer per round, and records every request.
    struct Scripted {
        replies: Vec<String>,
        seen: RefCell<Vec<String>>,
    }

    impl Scripted {
        /// The last reply repeats for every round past the script.
        fn of(replies: &[&str]) -> Self {
            Self {
                replies: replies.iter().map(|r| r.to_string()).collect(),
                seen: RefCell::new(Vec::new()),
            }
        }

        fn rounds(&self) -> usize {
            self.seen.borrow().len()
        }
    }

    impl LlmProvider for Scripted {
        fn complete(&self, _: &str, user_prompt: &str) -> Result<String, LlmError> {
            let round = self.seen.borrow().len();
            self.seen.borrow_mut().push(user_prompt.to_string());
            let index = round.min(self.replies.len().saturating_sub(1));
            match self.replies.get(index) {
                Some(reply) => Ok(reply.clone()),
                None => Err(LlmError::NetworkError("out of script".to_string())),
            }
        }
    }

    /// Fails on the nth call (1-based), replying normally before that.
    struct FailsOnRound {
        reply: String,
        fail_on: usize,
        calls: RefCell<usize>,
    }

    impl LlmProvider for FailsOnRound {
        fn complete(&self, _: &str, _: &str) -> Result<String, LlmError> {
            *self.calls.borrow_mut() += 1;
            if *self.calls.borrow() == self.fail_on {
                return Err(LlmError::NetworkError("connection refused".to_string()));
            }
            Ok(self.reply.clone())
        }
    }

    /// Answers every probe the same way, and counts.
    struct Counting {
        runs: RefCell<Vec<(u32, &'static str, String)>>,
    }

    impl Counting {
        fn new() -> Self {
            Self {
                runs: RefCell::new(Vec::new()),
            }
        }

        fn count(&self) -> usize {
            self.runs.borrow().len()
        }
    }

    impl ProbeRunner for Counting {
        fn run(
            &self,
            round: u32,
            probe: ProbeId,
            subject_ref: &str,
            _: &ProbeSubject,
            _: SystemTime,
        ) -> ProbeResultView {
            self.runs
                .borrow_mut()
                .push((round, probe.tag(), subject_ref.to_string()));
            ProbeResultView {
                round,
                probe_id: probe.tag(),
                subject_ref: subject_ref.to_string(),
                finding: ProbeFindingView::Unavailable {
                    reason: ProbeReason::ToolAbsent.tag(),
                },
            }
        }
    }

    /// A clock a test sets by hand.
    struct Frozen {
        elapsed: RefCell<Duration>,
    }

    impl Frozen {
        fn at(secs: u64) -> Self {
            Self {
                elapsed: RefCell::new(Duration::from_secs(secs)),
            }
        }
    }

    impl ElapsedClock for Frozen {
        fn elapsed(&self) -> Duration {
            *self.elapsed.borrow()
        }
    }

    use crate::actions::llm::LlmError;

    fn temp_cargo_project() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "glomeris-expand-{}-{nanos}-{n}",
            std::process::id()
        ));
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).expect("create temp target dir");
        fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write manifest");
        target_dir
    }

    fn candidate(path: &Path) -> (Evidence, PolicyDecision) {
        let resource = ResourceId::new(
            ResourceKind::CargoTargetDir,
            ResourceLocator::Path(path.to_path_buf()),
        );
        let evidence = Evidence {
            resource: resource.clone(),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(4096),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(4096),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(T0),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::CargoTargetDir.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Unsupported,
            open_by_process: ProbeOutcome::Observed(Vec::new()),
            process_cwd_match: ProbeOutcome::Observed(Vec::new()),
            git_state: ProbeOutcome::Observed(None),
            tool_liveness: ProbeOutcome::Observed(false),
            docker_lifecycle: None,
            collected_at: at(86_400 * 7),
            sources: Vec::new(),
        };
        let decision = PolicyDecision {
            resource,
            class: PolicyClass::AutoSafe,
            reasons: vec![ReasonCode::NoActiveUseObserved],
            evidence_collected_at: T0,
            evaluated_at: T0,
            policy_version: 1,
        };
        (evidence, decision)
    }

    fn projection() -> GraphProjection {
        let target = temp_cargo_project();
        let candidates = vec![candidate(&target)];
        let graph = WorkspaceEvidenceGraph::build(
            &candidates,
            &WorkspaceSurvey::unsurveyed(),
            MachineContext::unmeasured(),
            at(86_400 * 7),
        );
        let projection = GraphProjection::build(
            &graph,
            &candidates,
            &ActionRegistry::builtin(),
            at(86_400 * 7),
        );
        let root = target.parent().expect("a target has a project root");
        assert!(root.starts_with(std::env::temp_dir()));
        let _ = fs::remove_dir_all(root);
        projection
    }

    /// One item, no questions.
    const SETTLED: &str = r#"{"contract_version":2,
        "items":[{"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"ask_user","confidence":"inferred"}]}"#;

    /// One item, and one answerable question about the resource it named.
    const ASKS_ONCE: &str = r#"{"contract_version":2,
        "items":[{"resource_id":"resource_1","action_id":"cargo.clean.target_dir",
                  "disposition":"defer","confidence":"unknown"}],
        "evidence_requests":[{"probe_id":"process_activity","subject_ref":"resource_1"}]}"#;

    fn run(
        provider: &dyn LlmProvider,
        runner: &dyn ProbeRunner,
        bounds: EvidenceBounds,
        clock: &dyn ElapsedClock,
    ) -> ExpansionRun {
        expand_and_plan(
            provider,
            &projection(),
            &ActionRegistry::builtin(),
            runner,
            bounds,
            clock,
            at(86_400 * 7),
        )
    }

    /// AC 8. The default bounds are one round and zero probes, and that path is
    /// this function rather than a parallel one — so "the deterministic no-LLM
    /// path still works" cannot rot while this module changes.
    #[test]
    fn the_single_round_default_runs_one_call_and_no_probes() {
        let provider = Scripted::of(&[ASKS_ONCE]);
        let runner = Counting::new();
        let result = run(
            &provider,
            &runner,
            EvidenceBounds::SINGLE_ROUND,
            &Frozen::at(0),
        );

        assert_eq!(provider.rounds(), 1, "the default sent more than one round");
        assert_eq!(runner.count(), 0, "the default ran a probe");
        assert_eq!(result.rounds_run, 1);
        assert_eq!(result.stopped_because, StopReason::RoundLimit);
        assert_eq!(result.plan.plan.items.len(), 1);
        assert!(result.probe_results.is_empty());
    }

    /// A round that asks nothing converges immediately, which is the normal
    /// case and must not read as having hit a ceiling.
    #[test]
    fn a_round_that_asks_nothing_converges() {
        let provider = Scripted::of(&[SETTLED]);
        let runner = Counting::new();
        let result = run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        assert_eq!(provider.rounds(), 1);
        assert_eq!(runner.count(), 0);
        assert_eq!(result.stopped_because, StopReason::NothingMoreAsked);
        assert!(result.stopped_because.converged());
    }

    /// The whole loop: ask, probe, re-plan with the finding, settle.
    #[test]
    fn a_question_is_answered_and_the_next_round_sees_the_answer() {
        let provider = Scripted::of(&[ASKS_ONCE, SETTLED]);
        let runner = Counting::new();
        let result = run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        assert_eq!(result.rounds_run, 2);
        assert_eq!(result.probes_run(), 1);
        assert_eq!(result.stopped_because, StopReason::NothingMoreAsked);

        // The probe that ran is the one that was asked for, about the subject
        // that was named.
        assert_eq!(
            *runner.runs.borrow(),
            vec![(2, ProbeId::ProcessActivity.tag(), "resource_1".to_string())]
        );

        // And the second request carried the finding, where the first did not.
        let seen = provider.seen.borrow();
        assert!(seen[0].contains("\"probe_results\":[]"));
        assert!(seen[1].contains("\"probe_id\":\"process_activity\""));
        assert!(seen[1].contains("\"round\":2"));
    }

    /// A provider that asks the same question every round is answered once.
    /// Without this it would loop to the round ceiling re-running one probe.
    #[test]
    fn the_same_question_asked_twice_is_probed_once_and_ends_the_run() {
        let provider = Scripted::of(&[ASKS_ONCE]);
        let runner = Counting::new();
        let result = run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        assert_eq!(runner.count(), 1, "a repeated question ran a second probe");
        assert_eq!(result.rounds_run, 2);
        assert_eq!(
            result.stopped_because,
            StopReason::NothingMoreAsked,
            "a repeated question read as an outstanding one"
        );
    }

    /// AC 5, the round ceiling. A provider that never settles is cut off, and
    /// the report says so rather than claiming convergence.
    #[test]
    fn a_provider_that_never_settles_stops_at_the_round_ceiling() {
        // Each round asks about a different subject, so the dedupe never fires
        // and only the ceiling can stop this.
        let provider = Scripted::of(&[
            ASKS_ONCE,
            r#"{"contract_version":2,"items":[],
                "evidence_requests":[{"probe_id":"git_branch_state","subject_ref":"workspace_1"},
                                     {"probe_id":"tool_liveness","subject_ref":"resource_1"}]}"#,
        ]);
        let runner = Counting::new();
        let result = run(
            &provider,
            &runner,
            EvidenceBounds {
                max_rounds: 2,
                ..EvidenceBounds::DEFAULT
            },
            &Frozen::at(0),
        );

        assert_eq!(provider.rounds(), 2, "the round ceiling did not bind");
        assert_eq!(result.rounds_run, 2);
        assert_eq!(result.stopped_because, StopReason::RoundLimit);
        assert!(!result.stopped_because.converged());
    }

    /// A response asking for four things where only two are probeable: this
    /// fixture has one resource and no surveyed worktree, so `validate` drops
    /// both `workspace_1` requests before the loop sees them.
    const ASKS_FOUR: &str = r#"{"contract_version":2,"items":[],
        "evidence_requests":[{"probe_id":"git_branch_state","subject_ref":"workspace_1"},
                             {"probe_id":"process_activity","subject_ref":"workspace_1"},
                             {"probe_id":"tool_liveness","subject_ref":"resource_1"},
                             {"probe_id":"process_activity","subject_ref":"resource_1"}]}"#;

    /// AC 5, the probe ceiling — the one that binds inside a round, where a
    /// response asking for several probes would otherwise run all of them.
    ///
    /// The ceiling is 1 against a control of 2 because only two of
    /// [`ASKS_FOUR`]'s requests survive validation. A ceiling of 2 would have
    /// passed without ever binding, which is how the first version of this test
    /// was vacuous — the control is what caught it.
    #[test]
    fn the_probe_ceiling_binds_inside_a_single_round() {
        let provider = Scripted::of(&[ASKS_FOUR, SETTLED]);
        let runner = Counting::new();
        let result = run(
            &provider,
            &runner,
            EvidenceBounds {
                max_probes_total: 1,
                ..EvidenceBounds::DEFAULT
            },
            &Frozen::at(0),
        );

        assert_eq!(runner.count(), 1, "the probe ceiling did not bind");
        assert_eq!(result.probes_run(), 1);
        assert_eq!(result.stopped_because, StopReason::ProbeLimit);

        // Positive control: the same response with room runs both probeable
        // requests, so the assertion above is about the ceiling and not about
        // how many requests survived validation.
        let roomy = Scripted::of(&[ASKS_FOUR, SETTLED]);
        let unbounded = Counting::new();
        run(&roomy, &unbounded, EvidenceBounds::DEFAULT, &Frozen::at(0));
        assert_eq!(unbounded.count(), 2);
    }

    /// A question a ceiling cut stays cut, even though the round after it
    /// happened to ask nothing.
    ///
    /// Round 1 asks two probeable things and the ceiling allows one. Round 2
    /// settles. Without the sticky reason this would report convergence, and a
    /// reader would take a plan formed without evidence that was asked for as
    /// the best the local facts allow.
    #[test]
    fn a_question_a_ceiling_cut_is_not_reported_as_convergence() {
        let provider = Scripted::of(&[ASKS_FOUR, SETTLED]);
        let runner = Counting::new();
        let result = run(
            &provider,
            &runner,
            EvidenceBounds {
                max_probes_total: 1,
                ..EvidenceBounds::DEFAULT
            },
            &Frozen::at(0),
        );

        assert_eq!(
            provider.rounds(),
            2,
            "the run did not reach a settled round"
        );
        assert!(
            result.plan.plan.evidence_requests.is_empty(),
            "the final round still had questions, so this proves nothing"
        );
        assert!(
            !result.stopped_because.converged(),
            "a cut question was reported as convergence"
        );
        assert_eq!(result.stopped_because, StopReason::ProbeLimit);
    }

    /// AC 5, the time ceiling. Tested with a clock a test sets, so it runs in
    /// no time and therefore actually runs.
    #[test]
    fn a_run_past_its_time_ceiling_starts_no_more_probes() {
        let provider = Scripted::of(&[ASKS_ONCE, SETTLED]);
        let runner = Counting::new();
        let result = run(
            &provider,
            &runner,
            EvidenceBounds {
                max_total_duration: Duration::from_secs(10),
                ..EvidenceBounds::DEFAULT
            },
            &Frozen::at(11),
        );

        assert_eq!(runner.count(), 0, "a probe started past the time ceiling");
        assert_eq!(result.stopped_because, StopReason::TimeLimit);
        assert_eq!(result.rounds_run, 1);

        // Positive control: the same run inside the ceiling completes.
        let inside = Scripted::of(&[ASKS_ONCE, SETTLED]);
        let ran = Counting::new();
        let ok = run(
            &inside,
            &ran,
            EvidenceBounds {
                max_total_duration: Duration::from_secs(10),
                ..EvidenceBounds::DEFAULT
            },
            &Frozen::at(1),
        );
        assert_eq!(ran.count(), 1);
        assert_eq!(ok.stopped_because, StopReason::NothingMoreAsked);
    }

    /// A first round that fails is the failure the caller expects: empty plan,
    /// the reason, nothing probed.
    #[test]
    fn a_first_round_that_fails_reports_the_failure() {
        let provider = FailsOnRound {
            reply: SETTLED.to_string(),
            fail_on: 1,
            calls: RefCell::new(0),
        };
        let runner = Counting::new();
        let result = run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        assert_eq!(result.stopped_because, StopReason::ProviderError);
        assert!(result.plan.plan.items.is_empty());
        assert!(result.plan.provider_error.is_some());
        assert_eq!(runner.count(), 0);
    }

    /// The load-bearing one. A follow-up round failing must not throw away the
    /// plan the first round already produced and validated.
    #[test]
    fn a_later_round_that_fails_keeps_the_plan_the_earlier_one_produced() {
        let provider = FailsOnRound {
            reply: ASKS_ONCE.to_string(),
            fail_on: 2,
            calls: RefCell::new(0),
        };
        let runner = Counting::new();
        let result = run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        assert_eq!(result.stopped_because, StopReason::ProviderError);
        assert_eq!(result.rounds_run, 2);
        assert_eq!(
            result.plan.plan.items.len(),
            1,
            "a failed follow-up discarded the plan that had already validated"
        );
        assert!(
            result.plan.provider_error.is_none(),
            "the surviving plan was marked as the failed round's"
        );
        assert_eq!(result.probes_run(), 1, "the finding acquired was dropped");
    }

    /// A request naming an alias the projection does not know runs nothing.
    /// `validate` drops these first; this is the second check, and it also
    /// proves the run does not spin until the round ceiling when every request
    /// is unusable.
    #[test]
    fn a_request_naming_an_unknown_alias_probes_nothing_and_stops() {
        // `validate` drops an unknown subject_ref, so this arrives as a round
        // with no requests at all — which converges. Asserted here so the
        // behaviour is pinned at this layer too.
        let provider = Scripted::of(&[r#"{"contract_version":2,"items":[],
            "evidence_requests":[{"probe_id":"process_activity",
                                  "subject_ref":"/Users/someone/code"}]}"#]);
        let runner = Counting::new();
        let result = run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        assert_eq!(runner.count(), 0, "an unknown alias reached a probe");
        assert_eq!(provider.rounds(), 1, "the run spun on unusable requests");
        assert_eq!(result.stopped_because, StopReason::NothingMoreAsked);
    }

    /// Nothing the loop sends ever contains a local path, across every round.
    #[test]
    fn no_round_sends_a_local_path() {
        let provider = Scripted::of(&[ASKS_ONCE, SETTLED]);
        let runner = Counting::new();
        run(&provider, &runner, EvidenceBounds::DEFAULT, &Frozen::at(0));

        for (index, sent) in provider.seen.borrow().iter().enumerate() {
            assert!(
                !sent.contains('/'),
                "round {} sent a path separator: {sent}",
                index + 1
            );
            assert!(!sent.contains("glomeris-expand-"));
        }
    }
}
