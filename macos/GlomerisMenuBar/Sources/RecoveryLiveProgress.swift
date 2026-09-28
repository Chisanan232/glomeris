//
//  RecoveryLiveProgress.swift
//  GlomerisMenuBar
//
//  HORO-1509: the closed loop, made watchable. A recovery run is the longest
//  thing this app starts and the only one that deletes as it goes, and until
//  this ticket the card showed a spinner and the sentence "per-step progress is
//  not reported yet" for the whole of it.
//
//  ---------------------------------------------------------------------
//  Why this is a reducer and not view state
//  ---------------------------------------------------------------------
//  Each NDJSON line the run emits carries one slice of the picture: the
//  measurement, or the discovery count, or the action starting, or the bytes one
//  action reclaimed. Nothing carries the whole state, so somebody has to
//  accumulate. Doing that inside a `View` would put every rule below — which
//  counts increment, which figures are allowed to be shown as progress, what a
//  missing byte count means — somewhere no test can reach, and this is the one
//  surface in the app whose numbers describe bytes that are already gone.
//
//  So `absorb(_:)` is a pure `mutating` function over a value type, and the
//  tests drive event sequences through it directly.
//
//  ---------------------------------------------------------------------
//  The rules it exists to hold
//  ---------------------------------------------------------------------
//    * Only measured bytes are shown as progress. `bytesFreedSoFar` is
//      re-measured free space, which is why it is the only figure that reaches
//      `reclaimedText`. `estimatedBytes` is never accumulated into anything
//      (campaign §9).
//    * A figure nobody measured is absent, not zero. Every display string here
//      is `String?` and starts `nil`, so a card can render nothing rather than
//      "0 B" for a measurement that has not happened yet.
//    * The loop's own wording is preferred where it exists. Outcome tokens go
//      through `GlomerisVocabulary.outcome`, so an action's result is worded
//      here exactly as it is worded in the history card and the audit log.
//
//  ---------------------------------------------------------------------
//  What is deliberately NOT reported live
//  ---------------------------------------------------------------------
//  The campaign asks the recovery surface to show a skipped/refused/ASK count.
//  Those three counts exist in the CLI — `RemainingCandidates` in
//  `src/executor/recovery_loop.rs` — but its `requires_confirmation` is
//  deliberately zero on any pass where a candidate *was* selected, because an
//  `Ask` that something safer went ahead of has not been left behind; it is
//  next. Streaming that number per pass would therefore print "0 need your
//  confirmation" during a run that has several waiting, which is precisely the
//  misleading zero this ticket is about.
//
//  So those counts are reported where they are true: on the finished run, from
//  `RecoveryRunReportDto.actionsDeclinedOrSkipped` and — for the stop that turns
//  on them — `RecoveryRunReportDto.remaining`. What this type reports live is
//  what the loop actually states as it goes: the round, what it is doing, what
//  it has measured, and how each action it ran turned out.
//

import Foundation

/// The live state of one recovery run, accumulated from its `--progress-json`
/// stream.
///
/// Every property is a display string or a count, and there is no property a
/// caller has to combine with another to get a true sentence.
struct RecoveryLiveProgress: Equatable {
    /// Which pass of the loop the run is on. `0` before the first event, which
    /// is a real state: the child has been spawned and has not said anything
    /// yet.
    private(set) var iteration: UInt32 = 0

    /// One line naming what the run is doing right now, in the loop's own terms.
    ///
    /// `nil` until the first event arrives, so the caller shows its own
    /// "starting" wording rather than this type inventing one for a run it has
    /// heard nothing from.
    private(set) var statusText: String?

    /// The most recent measurement, as `"76.0% used — 120 B free"`.
    ///
    /// One decimal place, matching `print_recovery_preview_report`'s
    /// `"{:.1}% used"` and `RecoveryPreviewSummary.currentText`, so the card,
    /// the pre-flight and the terminal cannot report the same reading
    /// differently. Only a `measured` event sets this: it is the one event that
    /// states a fact about free space.
    private(set) var currentUsageText: String?

    /// `"20 B reclaimed so far"`, from re-measured free space.
    ///
    /// `nil` until the run has measured at least once. Deliberately not seeded
    /// with "0 B": before the first measurement nothing is known, and afterwards
    /// the stream supplies the figure — including a genuine zero, which means
    /// something different and is shown.
    private(set) var reclaimedSoFarText: String?

    /// The resource the run is working on, or worked on last. Kept after the
    /// action finishes, because "what it just did" is the context for the
    /// outcome beside it.
    private(set) var currentResource: String?
    /// The action id, shown separately from the resource: one resource can have
    /// more than one action, and a row reading only `cargo:/p/target` would not
    /// say which.
    private(set) var currentAction: String?

    /// `true` between an `action_started` and its `action_finished`. This is the
    /// window in which a stop request cannot take effect, which is what the
    /// "Stop after current action" wording promises.
    private(set) var isActionInFlight = false

    /// Actions that ran and measured out. The completed count.
    private(set) var actionsSucceeded: UInt32 = 0
    /// Actions the executor attempted and could not finish.
    private(set) var actionsFailed: UInt32 = 0
    /// Actions abandoned by revalidation — the resource changed between being
    /// classified and being acted on, so the mutation was refused. Counted
    /// apart from a failure because nothing went wrong: a guard held.
    private(set) var actionsStoppedSafely: UInt32 = 0

    /// `true` once the run has seen the cooperative stop request. Set from the
    /// run's own `stop_requested` event rather than from the button press, so
    /// the card says "stopping" only when the loop has actually noticed.
    private(set) var stopRequested = false

    /// Detectors that failed on the most recent discovery pass.
    ///
    /// Carried because it changes what a later `safe_exhausted` means: part of
    /// the disk was never looked at, so "nothing safe left" is a statement about
    /// an incomplete search.
    private(set) var detectorsFailedOnLastPass: UInt32 = 0

    /// Folds one progress line in.
    ///
    /// Unknown phases cannot arrive — `RecoveryProgressEventDto` throws on one
    /// and `GlomerisClient` skips a line that throws — so this is a total
    /// function over the seven the CLI emits.
    mutating func absorb(_ event: RecoveryProgressEventDto) {
        iteration = event.iteration

        switch event {
        case .measured(let payload):
            currentUsageText = String(format: "%.1f%% used", payload.usedPercent)
                + " — \(payload.freeHuman) free"
            reclaimedSoFarText = "\(payload.bytesFreedSoFarHuman) reclaimed so far"
            statusText = "Checked free space"

        case .discovering:
            // Every iteration rescans; the loop never reuses an earlier pass's
            // list. Saying so is the point — a user watching a second round
            // start needs to know the search is happening again rather than
            // wondering why nothing is being deleted.
            statusText = "Looking again for what can be reclaimed…"

        case .discovered(let payload):
            detectorsFailedOnLastPass = payload.detectorsFailed
            let found = "\(payload.candidates) "
                + (payload.candidates == 1 ? "candidate" : "candidates")
            if payload.detectorsFailed == 0 {
                statusText = "Found \(found)"
            } else {
                // The same distinction `ProgressStatusText` draws for a scan: a
                // count alone cannot tell "looked and found little" from "part
                // of the search never answered".
                let checks = payload.detectorsFailed == 1 ? "check" : "checks"
                statusText = "Found \(found); \(payload.detectorsFailed) \(checks) did not finish"
            }

        case .revalidating:
            statusText = "Re-checking the evidence before acting…"

        case .actionStarted(let payload):
            currentResource = payload.resource
            currentAction = payload.action
            isActionInFlight = true
            statusText = "Reclaiming \(payload.resource)…"

        case .actionFinished(let payload):
            currentResource = payload.resource
            currentAction = payload.action
            isActionInFlight = false
            reclaimedSoFarText = "\(payload.bytesFreedSoFarHuman) reclaimed so far"
            switch payload.outcome {
            case Self.succeededOutcome: actionsSucceeded += 1
            case Self.failedOutcome: actionsFailed += 1
            case Self.abortedByRevalidationOutcome: actionsStoppedSafely += 1
            default:
                // The three above are every outcome a *real* run can produce.
                // `dry_run`, the fourth token `execution_outcome_tag` emits, can
                // never arrive here: `--dry-run` previews through
                // `free_preview`, which never enters the loop, and `--stop-file`
                // is refused alongside `--dry-run` for the same reason. So this
                // arm is for a token added to Rust later, and it is counted
                // nowhere rather than added to the nearest tally. The status line
                // below still reports it — from the vocabulary's unrecognised
                // wording — so such an action is uncounted, never hidden.
                break
            }
            // The CLI's token, worded by the same vocabulary the history card
            // and the audit log use, so one action has one description
            // wherever it is shown.
            let outcome = GlomerisVocabulary.outcome(payload.outcome).title
            statusText = "\(outcome): \(payload.resource)"

        case .stopRequested:
            stopRequested = true
            statusText = "Stopping after the current action…"
        }
    }

    /// Whether anything has finished, which is what decides whether there is a
    /// work-done row to show at all. A row reading "0 cleaned" on a run that has
    /// not reached its first action is a figure standing in for nothing.
    var hasCompletedWork: Bool {
        actionsSucceeded + actionsFailed + actionsStoppedSafely > 0
    }

    /// `"2 cleaned"`, or `"2 cleaned · 1 failed"`, or `nil` when nothing has
    /// finished.
    ///
    /// Each count appears only when it is non-zero. Showing `"· 0 failed"` would
    /// invite a reader to check a number that is only there because the format
    /// string has a slot for it.
    var workDoneText: String? {
        guard hasCompletedWork else { return nil }
        var parts = ["\(actionsSucceeded) cleaned"]
        if actionsFailed > 0 {
            parts.append("\(actionsFailed) failed")
        }
        if actionsStoppedSafely > 0 {
            parts.append("\(actionsStoppedSafely) stopped safely")
        }
        return parts.joined(separator: " · ")
    }

    /// What VoiceOver reads for the whole live card, as one sentence (campaign
    /// §14).
    ///
    /// Assembled here rather than left to the reading order of six separate
    /// rows, for the reason `PressureAlertSectionView.spokenState` gives: a
    /// screen-reader user gets these figures one at a time and cannot glance
    /// back at the one above. Each clause names its own subject, so the round,
    /// the usage and the reclaimed total cannot be mistaken for each other — the
    /// campaign's rule that spoken state must distinguish current usage from
    /// progress.
    ///
    /// Composed with ``SpokenLabel``, which is the app's one producer of these,
    /// so each clause terminates the same way every other spoken row does — and
    /// so the `nil` clauses below drop out rather than becoming bare stops a
    /// listener has to interpret.
    var spokenState: String {
        SpokenLabel.compose([
            iteration > 0 ? "Round \(iteration)" : nil,
            statusText,
            currentUsageText.map { "Disk now \($0)" },
            reclaimedSoFarText,
            workDoneText,
            stopRequested ? "Stop requested; the current action will finish first" : nil,
        ])
    }

    // MARK: - The CLI's outcome tokens

    /// From `execution_outcome_tag` in `src/executor/recovery_loop.rs`, which is
    /// also the producer of the audit log's `outcome` field — HORO-1509 AC5's
    /// "the summary and the audit log agree" holds because there is one.
    ///
    /// Compared as strings rather than handed to the vocabulary because these
    /// three decide *counts*, not wording: the vocabulary's job is the sentence,
    /// and it is used for that on the line below. A token this app does not know
    /// is counted nowhere rather than counted wrongly.
    ///
    /// `scripts/check-vocabulary-covers-cli-tokens.sh` diffs the Rust tokens
    /// against `GlomerisVocabulary.outcome`'s case labels, so a rename on the
    /// Rust side turns that check red — which is where the red belongs.
    static let succeededOutcome = "succeeded"
    static let failedOutcome = "failed"
    static let abortedByRevalidationOutcome = "aborted_by_revalidation"
}
