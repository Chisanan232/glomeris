//
//  CandidateActionability.swift
//  GlomerisMenuBar
//
//  HORO-1323: the one place this app turns the CLI's actionability triple
//  into wording.
//
//  The triple is `executable`, the matching offered action's
//  `requires_confirmation`, and `refusal_reason` — computed together in
//  `executable_fields` (`src/reporting/dto.rs`) and carried identically by
//  `DetectCandidateReportDto` and `ExplainReportDto`. Three separate places
//  in this app had to say something about it: the candidates list, the AI
//  plan card and the candidate detail's Clean button. Only the AI plan card
//  actually did, in a private function of its own, and the other two said
//  nothing at all — which is the defect this ticket was filed for. A user
//  could only find out that a resource cannot be cleaned by opening it and
//  finding the button dimmed.
//
//  ============================================================================
//  WHY THIS IS WORDING AND NOT A DECISION
//  ============================================================================
//  The standing project rule (GlomerisMenuBarApp.swift) forbids policy
//  classification in this target, and this ticket's own scope says nothing
//  here may branch on a policy label. Two properties keep that checkable
//  rather than merely asserted:
//
//    1. This file imports Foundation only. It cannot build a control, cannot
//       disable one and cannot spawn the CLI.
//    2. It exposes no boolean. There is deliberately no `canClean` property,
//       because a boolean here would be the obvious thing for a future
//       `.disabled(...)` to read, and then the enablement of the one control
//       in this app that deletes files would depend on a display type. The
//       Clean button still reads `ExplainReportDto.executable` directly, in
//       `CandidateDetailView`, exactly as it did before — see
//       `CandidateDetailViewTests`, which proves that with fixtures whose
//       `policy_label` deliberately contradicts `executable`.
//
//  What it exposes instead is a sentence and an optional badge term. A
//  sentence cannot authorise anything.
//
//  ============================================================================
//  WHY THE REFUSAL TEXT IS PASSED THROUGH VERBATIM
//  ============================================================================
//  `executable_fields` produces four distinct shapes of `refusal_reason`,
//  and which one you get is the only honest way to tell the cases apart:
//
//    PROTECTED: <reason code>                     policy refused it
//    no registered cleanup action for this ...     nothing is registered
//    <action id> cannot be planned for this ...    the planner said no
//    <executor::structural_refusal's own words>    it could never be run
//
//  The last of those is the Homebrew case HORO-1358 exposed: an AUTO_SAFE
//  resource whose step has no `scoped_path`, so the executor rejects it on
//  sight regardless of policy class. `offered_actions` is empty in ALL FOUR
//  cases — verified against a live `detect --json`, where every one of the
//  three non-executable candidates on the machine reported `[]` — so the
//  array's emptiness cannot distinguish a safety refusal from a missing
//  action. The reason text can, and it is Rust's text.
//
//  So this file never rewords a refusal, never pattern-matches one, and in
//  particular never looks for the substring "PROTECTED" to decide what to
//  say. Reconstructing that distinction in Swift is exactly what the thin
//  client rule forbids, and it would go quietly wrong the first time Rust
//  added a fifth shape. The generic sentence below is reached only when the
//  CLI reported no reason at all.
//

import Foundation

/// What Glomeris will do about one candidate, in words, derived from the
/// CLI's actionability fields and nothing else.
///
/// Four cases rather than one "disabled" state, because the ticket's whole
/// point is that they are different situations for the user: two of them
/// clear by themselves (act now, or confirm and act), one is a property of
/// the resource that will not change, and the last means the CLI told us
/// nothing we can pass on.
///
/// Deliberately NOT a case for "temporarily stale". Staleness is discovered
/// by revalidation at deletion time, and no pre-execution field reports it —
/// `explain` cannot know it. Inventing an overview state for it would be a
/// claim this app has no evidence for; it already surfaces distinctly, and
/// truthfully, as `describeExecuteOutcome`'s `aborted_by_revalidation`
/// message once a run has actually been attempted.
enum CandidateActionability: Equatable {
    /// `executable` is `true` and no offered action asks for confirmation.
    case readyToClean
    /// `executable` is `true` and the offered action asks first.
    case asksFirstThenCleans
    /// `executable` is `false`, with the CLI's own sentence for why.
    case refused(reason: String)
    /// `executable` is `false` and no `refusal_reason` came back. Should not
    /// happen — every branch of `executable_fields` returns a reason — but a
    /// silent empty state here would be the worst possible failure mode for
    /// this particular ticket, so it gets wording of its own.
    case refusedWithoutStatedReason

    /// Reads the fields, in the order the CLI computes them. No string
    /// inspection of the reason, and no reference to the policy label.
    init(executable: Bool, requiresConfirmation: Bool, refusalReason: String?) {
        if executable {
            self = requiresConfirmation ? .asksFirstThenCleans : .readyToClean
            return
        }
        if let refusalReason, !refusalReason.isEmpty {
            self = .refused(reason: refusalReason)
            return
        }
        self = .refusedWithoutStatedReason
    }

    /// The array-taking form, for the two call sites that hold the DTO's
    /// `offered_actions` rather than an already-reduced boolean.
    ///
    /// `first?` rather than `contains`, and the distinction is a safety one.
    /// Rust offers at most one action today so the two agree, but `first` is
    /// the only element that can ever execute: `CandidateDetailViewModel`
    /// takes its `actionId` from `offeredActions.first`, gates the
    /// confirmation alert on `offeredActions.first?.requiresConfirmation`, and
    /// passes that one action's id to `execute`. A reduction saying "any
    /// offered action asks first" would, the moment Rust offered two, let the
    /// overview promise a confirmation step that the button then skipped — the
    /// user would be told their deletion needed confirming and would get an
    /// immediate, unconfirmed deletion instead. Describing the action that
    /// runs is the only reading that cannot produce that.
    init(executable: Bool, offeredActions: [OfferedActionDto], refusalReason: String?) {
        self.init(
            executable: executable,
            requiresConfirmation: offeredActions.first?.requiresConfirmation ?? false,
            refusalReason: refusalReason
        )
    }

    static let axis = "Cleanup"

    /// One sentence, always present. The refusal cases return Rust's own
    /// words unchanged; only the two permitted cases are wording this app
    /// chose, and they describe what the user can do rather than what policy
    /// decided.
    ///
    /// The two permitted sentences are carried over verbatim from
    /// `AiPlanRowViewModel.verdictLines`, which is where they were written,
    /// so moving them here changes no copy anywhere.
    var sentence: String {
        switch self {
        case .readyToClean:
            return "Glomeris is willing to run this. Open the row to review it and clean."
        case .asksFirstThenCleans:
            return "Glomeris will ask you to confirm this before anything runs."
        case .refused(let reason):
            return reason
        case .refusedWithoutStatedReason:
            return "Glomeris has no action it is willing to run for this resource."
        }
    }

    /// A badge for the overview, or `nil` when there is nothing there a
    /// reader needs called out.
    ///
    /// # Why the permitted cases return nil
    ///
    /// The same reason `GlomerisVocabulary.impactTier` does: a chip on every
    /// row is not emphasis, it is noise. It is also a duplicate — a row
    /// already carries its safety badge, which reads "Asks first" for ASK
    /// and "Safe to reclaim" for AUTO_SAFE, so a second chip saying
    /// substantially the same thing would spend the user's attention twice
    /// and make the one row that DOES need calling out harder to spot.
    ///
    /// The refusal cases are the information the overview was missing
    /// entirely, and the arrangement is deliberate: an AUTO_SAFE candidate
    /// whose action can never run now shows "Safe to reclaim" beside
    /// "Cannot be cleaned", which looks contradictory and is exactly the
    /// truth HORO-1358 established. Those are two different axes — what
    /// policy permits, and whether an action exists that could be carried
    /// out — and collapsing them is what made the Homebrew case so
    /// confusing in the first place.
    ///
    /// `.guarded`, never `.critical`: a refusal is Glomeris working, and the
    /// user has nothing to fix. Colouring it red would teach them the
    /// product is broken every time it correctly declines.
    var term: GlomerisTerm? {
        switch self {
        case .readyToClean, .asksFirstThenCleans:
            return nil
        case .refused, .refusedWithoutStatedReason:
            return GlomerisTerm(
                // The field this wording came from, stringified — the same
                // convention `GlomerisVocabulary.monitorLoaded` uses for a
                // boolean. There is no CLI enum token for this one.
                token: "false",
                axis: Self.axis,
                title: "Cannot be cleaned",
                explanation: sentence,
                symbolName: "slash.circle.fill",
                tone: .guarded
            )
        }
    }
}
