//
//  PressureAlertSectionView.swift
//  GlomerisMenuBar
//
//  HORO-1508 AC 6: the same three answers, reachable without the notification.
//
//  ---------------------------------------------------------------------
//  Why a card as well as a banner
//  ---------------------------------------------------------------------
//  A notification is not a reliable surface and must not be the only one. It can
//  be dismissed with a swipe, hidden by Do Not Disturb, turned off for this app
//  in System Settings, or missed entirely by someone who was away from the
//  machine. In every one of those cases the disk is still over the threshold, the
//  episode is still open, and the user still has three answers available to them
//  — so this card shows the question in the panel they can always open.
//
//  It is also the keyboard and VoiceOver route. Notification buttons are
//  reachable from the keyboard only as far as macOS allows and cannot be reached
//  at all once the banner has gone; these are ordinary buttons in the app's own
//  window, in the panel's reading order, with the spoken state AC 6 and the
//  campaign's accessibility section ask for.
//
//  ---------------------------------------------------------------------
//  It shows a question, never a verdict
//  ---------------------------------------------------------------------
//  Nothing on this card deletes anything. "Review & recover" opens the recovery
//  surface; the other two adjust when the user is next spoken to. And it decides
//  nothing about pressure: which answers exist, whether an episode is open,
//  whether a banner is owed and what the two percentages are all arrive from
//  `pressure show`. See the standing project rule in GlomerisMenuBarApp.swift.
//
//  ---------------------------------------------------------------------
//  It may have to report a deletion it did not make (HORO-1510)
//  ---------------------------------------------------------------------
//  On a Mac whose grant allows a run to start unasked, this card can appear after
//  one already has — because a banner is owed precisely when that run did not get
//  the disk under its goal. So the card carries something HORO-1508 did not need:
//  what the automatic run turned out to be. It is read from the monitor, not
//  composed here.
//

import SwiftUI

// MARK: - What the card says

/// The card's content as values, so what a listener hears is assertable.
///
/// Separated from the view because the claims worth pinning are about wording,
/// not layout: that a listener is told the three figures apart rather than being
/// read one percentage, and that a card with nothing to ask does not appear.
enum PressureAlertPresentation {
    static let cardTitle = "Disk pressure alert"

    /// Whether the panel should show this card at all.
    ///
    /// An open episode, or something wrong with the reader. Not
    /// `notificationDue`: a user who dismissed the banner has an open episode and
    /// no banner owed, and they are exactly the person this card exists for. And
    /// not "nothing": a background reader that has silently stopped looks
    /// identical to a disk that is fine, so an error is shown rather than
    /// swallowed.
    static func isShown(report: PressureStatusReportDto?, errorMessage: String?) -> Bool {
        report?.episode != nil || errorMessage != nil
    }

    /// The headline: where the disk is, in the app's own `%.1f%% used` rendering.
    static func headline(report: PressureStatusReportDto) -> String {
        String(format: "%.1f%% used", report.current.usedPercent)
    }

    /// What a screen reader is told, as one element.
    ///
    /// Four clauses, and the first three are deliberately separate. The threshold
    /// and the goal are different things — one is when Glomeris speaks, the other
    /// is where a run stops — and a listener given a single percentage has been
    /// told the wrong thing. The campaign's accessibility rule names this case
    /// directly: spoken state must distinguish current usage, threshold and target.
    ///
    /// The fourth is what has happened so far, and `automaticRun` is why it is not
    /// a constant: on an opted-in Mac a bounded run may already have reclaimed
    /// something, and a listener told "nothing has been deleted" would have been
    /// read a sentence the app knows to be false (HORO-1510).
    static func spokenState(
        report: PressureStatusReportDto,
        automaticRun: UnpromptedRecoveryOutcome?
    ) -> String {
        SpokenLabel.compose([
            SpokenLabel.clause(
                "Disk now",
                "\(headline(report: report)), \(report.current.freeHuman) free"
            ),
            SpokenLabel.clause("Alert threshold", report.notifyAtDescription),
            SpokenLabel.clause("Recovery goal", report.defaultGoal.description),
            // Said last because it is the account of what has happened, and a
            // listener should not have to wait through it to hear the figures.
            UnpromptedRecoveryAccount.deletionClause(after: automaticRun),
        ])
    }

    /// The hint under an answer button, from the shared vocabulary so the
    /// notification and the card explain an answer the same way.
    static func answerHint(_ responseToken: String) -> String {
        GlomerisVocabulary.episodeResponse(responseToken).explanation
    }
}

// MARK: - The card

struct PressureAlertSectionView: View {
    /// `@ObservedObject` rather than `@StateObject`: the monitor outlives every
    /// view, because it polls while nothing is on screen. It is created once by
    /// `GlomerisMenuBarApp` and handed down.
    @ObservedObject var monitor: PressureEpisodeMonitor

    var body: some View {
        if PressureAlertPresentation.isShown(
            report: monitor.lastReport,
            errorMessage: monitor.lastErrorMessage
        ) {
            GlomerisCard(title: PressureAlertPresentation.cardTitle) {
                if let message = monitor.lastErrorMessage {
                    GlomerisStateMessageView(message: .failure(message))
                }
                if let report = monitor.lastReport, report.episode != nil {
                    figures(report)
                    answers(report)
                }
            }
        }
    }

    /// The three figures, each labelled, each read as its own row.
    ///
    /// Labelled text rather than a gauge or a bar, and that is the accessibility
    /// rule rather than a style preference: a percentage a user can only get from
    /// the length of a bar is a percentage some users cannot get at all. The
    /// whole card is then one spoken element, so a listener hears the three
    /// figures as one sentence instead of tabbing through three rows to assemble
    /// it.
    private func figures(_ report: PressureStatusReportDto) -> some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            GlomerisDetailRow(label: "Disk now") {
                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    Text(PressureAlertPresentation.headline(report: report))
                        .font(GlomerisDesign.primaryFont)
                    GlomerisBadgeView(
                        term: GlomerisVocabulary.pressure(report.current.pressureState),
                        filled: false
                    )
                }
            }
            GlomerisDetailRow(label: "Free space") {
                Text(report.current.freeHuman)
                    .font(GlomerisDesign.secondaryFont)
            }
            GlomerisDetailRow(label: "You asked to be alerted at") {
                Text(report.notifyAtDescription)
                    .font(GlomerisDesign.secondaryFont)
            }
            GlomerisDetailRow(label: "Recovery goal") {
                Text(report.defaultGoal.description)
                    .font(GlomerisDesign.secondaryFont)
            }
            // What has happened so far. Inside the spoken element above rather
            // than beside it, so a listener hears the figures and the account as
            // one statement — and so the account cannot be missed by someone who
            // stops listening after the third figure.
            GlomerisStateMessageView(
                message: UnpromptedRecoveryAccount.message(after: automaticRun(report))
            )
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            PressureAlertPresentation.spokenState(
                report: report,
                automaticRun: automaticRun(report)
            )
        )
    }

    /// The outcome of an automatic run for *this* episode, asked of the monitor so
    /// the episode check lives in one place.
    private func automaticRun(_ report: PressureStatusReportDto) -> UnpromptedRecoveryOutcome? {
        monitor.automaticRun(forEpisodeIn: report)
    }

    /// One button per answer the CLI published, in its order.
    ///
    /// Not a fixed three. The set comes from `responses`, so this app cannot
    /// offer a fourth and cannot drop one the CLI would accept — the same rule
    /// the notification's buttons follow, for the same reason: a button built
    /// from anything else would be refused after the user pressed it.
    private func answers(_ report: PressureStatusReportDto) -> some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            ForEach(report.responses, id: \.self) { token in
                answerButton(token)
            }
        }
    }

    private func answerButton(_ token: String) -> some View {
        let term = GlomerisVocabulary.episodeResponse(token)
        return Button {
            Task { await monitor.answer(token) }
        } label: {
            Label(term.title, systemImage: term.symbolName ?? "questionmark.circle")
        }
        .buttonStyle(.borderless)
        .font(GlomerisDesign.secondaryFont)
        // The axis and the explanation, not just the title: "Remind me later"
        // alone does not say later than what, or what happens in the meantime.
        .accessibilityLabel(term.accessibilityLabel)
        .accessibilityHint(PressureAlertPresentation.answerHint(token))
        .help(term.explanation)
    }
}
