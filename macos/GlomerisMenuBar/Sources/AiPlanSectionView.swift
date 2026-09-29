//
//  AiPlanSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1308: the AI Plan card — a first-class GUI surface over the advisory
//  `glomeris llm-plan --json` report, so asking a provider "what should I
//  clean first?" no longer requires a terminal.
//
//  ---------------------------------------------------------------------
//  The one sentence this whole file exists to make visible on screen
//  ---------------------------------------------------------------------
//  AI recommends. Policy decides. Executor verifies.
//
//  Two fields in an `llm-plan` item are a provider's words — `modelReason`
//  and `priority` — and so is the ORDER of the items. Everything else was
//  computed locally by the real policy engine from real evidence and reads
//  identically whether or not a provider was ever contacted. This card is
//  therefore built around one visual rule: a model's claim is rendered as an
//  attributed quotation, and a machine verdict is rendered as a badge or a
//  plain statement. They never share a presentation, so "the model was
//  confident" can never be mistaken on a glance for "Glomeris agreed".
//
//  ---------------------------------------------------------------------
//  HORO-1550: why this card asks for contract version 2
//  ---------------------------------------------------------------------
//  Version 1 gave a row one sentence from the model and nothing else. That is
//  enough to rank and not enough to *explain*, and HORO-1550's whole subject is
//  explaining — which working trees look finished, which look active, and, most
//  of all, which the evidence does not settle. Version 2 is where the model's
//  reading of the workspace lives: a `disposition` (recommend now / ask / defer
//  / keep), a `model_confidence` that distinguishes observed from inferred from
//  unknown, the `uncertainties` it could not resolve, the `evidence_refs` it
//  claims to have read, plus a workspace `profile`, `observations` and the
//  read-only `evidence_requests` it wanted answered. So this card asks for
//  version 2 and version 2 only.
//
//  The visual rule above gets stricter as a result, because there is now much
//  more model text on the row. Everything the model produced — including its
//  disposition and its confidence — goes inside ONE attributed block, under one
//  `sparkles` heading, in italic secondary text. None of it becomes a
//  `GlomerisBadgeView`: a chip is this app's grammar for "a verdict was
//  reached", and `disposition` is the single field most likely to be mistaken
//  for the policy verdict if it were ever chipped next to one. The machine's
//  badges and sentences stay above it, unchanged and unmixed (§17).
//
//  `disposition` also gains no authority anywhere. It cannot make a row
//  executable, and the only place it changes behaviour at all is in the
//  direction of doing less: `ApplyPlanView` is handed the `recommend_now` items
//  only, so a bulk Apply never sweeps up something the model itself declined to
//  recommend now. The machine gate inside that view is unchanged and is still
//  what decides whether anything may run.
//
//  A `glomeris` that predates version 2 rejects the flag at argument-parse
//  time, before any provider is contacted and before anything is billed. That
//  is its own outcome — `contractUnsupported` — rather than a silent downgrade
//  to version 1, because a downgrade would drop every explanation above with no
//  visible reason and leave the user reading a thinner answer as if it were the
//  whole one.
//
//  ---------------------------------------------------------------------
//  Why there is no rank number on a row
//  ---------------------------------------------------------------------
//  `plan_with_llm` pushes validated items in the order the provider returned
//  them and never sorts; validation only drops entries (see the doc comment
//  on `LlmPlanItemReportDto`). So the list order here is advice, whereas the
//  candidates list's order is Glomeris's own ranking (`reporting::ranking`).
//  Numbering these rows "1, 2, 3" would dress a model's opinion in the exact
//  typography this app uses for machine judgments, and `priority` — a second,
//  independently-wrong-able copy of the same claim — would make it worse by
//  contradicting the order it sits in. Both are deliberately unrendered; the
//  one-line note above the rows says whose order it is instead.
//
//  There is equally no `.sorted` call in this file. Re-ordering a provider's
//  advice in Swift would be a third ranking, invented by the thinnest layer
//  in the system, and the honest way to disagree with the order is to read
//  the candidates list — which is right above this card.
//
//  ---------------------------------------------------------------------
//  Nothing here can execute anything
//  ---------------------------------------------------------------------
//  A row tap opens `CandidateDetailView`, the sole host of the Clean button,
//  by resource id — exactly as a candidates-list row does. That view issues
//  its own `explain` call and reads `executable` /
//  `offeredActions[].requiresConfirmation` / `fingerprintToken` from *that*
//  report, so an ASK resource still goes through its existing
//  confirmation-and-fingerprint path and an AUTO_SAFE one still goes through
//  the registered-action path. A plan item carries no `fingerprintToken` at
//  all (asserted on the wire in `DtoGoldenFixturesTests
//  .testLlmPlanItemsCarryNoFingerprintToken`), so a recommendation cannot
//  arrive pre-consented, and there is no second enablement path for one to
//  travel down. No string a model produced reaches an argument array: the
//  only value this file forwards is `candidate.resourceId`, which Rust set
//  from its own discovered evidence.
//
//  ---------------------------------------------------------------------
//  Nothing is sent anywhere until the user asks
//  ---------------------------------------------------------------------
//  `llm-plan` is invoked from exactly one place below, called only from the
//  "Ask AI for a plan" button's action. No `.onAppear`, no `.task`, no
//  `Timer`, no retry loop — a paid network call must never be a side effect
//  of opening a popover. `AiPlanSectionViewTests` greps this file for exactly
//  that invariant.
//
//  ---------------------------------------------------------------------
//  Why this reads the raw result instead of `client.run`
//  ---------------------------------------------------------------------
//  `llm-plan` exits 1 when the provider call itself failed — but it prints
//  the complete JSON report, `provider_error` and all, to stdout FIRST. A
//  `client.run` call would map that exit code to `executionFailed` and throw
//  the body away, losing the only structured account of what went wrong. So
//  this uses `runRaw` and maps the exit code itself, in `AiPlanInterpretation`
//  below — which is a pure function precisely so the five outcomes can be
//  asserted without rendering SwiftUI or spawning anything.
//

import SwiftUI

/// What one `llm-plan` invocation turned out to be.
///
/// Five cases rather than "a report or an error", because four of them need
/// materially different copy and exactly one of them is the user's problem to
/// fix:
///
///   - `plan` is the normal answer, and it carries a non-`nil`
///     `providerError` when the provider failed mid-flight — Rust still
///     reports what it managed, and so does this card;
///   - `notConfigured` is not a failure at all. Nothing was sent, nothing was
///     charged, and the remedy is a setting;
///   - `contractUnsupported` means the installed `glomeris` does not implement
///     the plan format this app asks for. Also not the user's fault, also
///     nothing sent — the flag is rejected while arguments are being parsed —
///     but the remedy is a CLI update rather than a setting, which is why it is
///     not folded into either neighbour;
///   - `malformedOutput` means the CLI exited 0 with something this app
///     cannot decode, which is a version skew between the app and the
///     `glomeris` on `PATH` — a different problem from the provider's;
///   - `failed` is everything else, carrying the CLI's own stderr rather than
///     a sentence invented here.
enum AiPlanOutcome: Equatable {
    case plan(WorkspacePlanReportDto)
    case notConfigured
    case contractUnsupported
    case malformedOutput
    case failed(String)
}

/// Pure, directly-testable mapping from `llm-plan`'s exit code and streams to
/// an `AiPlanOutcome`.
///
/// The exit-code contract is `run_llm_plan_command`'s, read from the source
/// rather than assumed (`src/main.rs`):
///
///   * **0** — the report was printed to stdout;
///   * **1** — `provider_error` was set; the report was STILL printed to
///     stdout first, then the process exited 1. Decoding stdout is what keeps
///     that account;
///   * **2** — either missing LLM configuration (a specific sentence on
///     stderr) or a usage error. The two are told apart by that sentence,
///     because only one of them is worth showing the user a settings prompt
///     for; a usage error means this app sent arguments the installed
///     `glomeris` does not understand, which is the app's bug, not theirs.
enum AiPlanInterpretation {
    /// A substring of the CLI's own exit-2 sentence, matched rather than
    /// reproduced in full: the full sentence names three environment
    /// variables and will grow a fourth, and a guard that breaks on rewording
    /// would silently reclassify "no provider configured" as a hard failure.
    static let missingConfigurationMarker = "missing LLM configuration"

    /// The exact sentence a `glomeris` older than HORO-1548 emits for the flag
    /// this app now always passes (`src/main.rs`'s `unrecognized argument
    /// '{other}'`), spelled with the flag inside it rather than matched on
    /// "unrecognized argument" alone — that broader marker would also swallow a
    /// genuine usage bug in this app, which is a different problem with a
    /// different remedy and must stay a `failed`.
    ///
    /// Argument parsing happens before any provider is contacted, so reaching
    /// this state costs nothing and sends nothing.
    static let unsupportedContractMarker = "unrecognized argument '\(Self.contractVersionFlag)'"

    /// Named once so the detector above and the invocation below cannot drift
    /// apart: a rename in the argument array that missed the marker would turn
    /// "your CLI is too old" back into an opaque failure.
    static let contractVersionFlag = "--contract-version"

    /// The one plan format this card can render. Passed to the CLI and checked
    /// on the way back, so the two are the same claim in one place.
    static let expectedContractVersion: UInt32 = 2

    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> AiPlanOutcome {
        let stderrText = Self.trimmedText(stderr)

        if exitCode == 2 {
            if stderrText.contains(Self.missingConfigurationMarker) {
                return .notConfigured
            }
            if stderrText.contains(Self.unsupportedContractMarker) {
                return .contractUnsupported
            }
            return .failed(
                stderrText.isEmpty
                    ? "glomeris rejected the arguments this app sent (exit 2)."
                    : stderrText
            )
        }

        // Tried before the exit code is judged, because exit 1 prints a full
        // report and throwing it away would discard the only structured
        // description of the provider failure.
        if let report = try? JSONDecoder().decode(WorkspacePlanReportDto.self, from: stdout),
            report.contractVersion == Self.expectedContractVersion {
            // The version is checked as well as the shape. A build that grew a
            // version 3 could emit something version-2-decodable whose fields
            // mean something else, and reading that as a version 2 plan is the
            // exact mistake `--contract-version`'s refusal path exists to
            // prevent (`src/main.rs`: "they will read a version 1 report as a
            // version 2 one"). A mismatch falls through to `malformedOutput`,
            // whose copy already says the app and the CLI disagree.
            return .plan(report)
        }

        if exitCode == 0 {
            return .malformedOutput
        }

        return .failed(
            stderrText.isEmpty
                ? "glomeris llm-plan exited with code \(exitCode) and printed no plan."
                : stderrText
        )
    }

    private static func trimmedText(_ data: Data) -> String {
        String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Pure, directly-testable mapping from a request's state to the one message
/// shown instead of — or, for a partial answer, above — the rows.
///
/// Extracted from the view because AC 7 of HORO-1308 is a claim about six
/// distinct situations having distinct, correct presentations, and a claim like
/// that is worth asserting rather than eyeballing. In particular the two
/// non-failures are easy to get wrong and expensive to get wrong:
///
///   * having never asked is not a finding, so it must not wear `empty`'s
///     checkmark — nobody has earned a clean bill of health;
///   * having no provider configured is not an error at all. Nothing was sent,
///     nothing was charged, and dressing it in a red triangle tells a user
///     something is broken when the answer is "this is opt-in".
///
/// `AiPlanSectionViewTests` pins both, and pins that a provider failure and a
/// version skew DO read as failures.
enum AiPlanStateMessages {
    /// `nil` means "the rows are the whole answer" — the only case with
    /// nothing to say.
    static func message(for outcome: AiPlanOutcome?, isPlanning: Bool) -> GlomerisStateMessage? {
        guard let outcome else {
            if isPlanning {
                return .loading("Asking your AI provider for a plan…")
            }
            return .notLookedYet(
                "No plan yet",
                detail: "Nothing has been sent anywhere. Ask for a plan when you want one."
            )
        }

        switch outcome {
        case .plan(let report):
            if let providerError = report.providerError {
                // Verbatim from Rust. This is the report the CLI printed
                // before exiting 1, which is exactly why it is worth reading.
                //
                // Checked before the empty case, and the ordering is load-
                // bearing: a failed provider call usually also yields zero
                // items, and "No suggestions" would report that as a
                // considered answer rather than as a call that did not
                // complete.
                return .failure(
                    "Your AI provider did not return a usable plan: \(providerError)"
                )
            }
            // HORO-1550: "no rows" stopped meaning "no answer" when the
            // contract grew a profile, observations and evidence requests. A
            // reply that proposes nothing but reports two conflicts and asks
            // for a branch probe IS an answer, and covering it with "had
            // nothing to propose" would hide the most useful thing the model
            // said. So the empty message is reserved for a reply that carried
            // nothing at all, and the explanation block speaks for the rest.
            if report.items.isEmpty && !report.saidSomethingBesidesItems {
                return .empty(
                    "No suggestions",
                    detail: "Your provider answered, but had nothing to propose for what "
                        + "Glomeris found."
                )
            }
            return nil

        case .notConfigured:
            // The environment note is here because it is the single most
            // likely reason a user who *has* configured a provider in their
            // shell still lands here: a menu-bar agent launched from Finder
            // inherits no shell environment. HORO-1309 made that fixable
            // without a terminal, and the view renders a Settings button
            // beside this message — a message cannot carry a control.
            return .notLookedYet(
                "No AI provider configured",
                detail: "Glomeris found no provider settings, so it sent nothing. Set one up in "
                    + "Settings — note that this app does not inherit your shell environment, "
                    + "so variables exported in a terminal are not visible here."
            )

        case .contractUnsupported:
            // A failure, but a truthful one about whose problem it is, and it
            // states the two facts a user would otherwise have to guess: that
            // nothing left the machine, and that no provider charged for it.
            return .failure(
                "The installed glomeris cannot produce the plan format this app needs. Nothing "
                    + "was sent to your AI provider and nothing was charged — update the "
                    + "glomeris CLI, then ask again."
            )

        case .malformedOutput:
            return .failure(
                "The installed glomeris reported success but this app could not read its "
                    + "plan. The app and the CLI are probably different versions."
            )

        case .failed(let message):
            return .failure(message)
        }
    }
}

/// Pure formatting step from one plan item to what a row renders.
///
/// `machineVerdictLines` is the interesting one: it is derived ONLY from
/// `skipReason`, `candidate.executable`, `candidate.refusalReason` and
/// `candidate.offeredActions[].requiresConfirmation` — the structured fields
/// that actually gate behaviour — and never from the policy label's text or
/// from anything the model said. `modelReason` sits beside it as a quotation
/// and has no influence on it whatsoever.
struct AiPlanRowViewModel: Equatable {
    /// Taken from `candidate.resourceId` rather than the item's own copy.
    /// The two are equal by construction in `build_llm_plan_report`, and both
    /// come from local discovery rather than from the model — but the
    /// candidate projection is the thing this app treats as authoritative
    /// everywhere else, so it is the one read here too.
    let resourceId: String

    let kindTerm: GlomerisTerm
    let impactTerm: GlomerisTerm
    let impactTierTerm: GlomerisTerm?
    let safetyTerm: GlomerisTerm

    /// The third axis from this campaign's design principles, and the reason
    /// the AI Plan card can show it when the candidates list cannot: a plan
    /// item carries `completeness`/`confidence` per row, whereas
    /// `detect --json` does not. Rendered unfilled and on its own line so it
    /// reads as "how well do we know this", never as part of the safety
    /// verdict — high confidence in a PROTECTED classification does not make
    /// the resource any less protected.
    let completenessTerm: GlomerisTerm
    let confidenceTerm: GlomerisTerm

    /// The provider's sentence, or `nil` when it gave none. Already bounded
    /// and control-character-stripped in Rust.
    let modelReason: String?

    /// Everything else the model said, when this row came from a
    /// contract-version-2 wrapper; `nil` when it came from a bare version 1
    /// item.
    ///
    /// Kept as a separate value rather than flattened into the fields above so
    /// that a surface reading "what Glomeris determined" and a surface reading
    /// "what the model read into it" cannot draw from the same properties — see
    /// `AiPlanModelReadingViewModel`.
    let modelReading: AiPlanModelReadingViewModel?

    /// What Glomeris itself says about acting on this suggestion, in reading
    /// order. Usually one line; two for a refusal that has both an
    /// item-level skip reason and a candidate-level reason code, because
    /// those say different things — "no action was rendered at all" versus
    /// "here is the policy reason" — and the refusal is the row that can
    /// least afford to be summarised.
    let machineVerdictLines: [String]

    /// What VoiceOver reads, because the row is a single button.
    ///
    /// Ordered deliberately: what it is, what Glomeris will permit, how much
    /// is at stake, how well it is known, and only THEN what the model said,
    /// explicitly attributed. A screen-reader user hears the machine's
    /// verdict before the model's opinion, which is the same priority a
    /// sighted user gets from the badges sitting above the quotation.
    ///
    /// # HORO-1451
    ///
    /// This used to append each clause with a bare space, so a refused
    /// suggestion — the one row that can least afford to be summarised — was
    /// heard as an unpunctuated run: the skip reason, the reason code, the
    /// model's sentence and the path all ran into one another, because every
    /// one of those four is a fragment Rust does not terminate. On screen they
    /// are separate `Text` views and layout supplies the boundary, which is why
    /// nothing but the live accessibility tree showed it.
    ///
    /// Two changes, both matching what `CandidateRowViewModel` has done since
    /// HORO-1323 so that one resource does not describe itself two ways in two
    /// lists. `SpokenLabel` terminates each clause exactly once, and the
    /// verdict carries the cleanup axis so a listener knows which question the
    /// refusal is answering. The refusal text itself is untouched.
    ///
    /// The axis names the verdict once rather than once per line. There can be
    /// two lines — the planner's "no action was rendered at all" and the
    /// candidate's "here is the policy reason" — and they are two statements on
    /// the same axis, so "Cleanup: A. B." is what they are. Repeating the
    /// prefix would claim they were about different things.
    var accessibilityLabel: String {
        var clauses: [String?] = [
            kindTerm.title,
            SpokenLabel.clause(safetyTerm.axis, safetyTerm.title),
            SpokenLabel.clause(impactTerm.axis, impactTerm.title),
            impactTierTerm?.title,
            SpokenLabel.clause(completenessTerm.axis, completenessTerm.title),
            SpokenLabel.clause(confidenceTerm.axis, confidenceTerm.title),
        ]
        for (index, line) in machineVerdictLines.enumerated() {
            clauses.append(index == 0 ? SpokenLabel.clause(CandidateActionability.axis, line) : line)
        }
        if let modelReading {
            // HORO-1550: the reading's own clauses, which open with the same
            // attribution the single-sentence form carried and then add the
            // disposition, the confidence, the uncertainties and the cited
            // aliases. Every one of those is spoken, because AC 6 is that a
            // VoiceOver user reaches all the evidence, conflict and uncertainty
            // text — and an uncertainty only a sighted user can read was not
            // disclosed.
            clauses.append(contentsOf: modelReading.accessibilityClauses.map { Optional($0) })
        } else if let modelReason {
            // Attribution and quotation are ONE clause on purpose. Split into
            // two they would be two sentences, and a listener arriving at the
            // second one late would hear the model's opinion in the same
            // grammatical position as Glomeris's verdicts above it.
            clauses.append("The model's reason, which is advice and not a verdict: \(modelReason)")
        }
        clauses.append("Path: \(resourceId)")
        return SpokenLabel.compose(clauses)
    }

    /// The contract-version-2 wrapper: the same machine row, plus the model's
    /// reading of it kept beside rather than inside.
    init(_ dto: WorkspacePlanItemReportDto) {
        self.init(dto.item, reading: AiPlanModelReadingViewModel(dto))
    }

    init(_ dto: LlmPlanItemReportDto) {
        self.init(dto, reading: nil)
    }

    private init(_ dto: LlmPlanItemReportDto, reading: AiPlanModelReadingViewModel?) {
        let candidate = dto.candidate
        resourceId = candidate.resourceId
        kindTerm = GlomerisVocabulary.kind(candidate.kind)
        impactTerm = GlomerisVocabulary.storageImpact(
            human: candidate.reclaimableHuman,
            isLowerBound: candidate.reclaimableBytesIsLowerBound
        )
        impactTierTerm = GlomerisVocabulary.impactTier(candidate.impactTier)
        safetyTerm = GlomerisVocabulary.safety(candidate.policyLabel)
        completenessTerm = GlomerisVocabulary.completeness(dto.completeness)
        confidenceTerm = GlomerisVocabulary.confidence(dto.confidence)
        modelReason = dto.modelReason
        modelReading = reading
        machineVerdictLines = Self.verdictLines(dto)
    }

    /// Built from the gating fields alone. `skipReason` and `refusalReason`
    /// are passed through verbatim — this layer never rewords a refusal.
    ///
    /// HORO-1323 moved the four sentences out to `CandidateActionability`,
    /// unchanged. They were written here, but the candidates list and the
    /// Clean button needed the same wording, and three copies of "what
    /// Glomeris will do about this resource" is three chances for them to
    /// disagree about it. What stays here is the part that is genuinely this
    /// card's own: a plan item can also carry a `skip_reason`, which is the
    /// planner explaining why it rendered no action at all, and that is a
    /// different statement from the candidate's refusal reason.
    private static func verdictLines(_ dto: LlmPlanItemReportDto) -> [String] {
        var lines: [String] = []
        if let skipReason = dto.skipReason {
            lines.append(skipReason)
        }

        let actionability = CandidateActionability(
            executable: dto.candidate.executable,
            offeredActions: dto.candidate.offeredActions,
            refusalReason: dto.candidate.refusalReason
        )

        switch actionability {
        case .refusedWithoutStatedReason:
            // Behaviour preserved on every input the CLI can produce: when the
            // CLI gave no reason of its own but the planner did explain itself,
            // the generic sentence adds nothing and is left off. Unreachable in
            // practice — every branch of `executable_fields` returns a reason —
            // but changing it silently while refactoring would be a behaviour
            // change smuggled in as a tidy-up, so
            // `testASkipReasonSuppressesTheGenericSentence` pins it.
            //
            // Two inputs Rust cannot emit do differ from the pre-HORO-1323
            // code, both in the direction of saying less: a `skip_reason` byte
            // -identical to one of the permitted sentences is no longer
            // duplicated, and `refusal_reason: Some("")` no longer produces a
            // blank verdict line.
            if lines.isEmpty {
                lines.append(actionability.sentence)
            }
        case .readyToClean, .asksFirstThenCleans, .refused:
            if !lines.contains(actionability.sentence) {
                lines.append(actionability.sentence)
            }
        }

        return lines
    }
}

/// Pure formatting step from the contract-version-2 wrapper around a plan item
/// to the one attributed block that renders everything the model said about it.
///
/// A separate type from `AiPlanRowViewModel` on purpose, and the separation is
/// the point rather than a tidiness preference. §17 of this campaign requires a
/// local fact, a model inference and a policy verdict to be impossible to
/// mistake for one another on screen; the wire format already keeps them apart
/// by nesting the machine's row *inside* the model's wrapper instead of
/// flattening the two together. Mirroring that split in two view models means a
/// surface renders "what Glomeris determined" and "what the model read into it"
/// from two different values, so merging them would take a deliberate edit
/// rather than an oversight.
///
/// Nothing here can gate anything. Every property is a sentence or a list of
/// sentences, and the only reader is a `Text`.
struct AiPlanModelReadingViewModel: Equatable {
    /// What the model recommends doing, in plain language.
    ///
    /// Not a `GlomerisTerm`, and this is the single most important omission in
    /// the file: a vocabulary term becomes a `GlomerisBadgeView`, a badge is
    /// this app's grammar for "a verdict was reached", and a `recommend_now`
    /// chip sitting beside an `ASK` chip would read as two verdicts that
    /// disagree rather than as an opinion under a ruling.
    let dispositionSentence: String

    /// How well the model says it knows its own claim — `observed`, `inferred`
    /// or `unknown` from §13's vocabulary. Deliberately worded as reported
    /// speech ("it says"), because the alternative phrasing states the model's
    /// epistemic position as a fact about the resource.
    let confidenceSentence: String

    /// The provider's sentence, or `nil` when it gave none. Already bounded and
    /// control-character-stripped in Rust.
    let quote: String?

    /// What the model says it could not settle. Rendered in full rather than
    /// counted: an uncertainty a user cannot read is an uncertainty that was
    /// not disclosed, and §13's rule is that missing evidence must never become
    /// negative evidence — which starts with it being visible at all.
    let uncertainties: [String]

    /// The opaque, request-scoped aliases the model cites — `resource_1`,
    /// `workspace_1`, `machine`, `workflow_history`. Shown because they are what
    /// makes the reply auditable, and safe to show precisely because they are
    /// aliases: §5 keeps real paths, repository names and task keys out of the
    /// payload, so there is nothing here to leak back onto the screen.
    let evidenceRefs: [String]

    /// Clauses appended to the row's spoken label, after every machine verdict.
    ///
    /// Attribution is carried inside the clauses rather than by their position,
    /// because position is exactly what a listener who arrives late does not
    /// have. A user who tabs into the middle of this list must still hear that
    /// what follows is the model's reading.
    var accessibilityClauses: [String] {
        var clauses: [String] = ["The model's reading, which is advice and not a verdict"]
        clauses.append(dispositionSentence)
        clauses.append(confidenceSentence)
        if let quote {
            clauses.append("Its reason: \(quote)")
        }
        if !uncertainties.isEmpty {
            clauses.append(
                "It says it could not settle: \(uncertainties.joined(separator: "; "))"
            )
        }
        if !evidenceRefs.isEmpty {
            clauses.append("It cites: \(evidenceRefs.joined(separator: ", "))")
        }
        return clauses
    }

    init(_ dto: WorkspacePlanItemReportDto) {
        dispositionSentence = Self.dispositionSentence(dto.disposition)
        confidenceSentence = Self.confidenceSentence(dto.modelConfidence)
        quote = dto.item.modelReason
        uncertainties = dto.uncertainties
        evidenceRefs = dto.evidenceRefs
    }

    /// `planner::contract::Disposition`'s four tags.
    ///
    /// The default arm exists although Rust drops unrecognised dispositions
    /// (`dropped.unknown_disposition` counts them), because "this build cannot
    /// produce that" is not the same claim as "no build can", and a newer CLI
    /// paired with this app is exactly the pairing `contractUnsupported` cannot
    /// catch. Naming the token is more honest than rendering nothing, which
    /// would silently drop the model's recommendation from the one place a user
    /// looks for it.
    private static func dispositionSentence(_ raw: String) -> String {
        switch raw {
        case "recommend_now": return "Recommends reclaiming this now."
        case "ask_user": return "Wants you to decide about this one."
        case "defer": return "Suggests leaving this for now."
        case "keep": return "Suggests keeping this."
        default: return "Gave a recommendation this app does not recognise: “\(raw)”."
        }
    }

    /// `planner::contract::ClaimConfidence`'s three tags — §13's
    /// observed/inferred/unknown distinction, which is the whole reason the
    /// version 2 contract exists.
    private static func confidenceSentence(_ raw: String) -> String {
        switch raw {
        case "observed": return "It says this rests on evidence it was given."
        case "inferred": return "It says this is inferred rather than observed."
        case "unknown": return "It says it does not know."
        default: return "Reported a confidence this app does not recognise: “\(raw)”."
        }
    }
}

/// Popover section: an explicitly-requested AI plan, rendered so the model's
/// advice and Glomeris's verdict can never be confused. No polling, no timer,
/// no first-appearance call — see file header.
struct AiPlanSectionView: View {
    private let client: GlomerisClient
    private let projectRootsStore: ProjectRootsStore

    /// Read at spawn time, not at init time, so a provider configured in the
    /// Settings window takes effect on the next Ask without reopening the
    /// popover (HORO-1309). Holding the resolved environment here instead
    /// would cache a credential for the lifetime of a view.
    private let settingsStore: GlomerisLlmSettingsStore

    /// HORO-1365: the plan this card renders is NOT owned here. A plan the
    /// user paid a provider for must outlive any drill-down into one of the
    /// resources it mentions, and `@State` on this view could not promise
    /// that — its lifetime is the view's place in the hierarchy, which the
    /// panel's detail navigation decides. See `OverviewState.swift`.
    ///
    /// This card is still the only thing that writes it, and still only from
    /// the Ask button below.
    @ObservedObject private var plan: PlanState

    /// What a row tap does: report the suggested resource upward, so the
    /// popover shell shows the same detail view a candidates-list row leads
    /// to (HORO-1357). It is the only thing a row tap ever does; no action
    /// runs from this list.
    ///
    /// This was a `.sheet(item:)` presented from this card. Both entry points
    /// into the detail view now go through the shell, so there is exactly one
    /// presentation of it to get right — and a sheet, which on a
    /// `MenuBarExtra(.window)` panel can order the panel out when it appears
    /// or resizes, is no longer any part of it.
    private let onOpenDetail: (String) -> Void

    /// `plan` has deliberately NO default, for the reason given on
    /// `CandidatesSectionView.init`: a defaulted `PlanState()` is a silent
    /// route back to "No plan yet" over a plan that exists.
    init(
        plan: PlanState,
        client: GlomerisClient = GlomerisClient(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore(),
        settingsStore: GlomerisLlmSettingsStore = GlomerisLlmSettingsStore(),
        onOpenDetail: @escaping (String) -> Void = { _ in }
    ) {
        self.plan = plan
        self.client = client
        self.projectRootsStore = projectRootsStore
        self.settingsStore = settingsStore
        self.onOpenDetail = onOpenDetail
    }

    var body: some View {
        GlomerisCard(title: "AI Plan", trailing: countText) {
            controlRow
            provenanceNote

            if plan.isPlanning, let progressStatusText = plan.progressStatusText {
                Text(progressStatusText)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
            }

            outcomeBody

            // Additive, never a replacement — the same rule the status and
            // candidates cards follow. A failed request leaves an earlier
            // plan visible, which is useful, but the freshness stamp above
            // still says when that plan was actually made.
            if let lastErrorMessage = plan.lastErrorMessage {
                GlomerisStateMessageView(message: .failure(lastErrorMessage))
            }
        }
    }

    // MARK: - Header

    private var controlRow: some View {
        HStack(spacing: GlomerisDesign.inlineSpacing) {
            Button(plan.isPlanning ? "Asking…" : "Ask AI for a plan") {
                // Set here as well as in `runLlmPlan`, for the reason given on
                // the Refresh button — and with a worse consequence if the
                // window is hit: a second tap would overwrite `planTask`,
                // orphaning the first request where Stop can no longer reach it,
                // and paying a provider twice for one question.
                plan.isPlanning = true
                plan.planTask = Task { await runLlmPlan() }
            }
            // HORO-1366 adds `isApplyingBatch`. A new plan replaces the one a
            // batch is running, and a batch cannot be stopped — so asking mid-
            // batch would leave a result to be rendered under a plan it was not
            // about, and let a second Apply start beside the first. Refusing the
            // question for the few seconds deletions take is the honest answer;
            // `applyPlan`'s own latch is the backstop, not the gate.
            .disabled(plan.isPlanning || plan.isApplyingBatch)

            if plan.isPlanning {
                Button("Stop") {
                    plan.planTask?.cancel()
                }
            }

            Text(lastPlannedText)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)

            Spacer(minLength: 0)
        }
    }

    /// Always visible, including before anything has been asked for: it is
    /// the standing rule of the product, not a caption on a result. Kept to
    /// two short lines because a paragraph here would be scrolled past, and
    /// the second line is the one a first-time user needs — that pressing the
    /// button is what sends anything anywhere.
    ///
    /// HORO-1367 shortened the second line. Every fact it carried is still
    /// here — that asking sends a summary, that it goes to the user's own
    /// provider, that it may cost money, and that Settings shows *exactly*
    /// what would be sent without sending it — because each one is a thing
    /// the user would be wronged by not knowing. What went was the wording
    /// around them: at the old width this line wrapped to three lines of
    /// tertiary caption directly above the button it describes, which is the
    /// shape a reader skips.
    ///
    /// "Shows exactly what would be sent" is the one phrase here that must
    /// not be paraphrased. It was briefly shortened to "previews it", which
    /// is true of a summary, a sample or an approximation as well — and the
    /// claim being made is the stronger one, that what Settings displays is
    /// the payload itself. On a product whose case rests on showing its
    /// evidence, trading that for four words is the wrong trade.
    private var provenanceNote: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("The model recommends. Glomeris decides what may run.")
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(
                "Asking sends a summary of what Glomeris found to your provider "
                    + "and may cost money. "
                    + "Settings shows exactly what would be sent, without sending it."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.tertiary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var lastPlannedText: String {
        guard let lastPlannedAt = plan.lastPlannedAt else { return "not asked yet" }
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .medium
        return "asked \(formatter.string(from: lastPlannedAt))"
    }

    /// Beside the card title, and only once there is a plan with rows in it —
    /// "0 suggestions" next to a message that already says so in words is
    /// noise.
    private var countText: String? {
        guard case .plan(let report) = plan.outcome, !report.items.isEmpty else { return nil }
        return report.items.count == 1 ? "1 suggestion" : "\(report.items.count) suggestions"
    }

    // MARK: - Body states

    /// A message and rows are not mutually exclusive here, unlike in the
    /// candidates card: a provider that failed partway can still have returned
    /// usable suggestions, and `llm-plan` reports both. So the message comes
    /// first and the rows follow if there are any, rather than one replacing
    /// the other.
    @ViewBuilder
    private var outcomeBody: some View {
        if let message = AiPlanStateMessages.message(for: plan.outcome, isPlanning: plan.isPlanning) {
            GlomerisStateMessageView(message: message)
        }

        // HORO-1309: the one state with a remedy the user can act on from
        // here. Rendered as a control beside the message rather than as more
        // words inside it, because `AiPlanStateMessages` is a pure token->copy
        // mapper and putting a button in it would give the message-building
        // layer the ability to act.
        if plan.outcome == .notConfigured {
            GlomerisSettingsButton(title: "Set up an AI provider…")
        }

        if case .plan(let report) = plan.outcome {
            if !report.items.isEmpty {
                orderingNote
                rows(report.items)
            }

            // HORO-1550. Below the rows, because a per-row reading is what a
            // user came for and this is the model's account of the workspace as
            // a whole — and because reading it first would frame every row
            // beneath it as following from it.
            workspaceReadingBlock(report)

            ForEach(Self.discardedTexts(report), id: \.self) { text in
                Divider()
                Text(text)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            // HORO-1366. Embedded, not implemented here: this card renders what
            // a provider said and must stay a surface that cannot delete
            // anything — its tests assert its source contains no `execute`, no
            // `performClean` and no fingerprint token, and `ApplyPlanView` is a
            // sibling precisely so those guards keep holding. Below the rows
            // because a user should read the suggestions before being offered a
            // way to act on all of them, and the decision about whether to offer
            // one at all is made inside it from the items themselves.
            //
            // HORO-1550 hands it `recommend_now` items only. `disposition`
            // gains no authority by that — it cannot make anything executable,
            // and the machine gate inside `ApplyPlanView` is untouched and is
            // still the only thing that decides what may run. The filter moves
            // in one direction only: a bulk Apply never sweeps up a resource the
            // model itself declined to recommend now. Anything it holds back is
            // still reachable one row at a time through the detail view, which
            // is where a single deliberate decision belongs anyway.
            ApplyPlanView(
                plan: plan,
                items: Self.recommendedNowItems(report),
                client: client,
                projectRootsStore: projectRootsStore
            )
        }
    }

    /// Whose order this is. One line, above the rows, because the alternative
    /// — numbering the rows — would state the opposite of the truth in the
    /// app's own typography for machine judgments. See file header.
    private var orderingNote: some View {
        // HORO-1367: "The model's order" says what "In the order the model
        // suggested" said, in a third of the words. The second clause is the
        // load-bearing half and is untouched.
        Text("The model's order. Glomeris's own ranking is the list above.")
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.tertiary)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// The items given to `ApplyPlanView`: `recommend_now` and nothing else.
    ///
    /// Pure and `static` so the filter is assertable without a view, because the
    /// assertion worth having is the negative one — that a `keep` or a `defer`
    /// never reaches a bulk Apply.
    static func recommendedNowItems(_ report: WorkspacePlanReportDto) -> [LlmPlanItemReportDto] {
        report.items
            .filter { $0.disposition == Self.recommendNowDisposition }
            .map(\.item)
    }

    /// `planner::contract::Disposition::RecommendNow`'s tag. One spelling, so
    /// the filter above and its test cannot disagree about it.
    static let recommendNowDisposition = "recommend_now"

    /// What the model said that never made it onto the screen, stated rather
    /// than swallowed — and sorted into the three things that can actually have
    /// happened to it, because they are three different reasons to trust a
    /// provider differently.
    ///
    /// Pure and `static` so the wording is assertable without a view. A plan
    /// with three rows and two silently discarded items is not a three-row plan,
    /// and a user deciding how much to trust a provider is entitled to know it
    /// asked for things that do not exist.
    ///
    /// HORO-1550 replaced a single sentence over two counters with three
    /// sentences over seventeen, and the split is not cosmetic. *Discarded* means
    /// validation refused the thing outright. *Read as unknown* means it was
    /// kept, with a claim downgraded rather than believed — reporting that as
    /// "discarded" would be a lie in the direction of making the model look
    /// worse, and reporting it as nothing at all would be a lie in the direction
    /// of making it look better. *Cut short* means Glomeris's own bound stopped
    /// reading, which is Glomeris's doing and not the provider's, and blaming it
    /// on the provider would be the least honest reading of the three.
    static func discardedTexts(_ report: WorkspacePlanReportDto) -> [String] {
        let dropped = report.dropped
        var texts: [String] = []

        let refused: [(UInt32, String)] = [
            (dropped.unknownResource, "named a resource Glomeris never found"),
            (dropped.unofferedAction, "asked for an action Glomeris does not offer for it"),
            (dropped.unknownDisposition, "gave a recommendation that is not in the contract"),
            (dropped.duplicateItem, "named a resource the reply had already covered"),
            (dropped.unknownObservationKind, "reported an observation of an unknown kind"),
            (dropped.unknownProbe, "asked for a check Glomeris does not run"),
            (dropped.unknownProbeSubject, "asked about something Glomeris never mentioned"),
            (dropped.incompatibleProbeSubject, "asked a check about the wrong kind of subject"),
            (dropped.duplicateEvidenceRequest, "asked the same question twice"),
            (dropped.uncitedEvidenceRef, "cited evidence that was never sent to it"),
        ]
        if let sentence = Self.tally(
            refused,
            singular: "1 part of the reply was discarded before reaching this card",
            plural: "parts of the reply were discarded before reaching this card"
        ) {
            texts.append(sentence)
        }

        let degraded: [(UInt32, String)] = [
            (dropped.degradedUnknownConfidence, "a confidence that is not in the contract"),
            (dropped.degradedUnknownWorkflowMode, "a workflow shape that is not in the contract"),
        ]
        if let sentence = Self.tally(
            degraded,
            singular: "1 claim was read as unknown rather than taken at face value",
            plural: "claims were read as unknown rather than taken at face value"
        ) {
            texts.append(sentence)
        }

        let truncated: [(UInt32, String)] = [
            (dropped.truncatedItems, "suggestions"),
            (dropped.truncatedObservations, "observations"),
            (dropped.truncatedEvidenceRequests, "requests for more evidence"),
            (dropped.truncatedUncertainties, "uncertainties on a suggestion"),
            (dropped.truncatedEvidenceRefs, "evidence citations"),
        ]
        if let sentence = Self.tally(
            truncated,
            singular: "1 entry was cut short by Glomeris's own limit on how much a reply may say",
            plural: "entries were cut short by Glomeris's own limit on how much a reply may say"
        ) {
            texts.append(sentence)
        }

        return texts
    }

    /// One sentence over a family of counters, or `nil` when every one is zero.
    ///
    /// The total is stated as well as the breakdown, and it is summed from the
    /// same array the breakdown is built from rather than passed in separately,
    /// so a counter that is added to one and forgotten in the other cannot make
    /// the total and the reasons disagree.
    private static func tally(
        _ counters: [(UInt32, String)],
        singular: String,
        plural: String
    ) -> String? {
        let present = counters.filter { $0.0 > 0 }
        guard !present.isEmpty else { return nil }

        let total = present.reduce(UInt32(0)) { $0 + $1.0 }
        let subject = total == 1 ? singular : "\(total) \(plural)"
        let parts = present.map { "\($0.0) \($0.1)" }
        return "\(subject): \(parts.joined(separator: ", "))."
    }

    /// Rows are keyed by position, not by `resourceId`: a provider may name
    /// the same resource twice, and `ForEach` over duplicate identities
    /// misrenders. Position is also the honest identity here, since the order
    /// is the model's.
    @ViewBuilder
    private func rows(_ items: [WorkspacePlanItemReportDto]) -> some View {
        ForEach(Array(items.enumerated()), id: \.offset) { index, item in
            if index > 0 {
                Divider()
            }
            rowView(item)
        }
    }

    /// One suggestion.
    ///
    /// Reading order is machine-first, model-last, and the two are visually
    /// unrelated: badges and plain statements for what Glomeris determined, a
    /// ruled and italic quotation for what the provider said. The quotation
    /// is deliberately NOT a `GlomerisBadgeView` — a chip is this app's
    /// grammar for "a verdict was reached", and putting a model's sentence in
    /// one would launder an opinion into a finding.
    @ViewBuilder
    private func rowView(_ item: WorkspacePlanItemReportDto) -> some View {
        let row = AiPlanRowViewModel(item)

        Button {
            // By resource id, into the same detail view a candidates-list row
            // leads to. That view makes its own `explain` call and reads its
            // own `executable`/`requiresConfirmation`/`fingerprintToken`, so
            // the plan cannot shortcut consent — see file header.
            onOpenDetail(item.item.candidate.resourceId)
        } label: {
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                    Text(row.kindTerm.title)
                        .font(GlomerisDesign.primaryFont)
                        .lineLimit(1)
                    Spacer(minLength: GlomerisDesign.inlineSpacing)
                    GlomerisBadgeView(term: row.impactTerm, filled: false)
                }

                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    GlomerisBadgeView(term: row.safetyTerm)
                    if let impactTierTerm = row.impactTierTerm {
                        GlomerisBadgeView(term: impactTierTerm)
                    }
                    Spacer(minLength: 0)
                    // Affordance only — it carries no state, so it is hidden
                    // from VoiceOver rather than read out once per row.
                    Image(systemName: "chevron.right")
                        .imageScale(.small)
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }

                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    GlomerisBadgeView(term: row.completenessTerm, filled: false)
                    GlomerisBadgeView(term: row.confidenceTerm, filled: false)
                    Spacer(minLength: 0)
                }

                ForEach(Array(row.machineVerdictLines.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if let modelReading = row.modelReading {
                    modelQuote(modelReading)
                }

                GlomerisPathText(path: row.resourceId)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // HORO-1363: deliberately NO `.accessibilityElement(children: .ignore)`
        // here — same reason as the candidates-list row. `.ignore` replaced
        // this `Button` with a plain container in the live tree, so the row
        // read as `AXUnknown` with no actions and a VoiceOver user had no way
        // to open the suggestion. The explicit label below already collapses
        // the row to one element without discarding the role.
        .accessibilityLabel(row.accessibilityLabel)
        .accessibilityHint("Opens the evidence and the available actions for this resource.")
    }

    /// Everything the model said about one row, attributed and set apart: a
    /// leading rule, a `sparkles` label naming who said it, and italic secondary
    /// text. Three signals rather than one, so the attribution survives a user
    /// who cannot distinguish italics, a narrow popover that clips the rule, and
    /// VoiceOver — which gets the attribution in words from the row's own label.
    ///
    /// HORO-1550 widened this from the single sentence to the whole reading, and
    /// the widening is why the three signals matter more than they did. The
    /// disposition inside here is the field a user is most likely to read as a
    /// ruling, because it is phrased as a recommendation and it sits on a row
    /// that also carries a real one — so it renders in the same italic secondary
    /// text as the quote, inside the same rule, under the same heading, and never
    /// as a chip. The uncertainties are the reason the block can be tall: §13's
    /// rule is that missing evidence must never become negative evidence, and a
    /// count in place of the text would be exactly that.
    @ViewBuilder
    private func modelQuote(_ reading: AiPlanModelReadingViewModel) -> some View {
        HStack(alignment: .top, spacing: GlomerisDesign.inlineSpacing) {
            RoundedRectangle(cornerRadius: 1)
                .fill(.tertiary)
                .frame(width: 2)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 3) {
                    Image(systemName: "sparkles")
                        .imageScale(.small)
                    Text("The model says")
                        .font(GlomerisDesign.badgeFont)
                }
                .foregroundStyle(.secondary)

                modelSentence(reading.dispositionSentence)
                modelSentence(reading.confidenceSentence)
                if let quote = reading.quote {
                    modelSentence(quote)
                }

                if !reading.uncertainties.isEmpty {
                    Text("It could not settle:")
                        .font(GlomerisDesign.badgeFont)
                        .foregroundStyle(.tertiary)
                    ForEach(reading.uncertainties, id: \.self) { uncertainty in
                        modelSentence("• \(uncertainty)")
                    }
                }

                if !reading.evidenceRefs.isEmpty {
                    // The aliases, not the things they stand for. They are what
                    // makes the reply auditable and they are safe to show for
                    // the same reason they were safe to send: `resource_1` names
                    // nothing outside this one request.
                    Text("Cites \(reading.evidenceRefs.joined(separator: ", "))")
                        .font(GlomerisDesign.badgeFont)
                        .foregroundStyle(.tertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    /// One line of model text. Factored out so every sentence in the block above
    /// is demonstrably styled the same way — a disposition that quietly acquired
    /// a heavier font would be the start of it reading as a verdict.
    private func modelSentence(_ text: String) -> some View {
        Text(text)
            .font(GlomerisDesign.captionFont)
            .italic()
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    /// The model's account of the workspace as a whole, rather than of one
    /// resource: the workflow shape it thinks it is looking at, the conflicts and
    /// gaps it noticed, the read-only checks it wanted run, and — when a bounded
    /// expansion ran — what those checks actually came back with.
    ///
    /// One attributed block, same grammar as a row's: same rule, same `sparkles`
    /// heading, same italic secondary text. §17 forbids this becoming a second
    /// control plane, and there is nothing in it to press; it explains, and the
    /// Recovery Goal above stays the thing a user acts on.
    ///
    /// The probe findings are the part most easily got wrong, and the mistake
    /// would be cheap to make: a check that could not run reports a *reason*, and
    /// rendering that as silence — or worse, as an answer — is precisely the
    /// "failed probe becomes idle" confusion HORO-1551 has a mutation test for.
    /// So an unavailable finding says so, in words, naming the reason.
    @ViewBuilder
    private func workspaceReadingBlock(_ report: WorkspacePlanReportDto) -> some View {
        let profile = report.profile
        let hasContent =
            profile != nil
            || !report.observations.isEmpty
            || !report.evidenceRequests.isEmpty
            || report.expansion != nil

        if hasContent {
            Divider()
            HStack(alignment: .top, spacing: GlomerisDesign.inlineSpacing) {
                RoundedRectangle(cornerRadius: 1)
                    .fill(.tertiary)
                    .frame(width: 2)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 3) {
                        Image(systemName: "sparkles")
                            .imageScale(.small)
                        Text("The model's reading of this workspace")
                            .font(GlomerisDesign.badgeFont)
                    }
                    .foregroundStyle(.secondary)

                    if let profile {
                        modelSentence(Self.profileSentence(profile))
                        if let summary = profile.summary {
                            modelSentence(summary)
                        }
                    }

                    ForEach(Array(report.observations.enumerated()), id: \.offset) { _, item in
                        modelSentence("• \(Self.observationSentence(item))")
                    }

                    ForEach(Array(report.evidenceRequests.enumerated()), id: \.offset) { _, item in
                        modelSentence("• \(Self.evidenceRequestSentence(item))")
                    }

                    if let expansion = report.expansion {
                        Text(Self.expansionSentence(expansion))
                            .font(GlomerisDesign.badgeFont)
                            .foregroundStyle(.tertiary)
                            .fixedSize(horizontal: false, vertical: true)
                        ForEach(Array(expansion.findings.enumerated()), id: \.offset) { _, finding in
                            modelSentence("• \(Self.findingSentence(finding))")
                        }
                    }
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityElement(children: .combine)
        }
    }

    /// The workflow shape the model believes it is looking at, with its own
    /// confidence in that belief attached — never stated flat. §12's vocabulary,
    /// and §12's rule that this describes a *workspace* and never a person: there
    /// is no wording here that could be read as ranking the user.
    static func profileSentence(_ profile: WorkspaceProfileReportDto) -> String {
        let shape: String
        switch profile.mode {
        case "serial_single_checkout":
            shape = "one checkout at a time"
        case "serial_multi_branch":
            shape = "one checkout, moved between branches"
        case "parallel_multi_worktree":
            shape = "several working trees in parallel"
        case "mixed":
            shape = "a mix of one-at-a-time and parallel working trees"
        case "unknown":
            shape = "a shape it could not determine"
        default:
            shape = "a shape this app does not recognise (“\(profile.mode)”)"
        }

        let standing: String
        switch profile.confidence {
        case "observed": standing = "from evidence it was given"
        case "inferred": standing = "inferred rather than observed"
        case "unknown": standing = "and it says it does not know"
        default: standing = "with a confidence this app does not recognise"
        }

        return "It reads this machine as \(shape) — \(standing)."
    }

    /// One observation. The kind is spelled out rather than shown raw, because
    /// `conflicting_evidence` is the single most useful thing a reply can contain
    /// and a user should not have to learn the contract to read it.
    static func observationSentence(_ observation: WorkspaceObservationReportDto) -> String {
        let lead: String
        switch observation.kind {
        case "conflicting_evidence": lead = "Evidence that disagrees with itself"
        case "missing_evidence": lead = "Evidence it says it did not get"
        case "workflow_shape": lead = "On how this machine is used"
        case "resource_lifecycle": lead = "On where something is in its life"
        case "recovery_outlook": lead = "On how much there is to reclaim"
        default: lead = "An observation of a kind this app does not recognise"
        }
        // A detail-less observation still says something — that the model raised
        // this kind of point at all — so the kind is stated either way rather
        // than the whole entry disappearing.
        guard let detail = observation.detail else { return "\(lead)." }
        return "\(lead): \(detail)"
    }

    /// A read-only check the model asked for. Phrased as a request that was
    /// *made*, with no claim about whether it was granted: whether it ran at all
    /// is the expansion's business, and conflating the two would let "it asked"
    /// read as "it found out".
    static func evidenceRequestSentence(_ request: WorkspaceEvidenceRequestReportDto) -> String {
        let subject = "on \(request.subjectRef)"
        guard let reason = request.reason else {
            return "It asked Glomeris to check \(Self.probeName(request.probeId)) \(subject)."
        }
        return "It asked Glomeris to check \(Self.probeName(request.probeId)) \(subject): \(reason)"
    }

    /// `planner::contract::ProbeId`'s seven tags, in words.
    static func probeName(_ probeId: String) -> String {
        switch probeId {
        case "git_branch_state": return "a branch's state"
        case "git_patch_equivalence": return "whether the commits exist elsewhere"
        case "process_activity": return "whether anything is using it"
        case "tool_liveness": return "whether the owning tool is running"
        case "github_pr_state": return "any pull request"
        case "jira_task_state": return "any tracked task"
        case "workspace_history_summary": return "this machine's own history"
        default: return "something this app does not recognise (“\(probeId)”)"
        }
    }

    /// How far the bounded loop got, and why it stopped. Both halves, always:
    /// "3 of 3 rounds" without "it still had questions" would read as a run that
    /// finished, and §16's bounds exist precisely because one might not.
    static func expansionSentence(_ expansion: WorkspaceExpansionReportDto) -> String {
        let stop: String
        switch expansion.stoppedBecause {
        case "nothing_more_asked": stop = "it had nothing more to ask"
        case "round_limit": stop = "it reached Glomeris's limit on rounds, still asking"
        case "probe_limit": stop = "it reached Glomeris's limit on checks, still asking"
        case "time_limit": stop = "Glomeris's time limit ran out, with it still asking"
        case "provider_error": stop = "the provider call failed partway"
        default: stop = "of a reason this app does not recognise"
        }
        return "Glomeris answered \(expansion.probesRun) of an allowed "
            + "\(expansion.probesAllowed) checks over \(expansion.roundsRun) of "
            + "\(expansion.roundsAllowed) rounds, then stopped because \(stop)."
    }

    /// What one answered check came back with — or, when it could not be
    /// answered, that it could not be, and why.
    ///
    /// The `unavailableReason` branch is the load-bearing one. A check that
    /// timed out, hit a missing tool or was never attempted has told Glomeris
    /// nothing, and the one thing this line must never do is let that read as a
    /// negative finding (§13).
    static func findingSentence(_ finding: WorkspaceProbeFindingReportDto) -> String {
        let subject = "\(Self.probeName(finding.probeId)) on \(finding.subjectRef)"
        guard let reason = finding.unavailableReason else {
            return "Round \(finding.round): checked \(subject) — \(finding.finding)."
        }
        return "Round \(finding.round): could not check \(subject) — "
            + "\(Self.unavailableReasonPhrase(reason)). That is not an answer either way."
    }

    /// `planner::probe::ProbeReason`'s seven tags, in words. Every one of them
    /// means "no information", and none of them means "no".
    static func unavailableReasonPhrase(_ reason: String) -> String {
        switch reason {
        case "tool_absent": return "the tool that would answer it is not installed"
        case "tool_not_running": return "the tool that owns it is not running"
        case "permission_denied": return "Glomeris was not allowed to look"
        case "timed_out": return "it took too long"
        case "rate_limited": return "the service asked Glomeris to slow down"
        case "failed": return "the check itself failed"
        case "not_attempted": return "Glomeris did not run it"
        default: return "of a reason this app does not recognise (“\(reason)”)"
        }
    }

    // MARK: - The one call site

    /// The one and only place `llm-plan` is invoked in this file — always
    /// with `--json --progress-json`, always in direct response to the "Ask
    /// AI for a plan" button's action closure above, never from an
    /// appear-triggered task, a `Timer`, or a retry loop. A network call that
    /// may cost money must be something the user did, not something the
    /// popover did.
    ///
    /// `runRaw` rather than `run` on purpose: exit 1 still carries a full
    /// report on stdout. See the file header and `AiPlanInterpretation`.
    ///
    /// `@MainActor` for the same reason as `CandidatesSectionView.runDetect()`
    /// — read that doc comment for what the annotation does and does not claim
    /// — and with one extra beneficiary here: `plan.planTask` is a plain `var`
    /// on a shared object, written from this function and from the Ask button
    /// and read by Stop. With all three on the main actor there is no window in
    /// which a finishing request nils the handle while Stop is reading it, which
    /// would have cancelled nothing and left a child `glomeris llm-plan` running
    /// and a provider request still billable.
    ///
    /// `internal` rather than `private` so the tests can drive it against a
    /// pinned fixture binary. Its one production call site is still the Ask
    /// button.
    @MainActor
    func runLlmPlan() async {
        plan.isPlanning = true
        plan.progressStatusText = nil
        plan.lastErrorMessage = nil
        // HORO-1366: a preview or a result belongs to the plan it was made
        // from. Asking again replaces that plan, so anything the previous one
        // was going to do — or had done — stops being on screen with it. Left
        // behind, a preview would go on naming resources from a list the user
        // can no longer see, and its Apply button would still be live.
        plan.resetApplyState()

        do {
            // `withEnvironment` is what makes the Settings window's provider
            // configuration reach the CLI: this app is `LSUIElement` and, when
            // launched from Finder, inherits no shell environment at all, so
            // before HORO-1309 the documented `GLOMERIS_LLM_*` setup silently
            // did nothing here. `childEnvironment` is a COMPLETE environment,
            // not an overlay — `Process.environment` replaces wholesale, and
            // dropping PATH would break executable resolution.
            //
            // HORO-1368: `resolvedChildEnvironment`, which performs the
            // keychain read off the main thread. `runLlmPlan` is `@MainActor`,
            // so the synchronous version ran the read on the main thread — and
            // on a build whose code identity the stored item's ACL does not
            // admit, that read waits on a `SecurityAgent` prompt, during which
            // the app has no menu-bar item at all.
            let raw = try await client
                .withEnvironment(await settingsStore.resolvedChildEnvironment())
                .runRaw(
                    // HORO-1550: version 2, always. Not "2 if available" — a
                    // fallback would ask a provider twice for one question, and
                    // a silent downgrade would render a thinner answer as if it
                    // were the whole one. `--evidence-rounds` is deliberately
                    // absent: a multi-round expansion is several billed calls,
                    // and a button labelled "Ask AI for a plan" must not decide
                    // to make more than one of them.
                    projectRootsStore.scoped([
                        "llm-plan",
                        AiPlanInterpretation.contractVersionFlag,
                        "\(AiPlanInterpretation.expectedContractVersion)",
                        "--json",
                        "--progress-json",
                    ]),
                    progressType: ProgressEventDto.self,
                    onProgress: { event in
                        Task { @MainActor in
                            plan.progressStatusText = ProgressStatusText.text(for: event)
                        }
                    }
                )
            plan.outcome = AiPlanInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
            plan.lastPlannedAt = Date()
        } catch {
            // `nil` for a cancellation, and a sentence for everything else.
            // A stopped request deliberately leaves `lastPlannedAt` alone, so
            // any plan still on screen keeps its real timestamp instead of
            // claiming to be current.
            plan.lastErrorMessage = SectionFetchErrors.shortMessage(error, subject: "AI plan")
        }

        plan.isPlanning = false
        plan.progressStatusText = nil
        plan.planTask = nil
    }
}

#Preview {
    AiPlanSectionView(plan: PlanState())
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
