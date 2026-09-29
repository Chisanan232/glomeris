//! A hostile provider reaches no probe, no path and no command (HORO-1549).
//!
//! HORO-1549 AC 9: *tests mutate provider output with command/path injection and
//! prove nothing executable is constructed*. The unit tests in
//! `src/planner/validate.rs` already show each refusal in isolation — an unknown
//! probe id, an alias never issued, an alias of the wrong kind. What they cannot
//! show is the thing that actually matters, which is that a refusal at
//! validation means **no probe ran**: a run that dropped a request but called
//! the probe runner anyway would pass every one of those tests while handing a
//! model-chosen subject to a real `git` invocation.
//!
//! So the runner here is a recorder. It answers nothing and records every
//! `(probe, subject)` it is asked for, and the assertions are about that list.
//!
//! # Why this is not vacuous
//!
//! A recorder that is never called passes every assertion in this file for the
//! wrong reason — the loop could be broken, `expand_and_plan` could be returning
//! early, `ProbeId::ALL` could be empty. So every hostile case is paired with
//! [`a_legitimate_request_does_reach_the_runner`], which uses the same recorder
//! against the same projection and asserts it *was* called. The hostile
//! assertions only mean something because that one passes.

use glomeris::actions::llm::{LlmError, LlmProvider};
use glomeris::actions::ActionRegistry;
use glomeris::detectors::DetectorId;
use glomeris::evidence::{
    Evidence, NativeCleanup, ProbeOutcome, ProbeReason, Recoverability, ResourceFingerprint,
    ResourceId, ResourceKind, ResourceLocator,
};
use glomeris::planner::{
    expand_and_plan, EvidenceBounds, GraphProjection, ProbeFindingView, ProbeId, ProbeResultView,
    ProbeRunner, ProbeSubject,
};
use glomeris::policy::{PolicyClass, PolicyDecision, ReasonCode};
use glomeris::workspace::{MachineContext, WorkspaceEvidenceGraph, WorkspaceSurvey};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const T0: SystemTime = SystemTime::UNIX_EPOCH;

/// Records what it was asked to run and answers nothing.
///
/// `Unavailable` rather than a real finding because this is not a stub of the
/// real runner: the point is the *call list*, and an answer here would only
/// invite assertions about a shape this file has no business pinning.
#[derive(Default)]
struct Recorder {
    asked: RefCell<Vec<(&'static str, String)>>,
}

impl ProbeRunner for Recorder {
    fn run(
        &self,
        _round: u32,
        probe: ProbeId,
        subject_ref: &str,
        _subject: &ProbeSubject,
        _now: SystemTime,
    ) -> ProbeResultView {
        self.asked
            .borrow_mut()
            .push((probe.tag(), subject_ref.to_string()));
        ProbeResultView {
            round: 2,
            probe_id: probe.tag(),
            subject_ref: subject_ref.to_string(),
            finding: ProbeFindingView::Unavailable {
                reason: "not_attempted",
            },
        }
    }
}

/// Answers with whatever it was given, every round.
struct Fixed(Result<String, LlmError>);

impl LlmProvider for Fixed {
    fn complete(&self, _system: &str, _user: &str) -> Result<String, LlmError> {
        self.0.clone()
    }
}

fn temp_cargo_project() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "glomeris-injection-{}-{nanos}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("target")).expect("create temp target dir");
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write manifest");
    root
}

fn candidate(target: &Path) -> (Evidence, PolicyDecision) {
    let resource = ResourceId::new(
        ResourceKind::CargoTargetDir,
        ResourceLocator::Path(target.to_path_buf()),
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
        collected_at: T0 + Duration::from_secs(86_400 * 7),
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

/// A projection with exactly one resource, so `resource_1` is the only alias a
/// well-formed request could name and every other spelling is an invention.
fn one_resource() -> (PathBuf, Vec<(Evidence, PolicyDecision)>, GraphProjection) {
    let root = temp_cargo_project();
    let candidates = vec![candidate(&root.join("target"))];
    let now = T0 + Duration::from_secs(86_400 * 7);
    let graph = WorkspaceEvidenceGraph::build(
        &candidates,
        &WorkspaceSurvey::unsurveyed(),
        MachineContext::unmeasured(),
        now,
    );
    let projection = GraphProjection::build(&graph, &candidates, &ActionRegistry::builtin(), now);
    (root, candidates, projection)
}

fn run(response: &str, recorder: &Recorder) -> glomeris::planner::ExpansionRun {
    let (root, _candidates, projection) = one_resource();
    let out = expand_and_plan(
        &Fixed(Ok(response.to_string())),
        &projection,
        &ActionRegistry::builtin(),
        recorder,
        EvidenceBounds::for_rounds(3),
        &glomeris::planner::MonotonicClock::started_now(),
        T0 + Duration::from_secs(86_400 * 7),
    );
    assert!(root.starts_with(std::env::temp_dir()));
    let _ = std::fs::remove_dir_all(&root);
    out
}

/// The control. Same recorder, same projection, a request the contract allows —
/// and the runner is called. Without this, every assertion below would also hold
/// of a loop that never ran a probe at all.
#[test]
fn a_legitimate_request_does_reach_the_runner() {
    let recorder = Recorder::default();
    let run = run(
        r#"{"contract_version":2,"items":[],
            "evidence_requests":[{"probe_id":"tool_liveness","subject_ref":"resource_1"}]}"#,
        &recorder,
    );

    let asked = recorder.asked.borrow();
    assert_eq!(
        *asked,
        vec![("tool_liveness", "resource_1".to_string())],
        "the control request did not reach the runner, so nothing below proves anything"
    );
    assert_eq!(run.probes_run(), 1);
    assert_eq!(run.stopped_because.tag(), "nothing_more_asked");
}

/// Every shape of executable a provider could try to smuggle into `probe_id`,
/// and none of them reaches the runner.
///
/// Each is refused for the same structural reason — `ProbeId::from_tag` is a
/// closed match over seven compiled-in words — but they are listed separately
/// because the failure they would represent is different in each case. A shell
/// string getting through would be command execution; a path getting through
/// would be a filesystem read the model chose.
#[test]
fn no_command_or_path_in_a_probe_id_reaches_the_runner() {
    const HOSTILE: &[&str] = &[
        "rm -rf /",
        "sh -c 'rm -rf /'",
        "; rm -rf /",
        "$(rm -rf /)",
        "`rm -rf /`",
        "git --exec-path=/tmp/evil",
        "/bin/sh",
        "/etc/passwd",
        "../../../../etc/passwd",
        "https://example.invalid/exfil",
        "file:///etc/passwd",
        // The closest misses: a real probe id with something appended, and the
        // same word in another case. A prefix match or a case-insensitive one
        // would let both through.
        "tool_liveness; rm -rf /",
        "tool_liveness /etc/passwd",
        "TOOL_LIVENESS",
        "",
    ];

    for hostile in HOSTILE {
        let escaped = hostile.replace('\\', "\\\\").replace('"', "\\\"");
        let recorder = Recorder::default();
        let out = run(
            &format!(
                r#"{{"contract_version":2,"items":[],
                     "evidence_requests":[{{"probe_id":"{escaped}",
                                            "subject_ref":"resource_1"}}]}}"#
            ),
            &recorder,
        );

        assert!(
            recorder.asked.borrow().is_empty(),
            "probe_id {hostile:?} reached the runner"
        );
        assert_eq!(
            out.probes_run(),
            0,
            "probe_id {hostile:?} produced a finding"
        );
        assert_eq!(
            out.plan.plan.counters.dropped_unknown_probe, 1,
            "probe_id {hostile:?} was dropped without being counted"
        );
        // Dropped, and the run is not a failure: a provider asking for a probe
        // this build does not have is a bad question, not a broken round.
        assert_eq!(out.plan.provider_error, None, "probe_id {hostile:?}");
    }
}

/// Every shape of path, URL and traversal a provider could try to put in
/// `subject_ref`, and none of them reaches the runner.
///
/// The probe id is a real one throughout, so this isolates the subject: the
/// question is whether a model can choose *what* a compiled-in probe reads.
#[test]
fn no_path_or_url_in_a_subject_ref_reaches_the_runner() {
    const HOSTILE: &[&str] = &[
        "/etc/passwd",
        "/",
        "~/.ssh/id_rsa",
        "../../../../../../etc/shadow",
        "/Users/someone/code",
        "C:\\Windows\\System32",
        "https://example.invalid/repo.git",
        "git@example.invalid:someone/repo.git",
        "file:///etc/passwd",
        "$HOME",
        "$(pwd)",
        "resource_1; rm -rf /",
        "resource_1 /etc/passwd",
        "resource_1/../resource_2",
        // The closest misses: an alias shape for a resource that does not
        // exist in this projection, and the alias with whitespace.
        "resource_2",
        "resource_99",
        " resource_1",
        "resource_1 ",
        "RESOURCE_1",
        "",
    ];

    for hostile in HOSTILE {
        let escaped = hostile.replace('\\', "\\\\").replace('"', "\\\"");
        let recorder = Recorder::default();
        let out = run(
            &format!(
                r#"{{"contract_version":2,"items":[],
                     "evidence_requests":[{{"probe_id":"process_activity",
                                            "subject_ref":"{escaped}"}}]}}"#
            ),
            &recorder,
        );

        assert!(
            recorder.asked.borrow().is_empty(),
            "subject_ref {hostile:?} reached the runner"
        );
        assert_eq!(
            out.probes_run(),
            0,
            "subject_ref {hostile:?} produced a finding"
        );
        assert_eq!(
            out.plan.plan.counters.dropped_unknown_probe_subject, 1,
            "subject_ref {hostile:?} was dropped without being counted"
        );
        assert_eq!(out.plan.provider_error, None, "subject_ref {hostile:?}");
    }
}

/// A field a provider invented is refused, and the whole response with it.
///
/// This is the one that has to fail loudly rather than quietly. A dropped
/// request is a question not answered; an *accepted* request carrying a
/// `command` field is a channel. `deny_unknown_fields` means the response never
/// parses, so there is no partial acceptance to reason about — and the run is
/// reported as a provider error rather than as an empty plan, because a provider
/// answering in a shape this build does not implement is not the same as a
/// provider having nothing to suggest.
#[test]
fn an_invented_field_beside_a_legitimate_request_refuses_the_whole_response() {
    const SMUGGLED: &[&str] = &[
        r#""command":"rm -rf /""#,
        r#""argv":["sh","-c","rm -rf /"]"#,
        r#""path":"/etc/passwd""#,
        r#""cwd":"/""#,
        r#""url":"https://example.invalid/exfil""#,
        r#""api_key":"redacted""#,
        r#""env":{"PATH":"/tmp/evil"}"#,
        r#""timeout_secs":86400"#,
    ];

    for smuggled in SMUGGLED {
        let recorder = Recorder::default();
        let out = run(
            &format!(
                r#"{{"contract_version":2,"items":[],
                     "evidence_requests":[{{"probe_id":"tool_liveness",
                                            "subject_ref":"resource_1",{smuggled}}}]}}"#
            ),
            &recorder,
        );

        assert!(
            recorder.asked.borrow().is_empty(),
            "a request carrying {smuggled} reached the runner"
        );
        assert!(
            matches!(out.plan.provider_error, Some(LlmError::InvalidResponse(_))),
            "a request carrying {smuggled} was not refused as a shape this build does not \
             implement: {:?}",
            out.plan.provider_error
        );
        assert!(out.plan.plan.evidence_requests.is_empty());
        assert_eq!(out.stopped_because.tag(), "provider_error");
        assert!(
            !out.stopped_because.converged(),
            "a refused response was reported as a converged run"
        );
    }
}

/// An item's `action_id` cannot become a command, and an item's `resource_id`
/// cannot become a path — the same injection battery on the half of the response
/// that, unlike an evidence request, does end up rendered to a human.
///
/// Also asserts the injected text is nowhere in the serialized plan. A dropped
/// item that still echoed `rm -rf /` into an explanation line would be a
/// refusal that taught the operator the attacker's command.
#[test]
fn no_command_in_an_item_survives_into_the_plan_or_its_rendering() {
    const HOSTILE_ACTION: &[&str] = &[
        "sh -c 'rm -rf /'",
        "cargo.clean.target_dir; rm -rf /",
        "/bin/rm",
        "docker.nuke.everything",
    ];

    for hostile in HOSTILE_ACTION {
        let recorder = Recorder::default();
        let out = run(
            &format!(
                r#"{{"contract_version":2,
                     "items":[{{"resource_id":"/etc/passwd","action_id":"{hostile}",
                                "disposition":"recommend_now","confidence":"observed",
                                "reason":"run {hostile}"}}]}}"#
            ),
            &recorder,
        );

        assert!(
            out.plan.plan.items.is_empty(),
            "an item naming {hostile:?} of /etc/passwd survived validation"
        );
        assert_eq!(out.plan.plan.counters.dropped_unknown_resource, 1);
        let rendered = format!("{:?}", out.plan.plan);
        assert!(
            !rendered.contains("rm -rf"),
            "the injected command survived into the plan: {rendered}"
        );
        assert!(
            !rendered.contains("/etc/passwd"),
            "the injected path survived into the plan: {rendered}"
        );
        assert!(recorder.asked.borrow().is_empty());
    }
}

/// A provider that cannot answer runs no probe, and the run says so.
///
/// HORO-1549 AC 8's deterministic no-LLM path, from the expansion's side: a run
/// whose provider is unreachable is byte-identical to one that was never made,
/// and it is reported as a provider error rather than as a converged run with
/// nothing to suggest. Two identical runs produce identical output, so nothing
/// in the loop depends on a clock or an ordering.
#[test]
fn a_provider_that_cannot_be_reached_runs_no_probe_and_is_not_convergence() {
    let (root, _candidates, projection) = one_resource();
    let mut outcomes = Vec::new();
    for _ in 0..2 {
        let recorder = Recorder::default();
        let out = expand_and_plan(
            &Fixed(Err(LlmError::NetworkError(
                "connection refused".to_string(),
            ))),
            &projection,
            &ActionRegistry::builtin(),
            &recorder,
            EvidenceBounds::for_rounds(3),
            &glomeris::planner::MonotonicClock::started_now(),
            T0 + Duration::from_secs(86_400 * 7),
        );
        assert!(
            recorder.asked.borrow().is_empty(),
            "a failed provider call still ran a probe"
        );
        assert_eq!(out.rounds_run, 1, "a failed round was retried");
        assert_eq!(out.stopped_because.tag(), "provider_error");
        assert!(!out.stopped_because.converged());
        assert!(out.plan.plan.items.is_empty());
        outcomes.push(format!("{out:?}"));
    }
    assert!(root.starts_with(std::env::temp_dir()));
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        outcomes[0], outcomes[1],
        "two identical runs differed, so something in the loop is not deterministic"
    );
}

/// Asking the same question forever does not buy rounds forever.
///
/// The provider here re-asks a legitimate request every round. After the first
/// answer it is a question already answered, so there is nothing left to run —
/// the probe count stays at one however many rounds are allowed, which is what
/// stops a model from spending a bounded run re-reading one thing.
#[test]
fn a_provider_repeating_one_question_runs_one_probe() {
    let recorder = Recorder::default();
    let out = run(
        r#"{"contract_version":2,"items":[],
            "evidence_requests":[{"probe_id":"tool_liveness","subject_ref":"resource_1"},
                                 {"probe_id":"tool_liveness","subject_ref":"resource_1"}]}"#,
        &recorder,
    );

    assert_eq!(
        recorder.asked.borrow().len(),
        1,
        "a repeated question was asked twice: {:?}",
        recorder.asked.borrow()
    );
    assert_eq!(out.probes_run(), 1);
    assert_eq!(
        out.plan.plan.counters.dropped_duplicate_evidence_request, 1,
        "the repeat within one response was not counted"
    );
    assert_eq!(
        out.rounds_run, 2,
        "the run did not stop once it had the answer"
    );
    assert_eq!(out.stopped_because.tag(), "nothing_more_asked");
}
