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
enum GlomerisLlmSettingSource: Equatable {
    /// Configured in this app: UserDefaults for the endpoint and model,
    /// the keychain for the key.
    case settings
    /// Not configured here, but present in the environment this app was
    /// launched with — so a `glomeris` child process would inherit it.
    case environment
    /// Not available from either place.
    case absent
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
    var apiKey: GlomerisLlmSettingSource

    /// Whether a live request could be attempted at all. All three are
    /// required — the CLI refuses with `NotConfigured` otherwise — so the UI
    /// can disable a connection-test button rather than invite a round trip
    /// that is guaranteed to fail locally.
    var isComplete: Bool {
        endpoint != .absent && model != .absent && apiKey != .absent
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
struct GlomerisLlmSettingsStore {
    private static let suiteName = "dev.glomeris.GlomerisMenuBar"
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

    init(defaults: UserDefaults? = nil, credentials: CredentialStore? = nil) {
        self.defaults = defaults ?? UserDefaults(suiteName: Self.suiteName) ?? .standard
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

    /// Whether an API key is stored here. There is deliberately no getter for
    /// the key itself on this type: the only code that needs the value is
    /// [`childEnvironment(basedOn:)`], which puts it straight into a child
    /// process's environment. Anything else asking for it would be a leak in
    /// the making.
    var hasStoredApiKey: Bool {
        credentials.secret(forKey: Self.apiKeyAccount) != nil
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

    // MARK: - Resolution

    /// Where each field's effective value comes from, given `environment`
    /// (the launching environment, injectable for tests).
    func status(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> GlomerisLlmSettingsStatus {
        GlomerisLlmSettingsStatus(
            endpoint: Self.source(
                stored: endpoint != nil,
                inherited: environment[Self.endpointEnvironmentVariable]),
            model: Self.source(
                stored: model != nil,
                inherited: environment[Self.modelEnvironmentVariable]),
            apiKey: Self.source(
                stored: hasStoredApiKey,
                inherited: environment[Self.apiKeyEnvironmentVariable])
        )
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
