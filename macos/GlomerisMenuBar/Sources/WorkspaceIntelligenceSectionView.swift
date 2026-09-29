//
//  WorkspaceIntelligenceSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1550 AC 2 and AC 4: the two halves of workspace intelligence that had
//  no surface at all.
//
//  ============================================================================
//  WHAT WAS MISSING
//  ============================================================================
//  Two CLI subcommands existed and nothing in this app ran either of them:
//
//    * `workflow-profile show --json` — this machine's own recorded baseline,
//      which is the *historical* half of AC 2 ("current evidence and historical
//      habit are visually/semantically distinct"). With no surface for it, there
//      was nothing for today's evidence to be distinct *from*, and the campaign's
//      §12 rule — history is context only, current evidence always wins — was
//      a rule about something the user could not see.
//
//    * `external-context --json` — which optional lookups are set up, what one
//      may contribute to a model request, and what is never sent. That is AC 4
//      ("optional GitHub/Jira being disabled/unavailable is explained without
//      implying no PR/task exists") and it is the one claim this app could not
//      make, because a provider nobody configured produced no evidence and
//      therefore no row anywhere.
//
//  ============================================================================
//  THE TIER THIS CARD OCCUPIES, AND WHY IT SITS WHERE IT SITS
//  ============================================================================
//  Campaign §17 wants four things kept apart: local fact, remote supporting
//  context, AI inference, policy verdict. The panel now reads in that order —
//  "Reclaimable space" and "Developer projects" are local fact measured today,
//  this card is the recorded-habit and remote-setup context, "AI assistance" is
//  the model's reading, and the safety class on every row is the verdict. So
//  this card is deliberately *above* the AI Plan card and *below* the projects
//  it gives context for: a reader meets what is true now before they meet what
//  was true before, and both before they meet an inference drawn from either.
//
//  Neither group in it is a reading of the present. Both say so in their own
//  words, because on screen a card is read in whatever order the eye lands and
//  a heading alone is not a disclaimer.
//
//  ============================================================================
//  IT CANNOT ACT, STRUCTURALLY
//  ============================================================================
//  No `Button`, no `.disabled`, no mutating subcommand, no action id, no path
//  construction. The only controls are disclosure toggles, which reveal text —
//  the same property `WorkspaceFamiliesSectionView` has and
//  scripts/check-workspace-aggregation-has-no-authority.sh asserts. §17's rule
//  is that workspace intelligence explains WHY and does not become a second
//  control plane, and the cheapest way to keep that rule is to give the surface
//  nothing to act with.
//
//  Both reads are read-only, local, and free: `workflow-profile show` reads one
//  file this machine wrote and `external-context` performs no network request at
//  all — it reports configuration, and it exits 0 even when the config is
//  unparseable, putting the error *inside* the report. Neither takes
//  `--project-root`, so unlike `detect` there is nothing to scope and no reason
//  to thread the roots store in here.
//
//  ============================================================================
//  AN OLDER CLI IS ITS OWN ANSWER
//  ============================================================================
//  A `glomeris` predating either subcommand exits 2 with
//  `glomeris: unknown command '…'`. Rendering that as "no habit recorded" or
//  "nothing configured" would be the exact confusion this whole campaign is
//  about — §13's "missing evidence must never become negative evidence" — so it
//  is a fourth outcome with its own sentence, and that sentence says outright
//  that it is a missing command rather than a finding.
//

import SwiftUI

// MARK: - The two reads

/// The argument vectors, as values, so a test can assert them without running
/// anything.
///
/// Both are read-only and neither mutates a file: `show` is
/// `workflow-profile`'s default subcommand and the only one this app ever asks
/// for — `record` is what writes a baseline, it is the CLI's and the daemon's
/// business, and a panel that quietly recorded an observation every time it was
/// opened would be measuring itself.
enum WorkspaceContextCommands {
    static let workflowProfile = ["workflow-profile", "show", "--json"]
    static let externalContext = ["external-context", "--json"]
}

/// What one of the two reads produced.
///
/// Four cases, and the fourth is the one that earns this type: "the installed
/// CLI has no such command" is not a failure of the read, not malformed output,
/// and above all not an empty answer. Modelled as a value so the distinction is
/// assertable without rendering anything.
enum WorkspaceContextOutcome<Report: Equatable>: Equatable {
    case report(Report)

    /// Exit 2 with `unknown command` on stderr. Carries the line verbatim.
    case cliTooOld(String)

    /// Exit 0 and stdout this app could not decode.
    case malformedOutput

    /// Anything else, with whatever the CLI or the spawn said.
    case failed(String)
}

/// Turns one raw run into an outcome.
///
/// Decodes before judging the exit code, for the same reason
/// `AutopilotEnvelopeInterpretation` does: a printed report is the best account
/// of what was read, whatever the exit code says. `external-context` in
/// particular exits 0 *and* reports a configuration error inside the JSON, and
/// that error belongs on screen rather than being replaced by a spawn-level
/// failure sentence.
enum WorkspaceContextInterpretation {
    /// The top-level arm in `src/main.rs`. Matched on the *command* wording and
    /// not on "unrecognized argument", which is what a rejected *flag* produces
    /// — that one is this app sending something wrong and stays a plain failure.
    static let unknownCommandMarker = "unknown command"

    static func interpret<Report: Decodable & Equatable>(
        _ type: Report.Type,
        exitCode: Int32,
        stdout: Data,
        stderr: Data
    ) -> WorkspaceContextOutcome<Report> {
        if let report = try? JSONDecoder().decode(Report.self, from: stdout) {
            return .report(report)
        }

        let stderrText = String(decoding: stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)

        if exitCode == 2, stderrText.contains(Self.unknownCommandMarker) {
            return .cliTooOld(stderrText)
        }
        if exitCode == 0 {
            return .malformedOutput
        }
        return .failed(
            stderrText.isEmpty
                ? "glomeris exited with code \(exitCode) and printed no result."
                : stderrText
        )
    }
}

// MARK: - What the card says

/// The card's wording, as values.
///
/// Every sentence that matters here is a sentence about what Glomeris does
/// *not* know, so the wording is the feature and is pinned by tests rather than
/// left to the view body.
enum WorkspaceIntelligencePresentation {
    static let cardTitle = "Workspace intelligence"

    /// The card's own scope, stated once. The second half is §17's rule in the
    /// place a user would otherwise have to infer it.
    static let purpose =
        "Context for the lists above: how this machine has been worked in before, and which "
        + "optional lookups are set up. Nothing here reclaims anything — the recovery goal above "
        + "is still where that happens."

    // MARK: Recorded habit

    static let habitHeading = "Previously recorded habit"

    static let habitLoadingSubject = "Reading this machine's recorded habit…"

    /// AC 2, in words rather than only in layout.
    ///
    /// On screen the two halves are separated by being in different cards, and
    /// that separation is invisible to somebody reading one card. §12's rule is
    /// not "show history elsewhere", it is "current evidence always wins", and
    /// that is a claim a reader has to be told.
    static let habitIsNotTodaySentence =
        "This is a record of earlier looks at this machine, not a reading of what is happening "
        + "now. Where it disagrees with what was measured today, today's measurement is the one "
        + "that counts."

    /// What to show when the baseline was never written.
    ///
    /// `notLookedYet` rather than `empty`: the default checkmark would read as
    /// "I looked, and there is no pattern here", and no look has happened.
    static let noHabitRecordedMessage = GlomerisStateMessage.notLookedYet(
        "No habit recorded yet",
        detail: "Glomeris has not written a baseline for this machine, so there is nothing to "
            + "compare today against. That is a measurement that has not been taken, not a "
            + "finding about how this machine is used."
    )

    /// Why an unreadable baseline says nothing in either direction.
    ///
    /// Unterminated: `SpokenLabel.compose` terminates it.
    static let unreadableIsNotAFindingSentence =
        "Nothing about how this machine is used can be read off that, in either direction"

    static func unreadableHabitMessage(_ reason: String?) -> GlomerisStateMessage {
        // The probe-reason vocabulary is shared with the AI card on purpose:
        // `tool_absent` means the same thing wherever it appears, and two
        // wordings for one tag is how a surface starts disagreeing with itself.
        let lead = reason.map { "The recorded baseline could not be read — "
            + AiPlanSectionView.unavailableReasonPhrase($0) }
            ?? "The recorded baseline could not be read, and glomeris did not say why"
        return .failure(SpokenLabel.compose([lead, Self.unreadableIsNotAFindingSentence]))
    }

    /// `WorkflowProfileReport.state`'s three tags, plus an unrecognised fourth.
    ///
    /// Returns `nil` for `collected`, which is the one case with rows to draw.
    /// Three presentations and not two because `never_collected` and
    /// `unreadable` have different next steps, and a single "no history" would
    /// send somebody to re-run a recorder that is running fine.
    static func habitStateMessage(_ report: WorkflowProfileReportDto) -> GlomerisStateMessage? {
        switch report.state {
        case "collected":
            return nil
        case "never_collected":
            return Self.noHabitRecordedMessage
        case "unreadable":
            return Self.unreadableHabitMessage(report.unreadableReason)
        default:
            return .failure(
                "The baseline reported a state this app does not recognise (“\(report.state)”), so "
                    + "nothing is stated from it."
            )
        }
    }

    /// How many looks, over how long, across how many repositories.
    ///
    /// Counts and never adjectives. §12 forbids describing a *user* as advanced,
    /// beginner, good, bad or expert, and the safe way to hold that line is for
    /// this surface to have no evaluative vocabulary at all — it reports numbers
    /// and a shape, and the shape is a property of the working trees.
    static func observationsSentence(_ report: WorkflowProfileReportDto) -> String {
        let looks = Self.count(UInt64(report.observationCount), "look", "looks")
        let days = Self.count(UInt64(report.support.spanningDays), "day", "days")
        let repositories = Self.count(
            UInt64(report.support.repositoriesObserved), "repository", "repositories")
        return "Glomeris has recorded \(looks) at this machine over \(days), across "
            + "\(repositories)."
    }

    /// The pattern, or the honest statement that there is not one yet.
    ///
    /// The `insufficient` branch is the load-bearing one, and it is why
    /// `mode: "unknown", confidence: "insufficient"` must never render as
    /// "workflow: unknown": a measurement with an answer pending is not a
    /// measurement that came back empty.
    static func patternSentence(_ report: WorkflowProfileReportDto) -> String {
        switch report.confidence {
        case "observed":
            return "The pattern it has recorded is \(Self.habitShape(report.mode))."
        case "insufficient":
            let more = Self.count(UInt64(report.observationsStillNeeded), "look", "looks")
            return "It has not named a pattern yet and needs \(more) more before it will. A "
                + "pattern it has not named is not a finding that this machine has none."
        default:
            return "It reported a confidence this app does not recognise "
                + "(“\(report.confidence)”), so no pattern is stated here."
        }
    }

    /// `workspace::WorkflowMode`'s five tags, worded as a property of the
    /// working trees rather than of whoever uses them.
    static func habitShape(_ mode: String) -> String {
        switch mode {
        case "serial_single_checkout": return "one checkout at a time"
        case "serial_multi_branch": return "one checkout, moved between branches"
        case "parallel_multi_worktree": return "several working trees at once"
        case "mixed": return "a mix of one-at-a-time and parallel working trees"
        case "unknown": return "not a shape it could settle on"
        default: return "a shape this app does not recognise (“\(mode)”)"
        }
    }

    /// The counts the pattern was read off, for a reader who wants to check it.
    ///
    /// Behind a disclosure, because they are the working and the sentence above
    /// is the answer. Nothing here is load-bearing for the not-a-finding claims,
    /// which stay always-visible.
    static func supportLines(_ support: WorkflowSupportReportDto) -> [String] {
        [
            "Looks that saw several working trees at once: \(support.parallelObservations)",
            "Looks that saw one: \(support.serialObservations)",
            "Looks that saw both kinds: \(support.mixedObservations)",
            "Times one checkout had changed branch since the look before: "
                + "\(support.singleCheckoutBranchChanges)",
            "Most working trees seen at once: \(support.mostWorktreesSeenAtOnce)",
        ]
    }

    static func retentionSentence(_ report: WorkflowProfileReportDto) -> String {
        "Kept for \(Self.count(report.retentionDays, "day", "days")), and at most one look every "
            + "\(Self.durationPhrase(report.minimumIntervalSecs)) is recorded."
    }

    // MARK: Optional remote context

    static let remoteHeading = "Optional remote context"

    static let remoteLoadingSubject = "Reading the optional-lookup setup…"

    /// AC 4, and the sentence the whole group exists for.
    ///
    /// Three situations — off, not set up, unreachable — collapse in a reader's
    /// head into "so there is no pull request", and campaign §10 says they must
    /// never be the same value as "no result". Stated once, above the providers,
    /// so it covers all three whatever any individual row says.
    static let remoteSilenceIsNotAnAnswerSentence =
        "Off, not set up and unreachable all mean the same thing: Glomeris learned nothing. None "
        + "of them means that no pull request and no task exist."

    static func remoteEnabledSentence(_ report: ExternalContextPreviewReportDto) -> String {
        report.enabled
            ? "At least one optional lookup is configured. It is asked only while a plan is being "
                + "made, and only for the fields listed below."
            : "No optional lookup is configured, so Glomeris asked GitHub and Jira nothing and "
                + "sent them nothing."
    }

    /// A configuration file that would not parse.
    ///
    /// First on screen, for the reason Rust's own prose gives: everything below
    /// it is the empty default rather than anything read from the file, and a
    /// reader who missed that would take "nothing configured" for a fact about
    /// their setup.
    static func configErrorMessage(
        _ report: ExternalContextPreviewReportDto
    ) -> GlomerisStateMessage? {
        guard let error = report.configError else { return nil }
        return .failure(
            SpokenLabel.compose([
                "The optional-lookup configuration could not be read",
                error,
                "What is set up here is therefore unknown rather than empty",
            ])
        )
    }

    /// `workspace::ExternalSource`'s tags, in words. An unrecognised one is
    /// shown as it arrived rather than dropped — it is a tag, not a name.
    static func providerName(_ source: String) -> String {
        switch source {
        case "github_pull_requests": return "GitHub pull requests"
        case "jira_issues": return "Jira issues"
        default: return source
        }
    }

    /// One provider's setup state.
    ///
    /// Three states from three fields rather than one status word, because §10
    /// forbids folding them: nobody set this up, and a service refused the
    /// credential, are different situations with different next steps.
    static func providerStateSentence(_ provider: ExternalProviderPreviewReportDto) -> String {
        if !provider.configured { return "not set up on this machine" }
        if provider.ready { return "set up, and its credential variable is present" }
        return "set up, but not usable"
    }

    /// What a provider is asked about, and from where. Never a credential value
    /// — `credentialEnv` is the *name* of a variable, which is the only part of
    /// a credential that ever appears anywhere in this product.
    static func providerDetailLines(
        _ provider: ExternalProviderPreviewReportDto
    ) -> [String] {
        var lines: [String] = []
        if let host = provider.repositoryHost {
            lines.append("Asked only about working trees whose remote is on \(host)")
        }
        if let endpoint = provider.endpoint {
            lines.append("Endpoint: \(endpoint)")
        }
        if let variable = provider.credentialEnv {
            lines.append("Credential read from \(variable) — never its value")
        }
        return lines
    }

    /// One field that may reach a model, with every value it can carry.
    ///
    /// Rust derives these from the enums it actually serializes, so this is a
    /// rendering and not a second list to keep in step.
    static func egressFieldSentence(_ field: ExternalEgressFieldReportDto) -> String {
        let shape: String
        switch field.shape {
        case "token": shape = "one of a fixed set of words"
        case "status": shape = "one of a fixed set of words"
        case "days": shape = "a whole number of days"
        default: shape = "a shape this app does not recognise (“\(field.shape)”)"
        }
        guard !field.vocabulary.isEmpty else { return "\(field.field) — \(shape)" }
        return "\(field.field) — \(shape): \(field.vocabulary.joined(separator: ", "))"
    }

    /// The heading over the egress list, which states the "and nothing else"
    /// that makes the list worth printing.
    static func egressHeadingText(_ count: Int) -> String {
        count == 1
            ? "1 field may reach a model, and nothing else"
            : "\(count) fields may reach a model, and nothing else"
    }

    static let neverSentHeading = "Never sent, to a provider or to a model"

    // MARK: Shared

    static let malformedOutputMessage = GlomerisStateMessage.failure(
        "glomeris printed something this app could not read. The app and the CLI are probably "
            + "different versions."
    )

    /// An installed CLI with no such command.
    ///
    /// `subject` is the command that is missing and `consequence` is what a
    /// reader must not conclude from its absence. Both halves are required: the
    /// first is what happened and the second is the mistake §13 exists to stop.
    static func cliTooOldMessage(
        command: String,
        consequence: String,
        detail: String
    ) -> GlomerisStateMessage {
        .failure(
            SpokenLabel.compose([
                "The installed glomeris has no `\(command)` command, so this app could not ask it. "
                    + consequence,
                detail,
            ])
        )
    }

    static func habitCliTooOldMessage(_ detail: String) -> GlomerisStateMessage {
        Self.cliTooOldMessage(
            command: "workflow-profile",
            consequence: "That is a missing command, not a machine with no recorded habit",
            detail: detail
        )
    }

    static func remoteCliTooOldMessage(_ detail: String) -> GlomerisStateMessage {
        Self.cliTooOldMessage(
            command: "external-context",
            consequence: "That is a missing command, not a setup with nothing configured, and it "
                + "is not evidence that no pull request or task exists",
            detail: detail
        )
    }

    /// `"1 look"` / `"3 looks"`. A count with its noun, because "1 looks" in a
    /// privacy-adjacent sentence reads as a surface nobody checked.
    static func count(_ amount: UInt64, _ singular: String, _ plural: String) -> String {
        "\(amount) \(amount == 1 ? singular : plural)"
    }

    /// A seconds interval as the unit it divides into. `3600` is the shipped
    /// value and reads as "hour", not "3600 seconds".
    static func durationPhrase(_ seconds: UInt64) -> String {
        if seconds >= 3600, seconds % 3600 == 0 {
            let hours = seconds / 3600
            return hours == 1 ? "hour" : "\(hours) hours"
        }
        if seconds >= 60, seconds % 60 == 0 {
            let minutes = seconds / 60
            return minutes == 1 ? "minute" : "\(minutes) minutes"
        }
        return seconds == 1 ? "second" : "\(seconds) seconds"
    }
}

// MARK: - The card

/// Popover section: this machine's recorded habit, and which optional lookups
/// are set up.
///
/// Read once per panel appearance rather than on a timer. Neither answer moves
/// while the panel is open — a baseline is written by `workflow-profile record`
/// and the config by editing a file, and doing either means leaving this panel.
struct WorkspaceIntelligenceSectionView: View {
    private let client: GlomerisClient

    /// `nil` means the read has not finished. Every code path in `load()` sets
    /// both to something, including the `catch`, so `nil` can never be a failure
    /// wearing a spinner.
    @State private var workflow: WorkspaceContextOutcome<WorkflowProfileReportDto>?
    @State private var external: WorkspaceContextOutcome<ExternalContextPreviewReportDto>?

    init(client: GlomerisClient = GlomerisClient()) {
        self.client = client
    }

    var body: some View {
        GlomerisCard(title: WorkspaceIntelligencePresentation.cardTitle) {
            Text(WorkspaceIntelligencePresentation.purpose)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            habitGroup
            Divider()
            remoteGroup
        }
        .task { await load() }
    }

    // MARK: - Recorded habit

    @ViewBuilder
    private var habitGroup: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            groupHeading(
                WorkspaceIntelligencePresentation.habitHeading,
                symbolName: "clock.arrow.circlepath"
            )

            // Always visible, in every state including the failures: it is the
            // claim the group makes about its own authority, and a state that
            // dropped it would be a state where the card said nothing about
            // whether history outranks today.
            Text(WorkspaceIntelligencePresentation.habitIsNotTodaySentence)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            switch workflow {
            case nil:
                GlomerisStateMessageView(
                    message: .loading(WorkspaceIntelligencePresentation.habitLoadingSubject)
                )
            case .report(let report)?:
                habitReport(report)
            case .cliTooOld(let detail)?:
                GlomerisStateMessageView(
                    message: WorkspaceIntelligencePresentation.habitCliTooOldMessage(detail)
                )
            case .malformedOutput?:
                GlomerisStateMessageView(
                    message: WorkspaceIntelligencePresentation.malformedOutputMessage
                )
            case .failed(let detail)?:
                GlomerisStateMessageView(message: .failure(detail))
            }
        }
    }

    @ViewBuilder
    private func habitReport(_ report: WorkflowProfileReportDto) -> some View {
        if let message = WorkspaceIntelligencePresentation.habitStateMessage(report) {
            GlomerisStateMessageView(message: message)
        } else {
            VStack(alignment: .leading, spacing: 2) {
                Text(WorkspaceIntelligencePresentation.observationsSentence(report))
                Text(WorkspaceIntelligencePresentation.patternSentence(report))
            }
            .font(GlomerisDesign.secondaryFont)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                SpokenLabel.compose([
                    WorkspaceIntelligencePresentation.observationsSentence(report),
                    WorkspaceIntelligencePresentation.patternSentence(report),
                ])
            )

            // Verbatim, because it is Rust's own statement of what this baseline
            // may and may not do, and paraphrasing a limit on one's own
            // authority is how a limit loosens.
            Text(report.authority)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            DisclosureGroup {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(
                        WorkspaceIntelligencePresentation.supportLines(report.support),
                        id: \.self
                    ) { line in
                        Text(line)
                    }
                    Text(WorkspaceIntelligencePresentation.retentionSentence(report))
                    if let storedAt = report.storedAt {
                        Text("Last recorded \(storedAt)")
                    }
                }
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 2)
            } label: {
                Text("What that was read from")
                    .font(GlomerisDesign.captionFont)
            }
        }
    }

    // MARK: - Optional remote context

    @ViewBuilder
    private var remoteGroup: some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            groupHeading(
                WorkspaceIntelligencePresentation.remoteHeading,
                symbolName: "antenna.radiowaves.left.and.right"
            )

            // Always visible, for the same reason as the habit group's sentence
            // and with more riding on it: this is AC 4, and it has to hold in
            // the states where nothing was read at all.
            Text(WorkspaceIntelligencePresentation.remoteSilenceIsNotAnAnswerSentence)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            switch external {
            case nil:
                GlomerisStateMessageView(
                    message: .loading(WorkspaceIntelligencePresentation.remoteLoadingSubject)
                )
            case .report(let report)?:
                remoteReport(report)
            case .cliTooOld(let detail)?:
                GlomerisStateMessageView(
                    message: WorkspaceIntelligencePresentation.remoteCliTooOldMessage(detail)
                )
            case .malformedOutput?:
                GlomerisStateMessageView(
                    message: WorkspaceIntelligencePresentation.malformedOutputMessage
                )
            case .failed(let detail)?:
                GlomerisStateMessageView(message: .failure(detail))
            }
        }
    }

    @ViewBuilder
    private func remoteReport(_ report: ExternalContextPreviewReportDto) -> some View {
        if let message = WorkspaceIntelligencePresentation.configErrorMessage(report) {
            GlomerisStateMessageView(message: message)
        }

        Text(WorkspaceIntelligencePresentation.remoteEnabledSentence(report))
            .font(GlomerisDesign.secondaryFont)
            .fixedSize(horizontal: false, vertical: true)

        ForEach(report.providers) { provider in
            providerView(provider)
        }

        if !report.egressFields.isEmpty {
            DisclosureGroup {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(report.egressFields) { field in
                        Text(WorkspaceIntelligencePresentation.egressFieldSentence(field))
                    }
                }
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 2)
            } label: {
                Text(WorkspaceIntelligencePresentation.egressHeadingText(report.egressFields.count))
                    .font(GlomerisDesign.captionFont)
            }
        }

        DisclosureGroup {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(report.neverSent, id: \.self) { item in
                    Text("• \(item)")
                }
            }
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.top, 2)
        } label: {
            Text(WorkspaceIntelligencePresentation.neverSentHeading)
                .font(GlomerisDesign.captionFont)
        }
    }

    /// One provider, read as one accessibility element.
    private func providerView(_ provider: ExternalProviderPreviewReportDto) -> some View {
        let name = WorkspaceIntelligencePresentation.providerName(provider.source)
        let state = WorkspaceIntelligencePresentation.providerStateSentence(provider)
        let details = WorkspaceIntelligencePresentation.providerDetailLines(provider)

        return VStack(alignment: .leading, spacing: 2) {
            Text("\(name): \(state)")
                .font(GlomerisDesign.secondaryFont)
                .fixedSize(horizontal: false, vertical: true)

            // Verbatim. A refusal names the variable to set, and rewording it
            // would send somebody to the wrong file.
            if let refusal = provider.refusal {
                Text(refusal)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            ForEach(details, id: \.self) { line in
                Text(line)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            SpokenLabel.compose(
                [SpokenLabel.clause(name, state), provider.refusal] + details.map { Optional($0) }
            )
        )
    }

    // MARK: - Shared chrome

    private func groupHeading(_ title: String, symbolName: String) -> some View {
        HStack(spacing: 3) {
            Image(systemName: symbolName)
                .imageScale(.small)
            Text(title)
                .font(GlomerisDesign.badgeFont)
        }
        .foregroundStyle(.secondary)
        .accessibilityAddTraits(.isHeader)
    }

    // MARK: - Fetching

    /// Both reads, each into its own slot.
    ///
    /// Separate slots for HORO-1297's reason: an older CLI answers one of these
    /// and not the other, and a shared error slot would let whichever returned
    /// last erase the other's failure. One after the other rather than
    /// concurrently — both are single local reads with nothing to wait on, and a
    /// second spawn overlapping the first buys milliseconds at the cost of two
    /// `glomeris` processes every time the panel opens.
    ///
    /// Nothing here is retried or polled.
    private func load() async {
        workflow = await readWorkflowProfile()
        external = await readExternalContext()
    }

    private func readWorkflowProfile() async -> WorkspaceContextOutcome<WorkflowProfileReportDto> {
        do {
            let raw = try await client.runRaw(
                WorkspaceContextCommands.workflowProfile,
                progressType: ProgressEventDto.self
            )
            return WorkspaceContextInterpretation.interpret(
                WorkflowProfileReportDto.self,
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
        } catch {
            return .failed(
                SectionFetchErrors.shortMessage(error, subject: "workflow-profile show")
                    ?? "The read of this machine's recorded habit did not finish."
            )
        }
    }

    private func readExternalContext() async
        -> WorkspaceContextOutcome<ExternalContextPreviewReportDto>
    {
        do {
            let raw = try await client.runRaw(
                WorkspaceContextCommands.externalContext,
                progressType: ProgressEventDto.self
            )
            return WorkspaceContextInterpretation.interpret(
                ExternalContextPreviewReportDto.self,
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
        } catch {
            return .failed(
                SectionFetchErrors.shortMessage(error, subject: "external-context")
                    ?? "The read of the optional-lookup setup did not finish."
            )
        }
    }
}

#Preview {
    WorkspaceIntelligenceSectionView()
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
