//
//  GlomerisLlmSettingsStore.swift
//  GlomerisMenuBar
//
//  HORO-1309: the BYOK provider settings — endpoint, model, API key — and the
//  rule for how they combine with the `GLOMERIS_LLM_*` environment variables
//  the Rust CLI already reads.
//
//  See the standing project rule in GlomerisMenuBarApp.swift. This file
//  decides nothing about providers: it does not validate the URL (the CLI's
//  `validate_base_url` does, and reports through `llm-check`), does not know
//  which models exist, and does not talk to a network. It persists three
//  values and assembles the child process's environment.
//

import Foundation

/// Where a single resolved setting came from — the answer to "why is this
/// working when I never typed it in", and equally to "why is it using the
/// wrong key".
///
/// Surfaced in the UI rather than kept internal, because a settings screen
/// that cannot explain an inherited value is how a user ends up editing a
/// field that has no effect.
enum GlomerisLlmSettingSource: Equatable, CaseIterable {
    /// Configured in this app: UserDefaults for the endpoint and model,
    /// the keychain for the key.
    case settings
    /// Not configured here, but present in the environment this app was
    /// launched with — so a `glomeris` child process would inherit it.
    case environment
    /// Not available from either place.
    case absent
    /// Stored here, but this app could not read it, and nothing was inherited
    /// to fall back to (HORO-1368). Only the API key can reach this state; the
    /// endpoint and model live in UserDefaults, which does not refuse.
    ///
    /// Distinct from `absent` because the remedy is different. "Not set" asks
    /// the user for a value they never provided; this asks them to provide a
    /// value they did, to a build that cannot get at the one they stored.
    case unreadable

    /// Whether a value will actually reach the CLI. `unreadable` will not:
    /// nothing was inherited, and the stored item cannot be read, so a spawned
    /// `glomeris` would see no variable and refuse with `NotConfigured`.
    var isAvailable: Bool {
        self == .settings || self == .environment
    }
}

/// One field's resolved state, without its value.
///
/// Deliberately value-free: this type is what the UI renders and what tests
/// assert on, and the API key is one of the three fields. A struct that
/// carried values would make "show the user where their key came from" and
/// "put the key on screen" the same operation.
struct GlomerisLlmSettingsStatus: Equatable {
    var endpoint: GlomerisLlmSettingSource
    var model: GlomerisLlmSettingSource

    /// `nil` while the answer is not known yet.
    ///
    /// Optional because of HORO-1368: this is the one field whose resolution
    /// touches the keychain, the keychain must not be touched on the main
    /// thread, and the screen has to render before an off-thread read can have
    /// finished. A placeholder case would have been the alternative, and it
    /// would have put "not configured yet" and "not looked at yet" into the
    /// same value — the first is a state to act on, the second is a spinner.
    var apiKey: GlomerisLlmSettingSource?

    /// Whether a live request could be attempted at all. All three are
    /// required — the CLI refuses with `NotConfigured` otherwise — so the UI
    /// can disable a connection-test button rather than invite a round trip
    /// that is guaranteed to fail locally.
    ///
    /// `false` while the key is still being resolved. A button that is briefly
    /// disabled is a smaller wrong than one that is briefly enabled.
    var isComplete: Bool {
        endpoint.isAvailable && model.isAvailable && apiKey?.isAvailable == true
    }
}

/// Reads and writes the BYOK provider settings, and builds the environment a
/// `glomeris` child process should run with.
///
/// ## Precedence
///
/// Per field, what this app has configured wins over what it inherited. The
/// rule is "the settings screen never lies": if the endpoint field shows a
/// URL, that is the URL used. A field left empty here falls back to the
/// inherited `GLOMERIS_LLM_*` variable, which is what keeps a user who
/// already exported them in their shell — and launches the app from that
/// shell — working without retyping anything (AC 3).
///
/// The alternative, environment-wins, was rejected: it makes the settings
/// screen display one endpoint while silently using another, and gives the
/// user no way to correct it from the GUI.
///
/// The CLI is unaffected either way. `glomeris` in a terminal reads that
/// terminal's environment and knows nothing about this store.
///
/// ## Where the key goes
///
/// Into the child's environment, and nowhere else. Never a command-line
/// argument (visible in `ps` and refused by the CLI by flag name), never
/// UserDefaults, never a file this app writes, never a log line. It is read
/// from the keychain at the moment a command is spawned and not cached.
///
/// ## Threads
///
/// Everything here is safe to call from any thread, and the two members that
/// reach the keychain must be called off the main one — see
/// [`resolvedStatus(environment:)`].
///
/// `@unchecked` for exactly one reason: `UserDefaults` is not marked `Sendable`
/// by Foundation, while being documented as thread-safe. Everything else here
/// is — `CredentialStore` requires it, and this struct has no mutable state of
/// its own. The unchecked part is that one documented guarantee and nothing
/// else.
struct GlomerisLlmSettingsStore: @unchecked Sendable {
    private static let endpointKey = "llmBaseUrl"
    private static let modelKey = "llmModel"

    /// The keychain account name for the API key. Not a UserDefaults key —
    /// nothing under this name is ever written to UserDefaults.
    static let apiKeyAccount = "llmApiKey"

    static let endpointEnvironmentVariable = "GLOMERIS_LLM_BASE_URL"
    static let modelEnvironmentVariable = "GLOMERIS_LLM_MODEL"
    static let apiKeyEnvironmentVariable = "GLOMERIS_LLM_API_KEY"

    private let defaults: UserDefaults
    private let credentials: CredentialStore

    /// `UserDefaults.standard` is the default rather than a named suite: for a
    /// bundled app it *is* the domain named by its bundle identifier, so the
    /// endpoint and model are namespaced under the running bundle by
    /// construction, and a build with a different identifier reads its own
    /// settings rather than the release app's.
    ///
    /// HORO-1456: this was `UserDefaults(suiteName:) ?? .standard` against the
    /// literal `dev.glomeris.GlomerisMenuBar`. Foundation returns `nil` from
    /// that initialiser when handed the calling process's own bundle
    /// identifier, so in the app the fallback was always the branch taken —
    /// the storage is unchanged, and nothing needs migrating.
    init(defaults: UserDefaults? = nil, credentials: CredentialStore? = nil) {
        self.defaults = defaults ?? .standard
        self.credentials = credentials ?? KeychainCredentialStore()
    }

    // MARK: - Stored values

    /// The configured endpoint, or `nil` if the field is empty. Whitespace-only
    /// is `nil` too: a user who selects a value and hits delete has cleared
    /// the field, whatever residue the text view reports.
    var endpoint: String? {
        get { Self.normalized(defaults.string(forKey: Self.endpointKey)) }
        nonmutating set { Self.write(newValue, to: Self.endpointKey, in: defaults) }
    }

    var model: String? {
        get { Self.normalized(defaults.string(forKey: Self.modelKey)) }
        nonmutating set { Self.write(newValue, to: Self.modelKey, in: defaults) }
    }

    /// Whether an API key is stored here, and whether that could be
    /// established at all. There is deliberately no getter for the key itself
    /// on this type: the only code that needs the value is
    /// [`childEnvironment(basedOn:)`], which puts it straight into a child
    /// process's environment. Anything else asking for it would be a leak in
    /// the making.
    ///
    /// Asks the store for *availability* and not for the value (HORO-1368).
    /// Retrieving the value can block behind an authorisation prompt; asking
    /// whether an item exists cannot.
    var apiKeyAvailability: CredentialAvailability {
        credentials.availability(forKey: Self.apiKeyAccount)
    }

    /// Whether an API key is stored here.
    ///
    /// `false` when the keychain refused the question, which is the same answer
    /// this gave before HORO-1368 and is why the UI uses
    /// [`status(environment:)`] instead: a `Bool` cannot tell "no key" apart
    /// from "a key this build cannot reach".
    var hasStoredApiKey: Bool {
        apiKeyAvailability == .present
    }

    /// Stores the API key, trimmed — a pasted key routinely carries a trailing
    /// newline, and a key with one is a 401 nobody can explain by looking at
    /// the field.
    ///
    /// Storing an empty string deletes instead, so "clear the field and save"
    /// and "revoke" cannot disagree about what happened.
    @discardableResult
    func setApiKey(_ key: String) -> Bool {
        guard let trimmed = Self.normalized(key) else { return deleteApiKey() }
        return credentials.setSecret(trimmed, forKey: Self.apiKeyAccount)
    }

    /// Removes the stored key (AC 8). Succeeds when no key is stored
    /// afterwards, including when none was stored before.
    @discardableResult
    func deleteApiKey() -> Bool {
        credentials.deleteSecret(forKey: Self.apiKeyAccount)
    }

    /// [`setApiKey(_:)`] off the main thread.
    ///
    /// Writes block for the same reason reads do: `SecItemUpdate` replaces an
    /// existing item's data, and the item's ACL guards that too, so saving over
    /// a key stored by a different build can wait on an authorisation prompt
    /// (HORO-1368 AC 1).
    @discardableResult
    func resolvedSetApiKey(_ key: String) async -> Bool {
        await Self.offMainThread { self.setApiKey(key) }
    }

    /// [`deleteApiKey()`] off the main thread, for the same reason.
    @discardableResult
    func resolvedDeleteApiKey() async -> Bool {
        await Self.offMainThread { self.deleteApiKey() }
    }

    // MARK: - Resolution

    /// Where each field's effective value comes from, given `environment`
    /// (the launching environment, injectable for tests).
    ///
    /// Touches the keychain, so it must not be called on the main thread — use
    /// [`resolvedStatus(environment:)`]. The read is non-blocking (availability,
    /// not the value), but "non-blocking" is a property of today's
    /// implementation and the main thread is the app's only entry point; see
    /// this file's callers.
    func status(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> GlomerisLlmSettingsStatus {
        var status = statusWithoutApiKey(environment: environment)
        status.apiKey = Self.source(
            availability: apiKeyAvailability,
            inherited: environment[Self.apiKeyEnvironmentVariable])
        return status
    }

    /// The endpoint and model only, with `apiKey` left `nil`.
    ///
    /// Reaches UserDefaults and nothing else, so it is safe on the main thread
    /// and is what a view seeds itself with (HORO-1368 AC 2). The two fields it
    /// does resolve are the two a user edits by typing, and re-resolving them on
    /// every keystroke must not imply a keychain round trip per character.
    func statusWithoutApiKey(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> GlomerisLlmSettingsStatus {
        GlomerisLlmSettingsStatus(
            endpoint: Self.source(
                stored: endpoint != nil,
                inherited: environment[Self.endpointEnvironmentVariable]),
            model: Self.source(
                stored: model != nil,
                inherited: environment[Self.modelEnvironmentVariable]),
            apiKey: nil
        )
    }

    /// [`status(environment:)`] performed off the main thread.
    ///
    /// The point of the hop is not speed. A keychain operation can wait on a
    /// person — `SecurityAgent` puts up an authorisation prompt and returns
    /// nothing until it is answered — and a main thread that is waiting on a
    /// person cannot service the status item, so AppKit removes it and the app
    /// disappears from the menu bar with no way back in (HORO-1368).
    func resolvedStatus(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) async -> GlomerisLlmSettingsStatus {
        await Self.offMainThread { self.status(environment: environment) }
    }

    /// [`childEnvironment(basedOn:)`] performed off the main thread.
    ///
    /// This is the call that reads the key itself, so it is the one that really
    /// can block on a prompt rather than merely being able to in principle
    /// (AC 3). Callers are already `async` — both are button actions that then
    /// spawn a `glomeris` child — so the hop costs them nothing.
    func resolvedChildEnvironment(
        basedOn environment: [String: String] = ProcessInfo.processInfo.environment
    ) async -> [String: String] {
        await Self.offMainThread { self.childEnvironment(basedOn: environment) }
    }

    /// The full environment to launch a `glomeris` child process with.
    ///
    /// Returns a *complete* environment, not an overlay of the three
    /// variables, because `Process.environment` is a wholesale replacement:
    /// assigning three variables to it would leave the child with no `PATH`,
    /// no `HOME` and no `TMPDIR`, which breaks executable resolution and every
    /// HOME-bounded detector. So this starts from `environment` and sets only
    /// what this app has configured.
    ///
    /// A field configured here overrides the inherited variable; a field left
    /// empty leaves the inherited one untouched, including leaving it absent.
    /// Nothing is ever removed — a user clearing a GUI field falls back to the
    /// environment rather than having a working setup switched off by a UI
    /// they may not have meant to change.
    func childEnvironment(
        basedOn environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> [String: String] {
        var result = environment
        if let endpoint { result[Self.endpointEnvironmentVariable] = endpoint }
        if let model { result[Self.modelEnvironmentVariable] = model }
        // Read at spawn time and not retained anywhere: the returned
        // dictionary is handed to `Process` and dropped.
        if let key = credentials.secret(forKey: Self.apiKeyAccount) {
            result[Self.apiKeyEnvironmentVariable] = key
        }
        return result
    }

    // MARK: - Helpers

    private static func source(stored: Bool, inherited: String?) -> GlomerisLlmSettingSource {
        if stored { return .settings }
        if normalized(inherited) != nil { return .environment }
        return .absent
    }

    /// The same rule for the one field whose storage can refuse the question.
    ///
    /// An inherited variable still wins over an unreadable item, and does so
    /// silently, because in that case the app genuinely does work: the child
    /// process gets the inherited key and the user has nothing to fix. The
    /// degraded state is reported only when there is no fallback — which is
    /// exactly when it changes what the user can do.
    private static func source(
        availability: CredentialAvailability,
        inherited: String?
    ) -> GlomerisLlmSettingSource {
        if availability == .present { return .settings }
        if normalized(inherited) != nil { return .environment }
        return availability == .unreadable ? .unreadable : .absent
    }

    /// Keychain work, off the main thread and one at a time.
    ///
    /// Two separate properties, and only the second one needs this queue —
    /// measured rather than assumed. Awaiting a nonisolated `async` function from
    /// a main-actor caller already leaves the main actor, so replacing the body
    /// below with a bare `work()` still runs the keychain call off the main
    /// thread. What it loses is serialisation: two concurrent reads of the same
    /// ACL-guarded item can raise two authorisation prompts, and a user facing a
    /// stack of identical prompts cannot tell whether answering one did anything.
    ///
    /// The explicit queue is kept for that, and because "off the main thread"
    /// then stops depending on a caller remembering to `await` from somewhere
    /// nonisolated. `MainActor.run` here — the one-line change that reintroduces
    /// the bug — is what the off-thread tests actually catch.
    ///
    /// The label is derived rather than written out (HORO-1456). A queue label
    /// is diagnostic only — it namespaces nothing and no state is filed under
    /// it — but it was the last place in `Sources/` spelling the release bundle
    /// identifier out, and leaving it would have meant the guard script that
    /// keeps that literal from coming back needed an exemption list. An
    /// exemption list is where the next literal hides.
    private static let keychainQueue = DispatchQueue(
        label: "\(BundleIdentity.current).keychain",
        qos: .userInitiated
    )

    private static func offMainThread<T: Sendable>(
        _ work: @escaping @Sendable () -> T
    ) async -> T {
        await withCheckedContinuation { continuation in
            keychainQueue.async { continuation.resume(returning: work()) }
        }
    }

    /// `nil` for absent, empty, or whitespace-only. One rule, applied to
    /// stored and inherited values alike, so an exported-but-empty
    /// `GLOMERIS_LLM_MODEL=` reads as absent here exactly as it does in the
    /// Rust `provider_from_parts`.
    private static func normalized(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines),
            !trimmed.isEmpty
        else { return nil }
        return trimmed
    }

    private static func write(_ value: String?, to key: String, in defaults: UserDefaults) {
        if let normalized = normalized(value) {
            defaults.set(normalized, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}
