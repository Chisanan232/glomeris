//
//  AutopilotPreferencesView.swift
//  GlomerisMenuBar
//
//  HORO-1310: the Autopilot settings screen — what standing authorization is
//  in force, what it may touch, what it may spend, when it may act, what may
//  be consented to in advance, and what Glomeris will refuse no matter what
//  is granted here.
//
//  Before this, Autopilot existed only as `glomeris autopilot enable --kinds …`
//  in a terminal. That is the wrong place for it: this is the one feature of
//  the product where a user grants standing permission to delete things
//  without being asked again, and the person granting it is exactly the person
//  least likely to be reading `--help`. An unread grant is not consent.
//
//  ---------------------------------------------------------------------
//  This screen decides nothing about Autopilot
//  ---------------------------------------------------------------------
//  See the standing project rule in GlomerisMenuBarApp.swift. Every choice
//  offered here arrives as data from one Rust producer — `autopilot show
//  --json`, projected by `src/autopilot/report.rs`:
//
//    * which kinds may be allowlisted (`allowlistable_kinds`) and which never
//      can (`never_allowlistable_kinds`);
//    * which policy reasons may be pre-authorized (`preauthorizable_reasons`)
//      and which never can (`never_preauthorizable_reasons`);
//    * the hard ceilings on actions, bytes and duration (`ceilings`);
//    * the disk-pressure states a threshold may name (`pressure_states`);
//    * the policy labels no envelope can make executable
//      (`never_executable_labels`);
//    * what the model's authority actually amounts to (`ai_authority`).
//
//  None of those lists is written in Swift, and the form is not rendered at
//  all until one arrives. That is deliberate: a screen that knew the ceilings
//  independently would eventually offer a value `enable` then refuses, and a
//  screen that listed "PROTECTED is never deleted" from its own knowledge
//  would be making a policy claim in the client. Here that sentence is a
//  quotation.
//
//  ---------------------------------------------------------------------
//  Enable replaces the whole grant
//  ---------------------------------------------------------------------
//  `autopilot enable` starts from `AutopilotEnvelope::revoked()` and applies
//  only the flags it is given, so anything omitted lands on a default rather
//  than on whatever was in force before. This screen therefore always sends
//  every field — kinds, all three budgets, the pressure threshold, every
//  pre-authorization — and it initialises the form from the envelope that is
//  in force. Both halves are needed: sending everything makes the form the
//  complete statement, and loading first means pressing Enable without
//  touching anything re-grants what was already there instead of silently
//  resetting the budgets.
//
//  ---------------------------------------------------------------------
//  What this screen deliberately does NOT have
//  ---------------------------------------------------------------------
//  A run button. Configuring standing authorization and triggering a deletion
//  sweep are different acts, and putting them on one panel would make the
//  second one a slip of the mouse away from the first. `autopilot run` is
//  offered here only as text to copy, with `--dry-run` already in it.
//
//  Also no self-expiry, because the envelope has none — see
//  `AutopilotWording.expiry`. The honest thing to do about a missing feature
//  on a consent screen is to say it out loud, not to leave the user to assume
//  the grant lapses on its own.
//

import AppKit
import SwiftUI

// MARK: - Reading the envelope

/// What one `autopilot show|enable|revoke --json` invocation turned out to be.
///
/// Four cases, split by the CLI's own exit contract rather than by severity
/// (`src/main.rs`):
///
///   - **0** — the report was printed to stdout. For `enable` and `revoke`
///     this is printed *after* the envelope was written, so the report is the
///     state now in force rather than the state requested;
///   - **1** — `autopilot_store_error_exit`: the envelope could not be read or
///     written. A prose sentence on stderr, no report;
///   - **2** — `autopilot_usage_exit`: the arguments were refused. Also prose,
///     plus the command's usage text. When it says "unrecognized argument"
///     this app sent flags the installed `glomeris` does not have, which is a
///     version skew and this app's problem, not the user's;
///   - exit 0 with undecodable stdout is the other half of a version skew.
enum AutopilotEnvelopeOutcome: Equatable {
    case envelope(AutopilotEnvelopeDto)
    case usageError(String)
    case malformedOutput
    case failed(String)
}

/// Pure, directly-testable mapping from an `autopilot` subcommand's exit code
/// and streams to an `AutopilotEnvelopeOutcome`.
enum AutopilotEnvelopeInterpretation {
    /// The prefix the CLI puts on its own stderr lines, stripped so the
    /// sentence reads as a sentence rather than as a log line. Matched as a
    /// literal; a mismatch degrades to showing the line verbatim.
    static let stderrPrefix = "glomeris autopilot: "

    /// Tells "this app sent flags that CLI does not have" apart from "that
    /// grant was refused". Only one of them is the user's to act on.
    static let usageErrorMarker = "unrecognized argument"

    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> AutopilotEnvelopeOutcome {
        // Decoded before the exit code is judged, for the same reason
        // `LlmCheckInterpretation` does it: if a report was printed it is the
        // best account of what is in force, whatever the exit code says.
        if let report = try? JSONDecoder().decode(AutopilotEnvelopeDto.self, from: stdout) {
            return .envelope(report)
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
                ? "glomeris autopilot exited with code \(exitCode) and printed no result."
                : Self.withoutPrefix(stderrText)
        )
    }

    private static func withoutPrefix(_ text: String) -> String {
        guard text.hasPrefix(Self.stderrPrefix) else { return text }
        return String(text.dropFirst(Self.stderrPrefix.count))
    }
}

// MARK: - The form

/// The grant being composed, as values.
///
/// Separate from the view so the part worth asserting is assertable: that the
/// argument vector it produces stays inside the ceilings the CLI reported,
/// never names a path, never carries a credential flag, and never asks for a
/// kind or a reason the CLI said could not be allowed. A form that could
/// compose a refusable grant would turn every one of the envelope's usage
/// errors into something the user sees only after pressing Enable.
///
/// The ceilings travel inside the draft rather than being reached for
/// globally, because clamping against a value this type does not hold is
/// clamping against an assumption.
struct AutopilotDraft: Equatable {
    /// Resource-kind tags, a subset of the report's `allowlistableKinds`.
    var allowedKinds: Set<String>

    /// `kind:reason` tokens — the same spelling as
    /// `AutopilotAskPreauthorizationDto.id` and as the CLI's
    /// `--preauthorize-ask` argument, so none of the three needs translating
    /// into the others.
    var preauthorizedAsks: Set<String>

    var maxActions: Int
    var maxGigabytes: Int
    var maxDurationSeconds: Int

    /// A pressure-state token from the report, or `nil` for the CLI's `none` —
    /// act whatever the disk is doing.
    var minPressure: String?

    let ceilings: AutopilotCeilingsDto

    /// Byte budgets are chosen in whole gigabytes because a stepper over
    /// 68,719,476,736 is not a control anybody can use. 1024-based, matching
    /// what `human_bytes` means by "GB" — the in-force figure on screen is the
    /// CLI's own rendering, and a mismatch would make the confirmation read as
    /// a different number from the request.
    static let bytesPerGigabyte = 1024 * 1024 * 1024

    /// Floors, and only floors, are this screen's own. The envelope's setters
    /// bound the budgets from above only, so `--max-actions 0` is accepted:
    /// an *enabled* grant that can do nothing. That is a coherent thing for a
    /// script to ask for and a pointless thing for a settings panel to offer,
    /// so the steppers start at one action, one gigabyte and five seconds.
    /// Narrowing what can be requested is always safe; widening it would not
    /// be.
    static let minimumActions = 1
    static let minimumGigabytes = 1
    static let minimumDurationSeconds = 5

    static func from(_ report: AutopilotEnvelopeDto) -> AutopilotDraft {
        AutopilotDraft(
            allowedKinds: Set(report.allowedKinds),
            preauthorizedAsks: Set(report.askPreauthorizations.map(\.id)),
            maxActions: Int(report.maxActions),
            // Rounded up, so a 512 MB envelope presents as the 1 GB the
            // stepper can represent instead of as zero. Rounding down would
            // let merely opening this screen and pressing Enable shrink a
            // budget the user did not touch.
            maxGigabytes: Self.gigabytesRoundingUp(report.maxBytes),
            maxDurationSeconds: Int(report.maxDurationSecs),
            minPressure: report.minPressure,
            ceilings: report.ceilings
        )
    }

    static func gigabytesRoundingUp(_ bytes: UInt64) -> Int {
        let unit = UInt64(Self.bytesPerGigabyte)
        return Int((bytes + unit - 1) / unit)
    }

    // MARK: Ceilings

    var actionsCeiling: Int { max(Self.minimumActions, Int(ceilings.maxActions)) }

    var gigabytesCeiling: Int {
        max(Self.minimumGigabytes, Int(ceilings.maxBytes / UInt64(Self.bytesPerGigabyte)))
    }

    var durationCeiling: Int {
        max(Self.minimumDurationSeconds, Int(ceilings.maxDurationSecs))
    }

    // MARK: What will actually be sent

    var clampedMaxActions: Int { min(max(maxActions, Self.minimumActions), actionsCeiling) }

    var clampedMaxGigabytes: Int { min(max(maxGigabytes, Self.minimumGigabytes), gigabytesCeiling) }

    var clampedMaxDurationSeconds: Int {
        min(max(maxDurationSeconds, Self.minimumDurationSeconds), durationCeiling)
    }

    var maxBytes: UInt64 { UInt64(clampedMaxGigabytes) * UInt64(Self.bytesPerGigabyte) }

    /// Pre-authorizations whose kind is still allowed, in a stable order.
    ///
    /// Pruned rather than kept: consenting in advance to an ASK about a kind
    /// Autopilot may not touch is dead weight that reads as a wider grant than
    /// it is. Unticking a kind therefore withdraws its pre-authorizations too,
    /// which is the direction a surprise is survivable in.
    var effectiveAsks: [String] {
        preauthorizedAsks
            .filter { allowedKinds.contains(Self.kind(ofAsk: $0)) }
            .sorted()
    }

    /// `true` when this draft is a grant the CLI will accept at all. `enable`
    /// requires at least one kind — an enabled envelope with an empty
    /// allowlist can execute nothing, and the CLI calls that a usage error
    /// rather than a silent no-op, so the button is disabled instead.
    var isGrantable: Bool { !allowedKinds.isEmpty }

    static func askToken(kind: String, reason: String) -> String { "\(kind):\(reason)" }

    static func kind(ofAsk ask: String) -> String {
        String(ask.prefix(while: { $0 != ":" }))
    }

    /// The complete `autopilot enable` invocation.
    ///
    /// Every field is always present — see this file's header on why omitting
    /// one is not "leave it alone" but "reset it to a default".
    var commandArguments: [String] {
        var arguments = ["autopilot", "enable", "--json"]
        arguments += ["--kinds", allowedKinds.sorted().joined(separator: ",")]
        arguments += ["--max-actions", String(clampedMaxActions)]
        arguments += ["--max-bytes", String(maxBytes)]
        arguments += ["--max-duration", String(clampedMaxDurationSeconds)]
        // `none` is the CLI's own spelling for "not gated on pressure", so the
        // threshold is stated either way rather than being left to a default.
        arguments += ["--min-pressure", minPressure ?? AutopilotCommands.noPressureThreshold]
        for ask in effectiveAsks {
            arguments += ["--preauthorize-ask", ask]
        }
        return arguments
    }
}

// MARK: - Commands

/// The argument vectors this screen runs, as data.
///
/// Pure values rather than literals inline in the view, so two properties are
/// assertable rather than eyeballed: nothing here names a filesystem path or a
/// credential, and the only `autopilot run` this screen knows about carries
/// `--dry-run`.
enum AutopilotCommands {
    static let show = ["autopilot", "show", "--json"]
    static let revoke = ["autopilot", "revoke", "--json"]

    /// The CLI's spelling for an ungated threshold.
    static let noPressureThreshold = "none"

    static func enable(_ draft: AutopilotDraft) -> [String] { draft.commandArguments }

    /// Shown for copying, never run from here. `--dry-run` is part of the
    /// value and not a flag the user has to remember to add: the text on a
    /// consent screen must not be one word away from a live deletion sweep.
    static let dryRunPreview = ["glomeris", "autopilot", "run", "--dry-run"]

    static var dryRunPreviewText: String { dryRunPreview.joined(separator: " ") }
}

// MARK: - Wording

/// Sentences this screen says in its own voice, kept out of the view so they
/// can be asserted and so none of them is invented twice.
///
/// Not in `GlomerisVocabulary`: none of these words a CLI token. They describe
/// this app's own surface and the envelope's own documented behaviour, so
/// there is nothing for `check-vocabulary-covers-cli-tokens.sh` to diff them
/// against.
enum AutopilotWording {
    /// The honest statement about expiry (there is none).
    ///
    /// The envelope has no expiry field, so a grant made here stays in force
    /// until it is revoked — across quits and restarts, because it is a file.
    /// Saying so plainly is the only option available: implying a lapse that
    /// never comes would be the worst possible lie on this particular screen,
    /// and inventing an expiry in Swift would be this client deciding policy.
    static let expiry = """
        This authorization does not expire on its own. It is stored on disk and \
        stays in force through quitting and restarting, until you revoke it \
        here or with `glomeris autopilot revoke`. Revocation takes effect \
        immediately and needs no confirmation.
        """

    /// Why the limits are still listed after a revocation.
    static let limitsSurviveRevocation = """
        Revoking keeps these limits on purpose, so enabling again later starts \
        from what you can see here rather than from something you never read.
        """

    static let replacesCurrentGrant = """
        Enabling replaces the whole authorization with exactly what is on this \
        screen — there is no part of it that is left as it was.
        """

    static func actionsLabel(_ count: Int) -> String {
        count == 1 ? "At most 1 action per run" : "At most \(count) actions per run"
    }

    static func gigabytesLabel(_ count: Int) -> String {
        count == 1 ? "At most 1 GB per run" : "At most \(count) GB per run"
    }

    static func durationLabel(_ seconds: Int) -> String {
        "Stop after \(seconds) seconds"
    }

    static func ceilingsNote(_ draft: AutopilotDraft) -> String {
        """
        Glomeris will not accept more than \(draft.actionsCeiling) actions, \
        \(draft.gigabytesCeiling) GB or \(draft.durationCeiling) seconds, \
        whatever is asked for here — those ceilings are in the CLI, not in \
        this window.
        """
    }

    /// Wording for the pressure threshold picker's "no threshold" choice.
    static let anyPressureTitle = "Whenever there is something to reclaim"

    static let anyPressureExplanation = """
        No disk-pressure threshold: a run may act even while the disk is \
        healthy.
        """

    static func pressureExplanation(_ term: GlomerisTerm) -> String {
        "Only act once the disk reaches \(term.title). \(term.explanation)"
    }
}

/// How the status card reads, as values.
///
/// A pure mapping, and the reason it is one is the second case: "revoked" and
/// "enabled" must not be presentable as the same thing with a different word
/// swapped in, and the thing keeping them apart should be testable without a
/// screenshot.
struct AutopilotStatusViewModel: Equatable {
    let title: String
    let detail: String
    let symbolName: String
    let tone: GlomerisTone

    static func make(_ report: AutopilotEnvelopeDto) -> AutopilotStatusViewModel {
        guard report.enabled else {
            return AutopilotStatusViewModel(
                title: "Autopilot is off",
                detail: "No run can delete anything. Glomeris still detects and explains; it "
                    + "just will not act without being asked each time.",
                symbolName: "pause.circle.fill",
                // Neutral, not positive: off is the default state, not an
                // achievement, and not a problem either.
                tone: .neutral
            )
        }
        return AutopilotStatusViewModel(
            title: "Autopilot is on",
            detail: "Glomeris may delete what is listed below, within these limits, without "
                + "asking you again.",
            symbolName: "bolt.circle.fill",
            // `.caution`, deliberately not `.critical`: standing deletion
            // authority is something to be aware of, and it is also the
            // feature working as designed. Red here would teach a user that
            // their own grant is a fault.
            tone: .caution
        )
    }
}

/// What to say after an **Enable** press.
///
/// The interesting cases are the two that are not "it worked": an exit-0
/// report that comes back *not enabled* (this app and that CLI disagree about
/// what just happened, and nothing on screen can be trusted), and a
/// malformed-but-successful run, where the grant *was* written and saying
/// "nothing was granted" would be exactly backwards.
enum AutopilotEnableWording {
    static func message(_ outcome: AutopilotEnvelopeOutcome) -> GlomerisStateMessage {
        switch outcome {
        case .envelope(let report) where report.enabled:
            return .success("Autopilot is now on, with exactly the authorization shown here.")

        case .envelope:
            return .failure(
                "glomeris saved the grant but reports Autopilot as off. Do not rely on this "
                    + "window: check it with `glomeris autopilot show`."
            )

        case .usageError(let detail):
            return .failure(
                "The installed glomeris rejected what this app asked for, so nothing was "
                    + "granted. The app and the CLI are probably different versions. It said: "
                    + detail
            )

        case .malformedOutput:
            // Exit 0, and `enable` prints only after `save_envelope` returned
            // Ok — so the grant is in force and this app cannot read it back.
            return .failure(
                "glomeris saved a grant but this app could not read what it saved, so nothing "
                    + "below describes it. Check it with `glomeris autopilot show`, and revoke "
                    + "it if it is not what you meant."
            )

        case .failed(let detail):
            return .failure("Nothing was granted. glomeris said: \(detail)")
        }
    }
}

/// What to say after a **Revoke** press.
///
/// Split from the enable wording rather than shared with a flag, because the
/// failure sentences are opposites. A failed enable leaves the user with less
/// than they asked for, which is safe; a failed revoke leaves standing
/// deletion authority in force while a button press suggests otherwise, and
/// that is the one outcome on this screen that must never be reported quietly.
enum AutopilotRevokeWording {
    static func message(_ outcome: AutopilotEnvelopeOutcome) -> GlomerisStateMessage {
        switch outcome {
        case .envelope(let report) where !report.enabled:
            return .success(
                "Autopilot is off. No run can execute anything until you enable it again. "
                    + AutopilotWording.limitsSurviveRevocation
            )

        case .envelope:
            return .failure(
                "glomeris still reports Autopilot as ON after revoking it. Assume the "
                    + "authorization is in force and revoke it with `glomeris autopilot revoke`."
            )

        case .usageError(let detail):
            return .failure(
                "The installed glomeris rejected what this app asked for, so Autopilot was NOT "
                    + "revoked and any authorization that was in force still is. Revoke it with "
                    + "`glomeris autopilot revoke`. It said: " + detail
            )

        case .malformedOutput:
            // Exit 0 here means `save_envelope` succeeded, so this one really
            // did revoke — only the readback is unreadable.
            return .failure(
                "Autopilot was revoked, but this app could not read the result, so what is shown "
                    + "below may be out of date."
            )

        case .failed(let detail):
            // The likely cause here is a store write that failed, so the
            // retry route is named even though it is the same route that
            // just failed: whatever stopped the write — a full disk, a
            // permission — has to be dealt with, and the user needs to know
            // there is something left to do rather than only that this
            // press did not work.
            return .failure(
                "Autopilot was NOT revoked — any authorization that was in force still is. Deal "
                    + "with what it reported and revoke it again here or with `glomeris autopilot "
                    + "revoke`. glomeris said: \(detail)"
            )
        }
    }
}

// MARK: - The screen

struct AutopilotPreferencesView: View {
    private let client: GlomerisClient

    /// The envelope in force, as last reported. `nil` until the first `show`
    /// returns — there is no form without it, because every choice the form
    /// offers is one of its fields.
    @State private var report: AutopilotEnvelopeDto?
    @State private var draft: AutopilotDraft?

    @State private var isLoading = false
    @State private var loadErrorMessage: String?
    @State private var loadOutcomeMessage: GlomerisStateMessage?

    @State private var isApplying = false
    @State private var actionMessage: GlomerisStateMessage?

    init(client: GlomerisClient = GlomerisClient()) {
        self.client = client
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                if let report, let draft {
                    statusCard(report)
                    kindsCard(report, draft)
                    limitsCard(draft)
                    pressureCard(report, draft)
                    preauthorizationCard(report, draft)
                    grantCard(report)
                    authorityCard(report)
                    dryRunCard
                } else {
                    unavailableCard
                }
            }
            .padding(GlomerisDesign.outerPadding)
        }
        .frame(width: 460, height: 560)
        // Reading the envelope is a read: `autopilot show` opens a file and
        // prints it. Enabling and revoking are writes and are bound to buttons
        // only — see `apply` and `revoke`.
        .task {
            await loadEnvelope()
        }
    }

    // MARK: Before there is a report

    private var unavailableCard: some View {
        GlomerisCard(title: "Autopilot") {
            Text(
                "Autopilot is standing permission for Glomeris to reclaim specific kinds of "
                    + "build output without asking you each time — within limits you set here, "
                    + "and never for anything its policy protects."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            if isLoading {
                GlomerisStateMessageView(message: .loading("Reading what is authorized…"))
            } else {
                // No form is offered without a report, and the reason is said
                // out loud: the alternative is a window full of plausible
                // defaults that were never anybody's policy.
                //
                // Both message slots are rendered here, not just the transport
                // failure: a version skew arrives as `loadOutcomeMessage`, and
                // it is the diagnosis a user needs most on this path.
                GlomerisStateMessageView(
                    message: loadOutcomeMessage
                        ?? .failure(
                            loadErrorMessage
                                ?? "Glomeris could not say what is authorized, so there is "
                                    + "nothing safe to show here."
                        ))
                Text(
                    "Every limit, kind and refusal on this screen comes from the glomeris "
                        + "command itself. Without it, this window would be guessing."
                )
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            Button(isLoading ? "Checking…" : "Check again") {
                Task { await loadEnvelope() }
            }
            .disabled(isLoading)
        }
    }

    // MARK: Status

    private func statusCard(_ report: AutopilotEnvelopeDto) -> some View {
        let status = AutopilotStatusViewModel.make(report)

        return GlomerisCard(title: "Autopilot", trailing: isLoading ? "checking…" : nil) {
            HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                Image(systemName: status.symbolName)
                    .foregroundStyle(status.tone.color)
                VStack(alignment: .leading, spacing: 2) {
                    Text(status.title)
                        .font(GlomerisDesign.primaryFont)
                    Text(status.detail)
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .accessibilityElement(children: .combine)

            inForceSummary(report)

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Button("Revoke now", role: .destructive) {
                    Task { await revoke() }
                }
                .disabled(isApplying)
                .help("Withdraws the authorization immediately. Nothing is deleted by this.")

                Button("Refresh") {
                    Task { await loadEnvelope() }
                }
                .disabled(isLoading)
                Spacer(minLength: 0)
            }

            if let actionMessage {
                GlomerisStateMessageView(message: actionMessage)
            }
            if let loadOutcomeMessage {
                GlomerisStateMessageView(message: loadOutcomeMessage)
            }
            if let loadErrorMessage {
                GlomerisStateMessageView(message: .failure(loadErrorMessage))
            }
        }
    }

    /// What is in force right now, in the CLI's own figures — including
    /// `maxBytesHuman`, so the confirmation is rendered by the thing that
    /// enforces it rather than re-formatted here.
    @ViewBuilder
    private func inForceSummary(_ report: AutopilotEnvelopeDto) -> some View {
        GlomerisDetailRow(label: "May touch") {
            if report.allowedKinds.isEmpty {
                Text("Nothing")
                    .font(GlomerisDesign.secondaryFont)
                    .foregroundStyle(.secondary)
            } else {
                Text(report.allowedKinds.map { GlomerisVocabulary.kind($0).title }
                    .joined(separator: ", "))
                    .font(GlomerisDesign.secondaryFont)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        GlomerisDetailRow(label: "Per run") {
            Text(
                "\(report.maxActions) actions · \(report.maxBytesHuman) · "
                    + "\(report.maxDurationSecs)s"
            )
            .font(GlomerisDesign.secondaryFont)
        }
        GlomerisDetailRow(label: "Disk pressure") {
            if let minPressure = report.minPressure {
                GlomerisBadgeView(term: GlomerisVocabulary.pressure(minPressure), filled: false)
            } else {
                Text("Any")
                    .font(GlomerisDesign.secondaryFont)
                    .foregroundStyle(.secondary)
            }
        }
        if let storedAt = report.storedAt {
            GlomerisDetailRow(label: "Stored at") {
                GlomerisPathText(path: storedAt)
            }
        }
    }

    // MARK: Kinds

    private func kindsCard(_ report: AutopilotEnvelopeDto, _ draft: AutopilotDraft) -> some View {
        GlomerisCard(title: "What It May Reclaim") {
            Text(
                "Only these kinds of resource, and only ones this Mac's own detectors found and "
                    + "Glomeris already classified as safe to reclaim. Ticking a kind does not "
                    + "mark anything for deletion."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            // Offered in the order the CLI reported them, which is
            // `ResourceKind::ALL`'s own order — not alphabetised here, so the
            // list a user reads matches the list the CLI documents.
            ForEach(report.allowlistableKinds, id: \.self) { kind in
                kindToggle(kind, draft)
            }

            if !report.neverAllowlistableKinds.isEmpty {
                Divider()
                Text("Never available, whatever is granted here:")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                ForEach(report.neverAllowlistableKinds, id: \.self) { kind in
                    let term = GlomerisVocabulary.kind(kind)
                    HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                        Image(systemName: "nosign")
                            .imageScale(.small)
                            .foregroundStyle(GlomerisTone.guarded.color)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(term.title)
                                .font(GlomerisDesign.secondaryFont)
                            Text(term.explanation)
                                .font(GlomerisDesign.captionFont)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("Never available: \(term.title). \(term.explanation)")
                }
            }
        }
    }

    private func kindToggle(_ kind: String, _ draft: AutopilotDraft) -> some View {
        let term = GlomerisVocabulary.kind(kind)
        return Toggle(isOn: kindBinding(kind)) {
            VStack(alignment: .leading, spacing: 2) {
                Text(term.title)
                    .font(GlomerisDesign.secondaryFont)
                Text(term.explanation)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityLabel("Allow \(term.title)")
        .accessibilityHint(term.explanation)
        .disabled(isApplying)
    }

    private func kindBinding(_ kind: String) -> Binding<Bool> {
        Binding(
            get: { draft?.allowedKinds.contains(kind) ?? false },
            set: { isOn in
                guard var updated = draft else { return }
                if isOn {
                    updated.allowedKinds.insert(kind)
                } else {
                    updated.allowedKinds.remove(kind)
                }
                draft = updated
            }
        )
    }

    // MARK: Limits

    private func limitsCard(_ draft: AutopilotDraft) -> some View {
        GlomerisCard(title: "How Much, Per Run") {
            Text(
                "Three independent budgets. A run stops at whichever it reaches first, and none "
                    + "of them can be raised past the ceiling the CLI enforces."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            Stepper(
                value: actionsBinding,
                in: AutopilotDraft.minimumActions...draft.actionsCeiling
            ) {
                Text(AutopilotWording.actionsLabel(draft.clampedMaxActions))
                    .font(GlomerisDesign.secondaryFont)
            }
            .accessibilityLabel("Maximum actions per run")

            Stepper(
                value: gigabytesBinding,
                in: AutopilotDraft.minimumGigabytes...draft.gigabytesCeiling
            ) {
                Text(AutopilotWording.gigabytesLabel(draft.clampedMaxGigabytes))
                    .font(GlomerisDesign.secondaryFont)
            }
            .accessibilityLabel("Maximum gigabytes per run")

            Stepper(
                value: durationBinding,
                in: AutopilotDraft.minimumDurationSeconds...draft.durationCeiling,
                step: 5
            ) {
                Text(AutopilotWording.durationLabel(draft.clampedMaxDurationSeconds))
                    .font(GlomerisDesign.secondaryFont)
            }
            .accessibilityLabel("Run time limit in seconds")

            Text(AutopilotWording.ceilingsNote(draft))
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var actionsBinding: Binding<Int> {
        Binding(
            get: { draft?.clampedMaxActions ?? AutopilotDraft.minimumActions },
            set: { newValue in
                guard var updated = draft else { return }
                updated.maxActions = newValue
                draft = updated
            }
        )
    }

    private var gigabytesBinding: Binding<Int> {
        Binding(
            get: { draft?.clampedMaxGigabytes ?? AutopilotDraft.minimumGigabytes },
            set: { newValue in
                guard var updated = draft else { return }
                updated.maxGigabytes = newValue
                draft = updated
            }
        )
    }

    private var durationBinding: Binding<Int> {
        Binding(
            get: { draft?.clampedMaxDurationSeconds ?? AutopilotDraft.minimumDurationSeconds },
            set: { newValue in
                guard var updated = draft else { return }
                updated.maxDurationSeconds = newValue
                draft = updated
            }
        )
    }

    // MARK: Pressure threshold

    private func pressureCard(
        _ report: AutopilotEnvelopeDto,
        _ draft: AutopilotDraft
    ) -> some View {
        GlomerisCard(title: "When It May Act") {
            Text(
                "A run can be held back until the disk is actually under pressure, so Autopilot "
                    + "does nothing on a machine that has plenty of room."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            Picker("Act only at", selection: pressureBinding) {
                Text(AutopilotWording.anyPressureTitle)
                    .tag(String?.none)
                // Straight from `pressure_states`, in the CLI's urgency order,
                // so this picker cannot offer a state `--min-pressure` would
                // reject.
                ForEach(report.pressureStates, id: \.self) { state in
                    Text(GlomerisVocabulary.pressure(state).title)
                        .tag(String?.some(state))
                }
            }
            .accessibilityLabel("Disk pressure threshold")
            .disabled(isApplying)

            Text(
                draft.minPressure.map { AutopilotWording.pressureExplanation(
                    GlomerisVocabulary.pressure($0)) }
                    ?? AutopilotWording.anyPressureExplanation
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var pressureBinding: Binding<String?> {
        Binding(
            get: { draft?.minPressure },
            set: { newValue in
                guard var updated = draft else { return }
                updated.minPressure = newValue
                draft = updated
            }
        )
    }

    // MARK: Pre-authorizing an ASK

    private func preauthorizationCard(
        _ report: AutopilotEnvelopeDto,
        _ draft: AutopilotDraft
    ) -> some View {
        GlomerisCard(title: "Answering In Advance") {
            Text(
                "Some resources are safe to reclaim but expensive to rebuild, so Glomeris stops "
                    + "and asks. You can answer one of those questions ahead of time, for one "
                    + "kind at a time. Nothing else can be answered in advance."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            if draft.allowedKinds.isEmpty {
                GlomerisStateMessageView(
                    message: .notLookedYet(
                        "Nothing to answer for yet",
                        detail: "Tick a kind above first — consenting in advance about something "
                            + "Autopilot may not touch would not mean anything."))
            } else {
                // The outer loop is the CLI's `preauthorizable_reasons`, so
                // the set of questions that can be answered in advance is
                // never this app's idea of it. Today there is exactly one.
                ForEach(report.preauthorizableReasons, id: \.self) { reason in
                    preauthorizableReason(reason, draft)
                }
            }

            if !report.neverPreauthorizableReasons.isEmpty {
                Divider()
                DisclosureGroup {
                    VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
                        ForEach(report.neverPreauthorizableReasons, id: \.self) { reason in
                            let term = GlomerisVocabulary.reason(reason)
                            HStack(
                                alignment: .firstTextBaseline,
                                spacing: GlomerisDesign.inlineSpacing
                            ) {
                                Image(systemName: "nosign")
                                    .imageScale(.small)
                                    .foregroundStyle(GlomerisTone.guarded.color)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(term.title)
                                        .font(GlomerisDesign.captionFont)
                                    Text(term.explanation)
                                        .font(GlomerisDesign.captionFont)
                                        .foregroundStyle(.secondary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    .padding(.top, 2)
                } label: {
                    Text(
                        "\(report.neverPreauthorizableReasons.count) things you cannot answer in "
                            + "advance"
                    )
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                }
                .accessibilityHint(
                    "Reasons Glomeris will always stop for, whatever is authorized here.")
            }
        }
    }

    @ViewBuilder
    private func preauthorizableReason(_ reason: String, _ draft: AutopilotDraft) -> some View {
        let term = GlomerisVocabulary.reason(reason)

        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            HStack(spacing: GlomerisDesign.inlineSpacing) {
                GlomerisBadgeView(term: term)
                Spacer(minLength: 0)
            }
            Text(term.explanation)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            // One checkbox per kind the user has already allowed — a
            // pre-authorization is a pair, and offering it as a single
            // blanket switch would be a wider grant than the envelope
            // actually stores.
            ForEach(draft.allowedKinds.sorted(), id: \.self) { kind in
                let kindTerm = GlomerisVocabulary.kind(kind)
                Toggle(isOn: askBinding(kind: kind, reason: reason)) {
                    Text(kindTerm.title)
                        .font(GlomerisDesign.captionFont)
                }
                .accessibilityLabel("Answer in advance for \(kindTerm.title): \(term.title)")
                .disabled(isApplying)
            }
        }
    }

    private func askBinding(kind: String, reason: String) -> Binding<Bool> {
        let token = AutopilotDraft.askToken(kind: kind, reason: reason)
        return Binding(
            get: { draft?.preauthorizedAsks.contains(token) ?? false },
            set: { isOn in
                guard var updated = draft else { return }
                if isOn {
                    updated.preauthorizedAsks.insert(token)
                } else {
                    updated.preauthorizedAsks.remove(token)
                }
                draft = updated
            }
        )
    }

    // MARK: Granting it

    private func grantCard(_ report: AutopilotEnvelopeDto) -> some View {
        GlomerisCard(title: report.enabled ? "Change This Authorization" : "Grant This") {
            Text(AutopilotWording.replacesCurrentGrant)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Text(AutopilotWording.expiry)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Button(isApplying ? "Saving…" : (report.enabled ? "Save changes" : "Enable")) {
                    Task { await apply() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(isApplying || !(draft?.isGrantable ?? false))

                if !(draft?.isGrantable ?? false) {
                    Text("Tick at least one kind first.")
                        .font(GlomerisDesign.captionFont)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }

            if isApplying {
                GlomerisStateMessageView(message: .loading("Saving the authorization…"))
            }
        }
    }

    // MARK: What the model may and may not do

    private func authorityCard(_ report: AutopilotEnvelopeDto) -> some View {
        GlomerisCard(title: "What the AI Decides") {
            // Sentence for sentence from `ai_authority`. Not paraphrased: each
            // line is a claim about the Rust code's behaviour, and a
            // paraphrase in Swift would be this app's claim instead.
            // Keyed on the offset rather than on the sentence: two identical
            // sentences would collapse into one row under `id: \.self`, which
            // would silently drop a constraint from a list of constraints.
            ForEach(Array(report.aiAuthority.enumerated()), id: \.offset) { pair in
                HStack(alignment: .firstTextBaseline, spacing: GlomerisDesign.inlineSpacing) {
                    Image(systemName: "checkmark.shield")
                        .imageScale(.small)
                        .foregroundStyle(GlomerisTone.guarded.color)
                    Text(pair.element)
                        .font(GlomerisDesign.captionFont)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .combine)
            }

            if !report.neverExecutableLabels.isEmpty {
                Divider()
                Text("Glomeris will never delete anything it has classified as:")
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: GlomerisDesign.inlineSpacing) {
                    ForEach(report.neverExecutableLabels, id: \.self) { token in
                        GlomerisBadgeView(term: GlomerisVocabulary.safety(token))
                    }
                    Spacer(minLength: 0)
                }
                Text(
                    "That holds whatever is authorized on this screen, and whatever the model "
                        + "recommends."
                )
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    // MARK: Seeing what it would do

    private var dryRunCard: some View {
        GlomerisCard(title: "See What It Would Do") {
            Text(
                "Autopilot runs are not started from this window. To see what a run would pick, "
                    + "without deleting anything, run this in a terminal:"
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Text(AutopilotCommands.dryRunPreviewText)
                    .font(GlomerisDesign.monospacedFont)
                    .textSelection(.enabled)
                Spacer(minLength: 0)
                Button("Copy") {
                    let pasteboard = NSPasteboard.general
                    pasteboard.clearContents()
                    pasteboard.setString(AutopilotCommands.dryRunPreviewText, forType: .string)
                }
                .accessibilityLabel("Copy the dry-run command")
            }
        }
    }

    // MARK: Running the commands

    /// Reads the envelope. The only command on this screen that changes
    /// nothing, and the only one allowed to run without a button press.
    ///
    /// The draft is rebuilt from every successful read, including the reads
    /// that follow an enable or a revoke — so what the form shows is always
    /// the last thing the CLI said is in force, never the last thing this
    /// window asked for.
    private func loadEnvelope() async {
        isLoading = true
        loadErrorMessage = nil
        loadOutcomeMessage = nil

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
                draft = AutopilotDraft.from(fresh)
            case .usageError(let detail):
                loadOutcomeMessage = .failure(
                    "The installed glomeris does not understand what this app asked it to do, so "
                        + "this screen cannot show what is authorized. The app and the CLI are "
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
            loadErrorMessage = SectionFetchErrors.shortMessage(error, subject: "autopilot show")
        }

        isLoading = false
    }

    /// Writes the grant, then re-reads it.
    ///
    /// Called only from the Enable button's action closure, in a detached
    /// `Task {}`: this one writes a file, and a cancelled write reported as
    /// nothing at all would leave the user unsure whether they had just
    /// granted standing deletion authority. There is no Stop button for the
    /// same reason.
    private func apply() async {
        guard let draft else { return }

        isApplying = true
        actionMessage = nil

        do {
            let raw = try await client.runRaw(
                AutopilotCommands.enable(draft),
                progressType: ProgressEventDto.self
            )
            let outcome = AutopilotEnvelopeInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
            actionMessage = AutopilotEnableWording.message(outcome)
            adopt(outcome)
        } catch {
            actionMessage = .failure(
                SectionFetchErrors.shortMessage(error, subject: "autopilot enable")
                    ?? "The attempt to save the authorization was stopped, so it is not known "
                        + "whether it was saved. Check with `glomeris autopilot show`."
            )
        }

        isApplying = false
        // Whatever happened, including a failure: the authoritative answer to
        // "what is in force now" is another read, not what this window tried
        // to do.
        await loadEnvelope()
    }

    private func revoke() async {
        isApplying = true
        actionMessage = nil

        do {
            let raw = try await client.runRaw(
                AutopilotCommands.revoke,
                progressType: ProgressEventDto.self
            )
            let outcome = AutopilotEnvelopeInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
            actionMessage = AutopilotRevokeWording.message(outcome)
            adopt(outcome)
        } catch {
            actionMessage = .failure(
                SectionFetchErrors.shortMessage(error, subject: "autopilot revoke")
                    ?? "The revocation was stopped, so any authorization that was in force may "
                        + "still be. Revoke it with `glomeris autopilot revoke`."
            )
        }

        isApplying = false
        await loadEnvelope()
    }

    /// Takes on an envelope a write reported back, so the screen updates from
    /// the state that reached the file rather than from the form.
    private func adopt(_ outcome: AutopilotEnvelopeOutcome) {
        guard case .envelope(let fresh) = outcome else { return }
        report = fresh
        draft = AutopilotDraft.from(fresh)
    }
}

#Preview {
    AutopilotPreferencesView()
}
