//
//  PlanApplication.swift
//  GlomerisMenuBar
//
//  HORO-1366: what "Apply Plan" is allowed to mean.
//
//  A reviewed AI plan can hold several candidates that Glomeris is willing to
//  clean, and before this the only way to act on them was to open each row and
//  press Clean. This file is the batch path's vocabulary: one step per plan
//  item, a preview assembled from those steps, and a result assembled from what
//  each attempt actually returned.
//
//  ============================================================================
//  THE BATCH PATH ADDS NO AUTHORITY, AND THAT IS STRUCTURAL
//  ============================================================================
//  "Apply Plan" is the phrase most likely to be misread as "let the model run
//  things", so the design answers that in the only way that stays true under
//  later edits: there is no second execution path to audit.
//
//    * Every item's status comes from its own fresh `explain --json`, and is
//      read through `CandidateActionability` — the same type, with the same
//      three fields, that the candidates list, the AI plan rows and the Clean
//      button already read. No field is re-derived here.
//    * Every mutation goes through `buildExecuteArguments` and one
//      `glomeris execute` child process — the same pure builder and the same
//      subcommand `CandidateDetailView.performClean()` uses. One argument
//      builder exists in this app, and the batch calls it in a loop.
//      `AiPlanSectionViewTests` pins that there is no second one.
//    * An ASK item's `--confirm-ask` is paired with the `--observed-fingerprint`
//      token from *that item's own* `explain` call, which is the only place a
//      token can come from. `LlmPlanItemReport` deliberately carries no
//      fingerprint token (`src/reporting/dto.rs`), so a provider's suggestion
//      cannot arrive pre-consented no matter what it says.
//
//  So the model's contribution to a batch is which resources are listed and in
//  what order. Neither can promote anything: a listed resource whose fresh
//  `explain` says `executable: false` is skipped, and order decides only the
//  sequence of identical per-item checks. The AI recommends, policy decides,
//  the executor verifies — unchanged, because none of those three moved.
//
//  ============================================================================
//  FOUNDATION ONLY, AND NO BOOLEAN THAT AUTHORISES ANYTHING
//  ============================================================================
//  Same two properties `CandidateActionability` documents, for the same
//  reason. This file cannot build a control, disable one, or spawn a process.
//  `PlanApplicationStep.disposition` is not permission — it is what the CLI
//  said, in a shape the preview can group by; the arguments that actually run
//  are built from the step's own `actionId`/`fingerprintToken`, which are
//  `nil` for every step this file describes as skipped.
//
//  ============================================================================
//  STALENESS IS REPORTED TWICE, IN THE TWO PLACES IT IS REAL
//  ============================================================================
//  `CandidateActionability`'s header explains why it has no "stale" case: no
//  pre-execution field reports staleness and inventing one would be a claim
//  without evidence. Both halves of that survive here.
//
//  A plan item whose `explain` call cannot be completed at preview time is
//  `.skippedStale` — not because this app decided the resource is gone, but
//  because the CLI would not describe it, and its own message says why. That
//  is evidence, and it is the CLI's.
//
//  An item that passes preview and then changes before its deletion is not
//  predicted at all. It surfaces as `execute`'s own `aborted_by_revalidation`
//  outcome with Rust's `abort_reason`, mapped to
//  `PlanApplicationItemStatus.abortedByRevalidation`. That is deletion-time
//  revalidation inside `executor::execute` doing its job, reported rather than
//  second-guessed.
//

import Foundation

// MARK: - One step

/// What Apply Plan will do about one item of the reviewed plan, as of the
/// preview.
///
/// Four cases, mapped one-to-one from `CandidateActionability` plus the one
/// situation that type cannot represent (the `explain` call itself did not
/// come back). The two "will run" cases are separate because the user has to
/// authorise them separately — see `PlanApplicationPreview`.
enum PlanApplicationDisposition: Equatable {
    /// `executable`, and the offered action does not ask first.
    case willRun
    /// `executable`, and the offered action asks first. Runs only with the
    /// user's explicit opt-in, and only with a fingerprint token.
    case willRunAfterConfirming
    /// Not `executable`. PROTECTED, no registered action, unplannable, or
    /// structurally refused — `reason` is the CLI's own sentence and the only
    /// thing that distinguishes those four, so it is passed through unchanged.
    case skippedRefused(reason: String)
    /// The resource could not be re-described at preview time, so nothing
    /// about it can be verified and nothing may be run against it.
    case skippedStale(reason: String)

    /// Whether this step is a candidate for execution at all. Not a
    /// permission — the runner still needs an `actionId`, and an
    /// `.willRunAfterConfirming` step also needs the user's opt-in and a
    /// token.
    var isRunnable: Bool {
        switch self {
        case .willRun, .willRunAfterConfirming: return true
        case .skippedRefused, .skippedStale: return false
        }
    }

    /// The sentence shown beside a skipped step. `nil` for the runnable
    /// cases, which the preview describes by grouping rather than by text.
    var skipReason: String? {
        switch self {
        case .willRun, .willRunAfterConfirming: return nil
        case .skippedRefused(let reason), .skippedStale(let reason): return reason
        }
    }
}

/// One plan item, re-checked against the CLI immediately before the preview is
/// shown.
///
/// Built from a *fresh* `ExplainReportDto`, never from the
/// `LlmPlanItemReportDto.candidate` embedded in the plan: that snapshot was
/// taken when the plan was requested, and a plan the user has been reading for
/// a minute is exactly when a stale `executable` would matter most. Re-asking
/// also means the preview and the deletion read the same field from the same
/// source, one after the other, which is what makes AC4's "same execution
/// invariants as single-item Clean" true rather than asserted.
/// Deliberately not `Identifiable`, and neither is `PlanApplicationItemResult`.
/// A provider may name the same resource twice — `AiPlanSectionView.rows`
/// already keys its `ForEach` by position for exactly that reason, and
/// `resourceId` as an identity would make a duplicated suggestion misrender in
/// the preview and the result too. Position is the honest identity here, since
/// the order is the plan's.
struct PlanApplicationStep: Equatable {
    let resourceId: String
    let kind: String
    let policyLabel: String

    /// Both `nil` for every skipped step, so a step this file describes as
    /// skipped cannot be turned into an `execute` invocation by any caller —
    /// `buildExecuteArguments` needs an action id, and `--confirm-ask` needs a
    /// token.
    let actionId: String?
    let fingerprintToken: String?

    let reclaimableBytes: UInt64?
    let reclaimableHuman: String?
    let reclaimableBytesIsLowerBound: Bool

    /// The shared reading of the CLI's actionability triple. Carried so the
    /// preview can show the same sentence the row and the detail show.
    let actionability: CandidateActionability
    let disposition: PlanApplicationDisposition

    var kindTerm: GlomerisTerm { GlomerisVocabulary.kind(kind) }
    var safetyTerm: GlomerisTerm { GlomerisVocabulary.safety(policyLabel) }

    /// The estimate, for the preview only. Never compared against, and never
    /// reported as an outcome: what a run reclaimed comes from
    /// `actual_reclaimed_bytes`.
    var reclaimableText: String {
        guard let reclaimableHuman, !reclaimableHuman.isEmpty else { return "size unknown" }
        return reclaimableBytesIsLowerBound ? "at least \(reclaimableHuman)" : reclaimableHuman
    }

    /// The normal path: `explain` came back, so every field is the CLI's.
    init(explain: ExplainReportDto) {
        let actionability = CandidateActionability(
            executable: explain.executable,
            offeredActions: explain.offeredActions,
            refusalReason: explain.refusalReason
        )
        self.resourceId = explain.resourceId
        self.kind = explain.kind
        self.policyLabel = explain.policyLabel
        self.reclaimableBytes = explain.reclaimableBytes
        self.reclaimableHuman = explain.reclaimableHuman
        self.reclaimableBytesIsLowerBound = explain.reclaimableBytesIsLowerBound
        self.actionability = actionability

        switch actionability {
        case .readyToClean:
            self.actionId = explain.offeredActions.first?.actionId
            self.fingerprintToken = explain.fingerprintToken
            self.disposition = .willRun
        case .asksFirstThenCleans:
            self.actionId = explain.offeredActions.first?.actionId
            self.fingerprintToken = explain.fingerprintToken
            self.disposition = .willRunAfterConfirming
        case .refused(let reason):
            self.actionId = nil
            self.fingerprintToken = nil
            self.disposition = .skippedRefused(reason: reason)
        case .refusedWithoutStatedReason:
            self.actionId = nil
            self.fingerprintToken = nil
            self.disposition = .skippedRefused(reason: actionability.sentence)
        }
    }

    /// The `explain` call did not come back. Everything this step would need
    /// in order to run is absent, which is the point.
    ///
    /// `kind` and `policyLabel` come from the plan item's own snapshot purely
    /// so the row can still be identified in the preview; they are display
    /// only, and no code path reads them to decide anything.
    init(staleResourceId resourceId: String, kind: String, policyLabel: String, reason: String) {
        self.resourceId = resourceId
        self.kind = kind
        self.policyLabel = policyLabel
        self.actionId = nil
        self.fingerprintToken = nil
        self.reclaimableBytes = nil
        self.reclaimableHuman = nil
        self.reclaimableBytesIsLowerBound = false
        self.actionability = .refused(reason: reason)
        self.disposition = .skippedStale(reason: reason)
    }
}

// MARK: - The preview

/// Everything the user sees before authorising a batch, and the only thing the
/// runner is allowed to execute from.
///
/// Order is the plan's order, which is the model's. That is deliberate and it
/// grants nothing: the sequence in which a fixed set of identical per-item
/// checks happens cannot change any of their outcomes. Re-sorting would also
/// contradict the AI Plan card, whose own header records that there is no
/// `.sorted` call in it — the list the user reviewed is the list they apply.
struct PlanApplicationPreview: Equatable {
    let steps: [PlanApplicationStep]

    init(steps: [PlanApplicationStep]) {
        self.steps = steps
    }

    var runnableSteps: [PlanApplicationStep] {
        steps.filter { $0.disposition == .willRun }
    }

    var confirmableSteps: [PlanApplicationStep] {
        steps.filter { $0.disposition == .willRunAfterConfirming }
    }

    var skippedSteps: [PlanApplicationStep] {
        steps.filter { !$0.disposition.isRunnable }
    }

    /// AC7's gate. `false` means the panel must not offer a destructive
    /// action at all — not a dimmed Apply button, which still asserts that
    /// applying this plan is a thing that could happen.
    ///
    /// "Confirmable" counts, per AC1: an all-ASK plan is a plan the user can
    /// apply, they just have to say so twice.
    var hasAnythingToApply: Bool {
        steps.contains { $0.disposition.isRunnable }
    }

    /// Whether the plan as it stands is worth offering a batch over at all —
    /// read from the snapshot `llm-plan` embedded in each item, before any
    /// `explain` has been re-run.
    ///
    /// This decides whether an *entry point* appears, and nothing else. It is
    /// explicitly not an authorisation and cannot be one: pressing that entry
    /// point runs the `explain` sweep, and the fresh reports are what every
    /// disposition is built from. A snapshot that has since gone stale
    /// therefore costs at most an offer to review a plan whose preview then
    /// says nothing in it can be applied — which is honest, and is the state
    /// `hasAnythingToApply` gates the destructive action on.
    ///
    /// Switched over rather than asked for a boolean, because
    /// `CandidateActionability` deliberately exposes none: a boolean there
    /// would be the obvious thing for a `.disabled(...)` to read, and then the
    /// enablement of a deleting control would depend on a display type. See
    /// that file's header.
    static func snapshotSuggestsAnApplicableItem(_ items: [LlmPlanItemReportDto]) -> Bool {
        items.contains { item in
            switch CandidateActionability(
                executable: item.candidate.executable,
                offeredActions: item.candidate.offeredActions,
                refusalReason: item.candidate.refusalReason
            ) {
            case .readyToClean, .asksFirstThenCleans: return true
            case .refused, .refusedWithoutStatedReason: return false
            }
        }
    }

    /// The positions of the steps that will actually be attempted, given the
    /// user's decision about items that ask first.
    ///
    /// When the opt-in is off, an ASK step is *skipped*, never run
    /// unconfirmed — the default is off, so the quietest possible misreading
    /// of this control leaves the more conservative batch.
    ///
    /// Positions rather than steps, because the runner has to record each
    /// attempt's outcome against the step it belongs to and a duplicated
    /// resource id cannot do that. `stepsToAttempt` is the same answer in the
    /// shape the preview's summary wants, derived from this one so there is a
    /// single definition of what gets attempted.
    func attemptedIndices(includingConfirmable: Bool) -> [Int] {
        steps.indices.filter { index in
            switch steps[index].disposition {
            case .willRun: return true
            case .willRunAfterConfirming: return includingConfirmable
            case .skippedRefused, .skippedStale: return false
            }
        }
    }

    func stepsToAttempt(includingConfirmable: Bool) -> [PlanApplicationStep] {
        attemptedIndices(includingConfirmable: includingConfirmable).map { steps[$0] }
    }

    /// The measured-evidence total for what will be attempted, or `nil` when
    /// no attempted step reported a size.
    ///
    /// Only steps that will be attempted are counted, and only their own
    /// reported `reclaimable_bytes`. A step whose size is unknown contributes
    /// nothing to the sum and sets `isLowerBound` instead, because the
    /// alternative — leaving it out silently — would present a confident
    /// total that understates the batch.
    func reclaimableEstimate(includingConfirmable: Bool) -> (bytes: UInt64?, isLowerBound: Bool) {
        let attempted = stepsToAttempt(includingConfirmable: includingConfirmable)
        var total: UInt64 = 0
        var measured = false
        var lowerBound = false
        for step in attempted {
            if let bytes = step.reclaimableBytes {
                total = total.addingReportingOverflow(bytes).partialValue
                measured = true
                if step.reclaimableBytesIsLowerBound { lowerBound = true }
            } else {
                lowerBound = true
            }
        }
        return measured ? (total, lowerBound) : (nil, lowerBound)
    }

    /// The estimate as one phrase, or `nil` when no attempted step reported a
    /// size — in which case the preview says nothing about space rather than
    /// showing a confident zero.
    ///
    /// Same convention as `PlanApplicationResult.reclaimedText`, for the same
    /// reason: one step quotes Rust's own rendering, and a sum is raw bytes
    /// because re-scaling it here would put a 1000-based number beside Rust's
    /// 1024-based ones.
    func reclaimableEstimateText(includingConfirmable: Bool) -> String? {
        let attempted = stepsToAttempt(includingConfirmable: includingConfirmable)
        let estimate = reclaimableEstimate(includingConfirmable: includingConfirmable)
        guard let bytes = estimate.bytes else { return nil }
        if attempted.count == 1, let only = attempted.first?.reclaimableHuman, !only.isEmpty {
            return estimate.isLowerBound ? "at least \(only)" : only
        }
        return estimate.isLowerBound ? "at least \(bytes) bytes" : "\(bytes) bytes"
    }
}

// MARK: - One item's result

/// What one attempted item's `execute` invocation actually did.
///
/// Every message on every case is text Rust produced — `describeExecuteOutcome`
/// assembles it, and this type calls that function rather than re-wording
/// anything, so a batch row and a single-item Clean report the same outcome in
/// the same words.
enum PlanApplicationItemStatus: Equatable {
    /// `outcome: "succeeded"`. `human` is built from the measured
    /// `actual_reclaimed_bytes`, never from the pre-run estimate.
    case cleaned(reclaimedBytes: UInt64?, human: String)
    /// An `ExecuteRefusalReport`. `reason` is Rust's discriminant, kept
    /// because whether the batch may continue depends on it; `message` is
    /// Rust's sentence, which is what the user reads.
    case refused(reason: String, message: String)
    /// `outcome: "aborted_by_revalidation"` — the resource changed between
    /// the preview and the deletion, and nothing was modified.
    case abortedByRevalidation(message: String)
    /// `outcome: "failed"`, or any outcome under which nothing was reclaimed
    /// and Rust did not name a refusal.
    case failed(message: String)
    /// Never attempted: skipped at preview time, excluded by the
    /// confirmation opt-in, or still queued when the batch stopped.
    case notAttempted(reason: String)

    var didClean: Bool {
        if case .cleaned = self { return true }
        return false
    }

    /// The one condition under which continuing the batch is pointless rather
    /// than merely unlucky: a different `glomeris` invocation holds the
    /// exclusive execution lock (`src/executor/lock.rs`), so every remaining
    /// item would refuse identically. Stopping and saying so is honest; a
    /// retry loop against a lock another process owns is not.
    ///
    /// This reads `ExecuteRefusalReport.reason`, which is an execution-state
    /// discriminant, not a policy label — no policy class is consulted here or
    /// anywhere else in this target.
    var haltsBatch: Bool {
        if case .refused(let reason, _) = self { return reason == "busy" }
        return false
    }

    /// The sentence for this item's row in the result.
    var message: String {
        switch self {
        case .cleaned(_, let human): return "Cleaned — reclaimed \(human)."
        case .refused(_, let message): return message
        case .abortedByRevalidation(let message): return message
        case .failed(let message): return message
        case .notAttempted(let reason): return reason
        }
    }

    /// Maps one `execute --json --progress-json` invocation to a status.
    ///
    /// The displayed text is taken from `describeExecuteOutcome` so the two
    /// paths cannot drift apart in wording; the classification decodes Rust's
    /// own discriminants, because "did this clean something" and "may the
    /// batch continue" are not answerable from a sentence.
    ///
    /// Anything that is neither a named success nor a named refusal is
    /// `.failed`, including `dry_run` and an unrecognised outcome. This path
    /// never requests a dry run, and treating an outcome we cannot interpret
    /// as "nothing was cleaned" is the fail-closed reading: the alternative
    /// would let an unknown future outcome be counted as a success.
    static func from(exitCode: Int32, stdout: Data, stderrText: String) -> PlanApplicationItemStatus {
        let display = describeExecuteOutcome(exitCode: exitCode, stdout: stdout, stderrText: stderrText)
        switch display {
        case .succeeded(let bytes, let human):
            return .cleaned(reclaimedBytes: bytes, human: human)
        case .message(let text):
            let decoder = JSONDecoder()
            if let report = try? decoder.decode(ExecuteReportDto.self, from: stdout) {
                if report.outcome == "aborted_by_revalidation" {
                    return .abortedByRevalidation(message: text)
                }
                return .failed(message: text)
            }
            if let refusal = try? decoder.decode(ExecuteRefusalReportDto.self, from: stdout) {
                return .refused(reason: refusal.reason, message: text)
            }
            return .failed(message: text)
        }
    }
}

/// One row of the result: which resource, and what happened to it.
struct PlanApplicationItemResult: Equatable {
    let resourceId: String
    let kind: String
    let status: PlanApplicationItemStatus

    var kindTerm: GlomerisTerm { GlomerisVocabulary.kind(kind) }
}

// MARK: - The aggregate

/// What a finished (or stopped) batch did, per item and in total.
///
/// AC8 is this type's whole purpose: a batch in which anything was refused,
/// aborted or failed is never reported as a success. The headline is computed
/// from the counts rather than from a flag someone has to remember to clear,
/// and `reclaimedBytes` sums only rows that actually cleaned something.
struct PlanApplicationResult: Equatable {
    let items: [PlanApplicationItemResult]

    /// Set when the batch ended before reaching every attempted item, with
    /// the deterministic reason it stopped. `nil` when every item the user
    /// authorised was tried.
    let stoppedEarlyReason: String?

    init(items: [PlanApplicationItemResult], stoppedEarlyReason: String? = nil) {
        self.items = items
        self.stoppedEarlyReason = stoppedEarlyReason
    }

    var cleanedCount: Int { items.filter { $0.status.didClean }.count }

    var notCleanedCount: Int { items.count - cleanedCount }

    /// Measured, from `actual_reclaimed_bytes` only. A cleaned row whose
    /// measured size was absent contributes nothing and is reported through
    /// `reclaimedIsIncomplete` instead of being guessed at.
    var reclaimedBytes: UInt64? {
        var total: UInt64 = 0
        var measured = false
        for item in items {
            if case .cleaned(let bytes, _) = item.status, let bytes {
                total = total.addingReportingOverflow(bytes).partialValue
                measured = true
            }
        }
        return measured ? total : nil
    }

    var reclaimedIsIncomplete: Bool {
        items.contains { item in
            if case .cleaned(let bytes, _) = item.status { return bytes == nil }
            return false
        }
    }

    /// `true` only when every row cleaned and nothing cut the batch short.
    /// An empty batch is not a success — there is nothing it could be a
    /// success at.
    var isCompleteSuccess: Bool {
        !items.isEmpty && notCleanedCount == 0 && stoppedEarlyReason == nil
    }

    /// One honest sentence. It names the number of items that did not clean
    /// whenever there are any, so no wording of this can report a partial
    /// batch as a finished one.
    var headline: String {
        if items.isEmpty {
            return "Nothing was run."
        }
        let cleaned = cleanedCount
        let notCleaned = notCleanedCount
        let itemWord = cleaned == 1 ? "item" : "items"
        if notCleaned == 0 {
            return "Cleaned \(cleaned) \(itemWord)."
        }
        if cleaned == 0 {
            return notCleaned == 1
                ? "Nothing was cleaned — 1 item did not run."
                : "Nothing was cleaned — \(notCleaned) items did not run."
        }
        return "Cleaned \(cleaned) of \(items.count) items — \(notCleaned) did not run."
    }

    /// The aggregate reclaimed figure, or `nil` when nothing cleaned. Kept
    /// separate from `headline` so a caller cannot show a size without also
    /// showing how many items it came from.
    ///
    /// # Why a multi-item total is raw bytes
    ///
    /// Rust renders a human string per `execute`, and there is no batch report
    /// for it to render a sum into — the batch is N single-item invocations, by
    /// design. So a total has to be assembled here, and `humanByteCount`'s
    /// header already settled what to do in exactly this position: emit the raw
    /// count with an explicit unit rather than scale it locally. Every other
    /// size in this panel comes from Rust's 1024-based `human_bytes`, and a
    /// locally-formatted "2.15 GB" beside Rust's own "2.0 GB" reads as though
    /// 150 MB went missing. A number that looks unformatted is a truthful
    /// signal; a number that looks finished and disagrees with the rows above
    /// it is not.
    ///
    /// A single cleaned row needs none of that: it quotes Rust's own string.
    ///
    /// # Why the single row is gated on the measured bytes, not on the string
    ///
    /// A successful `execute` does not always measure what it freed.
    /// `executor::execute_plan` reports `actual_reclaimed_bytes` as `Observed`
    /// only for a plan made entirely of `DeletePath` steps; any `RunTool` step
    /// makes it honestly `Unavailable`. `cargo.clean.target_dir` is a
    /// `RunTool`, so a real cargo cleanup returns `"actual_reclaimed_bytes":
    /// null` on *every* success — verified against the release binary, not
    /// inferred — and `humanByteCount` renders that as the literal `"unknown"`.
    ///
    /// Quoting the string unconditionally therefore produced "Cleaned 1 item.
    /// Reclaimed unknown." for the single commonest successful batch there is.
    /// Two such rows already said nothing, because the sum branch has no total
    /// to show — so the two branches disagreed about the same situation. Both
    /// now require a measured figure, and saying nothing about space is the
    /// truthful answer when nothing measured it.
    var reclaimedText: String? {
        let cleaned = items.compactMap { item -> (bytes: UInt64?, human: String)? in
            if case .cleaned(let bytes, let human) = item.status { return (bytes, human) }
            return nil
        }
        guard !cleaned.isEmpty else { return nil }
        if cleaned.count == 1, let only = cleaned.first, only.bytes != nil { return only.human }
        guard let total = reclaimedBytes else { return nil }
        return reclaimedIsIncomplete ? "at least \(total) bytes" : "\(total) bytes"
    }

    /// Builds the result from the preview it was applied from, so the account
    /// covers every item of the plan rather than only the ones that were tried.
    ///
    /// # Why skipped items are in the result
    ///
    /// A batch of five suggestions in which one cleaned and four were refused
    /// is not a one-item success with four items that were never mentioned
    /// again. AC8 says a plan is never reported successful when one or more
    /// items were refused, and the refused ones are precisely these — so every
    /// step appears, in the plan's order, each with either what its `execute`
    /// returned or the reason it was not attempted. That also means
    /// `isCompleteSuccess` is reachable only by a plan whose every item
    /// cleaned, which is the intended reading.
    ///
    /// # Why this is pure
    ///
    /// This mapping is the honesty of the whole feature, and it is the part a
    /// runner would be tempted to shortcut ("only report what I tried"). It
    /// takes the statuses it is given and decides nothing about them; the only
    /// judgment is which sentence a *not attempted* item gets, and there are
    /// three, each corresponding to a distinct thing that happened.
    ///
    /// `statusesByStepIndex` is keyed by position in `preview.steps`, not by
    /// resource id, because a provider may name the same resource twice.
    static func assemble(
        preview: PlanApplicationPreview,
        includingConfirmable: Bool,
        statusesByStepIndex: [Int: PlanApplicationItemStatus],
        stoppedEarlyReason: String?
    ) -> PlanApplicationResult {
        let attemptable = Set(preview.attemptedIndices(includingConfirmable: includingConfirmable))
        let items = preview.steps.enumerated().map { index, step -> PlanApplicationItemResult in
            let status = statusesByStepIndex[index]
                ?? .notAttempted(reason: notAttemptedReason(
                    step: step,
                    wasAttemptable: attemptable.contains(index),
                    batchStopped: stoppedEarlyReason != nil
                ))
            return PlanApplicationItemResult(
                resourceId: step.resourceId,
                kind: step.kind,
                status: status
            )
        }
        return PlanApplicationResult(items: items, stoppedEarlyReason: stoppedEarlyReason)
    }

    /// The three reasons an item of the plan was not attempted, in the order
    /// they take precedence.
    private static func notAttemptedReason(
        step: PlanApplicationStep,
        wasAttemptable: Bool,
        batchStopped: Bool
    ) -> String {
        // Glomeris would not run it. The CLI's own sentence, unchanged — the
        // only thing that distinguishes PROTECTED from a missing action from a
        // structural refusal from a stale resource.
        if let skipReason = step.disposition.skipReason { return skipReason }
        // It asks first, and the user did not include those.
        if !wasAttemptable {
            return "Not applied — you did not include the items that ask for confirmation."
        }
        // It was in the batch and the batch ended before reaching it.
        if batchStopped { return "Not attempted — the batch stopped before reaching this item." }
        // Attemptable, nothing stopped the batch, and yet no status was
        // recorded. Nothing produces this today; saying so plainly is better
        // than a sentence that implies a cause we do not have.
        return "Not attempted, and Glomeris cannot say why."
    }
}
