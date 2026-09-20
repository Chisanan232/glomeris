//
//  AiProviderPreferencesView.swift
//  GlomerisMenuBar
//
//  HORO-1309: the BYOK provider settings screen — endpoint, model, key, a
//  connection test, and a preview of exactly what an AI Plan would send.
//
//  Before this, configuring a provider meant exporting three environment
//  variables in a shell and relaunching the app from that shell. A menu-bar
//  agent launched from Finder inherits no shell environment at all, so the
//  documented setup silently did nothing for the most common way to start the
//  app. The AI Plan card's own "No AI provider configured" message had a
//  placeholder in it pointing here.
//
//  ---------------------------------------------------------------------
//  This file decides nothing about providers
//  ---------------------------------------------------------------------
//  See the standing project rule in GlomerisMenuBarApp.swift. There is no URL
//  validation here, no model list, no retry policy, and no HTTP client. The
//  only judge of whether a configuration can work is the Rust CLI's
//  `validate_base_url` / `provider_from_env`, and the only way its verdict
//  reaches this screen is by running `glomeris llm-check --json` and rendering
//  the token it printed. A second copy of those rules in Swift would drift,
//  and the field it would drift on — "is this URL the API root?" — is exactly
//  the one HORO-1299 showed is worth getting right.
//
//  ---------------------------------------------------------------------
//  Where the key goes, and where it does not
//  ---------------------------------------------------------------------
//  Into the keychain (`KeychainCredentialStore`) and into one child process's
//  environment at the moment that child is spawned. Never into UserDefaults,
//  never a file, never a log line, never an argument vector — the CLI itself
//  refuses `--api-key`/`--key`/`--token` by flag name for the same reason, and
//  `scripts/check-credential-store-uses-keychain.sh` asserts every one of
//  those properties over this whole target mechanically.
//
//  The key is bound to a `SecureField` and is cleared from memory the moment
//  it is saved. There is no "show key" affordance and no way to read a stored
//  key back for display: `GlomerisLlmSettingsStore` exposes
//  `hasStoredApiKey`, a `Bool`, and nothing else (AC 5).
//
//  ---------------------------------------------------------------------
//  Nothing here contacts a provider except the Test button
//  ---------------------------------------------------------------------
//  Two commands are run from this screen, both only from a button's action
//  closure — no `.onAppear`, no `.task`, no `Timer`, no retry:
//
//    * `llm-check --json` — the connection test. This DOES make one bounded
//      request, which is the entire point of pressing it, and the button says
//      so before it is pressed.
//    * `llm-plan --print-payload --json` — the privacy preview. This sends
//      NOTHING. `run_llm_plan_command` returns from the `--print-payload`
//      branch before a provider is ever constructed, so there is no
//      credential read, no network call, and no charge. AC 7 turns on that
//      distinction, so the preview deliberately runs on the plain client
//      rather than the credential-carrying one: a command that needs no key
//      does not get one.
//

import AppKit
import SwiftUI

// MARK: - Connection test

/// What one `llm-check` invocation turned out to be.
///
/// Five cases, split the way the CLI's own exit contract splits them rather
/// than by severity, because two of them are the user's to fix, two are a
/// broken install, and one is the provider's:
///
///   - `report` is a check that actually ran. Its `outcome` token says whether
///     it succeeded, and it arrives on exit 0 (`"ok"`) *and* exit 1 (every
///     other outcome) — the report is printed before the non-zero exit;
///   - `misconfigured` is exit 2: the settings cannot work as they stand, so
///     nothing was sent. There is no report at all on this path, which is why
///     this case carries the CLI's own sentence instead;
///   - `usageError` is also exit 2, but with "unrecognized argument" on
///     stderr. That is this app sending flags the installed `glomeris` does
///     not have — a version skew, and the app's bug, not the user's;
///   - `malformedOutput` is exit 0 with something undecodable, the other half
///     of a version skew;
///   - `failed` is everything else, carrying the CLI's stderr rather than a
///     sentence invented here.
enum LlmCheckOutcome: Equatable {
    case report(LlmCheckReportDto)
    case misconfigured(String)
    case usageError(String)
    case malformedOutput
    case failed(String)
}

/// Pure, directly-testable mapping from `llm-check`'s exit code and streams to
/// an `LlmCheckOutcome`.
///
/// The contract is `run_llm_check_command`'s, read from `src/main.rs` rather
/// than assumed:
///
///   * **0** — the report was printed to stdout and `outcome == "ok"`;
///   * **1** — the report was printed to stdout FIRST, then the process exited
///     1 because `outcome != "ok"`. A failed check is a completed check, and
///     its report is the only structured account of how it failed;
///   * **2** — no report at all. Either a credential/unknown flag
///     ("unrecognized argument …"), or `provider_from_env` refused: an
///     `InvalidConfiguration` detail sentence, or the missing-configuration
///     sentence naming the three environment variables.
///
/// Note the asymmetry with `llm-plan`, which prints a JSON report on exit 1
/// *and* has no exit-2 report either. Both mappers therefore decode stdout
/// before judging the exit code, and both treat exit 2 as prose-only.
enum LlmCheckInterpretation {
    /// The prefix the CLI puts on its own stderr lines. Stripped so the
    /// sentence reads as a sentence in a settings window instead of as a
    /// terminal log line; matched as a literal because it is the CLI's, and a
    /// mismatch degrades to showing the line verbatim rather than to hiding it.
    static let stderrPrefix = "glomeris llm-check: "

    /// Tells "this app sent bad flags" apart from "the user's settings cannot
    /// work". Both exit 2, and only one of them is worth pointing the user at
    /// their own fields for.
    static let usageErrorMarker = "unrecognized argument"

    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> LlmCheckOutcome {
        let stderrText = Self.trimmedText(stderr)

        // Tried before the exit code is judged: exit 1 carries a full report,
        // and discarding it would throw away the diagnosis while keeping the
        // failure — precisely the HORO-1299 collapse this ticket undoes.
        if let report = try? JSONDecoder().decode(LlmCheckReportDto.self, from: stdout) {
            return .report(report)
        }

        if exitCode == 2 {
            if stderrText.contains(Self.usageErrorMarker) {
                return .usageError(
                    stderrText.isEmpty
                        ? "The installed glomeris rejected the arguments this app sent."
                        : Self.withoutPrefix(stderrText)
                )
            }
            return .misconfigured(
                stderrText.isEmpty
                    ? "glomeris refused these settings but said nothing about why."
                    : Self.withoutPrefix(stderrText)
            )
        }

        if exitCode == 0 {
            return .malformedOutput
        }

        return .failed(
            stderrText.isEmpty
                ? "glomeris llm-check exited with code \(exitCode) and printed no result."
                : Self.withoutPrefix(stderrText)
        )
    }

    private static func withoutPrefix(_ text: String) -> String {
        guard text.hasPrefix(Self.stderrPrefix) else { return text }
        return String(text.dropFirst(Self.stderrPrefix.count))
    }

    private static func trimmedText(_ data: Data) -> String {
        String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Pure formatting step from an `LlmCheckOutcome` to what the result area
/// renders.
///
/// Extracted from the view because the interesting claim of AC 4 is that a
/// wrong key, an unreachable host and a URL that is not an API root read as
/// three different things — and that is a claim about this mapping, not about
/// SwiftUI. `term` is deliberately `nil` for the two version-skew cases:
/// `GlomerisVocabulary.llmCheckOutcome` words results the CLI produced, and
/// dressing "this app and that CLI disagree" in the same chip would attribute
/// a local install problem to the provider.
struct LlmCheckResultViewModel: Equatable {
    /// Wording for the CLI's own outcome token, when a check actually ran or
    /// was refused locally. `nil` when what broke is this app's conversation
    /// with the CLI.
    let term: GlomerisTerm?

    /// One secret-free sentence: the report's `error`, or the CLI's stderr.
    /// `nil` on success, where the excerpt below is the interesting part.
    let detail: String?

    /// Echoed back from the report so a successful check says *which* model
    /// answered, and a rejection says which path was posted to. Neither is a
    /// secret and both are things the user typed; the report carries no base
    /// URL and no key at all, so there is nothing else here to leak.
    let model: String?
    let endpointPath: String?

    /// A bounded excerpt of what the provider actually replied, present only
    /// for `"ok"`. Proof of life rather than decoration: "something answered
    /// at that address" and "it answered as a chat-completions API" are
    /// different facts, and this is the second one.
    let responseExcerpt: String?

    static func make(_ outcome: LlmCheckOutcome) -> LlmCheckResultViewModel {
        switch outcome {
        case .report(let report):
            return LlmCheckResultViewModel(
                term: GlomerisVocabulary.llmCheckOutcome(report.outcome),
                detail: report.error,
                model: report.model.isEmpty ? nil : report.model,
                endpointPath: report.endpointPath.isEmpty ? nil : report.endpointPath,
                responseExcerpt: report.responseExcerpt
            )

        case .misconfigured(let detail):
            // The same wording a report-carried `"misconfigured"` would get.
            // The user should not be able to tell that one of these paths
            // printed a report and the other did not — the fact that matters
            // is identical either way: nothing was sent, and a field is wrong.
            return LlmCheckResultViewModel(
                term: GlomerisVocabulary.llmCheckOutcome("misconfigured"),
                detail: detail,
                model: nil,
                endpointPath: nil,
                responseExcerpt: nil
            )

        case .usageError(let detail):
            return LlmCheckResultViewModel(
                term: nil,
                detail: "The installed glomeris does not understand what this app asked it to "
                    + "do, so no check ran. The app and the CLI are probably different "
                    + "versions. It said: \(detail)",
                model: nil,
                endpointPath: nil,
                responseExcerpt: nil
            )

        case .malformedOutput:
            return LlmCheckResultViewModel(
                term: nil,
                detail: "The installed glomeris reported success but this app could not read "
                    + "its result. The app and the CLI are probably different versions.",
                model: nil,
                endpointPath: nil,
                responseExcerpt: nil
            )

        case .failed(let detail):
            return LlmCheckResultViewModel(
                term: nil,
                detail: detail,
                model: nil,
                endpointPath: nil,
                responseExcerpt: nil
            )
        }
    }
}

// MARK: - Privacy preview

/// What one `llm-plan --print-payload` invocation turned out to be.
///
/// Three cases rather than five: `--print-payload` never reaches a provider,
/// so there is no provider failure to distinguish, and it needs no credential,
/// so there is no not-configured case either. That is the property AC 7 is
/// about, and it is visible right here in the shape of this type — a preview
/// that could report "no provider configured" would be a preview that had
/// tried to use one.
enum LlmPayloadPreviewOutcome: Equatable {
    case payload(LlmPayloadReportDto)
    case malformedOutput
    case failed(String)
}

/// Pure mapping from `llm-plan --print-payload`'s exit code and streams.
///
/// `run_llm_plan_command`'s `--print-payload` branch prints the report and
/// returns; on an error it prints a sentence to stderr and exits 1. There is
/// no exit-1-with-a-report case here, unlike a live plan.
enum LlmPayloadPreviewInterpretation {
    static func interpret(exitCode: Int32, stdout: Data, stderr: Data) -> LlmPayloadPreviewOutcome {
        if let report = try? JSONDecoder().decode(LlmPayloadReportDto.self, from: stdout) {
            return .payload(report)
        }

        let stderrText = String(decoding: stderr, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)

        if exitCode == 0 {
            return .malformedOutput
        }

        return .failed(
            stderrText.isEmpty
                ? "glomeris llm-plan --print-payload exited with code \(exitCode) and printed "
                    + "nothing."
                : stderrText
        )
    }
}

// MARK: - Commands

/// The two argument vectors this screen runs, as data.
///
/// Pure functions rather than literals inline in the view, because two
/// properties of them are worth asserting rather than eyeballing: neither
/// carries a credential flag (the CLI would refuse it, and `ps` would show
/// it), and the preview really does pass `--print-payload`, which is the flag
/// that makes it a preview instead of a paid request.
enum AiProviderCommands {
    /// One bounded request to the configured provider. `--json` because this
    /// app reads the report rather than the human rendering.
    static let connectionTest = ["llm-check", "--json"]

    /// Builds the request without sending it. `projectRootArguments` comes
    /// from `ProjectRootsStore.commandLineArguments`, the same value
    /// `AiPlanSectionView` passes to a live `llm-plan` — a preview scoped
    /// differently from the real call would disclose the wrong payload, which
    /// is worse than disclosing none.
    static func payloadPreview(projectRootArguments: [String]) -> [String] {
        ["llm-plan", "--print-payload", "--json"] + projectRootArguments
    }
}

// MARK: - Field source wording

/// Plain-language wording for where one setting came from.
///
/// Not in `GlomerisVocabulary`: this is not a CLI token. `GlomerisLlmSettingSource`
/// is decided in Swift, by this app, about this app's own storage, so there is
/// no Rust producer to diff it against and nothing for the
/// `vocabulary-covers-cli-tokens` job to check.
enum GlomerisLlmSettingSourceWording {
    static func title(_ source: GlomerisLlmSettingSource) -> String {
        switch source {
        case .settings: return "Set here"
        case .environment: return "From the environment this app was launched with"
        case .absent: return "Not set"
        }
    }

    static func symbolName(_ source: GlomerisLlmSettingSource) -> String {
        switch source {
        case .settings: return "pencil"
        case .environment: return "terminal"
        case .absent: return "circle.dashed"
        }
    }
}

// MARK: - Opening this screen from elsewhere

/// A button that opens the Settings scene.
///
/// `SettingsLink` is the supported route and needs macOS 14; the deployment
/// target is 13.0. On 13 there is no menu bar to fall back to either — the app
/// is `LSUIElement` — so the fallback is the AppKit action the `Settings`
/// scene installs, which is what the ⌘, key equivalent would have invoked.
struct GlomerisSettingsButton: View {
    let title: String

    var body: some View {
        if #available(macOS 14, *) {
            SettingsLink {
                Text(title)
            }
        } else {
            Button(title) {
                NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
            }
        }
    }
}

// MARK: - The screen

struct AiProviderPreferencesView: View {
    private let client: GlomerisClient
    private let store: GlomerisLlmSettingsStore
    private let projectRootsStore: ProjectRootsStore

    @State private var endpointText: String
    @State private var modelText: String

    /// The key as typed, held only until Save and cleared immediately after.
    /// Bound to a `SecureField` and to nothing else; there is no code path
    /// from a stored key back into this variable, because
    /// `GlomerisLlmSettingsStore` has no getter for one.
    @State private var apiKey: String = ""

    @State private var status: GlomerisLlmSettingsStatus
    @State private var keyActionMessage: GlomerisStateMessage?

    @State private var isTesting = false
    @State private var checkOutcome: LlmCheckOutcome?
    @State private var checkErrorMessage: String?
    @State private var testTask: Task<Void, Never>?

    @State private var isPreviewing = false
    @State private var previewOutcome: LlmPayloadPreviewOutcome?
    @State private var previewErrorMessage: String?
    @State private var previewTask: Task<Void, Never>?

    init(
        client: GlomerisClient = GlomerisClient(),
        store: GlomerisLlmSettingsStore = GlomerisLlmSettingsStore(),
        projectRootsStore: ProjectRootsStore = ProjectRootsStore()
    ) {
        self.client = client
        self.store = store
        self.projectRootsStore = projectRootsStore
        _endpointText = State(initialValue: store.endpoint ?? "")
        _modelText = State(initialValue: store.model ?? "")
        _status = State(initialValue: store.status())
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: GlomerisDesign.sectionSpacing) {
                providerCard
                credentialCard
                connectionCard
                privacyCard
            }
            .padding(GlomerisDesign.outerPadding)
        }
        .frame(width: 460, height: 560)
    }

    // MARK: Provider

    private var providerCard: some View {
        GlomerisCard(title: "AI Provider") {
            Text(
                "Glomeris talks to any OpenAI-compatible chat-completions API. Give it the "
                    + "address of the API root — not a full completions URL — and the model name "
                    + "your account can use."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 2) {
                TextField("https://api.example.com/v1", text: endpointBinding)
                    .textFieldStyle(.roundedBorder)
                    .font(GlomerisDesign.monospacedFont)
                    .accessibilityLabel("Provider API address")
                sourceRow(label: "Address", source: status.endpoint)
            }

            VStack(alignment: .leading, spacing: 2) {
                TextField("model-name", text: modelBinding)
                    .textFieldStyle(.roundedBorder)
                    .font(GlomerisDesign.monospacedFont)
                    .accessibilityLabel("Model name")
                sourceRow(label: "Model", source: status.model)
            }

            if status.endpoint == .environment || status.model == .environment
                || status.apiKey == .environment
            {
                Text(
                    "Anything left empty here falls back to the GLOMERIS_LLM_* variables this "
                        + "app inherited. Note that an app launched from Finder inherits no "
                        + "shell environment, so variables exported in a terminal are only "
                        + "visible if you launched it from that terminal."
                )
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// Writes straight through to the store on every keystroke, so the field
    /// and the request can never disagree about which endpoint is in use —
    /// there is no unsaved state on this screen to lose.
    ///
    /// A custom `Binding` rather than `.onChange(of:perform:)`, which is
    /// deprecated as of macOS 14 while the deployment target is still 13.0, so
    /// neither the old spelling nor the new one is available warning-free.
    ///
    /// Note what this does NOT do: correct, complete or reject the value.
    /// Whether a URL can work is `validate_base_url`'s call in the Rust CLI,
    /// reported by Test connection.
    private var endpointBinding: Binding<String> {
        Binding(
            get: { endpointText },
            set: { newValue in
                endpointText = newValue
                store.endpoint = newValue
                status = store.status()
            }
        )
    }

    private var modelBinding: Binding<String> {
        Binding(
            get: { modelText },
            set: { newValue in
                modelText = newValue
                store.model = newValue
                status = store.status()
            }
        )
    }

    private func sourceRow(label: String, source: GlomerisLlmSettingSource) -> some View {
        HStack(spacing: 3) {
            Image(systemName: GlomerisLlmSettingSourceWording.symbolName(source))
                .imageScale(.small)
            Text("\(label): \(GlomerisLlmSettingSourceWording.title(source))")
                .font(GlomerisDesign.captionFont)
        }
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .combine)
    }

    // MARK: Credential

    private var credentialCard: some View {
        GlomerisCard(title: "API Key") {
            Text(
                "Stored in your macOS keychain. It is passed to the glomeris command in its "
                    + "environment, never on a command line, and it is never written to a log, "
                    + "a file, or this app's preferences."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                SecureField("Paste your key", text: $apiKey)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("API key")
                    .onSubmit { saveKey() }
                Button("Save") {
                    saveKey()
                }
                .disabled(apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                sourceRow(label: "Key", source: status.apiKey)
                Spacer(minLength: 0)
                if store.hasStoredApiKey {
                    // AC 8. Explicit, always visible once a key exists, and
                    // labelled with what it does rather than with "Clear" —
                    // revoking a credential should be the easiest thing on
                    // this screen to find and the hardest to do by accident.
                    Button("Remove key", role: .destructive) {
                        removeKey()
                    }
                }
            }

            if let keyActionMessage {
                GlomerisStateMessageView(message: keyActionMessage)
            }
        }
    }

    private func saveKey() {
        let typed = apiKey
        guard !typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }

        let saved = store.setApiKey(typed)
        // Cleared whatever happened: leaving it in a `@State` after a failed
        // write keeps a secret in memory for as long as the window is open,
        // and the user can paste again.
        apiKey = ""
        status = store.status()
        keyActionMessage =
            saved
            ? .success("Key saved to your keychain.")
            : .failure(
                "macOS refused to store the key in the keychain. Nothing was saved, and "
                    + "nothing was written anywhere else.")
    }

    private func removeKey() {
        let removed = store.deleteApiKey()
        status = store.status()
        keyActionMessage =
            removed
            ? .success("Key removed from your keychain.")
            : .failure("macOS refused to remove the key from the keychain.")
    }

    // MARK: Connection test

    private var connectionCard: some View {
        GlomerisCard(title: "Test Connection") {
            Text(
                "Sends one tiny request — two words — to the address above, to find out whether "
                    + "the address, the key and the model all work. It reports what came back "
                    + "without showing your key."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Button(isTesting ? "Testing…" : "Test connection") {
                    testTask = Task { await runConnectionTest() }
                }
                .disabled(isTesting || !status.isComplete)

                if isTesting {
                    Button("Stop") {
                        testTask?.cancel()
                    }
                }
                Spacer(minLength: 0)
            }

            if !status.isComplete {
                Text(
                    "All three of the address, the model and the key are needed. A test without "
                        + "them would fail here without ever reaching your provider."
                )
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
            }

            if isTesting {
                GlomerisStateMessageView(message: .loading("Asking your provider…"))
            } else if let checkOutcome {
                checkResult(LlmCheckResultViewModel.make(checkOutcome))
            }

            if let checkErrorMessage {
                GlomerisStateMessageView(message: .failure(checkErrorMessage))
            }
        }
    }

    @ViewBuilder
    private func checkResult(_ model: LlmCheckResultViewModel) -> some View {
        VStack(alignment: .leading, spacing: GlomerisDesign.rowSpacing) {
            if let term = model.term {
                GlomerisBadgeView(term: term)
                Text(term.explanation)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if let detail = model.detail {
                // Verbatim from the CLI. `LlmError`'s `Display` is asserted
                // key-free per variant in the Rust tests, and the report
                // carries no base URL and no key for this to interpolate.
                Text(detail)
                    .font(GlomerisDesign.captionFont)
                    .foregroundStyle(model.term == nil ? .primary : .secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }

            if let endpointPath = model.endpointPath {
                GlomerisDetailRow(label: "Posted to") {
                    GlomerisPathText(path: endpointPath)
                }
            }
            if let modelName = model.model {
                GlomerisDetailRow(label: "Model") {
                    Text(modelName)
                        .font(GlomerisDesign.monospacedFont)
                }
            }
            if let excerpt = model.responseExcerpt {
                GlomerisDetailRow(label: "It replied") {
                    Text(excerpt)
                        .font(GlomerisDesign.monospacedFont)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// The one place in this app that deliberately spends the user's money,
    /// called only from the Test connection button's action closure.
    ///
    /// `runRaw` rather than `run`: exit 1 is a check that completed and came
    /// back negative, and its report is the diagnosis. Run through
    /// `withEnvironment` so the child sees what this screen has configured —
    /// otherwise the test would check the inherited environment while a plan
    /// used the settings, and a passing test would prove nothing about the
    /// thing the user is about to do.
    private func runConnectionTest() async {
        isTesting = true
        checkErrorMessage = nil

        do {
            // `progressType` is named only to satisfy generic inference —
            // neither of this screen's commands is given `--progress-json`, so
            // no progress event is ever emitted to decode.
            let raw = try await client
                .withEnvironment(store.childEnvironment())
                .runRaw(
                    AiProviderCommands.connectionTest,
                    progressType: ProgressEventDto.self
                )
            checkOutcome = LlmCheckInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
        } catch {
            checkErrorMessage = SectionFetchErrors.shortMessage(error, subject: "connection test")
        }

        isTesting = false
        testTask = nil
    }

    // MARK: Privacy preview

    private var privacyCard: some View {
        GlomerisCard(title: "What Gets Sent") {
            Text(
                "Builds the exact request an AI plan would send, without sending it. Nothing "
                    + "leaves this Mac until you ask for a plan."
            )
            .font(GlomerisDesign.captionFont)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: GlomerisDesign.inlineSpacing) {
                Button(isPreviewing ? "Building…" : "Show what would be sent") {
                    previewTask = Task { await runPayloadPreview() }
                }
                .disabled(isPreviewing)

                if isPreviewing {
                    Button("Stop") {
                        previewTask?.cancel()
                    }
                }
                Spacer(minLength: 0)
            }

            if isPreviewing {
                GlomerisStateMessageView(
                    message: .loading("Looking at this machine to build the request…"))
            } else if let previewOutcome {
                previewResult(previewOutcome)
            }

            if let previewErrorMessage {
                GlomerisStateMessageView(message: .failure(previewErrorMessage))
            }
        }
    }

    @ViewBuilder
    private func previewResult(_ outcome: LlmPayloadPreviewOutcome) -> some View {
        switch outcome {
        case .payload(let report):
            // Two groups, and the separation is the disclosure. The prompts
            // leave the machine; the alias table is how Glomeris keeps real
            // paths off the wire and is never transmitted. Showing them as one
            // list would misrepresent the very thing this card exists to show.
            payloadGroup(
                title: "Leaves this Mac",
                symbolName: "paperplane.fill",
                tone: .caution,
                caption: "\(report.systemPrompt.count + report.userPrompt.count) characters in "
                    + "total, sent to the address above."
            ) {
                Text(report.systemPrompt)
                    .font(GlomerisDesign.monospacedFont)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                Divider()
                Text(report.userPrompt)
                    .font(GlomerisDesign.monospacedFont)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }

            payloadGroup(
                title: "Stays on this Mac",
                symbolName: "lock.fill",
                // `.guarded`, not `.positive`: blue is this app's tone for a
                // deliberate hold, and green is reserved for AUTO_SAFE and for
                // something that succeeded (see `GlomerisTone.style`). Data
                // being withheld is the former.
                tone: .guarded,
                caption: report.resourceAliases.isEmpty
                    ? "No resources to stand in for — there was nothing to plan about."
                    : "Glomeris replaces real paths with the placeholders above. This is the "
                        + "table it maps them back with, and it is never sent."
            ) {
                ForEach(report.resourceAliases) { alias in
                    GlomerisDetailRow(label: alias.wireResourceId) {
                        GlomerisPathText(path: alias.localResourceId)
                    }
                }
            }

        case .malformedOutput:
            GlomerisStateMessageView(
                message: .failure(
                    "The installed glomeris reported success but this app could not read the "
                        + "payload it printed. The app and the CLI are probably different "
                        + "versions."))

        case .failed(let message):
            GlomerisStateMessageView(message: .failure(message))
        }
    }

    @ViewBuilder
    private func payloadGroup<Content: View>(
        title: String,
        symbolName: String,
        tone: GlomerisTone,
        caption: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 3) {
                Image(systemName: symbolName)
                    .imageScale(.small)
                Text(title)
                    .font(GlomerisDesign.badgeFont)
            }
            .foregroundStyle(tone.color)

            Text(caption)
                .font(GlomerisDesign.captionFont)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            VStack(alignment: .leading, spacing: 3) {
                content()
            }
            .padding(GlomerisDesign.cardPadding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: GlomerisDesign.badgeCornerRadius)
                    .stroke(tone.color.opacity(0.4))
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title). \(caption)")
    }

    /// Runs `llm-plan --print-payload`, which sends nothing anywhere.
    ///
    /// On the plain `client`, deliberately: this command constructs no
    /// provider, so it needs no credential, so it does not get one. A preview
    /// that carried the key would be a preview one keystroke away from being
    /// a request.
    private func runPayloadPreview() async {
        isPreviewing = true
        previewErrorMessage = nil

        do {
            let raw = try await client.runRaw(
                AiProviderCommands.payloadPreview(
                    projectRootArguments: projectRootsStore.commandLineArguments),
                progressType: ProgressEventDto.self
            )
            previewOutcome = LlmPayloadPreviewInterpretation.interpret(
                exitCode: raw.exitCode,
                stdout: raw.stdout,
                stderr: raw.stderr
            )
        } catch {
            previewErrorMessage = SectionFetchErrors.shortMessage(error, subject: "payload preview")
        }

        isPreviewing = false
        previewTask = nil
    }
}

#Preview {
    AiProviderPreferencesView()
}
