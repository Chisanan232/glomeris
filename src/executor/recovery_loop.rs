//! Bounded closed-loop disk recovery (HORO-952): the orchestration behind
//! `glomeris free --target <threshold>`.
//!
//! This module wires together, without reimplementing any of them:
//! [`crate::monitor::FsStat`] (measurement), [`crate::detectors::DetectorRegistry`]
//! (discovery), [`crate::evidence::correlate::EvidenceCollector`]
//! (correlation refresh), [`crate::policy::classify`]/`crate::policy::approval::authorize`
//! (the AUTO_SAFE/ASK/PROTECTED decision), and [`crate::executor::execute`]
//! (HORO-951's fully-hardened, TOCTOU-safe deletion). The loop itself adds
//! only orchestration and bookkeeping — target/budget checks, one-candidate-
//! per-iteration selection, and a no-progress guard.
//!
//! Placement note: this lives under `crate::executor` (rather than a new
//! top-level `crate::recovery` module) because it is a thin caller of
//! `executor::execute` plus the other already-hardened modules — it does
//! not introduce a new safety-critical primitive of its own, so it does
//! not need its own top-level module boundary.
//!
//! Canonical safety invariant, unchanged: AI can recommend. Policy
//! decides. Executor verifies. Filesystem reality wins. This loop never
//! bypasses `policy::classify`/`policy::authorize`, never constructs an
//! `Approval` itself, and never touches the filesystem directly — every
//! mutation happens inside a real `executor::execute` call.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::actions::ActionRegistry;
use crate::autopilot::{admit, admits_pressure, AutopilotEnvelope, BudgetLedger, RefusalReason};
use crate::detectors::{DetectorRegistry, DetectorStatus, DiscoveryContext};
use crate::evidence::correlate::{merge_into, EvidenceCollector, ProbeBudget};
use crate::evidence::model::{ActionId, Evidence, NativeCleanup, ResourceId};
use crate::evidence::probe::ProbeOutcome;
use crate::executor::{execute, ExecutionOutcome, ExecutionReport};
use crate::monitor::{
    append_audit_record, ActionSource, AuditRecord, Clock, FsStat, FsUsage, PressureState,
};
use crate::policy::approval::authorize;
use crate::policy::{classify, PolicyClass, PolicyConfig, PolicyDecision, UserConsent};
use crate::reporting::policy_label::label_for;

/// Time budget applied to each candidate's correlation refresh pass
/// (step 5 of the loop). Mirrors `executor::REVALIDATION_TIMEOUT`'s
/// reasoning: this runs synchronously inside a bounded recovery loop, so
/// it must not stall indefinitely. Not reused directly because that
/// constant is private to `executor`.
const CANDIDATE_CORRELATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Consecutive "executed successfully but freed nothing measurable"
/// iterations before the loop gives up rather than spinning forever (see
/// [`StopReason::NoProgress`]).
const NO_PROGRESS_STREAK_THRESHOLD: u32 = 2;

/// Abstraction over "what time is it right now" for the wall-clock
/// timestamps [`crate::policy::classify`]/`crate::policy::approval::authorize`
/// need (`Evidence::collected_at`, `UserConsent::granted_at`). Distinct
/// from [`crate::monitor::Clock`] (which is `Instant`-based and used here
/// only for the `max_duration` budget check) because `classify` takes a
/// `SystemTime`, and no existing trait in this codebase provides an
/// injectable `SystemTime` source.
pub trait WallClock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// Real wall-clock time. Used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// One thing that happened inside a run, as it happened (HORO-1509).
///
/// A closed loop that reports only its final [`RecoveryReport`] is a closed
/// loop nobody can watch. These are the events a UI needs to say which
/// iteration is running, what it is doing right now, and how many bytes have
/// actually been reclaimed — and every byte figure here is a *measured* one,
/// because the alternative (summing what candidates claimed they would free)
/// is the estimate-as-progress mistake the campaign's section 9 forbids.
///
/// Deliberately a domain type with no `serde` derive. The wire shape belongs
/// to the reporting layer, same as every other type this loop produces: see
/// `crate::reporting::dto::RecoveryProgressEvent`, which projects these. That
/// keeps the direction of dependency the one this codebase already has
/// (reporting reads the executor's types, never the reverse) and keeps a
/// rename of a JSON field from being a change to the executor.
///
/// `iteration` is always the 1-based ordinal of the iteration currently being
/// *attempted*, which is `RecoveryReport::iterations_run + 1` at the moment of
/// emission — an iteration is only counted once a candidate has been selected
/// for it, and progress has to be reportable before that.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryProgress {
    /// Step 1: the filesystem was measured. The only statement of fact about
    /// free space in this stream.
    Measured {
        iteration: u32,
        usage: FsUsage,
        bytes_freed_so_far: u64,
    },
    /// Step 4 is starting: detectors are being asked what exists right now.
    /// This is the "rescanning" state the ticket asks to be visible, and it is
    /// re-entered on every iteration by design — the loop never reuses an
    /// earlier pass's candidate list.
    Discovering { iteration: u32 },
    /// Step 4 finished. `candidates` counts what has a resolvable action, not
    /// what a detector saw, and `detectors_failed` being non-zero is what
    /// withdraws a later [`StopReason::SafeExhausted`]'s usual meaning.
    Discovered {
        iteration: u32,
        candidates: u32,
        detectors_failed: u32,
    },
    /// Steps 5-6: every candidate's evidence is being re-collected and
    /// reclassified before anything is chosen.
    Revalidating { iteration: u32 },
    /// Step 8: a real mutation is about to run. `estimated_bytes` is exactly
    /// that — an estimate, named so it cannot be mistaken for progress.
    ActionStarted {
        iteration: u32,
        resource: String,
        action: String,
        policy_label: &'static str,
        estimated_bytes: Option<u64>,
    },
    /// Step 9: the mutation finished. `reclaimed_bytes` is what the executor
    /// measured, and `None` means it could not be measured — never zero
    /// standing in for unknown.
    ActionFinished {
        iteration: u32,
        resource: String,
        action: String,
        outcome: &'static str,
        reclaimed_bytes: Option<u64>,
        bytes_freed_so_far: u64,
    },
    /// A cooperative stop was observed, between actions and never during one.
    StopRequested { iteration: u32 },
}

/// Where [`RecoveryProgress`] events go.
///
/// A trait rather than a closure for the same reason [`WallClock`] is one:
/// production passes a writer, tests pass a recorder, and both are named
/// types a signature can talk about.
pub trait RecoveryObserver {
    fn observe(&self, event: RecoveryProgress);
}

/// Discards every event. The default, and what every caller that does not ask
/// for `--progress-json` gets.
#[derive(Debug, Default, Clone, Copy)]
pub struct SilentObserver;

impl RecoveryObserver for SilentObserver {
    fn observe(&self, _event: RecoveryProgress) {}
}

/// Whether the user has asked the loop to stop after the action it is
/// currently running (HORO-1509).
///
/// Cooperative by construction: the loop asks, between iterations and after
/// each completed action, and there is no way for an implementation of this
/// trait to interrupt a mutation that is already in flight. That asymmetry is
/// the safety property — a `SIGTERM` partway through a deletion leaves the
/// filesystem in a state neither the loop nor the audit log could describe,
/// which is why the GUI has no handle on the child process and asks for a stop
/// through here instead.
pub trait StopSignal {
    fn stop_requested(&self) -> bool;
}

/// Never asks the loop to stop. The default, for every non-interactive run.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverStops;

impl StopSignal for NeverStops {
    fn stop_requested(&self) -> bool {
        false
    }
}

/// A [`StopSignal`] backed by the existence of a file the caller creates when
/// the user presses "Stop after current action".
///
/// A file rather than a signal because the GUI deliberately keeps no handle on
/// the recovery child (see [`StopSignal`]), and a sentinel needs no handle,
/// no signal-safe code and no IPC: `open(2)` from one process, `stat(2)` from
/// the other. The loop only ever *reads* it, so this does not weaken the
/// module's rule that every mutation happens inside [`crate::executor::execute`].
///
/// [`StopFile::watching`] refuses a path that already exists, which matters:
/// a stale sentinel left behind by an earlier run would stop the next one
/// before it did anything, and the report would truthfully say the user
/// stopped it while the user had done nothing at all.
#[derive(Debug)]
pub struct StopFile {
    path: std::path::PathBuf,
}

impl StopFile {
    /// Watches `path`, which must not exist yet.
    pub fn watching(path: &Path) -> std::io::Result<Self> {
        if path.try_exists()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("stop file {} already exists", path.display()),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// How an existence probe is read. An error counts as a stop request, and
    /// that direction is the point: a loop that cannot find out whether the
    /// user asked it to stop must not keep deleting things on the assumption
    /// they did not.
    fn requested_from(probe: std::io::Result<bool>) -> bool {
        probe.unwrap_or(true)
    }
}

impl StopSignal for StopFile {
    fn stop_requested(&self) -> bool {
        Self::requested_from(self.path.try_exists())
    }
}

/// The disk-free goal a recovery run is trying to reach.
///
/// Both variants describe a target *state* of the filesystem (how much
/// free space should exist when the loop stops), not an amount to
/// reclaim — this is what makes "target already met, stop immediately"
/// (loop step 2) a coherent check before any action has run for either
/// variant. `AbsoluteBytes` byte multipliers used by the CLI parser (see
/// [`parse_free_target`]) are binary (1024-based: `5GB` == `5 * 1024^3`
/// bytes), matching the ticket's `5GB` / `5368709120` example pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FreeTarget {
    /// Target: at least this percentage (0.0..=100.0) of total capacity
    /// free.
    Percentage(f64),
    /// Target: at least this many bytes free.
    AbsoluteBytes(u64),
}

/// Bounds and behavior tunables for one recovery run.
#[derive(Debug, Clone)]
pub struct RecoveryConfig {
    pub target: FreeTarget,
    pub max_iterations: u32,
    pub max_actions: u32,
    pub max_duration: Duration,
    /// If `false`, `Ask`-classified candidates are reported as declined/
    /// skipped rather than executed — there is no interactive prompt in
    /// this MVP (see module docs and the PR's "Known limitations": real
    /// interactive approval UX is HORO-955's job). If `true`, an `Ask`
    /// candidate is auto-approved by constructing a `UserConsent` from the
    /// exact evidence/fingerprint just observed, purely as an MVP
    /// simplification for non-interactive runs.
    pub auto_approve_ask: bool,
}

/// Turns a recovery run into an *automatic* one, by narrowing it with an
/// Autopilot envelope (HORO-1510).
///
/// Absent — `RecoveryRunRequest::admission` left `None` — a run is exactly what
/// it was before: a human asked for it, so the only limits are
/// [`RecoveryConfig`]'s. Present, every candidate additionally has to pass
/// [`crate::autopilot::admit`] before it can be selected, and the run stops
/// when one of the envelope's run-wide budgets or preconditions says so.
///
/// This is a second gate, never a second policy. The envelope can only ever
/// *remove* candidates the existing pipeline had already permitted
/// (`classify -> admit -> authorize -> execute`), which is why there is no
/// Autopilot-shaped variant of this loop: an automatic run is the same loop with
/// a narrower field of view. Nothing in here can make a `Protected` resource
/// executable, and nothing in here is consulted about whether the *filesystem*
/// goal was reached.
///
/// One consequence worth stating, because it is the one place an automatic run
/// may act where a plain one would not: an `Ask` candidate whose risks the
/// envelope pre-authorizes is eligible for selection even though
/// [`RecoveryConfig::auto_approve_ask`] is `false`. That is not the MVP
/// auto-consent that flag describes — the consent has a recorded basis, namely
/// the pre-authorization the user granted the envelope — and it is what §10 of
/// this feature's brief means by "ASK policy where explicitly pre-authorized".
pub struct RecoveryAdmission<'a> {
    /// The envelope, read live. Its enable bit is re-read every iteration, so
    /// revoking Autopilot mid-run stops the run at the next iteration boundary
    /// (never mid-action).
    pub envelope: &'a AutopilotEnvelope,
    /// Disk pressure as observed by the caller before the run, checked once
    /// against the envelope's floor. `None` — the reading was unavailable —
    /// fails closed, per [`crate::autopilot::admits_pressure`].
    pub observed_pressure: Option<PressureState>,
}

/// What the last revalidation pass looked at and left alone.
///
/// The counts are of candidates, never of bytes. Summing bytes a run is not
/// allowed to take would present unreachable space as an opportunity, which is
/// the same misread [`crate::reporting::dto::RecoveryOpportunityReport`]
/// already splits its own totals to prevent.
///
/// Each count is a different next step for the user, which is why they are not
/// one number: something needing confirmation is waiting on them, something
/// protected is not going to become available, and something not executable
/// right now (a live tool, a dirty worktree) may well be tomorrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemainingCandidates {
    /// Classified `Ask`: real, reachable, and waiting for the user to say so.
    pub requires_confirmation: u32,
    /// Classified `Protected`. Not a queue — the policy refuses these on
    /// evidence, and a later run refuses them again on the same evidence.
    pub protected: u32,
    /// Past the policy gate, but the offered action refuses to run against the
    /// resource as it currently stands (per
    /// [`crate::actionability::plan_refusal`]) — typically because the owning
    /// tool is live or the work is in progress.
    pub not_executable: u32,
    /// Executable, and refused by the Autopilot envelope instead (HORO-1510):
    /// a kind the user did not allowlist, an `Ask` risk they did not
    /// pre-authorize, or a size the remaining byte budget cannot cover.
    ///
    /// Always `0` for a run with no [`RecoveryAdmission`], which is what makes
    /// this the honest counterpart of the three above: those say a *resource*
    /// is unavailable to anyone, this says only that *this* run was not allowed
    /// to take it. A human-driven run over the same disk may well take all of
    /// them.
    pub not_permitted_by_autopilot: u32,
}

/// Why a recovery run stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// `RecoveryConfig::target` was met (checked at the top of an
    /// iteration, before any candidate is considered).
    TargetReached,
    /// No `AutoSafe` candidate remains, and either no `Ask` candidate
    /// remains or `auto_approve_ask` is `false`.
    ///
    /// Carries what the last pass left behind, and carries it in the variant
    /// rather than beside it: "nothing safe left" is only honest alongside
    /// what *is* still there, and a breakdown that could be omitted would be
    /// (HORO-1509).
    SafeExhausted(RemainingCandidates),
    /// `max_iterations`, `max_actions`, or `max_duration` was reached.
    BudgetExceeded,
    /// An *automatic* run (one given a [`RecoveryAdmission`]) ran out of
    /// envelope: Autopilot was revoked, the machine is not under enough disk
    /// pressure, or one of the envelope's action/time/byte budgets is spent
    /// (HORO-1510).
    ///
    /// Its own variant rather than a flavour of [`StopReason::BudgetExceeded`]
    /// or [`StopReason::SafeExhausted`], because it means something a user can
    /// act on and those two do not. `BudgetExceeded` is about the limits *this
    /// invocation* was given; this is about the standing authority the user
    /// granted Autopilot, which they can widen. And reporting it as
    /// `SafeExhausted` would be the serious error: it would tell someone their
    /// disk has no safe opportunities left when what actually happened is that
    /// Autopilot had used up its allowance and stopped — with, quite possibly,
    /// plenty still there for a run they start themselves (§7).
    ///
    /// Unreachable for a run with no `RecoveryAdmission`.
    EnvelopeRefused(RefusalReason),
    /// The last `NO_PROGRESS_STREAK_THRESHOLD` consecutive successfully
    /// executed actions each measured zero (or unmeasurable) actual
    /// reclaimed bytes.
    NoProgress,
    /// The user asked the run to stop, and it stopped — after the action that
    /// was in flight at the time finished, never in the middle of one
    /// (HORO-1509). Distinct from every other reason on purpose: a run the
    /// user ended has not failed, has not exhausted anything, and has not
    /// reached the goal.
    StoppedByUser,
    /// The initial or a subsequent disk-usage measurement itself failed.
    Error(String),
}

/// Summary of one recovery run.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    pub stop_reason: StopReason,
    pub iterations_run: u32,
    pub actions_executed: u32,
    /// `Ask` candidates that were found but not executed — either because
    /// `auto_approve_ask` was `false`, or because `policy::authorize`
    /// unexpectedly refused, or because the resource's action id could
    /// not be resolved. Also incremented for a candidate whose executed
    /// action came back `Failed`/`AbortedByRevalidation` (it is skipped,
    /// never retried, for the remainder of this run).
    pub actions_declined_or_skipped: u32,
    /// Sum of ACTUAL (not expected) reclaimed bytes across all executed
    /// actions, per [`crate::executor::ExecutionReport::actual_reclaimed_bytes`].
    pub total_bytes_freed: u64,
    pub started_free_bytes: u64,
    pub final_free_bytes: u64,
    /// Detectors whose probe failed during this run, as
    /// `<detector_id>: <reason>`, deduplicated and in first-seen order
    /// across every iteration (HORO-1484).
    ///
    /// Non-empty means discovery was incomplete, so
    /// [`StopReason::SafeExhausted`] does not carry its usual meaning:
    /// "no safe candidate remains" was concluded without having looked
    /// everywhere. A detector whose *tool is absent* is not in here —
    /// that is normal state, and its absence of candidates is a real
    /// answer rather than a missing one.
    pub detector_failures: Vec<String>,
}

impl RecoveryReport {
    /// The lines a human-facing renderer must print alongside the counts
    /// when discovery was incomplete — empty when every detector answered.
    ///
    /// This lives here rather than in `main.rs`'s printer so that the
    /// wording which withdraws a [`StopReason::SafeExhausted`] claim has one
    /// producer and can be asserted by a test (HORO-1484). A renderer that
    /// prints the counts without these lines presents a partial search as a
    /// complete one.
    pub fn discovery_caveat_lines(&self) -> Vec<String> {
        if self.detector_failures.is_empty() {
            return Vec::new();
        }
        let mut lines = vec![format!(
            "discovery incomplete:   {} detector(s) failed",
            self.detector_failures.len()
        )];
        for failure in &self.detector_failures {
            lines.push(format!("  - {failure}"));
        }
        if matches!(self.stop_reason, StopReason::SafeExhausted(_)) {
            lines.push(
                "note: this run stopped because no safe candidate remained among the \
                 detectors that answered; it is not a finding that nothing safe is left"
                    .to_string(),
            );
        }
        lines
    }
}

/// Is `usage` already at or past `target`? Shared by the loop's own
/// target check and available to callers (e.g. the CLI) that want to
/// report progress without duplicating the arithmetic.
pub fn target_met(usage: &FsUsage, target: &FreeTarget) -> bool {
    match target {
        FreeTarget::Percentage(pct) => {
            if usage.total_bytes == 0 {
                return true;
            }
            usage.free_bytes as f64 >= (pct / 100.0) * usage.total_bytes as f64
        }
        FreeTarget::AbsoluteBytes(bytes) => usage.free_bytes >= *bytes,
    }
}

/// Best-effort "how big is this candidate" metric used to rank candidates
/// within a bucket: prefer `reclaimable_bytes` (the more precise probe),
/// falling back to `logical_bytes`, falling back to `0` when neither was
/// observed (such a candidate can still be selected — e.g. an
/// `AutoSafe` bucket with only unmeasured candidates — just not
/// preferentially).
fn candidate_size(ev: &Evidence) -> u64 {
    candidate_bytes(ev).unwrap_or(0)
}

/// [`candidate_size`] without the `0` fallback: `None` means neither probe
/// answered, which is not the same fact as "this candidate would free nothing"
/// and must not be reported as though it were (HORO-1509).
fn candidate_bytes(ev: &Evidence) -> Option<u64> {
    ev.reclaimable_bytes
        .observed()
        .or_else(|| ev.logical_bytes.observed())
        .copied()
}

/// Resolves the [`ActionId`] to execute for `ev`, if any: prefers
/// `Evidence::native_cleanup` when it names a real registered action,
/// otherwise falls back to [`ActionRegistry::find_for_kind`]. The
/// fallback matters today — see [`ActionRegistry::find_for_kind`]'s doc
/// comment — because every built-in detector currently always leaves
/// `native_cleanup` as `Unsupported`.
fn resolve_action_id(ev: &Evidence, registry: &ActionRegistry) -> Option<ActionId> {
    if let NativeCleanup::Available(id) = ev.native_cleanup {
        if registry.get(id.0).is_some() {
            return Some(id);
        }
    }
    registry.find_for_kind(ev.resource.kind).map(|a| a.id())
}

/// Flattens every detector's [`DetectorStatus::Found`] evidence into one
/// list, pairing each candidate with its resolved [`ActionId`] and
/// dropping any candidate with no resolvable action at all — plus, as the
/// second half of the pair, every detector whose probe *failed*.
///
/// Both non-`Found` statuses contribute no candidate, and that is where
/// the similarity ends (HORO-1484). `ToolAbsent` is normal, expected state
/// — the tool isn't installed, there was never anything of that kind to
/// reclaim — and needs no report. `Failed` means the probe did not answer,
/// so whatever that detector would have found is unknown; this loop still
/// cannot act on it, but a run that stops at `SafeExhausted` having never
/// successfully looked in one place must not report that as "nothing safe
/// is left". The failures ride along so [`RecoveryReport`] can say so.
fn candidates_with_actions(
    statuses: Vec<(crate::detectors::DetectorId, DetectorStatus)>,
    registry: &ActionRegistry,
) -> (Vec<(Evidence, ActionId)>, Vec<String>) {
    let mut evidences = Vec::new();
    let mut failures = Vec::new();
    for (id, status) in statuses {
        match status {
            DetectorStatus::Found(found) => evidences.extend(found),
            DetectorStatus::ToolAbsent => {}
            DetectorStatus::Failed(reason) => failures.push(format!("{}: {reason}", id.0)),
        }
    }
    let candidates = evidences
        .into_iter()
        .filter_map(|ev| {
            let action_id = resolve_action_id(&ev, registry)?;
            Some((ev, action_id))
        })
        .collect();
    (candidates, failures)
}

/// One classified, sized candidate ready for approval.
struct ScoredCandidate {
    evidence: Evidence,
    action_id: ActionId,
    decision: PolicyDecision,
    size: u64,
}

/// One iteration's view of a [`RecoveryAdmission`]: the envelope, plus what the
/// run has spent so far, which is the other half of what
/// [`crate::autopilot::admit`] needs.
///
/// Borrowed rather than owned so the ledger stays in [`run`] — there is exactly
/// one per run, and a copy handed to a selection pass would let a pass's charges
/// vanish when it returned.
struct Narrowing<'a> {
    envelope: &'a AutopilotEnvelope,
    ledger: &'a BudgetLedger,
    elapsed: Duration,
}

/// Everything one [`select_candidate`] pass needs.
///
/// A struct rather than a parameter list because HORO-1510's `narrowing` was
/// the eighth argument, and eight positional arguments — three of which are
/// `Option`/`bool` flags — is a call site nobody can read. Mirrors
/// [`RecoveryRunRequest`], which exists for the same reason one level up.
struct Selecting<'a> {
    candidates: Vec<(Evidence, ActionId)>,
    registry: &'a ActionRegistry,
    policy_cfg: &'a PolicyConfig,
    collector: &'a dyn EvidenceCollector,
    now: SystemTime,
    /// Resources this run has already given up on — see loop step 9.
    excluded: &'a HashSet<ResourceId>,
    auto_approve_ask: bool,
    /// `Some` makes this an automatic run (HORO-1510).
    narrowing: Option<Narrowing<'a>>,
}

/// What one [`select_candidate`] pass concluded.
struct Selection {
    /// The one candidate to act on this iteration, if any.
    candidate: Option<ScoredCandidate>,
    remaining: RemainingCandidates,
    /// Under an Autopilot envelope: the first refusal naming one of its
    /// run-wide budgets or preconditions, if the pass hit one.
    ///
    /// Only meaningful when `candidate` is `None`, and the caller reads it only
    /// then. A pass that admitted something has nothing to explain: a byte
    /// budget too small for the largest candidate is not a reason to stop when a
    /// smaller one fits, and the pass keeps looking rather than taking the first
    /// refusal as a verdict on the rest.
    budget_refusal: Option<RefusalReason>,
}

/// Refreshes `ev`'s correlation fields via `collector` (reusing
/// [`crate::evidence::correlate::merge_into`] exactly as HORO-951's
/// deletion-time revalidation does) and re-stamps `collected_at` with the
/// injected wall-clock `now`, so [`crate::policy::classify`]'s staleness
/// check is evaluated against genuinely fresh evidence rather than
/// whatever moment the detector originally ran at.
fn refresh_evidence(
    mut ev: Evidence,
    collector: &dyn EvidenceCollector,
    now: SystemTime,
) -> Evidence {
    let correlation = collector.collect(
        &ev.resource,
        ProbeBudget {
            timeout: CANDIDATE_CORRELATION_TIMEOUT,
        },
    );
    merge_into(&mut ev, correlation);
    ev.collected_at = now;
    ev
}

/// Selects at most one candidate for this iteration: refreshes evidence
/// and classifies every discovered candidate (skipping any resource in
/// `excluded`, i.e. one this run has already given up on — see loop step
/// 9), then picks the largest-by-`candidate_size` `AutoSafe` candidate if
/// any exist; only when none exist, and only when `auto_approve_ask` is
/// set, picks the largest `Ask` candidate instead. Also returns what it left
/// behind, which the caller needs in two places: the number of `Ask`
/// candidates not eligible for auto-approval feeds
/// `actions_declined_or_skipped`, and the whole breakdown is what
/// [`StopReason::SafeExhausted`] carries.
///
/// # Why the actionability filter is here rather than at step 8
///
/// A candidate whose action execution would refuse on sight is dropped
/// during selection (HORO-1359), which means the loop picks the
/// next-largest candidate *in the same iteration*. Filtering it later, in
/// the caller just before `execute`, would have been a smaller diff and
/// wrong: the caller can only `continue`, so each guaranteed-to-fail
/// candidate would spend one of `config.max_iterations`. Burning iteration
/// budget on a step that could never have worked is the specific harm in a
/// low-disk recovery, which is the situation this loop exists for.
///
/// The `Protected` arm still returns before anything is planned. That
/// ordering is why the filter reads the policy-free
/// [`crate::actionability::plan_refusal`] rather than `static_refusal`: the
/// `match` below is the Protected gate, and it is already there.
///
/// # The Autopilot gate, when there is one
///
/// `narrowing` present makes this an automatic run (HORO-1510). Every candidate
/// that survives the filters above is then put to [`crate::autopilot::admit`],
/// and a refusal drops it here for the same reason as the actionability filter:
/// the loop moves on to the next-largest candidate within the same iteration
/// rather than spending one to be told no.
///
/// The gate runs *after* the actionability filter on purpose. A candidate whose
/// action refuses on sight is unavailable to every run, envelope or not, and
/// that is the more durable half of the answer — so it is counted as
/// `not_executable` rather than blamed on Autopilot.
fn select_candidate(request: Selecting<'_>) -> Selection {
    let Selecting {
        candidates,
        registry,
        policy_cfg,
        collector,
        now,
        excluded,
        auto_approve_ask,
        narrowing,
    } = request;

    let mut auto_safe: Vec<ScoredCandidate> = Vec::new();
    let mut ask: Vec<ScoredCandidate> = Vec::new();
    let mut protected: u32 = 0;
    let mut not_executable: u32 = 0;
    let mut not_permitted_by_autopilot: u32 = 0;
    let mut budget_refusal: Option<RefusalReason> = None;

    for (ev, action_id) in candidates {
        if excluded.contains(&ev.resource) {
            continue;
        }
        let refreshed = refresh_evidence(ev, collector, now);
        let decision = classify(&refreshed, policy_cfg, now);
        let size = candidate_size(&refreshed);
        match decision.class {
            PolicyClass::AutoSafe | PolicyClass::Ask => {
                // Past the Protected gate, so planning is permitted here
                // and only here.
                //
                // A missing action id is deliberately NOT treated as a
                // refusal: `candidates_with_actions` resolved it from this
                // same registry, so it cannot be missing in production, and
                // step 8 in the caller already reports the case. Answering
                // it here would mean this filter deciding something that is
                // not its question. Nothing unsafe follows either way —
                // `execute` refuses an unknown action independently.
                if let Some(action) = registry.get(action_id.0) {
                    if crate::actionability::plan_refusal(action, &refreshed).is_some() {
                        not_executable += 1;
                        continue;
                    }
                }
                if let Some(narrowing) = narrowing.as_ref() {
                    // `candidate_bytes`, not `size`: `admit` has to be told
                    // "unmeasured" as unmeasured, because a candidate whose
                    // reclaimable size is unknown cannot be charged against a
                    // byte budget and it refuses rather than charging it as the
                    // zero `candidate_size` would hand it.
                    let admission = admit(
                        narrowing.envelope,
                        narrowing.ledger,
                        &decision,
                        candidate_bytes(&refreshed),
                        narrowing.elapsed,
                    );
                    if let Some(reason) = admission.refusal() {
                        not_permitted_by_autopilot += 1;
                        if reason.is_budget_or_precondition_refusal() && budget_refusal.is_none() {
                            budget_refusal = Some(reason);
                        }
                        continue;
                    }
                }
                let scored = ScoredCandidate {
                    evidence: refreshed,
                    action_id,
                    decision,
                    size,
                };
                if scored.decision.class == PolicyClass::AutoSafe {
                    auto_safe.push(scored);
                } else {
                    ask.push(scored);
                }
            }
            PolicyClass::Protected => protected += 1,
        }
    }

    // `requires_confirmation` is what this pass could not act on *itself*, so
    // it is zero wherever a candidate was selected: an `Ask` left unpicked
    // because something safer went first has not been left behind, it is next.
    // The only reader of the breakdown is `StopReason::SafeExhausted`, which
    // exists precisely when nothing was selected.
    let remaining = |requires_confirmation: u32| RemainingCandidates {
        requires_confirmation,
        protected,
        not_executable,
        not_permitted_by_autopilot,
    };
    let selected = |candidate: Option<ScoredCandidate>, requires_confirmation: u32| Selection {
        candidate,
        remaining: remaining(requires_confirmation),
        budget_refusal,
    };

    if let Some(best) = auto_safe.into_iter().max_by_key(|c| c.size) {
        return selected(Some(best), 0);
    }

    // An `Ask` candidate that reached the bucket under an envelope was admitted
    // by the gate, and the gate admits `Ask` only where the envelope
    // pre-authorizes that exact risk — so the envelope, not
    // `auto_approve_ask`, is what makes it eligible here. See
    // [`RecoveryAdmission`] for why those two are different kinds of consent.
    if auto_approve_ask || narrowing.is_some() {
        let best = ask.into_iter().max_by_key(|c| c.size);
        selected(best, 0)
    } else {
        selected(None, ask.len() as u32)
    }
}

#[cfg(test)]
mod select_candidate_tests {
    use super::*;
    use crate::detectors::DetectorId;
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        GitState, Recoverability, ResourceFingerprint, ResourceKind, ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use std::path::PathBuf;

    /// [`select_candidate`] as every test below calls it: with no Autopilot
    /// envelope, which is what a recovery a human asked for looks like.
    ///
    /// The two assertions are the point, not plumbing. They hold HORO-1510's
    /// central claim — that adding the envelope seam changed nothing about a
    /// run that has no envelope — and because every existing test in this module
    /// routes through here, all of them assert it.
    #[allow(clippy::too_many_arguments)]
    fn select_without_envelope(
        candidates: Vec<(Evidence, ActionId)>,
        registry: &ActionRegistry,
        policy_cfg: &PolicyConfig,
        collector: &dyn EvidenceCollector,
        now: SystemTime,
        excluded: &HashSet<ResourceId>,
        auto_approve_ask: bool,
    ) -> (Option<ScoredCandidate>, RemainingCandidates) {
        let selection = select_candidate(Selecting {
            candidates,
            registry,
            policy_cfg,
            collector,
            now,
            excluded,
            auto_approve_ask,
            narrowing: None,
        });
        assert_eq!(
            selection.budget_refusal, None,
            "with no envelope there is no envelope allowance to have been spent"
        );
        assert_eq!(
            selection.remaining.not_permitted_by_autopilot, 0,
            "with no envelope nothing can have been refused by one"
        );
        (selection.candidate, selection.remaining)
    }

    /// A collector that always reports clean, complete correlation — the
    /// baseline that makes complete/fresh evidence reach `AutoSafe`.
    struct CleanCollector;

    impl EvidenceCollector for CleanCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    /// A collector that reports the owning tool as live — pins the
    /// resulting decision to `Ask{OwningToolLive}`.
    struct ToolLiveCollector;

    impl EvidenceCollector for ToolLiveCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(true),
            }
        }
    }

    fn evidence(path: &str, kind: ResourceKind, logical_bytes: u64) -> Evidence {
        Evidence {
            resource: ResourceId::new(kind, ResourceLocator::Path(PathBuf::from(path))),
            fingerprint: ResourceFingerprint {
                dev_ino: None,
                mtime: None,
                tool_revision: None,
            },
            detector: DetectorId("test"),
            logical_bytes: ProbeOutcome::Observed(logical_bytes),
            physical_bytes: None,
            reclaimable_bytes: ProbeOutcome::Observed(logical_bytes),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::UNIX_EPOCH),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: kind.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Available(ActionId("test.action")),
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::UNIX_EPOCH,
            sources: Vec::new(),
        }
    }

    #[test]
    fn picks_largest_auto_safe_candidate_by_size() {
        let small = evidence("/tmp/small/target", ResourceKind::CargoTargetDir, 100);
        let big = evidence("/tmp/big/target", ResourceKind::CargoTargetDir, 10_000);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, remaining) = select_without_envelope(
            vec![
                (small, ActionId("test.action")),
                (big, ActionId("test.action")),
            ],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            false,
        );

        assert_eq!(remaining.requires_confirmation, 0);
        let selected = selected.expect("expected an AutoSafe candidate");
        assert_eq!(selected.decision.class, PolicyClass::AutoSafe);
        assert_eq!(selected.size, 10_000);
    }

    #[test]
    fn excludes_resources_already_given_up_on() {
        let only = evidence("/tmp/only/target", ResourceKind::CargoTargetDir, 100);
        let resource = only.resource.clone();
        let mut excluded = HashSet::new();
        excluded.insert(resource);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, _) = select_without_envelope(
            vec![(only, ActionId("test.action"))],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &excluded,
            false,
        );

        assert!(selected.is_none());
    }

    #[test]
    fn ask_candidates_are_not_selected_when_auto_approve_ask_is_false() {
        let ev = evidence("/tmp/live/target", ResourceKind::XcodeDerivedData, 100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, remaining) = select_without_envelope(
            vec![(ev, ActionId("test.action"))],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &ToolLiveCollector,
            now,
            &HashSet::new(),
            false,
        );

        assert!(selected.is_none());
        // The whole breakdown, not just the one non-zero field: a candidate
        // waiting for consent must not also be reported as protected or as
        // unrunnable, because each of those is a different answer to "what
        // should I do next?" (HORO-1509).
        assert_eq!(
            remaining,
            RemainingCandidates {
                requires_confirmation: 1,
                protected: 0,
                not_executable: 0,
                not_permitted_by_autopilot: 0,
            }
        );
    }

    #[test]
    fn ask_candidates_are_selected_when_auto_approve_ask_is_true() {
        let ev = evidence("/tmp/live/target", ResourceKind::XcodeDerivedData, 100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, remaining) = select_without_envelope(
            vec![(ev, ActionId("test.action"))],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &ToolLiveCollector,
            now,
            &HashSet::new(),
            true,
        );

        assert_eq!(remaining.requires_confirmation, 0);
        let selected = selected.expect("expected an Ask candidate to be auto-approved");
        assert_eq!(selected.decision.class, PolicyClass::Ask);
    }

    #[test]
    fn protected_candidates_are_never_selected() {
        // Unknown resource kind is unconditionally Protected regardless
        // of evidence content.
        let ev = evidence("/tmp/unknown/thing", ResourceKind::Unknown, 100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, remaining) = select_without_envelope(
            vec![(ev, ActionId("test.action"))],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            true,
        );

        assert!(selected.is_none());
        // `auto_approve_ask` is true here, so the only thing that can have
        // held this candidate back is the policy class — and the breakdown
        // must name that rather than implying the user could consent to it.
        assert_eq!(
            remaining,
            RemainingCandidates {
                requires_confirmation: 0,
                protected: 1,
                not_executable: 0,
                not_permitted_by_autopilot: 0,
            }
        );
    }

    /// A real, minimal cargo project under the OS temp dir, so
    /// `cargo.clean.target_dir` can genuinely plan for it — its planner stats
    /// `Cargo.toml` and refuses without one. Returns the target dir.
    ///
    /// The other tests in this module use non-existent paths deliberately,
    /// since policy classification does not need the resource to exist. The
    /// two HORO-1359 tests below do need it, because they are about whether
    /// an action can be *planned*.
    fn cargo_project_fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "glomeris-h1359-select-{tag}-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("create target dir");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        )
        .expect("write Cargo.toml");
        target
    }

    /// HORO-1359: the loop must not pick a candidate whose action execution
    /// refuses on sight — and must fall through to the next-largest one in
    /// the *same* call.
    ///
    /// The Homebrew cache is deliberately the larger of the two, which is
    /// also the realistic case: it is often the biggest single thing on a
    /// developer's disk, so the old ordering picked it first, `execute`
    /// refused it, and one of `config.max_iterations` was spent. Asserting
    /// that the cargo target dir comes back — rather than merely that the
    /// cache does not — is what pins the filter to selection rather than to
    /// the caller, where each refused candidate would still cost an
    /// iteration.
    #[test]
    fn a_candidate_whose_action_cannot_run_is_skipped_for_the_next_largest() {
        let target = cargo_project_fixture("skip-for-next");
        let brew_cache = std::env::temp_dir().join("glomeris-h1359-nonexistent-brew-cache");
        let registry = ActionRegistry::builtin();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let big_but_refused = evidence(
            &brew_cache.to_string_lossy(),
            ResourceKind::HomebrewCache,
            10_000,
        );
        let small_but_runnable =
            evidence(&target.to_string_lossy(), ResourceKind::CargoTargetDir, 100);

        // Fixture validity: the refusal asserted below must come from the
        // action being unrunnable, not from the policy class or from a
        // missing registry entry.
        let brew_action = registry
            .get("homebrew.cleanup.cache")
            .expect("registered built-in");
        assert!(
            crate::actionability::plan_refusal(brew_action, &big_but_refused).is_some(),
            "fixture invalid: this action must be statically refused"
        );

        let (selected, remaining) = select_without_envelope(
            vec![
                (big_but_refused, ActionId("homebrew.cleanup.cache")),
                (small_but_runnable, ActionId("cargo.clean.target_dir")),
            ],
            &registry,
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            false,
        );

        // Positive control and headline assertion in one: the runnable
        // candidate is still selected, so this cannot pass by selecting
        // nothing — and it is selected despite being 100× smaller.
        let selected = selected.expect("the runnable candidate must still be selected");
        assert_eq!(selected.decision.class, PolicyClass::AutoSafe);
        assert_eq!(selected.action_id, ActionId("cargo.clean.target_dir"));
        assert_eq!(
            selected.size, 100,
            "the larger candidate is the refused one; picking it is the defect"
        );
        assert_eq!(
            remaining.requires_confirmation, 0,
            "a statically-refused candidate is not an Ask skip: that count means consent, \
             and conflating the two would misreport why the loop stopped"
        );

        std::fs::remove_dir_all(target.parent().expect("fixture has a parent")).ok();
    }

    /// The degenerate case: when the only candidate is one whose action
    /// cannot run, nothing is selected — and it is not counted as an `Ask`
    /// skip, so `free`'s own stop reason stays truthful.
    #[test]
    fn a_sole_candidate_whose_action_cannot_run_selects_nothing() {
        let brew_cache = std::env::temp_dir().join("glomeris-h1359-nonexistent-brew-cache-only");
        let ev = evidence(
            &brew_cache.to_string_lossy(),
            ResourceKind::HomebrewCache,
            10_000,
        );
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let (selected, remaining) = select_without_envelope(
            vec![(ev, ActionId("homebrew.cleanup.cache"))],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            true,
        );

        assert!(selected.is_none());
        assert_eq!(
            remaining,
            RemainingCandidates {
                requires_confirmation: 0,
                protected: 0,
                not_executable: 1,
                not_permitted_by_autopilot: 0,
            },
            "a candidate the offered action refuses on sight is neither waiting for \
             consent nor protected: it may well run tomorrow, and the breakdown is \
             what tells the user that"
        );
    }

    /// An action id absent from the registry is deliberately NOT treated as
    /// a refusal. Every other test in this module passes
    /// `ActionId("test.action")`, so this pins the behaviour they all rely
    /// on rather than leaving it as an accident of the filter's shape.
    ///
    /// The reasoning: `candidates_with_actions` resolved the id from this
    /// same registry, so it cannot be missing in production; step 8 in the
    /// caller already reports that case; and `execute` refuses an unknown
    /// action independently, so nothing unsafe follows either way. Answering
    /// it here would mean this filter deciding something that is not its
    /// question.
    #[test]
    fn an_unregistered_action_id_is_not_treated_as_a_refusal() {
        let ev = evidence(
            "/tmp/unregistered/target",
            ResourceKind::CargoTargetDir,
            100,
        );
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        assert!(
            ActionRegistry::builtin().get("test.action").is_none(),
            "fixture invalid: this id must be absent from the registry"
        );

        let (selected, _) = select_without_envelope(
            vec![(ev, ActionId("test.action"))],
            &ActionRegistry::builtin(),
            &PolicyConfig::default(),
            &CleanCollector,
            now,
            &HashSet::new(),
            false,
        );

        assert!(
            selected.is_some(),
            "an unresolvable action id must reach the caller, which reports it"
        );
    }

    // ----------------------------------------------------------------------
    // HORO-1510: the same pass, narrowed by an Autopilot envelope.
    //
    // Everything above calls `select_without_envelope`, so the no-envelope
    // behaviour is already pinned (including, in that helper's own two
    // assertions, that the seam is inert without one). What follows is about
    // the seam itself, and each test narrows the envelope in exactly ONE
    // place — an envelope narrowed in two cannot say which of them refused.
    // ----------------------------------------------------------------------

    /// An enabled envelope that allows `kinds` and nothing else, with every
    /// budget at its ceiling so no budget can be what refused.
    fn envelope_allowing(kinds: &[ResourceKind]) -> AutopilotEnvelope {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        for kind in kinds {
            envelope.allow_kind(*kind).expect("kind is allowlistable");
        }
        envelope
            .set_max_actions(crate::autopilot::envelope::ACTIONS_CEILING)
            .expect("the ceiling is within the ceiling");
        envelope
            .set_max_bytes(crate::autopilot::envelope::BYTES_CEILING)
            .expect("the ceiling is within the ceiling");
        envelope
            .set_max_duration(crate::autopilot::envelope::DURATION_CEILING)
            .expect("the ceiling is within the ceiling");
        envelope
    }

    /// [`select_candidate`] under `envelope`, with a fresh (nothing-spent)
    /// ledger and no elapsed time — so what refuses is the envelope's shape,
    /// never what an earlier action in the same run had already used up.
    fn select_under(
        envelope: &AutopilotEnvelope,
        candidates: Vec<(Evidence, ActionId)>,
        collector: &dyn EvidenceCollector,
        now: SystemTime,
        auto_approve_ask: bool,
    ) -> Selection {
        let ledger = BudgetLedger::for_envelope(envelope);
        select_candidate(Selecting {
            candidates,
            registry: &ActionRegistry::builtin(),
            policy_cfg: &PolicyConfig::default(),
            collector,
            now,
            excluded: &HashSet::new(),
            auto_approve_ask,
            narrowing: Some(Narrowing {
                envelope,
                ledger: &ledger,
                elapsed: Duration::ZERO,
            }),
        })
    }

    /// A kind the user did not allowlist is Autopilot's limit, not the
    /// disk's — so it is counted separately, and it is NOT reportable as a
    /// reason to stop. Widening the allowlist would admit it, which is
    /// precisely why `is_budget_or_precondition_refusal` excludes it.
    ///
    /// The allowed candidate here is the *smaller* of the two, so a pass that
    /// ignored the envelope would pick the other one and fail.
    #[test]
    fn a_kind_the_envelope_does_not_allow_is_counted_against_autopilot() {
        let allowed = evidence("/tmp/allowed/target", ResourceKind::CargoTargetDir, 100);
        let not_allowed = evidence(
            "/tmp/not-allowed/node_modules",
            ResourceKind::NodeModules,
            10_000,
        );
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let envelope = envelope_allowing(&[ResourceKind::CargoTargetDir]);

        let selection = select_under(
            &envelope,
            vec![
                (allowed, ActionId("test.action")),
                (not_allowed, ActionId("test.action")),
            ],
            &CleanCollector,
            now,
            false,
        );

        assert_eq!(
            selection.candidate.as_ref().map(|c| c.size),
            Some(100),
            "the allowlisted candidate must be selected even though it is smaller"
        );
        assert_eq!(
            selection.remaining,
            RemainingCandidates {
                requires_confirmation: 0,
                protected: 0,
                not_executable: 0,
                not_permitted_by_autopilot: 1,
            }
        );
        assert_eq!(
            selection.budget_refusal, None,
            "a non-allowlisted kind is not a spent allowance, and reporting it as \
             one would tell the user to come back later about something that will \
             refuse identically every time"
        );
    }

    /// A byte budget too small for the largest candidate must not end the
    /// pass: the loop is supposed to fall through to one that fits. This is
    /// the reason `budget_refusal` is only read when nothing was selected.
    #[test]
    fn a_byte_budget_too_small_for_the_biggest_candidate_still_selects_a_smaller_one() {
        let small = evidence("/tmp/small/target", ResourceKind::CargoTargetDir, 1_024);
        let big = evidence("/tmp/big/target", ResourceKind::CargoTargetDir, 16_384);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let mut envelope = envelope_allowing(&[ResourceKind::CargoTargetDir]);
        envelope.set_max_bytes(4_096).expect("under the ceiling");

        let selection = select_under(
            &envelope,
            vec![
                (big, ActionId("test.action")),
                (small, ActionId("test.action")),
            ],
            &CleanCollector,
            now,
            false,
        );

        assert_eq!(
            selection.candidate.as_ref().map(|c| c.size),
            Some(1_024),
            "the candidate that fits the remaining byte budget must be selected"
        );
        assert_eq!(selection.remaining.not_permitted_by_autopilot, 1);
        // Recorded, but the caller must not act on it: something WAS selected.
        assert!(matches!(
            selection.budget_refusal,
            Some(RefusalReason::ByteBudgetExhausted { .. })
        ));
    }

    /// When the byte budget fits nothing, the pass reports the budget — and
    /// the caller turns that into `EnvelopeRefused`. Calling this disk
    /// "exhausted of safe candidates" would be false: the same candidate is
    /// available to a recovery the user starts themselves.
    #[test]
    fn a_byte_budget_that_fits_nothing_reports_the_budget_not_exhaustion() {
        let big = evidence("/tmp/big/target", ResourceKind::CargoTargetDir, 16_384);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let mut envelope = envelope_allowing(&[ResourceKind::CargoTargetDir]);
        envelope.set_max_bytes(4_096).expect("under the ceiling");

        let selection = select_under(
            &envelope,
            vec![(big, ActionId("test.action"))],
            &CleanCollector,
            now,
            false,
        );

        assert!(selection.candidate.is_none());
        assert_eq!(
            selection.budget_refusal,
            Some(RefusalReason::ByteBudgetExhausted {
                would_reclaim: 16_384,
                remaining: 4_096,
            })
        );
    }

    /// An `Ask` risk the envelope pre-authorizes is selectable even though
    /// `auto_approve_ask` is false. Those two are different kinds of consent
    /// — see [`RecoveryAdmission`] — and this is the one place the difference
    /// is observable.
    ///
    /// `RebuildCostHigh` is the only pre-authorizable `Ask` reason there is
    /// (see [`crate::autopilot::envelope::is_preauthorizable`]), and
    /// per-instance `NotRegenerable` evidence is what produces it.
    #[test]
    fn an_ask_risk_the_envelope_preauthorized_is_selectable_without_auto_approve_ask() {
        let mut ev = evidence("/tmp/costly/target", ResourceKind::CargoTargetDir, 100);
        ev.regenerability = crate::evidence::model::Regenerability::NotRegenerable;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let mut envelope = envelope_allowing(&[ResourceKind::CargoTargetDir]);
        envelope
            .preauthorize_ask(
                ResourceKind::CargoTargetDir,
                crate::policy::ReasonCode::RebuildCostHigh,
            )
            .expect("RebuildCostHigh is pre-authorizable for an allowlisted kind");

        let selection = select_under(
            &envelope,
            vec![(ev, ActionId("test.action"))],
            &CleanCollector,
            now,
            // The flag this feature must NOT be borrowing authority from.
            false,
        );

        let selected = selection
            .candidate
            .expect("a pre-authorized Ask risk is admissible");
        assert_eq!(selected.decision.class, PolicyClass::Ask);
        assert_eq!(
            selected.decision.reasons,
            vec![crate::policy::ReasonCode::RebuildCostHigh],
            "fixture invalid unless this is the exact risk the envelope named"
        );
        assert_eq!(selection.remaining.not_permitted_by_autopilot, 0);
    }

    /// The other half of the same claim: an `Ask` risk the envelope did NOT
    /// name is refused, so pre-authorization is a per-risk grant rather than
    /// a blanket one. `OwningToolLive` is not pre-authorizable at all — it is
    /// true now and may be false a second later — so no envelope can admit
    /// this candidate.
    ///
    /// Note where it is counted: against Autopilot, not against
    /// `requires_confirmation`. Both are "left behind", but only one of them
    /// is answered by the user pressing a button in this run.
    #[test]
    fn an_ask_risk_the_envelope_did_not_preauthorize_is_refused() {
        let ev = evidence(
            "/tmp/live/derived-data",
            ResourceKind::XcodeDerivedData,
            100,
        );
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        let envelope = envelope_allowing(&[ResourceKind::XcodeDerivedData]);

        let selection = select_under(
            &envelope,
            vec![(ev, ActionId("test.action"))],
            &ToolLiveCollector,
            now,
            false,
        );

        assert!(selection.candidate.is_none());
        assert_eq!(
            selection.remaining,
            RemainingCandidates {
                requires_confirmation: 0,
                protected: 0,
                not_executable: 0,
                not_permitted_by_autopilot: 1,
            }
        );
        assert_eq!(
            selection.budget_refusal, None,
            "an un-named Ask risk is not a spent allowance"
        );
    }

    /// Anti-vacuity (campaign §15, §11): the envelope is a *narrowing* stage,
    /// so there must be no envelope at all under which a `Protected`
    /// candidate becomes selectable.
    ///
    /// Constructed to be the widest envelope the type system permits: every
    /// allowlistable kind, every budget at its ceiling, and a pre-authorized
    /// `Ask` risk. `ResourceKind::Unknown` is not among the allowlisted kinds
    /// because [`AutopilotEnvelope::allow_kind`] refuses it — which is itself
    /// part of what this test states.
    #[test]
    fn protected_is_not_selectable_under_the_widest_envelope_expressible() {
        let ev = evidence("/tmp/unknown/thing", ResourceKind::Unknown, 10_000);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let allowlistable: Vec<ResourceKind> = ResourceKind::ALL
            .iter()
            .copied()
            .filter(|k| *k != ResourceKind::Unknown)
            .collect();
        let mut envelope = envelope_allowing(&allowlistable);
        envelope
            .preauthorize_ask(
                ResourceKind::CargoTargetDir,
                crate::policy::ReasonCode::RebuildCostHigh,
            )
            .expect("allowlisted above");
        assert!(
            envelope.allow_kind(ResourceKind::Unknown).is_err(),
            "an envelope must not be able to name the fail-closed kind at all"
        );

        let selection = select_under(
            &envelope,
            vec![(ev, ActionId("test.action"))],
            &CleanCollector,
            now,
            // Both consent flags open simultaneously, so neither of them is
            // what refused.
            true,
        );

        assert!(
            selection.candidate.is_none(),
            "a Protected candidate is refused before the envelope is consulted"
        );
        assert_eq!(
            selection.remaining,
            RemainingCandidates {
                requires_confirmation: 0,
                protected: 1,
                not_executable: 0,
                // Attribution matters: this was not Autopilot's limit. A
                // protected resource is refused to every run, and telling the
                // user to widen their envelope would be a lie.
                not_permitted_by_autopilot: 0,
            }
        );
    }
}

/// Builds an [`AuditRecord`] (HORO-1057, `source: "free"`) from a real
/// [`ExecutionReport`] and appends it to `audit_log_path`, unconditionally
/// discarding the `Result` — matching
/// [`crate::monitor::append_audit_record`]'s best-effort contract: an
/// audit-write failure must never influence this loop's own outcome
/// bookkeeping. Never called for [`ExecutionOutcome::DryRun`]'s tag,
/// since [`run`] never produces it (see `cli::record_audit`'s equivalent
/// comment for the `execute` path).
fn append_recovery_audit_record(
    report: &ExecutionReport,
    policy_label: &'static str,
    audit_log_path: &Path,
    now: SystemTime,
) {
    let outcome = execution_outcome_tag(&report.outcome);
    let abort_reason = match &report.outcome {
        ExecutionOutcome::AbortedByRevalidation(reason) => Some(format!("{reason:?}")),
        _ => None,
    };
    let record = AuditRecord {
        timestamp: now
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        action_id: report.action.0.to_string(),
        resource_id: report.resource.to_string(),
        policy_label: policy_label.to_string(),
        outcome: outcome.to_string(),
        abort_reason,
        actual_reclaimed_bytes: report.actual_reclaimed_bytes.observed().copied(),
        source: ActionSource::Free.to_string(),
        // The recovery loop ranks by rule, never by model.
        model_rank: None,
    };
    let _ = append_audit_record(audit_log_path, &record);
}

/// The stable tag for one execution outcome.
///
/// One producer, used by both the audit record above and
/// [`RecoveryProgress::ActionFinished`] — which is what makes HORO-1509 AC5
/// ("final summary and local audit agree on actions/outcomes") a property of
/// the code rather than a coincidence two `match` arms happen to share.
fn execution_outcome_tag(outcome: &ExecutionOutcome) -> &'static str {
    match outcome {
        ExecutionOutcome::Succeeded => "succeeded",
        ExecutionOutcome::Failed(_) => "failed",
        ExecutionOutcome::AbortedByRevalidation(_) => "aborted_by_revalidation",
        ExecutionOutcome::DryRun => "dry_run",
    }
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    stop_reason: StopReason,
    iterations_run: u32,
    actions_executed: u32,
    actions_declined_or_skipped: u32,
    total_bytes_freed: u64,
    started_free_bytes: u64,
    final_free_bytes: u64,
    detector_failures: &[String],
) -> RecoveryReport {
    RecoveryReport {
        stop_reason,
        iterations_run,
        actions_executed,
        actions_declined_or_skipped,
        total_bytes_freed,
        started_free_bytes,
        final_free_bytes,
        detector_failures: detector_failures.to_vec(),
    }
}

/// Everything one recovery run needs.
///
/// Every I/O-touching dependency is injected so [`run`] is fully unit-testable
/// with fakes: `fs_stat` (disk measurement), `collector` (correlation refresh,
/// reused by `execute`'s own revalidation), `detector_registry` (discovery —
/// production callers pass `DetectorRegistry::builtin()`, but this must be a
/// parameter rather than constructed internally: several built-in detectors,
/// e.g. `HomebrewDetector`, shell out to a real already-installed system tool
/// regardless of `discovery_ctx`, which would make [`run`] touch genuine
/// machine state during a test no matter what `fs_stat`/`collector` fakes it
/// was given), `clock` (the `max_duration` budget check), and `wall_clock`
/// (the `SystemTime` `classify`/`authorize` need). `action_registry` and
/// `discovery_ctx` are plain data, not I/O seams, but are still fields rather
/// than constructed internally so a caller controls exactly what is
/// registered/discoverable.
///
/// A struct rather than a parameter list for the reason
/// [`crate::autopilot::run::AutopilotRunRequest`] already gives: eleven
/// independently fakeable seams read as eleven anonymous `&dyn` arguments at a
/// call site, and `#[allow(clippy::too_many_arguments)]` hid that rather than
/// fixing it.
pub struct RecoveryRunRequest<'a> {
    pub config: &'a RecoveryConfig,
    pub fs_stat: &'a dyn FsStat,
    pub collector: &'a dyn EvidenceCollector,
    pub detector_registry: &'a DetectorRegistry,
    pub action_registry: &'a ActionRegistry,
    pub clock: &'a dyn Clock,
    pub wall_clock: &'a dyn WallClock,
    pub policy_cfg: &'a PolicyConfig,
    pub target_mount: &'a Path,
    pub discovery_ctx: &'a DiscoveryContext,
    pub audit_log_path: &'a Path,
    /// Where per-iteration progress goes. Pass `&SilentObserver` for a run
    /// nobody is watching (HORO-1509).
    pub observer: &'a dyn RecoveryObserver,
    /// How the run finds out the user asked it to stop. Pass `&NeverStops`
    /// where nothing can ask (HORO-1509).
    pub stop: &'a dyn StopSignal,
    /// `Some` makes this an *automatic* run, narrowed by an Autopilot envelope.
    /// `None` is a run a human asked for, bounded only by `config`
    /// (HORO-1510). See [`RecoveryAdmission`].
    pub admission: Option<RecoveryAdmission<'a>>,
}

/// Runs one bounded closed-loop recovery pass against
/// [`RecoveryRunRequest::target_mount`], stopping for exactly one of the
/// [`StopReason`]s. See the module docs for the safety invariant this never
/// bypasses, and the ticket's loop steps 1-12 for the per-iteration shape this
/// implements.
pub fn run(request: RecoveryRunRequest<'_>) -> RecoveryReport {
    let RecoveryRunRequest {
        config,
        fs_stat,
        collector,
        detector_registry,
        action_registry,
        clock,
        wall_clock,
        policy_cfg,
        target_mount,
        discovery_ctx,
        audit_log_path,
        observer,
        stop,
        admission,
    } = request;

    let start_instant = clock.now();

    let started_usage = match fs_stat.stat(target_mount) {
        Ok(usage) => usage,
        Err(e) => {
            return build_report(
                StopReason::Error(format!("initial disk measurement failed: {e}")),
                0,
                0,
                0,
                0,
                0,
                0,
                // Nothing has been discovered yet, so there is nothing to
                // report as incomplete.
                &[],
            );
        }
    };
    let started_free_bytes = started_usage.free_bytes;

    let mut iterations_run: u32 = 0;
    let mut actions_executed: u32 = 0;
    let mut actions_declined_or_skipped: u32 = 0;
    let mut total_bytes_freed: u64 = 0;
    let mut no_retry: HashSet<ResourceId> = HashSet::new();
    let mut no_progress_streak: u32 = 0;
    let mut last_free_bytes = started_free_bytes;
    // First-seen order, deduplicated: a detector that fails the same way on
    // every iteration should be reported once, not once per iteration
    // (HORO-1484).
    let mut detector_failures: Vec<String> = Vec::new();
    // `Some` exactly for an automatic run (HORO-1510). The envelope and its
    // ledger are held together in one `Option` rather than in two, so there is
    // no representable state in which a run is narrowed by an envelope whose
    // budgets nothing is counting. One ledger per run, which is
    // `BudgetLedger::for_envelope`'s own contract — budgets that carried across
    // runs would quietly compound into a larger standing allowance than the user
    // granted.
    let mut budgeted: Option<(&AutopilotEnvelope, BudgetLedger)> = admission
        .as_ref()
        .map(|a| (a.envelope, BudgetLedger::for_envelope(a.envelope)));
    let mut pressure_checked = false;

    loop {
        // 1. Measure.
        let usage = match fs_stat.stat(target_mount) {
            Ok(usage) => usage,
            Err(e) => {
                return build_report(
                    StopReason::Error(format!("disk measurement failed: {e}")),
                    iterations_run,
                    actions_executed,
                    actions_declined_or_skipped,
                    total_bytes_freed,
                    started_free_bytes,
                    last_free_bytes,
                    &detector_failures,
                );
            }
        };
        last_free_bytes = usage.free_bytes;
        // The one statement of fact about free space in the stream, emitted
        // from the reading itself rather than derived from what was deleted
        // (HORO-1509 AC2).
        observer.observe(RecoveryProgress::Measured {
            iteration: iterations_run + 1,
            usage,
            bytes_freed_so_far: total_bytes_freed,
        });

        // 2. Target already met?
        if target_met(&usage, &config.target) {
            return build_report(
                StopReason::TargetReached,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
                &detector_failures,
            );
        }

        // 2b. Did the user ask to stop? Asked once per iteration, which is
        // also once after each completed action, because executing one is the
        // last thing an iteration does. There is deliberately no check
        // between `execute`'s revalidation and its mutation: "stop after the
        // current action" is the whole contract, and a deletion abandoned
        // halfway leaves state nobody can describe (HORO-1509).
        //
        // After the target check on purpose. A run that stopped having
        // already reached the goal reached the goal; that is the more useful
        // truth of the two, and the user gets the outcome they asked for
        // either way.
        if stop.stop_requested() {
            observer.observe(RecoveryProgress::StopRequested {
                iteration: iterations_run + 1,
            });
            return build_report(
                StopReason::StoppedByUser,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
                &detector_failures,
            );
        }

        // 2c. Envelope preconditions, for an automatic run only (HORO-1510).
        //
        // After the target and stop checks for the same reason those two are in
        // that order: if the goal is met the run reached the goal, and if the
        // user asked it to stop it stopped, and neither of those becomes an
        // Autopilot refusal just because Autopilot is what started the run.
        if let Some((envelope, _)) = budgeted.as_ref() {
            // Re-read every iteration. `admit` re-reads it per candidate too,
            // but that only bites where there is a candidate to refuse: a run
            // whose discovery comes back empty would otherwise report
            // `SafeExhausted` about an envelope that had been revoked out from
            // under it.
            if !envelope.is_enabled() {
                return build_report(
                    StopReason::EnvelopeRefused(RefusalReason::AutopilotRevoked),
                    iterations_run,
                    actions_executed,
                    actions_declined_or_skipped,
                    total_bytes_freed,
                    started_free_bytes,
                    last_free_bytes,
                    &detector_failures,
                );
            }
            // The pressure floor, checked once and deliberately never again.
            // Not an optimisation: a run that is working lowers the very
            // pressure that admitted it, so re-checking each iteration would
            // abort a successful recovery precisely *because* it had made
            // progress — and would report the abort as a refusal rather than as
            // the partial success it was. What bounds the run once it is under
            // way is the envelope's action, byte and time budgets.
            if !pressure_checked {
                pressure_checked = true;
                let observed = admission.as_ref().and_then(|a| a.observed_pressure);
                if let Err(reason) = admits_pressure(envelope, observed) {
                    return build_report(
                        StopReason::EnvelopeRefused(reason),
                        iterations_run,
                        actions_executed,
                        actions_declined_or_skipped,
                        total_bytes_freed,
                        started_free_bytes,
                        last_free_bytes,
                        &detector_failures,
                    );
                }
            }
        }

        // 3. Budget check.
        let elapsed = clock.now().duration_since(start_instant);
        if iterations_run >= config.max_iterations
            || actions_executed >= config.max_actions
            || elapsed >= config.max_duration
        {
            return build_report(
                StopReason::BudgetExceeded,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
                &detector_failures,
            );
        }

        // 4. Discover.
        observer.observe(RecoveryProgress::Discovering {
            iteration: iterations_run + 1,
        });
        let statuses = detector_registry.discover_all(discovery_ctx);
        let (candidates, failures) = candidates_with_actions(statuses, action_registry);
        for failure in failures {
            if !detector_failures.contains(&failure) {
                detector_failures.push(failure);
            }
        }
        observer.observe(RecoveryProgress::Discovered {
            iteration: iterations_run + 1,
            candidates: candidates.len() as u32,
            detectors_failed: detector_failures.len() as u32,
        });

        // 5-6. Refresh evidence, classify, select one candidate.
        observer.observe(RecoveryProgress::Revalidating {
            iteration: iterations_run + 1,
        });
        let now = wall_clock.now();
        let selection = select_candidate(Selecting {
            candidates,
            registry: action_registry,
            policy_cfg,
            collector,
            now,
            excluded: &no_retry,
            auto_approve_ask: config.auto_approve_ask,
            narrowing: budgeted.as_ref().map(|(envelope, ledger)| Narrowing {
                envelope,
                ledger,
                elapsed,
            }),
        });
        let remaining = selection.remaining;
        actions_declined_or_skipped += remaining.requires_confirmation;

        let Some(candidate) = selection.candidate else {
            // Nothing to act on. Which of the two possible reasons that is
            // matters (HORO-1510): an envelope out of allowance has not
            // discovered that the disk holds nothing safe, and must not be
            // reported as though it had.
            let stop_reason = match selection.budget_refusal {
                Some(reason) => StopReason::EnvelopeRefused(reason),
                None => StopReason::SafeExhausted(remaining),
            };
            return build_report(
                stop_reason,
                iterations_run,
                actions_executed,
                actions_declined_or_skipped,
                total_bytes_freed,
                started_free_bytes,
                last_free_bytes,
                &detector_failures,
            );
        };

        iterations_run += 1;

        let resource = candidate.evidence.resource.clone();
        let fingerprint = candidate.evidence.fingerprint.clone();
        let action_id = candidate.action_id;
        let decision_class = candidate.decision.class;
        // The canonical Display form, which is also what the audit log records
        // — so a progress line and an audit line name the same resource the
        // same way (HORO-1509 AC5).
        let resource_label = resource.to_string();
        let action_label = action_id.0.to_string();
        let estimated_bytes = candidate_bytes(&candidate.evidence);
        // Captured before `candidate.decision` is moved into `authorize`
        // below — HORO-1057's audit record needs the report-facing label
        // for whatever decision this candidate was actually authorized
        // under.
        let policy_label = label_for(&candidate.decision).as_str();

        // 7. Authorize. AutoSafe needs no consent; Ask needs a
        // UserConsent built from the exact evidence/fingerprint just
        // classified — a non-interactive auto-consent, only when
        // `auto_approve_ask` is true (see `RecoveryConfig::auto_approve_ask`
        // doc comment for why this is an MVP simplification, not real
        // interactive approval).
        let consent = if decision_class == PolicyClass::Ask {
            Some(UserConsent::new(resource.clone(), fingerprint.clone(), now))
        } else {
            None
        };
        let approval = match authorize(candidate.decision, fingerprint, consent.as_ref()) {
            Some(approval) => approval,
            None => {
                // Should not happen given how `consent` was just built to
                // match exactly, but never silently retry a candidate
                // authorize() refused.
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
                continue;
            }
        };

        // 8. Resolve the action (already resolved by `candidates_with_actions`
        // via `resolve_action_id`; re-look-up defensively rather than
        // trusting the id is still valid).
        let action = match action_registry.get(action_id.0) {
            Some(action) => action,
            None => {
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
                continue;
            }
        };

        // 8-9. Execute for real, reusing HORO-951's fully-hardened
        // revalidate-then-mutate path exactly as-is.
        observer.observe(RecoveryProgress::ActionStarted {
            iteration: iterations_run,
            resource: resource_label.clone(),
            action: action_label.clone(),
            policy_label,
            estimated_bytes,
        });
        let report = execute(action, &approval, collector, policy_cfg, now);

        // HORO-1057: best-effort audit-log append, AFTER the real outcome
        // above is already known. `append_recovery_audit_record` never
        // returns a `Result` its caller could (mis)handle — see that
        // function's doc comment for why an audit-write failure must
        // never influence this loop's own bookkeeping below.
        append_recovery_audit_record(&report, policy_label, audit_log_path, now);

        // The running total is updated before the outcome is reported, so the
        // figure in `ActionFinished` already includes this action — a UI that
        // renders the stream never shows a completed deletion beside a total
        // that has not caught up with it. `None` is what the executor could
        // not measure, and only a `Succeeded` outcome's measured bytes count
        // toward the total (HORO-1509 AC2).
        let reclaimed_bytes = match report.actual_reclaimed_bytes {
            ProbeOutcome::Observed(bytes) => Some(bytes),
            ProbeOutcome::Unavailable(_) => None,
        };

        // Charge the envelope for the action just *attempted*, per
        // `BudgetLedger::charge` — a failed attempt counts, because the action
        // budget bounds how much Autopilot does rather than how much of it
        // worked. Deliberately before the `report.outcome` match below, all of
        // whose arms `continue` or return.
        //
        // `unwrap_or(0)` on either figure is safe rather than lenient: `charge`
        // takes the larger of the two, and an automatic run cannot reach here
        // with an unknown estimate at all (`admit` refuses
        // `ReclaimSizeUnknown`), so the fallback is unreachable today and
        // charges nothing it could not measure if it ever stops being.
        if let Some((_, ledger)) = budgeted.as_mut() {
            ledger.charge(estimated_bytes.unwrap_or(0), reclaimed_bytes.unwrap_or(0));
        }

        if matches!(report.outcome, ExecutionOutcome::Succeeded) {
            if let Some(bytes) = reclaimed_bytes {
                total_bytes_freed += bytes;
            }
        }
        observer.observe(RecoveryProgress::ActionFinished {
            iteration: iterations_run,
            resource: resource_label,
            action: action_label,
            outcome: execution_outcome_tag(&report.outcome),
            reclaimed_bytes,
            bytes_freed_so_far: total_bytes_freed,
        });

        match report.outcome {
            ExecutionOutcome::Succeeded => {
                actions_executed += 1;
                let progressed = reclaimed_bytes.is_some_and(|bytes| bytes > 0);

                // 10. No-progress guard: consecutive successful executions
                // that freed nothing measurable.
                if progressed {
                    no_progress_streak = 0;
                } else {
                    no_progress_streak += 1;
                    if no_progress_streak >= NO_PROGRESS_STREAK_THRESHOLD {
                        let final_free_bytes = fs_stat
                            .stat(target_mount)
                            .map(|u| u.free_bytes)
                            .unwrap_or(last_free_bytes);
                        return build_report(
                            StopReason::NoProgress,
                            iterations_run,
                            actions_executed,
                            actions_declined_or_skipped,
                            total_bytes_freed,
                            started_free_bytes,
                            final_free_bytes,
                            &detector_failures,
                        );
                    }
                }
            }
            ExecutionOutcome::Failed(_) | ExecutionOutcome::AbortedByRevalidation(_) => {
                // 9. Never retry this exact candidate again this run.
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
                no_progress_streak = 0;
            }
            ExecutionOutcome::DryRun => {
                // execute() never returns this variant; defensively treat
                // it like a no-op rather than panicking.
                no_retry.insert(resource);
                actions_declined_or_skipped += 1;
            }
        }

        // 11. Loop back to step 1 — the top of the next iteration
        // re-measures disk usage rather than trusting expected bytes.
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::detectors::{Detector, DetectorId, DiscoveryContext};
    use crate::evidence::correlate::CorrelationResult;
    use crate::evidence::model::{
        ActionId as EvActionId, GitState, Recoverability, ResourceFingerprint, ResourceKind,
        ResourceLocator,
    };
    use crate::evidence::probe::ProbeReason;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A detector that always reports a fixed, caller-supplied
    /// [`DetectorStatus`] regardless of `ctx`. Used everywhere in this
    /// test module INSTEAD OF `DetectorRegistry::builtin()`'s real
    /// detectors: several of those (e.g. `HomebrewDetector`) shell out to
    /// a real, already-installed system tool and would discover a
    /// genuine resource on whatever machine runs the test, which combined
    /// with a real `ActionRegistry`/`executor::execute` could mutate real
    /// user state — exactly the risk `DetectorRegistry::from_detectors`
    /// exists to let tests avoid. See that constructor's doc comment.
    struct FakeDetector {
        found: Vec<Evidence>,
    }

    impl FakeDetector {
        fn found(evidence: Vec<Evidence>) -> Self {
            Self { found: evidence }
        }

        fn none() -> Self {
            Self { found: Vec::new() }
        }
    }

    impl Detector for FakeDetector {
        fn id(&self) -> DetectorId {
            DetectorId("fake_test_detector")
        }

        fn resource_kinds(&self) -> &'static [ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            // Re-checks each resource's path for existence on every call,
            // the same way a real detector's `canonicalize()` would stop
            // reporting a deleted resource — needed so a fixture actually
            // deleted by an earlier loop iteration doesn't keep getting
            // "rediscovered" with stale evidence on the next one. Only
            // ever touches paths this test itself constructed, never any
            // real machine state.
            let live: Vec<Evidence> = self
                .found
                .iter()
                .filter(|ev| match &ev.resource.locator {
                    ResourceLocator::Path(p) => p.exists(),
                    ResourceLocator::Tool { .. } => true,
                })
                .cloned()
                .collect();

            if live.is_empty() {
                DetectorStatus::ToolAbsent
            } else {
                DetectorStatus::Found(live)
            }
        }
    }

    /// A detector whose probe FAILED — the state this test module had no
    /// coverage for before HORO-1484, which is exactly why the loop could
    /// discard it silently. Reports no candidates, like
    /// `FakeDetector::none()`, so the only difference between the two is the
    /// one this ticket is about: whether the run knows it looked everywhere.
    struct FailingDetector;

    impl Detector for FailingDetector {
        fn id(&self) -> DetectorId {
            DetectorId("failing_test_detector")
        }

        fn resource_kinds(&self) -> &'static [ResourceKind] {
            &[]
        }

        fn discover(&self, _ctx: &DiscoveryContext) -> DetectorStatus {
            DetectorStatus::Failed("probe did not answer".to_string())
        }
    }

    fn fake_registry(evidence: Vec<Evidence>) -> DetectorRegistry {
        let detector: Box<dyn Detector> = if evidence.is_empty() {
            Box::new(FakeDetector::none())
        } else {
            Box::new(FakeDetector::found(evidence))
        };
        DetectorRegistry::from_detectors(vec![detector])
    }

    /// A real, disposable temp `node_modules` directory (with no files
    /// inside, so a real deletion measures zero bytes freed — see
    /// `stops_with_no_progress_after_two_zero_byte_successful_deletions`),
    /// paired with the `Evidence` a `FakeDetector` reports for it. The
    /// directory itself is real (the loop's own `executor::execute` call
    /// really deletes it, on purpose — that's what this loop is for); only
    /// *discovery* is faked.
    fn empty_node_modules_evidence(dir: &Path) -> Evidence {
        let node_modules = dir.join("node_modules");
        fs::create_dir_all(&node_modules).expect("create node_modules fixture");
        Evidence {
            resource: ResourceId::new(
                ResourceKind::NodeModules,
                ResourceLocator::Path(node_modules.clone()),
            ),
            fingerprint: ResourceFingerprint {
                dev_ino: crate::detectors::dev_ino_fingerprint(&node_modules),
                mtime: crate::detectors::probe_mtime(&node_modules)
                    .observed()
                    .copied(),
                tool_revision: None,
            },
            detector: DetectorId("fake_test_detector"),
            logical_bytes: ProbeOutcome::Observed(0),
            physical_bytes: None,
            // Post-HORO-994: `reclaimable_bytes == logical_bytes` is what a
            // real detector reports (see `detectors::discovery_evidence`'s
            // HORO-992 equivalence) AND what `executor::build_fresh_evidence`
            // now reproduces on revalidation — so a `NotAttempted` here
            // would be an artificial fixture mismatch that trips
            // `PolicyClassDowngraded` at execute-time instead of letting
            // the real (genuinely zero-byte) deletion run, which is this
            // test's actual purpose (see its own doc comment above).
            reclaimable_bytes: ProbeOutcome::Observed(0),
            reclaimable_bytes_is_lower_bound: false,
            last_modified: ProbeOutcome::Observed(SystemTime::now()),
            last_accessed: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            regenerability: ResourceKind::NodeModules.regenerability(),
            recoverability: Recoverability::RegenerableByRebuild,
            native_cleanup: NativeCleanup::Available(EvActionId("node.clean.node_modules")),
            open_by_process: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            process_cwd_match: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            git_state: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            tool_liveness: ProbeOutcome::Unavailable(ProbeReason::NotAttempted),
            collected_at: SystemTime::now(),
            sources: Vec::new(),
        }
    }

    fn make_temp_dir(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "glomeris-recovery-loop-{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A fixed disk-usage reading, injected via [`FsStat`] — never touches
    /// the real filesystem.
    struct FixedFsStat(FsUsage);

    impl FsStat for FixedFsStat {
        fn stat(&self, _path: &Path) -> std::io::Result<FsUsage> {
            Ok(self.0)
        }
    }

    /// A scripted sequence of disk-usage readings, one per [`FsStat::stat`]
    /// call, with the last reading repeated once the script runs out.
    ///
    /// [`FixedFsStat`] cannot distinguish "measures once and does arithmetic
    /// afterwards" from "re-measures every iteration", because with an
    /// unchanging reading both behave identically. A changing reading is the
    /// only way to tell them apart, and `calls` is exposed so a test can also
    /// state how many times the loop actually looked.
    struct ScriptedFsStat {
        readings: Vec<FsUsage>,
        calls: AtomicU64,
    }

    impl ScriptedFsStat {
        fn new(readings: Vec<FsUsage>) -> Self {
            assert!(!readings.is_empty(), "a script needs at least one reading");
            Self {
                readings,
                calls: AtomicU64::new(0),
            }
        }

        fn call_count(&self) -> u64 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl FsStat for ScriptedFsStat {
        fn stat(&self, _path: &Path) -> std::io::Result<FsUsage> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed) as usize;
            Ok(self.readings[n.min(self.readings.len() - 1)])
        }
    }

    /// Always errors — used to prove the `StopReason::Error` path.
    struct FailingFsStat;

    impl FsStat for FailingFsStat {
        fn stat(&self, _path: &Path) -> std::io::Result<FsUsage> {
            Err(std::io::Error::other("simulated disk measurement failure"))
        }
    }

    /// A fixed `SystemTime` for every call — sufficient here because
    /// every candidate's `Evidence::collected_at` is re-stamped to this
    /// same instant immediately before `classify` runs, so the staleness
    /// check never sees an age greater than zero regardless of how many
    /// iterations occur.
    struct FixedWallClock(SystemTime);

    impl WallClock for FixedWallClock {
        fn now(&self) -> SystemTime {
            self.0
        }
    }

    /// A collector that always reports clean, complete correlation.
    struct CleanCollector;

    impl EvidenceCollector for CleanCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(false),
            }
        }
    }

    /// A collector that reports the owning tool as live, which pins an
    /// otherwise-safe candidate to `Ask{OwningToolLive}` — a candidate the run
    /// may not touch without consent, and therefore one it leaves behind.
    struct ToolLiveCollector;

    impl EvidenceCollector for ToolLiveCollector {
        fn collect(&self, _id: &ResourceId, _budget: ProbeBudget) -> CorrelationResult {
            CorrelationResult {
                open_by_process: ProbeOutcome::Observed(Vec::new()),
                process_cwd_match: ProbeOutcome::Observed(Vec::new()),
                git_state: ProbeOutcome::Observed(None::<GitState>),
                tool_liveness: ProbeOutcome::Observed(true),
            }
        }
    }

    /// Keeps every event the loop emits, in order, so a test can state what
    /// a UI would have been able to render (HORO-1509).
    ///
    /// A `Mutex` rather than a `RefCell` because [`RecoveryObserver`] is the
    /// trait a real writing observer will implement too, and that one is
    /// shared across threads; a test fake that could not be is a fake of a
    /// different trait.
    #[derive(Default)]
    struct RecordingObserver {
        events: std::sync::Mutex<Vec<RecoveryProgress>>,
    }

    impl RecordingObserver {
        fn events(&self) -> Vec<RecoveryProgress> {
            self.events
                .lock()
                .expect("no test panics while holding this")
                .clone()
        }

        /// The stream's shape, which is what a progress UI's sequencing
        /// depends on. Deliberately not `Debug` output: that carries paths and
        /// byte counts, so a change to either would break a test about order.
        fn phases(&self) -> Vec<&'static str> {
            self.events().iter().map(stream_phase).collect()
        }
    }

    /// A stop signal that stays inert for its first `quiet_probes` answers and
    /// asks for a stop from then on — the shape of a user pressing the button
    /// while a run is already under way.
    ///
    /// Counting probes rather than wall time is what makes the test
    /// deterministic: the loop asks exactly once per iteration, so
    /// `quiet_probes: 1` means "stop is requested during iteration 1's action,
    /// and the loop finds out when iteration 2 begins".
    struct StopsAfterProbes {
        quiet_probes: u64,
        probes: AtomicU64,
    }

    impl StopsAfterProbes {
        fn quiet_for(quiet_probes: u64) -> Self {
            Self {
                quiet_probes,
                probes: AtomicU64::new(0),
            }
        }

        fn probe_count(&self) -> u64 {
            self.probes.load(Ordering::Relaxed)
        }
    }

    impl StopSignal for StopsAfterProbes {
        fn stop_requested(&self) -> bool {
            self.probes.fetch_add(1, Ordering::Relaxed) >= self.quiet_probes
        }
    }

    /// This test module's own label for one event, used only to talk about
    /// ordering. It is NOT the wire tag — the JSON `phase` values belong to
    /// the reporting DTO and are pinned where that DTO lives.
    fn stream_phase(event: &RecoveryProgress) -> &'static str {
        match event {
            RecoveryProgress::Measured { .. } => "measured",
            RecoveryProgress::Discovering { .. } => "discovering",
            RecoveryProgress::Discovered { .. } => "discovered",
            RecoveryProgress::Revalidating { .. } => "revalidating",
            RecoveryProgress::ActionStarted { .. } => "action_started",
            RecoveryProgress::ActionFinished { .. } => "action_finished",
            RecoveryProgress::StopRequested { .. } => "stop_requested",
        }
    }

    impl RecoveryObserver for RecordingObserver {
        fn observe(&self, event: RecoveryProgress) {
            self.events
                .lock()
                .expect("no test panics while holding this")
                .push(event);
        }
    }

    fn base_config() -> RecoveryConfig {
        RecoveryConfig {
            target: FreeTarget::AbsoluteBytes(999_999_999),
            max_iterations: 50,
            max_actions: 50,
            max_duration: Duration::from_secs(60),
            auto_approve_ask: false,
        }
    }

    fn empty_discovery_ctx() -> DiscoveryContext {
        DiscoveryContext::new(PathBuf::from("/nonexistent-glomeris-test-home"))
    }

    #[test]
    fn stops_with_target_reached_before_scanning_when_already_met() {
        let usage = FsUsage::new(1_000, 990); // 99% free
        let config = RecoveryConfig {
            target: FreeTarget::Percentage(90.0),
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);
        assert_eq!(report.iterations_run, 0);
        assert_eq!(report.actions_executed, 0);
        assert_eq!(report.started_free_bytes, 990);
        assert_eq!(report.final_free_bytes, 990);
    }

    #[test]
    fn stops_with_safe_exhausted_when_no_candidates_exist() {
        let usage = FsUsage::new(1_000_000_000, 100); // far from any target
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        // No detector found anything at all, so there is genuinely nothing
        // left behind — and the report says so with zeros rather than by
        // omitting the breakdown, which would read the same as "we did not
        // look" (HORO-1509).
        assert_eq!(
            report.stop_reason,
            StopReason::SafeExhausted(RemainingCandidates::default())
        );
        assert_eq!(report.iterations_run, 0);
        assert_eq!(report.actions_executed, 0);
        // The anti-vacuity half of the HORO-1484 test below: a run in which
        // every detector answered must report no caveat at all, so the
        // assertions there cannot be satisfied by a renderer that always
        // qualifies itself.
        assert!(
            report.detector_failures.is_empty(),
            "no detector failed in this run; got {:?}",
            report.detector_failures
        );
        assert!(report.discovery_caveat_lines().is_empty());
    }

    /// HORO-1484: `SafeExhausted` reached without having looked everywhere is
    /// not the finding "nothing safe is left", and the report must say so.
    ///
    /// The registry here holds one detector that fails and one that reports
    /// nothing, so the run reaches the same `SafeExhausted` stop reason as
    /// `stops_with_safe_exhausted_when_no_candidates_exist` above by a
    /// materially different route. Before the fix the two runs produced
    /// byte-identical reports: the failure was matched into the same
    /// do-nothing arm as `ToolAbsent` and never reached `RecoveryReport`.
    #[test]
    fn safe_exhausted_after_a_failed_detector_withdraws_its_own_claim() {
        let usage = FsUsage::new(1_000_000_000, 100); // far from any target
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = DetectorRegistry::from_detectors(vec![
            Box::new(FailingDetector),
            Box::new(FakeDetector::none()),
        ]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        // Zeros here too — and that is exactly why they are not the whole
        // answer. The detector that failed might have found anything; the
        // breakdown reports what was *seen*, and `detector_failures` below
        // reports the part of the disk nobody looked at (HORO-1484).
        assert_eq!(
            report.stop_reason,
            StopReason::SafeExhausted(RemainingCandidates::default())
        );
        assert_eq!(
            report.detector_failures,
            vec!["failing_test_detector: probe did not answer".to_string()],
            "the failed detector must be named in the report; the detector that \
             merely found nothing must not be"
        );

        let caveat = report.discovery_caveat_lines().join("\n");
        assert!(
            caveat.contains("discovery incomplete"),
            "the rendered report must state that discovery was incomplete; got:\n{caveat}"
        );
        assert!(
            caveat.contains("failing_test_detector: probe did not answer"),
            "the rendered report must name the detector and its reason; got:\n{caveat}"
        );
        assert!(
            caveat.contains("not a finding that nothing safe is left"),
            "reaching SafeExhausted without having looked everywhere must be \
             explicitly withdrawn as a finding; got:\n{caveat}"
        );
    }

    /// HORO-1509: "nothing safe is left" must arrive with what *is* left, so
    /// a user who is told the run stopped early learns what to do next.
    ///
    /// The two candidates here need opposite things — one is waiting for the
    /// user's consent, the other will never be offered at all — and a report
    /// that summed them into "2 candidates remain" would be describing a
    /// single next step that does not exist. Neither is ever executed (`Ask`
    /// without `auto_approve_ask`, and `Protected`), so both fixtures must
    /// survive the run; that assertion is also what proves the breakdown is
    /// about things still there rather than things already taken.
    ///
    /// Both are `node_modules` because that is one of the three kinds with a
    /// registered built-in action: a candidate with no resolvable action is
    /// dropped before classification (see `candidates_with_actions`), so a
    /// fixture of some actionless kind would have produced an all-zero
    /// breakdown that passed for the wrong reason. The protected one is
    /// protected by its path — a `.git` component, which `protected_reason`
    /// answers without any filesystem I/O — rather than by its evidence.
    #[test]
    fn safe_exhausted_names_what_it_left_behind() {
        let dir = make_temp_dir("remaining-breakdown");
        let consent_parent = dir.join("live-project");
        let protected_parent = dir.join(".git");
        let waiting_for_consent = consent_parent.join("node_modules");
        let never_offered = protected_parent.join("node_modules");

        let usage = FsUsage::new(1_000_000_000, 100); // far from any target
        let config = base_config(); // auto_approve_ask: false
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(vec![
            empty_node_modules_evidence(&consent_parent),
            empty_node_modules_evidence(&protected_parent),
        ]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            // A live owning tool is what pins the first candidate to `Ask`
            // rather than `AutoSafe`. It has no bearing on the second: a
            // protected path is refused before any evidence is weighed.
            collector: &ToolLiveCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(
            report.stop_reason,
            StopReason::SafeExhausted(RemainingCandidates {
                requires_confirmation: 1,
                protected: 1,
                not_executable: 0,
                not_permitted_by_autopilot: 0,
            }),
            "the stop reason must carry the breakdown, not merely the word"
        );
        assert_eq!(report.actions_executed, 0);
        assert_eq!(
            report.actions_declined_or_skipped, 1,
            "only the consent-gated candidate is a skip; a protected resource was \
             never a candidate for this run to decline"
        );
        assert!(
            waiting_for_consent.exists() && never_offered.exists(),
            "nothing was authorised, so nothing may have been deleted"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A detector that fails on every iteration is reported once, not once
    /// per iteration: `detector_failures` is deduplicated in first-seen
    /// order. Uses a budget of three iterations with a candidate that really
    /// exists, so discovery genuinely runs more than once.
    #[test]
    fn a_detector_that_keeps_failing_is_reported_once() {
        let dir = make_temp_dir("repeated-failure");
        let evidence = empty_node_modules_evidence(&dir);
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = DetectorRegistry::from_detectors(vec![
            Box::new(FailingDetector),
            Box::new(FakeDetector::found(vec![evidence])),
        ]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        assert!(
            report.iterations_run >= 1,
            "discovery must have run at least once for this test to mean anything"
        );
        assert_eq!(
            report.detector_failures.len(),
            1,
            "one failing detector across {} iteration(s) is one line, not {}: {:?}",
            report.iterations_run,
            report.detector_failures.len(),
            report.detector_failures
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stops_with_budget_exceeded_when_max_iterations_is_zero() {
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = RecoveryConfig {
            max_iterations: 0,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::BudgetExceeded);
        assert_eq!(report.iterations_run, 0);
    }

    #[test]
    fn stops_with_error_when_initial_measurement_fails() {
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let detector_registry = fake_registry(Vec::new());
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FailingFsStat,
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        match report.stop_reason {
            StopReason::Error(_) => {}
            other => panic!("expected StopReason::Error, got {other:?}"),
        }
    }

    /// Two real, disposable temp `node_modules` directories with no files
    /// inside, reported by a [`FakeDetector`] (never `DetectorRegistry::
    /// builtin()`'s real detectors — see that struct's doc comment for
    /// why). `reclaimable_bytes` matches `logical_bytes` here (post-HORO-994
    /// this is also what `executor::build_fresh_evidence` reproduces on
    /// revalidation, so plan-time and execute-time classification agree —
    /// see `empty_node_modules_evidence`), so the resulting evidence
    /// classifies as `AutoSafe` and is executed directly; `auto_approve_ask:
    /// true` in this test's config is inert for this fixture, kept only to
    /// match this suite's other `base_config()` overrides. Each directory
    /// being empty means `NodeCleanNodeModules`'s real deletion measures
    /// `actual_reclaimed_bytes == Observed(0)` — "succeeded but freed
    /// nothing measurable" — twice in a row, exactly what the no-progress
    /// guard exists to catch.
    #[test]
    fn stops_with_no_progress_after_two_zero_byte_successful_deletions() {
        let root_a = make_temp_dir("no-progress-a");
        let root_b = make_temp_dir("no-progress-b");
        let ev_a = empty_node_modules_evidence(&root_a);
        let ev_b = empty_node_modules_evidence(&root_b);
        let node_modules_a = root_a.join("node_modules");
        let node_modules_b = root_b.join("node_modules");

        let usage = FsUsage::new(1_000_000_000, 100); // never meets target
        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev_a, ev_b]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root_a.join("actions.jsonl");

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::NoProgress);
        assert_eq!(report.iterations_run, 2);
        assert_eq!(report.actions_executed, 2);
        assert_eq!(report.total_bytes_freed, 0);
        assert!(!node_modules_a.exists());
        assert!(!node_modules_b.exists());

        // HORO-1057 AC: the `free` real-execution path appends one audit
        // record per executed candidate.
        let audit_tail = crate::monitor::read_audit_tail(&audit_log_path, 10);
        assert_eq!(
            audit_tail.len(),
            2,
            "expected one audit record per executed action"
        );
        assert!(audit_tail.iter().all(|r| r.source == "free"));
        assert!(audit_tail.iter().all(|r| r.outcome == "succeeded"));

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    /// The target is decided by a reading taken after the mutation, not by the
    /// loop's own arithmetic (HORO-1506 AC 6, the campaign's §9).
    ///
    /// The script makes the two answers disagree on purpose. The deleted
    /// directory is empty, so the executor measures `Observed(0)` reclaimed and
    /// `total_bytes_freed` stays at zero; started free space was 100 bytes;
    /// `started + freed` is therefore 100, which does not come close to the
    /// 900 MB target. Only the third reading does. A loop that tracked free
    /// space by adding up what it believed it had reclaimed — or that trusted
    /// candidate estimates — would run until some other stop reason ended it
    /// and could never report `TargetReached` here.
    ///
    /// It is deliberately the *first* two readings that are low: one is
    /// consumed by the initial measurement before the loop starts, and the
    /// second by iteration 1's own step 1. The third is the post-mutation
    /// re-measure at the top of iteration 2.
    #[test]
    fn target_reached_comes_from_the_reading_taken_after_the_mutation() {
        let root = make_temp_dir("remeasure-after-mutation");
        let ev = empty_node_modules_evidence(&root);
        let node_modules = root.join("node_modules");

        let fs_stat = ScriptedFsStat::new(vec![
            FsUsage::new(1_000_000_000, 100),
            FsUsage::new(1_000_000_000, 100),
            FsUsage::new(1_000_000_000, 950_000_000),
        ]);
        let config = RecoveryConfig {
            target: FreeTarget::AbsoluteBytes(900_000_000),
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root.join("actions.jsonl");

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &fs_stat,
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);
        assert_eq!(report.iterations_run, 1);
        assert_eq!(report.actions_executed, 1);
        assert!(!node_modules.exists(), "the mutation really happened");

        // The two figures the verdict could have come from, and which of them
        // it did come from.
        assert_eq!(
            report.total_bytes_freed, 0,
            "nothing measurable was reclaimed, so no accumulator could have met the target"
        );
        assert_eq!(report.started_free_bytes, 100);
        assert_eq!(report.final_free_bytes, 950_000_000);
        assert!(
            !target_met(
                &FsUsage::new(
                    1_000_000_000,
                    report.started_free_bytes + report.total_bytes_freed
                ),
                &config.target
            ),
            "the arithmetic route to a free-space figure does not meet this target; \
             only a fresh reading does"
        );
        assert!(
            fs_stat.call_count() >= 3,
            "the loop must measure again after mutating, not once at the start \
             (measured {} time(s))",
            fs_stat.call_count()
        );

        fs::remove_dir_all(&root).ok();
    }

    /// HORO-1509 AC1: every iteration of a run can be reconstructed from what
    /// the loop emitted, without reading the loop's own final summary.
    ///
    /// The fixture is the two-empty-`node_modules` no-progress run, chosen
    /// because it executes twice — one iteration could not tell an ordinal
    /// that is always `1` from one that counts.
    #[test]
    fn every_iteration_of_a_run_is_visible_in_the_event_stream() {
        let root_a = make_temp_dir("stream-iterations-a");
        let root_b = make_temp_dir("stream-iterations-b");
        let ev_a = empty_node_modules_evidence(&root_a);
        let ev_b = empty_node_modules_evidence(&root_b);

        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev_a, ev_b]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root_a.join("actions.jsonl");
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(FsUsage::new(1_000_000_000, 100)),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &observer,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.iterations_run, 2);
        assert_eq!(
            observer.phases(),
            vec![
                "measured",
                "discovering",
                "discovered",
                "revalidating",
                "action_started",
                "action_finished",
                "measured",
                "discovering",
                "discovered",
                "revalidating",
                "action_started",
                "action_finished",
            ],
            "each iteration must report measuring, scanning, revalidating and \
             its action, in that order"
        );

        // The ordinal a UI renders as "iteration N of at most M". Every event
        // in the first six belongs to iteration 1 and every event in the
        // second six to iteration 2 — including the measurement, which is
        // taken *for* the iteration it opens.
        let ordinals: Vec<u32> = observer
            .events()
            .iter()
            .map(|event| match event {
                RecoveryProgress::Measured { iteration, .. }
                | RecoveryProgress::Discovering { iteration }
                | RecoveryProgress::Discovered { iteration, .. }
                | RecoveryProgress::Revalidating { iteration }
                | RecoveryProgress::ActionStarted { iteration, .. }
                | RecoveryProgress::ActionFinished { iteration, .. }
                | RecoveryProgress::StopRequested { iteration } => *iteration,
            })
            .collect();
        assert_eq!(ordinals, vec![1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2]);

        // What a UI needs to name the work in flight, rather than a spinner.
        let started: Vec<(String, String)> = observer
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryProgress::ActionStarted {
                    resource, action, ..
                } => Some((resource.clone(), action.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 2);
        assert!(started
            .iter()
            .all(|(_, action)| action == "node.clean.node_modules"));
        assert_ne!(
            started[0].0, started[1].0,
            "two iterations acted on two different resources, and the stream \
             must say which"
        );

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    /// HORO-1509 AC5: the progress stream and the local audit log describe the
    /// same actions with the same words — which they do because
    /// [`execution_outcome_tag`] and `ResourceId`'s `Display` each have one
    /// producer, not because two `match` arms happen to agree.
    #[test]
    fn the_stream_and_the_audit_log_describe_the_same_actions() {
        let root_a = make_temp_dir("stream-audit-agree-a");
        let root_b = make_temp_dir("stream-audit-agree-b");
        let ev_a = empty_node_modules_evidence(&root_a);
        let ev_b = empty_node_modules_evidence(&root_b);

        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev_a, ev_b]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root_a.join("actions.jsonl");
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(FsUsage::new(1_000_000_000, 100)),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &observer,
            stop: &NeverStops,
            admission: None,
        });

        let finished: Vec<(String, String, &'static str, Option<u64>)> = observer
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryProgress::ActionFinished {
                    resource,
                    action,
                    outcome,
                    reclaimed_bytes,
                    ..
                } => Some((resource.clone(), action.clone(), *outcome, *reclaimed_bytes)),
                _ => None,
            })
            .collect();
        let audit_tail = crate::monitor::read_audit_tail(&audit_log_path, 10);

        assert_eq!(
            finished.len(),
            audit_tail.len(),
            "one progress event per audit record"
        );
        assert_eq!(finished.len(), report.actions_executed as usize);
        for (event, record) in finished.iter().zip(audit_tail.iter()) {
            assert_eq!(event.0, record.resource_id);
            assert_eq!(event.1, record.action_id);
            assert_eq!(event.2, record.outcome);
            assert_eq!(event.3, record.actual_reclaimed_bytes);
        }

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    /// HORO-1509 AC2: the free-space figures in the stream are readings, not
    /// arithmetic. The same scripted disagreement
    /// `target_reached_comes_from_the_reading_taken_after_the_mutation` uses,
    /// read through the stream this time: the loop deletes an empty directory
    /// (measuring zero bytes reclaimed) and free space nevertheless jumps to
    /// 950 MB, which no accumulator could have produced.
    #[test]
    fn free_space_in_the_stream_comes_from_readings_not_arithmetic() {
        let root = make_temp_dir("stream-measured-not-predicted");
        let ev = empty_node_modules_evidence(&root);

        let fs_stat = ScriptedFsStat::new(vec![
            FsUsage::new(1_000_000_000, 100),
            FsUsage::new(1_000_000_000, 100),
            FsUsage::new(1_000_000_000, 950_000_000),
        ]);
        let config = RecoveryConfig {
            target: FreeTarget::AbsoluteBytes(900_000_000),
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root.join("actions.jsonl");
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &fs_stat,
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &observer,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);

        let measurements: Vec<(u32, u64, u64)> = observer
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryProgress::Measured {
                    iteration,
                    usage,
                    bytes_freed_so_far,
                } => Some((*iteration, usage.free_bytes, *bytes_freed_so_far)),
                _ => None,
            })
            .collect();
        assert_eq!(
            measurements,
            vec![(1, 100, 0), (2, 950_000_000, 0)],
            "the stream must carry the readings the loop took, and must not \
             infer free space from a running total that stayed at zero"
        );

        // The same event carries both figures, and they are different facts:
        // `bytes_freed_so_far` is what was measured as reclaimed (nothing),
        // while free space moved for reasons outside this run. A UI that
        // rendered one as the other would be wrong in both directions here.
        let estimates: Vec<Option<u64>> = observer
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryProgress::ActionStarted {
                    estimated_bytes, ..
                } => Some(*estimated_bytes),
                _ => None,
            })
            .collect();
        assert_eq!(
            estimates,
            vec![Some(0)],
            "a plan-time estimate is reported as its own field, never folded \
             into the measured total"
        );
        assert_eq!(report.total_bytes_freed, 0);

        fs::remove_dir_all(&root).ok();
    }

    /// The running total in the stream already includes the action being
    /// reported, so a UI never renders a finished deletion beside a total that
    /// has not caught up with it (HORO-1509 AC2), and the last figure the
    /// stream carries is the one the final summary reports.
    #[test]
    fn the_running_total_never_lags_behind_the_action_it_reports() {
        let root = make_temp_dir("stream-total-keeps-up");
        let ev = empty_node_modules_evidence(&root);

        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root.join("actions.jsonl");
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(FsUsage::new(1_000_000_000, 100)),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &observer,
            stop: &NeverStops,
            admission: None,
        });

        let totals: Vec<u64> = observer
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryProgress::Measured {
                    bytes_freed_so_far, ..
                }
                | RecoveryProgress::ActionFinished {
                    bytes_freed_so_far, ..
                } => Some(*bytes_freed_so_far),
                _ => None,
            })
            .collect();

        assert!(!totals.is_empty());
        assert!(
            totals.windows(2).all(|pair| pair[0] <= pair[1]),
            "the reclaimed total can only grow: {totals:?}"
        );
        assert_eq!(
            *totals.last().expect("at least one figure"),
            report.total_bytes_freed,
            "the last figure the stream carried and the figure the run reports \
             must be the same number"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// A run that stops before it scans anything still tells a UI the one
    /// thing it knows: what the disk currently looks like. No phantom
    /// discovery or action phase appears (HORO-1509 AC1).
    #[test]
    fn a_run_that_stops_before_scanning_still_reports_its_measurement() {
        let usage = FsUsage::new(1_000, 990); // 99% free
        let config = RecoveryConfig {
            target: FreeTarget::Percentage(90.0),
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(Vec::new());
        let ctx = empty_discovery_ctx();
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &observer,
            stop: &NeverStops,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);
        assert_eq!(report.iterations_run, 0);
        assert_eq!(observer.phases(), vec!["measured"]);
        assert_eq!(
            observer.events(),
            vec![RecoveryProgress::Measured {
                iteration: 1,
                usage,
                bytes_freed_so_far: 0,
            }],
        );
    }

    /// HORO-1509 AC4: "stop after the current action" means exactly that. The
    /// action that was running when the user asked finishes, the next one
    /// never starts, and the run says the user stopped it rather than
    /// inventing a reason of its own.
    ///
    /// Two fixtures, one stop. The loop asks for a stop once per iteration, so
    /// a signal that stays quiet for one probe is requested while iteration
    /// 1's deletion is under way: the first directory must be gone, the second
    /// must still be there.
    #[test]
    fn a_run_asked_to_stop_finishes_its_action_and_starts_no_other() {
        let root_a = make_temp_dir("stop-after-current-a");
        let root_b = make_temp_dir("stop-after-current-b");
        let ev_a = empty_node_modules_evidence(&root_a);
        let ev_b = empty_node_modules_evidence(&root_b);
        let node_modules_a = root_a.join("node_modules");
        let node_modules_b = root_b.join("node_modules");

        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev_a, ev_b]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root_a.join("actions.jsonl");
        let observer = RecordingObserver::default();
        let stop = StopsAfterProbes::quiet_for(1);

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(FsUsage::new(1_000_000_000, 100)),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &observer,
            stop: &stop,
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::StoppedByUser);
        assert_eq!(report.iterations_run, 1);
        assert_eq!(report.actions_executed, 1);
        assert_eq!(stop.probe_count(), 2, "asked once per iteration");
        // Which of two equally-sized candidates the ranking picked first is
        // not this test's business; that exactly one of them survived is.
        let survivors = [&node_modules_a, &node_modules_b]
            .iter()
            .filter(|path| path.exists())
            .count();
        assert_eq!(
            survivors, 1,
            "the action already running when the stop arrived must have \
             finished, and no further action may start"
        );

        // The stop is announced, and it is announced where it happened:
        // after iteration 1's action, at the top of what would have been
        // iteration 2.
        assert_eq!(
            observer.phases(),
            vec![
                "measured",
                "discovering",
                "discovered",
                "revalidating",
                "action_started",
                "action_finished",
                "measured",
                "stop_requested",
            ]
        );
        assert!(observer
            .events()
            .contains(&RecoveryProgress::StopRequested { iteration: 2 }));

        // One executed action, one audit record: a stopped run's trail is as
        // complete as any other run's.
        let audit_tail = crate::monitor::read_audit_tail(&audit_log_path, 10);
        assert_eq!(audit_tail.len(), 1);
        assert_eq!(audit_tail[0].outcome, "succeeded");

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    /// A stop that arrives before the run does anything ends it without
    /// touching a single resource — and the run reports that honestly rather
    /// than as an exhausted search (HORO-1509 AC4).
    #[test]
    fn a_stop_requested_before_any_action_reclaims_nothing() {
        let root = make_temp_dir("stop-before-anything");
        let ev = empty_node_modules_evidence(&root);
        let node_modules = root.join("node_modules");

        let config = RecoveryConfig {
            auto_approve_ask: true,
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::now());
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(vec![ev]);
        let ctx = empty_discovery_ctx();
        let audit_log_path = root.join("actions.jsonl");
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(FsUsage::new(1_000_000_000, 100)),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &audit_log_path,
            observer: &observer,
            stop: &StopsAfterProbes::quiet_for(0),
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::StoppedByUser);
        assert_eq!(report.iterations_run, 0);
        assert_eq!(report.actions_executed, 0);
        assert_eq!(report.total_bytes_freed, 0);
        assert!(node_modules.exists(), "nothing was deleted");
        assert_eq!(observer.phases(), vec!["measured", "stop_requested"]);
        assert!(
            !audit_log_path.exists(),
            "a run that executed nothing writes no audit record"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// A stop and a reached goal in the same breath: the goal wins, because
    /// that is the outcome the user actually asked for and got. The stop is
    /// not discarded — there was simply nothing left for it to prevent.
    #[test]
    fn a_stop_does_not_mask_a_goal_that_was_already_reached() {
        let config = RecoveryConfig {
            target: FreeTarget::Percentage(90.0),
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let detector_registry = fake_registry(Vec::new());
        let ctx = empty_discovery_ctx();
        let observer = RecordingObserver::default();

        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(FsUsage::new(1_000, 990)), // 99% free
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &observer,
            stop: &StopsAfterProbes::quiet_for(0),
            admission: None,
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);
        assert_eq!(observer.phases(), vec!["measured"]);
    }

    /// A candidate neither probe could size is reported as unknown, not as
    /// zero: "this would free nothing" and "nobody could tell" are different
    /// facts, and only one of them is an argument against acting
    /// (HORO-1509 AC2).
    #[test]
    fn a_candidate_no_probe_could_size_reports_unknown_rather_than_zero() {
        let root = make_temp_dir("unsized-candidate");
        let mut ev = empty_node_modules_evidence(&root);

        assert_eq!(
            candidate_bytes(&ev),
            Some(0),
            "a candidate a probe measured at zero really is zero"
        );

        ev.reclaimable_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);
        ev.logical_bytes = ProbeOutcome::Unavailable(ProbeReason::NotAttempted);

        assert_eq!(candidate_bytes(&ev), None);
        assert_eq!(
            candidate_size(&ev),
            0,
            "the ranking helper still needs a number, and that is precisely \
             why the reporting helper must not use it"
        );

        fs::remove_dir_all(&root).ok();
    }

    // ----------------------------------------------------------------------
    // HORO-1510: the same loop, made *automatic* by a `RecoveryAdmission`.
    //
    // Every test above passes `admission: None`, so the human-started run is
    // already pinned. These are about what the envelope adds — and the one
    // claim they exist to hold is the §7 one: an envelope that runs out of
    // allowance has not discovered that the disk holds nothing safe, and must
    // not report as though it had.
    // ----------------------------------------------------------------------

    /// Like [`empty_node_modules_evidence`] but with `bytes` of real content
    /// inside, because several of the envelope's budgets are *about* size: a
    /// zero-byte fixture can never be refused by a byte budget, so a test
    /// built on one would pass without the budget ever being consulted.
    ///
    /// The reported figures must equal what `executor::build_fresh_evidence`
    /// recomputes from the same tree at revalidation time, or the run aborts
    /// on a byte-magnitude mismatch instead of executing — see
    /// [`empty_node_modules_evidence`]'s note on that same equivalence. The
    /// fingerprint is re-probed for the same reason: writing into the tree
    /// moves `node_modules`' own mtime.
    fn sized_node_modules_evidence(dir: &Path, bytes: u64) -> Evidence {
        let mut ev = empty_node_modules_evidence(dir);
        let node_modules = dir.join("node_modules");
        let pkg = node_modules.join("pkg");
        fs::create_dir_all(&pkg).expect("create pkg fixture");
        fs::write(pkg.join("index.js"), vec![0u8; bytes as usize]).expect("write fixture bytes");
        ev.logical_bytes = ProbeOutcome::Observed(bytes);
        ev.reclaimable_bytes = ProbeOutcome::Observed(bytes);
        ev.fingerprint.mtime = crate::detectors::probe_mtime(&node_modules)
            .observed()
            .copied();
        ev
    }

    /// An enabled envelope allowing `NodeModules` (the kind every fixture in
    /// this module is) with every budget at its ceiling, so that whatever a
    /// test then narrows is unambiguously what refused.
    fn wide_node_modules_envelope() -> AutopilotEnvelope {
        let mut envelope = AutopilotEnvelope::revoked();
        envelope.enable();
        envelope
            .allow_kind(ResourceKind::NodeModules)
            .expect("kind is allowlistable");
        envelope
            .set_max_actions(crate::autopilot::envelope::ACTIONS_CEILING)
            .expect("the ceiling is within the ceiling");
        envelope
            .set_max_bytes(crate::autopilot::envelope::BYTES_CEILING)
            .expect("the ceiling is within the ceiling");
        envelope
            .set_max_duration(crate::autopilot::envelope::DURATION_CEILING)
            .expect("the ceiling is within the ceiling");
        envelope
    }

    /// A revoked envelope stops the run before it discovers anything, and
    /// says whose decision that was.
    ///
    /// `is_enabled()` is re-read at the top of every iteration rather than
    /// only inside `admit`, and this is the case that needs it: a run whose
    /// discovery came back empty would otherwise reach `SafeExhausted` and
    /// report a finding about the disk on the strength of an envelope that had
    /// been switched off.
    #[test]
    fn a_revoked_envelope_stops_the_run_before_discovery() {
        let dir = make_temp_dir("autopilot-revoked");
        let evidence = sized_node_modules_evidence(&dir, 16_384);
        let survivor = dir.join("node_modules");
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();
        let observer = RecordingObserver::default();

        // Enabled, then revoked: the budgets and the allowlist are all still
        // wide, so the enable bit is the only thing that could refuse.
        let mut envelope = wide_node_modules_envelope();
        envelope.revoke();

        let detector_registry = fake_registry(vec![evidence]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &observer,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: Some(PressureState::Emergency),
            }),
        });

        assert_eq!(
            report.stop_reason,
            StopReason::EnvelopeRefused(RefusalReason::AutopilotRevoked)
        );
        assert_eq!(report.iterations_run, 0);
        assert_eq!(report.actions_executed, 0);
        assert_eq!(
            observer.phases(),
            vec!["measured"],
            "the refusal is a precondition, so the run must not have gone looking"
        );
        assert!(
            survivor.exists(),
            "nothing was authorised, so nothing may have been deleted"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// Too little disk pressure refuses the run as a whole, before discovery
    /// — it is a property of the machine, not of any candidate.
    #[test]
    fn an_envelope_pressure_floor_the_machine_does_not_meet_refuses_the_run() {
        let dir = make_temp_dir("autopilot-pressure-low");
        let evidence = sized_node_modules_evidence(&dir, 16_384);
        let survivor = dir.join("node_modules");
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();
        let observer = RecordingObserver::default();

        let mut envelope = wide_node_modules_envelope();
        envelope.set_min_pressure(Some(PressureState::Critical));

        let detector_registry = fake_registry(vec![evidence]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &observer,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: Some(PressureState::Warn),
            }),
        });

        assert_eq!(
            report.stop_reason,
            StopReason::EnvelopeRefused(RefusalReason::DiskPressureTooLow {
                required: PressureState::Critical,
                observed: Some(PressureState::Warn),
            })
        );
        assert_eq!(
            observer.phases(),
            vec!["measured"],
            "checked once for the run, not once per candidate"
        );
        assert!(survivor.exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// Unmeasured pressure fails closed: an envelope with a floor refuses a
    /// run that cannot say where the machine is relative to it. "We could not
    /// tell" is not "it is fine".
    #[test]
    fn an_unobserved_pressure_state_fails_closed_against_a_floor() {
        let dir = make_temp_dir("autopilot-pressure-unknown");
        let evidence = sized_node_modules_evidence(&dir, 16_384);
        let survivor = dir.join("node_modules");
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let mut envelope = wide_node_modules_envelope();
        envelope.set_min_pressure(Some(PressureState::Warn));

        let detector_registry = fake_registry(vec![evidence]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: None,
            }),
        });

        assert_eq!(
            report.stop_reason,
            StopReason::EnvelopeRefused(RefusalReason::DiskPressureTooLow {
                required: PressureState::Warn,
                observed: None,
            })
        );
        assert!(survivor.exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// A goal already met is `TargetReached` even under a revoked envelope.
    /// Autopilot being switched off is not a finding about the disk either —
    /// and the user asking "am I there?" gets the same answer whoever asked.
    #[test]
    fn an_already_met_goal_reports_target_reached_even_under_a_revoked_envelope() {
        let usage = FsUsage::new(1_000, 990); // 99% free
        let config = RecoveryConfig {
            target: FreeTarget::Percentage(90.0),
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();
        let envelope = AutopilotEnvelope::revoked();

        let detector_registry = fake_registry(Vec::new());
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: None,
            }),
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);
    }

    /// A user stop outranks an envelope refusal. The run stopped because
    /// somebody asked it to; it did not stop because Autopilot was off.
    #[test]
    fn a_user_stop_outranks_an_envelope_refusal() {
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();
        let envelope = AutopilotEnvelope::revoked();
        let stop = StopsAfterProbes::quiet_for(0);

        let detector_registry = fake_registry(Vec::new());
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: Path::new("/nonexistent-glomeris-recovery-audit-test/actions.jsonl"),
            observer: &SilentObserver,
            stop: &stop,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: None,
            }),
        });

        assert_eq!(report.stop_reason, StopReason::StoppedByUser);
        assert!(
            stop.probe_count() >= 1,
            "fixture invalid unless the loop actually asked"
        );
    }

    /// THE HORO-1510 test (campaign §7). Two real candidates, an envelope
    /// that permits one action: the run does one, then stops — and it reports
    /// that Autopilot reached its limit, NOT that the disk has nothing safe
    /// left on it. The second candidate is still there, still safe, and still
    /// available to a recovery the user starts themselves.
    ///
    /// Paired with
    /// `the_same_two_candidates_run_to_exhaustion_under_a_wide_envelope`
    /// below, which is byte-for-byte the same fixture with the action budget
    /// widened. Together they are the anti-vacuity pair: the *only*
    /// difference between the two runs is `set_max_actions`, so nothing else
    /// can be what produced the different stop reason.
    #[test]
    fn an_exhausted_envelope_action_budget_is_not_reported_as_an_exhausted_disk() {
        let dir = make_temp_dir("autopilot-action-budget");
        let big_dir = dir.join("big");
        let small_dir = dir.join("small");
        let big = sized_node_modules_evidence(&big_dir, 16_384);
        let small = sized_node_modules_evidence(&small_dir, 1_024);
        let usage = FsUsage::new(1_000_000_000, 100); // far from any target
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();

        let mut envelope = wide_node_modules_envelope();
        envelope.set_max_actions(1).expect("under the ceiling");

        let detector_registry = fake_registry(vec![big, small]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: Some(PressureState::Critical),
            }),
        });

        assert_eq!(
            report.stop_reason,
            StopReason::EnvelopeRefused(RefusalReason::ActionBudgetExhausted { max_actions: 1 })
        );
        // Stated separately and negatively, because this is the specific
        // wrong answer the variant exists to prevent — a user told
        // "nothing safe left" would stop looking.
        assert!(
            !matches!(report.stop_reason, StopReason::SafeExhausted(_)),
            "an envelope out of allowance has not established anything about this disk"
        );
        assert_eq!(report.actions_executed, 1);
        assert_eq!(
            report.total_bytes_freed, 16_384,
            "the largest candidate is the one an action budget of 1 should have spent \
             itself on"
        );
        assert!(
            !big_dir.join("node_modules").exists(),
            "the one authorised action must really have run"
        );
        assert!(
            small_dir.join("node_modules").exists(),
            "the candidate the budget refused must still be there — that is what \
             makes reporting it as SafeExhausted a lie"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The other half of the pair above: same two candidates, same collector,
    /// same disk, envelope budgets at their ceilings. The run clears both and
    /// reaches a genuine `SafeExhausted` — so `SafeExhausted` is still
    /// reachable under an envelope, and the paired test's different answer
    /// came from the action budget rather than from the envelope's mere
    /// presence.
    #[test]
    fn the_same_two_candidates_run_to_exhaustion_under_a_wide_envelope() {
        let dir = make_temp_dir("autopilot-wide-envelope");
        let big_dir = dir.join("big");
        let small_dir = dir.join("small");
        let big = sized_node_modules_evidence(&big_dir, 16_384);
        let small = sized_node_modules_evidence(&small_dir, 1_024);
        let usage = FsUsage::new(1_000_000_000, 100);
        let config = base_config();
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();
        let envelope = wide_node_modules_envelope();

        let detector_registry = fake_registry(vec![big, small]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &FixedFsStat(usage),
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: Some(PressureState::Critical),
            }),
        });

        assert_eq!(
            report.stop_reason,
            StopReason::SafeExhausted(RemainingCandidates::default()),
            "an automatic run that really did run out of candidates says so, with \
             zeros rather than with an Autopilot excuse"
        );
        assert_eq!(report.actions_executed, 2);
        assert_eq!(report.total_bytes_freed, 17_408);
        assert!(!big_dir.join("node_modules").exists());
        assert!(!small_dir.join("node_modules").exists());

        fs::remove_dir_all(&dir).ok();
    }

    /// An automatic run that reaches the goal reports `TargetReached`, decided
    /// by a re-measurement of the filesystem — campaign §9 and §11: nothing in
    /// the envelope is consulted about whether the goal was met, and the
    /// envelope's remaining allowance (wide here, and untouched) is not what
    /// ended the run.
    #[test]
    fn an_automatic_run_that_reaches_the_goal_reports_target_reached() {
        let dir = make_temp_dir("autopilot-target-reached");
        let evidence = sized_node_modules_evidence(&dir, 16_384);
        let config = RecoveryConfig {
            target: FreeTarget::AbsoluteBytes(500),
            ..base_config()
        };
        let clock = crate::monitor::FakeClock::new();
        let wall_clock = FixedWallClock(SystemTime::UNIX_EPOCH);
        let action_registry = ActionRegistry::builtin();
        let ctx = empty_discovery_ctx();
        let envelope = wide_node_modules_envelope();

        // Below target twice — the opening reading that becomes
        // `started_free_bytes`, then iteration 1's own — and above it on the
        // third, which is the reading taken after the mutation. Only a loop
        // that re-measures can tell the second and third apart.
        let fs_stat = ScriptedFsStat::new(vec![
            FsUsage::new(1_000_000, 100),
            FsUsage::new(1_000_000, 100),
            FsUsage::new(1_000_000, 900),
        ]);

        let detector_registry = fake_registry(vec![evidence]);
        let report = run(RecoveryRunRequest {
            config: &config,
            fs_stat: &fs_stat,
            collector: &CleanCollector,
            detector_registry: &detector_registry,
            action_registry: &action_registry,
            clock: &clock,
            wall_clock: &wall_clock,
            policy_cfg: &PolicyConfig::default(),
            target_mount: Path::new("/"),
            discovery_ctx: &ctx,
            audit_log_path: &dir.join("actions.jsonl"),
            observer: &SilentObserver,
            stop: &NeverStops,
            admission: Some(RecoveryAdmission {
                envelope: &envelope,
                observed_pressure: Some(PressureState::Critical),
            }),
        });

        assert_eq!(report.stop_reason, StopReason::TargetReached);
        assert_eq!(report.actions_executed, 1);
        assert_eq!(report.started_free_bytes, 100);
        assert_eq!(report.final_free_bytes, 900);
        assert!(
            fs_stat.call_count() >= 3,
            "the goal must have been judged on a second, post-mutation reading"
        );

        fs::remove_dir_all(&dir).ok();
    }
}

/// Parses a `--target` CLI value into a [`FreeTarget`].
///
/// Format (deliberately minimal — see the PR's "Known limitations"):
/// - A trailing `%` is a [`FreeTarget::Percentage`], e.g. `"20%"`. Must
///   parse as a number in `0.0..=100.0`.
/// - Otherwise the value is a [`FreeTarget::AbsoluteBytes`]. An optional
///   case-insensitive `KB`/`MB`/`GB`/`TB` suffix uses binary (1024-based)
///   multipliers (`"5GB"` == `5 * 1024^3` bytes, matching the ticket's
///   `5GB`/`5368709120` example pair); a bare trailing `B` means "no
///   multiplier"; no suffix at all is also read as a raw byte count
///   (e.g. `"5368709120"`).
/// - No support for fractional shorthand combinations, negative values,
///   or any unit beyond TB.
pub fn parse_free_target(input: &str) -> Result<FreeTarget, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty --target value".to_string());
    }

    if let Some(pct) = trimmed.strip_suffix('%') {
        let value: f64 = pct
            .trim()
            .parse()
            .map_err(|_| format!("invalid percentage in --target value: {input}"))?;
        if !(0.0..=100.0).contains(&value) {
            return Err(format!("--target percentage out of range 0-100: {input}"));
        }
        return Ok(FreeTarget::Percentage(value));
    }

    let upper = trimmed.to_ascii_uppercase();
    let (numeric_part, multiplier): (&str, u64) = if let Some(p) = upper.strip_suffix("TB") {
        (p, 1024u64.pow(4))
    } else if let Some(p) = upper.strip_suffix("GB") {
        (p, 1024u64.pow(3))
    } else if let Some(p) = upper.strip_suffix("MB") {
        (p, 1024u64.pow(2))
    } else if let Some(p) = upper.strip_suffix("KB") {
        (p, 1024)
    } else if let Some(p) = upper.strip_suffix('B') {
        (p, 1)
    } else {
        (upper.as_str(), 1)
    };

    let value: f64 = numeric_part
        .trim()
        .parse()
        .map_err(|_| format!("invalid byte amount in --target value: {input}"))?;
    if value < 0.0 {
        return Err(format!(
            "--target byte amount must not be negative: {input}"
        ));
    }

    Ok(FreeTarget::AbsoluteBytes(
        (value * multiplier as f64) as u64,
    ))
}

#[cfg(test)]
mod parse_free_target_tests {
    use super::*;

    #[test]
    fn parses_percentage_form() {
        assert_eq!(parse_free_target("20%"), Ok(FreeTarget::Percentage(20.0)));
    }

    #[test]
    fn parses_percentage_with_fraction() {
        assert_eq!(parse_free_target("12.5%"), Ok(FreeTarget::Percentage(12.5)));
    }

    #[test]
    fn rejects_percentage_out_of_range() {
        assert!(parse_free_target("150%").is_err());
    }

    #[test]
    fn parses_raw_byte_count_with_no_suffix() {
        assert_eq!(
            parse_free_target("5368709120"),
            Ok(FreeTarget::AbsoluteBytes(5_368_709_120))
        );
    }

    #[test]
    fn parses_gb_suffix_as_binary_gigabytes() {
        assert_eq!(
            parse_free_target("5GB"),
            Ok(FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn parses_lowercase_suffix() {
        assert_eq!(
            parse_free_target("5gb"),
            Ok(FreeTarget::AbsoluteBytes(5 * 1024 * 1024 * 1024))
        );
    }

    #[test]
    fn parses_kb_and_mb_and_tb_suffixes() {
        assert_eq!(
            parse_free_target("1KB"),
            Ok(FreeTarget::AbsoluteBytes(1024))
        );
        assert_eq!(
            parse_free_target("1MB"),
            Ok(FreeTarget::AbsoluteBytes(1024 * 1024))
        );
        assert_eq!(
            parse_free_target("1TB"),
            Ok(FreeTarget::AbsoluteBytes(1024u64.pow(4)))
        );
    }

    #[test]
    fn rejects_negative_byte_amount() {
        assert!(parse_free_target("-5GB").is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(parse_free_target("").is_err());
        assert!(parse_free_target("   ").is_err());
    }

    #[test]
    fn rejects_garbage_input() {
        assert!(parse_free_target("not-a-target").is_err());
    }
}

#[cfg(test)]
mod types_tests {
    use super::*;

    #[test]
    fn percentage_target_met_when_free_fraction_meets_threshold() {
        let usage = FsUsage::new(100, 20);
        assert!(target_met(&usage, &FreeTarget::Percentage(20.0)));
        assert!(!target_met(&usage, &FreeTarget::Percentage(20.01)));
    }

    #[test]
    fn absolute_target_met_when_free_bytes_meets_threshold() {
        let usage = FsUsage::new(1_000, 500);
        assert!(target_met(&usage, &FreeTarget::AbsoluteBytes(500)));
        assert!(!target_met(&usage, &FreeTarget::AbsoluteBytes(501)));
    }

    #[test]
    fn percentage_target_on_zero_capacity_filesystem_is_trivially_met() {
        // Degenerate zero-total filesystem: nothing to free, so the
        // target can't meaningfully be unmet.
        let usage = FsUsage::new(0, 0);
        assert!(target_met(&usage, &FreeTarget::Percentage(50.0)));
    }

    #[test]
    fn the_default_stop_signal_never_asks_the_loop_to_stop() {
        assert!(!NeverStops.stop_requested());
    }

    #[test]
    fn a_stop_file_reports_a_stop_only_once_it_exists() {
        let dir = std::env::temp_dir().join(format!(
            "glomeris-stopfile-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("stop");

        let signal = StopFile::watching(&path).expect("a path that does not exist is watchable");
        assert!(!signal.stop_requested(), "nothing has asked for a stop yet");

        std::fs::write(&path, b"").expect("create the sentinel");
        assert!(signal.stop_requested());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stop_file_that_already_exists_is_refused_rather_than_watched() {
        // A stale sentinel would stop the next run before it did anything,
        // and the report would say the user stopped a run they never touched.
        let dir = std::env::temp_dir().join(format!(
            "glomeris-stopfile-stale-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("stop");
        std::fs::write(&path, b"").expect("leave a stale sentinel");

        let refused = StopFile::watching(&path);
        assert!(refused.is_err());
        assert_eq!(
            refused.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_stop_file_counts_as_a_stop_request() {
        // Fail-closed: a loop that cannot find out whether the user asked it
        // to stop must not keep deleting on the assumption they did not.
        assert!(StopFile::requested_from(Err(std::io::Error::other(
            "parent directory is unreadable"
        ))));
        assert!(!StopFile::requested_from(Ok(false)));
        assert!(StopFile::requested_from(Ok(true)));
    }

    #[test]
    fn the_silent_observer_accepts_every_event_and_keeps_nothing() {
        // Nothing to assert beyond "this compiles and does not panic": the
        // point of the type is that a caller who wants no progress stream
        // needs no branch at the call site.
        SilentObserver.observe(RecoveryProgress::Discovering { iteration: 1 });
        SilentObserver.observe(RecoveryProgress::Measured {
            iteration: 1,
            usage: FsUsage::new(100, 10),
            bytes_freed_so_far: 0,
        });
    }
}
