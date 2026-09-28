//
//  AutopilotSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1510 AC 2, standing half: automatic mode is opt-in and *visibly
//  distinct* from notification-only mode — in the panel, on an ordinary day.
//
//  ---------------------------------------------------------------------
//  Why the two surfaces that already existed were not enough
//  ---------------------------------------------------------------------
//  The Autopilot pane says which mode was chosen, and the pressure-alert card
//  says which one is in force at the moment it matters. Neither is the panel a
//  user opens when nothing is wrong, and on that day the two Macs were
//  indistinguishable: one that will ask before deleting anything, and one that
//  will not. That is the distinction AC 2 is about, so it is stated where the
//  panel is looked at rather than only where the grant is edited.
//
//  This is also campaign §13's Autopilot position — after AI assistance, before
//  History — which the panel had no section for because the *controls* live in
//  the Settings scene. They stay there: a bounded grant with a dozen knobs on a
//  consent screen is the right native shape, and it is not made better by being
//  editable from a popover. What was missing was the standing *statement*.
//
//  ---------------------------------------------------------------------
//  It shows a state; it offers no control
//  ---------------------------------------------------------------------
//  No Enable, no Revoke, no toggle. Every field it draws came from
//  `autopilot show`, and the route to changing any of it is the Settings link at
//  the bottom of the card. See the standing project rule at the top of
//  GlomerisMenuBarApp.swift: the acting case is `startsUnprompted`, which Rust
//  computed, read through `AutopilotUnpromptedMode`.
//
//  ---------------------------------------------------------------------
//  Why it does not reuse the pane's `detail` sentences
//  ---------------------------------------------------------------------
//  `AutopilotStatusViewModel.detail` says "what is listed below, within these
//  limits", and `AutopilotUnpromptedMode.detail` says "inside the limits above".
//  Both are true on the pane, where the limits are on screen. Neither is true
//  here, where they are not. So this card renders the two `title`/`summary`
//  forms — which name states and point at nothing — and adds one sentence of its
//  own about where the limits are. Reusing the positional wording would have
//  been the cheaper-looking choice and would have pointed a reader at a card
//  that is not there.
//

import SwiftUI

// MARK: - What the card is showing

/// Which of the three things this card can say it is saying.
///
/// Three and not two, and that is the claim this type exists to make
/// unmissable: "not read yet" and "could not be read" are different from each
/// other and neither of them is "off". Modelled as a value so the distinction is
/// assertable without rendering anything — a card is exactly the place where a
/// missing third state is invisible, because the fallback looks like a finished
/// answer.
enum AutopilotSectionState: Equatable {
    /// No read has returned yet.
    case loading

    /// The grant, as `autopilot show` reported it.
    case grant(AutopilotEnvelopeDto)

    /// A read returned and this app could not get a grant out of it. `detail` is
    /// whatever the CLI or the spawn said, when it said anything.
    case unreadable(String?)
}

// MARK: - What the card says

/// The card's wording and its spoken form, as values.
enum AutopilotSectionPresentation {
    static let cardTitle = "Autopilot"

    static let loadingSubject = "Reading the Autopilot grant…"

    static let settingsLinkTitle = "Autopilot settings…"

    static let settingsLinkAccessibilityLabel =
        "Open Autopilot settings, where the grant and its limits are set"

    /// Where the numbers are, since this card deliberately does not repeat them.
    static let limitsElsewhere =
        "The limits a run may not exceed — what it may touch, how many actions, "
        + "how many bytes, how long — are in Autopilot settings."

    /// Which state the card is in, from what the reads have produced.
    ///
    /// The load-bearing function in this file. A card that cannot read the grant
    /// must not render the "off" state, because "off" is a claim that nothing on
    /// this Mac will delete anything unasked — and the situation where the read
    /// failed is exactly the situation in which that claim is unfounded.
    ///
    /// The last good grant survives a later failed read, which is the same rule
    /// the history and candidates cards follow: a transient failure should not
    /// replace an answer that was true when it arrived.
    static func state(
        report: AutopilotEnvelopeDto?,
        errorDetail: String?,
        hasLoadedOnce: Bool
    ) -> AutopilotSectionState {
        if let report { return .grant(report) }
        if !hasLoadedOnce { return .loading }
        return .unreadable(errorDetail)
    }

    /// What to show for ``AutopilotSectionState/unreadable(_:)``.
    ///
    /// Worded as a failure and not as an empty state: an unreadable grant is
    /// something to go and look at, and an empty state reads as good news in this
    /// app by convention.
    /// Unterminated: `SpokenLabel.compose` terminates it, and a detail from the
    /// CLI follows it as a second clause when there is one.
    static let unreadableGrantSentence =
        "Glomeris could not read what Autopilot is authorized to do, so this card cannot say "
        + "whether a recovery run may start on its own"

    static func unreadableGrantMessage(_ detail: String?) -> GlomerisStateMessage {
        .failure(SpokenLabel.compose([Self.unreadableGrantSentence, detail]))
    }

    /// What VoiceOver reads for the state rows, which are one element.
    ///
    /// Two clauses, and the second is the one AC 2 is about: a listener told
    /// only "Autopilot is on" has not been told whether it starts by itself. The
    /// mode's own `summary` is a fragment answering a label, so it is spoken with
    /// its axis attached the way the alert card's is.
    static func spokenState(
        status: AutopilotStatusViewModel,
        mode: AutopilotUnpromptedMode
    ) -> String {
        SpokenLabel.compose([
            status.title,
            SpokenLabel.clause(PressureAlertPresentation.unpromptedModeLabel, mode.summary),
        ])
    }
}

// MARK: - The card

/// Popover section: whether Autopilot is on, and whether it may start a run
/// with nobody present.
///
/// Read on appear rather than on a timer. The grant changes in the Settings
/// scene or from the CLI, and opening either closes this panel — so a read per
/// appearance bounds how stale this can be to one panel session. The surface
/// where staleness would matter does not rely on this one: the monitor re-reads
/// the grant on every poll that owes a banner, so the in-the-moment statement on
/// the alert card is always fresh, and `free` itself refuses an unauthorized
/// unattended run whatever any card says.
struct AutopilotSectionView: View {
    private let client: GlomerisClient

    @State private var report: AutopilotEnvelopeDto?
    @State private var errorDetail: String?
    @State private var hasLoadedOnce = false

    init(client: GlomerisClient = GlomerisClient()) {
        self.client = client
    }

    var body: some View {
        GlomerisCard(title: AutopilotSectionPresentation.cardTitle) {
            switch AutopilotSectionPresentation.state(
                report: report,
                errorDetail: errorDetail,
                hasLoadedOnce: hasLoadedOnce
            ) {
            case .grant(let report):
                rows(report)
            case .loading:
                GlomerisStateMessageView(
                    message: .loading(AutopilotSectionPresentation.loadingSubject)
                )
            case .unreadable(let detail):
                GlomerisStateMessageView(
                    message: AutopilotSectionPresentation.unreadableGrantMessage(detail)
                )
            }

            GlomerisSettingsLink(
                title: AutopilotSectionPresentation.settingsLinkTitle,
                accessibilityLabel: AutopilotSectionPresentation.settingsLinkAccessibilityLabel
            )
        }
        .task { await load() }
    }

    /// The two states, as two symbol-and-word rows read as one sentence.
    private func rows(_ report: AutopilotEnvelopeDto) -> some View {
        let status = AutopilotStatusViewModel.make(report)
        let mode = AutopilotUnpromptedMode.make(report)

        return VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            row(symbolName: status.symbolName, tone: status.tone, text: status.title)
            // The second row is the whole point of the card. Its own symbol and
            // tone, so the two Macs differ by more than one word (campaign §14).
            row(symbolName: mode.symbolName, tone: mode.tone, text: mode.summary)
            Text(AutopilotSectionPresentation.limitsElsewhere)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityHidden(true)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            AutopilotSectionPresentation.spokenState(status: status, mode: mode)
        )
    }

    private func row(symbolName: String, tone: GlomerisTone, text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
            Image(systemName: symbolName)
                .imageScale(.small)
                .foregroundStyle(tone.color)
            Text(text)
                .font(GlomerisDesign.secondaryFont)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
    }

    // MARK: - Fetching

    /// Reads the grant, keeping the last good one if a later read fails.
    ///
    /// `report` is left alone on failure and `errorDetail` is recorded, so a
    /// transient read failure does not replace a state that was read
    /// successfully. `hasLoadedOnce` is what separates "not read yet" from
    /// "could not be read", which are two different things to say.
    private func load() async {
        defer { hasLoadedOnce = true }
        do {
            let raw = try await client.runRaw(
                AutopilotCommands.show,
                progressType: ProgressEventDto.self
            )
            switch AutopilotEnvelopeInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            ) {
            case .envelope(let fresh):
                report = fresh
                errorDetail = nil
            case .usageError(let detail):
                errorDetail = detail
            case .malformedOutput:
                errorDetail = "The app and the CLI are probably different versions."
            case .failed(let detail):
                errorDetail = detail
            }
        } catch {
            errorDetail = SectionFetchErrors.shortMessage(error, subject: "autopilot show")
        }
    }
}

#Preview {
    AutopilotSectionView()
        .padding(GlomerisDesign.outerPadding)
        .frame(width: GlomerisDesign.popoverWidth)
}
