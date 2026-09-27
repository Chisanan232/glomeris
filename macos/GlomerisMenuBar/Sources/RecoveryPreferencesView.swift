//
//  RecoveryPreferencesView.swift
//  GlomerisMenuBar
//
//  HORO-1507: the two numbers that decide when Glomeris speaks up and where a
//  recovery run stops.
//
//  They are two settings, not one, and this pane's whole shape is that claim.
//  An alert threshold answers "when is this worth your attention"; a recovery
//  goal answers "where should recovery stop". They are both percentages, both
//  about the same disk, and a user who reads them as one number has been told
//  that the point they get warned at is the point recovery aims for — which is
//  the opposite of what either setting means. So they live in two cards under
//  two headings, are written to two separately named fields, and every label
//  that shows either of them names its axis in the same sentence as the figure.
//
//  ---------------------------------------------------------------------
//  This screen decides nothing
//  ---------------------------------------------------------------------
//  See the standing project rule in GlomerisMenuBarApp.swift. Both limits, both
//  in-force figures and every refusal shown here arrive from one Rust producer
//  — `glomeris settings show|set --json`, projected by `src/cli/settings.rs`.
//
//  In particular the *bounds*. `settings show` reports what each number may be
//  (`RecoverySettingsBoundsReport`), and the steppers are bounded by that
//  report rather than by constants in this file. A pane that knew the limits
//  independently would eventually offer a value the CLI then refuses, and the
//  refusal would arrive after the user pressed Save.
//
//  What the bounds deliberately do NOT carry is the rule that the goal must sit
//  below the threshold. That is not a bound on either number — it moves as the
//  other one moves — and a client that turned it into a stepper range would be
//  reimplementing `RecoverySettings::with_changes` rather than reading it. So
//  this pane does not pre-judge the pair: it sends what the user composed, and
//  when the CLI refuses it the refusal is shown with the CLI's own wording
//  beside `GlomerisVocabulary.settingsRejection`'s explanation of which control
//  to move. Learning the rule from the refusal is the only way the pane and the
//  validator cannot disagree about it.
//
//  ---------------------------------------------------------------------
//  Save sends both numbers, always
//  ---------------------------------------------------------------------
//  `settings set` validates the pair in one call, precisely so that a
//  destination like (75, 70) → (60, 55) is reachable — it is not reachable one
//  field at a time, because whichever field moves first is momentarily invalid
//  against the old value of the other. This pane therefore always sends both
//  flags, which makes the form a complete statement of the destination rather
//  than a sequence of edits whose order could decide whether it is accepted.
//
import SwiftUI

// MARK: - Reading and writing the settings

/// What one `settings show|set --json` invocation turned out to be.
///
/// Five cases rather than the four `AutopilotEnvelopeOutcome` has, and the extra
/// one is the point: `settings set --json` prints a *rejection report* on stdout
/// and exits 2 (`src/main.rs`). A refused change is a first-class, structured
/// answer with its own reason token and its own figures — not a prose failure —
/// and flattening it into `.failed` would throw away the only thing that can
/// tell the user which of the two controls to move.
///
/// Split by the CLI's own exit contract:
///
///   - **0** — the report was printed to stdout. For `set` this is printed
///     *after* the file was written, so it describes what is now in force
///     rather than what was asked for;
///   - **2** with a rejection report — the pair was refused and nothing was
///     saved;
///   - **2** with prose — the arguments were refused. When it says
///     "unrecognized argument" this app sent flags the installed `glomeris` does
///     not have, which is a version skew and this app's problem;
///   - **1** — the settings file could not be read or written. Prose, no report;
///   - exit 0 with undecodable stdout is the other half of a version skew.
enum RecoverySettingsOutcome: Equatable {
    case settings(RecoverySettingsReportDto)
    case refused(SettingsRejectionReportDto)
    case usageError(String)
    case malformedOutput
    case failed(String)

    /// `true` when nothing reached the settings file.
    ///
    /// A property on the outcome rather than a `switch` in the view, because this
    /// is the one judgement on this pane that is easy to get backwards and
    /// expensive when it is. `.malformedOutput` is the case that catches people
    /// out: it is an exit 0, and `set` prints only after `save_settings` returned
    /// `Ok`, so the pair *is* stored and only the readback failed. Treating it as
    /// "nothing was saved" would leave the user believing their old values were
    /// still in force.
    ///
    /// `.settings` is the other exit-0 case and also wrote. `.refused`,
    /// `.usageError` and `.failed` all return before or instead of the write —
    /// `src/main.rs` exits 2 on a refused pair without calling `save_settings`,
    /// and exits 1 from `settings_store_error_exit` when the write itself failed.
    var wroteNothing: Bool {
        switch self {
        case .settings, .malformedOutput:
            return false
        case .refused, .usageError, .failed:
            return true
        }
    }
}

/// Pure, directly-testable mapping from a `settings` subcommand's exit code and
/// streams to a `RecoverySettingsOutcome`.
enum RecoverySettingsInterpretation {
    /// The prefix the CLI puts on its own stderr lines, stripped so the sentence
    /// reads as a sentence rather than as a log line. Matched as a literal; a
    /// mismatch degrades to showing the line verbatim.
    static let stderrPrefix = "glomeris settings: "

    /// Tells "this app sent flags that CLI does not have" apart from "that
    /// change was refused". Only one of them is the user's to act on.
    static let usageErrorMarker = "unrecognized argument"

    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> RecoverySettingsOutcome {
        // The report is tried first, then the rejection, and the order is not
        // arbitrary: the two shapes are decode-disjoint (a report requires
        // `notify_at_description`, `default_goal` and `loaded_from_file`; a
        // rejection requires `reason` and `message`), so either match is
        // unambiguous — but only this order keeps a successful `set` from being
        // read as anything else if the shapes ever converge.
        //
        // Both are decoded before the exit code is judged, for the same reason
        // `AutopilotEnvelopeInterpretation` does it: whatever was printed is a
        // better account of what happened than the exit code alone.
        if let report = try? JSONDecoder().decode(RecoverySettingsReportDto.self, from: stdout) {
            return .settings(report)
        }
        if let rejection = try? JSONDecoder().decode(
            SettingsRejectionReportDto.self, from: stdout)
        {
            return .refused(rejection)
        }

        let stderrText = String(decoding: stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)

        if exitCode == 2 {
            if stderrText.contains(Self.usageErrorMarker) {
                return .usageError(
                    stderrText.isEmpty
                        ? "The installed glomeris rejected the arguments this app sent."
                        : Self.withoutPrefix(stderrText)
                )
            }
            return .failed(
                stderrText.isEmpty
                    ? "glomeris refused this request but said nothing about why."
                    : Self.withoutPrefix(stderrText)
            )
        }

        if exitCode == 0 {
            return .malformedOutput
        }

        return .failed(
            stderrText.isEmpty
                ? "glomeris settings exited with code \(exitCode) and printed no result."
                : Self.withoutPrefix(stderrText)
        )
    }

    private static func withoutPrefix(_ text: String) -> String {
        guard text.hasPrefix(Self.stderrPrefix) else { return text }
        return String(text.dropFirst(Self.stderrPrefix.count))
    }
}

// MARK: - Commands

/// The argument vectors this pane runs, as data.
///
/// Pure values rather than literals inline in the view, so three properties are
/// assertable rather than eyeballed: nothing here names a filesystem path or a
/// credential, `set` always carries both flags, and the flag spellings match the
/// ones `src/main.rs` parses.
enum RecoverySettingsCommands {
    static let show = ["settings", "show", "--json"]

    /// The complete change. Both flags are always present — see this file's
    /// header on why sending one is not "leave the other alone" but "ask to be
    /// validated against a value the user may be in the middle of replacing".
    ///
    /// The figures are formatted by `RecoverySettingsFigure.argument`, which
    /// emits no `%` and no exponent: `settings set` parses with `f64::parse`
    /// after trimming a trailing percent sign, and `"9e1"` would parse to 90
    /// while reading as nothing at all in a log.
    static func set(notifyAtUsedPercent: Double, goalUsedPercent: Double) -> [String] {
        [
            "settings", "set",
            "--notify-at-used-percent", RecoverySettingsFigure.argument(notifyAtUsedPercent),
            "--default-goal-used-percent", RecoverySettingsFigure.argument(goalUsedPercent),
            "--json",
        ]
    }
}

// MARK: - Rendering a percentage

/// How this pane turns a percentage into text.
///
/// One place, because the alternative is three: a stepper label, a spoken value
/// and a command-line argument, each free to round differently. A control
/// reading 62 that sends 62.5 is the specific failure this prevents.
enum RecoverySettingsFigure {
    /// `62` and `62.5`, never `62.0`.
    ///
    /// Trailing `.0` is dropped because the stored value is usually whole and
    /// `"62.0% used"` reads as a precision nobody chose. A fractional value —
    /// which a config file or another client may perfectly well have stored — is
    /// shown as it is rather than rounded, because rounding it on screen would
    /// make the pane disagree with `settings show`.
    static func text(_ percent: Double) -> String {
        percent == percent.rounded() && percent.isFinite
            ? String(Int(percent.rounded()))
            : String(percent)
    }

    /// What goes in `argv`. The same digits as `text`, deliberately: an
    /// invocation that carried a different number from the one on screen would
    /// make the confirmation describe a change nobody asked for.
    static func argument(_ percent: Double) -> String { text(percent) }

    /// What VoiceOver reads as a stepper's value: `"85 percent used"`.
    ///
    /// Spelled out rather than `%`, for the reason
    /// `RecoverySectionView.goalAccessibilityValue` spells it out: a spoken `%`
    /// is at the mercy of the voice, and these are the two numbers on the pane a
    /// user can change. A value read as a bare "85" leaves the axis to be
    /// inferred from a label heard some moments earlier — and there are two
    /// axes here that a listener must not merge.
    static func spokenValue(_ percent: Double) -> String { "\(text(percent)) percent used" }
}

// MARK: - The form

/// The pair being composed, as values.
///
/// Separate from the view so the part worth asserting is assertable: that the
/// argument vector it produces stays inside the bounds the CLI reported, always
/// states both numbers, and never names a path.
///
/// The bounds travel inside the draft rather than being reached for globally,
/// because clamping against a value this type does not hold is clamping against
/// an assumption.
struct RecoverySettingsDraft: Equatable {
    /// When to call attention to disk usage, percent USED.
    var notifyAtUsedPercent: Double

    /// Where a recovery run should stop, percent USED.
    var goalUsedPercent: Double

    let bounds: RecoverySettingsBoundsReportDto

    /// Initialised from the report verbatim — not snapped to the stepper's step.
    ///
    /// A stored 62.5 presents as 62.5. Showing 60 or 65 for it would make merely
    /// opening this pane and pressing Save change a number the user never
    /// touched, and would make the pane disagree with `glomeris settings show`
    /// about what is stored.
    static func from(_ report: RecoverySettingsReportDto) -> RecoverySettingsDraft {
        RecoverySettingsDraft(
            notifyAtUsedPercent: report.notifyAtUsedPercent,
            goalUsedPercent: report.defaultGoal.usedPercent,
            bounds: report.bounds
        )
    }

    /// Whole percentage points. Fine enough to express any threshold worth
    /// setting, coarse enough to be operable from the keyboard.
    static let step = 1.0

    // MARK: What the steppers may offer

    var notifyAtRange: ClosedRange<Double> {
        Self.range(bounds.notifyAtMinimumUsedPercent, bounds.notifyAtMaximumUsedPercent)
    }

    var goalRange: ClosedRange<Double> {
        Self.range(bounds.goalMinimumUsedPercent, bounds.goalMaximumUsedPercent)
    }

    /// Guards the one thing a `ClosedRange` will not tolerate.
    ///
    /// `Stepper(value:in:)` traps on a reversed range, so a CLI that ever
    /// reported a minimum above its maximum would crash this pane rather than
    /// render it. Degrading to the single value is not a correction of the
    /// report — it is a refusal to offer a choice this app cannot make sense of.
    private static func range(_ lower: Double, _ upper: Double) -> ClosedRange<Double> {
        lower <= upper ? lower...upper : lower...lower
    }

    /// What will actually be sent, clamped into the reported bounds.
    ///
    /// Clamped as well as bounded by the steppers, because a value loaded from
    /// the report can already sit outside them: a settings file written by an
    /// older CLI with wider limits, or a hand-edited one. A pane that sent it
    /// back unchanged would be forwarding a value this CLI refuses.
    var clampedNotifyAtUsedPercent: Double { Self.clamp(notifyAtUsedPercent, notifyAtRange) }

    var clampedGoalUsedPercent: Double { Self.clamp(goalUsedPercent, goalRange) }

    private static func clamp(_ value: Double, _ range: ClosedRange<Double>) -> Double {
        // NaN compares false against everything, so `min`/`max` would pass it
        // through. It cannot arrive from a decoded report — JSON has no NaN —
        // but a stepper binding is a `Double` and this is the one value that
        // would reach `argv` as the literal text "nan".
        guard value.isFinite else { return range.lowerBound }
        return Swift.min(Swift.max(value, range.lowerBound), range.upperBound)
    }

    /// The complete `settings set` invocation for this pair.
    var commandArguments: [String] {
        RecoverySettingsCommands.set(
            notifyAtUsedPercent: clampedNotifyAtUsedPercent,
            goalUsedPercent: clampedGoalUsedPercent
        )
    }

    /// `true` when this draft differs from what the report said is in force.
    ///
    /// Compared against the report rather than tracked as a dirty flag, so
    /// moving a stepper away and back again correctly reads as no change.
    ///
    /// Note what this is *not*: a judgement about whether the pair is
    /// acceptable. This pane does not decide that — see the header.
    func differs(from report: RecoverySettingsReportDto) -> Bool {
        clampedNotifyAtUsedPercent != report.notifyAtUsedPercent
            || clampedGoalUsedPercent != report.defaultGoal.usedPercent
    }
}

// MARK: - Wording

/// Sentences this pane says in its own voice, kept out of the view so they can
/// be asserted and so none of them is invented twice.
///
/// Not in `GlomerisVocabulary`: none of these words a CLI token. The refusal
/// wording *is* keyed on CLI tokens and therefore lives there, where the
/// vocabulary guard can diff it against `SettingsRejection::as_str`.
enum RecoveryPreferencesWording {
    static let thresholdCardTitle = "Tell Me When"
    static let goalCardTitle = "Recover Down To"

    static let thresholdExplanation = """
        The level of disk usage Glomeris treats as worth your attention. \
        Crossing it is a prompt, never a cleanup: nothing is deleted because \
        this number was reached.
        """

    /// What crossing the threshold actually does, now that it does something.
    ///
    /// This replaced a caveat HORO-1507 shipped on purpose: the number was stored
    /// and reported, but nothing notified from it, and a pane that implied
    /// otherwise would have been promising a notification that never arrived.
    /// HORO-1508 landed the notification, so the caveat became the false sentence
    /// and went.
    ///
    /// It names the one answer that leads anywhere rather than listing them. The
    /// set of answers comes from the CLI, and prose here that counted them would be
    /// a second, unversioned copy of that set — wrong the first time one is added.
    static let thresholdNotification = """
        Crossing it raises a notification offering to review and recover. \
        Nothing is deleted unless you choose it there.
        """

    static let goalExplanation = """
        Where a recovery run stops. Glomeris works down to this level and then \
        stops on its own, whether or not there is more it could have taken.
        """

    /// The one sentence that has to do the work of keeping the two apart.
    static let twoDifferentNumbers = """
        These are two different numbers. The first is when Glomeris should say \
        something; the second is where recovery should stop. The goal has to be \
        below the threshold — recovery that stopped at the level that raised the \
        alert would not have got you anywhere.
        """

    static func thresholdLabel(_ percent: Double) -> String {
        "Say something at \(RecoverySettingsFigure.text(percent))% used"
    }

    static func goalLabel(_ percent: Double) -> String {
        "Recover down to \(RecoverySettingsFigure.text(percent))% used"
    }

    static func boundsNote(_ draft: RecoverySettingsDraft) -> String {
        """
        Glomeris accepts a threshold between \
        \(RecoverySettingsFigure.text(draft.bounds.notifyAtMinimumUsedPercent))% and \
        \(RecoverySettingsFigure.text(draft.bounds.notifyAtMaximumUsedPercent))% used, and a goal \
        between \(RecoverySettingsFigure.text(draft.bounds.goalMinimumUsedPercent))% and \
        \(RecoverySettingsFigure.text(draft.bounds.goalMaximumUsedPercent))% used. Those limits \
        are in the CLI, not in this window.
        """
    }

    static let storedDefaults = """
        Nothing is stored yet, so these are the built-in defaults. They are not \
        a choice anybody made.
        """

    static let storedChosen = "These are the values stored on this Mac."
}

/// What VoiceOver reads for a refusal (HORO-1470's rule, applied to this pane's
/// one multi-`Text` element).
///
/// Three sentences stacked as three `Text` views, so layout gives a sighted user
/// the boundaries and a listener gets them only if they are stated. The axis is
/// prefixed — a bare "Goal is not below the threshold" arriving after a Save
/// press does not say which of this app's several kinds of refusal it is.
///
/// Pure and `internal` so the composition is asserted rather than eyeballed: the
/// campaign's accessibility rule is that spoken state must distinguish the
/// threshold from the target, and this is the one label that says both numbers.
enum RecoverySettingsRefusalLabel {
    static func spoken(_ rejection: SettingsRejectionReportDto) -> String {
        let term = GlomerisVocabulary.settingsRejection(rejection.reason)
        return SpokenLabel.compose([
            SpokenLabel.clause(term.axis, term.title),
            term.explanation,
            rejection.message,
        ])
    }
}

/// What to say after a **Save** press.
///
/// The interesting cases are the two that are neither success nor refusal: a
/// malformed-but-successful run, where the file *was* written and saying
/// "nothing was saved" would be exactly backwards; and a store failure, where
/// nothing was saved and the old pair is still in force.
///
/// A refusal is deliberately absent from here. It has its own structured report
/// with its own reason token, and squashing it into one sentence would drop the
/// only thing that tells the user which of the two controls to move — see
/// `refusalCard`.
enum RecoverySettingsSaveWording {
    static func message(_ outcome: RecoverySettingsOutcome) -> GlomerisStateMessage? {
        switch outcome {
        case .settings:
            return .success("Saved. These are the values in force now.")

        case .refused:
            // Rendered as a card, not as a line. `nil` rather than a placeholder
            // sentence, so there is no path on which both appear and the user
            // reads the refusal twice in two different wordings.
            return nil

        case .usageError(let detail):
            return .failure(
                "The installed glomeris rejected what this app asked for, so nothing was saved. "
                    + "The app and the CLI are probably different versions. It said: " + detail
            )

        case .malformedOutput:
            // Exit 0, and `set` prints only after `save_settings` returned Ok —
            // so the pair is stored and this app cannot read it back.
            return .failure(
                "glomeris saved these values but this app could not read back what it saved, so "
                    + "what is shown here may not be what is stored. Check it with `glomeris "
                    + "settings show`."
            )

        case .failed(let detail):
            return .failure("Nothing was saved. glomeris said: \(detail)")
        }
    }
}

// MARK: - The screen

struct RecoveryPreferencesView: View {
    private let client: GlomerisClient

    /// What is stored, as last reported. `nil` until the first `show` returns —
    /// there is no form without it, because the limits the form offers are its
    /// fields.
    @State private var report: RecoverySettingsReportDto?
    @State private var draft: RecoverySettingsDraft?

    @State private var isLoading = false
    @State private var loadErrorMessage: String?
    @State private var loadOutcomeMessage: GlomerisStateMessage?

    @State private var isSaving = false
    @State private var saveMessage: GlomerisStateMessage?

    /// The last refusal, kept as the structured report rather than as a
    /// sentence, so the pane can show which number was refused and the CLI's own
    /// explanation of why.
    @State private var refusal: SettingsRejectionReportDto?

    init(client: GlomerisClient = GlomerisClient()) {
        self.client = client
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                if let report, let draft {
                    storedCard(report)
                    thresholdCard(draft)
                    goalCard(draft)
                    saveCard(report, draft)
                } else {
                    unavailableCard
                }
            }
            .padding(GlomerisDesign.outerPadding)
        }
        .frame(width: 460, height: 480)
        // `settings show` opens a file and prints it. Saving is a write and is
        // bound to a button only — see `save`.
        .task {
            await loadSettings()
        }
    }

    // MARK: Before there is a report

    private var unavailableCard: some View {
        GlomerisCard(title: "Recovery") {
            Text(
                "Two settings live here: the disk usage Glomeris should tell you about, and the "
                    + "level a recovery run should work down to."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            if isLoading {
                GlomerisStateMessageView(message: .loading("Reading what is stored…"))
            } else {
                // No form without a report, and the reason is said out loud: the
                // alternative is two steppers full of plausible defaults that
                // were never anybody's setting, over limits this window guessed.
                GlomerisStateMessageView(
                    message: loadOutcomeMessage
                        ?? .failure(
                            loadErrorMessage
                                ?? "Glomeris could not say what is stored, so there is nothing "
                                    + "safe to show here."
                        ))
                Text(
                    "Both values and the limits they have to stay inside come from the glomeris "
                        + "command itself. Without it, this window would be guessing."
                )
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            Button(isLoading ? "Checking…" : "Check again") {
                Task { await loadSettings() }
            }
            .disabled(isLoading)
        }
    }

    // MARK: What is stored

    private func storedCard(_ report: RecoverySettingsReportDto) -> some View {
        GlomerisCard(title: "Recovery", trailing: isLoading ? "checking…" : nil) {
            Text(RecoveryPreferencesWording.twoDifferentNumbers)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            // Both in-force figures in the CLI's own rendering, so what confirms
            // a change is worded by the thing that stored it.
            GlomerisDetailRow(label: "Tell me at") {
                Text(report.notifyAtDescription)
                    .font(GlomerisDesign.secondaryFont)
            }
            GlomerisDetailRow(label: "Recover to") {
                Text(report.defaultGoal.description)
                    .font(GlomerisDesign.secondaryFont)
            }

            Text(
                report.loadedFromFile
                    ? RecoveryPreferencesWording.storedChosen
                    : RecoveryPreferencesWording.storedDefaults
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            if let storedAt = report.storedAt {
                GlomerisDetailRow(label: "Stored at") {
                    GlomerisPathText(path: storedAt)
                }
            }

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Button("Refresh") {
                    Task { await loadSettings() }
                }
                .disabled(isLoading || isSaving)
                Spacer(minLength: 0)
            }

            if let loadOutcomeMessage {
                GlomerisStateMessageView(message: loadOutcomeMessage)
            }
            if let loadErrorMessage {
                GlomerisStateMessageView(message: .failure(loadErrorMessage))
            }
        }
    }

    // MARK: The threshold

    private func thresholdCard(_ draft: RecoverySettingsDraft) -> some View {
        GlomerisCard(title: RecoveryPreferencesWording.thresholdCardTitle) {
            Text(RecoveryPreferencesWording.thresholdExplanation)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            // A stepper, deliberately not a slider — the campaign's
            // accessibility rule names unlabeled percentage sliders as the thing
            // not to build, and `RecoverySectionView` made the same choice for
            // the same reason. The label states the number and its axis in the
            // one string a sighted user reads.
            Stepper(
                value: notifyAtBinding,
                in: draft.notifyAtRange,
                step: RecoverySettingsDraft.step
            ) {
                Text(
                    RecoveryPreferencesWording.thresholdLabel(draft.clampedNotifyAtUsedPercent)
                )
                .font(GlomerisDesign.secondaryFont)
            }
            .accessibilityLabel("Tell me when disk usage reaches")
            .accessibilityValue(
                RecoverySettingsFigure.spokenValue(draft.clampedNotifyAtUsedPercent))
            .accessibilityHint(
                "The percentage of the disk in use at which Glomeris should call your attention "
                    + "to it.")
            .disabled(isSaving)

            Text(RecoveryPreferencesWording.thresholdNotification)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var notifyAtBinding: Binding<Double> {
        Binding(
            get: { draft?.clampedNotifyAtUsedPercent ?? 0 },
            set: { newValue in
                guard var updated = draft else { return }
                updated.notifyAtUsedPercent = newValue
                draft = updated
                // A refusal describes the pair that was sent. Once either number
                // moves it no longer describes what Save would do, and leaving it
                // on screen would attach an explanation to a change nobody asked
                // for yet.
                refusal = nil
                saveMessage = nil
            }
        )
    }

    // MARK: The goal

    private func goalCard(_ draft: RecoverySettingsDraft) -> some View {
        GlomerisCard(title: RecoveryPreferencesWording.goalCardTitle) {
            Text(RecoveryPreferencesWording.goalExplanation)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Stepper(
                value: goalBinding,
                in: draft.goalRange,
                step: RecoverySettingsDraft.step
            ) {
                Text(RecoveryPreferencesWording.goalLabel(draft.clampedGoalUsedPercent))
                    .font(GlomerisDesign.secondaryFont)
            }
            .accessibilityLabel("Default recovery goal")
            .accessibilityValue(RecoverySettingsFigure.spokenValue(draft.clampedGoalUsedPercent))
            .accessibilityHint(
                "The percentage of the disk still in use that a recovery run should work down "
                    + "to, and then stop.")
            .disabled(isSaving)

            Text(RecoveryPreferencesWording.boundsNote(draft))
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var goalBinding: Binding<Double> {
        Binding(
            get: { draft?.clampedGoalUsedPercent ?? 0 },
            set: { newValue in
                guard var updated = draft else { return }
                updated.goalUsedPercent = newValue
                draft = updated
                refusal = nil
                saveMessage = nil
            }
        )
    }

    // MARK: Saving

    private func saveCard(
        _ report: RecoverySettingsReportDto,
        _ draft: RecoverySettingsDraft
    ) -> some View {
        GlomerisCard(title: "Save These") {
            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Button(isSaving ? "Saving…" : "Save") {
                    Task { await save() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(isSaving || !draft.differs(from: report))

                if !draft.differs(from: report) {
                    Text("Nothing to save — these are the stored values.")
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }

            if isSaving {
                GlomerisStateMessageView(message: .loading("Saving…"))
            }
            if let saveMessage {
                GlomerisStateMessageView(message: saveMessage)
            }
            if let refusal {
                refusalCard(refusal)
            }
        }
    }

    /// A refused pair, with the CLI's reason and the CLI's own sentence.
    ///
    /// Two wordings on purpose, and they do different jobs: the vocabulary term
    /// says which control to move, and the message beneath it is the validator's
    /// verbatim account of what it refused and against what. Paraphrasing the
    /// second into the first would make this app the author of a judgement it
    /// did not make.
    @ViewBuilder
    private func refusalCard(_ rejection: SettingsRejectionReportDto) -> some View {
        let term = GlomerisVocabulary.settingsRejection(rejection.reason)

        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: term)
                Spacer(minLength: 0)
            }
            Text(term.explanation)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(rejection.message)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(RecoverySettingsRefusalLabel.spoken(rejection))
    }

    // MARK: Running the commands

    /// Reads the settings. The only command on this pane that changes nothing,
    /// and the only one allowed to run without a button press.
    ///
    /// The draft is rebuilt from every successful read, including the read that
    /// follows a save — so the form always shows the last thing the CLI said is
    /// stored, never the last thing this window asked for.
    ///
    /// `preservingDraft` is the one exception, and it exists for the path where
    /// nothing was written: a refused pair stays in the steppers to be
    /// corrected, beside a refusal that names which of the two to move. Silently
    /// replacing it with the stored values would delete what the user was in the
    /// middle of composing and leave an explanation attached to numbers that are
    /// no longer on screen. The *report* is still refreshed either way, because
    /// what is stored is a fact this window does not get to keep a stale copy of.
    private func loadSettings(preservingDraft: Bool = false) async {
        isLoading = true
        loadErrorMessage = nil
        loadOutcomeMessage = nil

        do {
            let raw = try await client.runRaw(
                RecoverySettingsCommands.show,
                progressType: ProgressEventDto.self
            )
            switch RecoverySettingsInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            ) {
            case .settings(let fresh):
                report = fresh
                if !preservingDraft || draft == nil {
                    draft = RecoverySettingsDraft.from(fresh)
                }
            case .refused(let rejection):
                // `show` takes no values, so it has nothing to refuse. Reaching
                // here means the stored file itself is a pair this CLI will not
                // accept, which is worth saying plainly rather than showing as a
                // refusal of something the user just did.
                loadOutcomeMessage = .failure(
                    "glomeris refused the settings that are stored on this Mac, so this window "
                        + "cannot show them. It said: " + rejection.message
                )
            case .usageError(let detail):
                loadOutcomeMessage = .failure(
                    "The installed glomeris does not understand what this app asked it to do, so "
                        + "this window cannot show what is stored. The app and the CLI are "
                        + "probably different versions. It said: " + detail
                )
            case .malformedOutput:
                loadOutcomeMessage = .failure(
                    "glomeris printed something this app could not read. The app and the CLI are "
                        + "probably different versions."
                )
            case .failed(let detail):
                loadOutcomeMessage = .failure(detail)
            }
        } catch {
            loadErrorMessage = SectionFetchErrors.shortMessage(error, subject: "settings show")
        }

        isLoading = false
    }

    /// Writes the pair, then re-reads it.
    ///
    /// Called only from the Save button's action closure, in a detached `Task
    /// {}`: this one writes a file, and a cancelled write reported as nothing at
    /// all would leave the user unsure which pair is in force.
    private func save() async {
        guard let draft else { return }

        isSaving = true
        saveMessage = nil
        refusal = nil

        /// Whether the form survives the re-read below. `false` when the outcome
        /// is unknown, because a form kept over an unknown write would be showing
        /// a pair the user could reasonably read as the one in force.
        var keepForm = false

        do {
            let raw = try await client.runRaw(
                draft.commandArguments,
                progressType: ProgressEventDto.self
            )
            let outcome = RecoverySettingsInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
            saveMessage = RecoverySettingsSaveWording.message(outcome)
            if case .refused(let rejection) = outcome {
                refusal = rejection
            }
            keepForm = outcome.wroteNothing
            adopt(outcome)
        } catch {
            saveMessage = .failure(
                SectionFetchErrors.shortMessage(error, subject: "settings set")
                    ?? "The attempt to save was stopped, so it is not known whether it was "
                        + "saved. Check with `glomeris settings show`."
            )
        }

        isSaving = false
        // Whatever happened, including a refusal: the authoritative answer to
        // "what is stored now" is another read, not what this window tried to do.
        //
        // The form is kept on the paths where nothing reached the file, so the
        // pair that was refused is still there to be corrected. `.malformedOutput`
        // is not one of them — exit 0 means `save_settings` returned Ok, so the
        // pair *is* stored and the readback is the only thing that failed.
        await loadSettings(preservingDraft: keepForm)
    }

    /// Takes on a report a write printed back, so the pane updates from what
    /// reached the file rather than from the form.
    ///
    /// Only `.settings` adopts. A refusal prints a rejection, not a report, and
    /// nothing was written — so there is nothing to take on, and the subsequent
    /// `loadSettings` is what restates the unchanged stored pair.
    private func adopt(_ outcome: RecoverySettingsOutcome) {
        guard case .settings(let fresh) = outcome else { return }
        report = fresh
        draft = RecoverySettingsDraft.from(fresh)
    }
}

#Preview {
    RecoveryPreferencesView()
}
